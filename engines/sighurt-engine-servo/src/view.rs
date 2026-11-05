//! One Servo view: its WebView, presentation target and rendering context, with its visibility,
//! focus, zoom, current metrics, navigations, held keys, input-method state, pending service
//! requests, the navigations its pages asked the browser about and the pop-ups it opened.
//!
//! The WebView is made once the view has room (non-empty metrics, so a GL context can exist)
//! and its first navigation: Servo loads a WebView's first URL as it builds it, and building
//! one without would put `about:blank` into its history. Until then the view keeps its first
//! navigation and its settings. A hidden view keeps its WebView, but Servo throttles it and it
//! presents nothing. A Wayland view gives its EGL window surface back while hidden; an offscreen
//! view keeps its framebuffer. Servo 0.7 stores hidden on the WebView (`WebView::hide`) and new
//! documents inherit it. `hide` does not resend when paint already recorded hidden, and a hide
//! sent before the constellation has the WebView is dropped, so a view that is hidden before its
//! WebView exists is hidden again once the WebView is built and at each history change
//! ([`View::keep_throttled`]). Closing it drops the WebView (which drops Servo's painter and its
//! reference to the context), then the context, then the target, in that order, on the owner
//! thread.
//!
//! Servo 0.7 still drops a `WebView::load` for a browsing context that has a load in flight
//! (`Constellation::load_url` returns without telling the embedder), and tells the embedder
//! nothing when one ends without a document:
//! a 204 or 205 response aborts it (`AbortLoadUrl`). So a browser navigation asked for while a
//! top-level load may be in flight waits in [`View::queued`] until that load's document arrives
//! or it has shown no sign of life for [`LOAD_WAIT`] ([`Load`]), and is then sent.
//!
//! Must not own: the tab it belongs to (the core), or anything shared by the runtime's views.

use std::rc::Rc;
use std::time::{Duration, Instant};

use euclid::Scale as ServoScale;
use servo::{
    EmbedderControlId, FilePicker, PermissionRequest, Servo, StringRequest, TraversalId,
    UserContentManager, WebView, WebViewBuilder,
};
use sighurt_api::engine::ViewOptions;
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::Metrics;
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, RequestId, ViewId};
use sighurt_api::input::InputEvent;
use url::Url;

use crate::delegate::{Delegate, PageNavigation};
use crate::input::{Composition, Keys};
use crate::navigation::{Navigator, Request};
use crate::presentation::{physical, FrameDriver, Readbacks};
use crate::semantic::Semantics;
use crate::target::ServoTarget;

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
/// or behind an iframe's navigation (which Servo's public API still doesn't tell apart) isn't
/// held for long.
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

pub(crate) struct View<T: ServoTarget> {
    pub id: ViewId,
    pub metrics: Metrics,
    pub visible: bool,
    /// The view has the browser's logical focus.
    pub focused: bool,
    /// The window has the compositor's keyboard focus.
    pub active: bool,
    pub zoom: f64,
    pub context: Option<Rc<T::Surface>>,
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
    /// The browser asked for a reload, which Servo 0.5 carried out as the page's own navigation
    /// to its address (`Location::reload`): that navigation is the browser's, and goes ahead
    /// without asking (kept; see `SERVO-0.7.md`).
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
    /// Semantic snapshots. Off until `SetSemantics`, then at most one extraction at a time.
    pub semantics: Semantics,
    /// The document last reported fullscreen (`FullscreenChanged { fullscreen: true }`), until
    /// `false` is reported for it.
    pub fullscreen: Option<DocumentGeneration>,
    /// Dropped last, after the context and everything Servo made from it.
    pub target: T,
}

impl<T: ServoTarget> View<T> {
    pub fn new(id: ViewId, options: ViewOptions, target: T) -> Self {
        Self {
            id,
            metrics: target.metrics(),
            target,
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
            semantics: Semantics::default(),
            fullscreen: None,
        }
    }

    /// Reports that the page left fullscreen, if it was reported in it: Servo's notification,
    /// or the document, the page or the view going while an element was fullscreen.
    pub fn left_fullscreen(&mut self) -> Option<EngineEvent> {
        let document = self.fullscreen.take()?;
        Some(EngineEvent::FullscreenChanged {
            view: self.id,
            document,
            fullscreen: false,
        })
    }

    /// Makes the GL context once the view has room for one. A hidden Wayland view gives its
    /// window surface back at once; an offscreen view keeps the framebuffer.
    pub fn ensure_context(&mut self, readbacks: &Readbacks) -> Result<(), String> {
        if self.context.is_some() || self.metrics.is_empty() {
            return Ok(());
        }
        let context = self.target.create_context(
            self.metrics.buffer(),
            Rc::clone(&self.driver),
            readbacks,
        )?;
        if !self.visible {
            self.target.hide(&context);
        }
        self.context = Some(Rc::new(context));
        Ok(())
    }

