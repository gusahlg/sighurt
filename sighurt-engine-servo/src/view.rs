//! One Servo view: its WebView, target lease and rendering context, with its visibility, focus,
//! zoom, current metrics, navigations, held keys, input-method state, pending service requests,
//! the navigations its pages asked the browser about and the pop-ups it opened.
//!
//! The WebView is made once the view has room (non-empty metrics, so a GL context can exist)
//! and its first navigation: Servo loads a WebView's first URL as it builds it, and building
//! one without would put `about:blank` into its history. Until then the view keeps its first
//! navigation and its settings. A hidden view keeps its WebView, but Servo throttles it, it
//! presents nothing and its EGL window surface is given back. Servo 0.5 throttles the document
//! a WebView shows when asked, not the WebView: a hidden view's first document, and each it
//! navigates to, would run unthrottled, so every history change of a hidden view asks again
//! ([`View::keep_throttled`]). Closing it drops the WebView
//! (which drops Servo's painter and its reference to the context), then the context, then the
//! lease, in that order, on the owner thread.
//!
//! Servo 0.5 drops a `WebView::load` for a browsing context that has a load in flight
//! (`Constellation::load_url`), and tells the embedder nothing when one ends without a document:
//! a 204 or 205 response aborts it (`AbortLoadUrl`). So a browser navigation asked for while a
//! top-level load may be in flight waits in [`View::queued`] until that load's document arrives
//! or it has shown no sign of life for [`LOAD_WAIT`] ([`Load`]), and is then sent.
//!
//! Must not own: the tab it belongs to (the core), or anything shared by the runtime's views.

use std::rc::Rc;
use std::time::{Duration, Instant};

use euclid::Scale as ServoScale;
use servo::{
    EmbedderControlId, FilePicker, PermissionRequest, Servo, StringRequest, TraversalId, WebView,
    WebViewBuilder,
};
use sighurt_api::engine::ViewOptions;
use sighurt_api::geometry::Metrics;
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, RequestId, ViewId};
use sighurt_api::input::InputEvent;
use sighurt_wayland::TargetLease;
use url::Url;

use crate::delegate::{Delegate, PageNavigation};
use crate::input::{Composition, Keys};
use crate::navigation::{Navigator, Request};
use crate::presentation::{physical, FrameDriver, PageContext, Readbacks};

/// A service request of the view the browser hasn't answered yet, with what Servo waits on.
pub(crate) enum Pending {
    ClipboardRead(StringRequest),
    ClipboardWrite,
    Permission(PermissionRequest),
    Files(FilePicker),
}

/// How long a top-level load may go without a sign of life (being asked for, its document
/// `Started`) before the view stops waiting for it: long enough for a slow server's response,
/// short enough that a navigation queued behind a load Servo silently dropped (a 204 response)
/// or behind an iframe's navigation (which Servo 0.5 doesn't tell apart) isn't held for long.
pub(crate) const LOAD_WAIT: Duration = Duration::from_secs(5);

/// A top-level load Servo may still be carrying out, which it would drop a new load for.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Load {
    /// When it was asked for, or when its document `Started`.
    pub since: Instant,
    /// The browser's navigation or reload, or `None` for one the page started (perhaps an
    /// iframe's).
    pub navigation: Option<NavigationId>,
}

impl Load {
    pub fn new(navigation: Option<NavigationId>) -> Self {
        Self {
            since: Instant::now(),
            navigation,
        }
    }

    /// When the view stops waiting for it.
    pub fn deadline(&self) -> Instant {
        self.since + LOAD_WAIT
    }

    pub fn alive(&self, now: Instant) -> bool {
        now < self.deadline()
    }
}

