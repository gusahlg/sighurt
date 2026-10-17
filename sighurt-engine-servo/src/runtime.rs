//! The Servo runtime: owns the `Servo` instance and its views, and implements `EngineRuntime`.
//!
//! [`ServoRuntime::new`] builds Servo on the owner thread (with the preferences the old
//! `sig-servo` chose, and their reasons, from its README; `hard_fail` off) and hooks Servo's
//! event-loop waker to the [`EngineWaker`]. `pump` spins Servo and turns what its delegates
//! queued into `EngineEvent`s after the call returns ([`ServoRuntime::absorb`]). A page crash
//! is `ViewFailed` (the view stays and can be reloaded); a view whose GL context can't be made
//! is `ViewFailed` then `ViewClosed`; losing Servo's backend is `RuntimeFailed`, after which
//! every call fails: Servo can be built only once per process, so web pages need a browser
//! restart, and dropping and rebuilding this wrapper would not reset Servo's global state.
//!
//! The events wait in one queue, which keeps the latest replaceable event per view and kind; a
//! pump hands over at most [`MAX_EVENTS_PER_PUMP`] and spins Servo only while fewer than that
//! are waiting, so a page that floods Servo with commits fills Servo's own channels, not this
//! queue.
//!
//! Before Servo carries out a navigation a page started that it would load itself (a link, a
//! form, a script, of the top-level document or of a frame: Servo doesn't say which), the
//! view asks the browser (`NavigationRequested`) and holds Servo's request until the answer
//! (`AnswerNavigation`), at most [`MAX_ASKED`] per view; beyond them, or without a committed
//! document to ask for, the navigation goes ahead without asking, and so does the browser's own
//! reload, which Servo 0.5 carried out as the page's navigation to its address (kept; see
//! `SERVO-0.7.md`). A new document,
//! a crash, closing the view and shutting down deny what is still held. A navigation Servo
//! doesn't load itself (another scheme; a local file from a web page) is denied and handed to
//! the browser (`OpenRequested`).
//!
//! `Stop` cancels a navigation still queued (see [`crate::view`]), gives up on a top-level load
//! that has shown no sign of life for [`LOAD_WAIT`](crate::view::LOAD_WAIT) (its
//! `NavigationFailed` is `Cancelled`), and stops the document's own loading (`window.stop()`).
//! Servo 0.7 still has no API to cancel a top-level load that is waiting for its response: that
//! one goes on and commits when its response comes.
//!
//! Servo keeps its profile (cookies, local storage, HSTS, HTTP authentication) in the directory
//! the executable passes, written by this runtime only: it holds `{profile}/lock` from before
//! Servo is built until Servo has shut down (Servo writes the profile while it shuts down), and
//! a second process that finds the lock taken runs Servo without a profile (M0, decision 26).
//!
//! A crashed page's view loads its next navigation in a new WebView on the same context. On
//! Servo 0.5 the crashed site's documents kept going to the dead script thread; that workaround
//! stays until `tests/crash.rs` shows 0.7 can shut down afterwards (see `SERVO-0.7.md`).
//!
//! Timing ([`ServoTiming`]): every submitted frame's paint (`WebView::paint`) and swap, and how
//! long the browser's navigations took to their document's first frame, in
//! [`ServoDiagnostics`], and one line each through the `log` target [`TIMING`] at debug level,
//! which the executable's logger enables (`RUST_LOG=sighurt_engine_servo::timing=debug`):
//!
//! ```text
//! servo-timing frame view=3 w=1280 h=696 paint_ms=1.234 swap_ms=0.210
//! servo-timing first-frame view=3 after_navigate_ms=345.6
//! servo-timing navigation-answered view=3 held_ms=0.412
//! ```
//!
//! On an offscreen target `swap_ms` is the readback into the lent frame. The line stays this
//! shape either way. The Wayland path swaps GL into the subsurface and does not read the page
//! back.
//!
//! The last is how long a navigation the page asked for waited for the browser's answer, from
//! Servo's request to the answer.
//!
//! [`EngineRuntime::shutdown`] closes in the order M1 established: the WebViews, then Servo
//! (which spins until its constellation, WebGL and paint threads are done), then each view's
//! GL context, then the EGL display, then the target leases; the host releases its Wayland
//! objects after that. After a page crash or a lost backend, Servo 0.5 never finished shutting
//! down (its constellation waited for the dead script thread's pipelines). The runtime still
//! leaves it running instead of hanging ([`ServoDiagnostics::leaked`]) until that is shown to
//! have changed (see `SERVO-0.7.md`).
//!
//! Must not own: process lifetime, logging or TLS setup, the allocator (the executable): no
//! `process::exit`, `_exit`, panic hook, logger or crypto provider here.

use std::collections::HashMap;
use std::fs::{File, TryLockError};
use std::mem::Discriminant;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use servo::{
    EmbedderControl, EventLoopWaker, LoadStatus, Opts, PermissionFeature, Preferences,
    RenderingContext, Servo, ServoBuilder, WebViewId,
};
use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, ViewOptions,
};
use sighurt_api::events::{EngineEvent, HistoryEffect, NavigationError, OpenDisposition};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, ViewId};
use sighurt_api::services::{
    ClipboardData, Permission, PickMode, Selection, Service, ServiceReply, ServiceRequest,
    ServiceResult,
};
use sighurt_wayland::TargetLease;
use url::Url;

use crate::delegate::{Coalescing, Delegate, Note};
use crate::input::text_input_state;
use crate::navigation::{Navigator, Request};
use crate::presentation::{physical, Egl, GlInfo, OffscreenAdapter, PageSurface, Readbacks};
use crate::target::ServoTarget;
use crate::view::{Load, Pending, View};

/// Events handed over per pump; the rest wait for the next.
const MAX_EVENTS_PER_PUMP: usize = 256;
/// Service requests a view may have outstanding; beyond them Servo's requests are refused here.
const MAX_REQUESTS: usize = 16;
/// Pop-ups a view may have waiting for their first navigation.
const MAX_POPUPS: usize = 4;
/// Navigations a view may hold for the browser's answer; the page's navigations beyond them go
/// ahead without asking.
const MAX_ASKED: usize = 16;
/// The most clipboard text a page reads.
const CLIPBOARD_LIMIT: usize = 4 << 20;
const TEXT_PLAIN: &str = "text/plain;charset=utf-8";
/// The `log` target of the timing lines (see the module documentation).
pub const TIMING: &str = "sighurt_engine_servo::timing";

/// What the Servo engine supports, for the executable's registry before the runtime exists.
pub const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    history_traversal: true,
    text_input: true,
    accessibility: false,
    screenshots: false,
    find_in_page: false,
    page_zoom: true,
    pointer_lock: false,
    native_popups: false,
    process_per_view: false,
    semantics: false,
};

/// What the executable decides for the Servo runtime.
#[derive(Clone, Debug, Default)]
pub struct ServoConfig {
    /// Servo's profile directory: `{data_dir}/sighurt/servo` in `sig`. `None` keeps nothing.
    pub profile: Option<PathBuf>,
    /// The user agent pages see and requests send, process-wide; `None` for the adapter's own,
    /// Servo's with Firefox's `Gecko/20100101` token. It must be a valid HTTP header value:
    /// Servo's network code unwraps its parse.
    pub user_agent: Option<String>,
    /// Counts every `glReadPixels`/`glGetTexImage` WebRender makes, for tests and diagnostics.
    /// It times every GL call, so it is off for normal use; `read_to_image` is always counted.
    pub audit_gl: bool,
}

