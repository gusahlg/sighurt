//! The WPE runtime: one per process, serving every WPE view behind `EngineRuntime`, with the
//! host's target leases and frame permits.
//!
//! WebKit runs its pages in its own web and network processes; that is WebKit's architecture,
//! not Sighurt IPC. Its UI-process side lives here, on the owner thread: GLib's default context
//! ([`GlibLoop`]), Sighurt's WPE platform ([`Display`], one [`View`] per page) and the presenter
//! ([`Wayland`]). The first view decides the transport: DMA-BUF when the compositor offers
//! `zwp_linux_dmabuf_v1` version 4 with a DRM device WebKit can render on, shared memory
//! otherwise (or with [`WpeConfig::force_shm`]); WebKit's web processes learn it from the
//! primary display when they start.
//!
//! [`WpeRuntime::new`] opens WebKit's library ([`ffi::load`]: nothing of WebKit is mapped
//! before), keeps JavaScriptCore in this process off signals (decision 66) and takes GLib's
//! default context, whose watch thread wakes the owner through the [`EngineWaker`] (decision
//! 64). The views share a `WebKitWebContext` made with the first and ended with the last, so no
//! web process outlives the last WPE page (`WebContext`; decision 84).
//!
//! WebKit's callbacks only record notes ([`crate::view`]), which each runtime entry turns into
//! events after WebKit returns. A crashed web process is `ViewFailed`: the view stays, and a
//! reload or navigation starts a new process. A broken connection to the compositor on the
//! presenter's queue is `RuntimeFailed`, after which every call fails.
//!
//! A link or a GET form the user clicked in a page (WebKit's user-gesture flag and navigation
//! type, `view::asks`) is held as WebKit's policy decision and asked about with
//! `NavigationRequested` for the committed document, at most [`view::MAX_HELD`] per view (beyond
//! them, or before a document commits, it goes ahead). The browser's `AnswerNavigation` uses or
//! ignores it; a new document, a crash, closing the view and shutdown ignore what is still held.
//! `RUST_LOG=sighurt_engine_wpe::timing=debug` logs how long each waited ([`TIMING`]).
//!
//! The profile (cookies, storage, caches) is a persistent `WebKitNetworkSession` in the
//! configured directory, held with `{profile}/lock` for the runtime's life; a second process
//! that finds it locked, or no directory, gets an ephemeral session. [`WpeConfig::user_agent`]
//! replaces WebKit's user agent in every view's settings.
//!
//! [`EngineRuntime::shutdown`] closes every view (WebKit's view, its buffers and our
//! `wl_buffer`s, our surface proxy and SHM buffers, then the lease), lets GLib finish WebKit's
//! teardown, and drops the session, the display and presenter, and GLib's context, in that
//! order.
//!
//! Must not own: tabs, routing or history policy (the core), surfaces (the host), process-wide
//! setup other than WebKit's own (the executable).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::CString;
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use glib::prelude::*;
use glib::translate::*;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, ViewOptions,
};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError, OpenDisposition};
use sighurt_api::geometry::{Metrics, SurfaceRect};
use sighurt_api::ids::{NavigationId, RequestId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputKind, PointerEvent, TextInputState, TextPurpose,
};
use sighurt_api::services::{DownloadSource, Selection, Service, ServiceRequest};
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};

use crate::clipboard::{self, ClipboardCall};
use crate::ffi;
use crate::glib_loop::GlibLoop;
use crate::ime::Ime;
use crate::input::Input;
use crate::navigation::Commit;
use crate::platform::{Buffer, Call, Display, View};
use crate::present::{self, BufferKey, PresentStats, Wayland};
use crate::view::{self, Answer, Note, Page, RequestKind, Shared, TextInput};

/// What the WPE engine supports, for the executable's registry before the runtime exists.
pub const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    history_traversal: true,
    text_input: true,
    accessibility: false,
    screenshots: false,
    find_in_page: false,
    page_zoom: true,
    pointer_lock: false,
    native_popups: false,
    process_per_view: true,
    semantics: false,
};

/// The `log` target of the timing lines: `RUST_LOG=sighurt_engine_wpe::timing=debug`.
pub const TIMING: &str = "sighurt_engine_wpe::timing";

/// The most clipboard text a page reads.
const CLIPBOARD_LIMIT: usize = 4 << 20;

/// Whether a WPE runtime was started in this process: WebKit's UI-process state is global and
/// the display is its primary one, so there is one.
static STARTED: AtomicBool = AtomicBool::new(false);

/// What the executable decides for the WPE runtime.
#[derive(Clone, Debug, Default)]
pub struct WpeConfig {
    /// WebKit's profile directory (`{data_dir}/sighurt/wpe` in `sig`): cookies, storage, and
    /// its caches under `cache/`. `None` keeps nothing.
    pub profile: Option<PathBuf>,
    /// What WebKit's requests send and its pages see as the user agent, instead of WebKit's own.
    pub user_agent: Option<String>,
    /// Shared memory even where DMA-BUF works: the fallback path, for testing it.
    pub force_shm: bool,
    /// Lets JavaScriptCore in this process install its signal handlers, as WebKit does by
    /// default: only to reproduce the clash it causes (experiments/wpe-coexist). Never in `sig`.
    pub jsc_signal_handlers: bool,
}

/// How WebKit's frames reach the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// WebKit's GPU buffers, attached as they are: no CPU copy.
    Dmabuf,
    /// WebKit's shared-memory frames, copied once into ours: the diagnosed fallback.
    Shm,
}

/// What the runtime can tell about itself.
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub transport: Option<Transport>,
    /// The DRM render node WebKit renders on, with DMA-BUF.
    pub device: Option<String>,
    /// That device's kernel driver ("nvidia", "amdgpu", ...).
    pub driver: Option<String>,
    /// DMA-BUF format and modifier pairs the compositor imports on that device.
    pub formats: usize,
    pub present: PresentStats,
    /// DMA-BUF imports the compositor refused.
    pub failed_imports: u64,
    /// Times GLib's context dispatched sources.
    pub glib_dispatches: u64,
    /// Frames of an old geometry, dropped as stale.
    pub stale_frames: u64,
    /// Frames one pixel off the host's buffer size in a dimension, shown anyway: WebKit floors
    /// `logical × scale` where the host rounds half away from zero, and the host's viewport
    /// scales the buffer to the logical size whatever its size.
    pub rounded_frames: u64,
    /// Open views.
    pub views: usize,
    /// Navigations the views' pages asked for that wait for the browser's answer.
    pub asked: usize,
    /// The profile in use, or `None`: none was configured, or another process holds it.
    pub profile: Option<PathBuf>,
    /// The longest and the total time `present` took for submitted frames.
    pub max_present: Duration,
    pub total_present: Duration,
}