pub(crate) struct View {
    pub id: ViewId,
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
    /// The first navigation and where it goes, until the WebView exists to carry it out.
    pub first: Option<(NavigationId, Url)>,
    /// The top-level load that may be in flight.
    pub load: Option<Load>,
    /// The browser's navigation waiting for [`Self::load`] to end.
    pub queued: Option<(NavigationId, Url)>,
    /// The browser's traversals Servo hasn't said are complete, by Servo's id.
    pub traversals: Vec<(TraversalId, NavigationId)>,
    pub navigator: Navigator,
    pub loading: bool,
    pub keys: Keys,
    pub composition: Composition,
    /// Servo's input-method request for the focused field, while there is one.
    pub ime: Option<EmbedderControlId>,
    pub requests: Vec<(RequestId, Pending)>,
    /// Navigations the current document asked for, waiting for the browser's answer. Dropped
    /// (with the document, the view or the runtime), they are denied.
    pub asked: Vec<(RequestId, PageNavigation)>,
    /// The browser asked for a reload, which Servo 0.5 carries out as the page's own navigation
    /// to its address (`Location::reload`): that navigation is the browser's, and goes ahead
    /// without asking.
    pub reloading: bool,
    /// Hidden WebViews for pages the view's current document opened, until their first
    /// navigation says where they go.
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
    /// The browser's latest navigation, when it was asked for, and whether it has committed:
    /// its first frame is timed.
    pub navigated: Option<(NavigationId, Instant, bool)>,
    /// Dropped last, after the context and everything Servo made from it.
    pub lease: TargetLease,
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
            load: None,
            queued: None,
            traversals: Vec::new(),
            navigator: Navigator::default(),
            loading: false,
            keys: Keys::default(),
            composition: Composition::default(),
            ime: None,
            requests: Vec::new(),
            asked: Vec::new(),
            reloading: false,
            popups: Vec::new(),
            ready: false,
            crashed: false,
            frame_ready: false,
            needs_submit: true,
            frame_requested: false,
            presented: None,
            navigated: None,
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
        let (navigation, url) = self.first.take()?;
        let webview = WebViewBuilder::new(servo, context)
            .url(url)
            .hidpi_scale_factor(ServoScale::new(self.metrics.scale().factor() as f32))
            .delegate(Rc::clone(delegate) as _)
            .clipboard_delegate(Rc::clone(delegate) as _)
            .build();
        if self.zoom != 1.0 {
            webview.set_page_zoom(self.zoom as f32);
        }
        // The context was made at the metrics of its time, which may have changed since. A
        // hidden view is throttled at its first history change (`keep_throttled`): asked now,
        // before its browsing context exists, Servo drops the request.
        webview.resize(physical(self.metrics.buffer()));
        // A new WebView's load status starts as `Started` without a report, so its first
        // document's own `Started` is suppressed as unchanged.
        self.navigator.load_status(true, true);
        self.load = Some(Load::new(Some(navigation)));
        self.webview = Some(webview.clone());
        Some(webview)
    }

    /// Loads the queued navigation, unless a load Servo would drop it for may still be in
    /// flight. Returns the browser navigation given up on to send it: one that showed no sign
    /// of life for [`LOAD_WAIT`].
    pub fn send_queued(&mut self, now: Instant) -> Option<NavigationId> {
        if self.webview.is_none() || self.load.is_some_and(|load| load.alive(now)) {
            return None;
        }
        let (navigation, url) = self.queued.take()?;
        let given_up = self.give_up_load();
        self.navigator.request(navigation, Request::Navigate);
        if let Some(webview) = &self.webview {
            webview.load(url);
        }
        self.load = Some(Load::new(Some(navigation)));
        given_up
    }

    /// Lets a navigation the page asked for go ahead. If it is the top level's (Servo 0.5 doesn't
    /// say whether it is an iframe's), Servo would drop a browser navigation until it has its
    /// document: a load already in flight goes on, and one that showed no sign of life for
    /// [`LOAD_WAIT`] is given up for it. Returns the browser navigation given up.
    pub fn allow(&mut self, navigation: PageNavigation) -> Option<NavigationId> {
        navigation.allow();
        if self.load.is_some_and(|load| load.alive(Instant::now())) {
            return None;
        }
        let given_up = self.give_up_load();
        self.load = Some(Load::new(None));
        given_up
    }

    /// Stops waiting for the load in flight. Returns the browser's navigation it was, which is
    /// forgotten: if Servo commits it after all, it counts as the page's own.
    pub fn give_up_load(&mut self) -> Option<NavigationId> {
        let navigation = self.load.take()?.navigation?;
        self.navigator.forget(navigation);
        Some(navigation)
    }

    /// New metrics: the WebView renders at their buffer size and scale from now on. An empty
    /// rectangle keeps the last size (nothing can be drawn at 0×0). A hidden view is resized
    /// too, so its page is laid out at the page area's size while hidden, as a shown one is
    /// (decision 13); without an EGL window surface, surfman's resize on Wayland only records
    /// the size. (Resized only when shown, Servo 0.5 kept a page with a running CSS animation
    /// at its old layout.)
    pub fn set_metrics(&mut self, metrics: Metrics) {
        if metrics.is_empty() {
            return;
        }
        self.metrics = metrics;
        if let Some(webview) = &self.webview {
            webview.resize(physical(metrics.buffer()));
            webview.set_hidpi_scale_factor(ServoScale::new(metrics.scale().factor() as f32));
        }
        self.needs_submit = true;
    }

    /// Throttles the document a hidden view shows now. Servo 0.5 applies `set_throttled` to the
    /// browsing context's current document only (`Constellation::set_webview_throttled`), and a
    /// new document starts unthrottled: a new WebView's first one, whose browsing context
    /// doesn't exist yet when the WebView is built (the request is dropped), and every one it
    /// navigates to. Called at each history change, when a new document has become current.
    pub fn keep_throttled(&self) {
        if let (false, Some(webview)) = (self.visible, &self.webview) {
            webview.set_throttled(true);
        }
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
