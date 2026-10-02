//! One Servo view: its WebView, target lease and rendering context, with its visibility, focus,
//! zoom, current metrics, navigations, held keys, input-method state, pending service requests
//! and the pop-ups it opened.
//!
//! The WebView is made once the view has room (non-empty metrics, so a GL context can exist)
//! and its first navigation: Servo loads a WebView's first URL as it builds it, and building
//! one without would put `about:blank` into its history. Until then the view keeps its first
//! navigation and its settings. A hidden view keeps its WebView, but Servo throttles it, it
//! presents nothing and its EGL window surface is given back. Closing it drops the WebView
//! (which drops Servo's painter and its reference to the context), then the context, then the
//! lease, in that order, on the owner thread.
//!
//! Must not own: the tab it belongs to (the core), or anything shared by the runtime's views.

use std::rc::Rc;

use euclid::Scale as ServoScale;
use servo::{
    EmbedderControlId, FilePicker, PermissionRequest, Servo, StringRequest, WebView, WebViewBuilder,
};
use sighurt_api::engine::ViewOptions;
use sighurt_api::geometry::Metrics;
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, RequestId, ViewId};
use sighurt_api::input::InputEvent;
use sighurt_wayland::TargetLease;
use url::Url;

use crate::delegate::Delegate;
use crate::input::{Composition, Keys};
use crate::navigation::Navigator;
use crate::presentation::{physical, FrameDriver, PageContext, Readbacks};

/// A service request of the view the browser hasn't answered yet, with what Servo waits on.
pub(crate) enum Pending {
    ClipboardRead(StringRequest),
    ClipboardWrite,
    Permission(PermissionRequest),
    Files(FilePicker),
}

pub(crate) struct View {
    pub id: ViewId,
    /// Dropped last: the context and everything Servo made from it go first.
    pub lease: TargetLease,
    pub metrics: Metrics,
    pub visible: bool,
    /// The view has the browser's logical focus.
    pub focused: bool,
    /// The window has the compositor's keyboard focus.
    pub active: bool,
    pub zoom: f64,
    pub context: Option<Rc<PageContext>>,
    pub driver: Rc<FrameDriver>,
    pub webview: Option<WebView>,
    /// Where the first navigation goes, until the WebView exists to carry it out.
    pub first: Option<Url>,
    /// A load Servo has been asked for hasn't brought its document yet. Servo 0.5 drops a
    /// `WebView::load` for a browsing context with a load in flight (`Constellation::load_url`),
    /// so a navigation asked for meanwhile waits in `queued` until the document changes.
    pub in_flight: bool,
    pub queued: Option<(NavigationId, Url)>,
    pub navigator: Navigator,
    pub loading: bool,
    pub keys: Keys,
    pub composition: Composition,
    /// Servo's input-method request for the focused field, while there is one.
    pub ime: Option<EmbedderControlId>,
    pub requests: Vec<(RequestId, Pending)>,
    /// Hidden WebViews for pages the view's page opened, until their first navigation says
    /// where they go.
    pub popups: Vec<WebView>,
    /// `ViewReady` was reported.
    pub ready: bool,
    /// The page crashed: the WebView is replaced at the next navigation.
    pub crashed: bool,
    /// Servo has a new frame.
    pub frame_ready: bool,
    /// The next permit must submit a frame even without a new one: a new size, just shown.
    pub needs_submit: bool,
    /// `FrameReady` was reported and no permit has come since.
    pub frame_requested: bool,
    pub presented: Option<LayoutEpoch>,
}

impl View {
    pub fn new(id: ViewId, options: ViewOptions, lease: TargetLease) -> Self {
        Self {
            id,
            metrics: lease.metrics(),
            lease,
            visible: options.visible,
            focused: false,
            active: true,
            zoom: options.zoom,
            context: None,
            driver: Rc::default(),
            webview: None,
            first: None,
            in_flight: false,
            queued: None,
            navigator: Navigator::default(),
            loading: false,
            keys: Keys::default(),
            composition: Composition::default(),
            ime: None,
            requests: Vec::new(),
            popups: Vec::new(),
            ready: false,
            crashed: false,
            frame_ready: false,
            needs_submit: true,
            frame_requested: false,
            presented: None,
        }
    }

    /// Makes the GL context once the view has room for one. A hidden view gives its window
    /// surface back at once.
    pub fn ensure_context(&mut self, readbacks: &Readbacks) -> Result<(), String> {
        if self.context.is_some() || self.metrics.is_empty() {
            return Ok(());
        }
        // SAFETY: everything made from these handles (the EGL window surface and
        // `wl_egl_window` inside the context) is destroyed before the lease is dropped:
        // `release` drops the WebView and the context first, and keeps the lease while anything
        // still holds the context. The EGL display is terminated at shutdown, after every
        // context and Servo, while the host (and so the connection) still exists; it is shared
        // by every view's context and can't be terminated per lease (see `presentation`).
        let handles = unsafe { self.lease.gl_handles() }.ok_or("the target isn't a GL target")?;
        let context = PageContext::new(
            handles,
            self.metrics.buffer(),
            Rc::clone(&self.driver),
            readbacks,
        )?;
        if !self.visible {
            context.release_surface();
        }
        self.context = Some(Rc::new(context));
        Ok(())
    }