/// What the runtime reports about itself.
#[derive(Clone, Debug, Default)]
pub struct ServoDiagnostics {
    /// The first view's GL context: the actual renderer, and whether it is a software fallback.
    pub gl: Option<GlInfo>,
    /// The profile in use, or `None`: none was configured, or another process holds it.
    pub profile: Option<PathBuf>,
    /// Page readbacks to the CPU. On the Wayland path this is `RenderingContext::read_to_image`
    /// and stays 0. On an offscreen target each successful `glReadPixels` into a
    /// [`CpuTarget`](sighurt_api::present::CpuTarget) frame counts once here.
    pub read_to_image: u64,
    /// `glReadPixels`/`glGetTexImage` calls WebRender makes, with [`ServoConfig::audit_gl`].
    /// The offscreen present reads through glow, so it is not counted here. 0 on the display
    /// path.
    pub gl_reads: Option<u64>,
    /// Frames submitted into targets.
    pub frames: u64,
    /// Open views.
    pub views: usize,
    /// Navigations the views' pages asked for that wait for the browser's answer.
    pub asked: usize,
    /// How long painting and navigating took. On an offscreen target [`ServoTiming::swap`] is
    /// the readback.
    pub timing: ServoTiming,
    /// The surfaceless adapter the first offscreen context asked for. `None` when views present
    /// straight into Wayland subsurfaces.
    pub offscreen_adapter: Option<OffscreenAdapter>,
    /// Servo was left running when the runtime shut down (after a page crash or a lost
    /// backend, it can't shut down): its threads outlive the runtime, the profile wasn't
    /// written, and the executable should end the process without running exit handlers.
    pub leaked: bool,
}

/// Paint and swap times of the frames submitted so far, and how long the browser's latest
/// navigation took to show (the baseline's `SIG_SERVO_TIMING`). A Wayland swap never waits
/// (swap interval 0), so it measures the driver's submission, not the display. On an offscreen
/// target [`Self::swap`] is the `glReadPixels` into the lent frame, logged as `swap_ms`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServoTiming {
    /// `WebView::paint` of the last frame submitted: WebRender drawing the frame.
    pub paint: Duration,
    /// The swap of the last frame submitted, or the offscreen readback.
    pub swap: Duration,
    /// The longest paint and swap so far.
    pub max_paint: Duration,
    pub max_swap: Duration,
    /// All paints and swaps so far, over [`ServoDiagnostics::frames`] frames.
    pub total_paint: Duration,
    pub total_swap: Duration,
    /// From the browser's latest `Navigate` that has shown to its document's first submitted
    /// frame.
    pub first_frame: Option<Duration>,
}

/// Who a Servo WebView is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    View(ViewId),
    /// A hidden pop-up the view's document opened, until its first navigation.
    Popup(ViewId, DocumentGeneration),
}

/// The events a pump hands over, keeping only the latest replaceable event per view and kind.
type Events = Coalescing<(Option<ViewId>, Discriminant<EngineEvent>), EngineEvent>;

fn push(events: &mut Events, event: EngineEvent) {
    let key = replaceable(&event).then(|| (event.view(), std::mem::discriminant(&event)));
    events.push(key, event);
}

fn replaceable(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::TitleChanged { .. }
            | EngineEvent::StatusTextChanged { .. }
            | EngineEvent::CursorChanged { .. }
            | EngineEvent::LoadingChanged { .. }
            | EngineEvent::HistoryChanged { .. }
            | EngineEvent::TextInputChanged { .. }
            | EngineEvent::FrameReady { .. }
    )
}

fn cancelled(view: ViewId, navigation: NavigationId) -> EngineEvent {
    EngineEvent::NavigationFailed {
        view,
        navigation,
        error: NavigationError::Cancelled,
    }
}

/// Whether the note `notes` holds after a history change of `webview` says the change was a
/// browser traversal: Servo's constellation reports a traversal's history change and then, at
/// once, its completion, so the next history note or completion of the WebView tells.
enum Traversal {
    /// It was this traversal of the browser's.
    Of(NavigationId),
    /// It wasn't one.
    No,
    /// The completion may not have arrived yet.
    Unknown,
}

/// The one Servo runtime of the process. See the module documentation.
///
/// `T` is the presentation target. It defaults to the Wayland host's [`TargetLease`]: GL goes
/// straight into the subsurface and nothing is read back. [`crate::Offscreen`] is a
/// [`CpuTarget`](sighurt_api::present::CpuTarget) with no display connection. [`Self::new`] is
/// the constructor for both; the target kind is the type of the targets passed to `open_view`.
pub struct ServoRuntime<T: ServoTarget = TargetLease> {
    /// `None` once shut down.
    servo: Option<Servo>,
    delegate: Rc<Delegate>,
    waker: EngineWaker,
    views: HashMap<ViewId, View<T>>,
    owners: HashMap<WebViewId, Owner>,
    /// Views closed while something still held their context: kept, with their target, until
    /// it is released.
    closing: Vec<View<T>>,
    /// The view whose WebView this runtime last gave system focus. Servo 0.7's focus is
    /// per WebView and does not clear the others; a new WebView starts focused.
    servo_focus: Option<ViewId>,
    events: Events,
    /// Notes of WebViews whose last history change may be a traversal whose completion hasn't
    /// arrived, held in order for the next absorb, with whether each was counted already.
    held: Vec<(WebViewId, Note, bool)>,
    readbacks: Readbacks,
    gl: Option<GlInfo>,
    /// Set from the first offscreen context. Stays `None` on the Wayland path.
    offscreen_adapter: Option<OffscreenAdapter>,
    egl: Option<Egl>,
    frames: u64,
    timing: ServoTiming,
    profile: Option<PathBuf>,
    /// Held from before Servo is built until it has shut down.
    profile_lock: Option<File>,
    failed: Option<String>,
    /// A page's script thread panicked. On Servo 0.5 the constellation then never finished
    /// shutting down (it waited for the dead thread's pipelines). Shutdown still leaks Servo
    /// rather than hang; see `SERVO-0.7.md`.
    page_crashed: bool,
    leaked: bool,
}

impl<T: ServoTarget> ServoRuntime<T> {
    /// Builds Servo, which can happen once per process. Its threads start in the background;
    /// pages cost their script threads when they open. `waker` wakes the owner loop for
    /// Servo's work.
    ///
    /// # Panics
    ///
    /// If Servo was built before in this process ("Already initialized").
    pub fn new(config: ServoConfig, waker: EngineWaker) -> Self {
        let profile_lock = config.profile.as_deref().and_then(lock_profile);
        let profile = profile_lock.as_ref().and(config.profile);
        let readbacks = Readbacks {
            read_to_image: Rc::default(),
            gl_reads: config.audit_gl.then(Rc::default),
        };
        let servo = ServoBuilder::default()
            .opts(Opts {
                // Page panics are `notify_crashed`, never a process exit.
                hard_fail: false,
                config_dir: profile.clone(),
                ..Opts::default()
            })
            .preferences(preferences(config.user_agent.as_deref()))
            .event_loop_waker(Box::new(Waker(waker.clone())))
            .build();
        let delegate = Rc::new(Delegate::default());
        servo.set_delegate(Rc::clone(&delegate) as _);
        Self {
            servo: Some(servo),
            delegate,
            waker,
            views: HashMap::new(),
            owners: HashMap::new(),
            closing: Vec::new(),
            servo_focus: None,
            events: Events::default(),
            held: Vec::new(),
            readbacks,
            gl: None,
            offscreen_adapter: None,
            egl: None,
            frames: 0,
            timing: ServoTiming::default(),
            profile,
            profile_lock,
            failed: None,
            page_crashed: false,
            leaked: false,
        }
    }