impl Diagnostics {
    /// Frames submitted to the compositor.
    pub fn frames(&self) -> u64 {
        self.present.dmabuf_frames + self.present.shm_copies
    }

    /// How WebKit's pages reach the screen, for the browser's diagnostics: "WebKit, DMA-BUF on
    /// DEVICE (DRIVER)", or the shared-memory fallback, which copies every frame.
    pub fn renderer(&self) -> Option<String> {
        Some(match self.transport? {
            Transport::Dmabuf => format!(
                "WebKit, DMA-BUF on {} ({})",
                self.device.as_deref().unwrap_or("?"),
                self.driver.as_deref().unwrap_or("?")
            ),
            Transport::Shm => "WebKit, SHARED-MEMORY FALLBACK: one CPU copy per frame".into(),
        })
    }
}

/// The display and presenter, made with the first view.
struct Platform {
    wayland: Wayland,
    display: Display,
    session: glib::Object,
    /// The views' web context, while there are views.
    context: Option<WebContext>,
    /// Our handlers on WebKit's objects that may outlive the runtime (WebKit may hold them
    /// after we drop ours), disconnected at shutdown.
    handlers: Vec<(glib::Object, glib::SignalHandlerId)>,
    transport: Transport,
    device: Option<String>,
    driver: Option<String>,
    formats: usize,
}

/// The WPE engine's runtime. See the module documentation.
pub struct WpeRuntime {
    config: WpeConfig,
    waker: EngineWaker,
    glib: Option<GlibLoop>,
    platform: Option<Platform>,
    pages: HashMap<ViewId, Page>,
    inbox: Shared,
    events: Vec<EngineEvent>,
    /// Our DMA-BUF `wl_buffer`s the compositor holds, with the WebKit buffer to release.
    in_flight: HashMap<BufferKey, (ViewId, Buffer)>,
    stats: PresentStats,
    stale_frames: u64,
    rounded_frames: u64,
    max_present: Duration,
    total_present: Duration,
    scripts: Vec<(ViewId, Result<String, String>)>,
    /// The view whose page last got a click or key press: the one the clipboard acts for.
    last_input: Option<ViewId>,
    profile: Option<PathBuf>,
    /// Held while the profile is in use.
    profile_lock: Option<File>,
    failed: Option<String>,
    shut_down: bool,
}

impl WpeRuntime {
    /// Starts the runtime on the owner thread: loads WPE WebKit's library ([`ffi::load`]), keeps
    /// JavaScriptCore off signals and takes GLib's default context, which WebKit's UI-process
    /// side will use. WebKit itself starts with the first view. Fails if a WPE runtime was started
    /// in this process before, or the library can't be loaded.
    pub fn new(config: WpeConfig, waker: EngineWaker) -> Result<Self, EngineError> {
        if STARTED.swap(true, Ordering::SeqCst) {
            return Err(EngineError::Runtime(
                "WebKit runs once per process: a WPE runtime was started already".into(),
            ));
        }
        ffi::load().map_err(EngineError::Runtime)?;
        if !config.jsc_signal_handlers {
            keep_jsc_off_signals()?;
        }
        let glib = GlibLoop::new(waker.clone()).map_err(EngineError::Runtime)?;
        let profile_lock = config.profile.as_deref().and_then(lock_profile);
        let profile = profile_lock.as_ref().and(config.profile.clone());
        Ok(Self {
            config,
            waker,
            glib: Some(glib),
            platform: None,
            pages: HashMap::new(),
            inbox: Shared::default(),
            events: Vec::new(),
            in_flight: HashMap::new(),
            stats: PresentStats::default(),
            stale_frames: 0,
            rounded_frames: 0,
            max_present: Duration::ZERO,
            total_present: Duration::ZERO,
            scripts: Vec::new(),
            last_input: None,
            profile,
            profile_lock,
            failed: None,
            shut_down: false,
        })
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let platform = self.platform.as_ref();
        let (imports, failed_imports) = platform.map_or((0, 0), |p| p.wayland.imports());
        Diagnostics {
            transport: platform.map(|p| p.transport),
            device: platform.and_then(|p| p.device.clone()),
            driver: platform.and_then(|p| p.driver.clone()),
            formats: platform.map_or(0, |p| p.formats),
            present: PresentStats {
                dmabuf_imports: imports,
                ..self.stats
            },
            failed_imports,
            glib_dispatches: self.glib.as_ref().map_or(0, |g| g.dispatches),
            stale_frames: self.stale_frames,
            rounded_frames: self.rounded_frames,
            views: self.pages.len(),
            asked: self.pages.values().map(|page| page.held.len()).sum(),
            profile: self.profile.clone(),
            max_present: self.max_present,
            total_present: self.total_present,
        }
    }

    /// Runs `script` in `view`'s main frame; its result (as a string) arrives in
    /// [`take_script_results`](Self::take_script_results) after a later pump. For tests and
    /// diagnostics: the browser never runs scripts in pages.
    pub fn evaluate(&mut self, view: ViewId, script: &str) -> Result<(), EngineError> {
        let page = self
            .pages
            .get(&view)
            .ok_or(EngineError::UnknownView(view))?;
        let script =
            CString::new(script).map_err(|_| EngineError::Unsupported("NUL in a script"))?;
        let data = Box::into_raw(Box::new((Rc::downgrade(&self.inbox), view)));
        unsafe {
            ffi::webkit_web_view_evaluate_javascript(
                page.web(),
                script.as_ptr(),
                -1,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                Some(script_done),
                data.cast(),
            );
        }
        Ok(())
    }

    /// Script results that arrived, oldest first.
    pub fn take_script_results(&mut self) -> Vec<(ViewId, Result<String, String>)> {
        std::mem::take(&mut self.scripts)
    }

    /// Ends `view`'s web process, as a crash would: for tests of crash recovery.
    pub fn terminate_web_process(&mut self, view: ViewId) -> Result<(), EngineError> {
        let page = self
            .pages
            .get(&view)
            .ok_or(EngineError::UnknownView(view))?;
        unsafe { ffi::webkit_web_view_terminate_web_process(page.web()) };
        self.handle_notes();
        Ok(())
    }