    /// Builds the WebView for the first navigation, once the context exists. Returns it for the
    /// runtime to register.
    pub fn ensure_webview(
        &mut self,
        servo: &Servo,
        delegate: &Rc<Delegate>,
        user_content: Option<Rc<UserContentManager>>,
    ) -> Option<WebView> {
        if self.webview.is_some() {
            return None;
        }
        let context = self.context.clone()?;
        let context: Rc<dyn servo::RenderingContext> = context;
        let (navigation, url) = self.first.take()?;
        let mut builder = WebViewBuilder::new(servo, context)
            .url(url)
            .hidpi_scale_factor(ServoScale::new(self.metrics.scale().factor() as f32))
            .delegate(Rc::clone(delegate) as _)
            .clipboard_delegate(Rc::clone(delegate) as _);
        if let Some(manager) = user_content {
            builder = builder.user_content_manager(manager);
        }
        let webview = builder.build();
        if self.zoom != 1.0 {
            webview.set_page_zoom(self.zoom as f32);
        }
        // The context was made at the metrics of its time, which may have changed since.
        // `NewWebView` is already queued. `hide` behind it is applied once the constellation
        // has the WebView, and later documents inherit the flag. `keep_throttled` repeats it
        // for a view that was hidden with no WebView yet; a repeat does not resend.
        webview.resize(physical(self.metrics.buffer()));
        if !self.visible {
            webview.hide();
        }
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

    /// Lets a navigation the page asked for go ahead. If it is the top level's (Servo doesn't
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
    /// at its old layout. The resize-while-hidden workaround stays; see `SERVO-0.7.md`.)
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
        // A frontend that paces frames (the terminal) drops an unpresented `FrameReady` when
        // metrics change. Clear the request so the next pump emits again. `resize` has already
        // replaced the surface at the new size, so that emit is a frame for the new size.
        // Waiting for a later `FrameReady` from Servo deadlocks an animating page: its repaint
        // waits on the refresh driver, and the driver starts only inside `present`, after the
        // frontend has a `FrameReady`.
        self.frame_requested = false;
    }

    /// Hides the WebView again when this view is hidden. Servo 0.7 stores that on the WebView
    /// and new documents inherit it. `hide` sends nothing when paint already recorded hidden,
    /// so this only matters for a WebView built while the view was still shown, or for one
    /// whose first `hide` was dropped because the constellation did not have it yet.
    pub fn keep_throttled(&self) {
        if let (false, Some(webview)) = (self.visible, &self.webview) {
            webview.hide();
        }
    }

    /// Shows or hides the view. Hidden, Servo throttles it (no animations, slow timers) and it
    /// presents nothing. A Wayland view's EGL window surface is destroyed, and shown again it
    /// gets a new one; an offscreen framebuffer stays. Showing recreates the surface before
    /// `WebView::show`, so the first frame has one. Hiding tells Servo first, then releases
    /// the surface. A view just shown presents at the next permit without waiting for anything.
    pub fn set_visible(&mut self, visible: bool) -> Result<(), String> {
        if visible == self.visible {
            return Ok(());
        }
        self.visible = visible;
        if visible {
            if let Some(context) = &self.context {
                self.target.show(context, self.metrics.buffer())?;
            }
            if let Some(webview) = &self.webview {
                webview.resize(physical(self.metrics.buffer()));
                webview.show();
            }
            if self.context.is_some() {
                self.needs_submit = true;
                self.frame_requested = false;
            }
        } else {
            if let Some(webview) = &self.webview {
                webview.hide();
            }
            if let Some(context) = &self.context {
                self.target.hide(context);
            }
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
            InputKind::Pointer(pointer) => {
                use sighurt_api::input::PointerEvent;
                // Servo takes a click's modifiers from the keys it was sent (`crate::input`).
                let mut events = match pointer {
                    PointerEvent::Button { modifiers, .. } => self.keys.sync_modifiers(*modifiers),
                    _ => Vec::new(),
                };
                events.push(crate::input::pointer(pointer, &self.metrics));
                events
            }
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

    /// The oldest of the browser's traversals Servo hasn't completed that lands on entry
    /// `current` of a history of `len` entries.
    pub fn traversal_landing(&self, len: usize, current: usize) -> Option<NavigationId> {
        self.traversals
            .iter()
            .map(|&(_, navigation)| navigation)
            .find(|&navigation| self.navigator.traversal_lands(navigation, len, current))
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