    /// Installs Servo's logger as the process's `log` logger: the only way script-thread
    /// panics reach Servo (decision 26). The executable calls it once, before anything logs;
    /// it panics if a logger is installed already.
    pub fn setup_logging(&self) {
        if let Some(servo) = &self.servo {
            servo.setup_logging();
        }
    }

    pub fn diagnostics(&self) -> ServoDiagnostics {
        ServoDiagnostics {
            gl: self.gl.clone(),
            profile: self.profile.clone(),
            read_to_image: self.readbacks.read_to_image.get(),
            gl_reads: self.readbacks.gl_reads.as_ref().map(|reads| reads.get()),
            frames: self.frames,
            views: self.views.len(),
            asked: self.views.values().map(|view| view.asked.len()).sum(),
            timing: self.timing,
            offscreen_adapter: self.offscreen_adapter,
            leaked: self.leaked,
        }
    }

    fn alive(&self) -> Result<&Servo, EngineError> {
        match (&self.failed, &self.servo) {
            (None, Some(servo)) => Ok(servo),
            (Some(message), _) => Err(EngineError::Runtime(message.clone())),
            (None, None) => Err(EngineError::Runtime(
                "the Servo engine was shut down".into(),
            )),
        }
    }

    /// Spins Servo once and takes in what its delegates queued.
    fn spin(&mut self) {
        if let Some(servo) = &self.servo {
            servo.spin_event_loop();
        }
        self.absorb();
    }

    /// Makes `view`'s GL context and WebView when it can have them. A context that can't be
    /// made fails the view.
    fn start(&mut self, id: ViewId) -> Result<(), String> {
        let servo = self.servo.clone().expect("checked by the caller");
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let had_context = view.context.is_some();
        view.ensure_context(&self.readbacks)?;
        if let (false, Some(context)) = (had_context, &view.context) {
            if self.egl.is_none() {
                // The context is current: its display is the one every view shares.
                let egl = Egl::load()?;
                let info = context.gl_info(&egl);
                if let Some(adapter) = context.offscreen_adapter() {
                    log::info!("servo: offscreen {adapter} adapter");
                    self.offscreen_adapter = Some(adapter);
                }
                log::info!(
                    "servo: GL {} ({}; {}), EGL {}",
                    info.renderer,
                    info.vendor,
                    info.version,
                    info.egl
                );
                if info.software {
                    log::warn!(
                        "servo: SOFTWARE FALLBACK: {} renders the pages, not the GPU path",
                        info.renderer
                    );
                }
                self.gl = Some(info);
                self.egl = Some(egl);
            }
        }
        if let Some(webview) = view.ensure_webview(&servo, &self.delegate) {
            self.owners.insert(webview.id(), Owner::View(id));
            if !std::mem::replace(&mut view.ready, true) {
                push(&mut self.events, EngineEvent::ViewReady { view: id });
            }
            self.apply_focus(id);
        }
        Ok(())
    }

    /// Loads `url` for `navigation` in a new WebView, after the page crashed. On Servo 0.5 the
    /// crashed site's documents kept going to the dead script thread ("crash page crashed"), so
    /// the old WebView is not reused (see `SERVO-0.7.md`). The new one shares the view's context
    /// and target, and its history starts with this navigation.
    fn restart(&mut self, id: ViewId, navigation: NavigationId, url: Url) {
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let old = view.webview.take();
        for popup in view.popups.drain(..) {
            self.owners.remove(&popup.id());
        }
        view.crashed = false;
        view.queued = None;
        view.load = None;
        view.traversals.clear();
        view.navigator = Navigator::default();
        view.navigator.request(navigation, Request::Navigate);
        view.first = Some((navigation, url));
        view.requests.clear();
        view.ime = None;
        view.composition.reset();
        view.loading = false;
        if self.servo_focus == Some(id) {
            self.servo_focus = None;
        }
        let started = self.start(id);
        // Dropped after the new WebView took over Servo's painter for the context.
        if let Some(old) = old {
            self.owners.remove(&old.id());
        }
        if let Err(message) = started {
            self.fail_view(id, message);
        }
    }

    /// `view` couldn't continue: it is closed, reported as failed and closed.
    fn fail_view(&mut self, id: ViewId, message: String) {
        push(
            &mut self.events,
            EngineEvent::ViewFailed { view: id, message },
        );
        self.close_view(id);
    }