    fn alive(&self) -> Result<(), EngineError> {
        match (&self.failed, self.shut_down) {
            (Some(message), _) => Err(EngineError::Runtime(message.clone())),
            (None, true) => Err(EngineError::Runtime("the WPE runtime was shut down".into())),
            (None, false) => Ok(()),
        }
    }

    /// Makes the display, presenter and network session on the host's connection, from the
    /// first lease.
    fn platform(&mut self, lease: &TargetLease) -> Result<&mut Platform, EngineError> {
        if self.platform.is_none() {
            let runtime = |e: String| EngineError::Runtime(e);
            // SAFETY: the presenter's objects (our queue, buffers and surface proxies) are
            // destroyed before the leases go (`close`, `shutdown`).
            let handles = unsafe { lease.gl_handles() }.ok_or(EngineError::Unsupported(
                "WPE views present on native targets",
            ))?;
            let display = match handles.display_handle().map(|h| h.as_raw()) {
                Ok(RawDisplayHandle::Wayland(h)) => h.display,
                _ => return Err(runtime("no Wayland display".into())),
            };
            let wayland = unsafe { Wayland::new(display) }.map_err(runtime)?;
            let dmabuf = wayland.dmabuf().filter(|_| !self.config.force_shm);
            let transport = match dmabuf {
                Some(_) => Transport::Dmabuf,
                None => Transport::Shm,
            };
            let device = dmabuf.as_ref().map(|d| {
                let node = d.render.as_ref().unwrap_or(&d.primary);
                node.to_string_lossy().into_owned()
            });
            let driver = dmabuf.as_ref().and_then(|_| wayland.driver());
            let formats = dmabuf.as_ref().map_or(0, |d| d.formats.len());
            let inbox = Rc::downgrade(&self.inbox);
            let clipboard = Rc::new(move |call| {
                if let Some(inbox) = inbox.upgrade() {
                    inbox.borrow_mut().clipboard.push(call);
                }
            });
            let display = Display::new(dmabuf, clipboard).map_err(|e| runtime(e.to_string()))?;
            let session = network_session(self.profile.as_deref());
            let downloads = view::connect_downloads(&session, &self.inbox);
            match transport {
                Transport::Dmabuf => {}
                Transport::Shm => {
                    log::warn!(
                    "wpe: SHARED-MEMORY FALLBACK: no DMA-BUF{}; every frame is copied on the CPU",
                    if self.config.force_shm { " (forced)" } else { " from the compositor" }
                )
                }
            }
            self.platform = Some(Platform {
                wayland,
                display,
                handlers: vec![(session.clone(), downloads)],
                session,
                context: None,
                transport,
                device,
                driver,
                formats,
            });
        }
        Ok(self.platform.as_mut().expect("made above"))
    }

    fn push(&mut self, event: EngineEvent) {
        push(&mut self.events, event);
    }

    /// Turns what WebKit's callbacks recorded into events and view state.
    fn handle_notes(&mut self) {
        loop {
            let (notes, facts, clipboard) = {
                let mut inbox = self.inbox.borrow_mut();
                (
                    std::mem::take(&mut inbox.notes),
                    std::mem::take(&mut inbox.facts),
                    std::mem::take(&mut inbox.clipboard),
                )
            };
            if notes.is_empty() && facts.is_empty() && clipboard.is_empty() {
                return;
            }
            for (view, note) in notes {
                self.handle(view, note);
            }
            for (view, facts) in facts {
                self.facts(view, facts);
            }
            for call in clipboard {
                self.clipboard(call);
            }
        }
    }