    /// Builds the WebView for the first navigation, once the context exists. Returns it for the
    /// runtime to register.
    pub fn ensure_webview(&mut self, servo: &Servo, delegate: &Rc<Delegate>) -> Option<WebView> {
        if self.webview.is_some() {
            return None;
        }
        let context: Rc<dyn servo::RenderingContext> = self.context.clone()?;
        let url = self.first.take()?;
        let webview = WebViewBuilder::new(servo, context)
            .url(url)
            .hidpi_scale_factor(ServoScale::new(self.metrics.scale().factor() as f32))
            .delegate(Rc::clone(delegate) as _)
            .clipboard_delegate(Rc::clone(delegate) as _)
            .build();
        if self.zoom != 1.0 {
            webview.set_page_zoom(self.zoom as f32);
        }
        if !self.visible {
            webview.set_throttled(true);
        }
        // A new WebView's load status starts as `Started` without a report, so its first
        // document's own `Started` is suppressed as unchanged.
        self.navigator.load_status(true, true);
        self.in_flight = true;
        self.webview = Some(webview.clone());
        Some(webview)
    }

    /// New metrics: the WebView renders at their buffer size and scale from now on. An empty
    /// rectangle keeps the last size (nothing can be drawn at 0×0).
    pub fn set_metrics(&mut self, metrics: Metrics) {
        if metrics.is_empty() {
            return;
        }
        self.metrics = metrics;
        if let (Some(webview), Some(context)) = (&self.webview, &self.context) {
            if context.is_bound() {
                webview.resize(physical(metrics.buffer()));
            }
            webview.set_hidpi_scale_factor(ServoScale::new(metrics.scale().factor() as f32));
        }
        self.needs_submit = true;
    }

    /// Shows or hides the view. Hidden, Servo throttles it (no animations, slow timers), it
    /// presents nothing and its EGL window surface is destroyed; shown, it gets a new one and
    /// presents at the next permit without waiting for anything.
    pub fn set_visible(&mut self, visible: bool) -> Result<(), String> {
        if visible == self.visible {
            return Ok(());
        }
        self.visible = visible;
        if let Some(webview) = &self.webview {
            webview.set_throttled(!visible);
        }
        let Some(context) = &self.context else {
            return Ok(());
        };
        if visible {
            // SAFETY: as in `ensure_context`; the new surface belongs to the same context.
            let handles =
                unsafe { self.lease.gl_handles() }.ok_or("the target isn't a GL target")?;
            context.bind_surface(handles, self.metrics.buffer())?;
            if let Some(webview) = &self.webview {
                webview.resize(physical(self.metrics.buffer()));
            }
            self.needs_submit = true;
            self.frame_requested = false;
        } else {
            context.release_surface();
        }
        Ok(())
    }

    /// Sends `events` to the page, if there is one yet.
    pub fn send(&self, events: impl IntoIterator<Item = servo::InputEvent>) {
        if let Some(webview) = &self.webview {
            for event in events {
                webview.notify_input_event(event);
            }
        }
    }

    /// Servo's input events for `event`, updating what the view tracks (held keys, the
    /// composition, window activation).
    pub fn translate(&mut self, event: &InputEvent) -> Vec<servo::InputEvent> {
        use sighurt_api::input::{FocusEvent, InputKind};
        match &event.kind {
            InputKind::Pointer(pointer) => vec![crate::input::pointer(pointer, &self.metrics)],
            InputKind::Key(key) => self.keys.key(key).into_iter().collect(),
            InputKind::TextInput(text) => self.composition.update(text),
            InputKind::Focus(focus) => {
                self.active = *focus == FocusEvent::Activated;
                if self.active {
                    Vec::new()
                } else {
                    self.keys.release_all()
                }
            }
        }
    }

    /// The origin of the committed top-level URL, for service requests.
    pub fn origin(&self) -> Option<String> {
        let url = Url::parse(self.navigator.url()?).ok()?;
        let origin = url.origin();
        origin.is_tuple().then(|| origin.ascii_serialization())
    }

    pub fn document(&self) -> Option<DocumentGeneration> {
        self.navigator.document()
    }

    /// Whether Servo may carry out a navigation to `url` itself: web content, and local files
    /// only from a local page. Anything else (another scheme, a file from a web page) goes to
    /// the browser, which decides where it opens.
    pub fn loads_itself(&self, url: &Url) -> bool {
        match url.scheme() {
            "http" | "https" | "data" | "blob" | "about" | "javascript" => true,
            "file" => self.navigator.url().is_some_and(|u| u.starts_with("file:")),
            _ => false,
        }
    }
}