    /// Gives or takes Servo's system focus for `id`: the view has it while it has the
    /// browser's focus, is shown and the window is active. Servo 0.7's `set_focused` is per
    /// WebView and does not clear the others. A new WebView starts focused, in the embedder
    /// and in the constellation, so one that should not have focus is unfocused even when this
    /// runtime never focused it. `set_focused` sends nothing when the flag is already right.
    fn apply_focus(&mut self, id: ViewId) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let Some(webview) = &view.webview else {
            return;
        };
        let wanted = view.focused && view.visible && view.active;
        if wanted && self.servo_focus != Some(id) {
            webview.set_focused(true);
            self.servo_focus = Some(id);
        } else if !wanted {
            if self.servo_focus == Some(id) {
                let releases = view.keys.release_all();
                view.send(releases);
                view.composition.reset();
                self.servo_focus = None;
            }
            webview.set_focused(false);
        }
    }

    /// Asks the browser for `service` on behalf of `view`'s current document, keeping `pending`
    /// until the answer. Without a document, or with too many requests outstanding, `pending`
    /// is dropped, which answers Servo "no".
    fn request(&mut self, id: ViewId, service: Service, pending: Pending) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let Some(document) = view.document() else {
            return;
        };
        if view.requests.len() >= MAX_REQUESTS {
            return;
        }
        let request = RequestId::next();
        view.requests.push((request, pending));
        push(
            &mut self.events,
            EngineEvent::ServiceRequested(ServiceRequest {
                id: request,
                view: id,
                document,
                origin: view.origin(),
                service,
            }),
        );
    }

    /// Turns what Servo's delegates queued into events and view state.
    fn absorb(&mut self) {
        if self.delegate.backend_lost() && self.failed.is_none() {
            let message = "Servo lost its backend; web pages need a browser restart".to_string();
            log::error!("servo: {message}");
            push(
                &mut self.events,
                EngineEvent::RuntimeFailed {
                    message: message.clone(),
                },
            );
            self.failed = Some(message);
        }
        // Each note, and whether it is a `Started` already counted with the history change it
        // belongs to: the two arrive on different channels, so within one batch the history
        // change may come first. Notes held back last time come first.
        let fresh = self.delegate.take().into_iter().map(|(w, n)| (w, n, false));
        let mut notes: Vec<_> = std::mem::take(&mut self.held)
            .into_iter()
            .chain(fresh)
            .map(Some)
            .collect();
        let mut holding = Vec::new();
        for index in 0..notes.len() {
            let Some((webview, note, counted)) = notes[index].take() else {
                continue;
            };
            if holding.contains(&webview) {
                self.held.push((webview, note, counted));
                continue;
            }
            match self.owners.get(&webview).copied() {
                Some(Owner::View(view)) => {
                    let mut traversal = None;
                    let mut started_later = false;
                    if matches!(note, Note::History(..)) {
                        match self.traversal(view, webview, &notes[index + 1..]) {
                            Traversal::Of(navigation) => traversal = Some(navigation),
                            Traversal::No => {}
                            Traversal::Unknown => {
                                // Its completion comes with Servo's next batch: this note and
                                // the WebView's later ones wait for it, in order.
                                holding.push(webview);
                                self.held.push((webview, note, counted));
                                continue;
                            }
                        }
                        let later = notes[index + 1..].iter_mut().flatten().find(|(id, n, c)| {
                            *id == webview && !*c && matches!(n, Note::Load(LoadStatus::Started))
                        });
                        if let Some((.., counted)) = later {
                            *counted = true;
                            started_later = true;
                        }
                        // Servo 0.7 drops `Started` while the WebView is still `Started`. The
                        // new document's `HeadParsed` is in this batch, before any later history
                        // change. Pairing it is what attributes the browser's navigation.
                        if !started_later
                            && traversal.is_none()
                            && self
                                .views
                                .get(&view)
                                .is_some_and(|v| v.navigator.pending_load_needs_evidence())
                        {
                            for (id, n, counted) in notes[index + 1..].iter_mut().flatten() {
                                if *id != webview {
                                    continue;
                                }
                                if matches!(n, Note::History(..)) {
                                    break;
                                }
                                if !*counted && matches!(n, Note::Load(LoadStatus::HeadParsed)) {
                                    *counted = true;
                                    started_later = true;
                                    break;
                                }
                            }
                        }
                    }
                    self.view_note(view, note, started_later || counted, traversal);
                }
                Some(Owner::Popup(parent, document)) => {
                    self.popup_note(parent, document, webview, note)
                }
                // A WebView being closed; what it says no longer matters.
                None => {}
            }
        }
    }

    /// Whether the history change of `webview` before `rest` is a browser traversal of `view`,
    /// and which (see [`Traversal`]).
    fn traversal(
        &self,
        view: ViewId,
        webview: WebViewId,
        rest: &[Option<(WebViewId, Note, bool)>],
    ) -> Traversal {
        let Some(view) = self.views.get(&view).filter(|v| !v.traversals.is_empty()) else {
            return Traversal::No;
        };
        let next = rest.iter().flatten().find(|(id, note, _)| {
            *id == webview && matches!(note, Note::History(..) | Note::TraversalComplete(_))
        });
        match next {
            Some((_, Note::TraversalComplete(done), _)) => view
                .traversals
                .iter()
                .find(|(t, _)| t == done)
                .map_or(Traversal::No, |&(_, navigation)| Traversal::Of(navigation)),
            Some(_) => Traversal::No,
            None => Traversal::Unknown,
        }
    }

    /// `counted`: for a history change, a `Started` that belongs to it comes later in the
    /// batch; for a `Started`, it was counted with its history change already. `traversal`: the
    /// browser's traversal a history change carried out.
    fn view_note(
        &mut self,
        id: ViewId,
        note: Note,
        counted: bool,
        traversal: Option<NavigationId>,
    ) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let events = &mut self.events;
        match note {
            Note::FrameReady => view.frame_ready = true,
            Note::Title(title) => push(events, EngineEvent::TitleChanged { view: id, title }),
            Note::Status(text) => push(events, EngineEvent::StatusTextChanged { view: id, text }),
            Note::Cursor(cursor) => push(
                events,
                EngineEvent::CursorChanged {
                    view: id,
                    cursor: crate::input::cursor(cursor),
                },
            ),
            Note::Load(status) => {
                let started = status == LoadStatus::Started;
                view.navigator.load_status(started, counted);
                if status == LoadStatus::HeadParsed {
                    view.navigator.head_parsed(counted);
                }
                if let (true, Some(load)) = (started, &mut view.load) {
                    // Its document is on its way: the commit follows.
                    load.since = Instant::now();
                }
                let loading = status != LoadStatus::Complete;
                if loading != view.loading {
                    view.loading = loading;
                    push(events, EngineEvent::LoadingChanged { view: id, loading });
                }
            }
            Note::History(entries, current) => {
                let entries: Vec<String> = entries.into_iter().map(String::from).collect();
                let before = view.document();
                if let Some(navigation) = traversal {
                    view.traversals.retain(|&(_, n)| n != navigation);
                }
                let commit = view
                    .navigator
                    .history_changed(entries, current, counted, traversal);
                if let Some(commit) = commit {
                    if let Some((navigation, _, committed)) = &mut view.navigated {
                        *committed |= *navigation == commit.navigation;
                    }
                    let loaded = commit.history != Some(HistoryEffect::Traverse);
                    push(
                        events,
                        EngineEvent::NavigationCommitted {
                            view: id,
                            navigation: commit.navigation,
                            url: commit.url,
                            document: commit.document,
                            history: commit.history,
                        },
                    );
                    if view.document() != before {
                        // Nothing asked for by the old document carries over; the browser
                        // cancels its requests itself, and its navigations are denied.
                        view.requests.clear();
                        view.asked.clear();
                        view.reloading = false;
                        for popup in view.popups.drain(..) {
                            self.owners.remove(&popup.id());
                        }
                        if view.ime.take().is_some() {
                            view.composition.reset();
                            push(
                                events,
                                EngineEvent::TextInputChanged {
                                    view: id,
                                    state: None,
                                },
                            );
                        }
                        if loaded {
                            // The load Servo had in flight brought this document (a traversal
                            // doesn't end one): the navigation waiting for it goes now.
                            view.load = None;
                            if let Some(given_up) = view.send_queued(Instant::now()) {
                                push(events, cancelled(id, given_up));
                            }
                        }
                    }
                }
                push(
                    events,
                    EngineEvent::HistoryChanged {
                        view: id,
                        entries: view.navigator.entries().to_vec(),
                        current,
                    },
                );
                view.keep_throttled();
            }
            Note::TraversalComplete(done) => {
                // Still waiting: the traversal changed nothing, Servo found no entry there.
                let index = view.traversals.iter().position(|(t, _)| *t == done);
                if let Some(index) = index {
                    let (_, navigation) = view.traversals.remove(index);
                    view.navigator.forget(navigation);
                    push(events, cancelled(id, navigation));
                }
            }
            Note::Crashed(reason) => {
                view.crashed = true;
                view.asked.clear();
                self.page_crashed = true;
                push(
                    events,
                    EngineEvent::ViewFailed {
                        view: id,
                        message: format!("The page crashed: {reason}"),
                    },
                );
            }
            Note::Closed => {
                if let Some(document) = view.document() {
                    push(events, EngineEvent::CloseRequested { view: id, document });
                }
            }
            Note::Navigation(navigation) => {
                let url = navigation.url().to_string();
                if view.loads_itself(navigation.url()) {
                    let reload = view.reloading && view.navigator.url() == Some(url.as_str());
                    match view.document() {
                        Some(document) if !reload && view.asked.len() < MAX_ASKED => {
                            let request = RequestId::next();
                            view.asked.push((request, navigation));
                            push(
                                events,
                                EngineEvent::NavigationRequested {
                                    view: id,
                                    document,
                                    request,
                                    url,
                                },
                            );
                        }
                        // The browser's reload, or nothing to ask for, or too much asked.
                        _ => {
                            if reload {
                                view.reloading = false;
                            }
                            if let Some(given_up) = view.allow(navigation) {
                                push(events, cancelled(id, given_up));
                            }
                        }
                    }
                    return;
                }
                // Denied, and handed to the browser.
                drop(navigation);
                if let Some(document) = view.document() {
                    push(
                        events,
                        EngineEvent::OpenRequested {
                            view: id,
                            document,
                            url,
                            disposition: OpenDisposition::CurrentView,
                        },
                    );
                }
            }
            Note::CreateNew(request) => {
                let (Some(context), Some(document)) = (view.context.clone(), view.document())
                else {
                    return;
                };
                if view.popups.len() >= MAX_POPUPS {
                    return;
                }
                let context: Rc<dyn RenderingContext> = context;
                let popup = request
                    .builder(context)
                    .delegate(Rc::clone(&self.delegate) as _)
                    .clipboard_delegate(Rc::clone(&self.delegate) as _)
                    .build();
                // It shares the page's rendering context and must never paint over it. A new
                // WebView also starts with system focus; this one is not the focused view.
                popup.hide();
                popup.set_focused(false);
                self.owners.insert(popup.id(), Owner::Popup(id, document));
                view.popups.push(popup);
            }
            Note::Permission(request) => {
                let permission = match request.feature() {
                    PermissionFeature::Geolocation => Permission::Geolocation,
                    PermissionFeature::Camera => Permission::Camera,
                    PermissionFeature::Microphone => Permission::Microphone,
                    // Nothing else is in the API: refused.
                    _ => return request.deny(),
                };
                self.request(
                    id,
                    Service::Permission(permission),
                    Pending::Permission(request),
                );
            }
            Note::Control(EmbedderControl::InputMethod(control)) => {
                view.ime = Some(control.id());
                let state = text_input_state(
                    control.input_method_type(),
                    &control.text(),
                    control.insertion_point(),
                    control.position(),
                    control.multiline(),
                    &view.metrics,
                );
                push(
                    events,
                    EngineEvent::TextInputChanged {
                        view: id,
                        state: Some(state),
                    },
                );
            }
            Note::Control(EmbedderControl::FilePicker(picker)) => {
                let mode = match picker.allow_select_multiple() {
                    true => PickMode::OpenFiles,
                    false => PickMode::OpenFile,
                };
                let filters = picker
                    .filter_patterns()
                    .iter()
                    .map(|pattern| format!(".{}", pattern.0))
                    .collect();
                self.request(
                    id,
                    Service::PickFiles { mode, filters },
                    Pending::Files(picker),
                );
            }
            // Only input methods and file pickers are queued (see `delegate`).
            Note::Control(_) => {}
            Note::HideControl(control) => {
                if view.ime == Some(control) {
                    view.ime = None;
                    view.composition.reset();
                    push(
                        events,
                        EngineEvent::TextInputChanged {
                            view: id,
                            state: None,
                        },
                    );
                }
                let picker = view.requests.iter().position(
                    |(_, pending)| matches!(pending, Pending::Files(p) if p.id() == control),
                );
                if let Some(index) = picker {
                    let (request, _) = view.requests.remove(index);
                    push(events, EngineEvent::ServiceCancelled { view: id, request });
                }
            }
            Note::ClipboardRead(request) => self.request(
                id,
                Service::ClipboardRead {
                    selection: Selection::Clipboard,
                    mime_types: vec![TEXT_PLAIN.into()],
                    limit: CLIPBOARD_LIMIT,
                },
                Pending::ClipboardRead(request),
            ),
            Note::ClipboardWrite(text) => {
                let items = text
                    .map(|text| ClipboardData {
                        mime_type: TEXT_PLAIN.into(),
                        data: text.into_bytes(),
                    })
                    .into_iter()
                    .collect();
                self.request(
                    id,
                    Service::ClipboardWrite {
                        selection: Selection::Clipboard,
                        items,
                    },
                    Pending::ClipboardWrite,
                );
            }
            Note::Stopped => {
                if view.loading {
                    view.loading = false;
                    push(
                        events,
                        EngineEvent::LoadingChanged {
                            view: id,
                            loading: false,
                        },
                    );
                }
            }
        }
    }

    /// A pop-up's first navigation goes to the browser as a new tab, on behalf of the
    /// `document` that opened it, and the pop-up closes. Nothing else it asks for is granted.
    fn popup_note(
        &mut self,
        parent: ViewId,
        document: DocumentGeneration,
        popup: WebViewId,
        note: Note,
    ) {
        let url = match note {
            // Denied when dropped.
            Note::Navigation(navigation) => navigation.url().clone(),
            Note::History(entries, current) => match entries.into_iter().nth(current) {
                Some(url) => url,
                None => return,
            },
            // Dropping a request object refuses it.
            _ => return,
        };
        if url.as_str() == "about:blank" {
            return;
        }
        self.owners.remove(&popup);
        let Some(view) = self.views.get_mut(&parent) else {
            return;
        };
        view.popups.retain(|p| p.id() != popup);
        push(
            &mut self.events,
            EngineEvent::OpenRequested {
                view: parent,
                document,
                url: url.into(),
                disposition: OpenDisposition::NewView,
            },
        );
    }

    /// Drops what Servo made for a view, then the context, then the target. Returns the view
    /// back if something still holds the context, so its target isn't dropped under it.
    fn release(&mut self, mut view: View<T>) -> Option<View<T>> {
        // Denied while their WebView still exists.
        view.asked.clear();
        for popup in view.popups.drain(..) {
            self.owners.remove(&popup.id());
        }
        if let Some(webview) = view.webview.take() {
            self.owners.remove(&webview.id());
        }
        view.requests.clear();
        if let Some(context) = view.context.take() {
            match Rc::try_unwrap(context) {
                Ok(context) => drop(context),
                Err(context) => {
                    view.context = Some(context);
                    return Some(view);
                }
            }
        }
        None
    }

    /// Retries views whose context was still held when they closed.
    fn release_closing(&mut self) {
        for view in std::mem::take(&mut self.closing) {
            let id = view.id;
            match self.release(view) {
                Some(view) => self.closing.push(view),
                None => push(&mut self.events, EngineEvent::ViewClosed { view: id }),
            }
        }
    }

    /// Whether `view` can show a frame for `permit`: a visible page, a bound surface, and a
    /// permit that is still the current layout.
    fn can_show(view: &View<T>, permit: &T::Permit) -> bool {
        matches!(
            (&view.webview, &view.context),
            (Some(_), Some(context))
                if view.visible
                    && context.surface_is_bound()
                    && T::permit_is_current(permit, view.metrics.epoch())
        )
    }

    /// Paints `id`'s frame at `size`. The caller delivers it (a swap, or one readback).
    fn draw(&mut self, id: ViewId, size: dpi::PhysicalSize<u32>) -> Duration {
        let view = self.views.get_mut(&id).expect("present checked the view");
        let (Some(webview), Some(context)) = (&view.webview, &view.context) else {
            return Duration::ZERO;
        };
        if context.size() != size {
            webview.resize(size);
        }
        let start = Instant::now();
        webview.paint();
        start.elapsed()
    }

    /// Records a submitted frame. `swap` is the Wayland swap or the offscreen readback.
    fn account_frame(
        &mut self,
        id: ViewId,
        size: dpi::PhysicalSize<u32>,
        paint: Duration,
        swap: Duration,
    ) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        self.frames += 1;
        let t = &mut self.timing;
        t.paint = paint;
        t.swap = swap;
        t.max_paint = t.max_paint.max(paint);
        t.max_swap = t.max_swap.max(swap);
        t.total_paint += paint;
        t.total_swap += swap;
        let serial = id.serial();
        log::debug!(
            target: TIMING,
            "servo-timing frame view={serial} w={} h={} paint_ms={:.3} swap_ms={:.3}",
            size.width,
            size.height,
            paint.as_secs_f64() * 1e3,
            swap.as_secs_f64() * 1e3
        );
        if let Some((_, asked, true)) = view.navigated.take_if(|&mut (_, _, shown)| shown) {
            let after = asked.elapsed();
            t.first_frame = Some(after);
            log::debug!(
                target: TIMING,
                "servo-timing first-frame view={serial} after_navigate_ms={:.1}",
                after.as_secs_f64() * 1e3
            );
        }
    }
}