    fn handle(&mut self, id: ViewId, note: Note) {
        if let Note::Script(result) = note {
            self.scripts.push((id, result));
            return;
        }
        let Some(page) = self.pages.get_mut(&id) else {
            return;
        };
        let events = &mut self.events;
        match note {
            Note::Frame(buffer, damage) => {
                if let Some((old, _)) = page.pending.replace((buffer, damage)) {
                    self.stats.dropped += 1;
                    finish_unshown(page.wpe(), &old);
                }
                push(events, EngineEvent::FrameReady { view: id });
            }
            Note::Started => {
                let (navigation, browsers) = page.navigator.started();
                if browsers {
                    let url = view::address(&page.web_view);
                    push(
                        events,
                        EngineEvent::NavigationStarted {
                            view: id,
                            navigation,
                            url,
                        },
                    );
                }
            }
            Note::Committed(url, history) => {
                let commit = page.navigator.committed(url, history);
                committed(page, events, commit);
            }
            Note::Failed { provisional, error } => {
                if let Some(navigation) = provisional.then(|| page.navigator.failed()).flatten() {
                    push(
                        events,
                        EngineEvent::NavigationFailed {
                            view: id,
                            navigation,
                            error,
                        },
                    );
                }
            }
            Note::ListChanged(added, history) => {
                if let Some(commit) = page.navigator.list_changed(added, history) {
                    committed(page, events, commit);
                }
            }
            Note::Url(url, history) => {
                if let Some(commit) = page.navigator.url_changed(url, history) {
                    committed(page, events, commit);
                }
            }
            Note::Loading(loading, url, history) => {
                if let Some(commit) = (!loading)
                    .then(|| page.navigator.loading_stopped(url, history))
                    .flatten()
                {
                    committed(page, events, commit);
                }
                if loading != page.loading {
                    page.loading = loading;
                    push(events, EngineEvent::LoadingChanged { view: id, loading });
                }
            }
            Note::Elsewhere(url) => match page.navigator.refused() {
                // The browser asked for it: the core routed a URL here that WebKit can't load.
                Some(navigation) => push(
                    events,
                    EngineEvent::NavigationFailed {
                        view: id,
                        navigation,
                        error: NavigationError::Load(format!(
                            "WebKit doesn't open {} addresses",
                            url.split(':').next().unwrap_or_default()
                        )),
                    },
                ),
                None => {
                    if let Some(document) = page.navigator.document() {
                        let disposition = OpenDisposition::CurrentView;
                        push(
                            events,
                            EngineEvent::OpenRequested {
                                view: id,
                                document,
                                url,
                                disposition,
                            },
                        );
                    }
                }
            },
            Note::Navigation(navigation, url) => match page.navigator.document() {
                Some(document) if page.held.len() < view::MAX_HELD => {
                    let request = RequestId::next();
                    page.held.push((request, navigation, Instant::now()));
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
                // Nothing to ask for, or too much asked.
                _ => navigation.allow(),
            },
            Note::NewView(url) => {
                if let Some(document) = page.navigator.document() {
                    let disposition = OpenDisposition::NewView;
                    push(
                        events,
                        EngineEvent::OpenRequested {
                            view: id,
                            document,
                            url,
                            disposition,
                        },
                    );
                }
            }
            Note::Close => {
                if let Some(document) = page.navigator.document() {
                    push(events, EngineEvent::CloseRequested { view: id, document });
                }
            }
            Note::Terminated(reason) => {
                if let Some((frame, _)) = page.pending.take() {
                    finish_unshown(page.wpe(), &frame);
                }
                for navigation in page.navigator.crashed() {
                    push(events, cancelled(id, navigation));
                }
                page.requests.clear();
                page.held.clear();
                if page.text_input.take().is_some() {
                    push(
                        events,
                        EngineEvent::TextInputChanged {
                            view: id,
                            state: None,
                        },
                    );
                }
                let why = match reason {
                    ffi::WEBKIT_WEB_PROCESS_CRASHED => "crashed",
                    ffi::WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT => "exceeded its memory limit",
                    _ => "was terminated",
                };
                push(
                    events,
                    EngineEvent::ViewFailed {
                        view: id,
                        message: format!("The page's web process {why}; reload to start it again"),
                    },
                );
            }
            Note::Permission(request, permissions) => {
                let services = permissions.into_iter().map(Service::Permission).collect();
                match page.ask(services, RequestKind::Permission(request.clone())) {
                    Some(requests) => push_requests(events, requests),
                    None => unsafe { ffi::webkit_permission_request_deny(request.as_ptr().cast()) },
                }
            }
            Note::Files(request, mode, filters) => {
                let service = Service::PickFiles { mode, filters };
                match page.ask(vec![service], RequestKind::Files(request.clone())) {
                    Some(requests) => push_requests(events, requests),
                    None => unsafe {
                        ffi::webkit_file_chooser_request_cancel(request.as_ptr().cast())
                    },
                }
            }
            Note::Download(url, suggested_name) => {
                let service = Service::Download {
                    source: DownloadSource::Url(url),
                    suggested_name,
                };
                if let Some(requests) = page.ask(vec![service], RequestKind::Done) {
                    push_requests(events, requests);
                }
            }
            Note::Ime(ime) => {
                let before = page.text_input.clone();
                match ime {
                    Ime::Focus(true) => {
                        page.text_input.get_or_insert_with(TextInput::default);
                    }
                    Ime::Focus(false) => page.text_input = None,
                    Ime::Caret(caret) => {
                        if let Some(input) = &mut page.text_input {
                            input.caret = caret;
                        }
                    }
                    Ime::Surrounding(surrounding) => {
                        if let Some(input) = &mut page.text_input {
                            input.surrounding = Some(surrounding);
                        }
                    }
                }
                // A field is reported once WebKit has said where its caret is, with its text:
                // what an input method gets when it is enabled is what it works with (wlroots
                // latches the features of a text-input session at enable).
                let placed = |input: &Option<TextInput>| {
                    input
                        .as_ref()
                        .is_some_and(|i| i.caret != SurfaceRect::default())
                };
                if page.text_input != before && (placed(&page.text_input) || placed(&before)) {
                    let state = text_input_state(page);
                    push(events, EngineEvent::TextInputChanged { view: id, state });
                }
            }
            Note::Script(_) => unreachable!("handled above"),
        }
    }

    fn facts(&mut self, id: ViewId, facts: view::Facts) {
        let Some(page) = self.pages.get(&id) else {
            return;
        };
        if facts.title {
            let title = unsafe { ffi::string(ffi::webkit_web_view_get_title(page.web())) };
            self.push(EngineEvent::TitleChanged { view: id, title });
        }
        if let Some(text) = facts.status {
            self.push(EngineEvent::StatusTextChanged { view: id, text });
        }
        if let Some(name) = facts.cursor {
            self.push(EngineEvent::CursorChanged {
                view: id,
                cursor: cursor(&name),
            });
        }
    }

    /// A copy or a paste, for the view with the user's latest input.
    fn clipboard(&mut self, call: ClipboardCall) {
        let Some(page) = self.last_input.and_then(|id| self.pages.get_mut(&id)) else {
            return;
        };
        let request = match call {
            ClipboardCall::Written(items) => {
                let service = Service::ClipboardWrite {
                    selection: Selection::Clipboard,
                    items,
                };
                page.ask(vec![service], RequestKind::Done)
            }
            ClipboardCall::Wanted if !page.reading_clipboard() => {
                let service = Service::ClipboardRead {
                    selection: Selection::Clipboard,
                    mime_types: clipboard::TEXT
                        .iter()
                        .map(|t| t.to_string_lossy().into_owned())
                        .collect(),
                    limit: CLIPBOARD_LIMIT,
                };
                page.ask(vec![service], RequestKind::ClipboardRead)
            }
            ClipboardCall::Wanted => None,
        };
        if let Some(requests) = request {
            push_requests(&mut self.events, requests);
        }
    }

    /// Gives WebKit back the buffers the compositor released.
    fn release_buffers(&mut self) {
        let Some(platform) = self.platform.as_mut() else {
            return;
        };
        let released = match platform.wayland.dispatch() {
            Ok(released) => released,
            Err(error) => return self.fail(format!("the compositor connection failed: {error}")),
        };
        for key in released {
            self.stats.releases += 1;
            if let Some((view, buffer)) = self.in_flight.remove(&key) {
                if let Some(page) = self.pages.get(&view) {
                    unsafe { ffi::wpe_view_buffer_released(page.wpe(), buffer.as_ptr()) };
                }
            } else if let Some((&view, page)) = self
                .pages
                .iter_mut()
                .find_map(|(view, page)| page.shm.release(key).then_some((view, page)))
            {
                // A frame that waited for a free SHM buffer can be shown now.
                if page.pending.is_some() {
                    push(&mut self.events, EngineEvent::FrameReady { view });
                }
            }
        }
    }

    /// The whole runtime failed: reported once; every call fails from now on.
    fn fail(&mut self, message: String) {
        if self.failed.is_none() {
            self.push(EngineEvent::RuntimeFailed {
                message: message.clone(),
            });
            self.failed = Some(message);
        }
    }

    fn apply_metrics(&mut self, view: ViewId, metrics: Metrics) {
        let Some(page) = self.pages.get_mut(&view) else {
            return;
        };
        if metrics.is_empty() {
            return; // No room: keep the last size (plan §11).
        }
        let size = metrics.rect().size();
        let (width, height) = (size.width as i32, size.height as i32);
        unsafe {
            let toplevel = toplevel(page.wpe());
            ffi::wpe_toplevel_scale_changed(toplevel, metrics.scale().factor());
            ffi::wpe_toplevel_resized(toplevel, width, height);
            ffi::wpe_view_resized(page.wpe(), width, height);
        }
        // A frame of the old geometry would never be shown: WebKit may render the new one now.
        let buffer = metrics.buffer();
        if page
            .pending
            .as_ref()
            .is_some_and(|(frame, _)| frame.size() != (buffer.width, buffer.height))
        {
            let (frame, _) = page.pending.take().expect("checked");
            self.stats.dropped += 1;
            finish_unshown(page.wpe(), &frame);
        }
    }

    fn input(&mut self, view: ViewId, kind: InputKind) {
        let Some(page) = self.pages.get_mut(&view) else {
            return;
        };
        let wpe = page.wpe();
        let events = match kind {
            InputKind::Pointer(event) => {
                if let PointerEvent::Button {
                    state: ButtonState::Pressed,
                    ..
                } = event
                {
                    self.last_input = Some(view);
                }
                page.input.pointer(&page.view, &event).into_iter().collect()
            }
            InputKind::Key(event) => {
                if event.state == ButtonState::Pressed {
                    self.last_input = Some(view);
                }
                page.input.key(&page.view, &event).into_iter().collect()
            }
            InputKind::Focus(FocusEvent::Activated) => {
                unsafe {
                    ffi::wpe_toplevel_state_changed(toplevel(wpe), ffi::WPE_TOPLEVEL_STATE_ACTIVE)
                };
                Vec::new()
            }
            InputKind::Focus(FocusEvent::Deactivated) => {
                unsafe { ffi::wpe_toplevel_state_changed(toplevel(wpe), 0) };
                page.input.release_keys(&page.view)
            }
            InputKind::TextInput(event) => {
                if let Some(context) = page.view.input_method() {
                    let surrounding = page
                        .text_input
                        .as_ref()
                        .and_then(|t| t.surrounding.as_ref());
                    context.apply(&event, surrounding);
                }
                Vec::new()
            }
        };
        for event in events {
            unsafe {
                ffi::wpe_view_event(wpe, event);
                ffi::wpe_event_unref(event);
            }
        }
    }

    fn close(&mut self, view: ViewId) -> bool {
        let Some(page) = self.pages.remove(&view) else {
            return false;
        };
        page.view.set_sink(None);
        if let Some((frame, _)) = &page.pending {
            finish_unshown(page.wpe(), frame);
        }
        self.in_flight.retain(|_, (v, _)| *v != view);
        if self.last_input == Some(view) {
            self.last_input = None;
        }
        // WebKit's requests first (dropping them refuses them), then WebKit's view and its
        // buffers (and our wl_buffers in them), then our surface proxy and SHM buffers, then the
        // lease (sighurt-wayland's targets.rs, "Destruction order").
        let Page {
            web_view,
            view: wpe_view,
            lease,
            surface,
            shm,
            requests,
            held,
            navigator,
            ..
        } = page;
        drop(held);
        drop(requests);
        drop(navigator);
        drop(wpe_view);
        drop(web_view);
        drop(shm);
        drop(surface);
        lease.retire();
        // The last view takes the web context with it (`WebContext`).
        if self.pages.is_empty() {
            let platform = self.platform.as_mut();
            if let Some(context) = platform.and_then(|p| p.context.take()) {
                context.end();
            }
        }
        true
    }
}

/// Reports `commit`: the navigation and the history after it. A new document's requests and
/// held navigations end with the old one: dropping WebKit's objects refuses them.
fn committed(page: &mut Page, events: &mut Vec<EngineEvent>, commit: Commit) {
    let view = page.id;
    if commit.new_document {
        page.requests.clear();
        page.held.clear();
    }
    push(
        events,
        EngineEvent::NavigationCommitted {
            view,
            navigation: commit.navigation,
            url: commit.url,
            document: commit.document,
            history: commit.history,
        },
    );
    if let Some((entries, current)) = commit.entries {
        push(
            events,
            EngineEvent::HistoryChanged {
                view,
                entries,
                current,
            },
        );
    }
}

fn push_requests(events: &mut Vec<EngineEvent>, requests: Vec<ServiceRequest>) {
    for request in requests {
        events.push(EngineEvent::ServiceRequested(request));
    }
}

fn cancelled(view: ViewId, navigation: NavigationId) -> EngineEvent {
    EngineEvent::NavigationFailed {
        view,
        navigation,
        error: NavigationError::Cancelled,
    }
}

/// The input method's view of `page`'s focused field. A password's text never leaves.
fn text_input_state(page: &Page) -> Option<TextInputState> {
    let input = page.text_input.as_ref()?;
    let purpose = page
        .view
        .input_method()
        .map_or(TextPurpose::Normal, |context| context.purpose());
    Some(TextInputState {
        caret: input.caret,
        surrounding: input
            .surrounding
            .clone()
            .filter(|_| purpose != TextPurpose::Password),
        purpose,
        // WebKit's input-method state doesn't say.
        multiline: false,
    })
}

/// Keeps JavaScriptCore in this process away from signals. WebKit runs JavaScriptCore in the
/// UI process too (it holds the values scripts return), and once a VM exists there it installs
/// SIGSEGV and SIGBUS handlers that block every signal while they chain to the next handler.
/// Wasmtime leaves its handler with `longjmp`, which keeps that mask: after one Wasmtime trap
/// under JavaScriptCore's handler, the thread's next trap is a blocked SIGILL that kills the
/// process (docs/rewrite/WPE.md, "Signals"). So this process's JavaScriptCore polls for its VM
/// traps and has no Wasm fault handler; the web processes, where pages run, keep theirs. Must
/// run before WebKit is first used; options are frozen once JavaScriptCore initializes.
fn keep_jsc_off_signals() -> Result<(), EngineError> {
    for (option, value) in [
        (c"usePollingTraps", true),
        (c"useWasmFaultSignalHandler", false),
    ] {
        let mut now = 0;
        let ok = unsafe {
            ffi::jsc_options_set_boolean(option.as_ptr(), i32::from(value)) != 0
                && ffi::jsc_options_get_boolean(option.as_ptr(), &mut now) != 0
        };
        if !ok || (now != 0) != value {
            let message = format!("JavaScriptCore's option {option:?} is not {value}");
            return Err(EngineError::Runtime(message));
        }
    }
    Ok(())
}

/// Takes `{profile}/lock`, creating the directory; `None` if another process holds it or it
/// can't be made, and then WebKit keeps nothing.
fn lock_profile(profile: &Path) -> Option<File> {
    std::fs::create_dir_all(profile).ok()?;
    let lock = File::create(profile.join("lock")).ok()?;
    match lock.try_lock() {
        Ok(()) => Some(lock),
        Err(TryLockError::WouldBlock) => {
            log::warn!(
                "wpe: another process uses the profile {}; this one keeps nothing",
                profile.display()
            );
            None
        }
        Err(TryLockError::Error(_)) => None,
    }
}

/// The views' `WebKitWebContext`, made with the first view and ended with the last, so no web
/// process outlives the last WPE page: WebKit's default context keeps a closed page's web
/// process for up to 30 minutes in case its site is opened again (its process cache, for the
/// web-browser cache model), and after a page has switched processes for another site it keeps a
/// spare one started for the next (prewarming), which only the end of its context shuts down.
/// WPE is the engine for a few sites here, so a process lives as long as a page needs it.
struct WebContext {
    context: glib::Object,
    /// Our handler on its geolocation manager ([`view::refuse_positions`]).
    positions: (glib::Object, glib::SignalHandlerId),
}

impl WebContext {
    /// A context with the document-browser cache model, which leaves out the process cache.
    /// WebKit sizes that cache when a context is made, from the model set then, so the model is
    /// set first (through the default context, which `set_cache_model` doesn't otherwise use).
    fn new() -> Self {
        let context: glib::Object = unsafe {
            ffi::webkit_web_context_set_cache_model(
                ffi::webkit_web_context_get_default(),
                ffi::WEBKIT_CACHE_MODEL_DOCUMENT_BROWSER,
            );
            glib::Object::from_glib_full(ffi::webkit_web_context_new())
        };
        let positions = view::refuse_positions(&context);
        Self { context, positions }
    }

    /// Drops our handler and our reference: WebKit ends the context, and its spare processes,
    /// once its views are gone.
    fn end(self) {
        let (manager, handler) = self.positions;
        manager.disconnect(handler);
    }
}

/// WebKit's network session: persistent in `profile` (caches in `profile/cache`), else
/// ephemeral.
fn network_session(profile: Option<&Path>) -> glib::Object {
    let session = match profile.and_then(|p| {
        let data = CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
        let cache = CString::new(p.join("cache").as_os_str().as_encoded_bytes()).ok()?;
        Some((data, cache))
    }) {
        Some((data, cache)) => unsafe {
            ffi::webkit_network_session_new(data.as_ptr(), cache.as_ptr())
        },
        None => unsafe { ffi::webkit_network_session_new_ephemeral() },
    };
    unsafe { glib::Object::from_glib_full(session) }
}

/// `push`, keeping only the latest replaceable event of a kind for a view.
fn push(events: &mut Vec<EngineEvent>, event: EngineEvent) {
    let replaceable = matches!(
        event,
        EngineEvent::FrameReady { .. }
            | EngineEvent::TitleChanged { .. }
            | EngineEvent::StatusTextChanged { .. }
            | EngineEvent::LoadingChanged { .. }
            | EngineEvent::CursorChanged { .. }
            | EngineEvent::HistoryChanged { .. }
            | EngineEvent::TextInputChanged { .. }
    );
    if replaceable {
        let kind = std::mem::discriminant(&event);
        events.retain(|e| std::mem::discriminant(e) != kind || e.view() != event.view());
    }
    events.push(event);
}

/// Tells WebKit a frame it will never see shown is done and free.
fn finish_unshown(view: *mut ffi::WPEView, frame: &Buffer) {
    unsafe {
        ffi::wpe_view_buffer_rendered(view, frame.as_ptr());
        ffi::wpe_view_buffer_released(view, frame.as_ptr());
    }
}

fn toplevel(view: *mut ffi::WPEView) -> *mut ffi::WPEToplevel {
    let view: glib::Object = unsafe { from_glib_none(view.cast::<glib::gobject_ffi::GObject>()) };
    let toplevel: glib::Object = view.property("toplevel");
    toplevel.as_ptr().cast()
}

/// A CSS cursor name as the api's cursor.
fn cursor(name: &str) -> Cursor {
    const ALL: [Cursor; 35] = [
        Cursor::Default,
        Cursor::None,
        Cursor::ContextMenu,
        Cursor::Help,
        Cursor::Pointer,
        Cursor::Progress,
        Cursor::Wait,
        Cursor::Cell,
        Cursor::Crosshair,
        Cursor::Text,
        Cursor::VerticalText,
        Cursor::Alias,
        Cursor::Copy,
        Cursor::Move,
        Cursor::NoDrop,
        Cursor::NotAllowed,
        Cursor::Grab,
        Cursor::Grabbing,
        Cursor::EResize,
        Cursor::NResize,
        Cursor::NeResize,
        Cursor::NwResize,
        Cursor::SResize,
        Cursor::SeResize,
        Cursor::SwResize,
        Cursor::WResize,
        Cursor::EwResize,
        Cursor::NsResize,
        Cursor::NeswResize,
        Cursor::NwseResize,
        Cursor::ColResize,
        Cursor::RowResize,
        Cursor::AllScroll,
        Cursor::ZoomIn,
        Cursor::ZoomOut,
    ];
    ALL.into_iter()
        .find(|c| c.css_name() == name)
        .unwrap_or_default()
}

unsafe extern "C" fn script_done(
    source: *mut glib::gobject_ffi::GObject,
    result: *mut ffi::GAsyncResult,
    data: glib::ffi::gpointer,
) {
    let data = unsafe { Box::from_raw(data.cast::<(Weak<RefCell<view::Inbox>>, ViewId)>()) };
    let (inbox, view) = *data;
    let mut error = std::ptr::null_mut();
    let value = unsafe {
        ffi::webkit_web_view_evaluate_javascript_finish(source.cast(), result, &mut error)
    };
    let outcome = if value.is_null() {
        let error: glib::Error = unsafe { from_glib_full(error) };
        Err(error.message().to_owned())
    } else {
        let text: glib::GString = unsafe { from_glib_full(ffi::jsc_value_to_string(value)) };
        unsafe { glib::gobject_ffi::g_object_unref(value.cast()) };
        Ok(text.to_string())
    };
    if let Some(inbox) = inbox.upgrade() {
        inbox.borrow_mut().notes.push((view, Note::Script(outcome)));
    }
}

impl EngineRuntime for WpeRuntime {
    type Target = TargetLease;
    type FramePermit = FramePermit;

    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        lease: TargetLease,
    ) -> Result<(), EngineError> {
        self.alive()?;
        if self.pages.contains_key(&view) {
            return Err(EngineError::Runtime(format!("{view:?} is open already")));
        }
        if lease.mode() != PresentMode::Gl {
            return Err(EngineError::Unsupported(
                "WPE views present on native targets",
            ));
        }
        let inbox = Rc::clone(&self.inbox);
        let platform = self.platform(&lease)?;
        // SAFETY: the proxy is dropped before the lease (`close`).
        let surface = match unsafe { lease.gl_handles() }
            .map(|h| h.window_handle().map(|w| w.as_raw()))
        {
            Some(Ok(RawWindowHandle::Wayland(h))) => unsafe { platform.wayland.surface(h.surface) },
            _ => Err("no wl_surface".to_string()),
        }
        .map_err(EngineError::Runtime)?;
        let shm = platform.wayland.shm_buffers();
        let web_view =
            glib::Object::builder_with_type(unsafe { from_glib(ffi::webkit_web_view_get_type()) })
                .property("display", platform.display.clone())
                .property(
                    "web-context",
                    &platform.context.get_or_insert_with(WebContext::new).context,
                )
                .property("network-session", platform.session.clone())
                .build();
        let settings: glib::Object =
            unsafe { from_glib_none(ffi::webkit_web_view_get_settings(web_view.as_ptr().cast())) };
        // Camera and microphone reach the page only through the browser's permission service;
        // fullscreen waits for an api to coordinate it with the chrome (§17).
        settings.set_property("enable-media-stream", true);
        settings.set_property("enable-fullscreen", false);
        if let Some(agent) = &self.config.user_agent {
            settings.set_property("user-agent", agent);
        }
        let wpe: *mut ffi::WPEView =
            unsafe { ffi::webkit_web_view_get_wpe_view(web_view.as_ptr().cast()) };
        let wpe_view: View =
            unsafe { glib::Object::from_glib_none(wpe.cast::<glib::gobject_ffi::GObject>()) }
                .downcast()
                .map_err(|_| {
                    EngineError::Runtime("WebKit made a view of another platform".into())
                })?;
        let sink = Rc::downgrade(&inbox);
        wpe_view.set_sink(Some(Rc::new(move |call| {
            let Some(inbox) = sink.upgrade() else {
                return;
            };
            let mut inbox = inbox.borrow_mut();
            match call {
                Call::Frame(buffer, damage) => {
                    inbox.notes.push((view, Note::Frame(buffer, damage)))
                }
                Call::Cursor(name) => inbox.facts.entry(view).or_default().cursor = Some(name),
                Call::Ime(ime) => inbox.notes.push((view, Note::Ime(ime))),
            }
        })));
        view::connect(&web_view, &inbox, view);
        unsafe { ffi::webkit_web_view_set_zoom_level(web_view.as_ptr().cast(), options.zoom) };
        let metrics = lease.metrics();
        self.pages.insert(
            view,
            Page {
                id: view,
                web_view,
                view: wpe_view,
                surface,
                shm,
                input: Input::default(),
                pending: None,
                presented: None,
                navigator: Default::default(),
                loading: false,
                requests: Vec::new(),
                held: Vec::new(),
                text_input: None,
                lease,
            },
        );
        self.apply_metrics(view, metrics);
        let wpe = self.pages[&view].wpe();
        unsafe {
            match options.visible {
                true => ffi::wpe_view_map(wpe),
                false => ffi::wpe_view_unmap(wpe),
            }
        }
        self.push(EngineEvent::ViewReady { view });
        self.handle_notes();
        Ok(())
    }