impl<T: ServoTarget> EngineRuntime for ServoRuntime<T> {
    type Target = T;
    type FramePermit = T::Permit;

    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: T,
    ) -> Result<(), EngineError> {
        self.alive()?;
        if let Some(reason) = target.unsupported() {
            return Err(EngineError::Unsupported(reason));
        }
        if self.views.contains_key(&view) {
            return Err(EngineError::View {
                view,
                message: "this view is open already".into(),
            });
        }
        self.views.insert(view, View::new(view, options, target));
        if let Err(message) = self.start(view) {
            if let Some(v) = self.views.remove(&view) {
                // Nothing was made from the lease, or the context would have been kept.
                drop(self.release(v));
            }
            return Err(EngineError::View { view, message });
        }
        Ok(())
    }

    fn command(&mut self, id: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.alive()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        let now = Instant::now();
        match command {
            EngineCommand::Navigate { navigation, url } => {
                let url = match Url::parse(&url) {
                    Ok(url) => url,
                    Err(error) => {
                        push(
                            &mut self.events,
                            EngineEvent::NavigationFailed {
                                view: id,
                                navigation,
                                error: NavigationError::Load(format!(
                                    "Not a valid address: {error}"
                                )),
                            },
                        );
                        return Ok(());
                    }
                };
                push(
                    &mut self.events,
                    EngineEvent::NavigationStarted {
                        view: id,
                        navigation,
                        url: url.to_string(),
                    },
                );
                view.navigated = Some((navigation, now, false));
                if view.crashed {
                    self.restart(id, navigation, url);
                    return Ok(());
                }
                if view.webview.is_none() {
                    view.navigator.request(navigation, Request::Navigate);
                    view.first = Some((navigation, url));
                    if let Err(message) = self.start(id) {
                        self.fail_view(id, message);
                    }
                    return Ok(());
                }
                if let Some((old, _)) = view.queued.replace((navigation, url)) {
                    push(&mut self.events, cancelled(id, old));
                }
                if let Some(given_up) = view.send_queued(now) {
                    push(&mut self.events, cancelled(id, given_up));
                }
            }
            EngineCommand::Reload { navigation } => match &view.webview {
                Some(_) if view.crashed => {
                    match view.navigator.url().and_then(|url| Url::parse(url).ok()) {
                        Some(url) => self.restart(id, navigation, url),
                        None => push(&mut self.events, cancelled(id, navigation)),
                    }
                }
                Some(webview) if view.navigator.url().is_some() => {
                    view.navigator.request(navigation, Request::Reload);
                    webview.reload();
                    view.reloading = true;
                    view.load = Some(Load::new(Some(navigation)));
                    if !view.loading {
                        view.loading = true;
                        push(
                            &mut self.events,
                            EngineEvent::LoadingChanged {
                                view: id,
                                loading: true,
                            },
                        );
                    }
                }
                _ => push(&mut self.events, cancelled(id, navigation)),
            },
            EngineCommand::Stop => {
                if let Some((old, _)) = view.queued.take() {
                    push(&mut self.events, cancelled(id, old));
                }
                // Servo 0.7 still can't cancel a top-level load awaiting its response (there is
                // no public stop, and `load_url` still drops a load while one is in flight).
                // One that has shown no sign of life for `LOAD_WAIT` is given up.
                if view.load.is_some_and(|load| !load.alive(now)) {
                    if let Some(given_up) = view.give_up_load() {
                        push(&mut self.events, cancelled(id, given_up));
                    }
                }
                // The document's own loading. `Window` is a [Global] interface, so `stop` is the
                // window's own property, not the prototype's (`Window.prototype.stop` is
                // undefined); a page that replaces it keeps loading, as it could by starting
                // new fetches.
                if let Some(webview) = &view.webview {
                    let delegate = Rc::clone(&self.delegate);
                    let webview_id = webview.id();
                    webview.evaluate_javascript("window.stop()", move |result| {
                        if result.is_ok() {
                            delegate.stopped(webview_id);
                        }
                    });
                }
            }
            EngineCommand::Traverse { navigation, delta } => {
                match (&view.webview, view.navigator.can_traverse(delta)) {
                    (Some(webview), true) if !view.crashed => {
                        view.navigator.request(navigation, Request::Traverse(delta));
                        let steps = delta.unsigned_abs() as usize;
                        let traversal = if delta < 0 {
                            webview.go_back(steps)
                        } else {
                            webview.go_forward(steps)
                        };
                        view.traversals.push((traversal, navigation));
                    }
                    _ => push(&mut self.events, cancelled(id, navigation)),
                }
            }
            EngineCommand::SetMetrics(metrics) => {
                view.set_metrics(metrics);
                if let Err(message) = self.start(id) {
                    self.fail_view(id, message);
                }
            }
            EngineCommand::SetVisibility(visible) => {
                if let Err(message) = view.set_visible(visible) {
                    self.fail_view(id, message);
                    return Ok(());
                }
                self.apply_focus(id);
            }
            EngineCommand::SetFocus(focused) => {
                view.focused = focused;
                if !focused {
                    let releases = view.keys.release_all();
                    view.send(releases);
                }
                self.apply_focus(id);
            }
            EngineCommand::SetZoom(zoom) => {
                view.zoom = zoom;
                if let Some(webview) = &view.webview {
                    webview.set_page_zoom(zoom as f32);
                }
            }
            EngineCommand::Input(event) => {
                let events = view.translate(&event);
                view.send(events);
                if matches!(event.kind, sighurt_api::input::InputKind::Focus(_)) {
                    self.apply_focus(id);
                }
            }
            EngineCommand::ResolveServiceRequest { request, result } => {
                let index = view.requests.iter().position(|(r, _)| *r == request);
                if let Some(index) = index {
                    let (_, pending) = view.requests.remove(index);
                    resolve(pending, result);
                }
            }
            EngineCommand::AnswerNavigation { request, allow } => {
                let index = view.asked.iter().position(|(r, _)| *r == request);
                if let Some(index) = index {
                    let (_, navigation) = view.asked.remove(index);
                    log::debug!(
                        target: TIMING,
                        "servo-timing navigation-answered view={} held_ms={:.3}",
                        id.serial(),
                        navigation.since.elapsed().as_secs_f64() * 1e3
                    );
                    match allow {
                        true => {
                            if let Some(given_up) = view.allow(navigation) {
                                push(&mut self.events, cancelled(id, given_up));
                            }
                        }
                        false => drop(navigation),
                    }
                }
            }
            // Content as structure is not implemented yet (TUI.md §8, phase 4).
            EngineCommand::SetSemantics(_) | EngineCommand::Semantic { .. } => {
                return Err(EngineError::Unsupported("semantics"))
            }
        }
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        let mut deadline: Option<Instant> = None;
        if self.servo.is_some() && self.failed.is_none() {
            // With a backlog, what Servo has next waits in its own channels.
            if self.events.len() < MAX_EVENTS_PER_PUMP {
                self.spin();
            }
            self.release_closing();
            let now = Instant::now();
            for view in self.views.values_mut() {
                if let Some(given_up) = view.send_queued(now) {
                    push(&mut self.events, cancelled(view.id, given_up));
                }
                if let (Some(_), Some(load)) = (&view.queued, view.load) {
                    let due = load.deadline();
                    deadline = Some(deadline.map_or(due, |d| d.min(due)));
                }
                let wants = view.frame_ready || view.needs_submit || view.driver.is_waiting();
                if wants && view.visible && view.webview.is_some() && !view.frame_requested {
                    view.frame_requested = true;
                    push(&mut self.events, EngineEvent::FrameReady { view: view.id });
                }
            }
        }
        let count = self.events.len().min(MAX_EVENTS_PER_PUMP);
        events.extend(std::iter::from_fn(|| self.events.pop()).take(count));
        match (self.events.len(), deadline) {
            (0, None) => PumpStatus::Idle,
            (0, deadline) => PumpStatus::Pending { deadline },
            _ => PumpStatus::Pending { deadline: None },
        }
    }

    fn present(&mut self, id: ViewId, permit: T::Permit) -> Result<PresentOutcome, EngineError> {
        self.alive()?;
        let view = self.views.get(&id).ok_or(EngineError::UnknownView(id))?;
        if !T::permit_is_for(&permit, id) {
            return Err(EngineError::View {
                view: id,
                message: "a frame permit for another view's target".into(),
            });
        }
        let driver = Rc::clone(&view.driver);
        let spun = driver.is_waiting();
        if spun {
            // The host issues a permit once the last frame's callback has come: the frame
            // Servo waits for starts now, and its content is usually built already.
            driver.start_frame();
            self.spin();
        }
        let ready = {
            let view = self
                .views
                .get_mut(&id)
                .ok_or(EngineError::UnknownView(id))?;
            view.frame_requested = false;
            let epoch = T::permit_epoch(&permit);
            Self::can_show(view, &permit)
                && (view.frame_ready || view.needs_submit || view.presented != Some(epoch))
        };
        if !ready {
            let view = self.views.get(&id).ok_or(EngineError::UnknownView(id))?;
            let outcome = if Self::can_show(view, &permit) {
                PresentOutcome::Unchanged
            } else {
                PresentOutcome::NotReady
            };
            T::dismiss(permit, outcome);
            if spun && !self.events.is_empty() {
                self.waker.wake();
            }
            return Ok(outcome);
        }
        // At a new size this is WebRender's current scene at that size: Servo lays the page
        // out again asynchronously and gives no sign when it has (decision 56).
        let (size, context, previous) = {
            let view = self
                .views
                .get_mut(&id)
                .ok_or(EngineError::UnknownView(id))?;
            let epoch = T::permit_epoch(&permit);
            view.frame_ready = false;
            view.needs_submit = false;
            let previous = view.presented.replace(epoch);
            let size = physical(T::permit_buffer(&permit, view.metrics.buffer()));
            let context = Rc::clone(view.context.as_ref().expect("present found a context"));
            (size, context, previous)
        };
        let paint = self.draw(id, size);
        let view = self.views.get_mut(&id).expect("present checked the view");
        match view.target.deliver(&context, permit) {
            Ok(swap) => {
                self.account_frame(id, size, paint, swap);
                if spun && !self.events.is_empty() {
                    self.waker.wake();
                }
                Ok(PresentOutcome::Submitted)
            }
            Err(message) => {
                if let Some(view) = self.views.get_mut(&id) {
                    view.presented = previous;
                    view.needs_submit = true;
                }
                Err(EngineError::View { view: id, message })
            }
        }
    }

    fn close_view(&mut self, id: ViewId) {
        let Some(view) = self.views.remove(&id) else {
            return;
        };
        if self.servo_focus == Some(id) {
            self.servo_focus = None;
        }
        match self.release(view) {
            Some(view) => self.closing.push(view),
            None => push(&mut self.events, EngineEvent::ViewClosed { view: id }),
        }
    }

    fn shutdown(&mut self) {
        let Some(servo) = self.servo.take() else {
            return;
        };
        let mut views: Vec<View<T>> = self.views.drain().map(|(_, view)| view).collect();
        views.append(&mut self.closing);
        for view in &mut views {
            view.asked.clear();
            view.popups.clear();
            view.webview = None;
            view.requests.clear();
        }
        self.owners.clear();
        // Navigations in notes not taken in yet are denied with them.
        self.held.clear();
        drop(self.delegate.take());
        if self.failed.is_some() || self.page_crashed {
            // Servo's `Drop` spins until its constellation says it has exited, which neither a
            // lost backend nor, after a page crash, a constellation waiting for the dead
            // script thread's pipelines ever does: leak Servo, and with it every context and
            // lease it may still use, rather than hang the browser's exit.
            log::error!("servo: Servo can't shut down after a crash; leaving it running");
            std::mem::forget(servo);
            std::mem::forget(views);
            self.leaked = true;
            return;
        }
        // Servo shuts down only in `Drop`: it tells the constellation to exit and spins until
        // the constellation, WebGL and paint threads are done, releasing its clones of the
        // rendering contexts. It writes the profile on the way, under the lock.
        drop(servo);
        let mut contexts_leaked = false;
        let mut leases = Vec::new();
        for mut view in views {
            if let Some(context) = view.context.take() {
                match Rc::try_unwrap(context) {
                    Ok(context) => drop(context),
                    Err(context) => {
                        log::error!("servo: a page context outlived Servo; leaking its surface");
                        std::mem::forget(context);
                        std::mem::forget(view);
                        contexts_leaked = true;
                        continue;
                    }
                }
            }
            leases.push(view);
        }
        if let (Some(egl), false) = (self.egl.take(), contexts_leaked) {
            if let Err(error) = egl.terminate() {
                log::warn!("servo: {error}");
            }
        }
        // The leases last: the display is gone, and nothing made from their surfaces is left.
        drop(leases);
        self.profile_lock = None;
        self.events.clear();
    }
}