    fn command(&mut self, view: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.alive()?;
        let page = self
            .pages
            .get_mut(&view)
            .ok_or(EngineError::UnknownView(view))?;
        let web = page.web();
        match command {
            EngineCommand::Navigate { navigation, url } => {
                let url =
                    CString::new(url).map_err(|_| EngineError::Unsupported("NUL in a URL"))?;
                page.navigator.request(navigation);
                unsafe { ffi::webkit_web_view_load_uri(web, url.as_ptr()) };
            }
            EngineCommand::Reload { navigation } => {
                if page.navigator.document().is_none() {
                    // Nothing to reload: WebKit would do nothing.
                    self.push(cancelled(view, navigation));
                    return Ok(());
                }
                page.navigator.request(navigation);
                unsafe { ffi::webkit_web_view_reload(web) };
            }
            EngineCommand::Stop => unsafe { ffi::webkit_web_view_stop_loading(web) },
            EngineCommand::Traverse { navigation, delta } => {
                let item = unsafe {
                    let list = ffi::webkit_web_view_get_back_forward_list(web);
                    match delta {
                        0 => std::ptr::null_mut(),
                        delta => ffi::webkit_back_forward_list_get_nth_item(list, delta),
                    }
                };
                if item.is_null() {
                    self.push(cancelled(view, navigation));
                    return Ok(());
                }
                page.navigator.request(navigation);
                unsafe { ffi::webkit_web_view_go_to_back_forward_list_item(web, item) };
            }
            EngineCommand::SetMetrics(metrics) => self.apply_metrics(view, metrics),
            EngineCommand::SetVisibility(visible) => {
                unsafe {
                    match visible {
                        true => ffi::wpe_view_map(page.wpe()),
                        false => ffi::wpe_view_unmap(page.wpe()),
                    }
                }
                if visible && page.pending.is_some() {
                    self.push(EngineEvent::FrameReady { view });
                }
            }
            EngineCommand::SetFocus(focused) => {
                let wpe = page.wpe();
                if focused {
                    unsafe { ffi::wpe_view_focus_in(wpe) };
                } else {
                    for event in page.input.release_keys(&page.view) {
                        unsafe {
                            ffi::wpe_view_event(wpe, event);
                            ffi::wpe_event_unref(event);
                        }
                    }
                    unsafe { ffi::wpe_view_focus_out(wpe) };
                }
            }
            EngineCommand::SetZoom(zoom) => unsafe {
                ffi::webkit_web_view_set_zoom_level(web, zoom)
            },
            EngineCommand::Input(event) => self.input(view, event.kind),
            EngineCommand::AnswerNavigation { request, allow } => {
                if let Some(index) = page.held.iter().position(|(r, ..)| *r == request) {
                    let (_, navigation, since) = page.held.remove(index);
                    log::debug!(
                        target: TIMING,
                        "wpe-timing navigation-answered view={} held_ms={:.3}",
                        view.serial(),
                        since.elapsed().as_secs_f64() * 1e3
                    );
                    if allow {
                        navigation.allow();
                    }
                }
            }
            EngineCommand::ResolveServiceRequest { request, result } => {
                match page.answer(request, result) {
                    Answer::None => {}
                    Answer::Paste(text) => {
                        if let Some(clipboard) =
                            self.platform.as_ref().and_then(|p| p.display.clipboard())
                        {
                            clipboard.grant(text);
                            unsafe {
                                ffi::webkit_web_view_execute_editing_command(web, c"Paste".as_ptr())
                            };
                        }
                    }
                    Answer::Withdraw(requests) => {
                        for request in requests {
                            self.push(EngineEvent::ServiceCancelled { view, request });
                        }
                    }
                }
            }
            // Content as structure is not implemented yet (TUI.md §8, phase 4).
            EngineCommand::SetSemantics(_) | EngineCommand::Semantic { .. } => {
                return Err(EngineError::Unsupported("semantics"))
            }
        }
        self.handle_notes();
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.shut_down {
            return PumpStatus::Idle;
        }
        let mut next = None;
        if self.failed.is_none() {
            self.release_buffers();
            next = self.glib.as_mut().and_then(GlibLoop::iterate);
            self.handle_notes();
            if let Some(clipboard) = self.platform.as_ref().and_then(|p| p.display.clipboard()) {
                clipboard.expire();
            }
        }
        events.append(&mut self.events);
        match next {
            None => PumpStatus::Idle,
            Some(at) if at <= Instant::now() => PumpStatus::Pending { deadline: None },
            Some(at) => PumpStatus::Pending { deadline: Some(at) },
        }
    }