/// Completes Servo's request object with the browser's answer.
fn resolve(pending: Pending, result: ServiceResult) {
    match (pending, result) {
        (Pending::ClipboardRead(request), Ok(ServiceReply::Clipboard(data))) => {
            match String::from_utf8(data.data) {
                Ok(text) => request.success(text),
                Err(_) => request.failure("the clipboard text isn't UTF-8".into()),
            }
        }
        (Pending::ClipboardRead(request), Err(error)) => request.failure(error.to_string()),
        (Pending::Permission(request), Ok(ServiceReply::Done)) => request.allow(),
        (Pending::Files(mut picker), Ok(ServiceReply::Files(paths))) => {
            picker.select(&paths);
            picker.submit();
        }
        // Anything else is a refusal (or, for a clipboard write, nothing to complete):
        // dropping Servo's request object answers it "no".
        _ => {}
    }
}

/// Servo's preferences: its defaults with the changes the old `sig-servo` made, for the
/// reasons its README records (`gpui-baseline:sighurt-engine-servo/README.md`), and
/// `user_agent` if the executable sets one.
fn preferences(user_agent: Option<&str>) -> Preferences {
    let mut prefs = Preferences::default();
    prefs.user_agent = match user_agent {
        Some(agent) => agent.to_owned(),
        None => firefox_user_agent(&prefs.user_agent),
    };
    // Web platform features common sites use. Several were off by default in Servo 0.5;
    // setting them keeps that set on 0.7 even where a default has flipped. Without grid,
    // sites like mozilla.org and theguardian.com lose their layout.
    prefs.dom_exec_command_enabled = true;
    prefs.dom_fontface_enabled = true;
    prefs.dom_indexeddb_enabled = true;
    prefs.dom_offscreen_canvas_enabled = true;
    prefs.dom_permissions_enabled = true;
    prefs.dom_storage_manager_api_enabled = true;
    prefs.dom_webgl2_enabled = true;
    prefs.layout_columns_enabled = true;
    prefs.layout_container_queries_enabled = true;
    prefs.layout_grid_enabled = true;
    prefs.layout_variable_fonts_enabled = true;
    // Left off because they broke common sites on Servo 0.5: with `IntersectionObserver`
    // script spun forever and the page stayed blank (reddit.com, microsoft.com), a layout bug
    // fixed upstream in servo/servo#47693, and with `adoptedStyleSheets` layout panicked
    // (microsoft.com). Not turned on in this port; see `SERVO-0.7.md`.
    //
    // Off: a page's shared worker never exits, so Servo can't shut down (x.com) and the browser's
    // exit would wait forever on it (M0, open problem 5).
    prefs.dom_sharedworker_enabled = false;
    // With Servo's default of 3, parallel styling races and panics a style thread on
    // developer.mozilla.org in ~40% of loads (14 of 36), and the page stops rendering. With 2 it
    // didn't happen in 30 loads.
    prefs.layout_threads = 2;
    prefs
}

/// Servo's user agent with its `Servo/<version>` token replaced by Firefox's `Gecko/20100101`.
/// Servo 0.7's default claims Firefox 153 and still includes a `Servo/` token. Some sites turn
/// away a user agent that isn't exactly Firefox's: web.whatsapp.com asks for "Firefox 115+"
/// instead of showing its login.
fn firefox_user_agent(servo: &str) -> String {
    let tokens = servo.split(' ').map(|token| {
        if token.starts_with("Servo/") {
            "Gecko/20100101"
        } else {
            token
        }
    });
    tokens.collect::<Vec<_>>().join(" ")
}

/// Takes `{dir}/lock` for this process's Servo, which reads the profile when it starts and
/// writes it when it shuts down. `None` if the directory can't be made or another process
/// holds the lock: Servo then runs without a profile rather than race the other's writes.
fn lock_profile(dir: &Path) -> Option<File> {
    let locked = std::fs::create_dir_all(dir).and_then(|()| {
        File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join("lock"))
    });
    let file = match locked {
        Ok(file) => file,
        Err(error) => {
            log::warn!("servo: no profile: cannot use {}: {error}", dir.display());
            return None;
        }
    };
    match file.try_lock() {
        Ok(()) => Some(file),
        Err(TryLockError::WouldBlock) => {
            log::warn!(
                "servo: no profile: another process uses {}; nothing is read or saved",
                dir.display()
            );
            None
        }
        Err(TryLockError::Error(error)) => {
            log::warn!("servo: no profile: cannot lock {}: {error}", dir.display());
            None
        }
    }
}