    fn present(
        &mut self,
        view: ViewId,
        permit: FramePermit,
    ) -> Result<PresentOutcome, EngineError> {
        self.alive()?;
        let start = Instant::now();
        self.release_buffers();
        self.alive()?;
        let page = self
            .pages
            .get_mut(&view)
            .ok_or(EngineError::UnknownView(view))?;
        let epoch = permit.metrics().epoch();
        let unchanged = match page.presented == Some(epoch) {
            true => PresentOutcome::Unchanged,
            false => PresentOutcome::NotReady,
        };
        if !permit.is_current() || page.pending.is_none() {
            permit.finish(unchanged);
            return Ok(unchanged);
        }
        let (frame, damage) = page.pending.take().expect("checked");
        let buffer = permit.metrics().buffer();
        let size = (buffer.width, buffer.height);
        let (width, height) = frame.size();
        if width.abs_diff(size.0) > 1 || height.abs_diff(size.1) > 1 {
            self.stale_frames += 1;
            self.stats.dropped += 1;
            finish_unshown(page.wpe(), &frame);
            permit.finish(unchanged);
            return Ok(unchanged);
        }
        if (width, height) != size {
            self.rounded_frames += 1;
        }
        let size = (width, height);
        let platform = self.platform.as_mut().expect("a page has a platform");
        let imported = if frame.is_dma_buf() {
            platform
                .wayland
                .dmabuf_buffer(&frame)
                .map(|b| Some((b, false)))
        } else {
            page.shm
                .copy(&mut platform.wayland, &frame)
                .map(|b| b.map(|b| (b, true)))
        }
        .map_err(|message| EngineError::View { view, message })?;
        let Some(((wl_buffer, key), copied)) = imported else {
            // Every SHM buffer of ours is still with the compositor: keep the frame.
            page.pending = Some((frame, damage));
            permit.finish(unchanged);
            return Ok(unchanged);
        };
        page.surface.attach(Some(&wl_buffer), 0, 0);
        present::damage(&page.surface, if copied { &[] } else { &damage }, size);
        page.surface.commit();
        page.presented = Some(epoch);
        unsafe { ffi::wpe_view_buffer_rendered(page.wpe(), frame.as_ptr()) };
        if copied {
            self.stats.shm_copies += 1;
            self.stats.shm_bytes += u64::from(size.0) * u64::from(size.1) * 4;
            unsafe { ffi::wpe_view_buffer_released(page.wpe(), frame.as_ptr()) };
        } else {
            self.stats.dmabuf_frames += 1;
            self.in_flight.insert(key, (view, frame));
        }
        permit.finish(PresentOutcome::Submitted);
        let took = start.elapsed();
        self.max_present = self.max_present.max(took);
        self.total_present += took;
        self.handle_notes();
        // Events reach the owner only through `pump`: ask for one.
        if !self.events.is_empty() {
            self.waker.wake();
        }
        Ok(PresentOutcome::Submitted)
    }

    fn close_view(&mut self, view: ViewId) {
        if self.close(view) {
            self.push(EngineEvent::ViewClosed { view });
        }
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        let views: Vec<ViewId> = self.pages.keys().copied().collect();
        for view in views {
            self.close(view);
        }
        self.in_flight.clear();
        // Let WebKit finish tearing its views down (their buffers hold our wl_buffers).
        if let Some(glib) = self.glib.as_mut() {
            for _ in 0..8 {
                glib.iterate();
            }
        }
        *self.inbox.borrow_mut() = view::Inbox::default();
        self.events.clear();
        if let Some(platform) = self.platform.as_mut() {
            for (object, handler) in platform.handlers.drain(..) {
                object.disconnect(handler);
            }
        }
        self.platform = None;
        self.glib = None;
        self.profile_lock = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_cursor_names_map_onto_the_apis() {
        assert_eq!(cursor("pointer"), Cursor::Pointer);
        assert_eq!(cursor("ew-resize"), Cursor::EwResize);
        assert_eq!(cursor("zoom-out"), Cursor::ZoomOut);
        assert_eq!(cursor("no-such-cursor"), Cursor::Default);
    }

    #[test]
    fn replaceable_events_keep_only_the_latest_per_view() {
        let (a, b) = (ViewId::next(), ViewId::next());
        let mut events = Vec::new();
        let title = |view, t: &str| EngineEvent::TitleChanged {
            view,
            title: Some(t.into()),
        };
        push(&mut events, title(a, "1"));
        push(&mut events, EngineEvent::ViewReady { view: a });
        push(&mut events, title(b, "x"));
        push(&mut events, title(a, "2"));
        assert_eq!(
            events,
            [
                EngineEvent::ViewReady { view: a },
                title(b, "x"),
                title(a, "2")
            ]
        );
    }
}