/// Servo's event-loop waker, waking the owner loop through the engine waker.
#[derive(Clone)]
struct Waker(EngineWaker);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.0.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_firefox() {
        assert_eq!(
            firefox_user_agent(
                "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Servo/0.5.0 Firefox/140.0"
            ),
            "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"
        );
        let default = firefox_user_agent(&Preferences::default().user_agent);
        assert!(default.contains(" Gecko/20100101 Firefox/"), "{default}");
        assert_eq!(preferences(None).user_agent, default);
        // The executable's, as it is.
        let agent = "Mozilla/5.0 (X11; Linux x86_64) Sighurt/0.1";
        assert_eq!(preferences(Some(agent)).user_agent, agent);
    }

    /// Servo's constellation can't be killed from outside, so this reports its loss the way
    /// Servo does, through `ServoDelegate::notify_error`, to a real Servo. (Servo initializes
    /// once per process: this is the only unit test that builds one.)
    #[test]
    fn losing_servos_backend_fails_the_runtime_once() {
        use servo::{ServoDelegate, ServoError};
        use sighurt_api::engine::EngineCommand;

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut runtime: ServoRuntime =
            ServoRuntime::new(ServoConfig::default(), EngineWaker::new(|| {}));
        let mut events = Vec::new();
        assert_eq!(runtime.pump(&mut events), PumpStatus::Idle);
        assert!(events.is_empty());

        runtime
            .delegate
            .notify_error(ServoError::LostConnectionWithBackend);
        runtime.pump(&mut events);
        assert!(
            matches!(events[..], [EngineEvent::RuntimeFailed { .. }]),
            "{events:?}"
        );
        events.clear();
        runtime.pump(&mut events);
        assert!(events.is_empty(), "reported once");
        let command = runtime.command(ViewId::next(), EngineCommand::Stop);
        assert!(
            matches!(command, Err(EngineError::Runtime(_))),
            "{command:?}"
        );
        // A failed Servo is leaked, not waited on: shutting down returns.
        runtime.shutdown();
        assert!(runtime.diagnostics().leaked);
        runtime.shutdown();
        assert!(matches!(
            runtime.command(ViewId::next(), EngineCommand::Stop),
            Err(EngineError::Runtime(_))
        ));
    }

    #[test]
    fn a_second_process_gets_no_profile() {
        let dir = std::env::temp_dir().join(format!("sighurt-profile-{}", std::process::id()));
        let first = lock_profile(&dir).expect("the first lock");
        // Another open file description conflicts like another process would.
        assert!(lock_profile(&dir).is_none());
        drop(first);
        assert!(lock_profile(&dir).is_some(), "free again once released");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn replaceable_events_keep_only_the_latest_per_view_in_place() {
        let (a, b) = (ViewId::next(), ViewId::next());
        let mut events = Events::default();
        push(
            &mut events,
            EngineEvent::TitleChanged {
                view: a,
                title: Some("1".into()),
            },
        );
        push(&mut events, EngineEvent::ViewReady { view: a });
        push(
            &mut events,
            EngineEvent::TitleChanged {
                view: b,
                title: Some("b".into()),
            },
        );
        push(
            &mut events,
            EngineEvent::TitleChanged {
                view: a,
                title: Some("2".into()),
            },
        );
        push(&mut events, EngineEvent::ViewReady { view: a });
        let got: Vec<_> = std::iter::from_fn(|| events.pop()).collect();
        assert_eq!(
            got,
            [
                EngineEvent::ViewReady { view: a },
                EngineEvent::TitleChanged {
                    view: b,
                    title: Some("b".into())
                },
                EngineEvent::TitleChanged {
                    view: a,
                    title: Some("2".into())
                },
                EngineEvent::ViewReady { view: a },
            ]
        );
    }
}
