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
//! Servo keeps its profile (cookies, local storage, HSTS, HTTP authentication) in the directory
//! the executable passes, written by this runtime only: it holds `{profile}/lock` from before
//! Servo is built until Servo has shut down (Servo writes the profile while it shuts down), and
//! a second process that finds the lock taken runs Servo without a profile (M0, decision 26).
//!
//! A crashed page's view loads its next navigation in a new WebView on the same context: Servo
//! 0.5 keeps sending the crashed site's documents to the dead script thread.
//!
//! [`EngineRuntime::shutdown`] closes in the order M1 established: the WebViews, then Servo
//! (which spins until its constellation, WebGL and paint threads are done), then each view's
//! GL context, then the EGL display, then the target leases; the host releases its Wayland
//! objects after that. After a page crash or a lost backend, Servo 0.5 never finishes shutting
//! down (its constellation waits for the dead script thread's pipelines), so the runtime leaves
//! it running instead of hanging: [`ServoDiagnostics::leaked`].
//!
//! Must not own: process lifetime, logging or TLS setup, the allocator (the executable): no
//! `process::exit`, `_exit`, panic hook, logger or crypto provider here.

use std::collections::{HashMap, VecDeque};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use servo::{
    EmbedderControl, EventLoopWaker, LoadStatus, Opts, PermissionFeature, Preferences,
    RenderingContext, Servo, ServoBuilder, WebViewId,
};
use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, ViewOptions,
};
use sighurt_api::events::{EngineEvent, OpenDisposition};
use sighurt_api::ids::{NavigationId, RequestId, ViewId};
use sighurt_api::services::{
    ClipboardData, Permission, PickMode, Selection, Service, ServiceReply, ServiceRequest,
    ServiceResult,
};
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};
use url::Url;

use crate::delegate::{Delegate, Note};
use crate::input::text_input_state;
use crate::navigation::{Navigator, Request};
use crate::presentation::{physical, Egl, GlInfo, Readbacks};
use crate::view::{Pending, View};

/// Events handed over per pump; the rest wait for the next.
const MAX_EVENTS_PER_PUMP: usize = 256;
/// Service requests a view may have outstanding; beyond them Servo's requests are refused here.
const MAX_REQUESTS: usize = 16;
/// Pop-ups a view may have waiting for their first navigation.
const MAX_POPUPS: usize = 4;
/// The most clipboard text a page reads.
const CLIPBOARD_LIMIT: usize = 4 << 20;
const TEXT_PLAIN: &str = "text/plain;charset=utf-8";

/// What the executable decides for the Servo runtime.
#[derive(Clone, Debug, Default)]
pub struct ServoConfig {
    /// Servo's profile directory: `{data_dir}/sighurt/servo` in `sig`. `None` keeps nothing.
    pub profile: Option<PathBuf>,
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
    /// Page readbacks to the CPU (`RenderingContext::read_to_image`). 0 on the display path.
    pub read_to_image: u64,
    /// `glReadPixels`/`glGetTexImage` calls, with [`ServoConfig::audit_gl`]. 0 on the display
    /// path.
    pub gl_reads: Option<u64>,
    /// Frames swapped into targets.
    pub frames: u64,
    /// Open views.
    pub views: usize,
    /// Servo was left running when the runtime shut down (after a page crash or a lost
    /// backend, it can't shut down): its threads outlive the runtime, the profile wasn't
    /// written, and the executable should end the process without running exit handlers.
    pub leaked: bool,
}

/// Who a Servo WebView is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    View(ViewId),
    /// A hidden pop-up the view's page opened, until its first navigation.
    Popup(ViewId),
}

/// The events a pump hands over, keeping only the latest replaceable event per view.
#[derive(Debug, Default)]
struct Events(VecDeque<EngineEvent>);

impl Events {
    fn push(&mut self, event: EngineEvent) {
        if replaceable(&event) {
            let kind = std::mem::discriminant(&event);
            let view = event.view();
            self.0
                .retain(|old| std::mem::discriminant(old) != kind || old.view() != view);
        }
        self.0.push_back(event);
    }
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

/// The one Servo runtime of the process. See the module documentation.
pub struct ServoRuntime {
    /// `None` once shut down.
    servo: Option<Servo>,
    delegate: Rc<Delegate>,
    waker: EngineWaker,
    views: HashMap<ViewId, View>,
    owners: HashMap<WebViewId, Owner>,
    /// Views closed while something still held their context: kept, with their lease, until
    /// it is released.
    closing: Vec<View>,
    /// Servo's keyboard focus is one WebView at a time; `blur` blurs whichever has it.
    servo_focus: Option<ViewId>,
    events: Events,
    readbacks: Readbacks,
    gl: Option<GlInfo>,
    egl: Option<Egl>,
    frames: u64,
    profile: Option<PathBuf>,
    /// Held from before Servo is built until it has shut down.
    profile_lock: Option<File>,
    failed: Option<String>,
    /// A page's script thread panicked. Servo 0.5 then never finishes shutting down: its
    /// constellation waits for the dead thread's pipelines to exit (`Constellation::run`).
    page_crashed: bool,
    leaked: bool,
}

impl ServoRuntime {
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
            .preferences(preferences())
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
            readbacks,
            gl: None,
            egl: None,
            frames: 0,
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
                let info = context.info(&egl);
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
                self.events.push(EngineEvent::ViewReady { view: id });
            }
            self.apply_focus(id);
        }
        Ok(())
    }

    /// Loads `url` for `navigation` in a new WebView, after the page crashed. Servo 0.5 keeps
    /// sending the crashed site's documents to the dead script thread ("crash page crashed"), so
    /// the old WebView can't load again. The new one shares the view's context and target, and
    /// its history starts with this navigation.
    fn restart(&mut self, id: ViewId, navigation: NavigationId, url: Url) {
        let view = self.views.get_mut(&id).expect("checked by the caller");
        let old = view.webview.take();
        for popup in view.popups.drain(..) {
            self.owners.remove(&popup.id());
        }
        view.crashed = false;
        view.queued = None;
        view.navigator = Navigator::default();
        view.navigator.request(navigation, Request::Navigate);
        view.first = Some(url);
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
        self.events
            .push(EngineEvent::ViewFailed { view: id, message });
        self.close_view(id);
    }

    /// Gives or takes Servo's keyboard focus for `id`: the view has it while it has the
    /// browser's focus, is shown and the window is active. Servo's `blur` blurs whichever
    /// WebView has focus, so only the view that has it blurs.
    fn apply_focus(&mut self, id: ViewId) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let Some(webview) = &view.webview else {
            return;
        };
        let wanted = view.focused && view.visible && view.active;
        if wanted && self.servo_focus != Some(id) {
            webview.focus();
            self.servo_focus = Some(id);
        } else if !wanted && self.servo_focus == Some(id) {
            let releases = view.keys.release_all();
            view.send(releases);
            view.composition.reset();
            webview.blur();
            self.servo_focus = None;
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
        self.events
            .push(EngineEvent::ServiceRequested(ServiceRequest {
                id: request,
                view: id,
                document,
                origin: view.origin(),
                service,
            }));
    }

    /// Turns what Servo's delegates queued into events and view state.
    fn absorb(&mut self) {
        if self.delegate.backend_lost() && self.failed.is_none() {
            let message = "Servo lost its backend; web pages need a browser restart".to_string();
            log::error!("servo: {message}");
            self.events.push(EngineEvent::RuntimeFailed {
                message: message.clone(),
            });
            self.failed = Some(message);
        }
        // Each note, and whether it is a `Started` already counted with the history change it
        // belongs to: the two arrive on different channels, so within one batch the history
        // change may come first.
        let mut notes: Vec<_> = self
            .delegate
            .take()
            .into_iter()
            .map(|(webview, note)| Some((webview, note, false)))
            .collect();
        for index in 0..notes.len() {
            let Some((webview, note, counted)) = notes[index].take() else {
                continue;
            };
            match self.owners.get(&webview).copied() {
                Some(Owner::View(view)) => {
                    let mut started_later = false;
                    if matches!(note, Note::History(..)) {
                        let later = notes[index + 1..].iter_mut().flatten().find(|(id, n, c)| {
                            *id == webview && !*c && matches!(n, Note::Load(LoadStatus::Started))
                        });
                        if let Some((.., counted)) = later {
                            *counted = true;
                            started_later = true;
                        }
                    }
                    self.view_note(view, note, started_later || counted);
                }
                Some(Owner::Popup(parent)) => self.popup_note(parent, webview, note),
                // A WebView being closed; what it says no longer matters.
                None => {}
            }
        }
    }

    /// `counted`: for a history change, a `Started` that belongs to it comes later in the
    /// batch; for a `Started`, it was counted with its history change already.
    fn view_note(&mut self, id: ViewId, note: Note, counted: bool) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        match note {
            Note::FrameReady => view.frame_ready = true,
            Note::Title(title) => self
                .events
                .push(EngineEvent::TitleChanged { view: id, title }),
            Note::Status(text) => self
                .events
                .push(EngineEvent::StatusTextChanged { view: id, text }),
            Note::Cursor(cursor) => self.events.push(EngineEvent::CursorChanged {
                view: id,
                cursor: crate::input::cursor(cursor),
            }),
            Note::Load(status) => {
                view.navigator
                    .load_status(status == LoadStatus::Started, counted);
                let loading = status != LoadStatus::Complete;
                if loading != view.loading {
                    view.loading = loading;
                    self.events
                        .push(EngineEvent::LoadingChanged { view: id, loading });
                }
            }
            Note::History(entries, current) => {
                let entries: Vec<String> = entries.into_iter().map(String::from).collect();
                let before = view.document();
                let commit = view
                    .navigator
                    .history_changed(entries.clone(), current, counted);
                if let Some(commit) = commit {
                    self.events.push(EngineEvent::NavigationCommitted {
                        view: id,
                        navigation: commit.navigation,
                        url: commit.url,
                        document: commit.document,
                    });
                }
                if view.document() != before {
                    // Servo has no load in flight any more: the one waiting goes now.
                    view.in_flight = false;
                    if let (Some((navigation, url)), Some(webview)) =
                        (view.queued.take(), &view.webview)
                    {
                        view.navigator.request(navigation, Request::Navigate);
                        webview.load(url);
                        view.in_flight = true;
                    }
                    // Nothing asked for by the old document carries over; the browser
                    // cancels its requests itself.
                    view.requests.clear();
                    if view.ime.take().is_some() {
                        view.composition.reset();
                        self.events.push(EngineEvent::TextInputChanged {
                            view: id,
                            state: None,
                        });
                    }
                }
                self.events.push(EngineEvent::HistoryChanged {
                    view: id,
                    entries,
                    current,
                });
            }
            Note::Crashed(reason) => {
                view.crashed = true;
                self.page_crashed = true;
                self.events.push(EngineEvent::ViewFailed {
                    view: id,
                    message: format!("The page crashed: {reason}"),
                });
            }
            Note::Closed => {
                if let Some(document) = view.document() {
                    self.events
                        .push(EngineEvent::CloseRequested { view: id, document });
                }
            }
            Note::Navigation(request) => {
                if view.loads_itself(&request.url) {
                    request.allow();
                    return;
                }
                let url = request.url.to_string();
                request.deny();
                if let Some(document) = view.document() {
                    self.events.push(EngineEvent::OpenRequested {
                        view: id,
                        document,
                        url,
                        disposition: OpenDisposition::CurrentView,
                    });
                }
            }
            Note::CreateNew(request) => {
                let Some(context) = view.context.clone() else {
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
                // It shares the page's rendering context and must never paint over it.
                popup.hide();
                self.owners.insert(popup.id(), Owner::Popup(id));
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
                self.events.push(EngineEvent::TextInputChanged {
                    view: id,
                    state: Some(state),
                });
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
                    self.events.push(EngineEvent::TextInputChanged {
                        view: id,
                        state: None,
                    });
                }
                let picker = view.requests.iter().position(
                    |(_, pending)| matches!(pending, Pending::Files(p) if p.id() == control),
                );
                if let Some(index) = picker {
                    let (request, _) = view.requests.remove(index);
                    self.events
                        .push(EngineEvent::ServiceCancelled { view: id, request });
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
                    self.events.push(EngineEvent::LoadingChanged {
                        view: id,
                        loading: false,
                    });
                }
            }
        }
    }

    /// A pop-up's first navigation goes to the browser as a new tab, and the pop-up closes.
    /// Nothing else it asks for is granted.
    fn popup_note(&mut self, parent: ViewId, popup: WebViewId, note: Note) {
        let url = match note {
            Note::Navigation(request) => {
                let url = request.url.clone();
                request.deny();
                url
            }
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
        if let Some(document) = view.document() {
            self.events.push(EngineEvent::OpenRequested {
                view: parent,
                document,
                url: url.into(),
                disposition: OpenDisposition::NewView,
            });
        }
    }

    /// Drops what Servo made for a view, then the context, then the lease. Returns the view
    /// back if something still holds the context, so its lease isn't dropped under it.
    fn release(&mut self, mut view: View) -> Option<View> {
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
                None => self.events.push(EngineEvent::ViewClosed { view: id }),
            }
        }
    }
}

impl EngineRuntime for ServoRuntime {
    type Target = TargetLease;
    type FramePermit = FramePermit;

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            history_traversal: true,
            text_input: true,
            page_zoom: true,
            ..EngineCapabilities::default()
        }
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: TargetLease,
    ) -> Result<(), EngineError> {
        self.alive()?;
        if target.mode() != PresentMode::Gl {
            return Err(EngineError::Unsupported(
                "presenting into a shared-memory target",
            ));
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
        let cancelled = |navigation| EngineEvent::NavigationFailed {
            view: id,
            navigation,
            error: sighurt_api::events::NavigationError::Cancelled,
        };
        match command {
            EngineCommand::Navigate { navigation, url } => {
                let url = match Url::parse(&url) {
                    Ok(url) => url,
                    Err(error) => {
                        self.events.push(EngineEvent::NavigationFailed {
                            view: id,
                            navigation,
                            error: sighurt_api::events::NavigationError::Load(format!(
                                "Not a valid address: {error}"
                            )),
                        });
                        return Ok(());
                    }
                };
                self.events.push(EngineEvent::NavigationStarted {
                    view: id,
                    navigation,
                    url: url.to_string(),
                });
                if view.crashed {
                    self.restart(id, navigation, url);
                    return Ok(());
                }
                match &view.webview {
                    Some(_) if view.in_flight => {
                        if let Some((old, _)) = view.queued.replace((navigation, url)) {
                            self.events.push(cancelled(old));
                        }
                    }
                    Some(webview) => {
                        view.navigator.request(navigation, Request::Navigate);
                        webview.load(url);
                        view.in_flight = true;
                    }
                    None => {
                        view.navigator.request(navigation, Request::Navigate);
                        view.first = Some(url);
                        if let Err(message) = self.start(id) {
                            self.fail_view(id, message);
                        }
                    }
                }
            }
            EngineCommand::Reload { navigation } => match &view.webview {
                Some(_) if view.crashed => {
                    match view.navigator.url().and_then(|url| Url::parse(url).ok()) {
                        Some(url) => self.restart(id, navigation, url),
                        None => self.events.push(cancelled(navigation)),
                    }
                }
                Some(webview) if view.navigator.url().is_some() => {
                    view.navigator.request(navigation, Request::Reload);
                    webview.reload();
                    view.in_flight = true;
                    if !view.loading {
                        view.loading = true;
                        self.events.push(EngineEvent::LoadingChanged {
                            view: id,
                            loading: true,
                        });
                    }
                }
                _ => self.events.push(cancelled(navigation)),
            },
            EngineCommand::Stop => {
                if let Some((old, _)) = view.queued.take() {
                    self.events.push(cancelled(old));
                }
                // Servo 0.5 has no stop: `window.stop()` stops the document's loading, but
                // can't cancel a navigation whose response hasn't arrived.
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
                        if delta < 0 {
                            webview.go_back(steps);
                        } else {
                            webview.go_forward(steps);
                        }
                    }
                    _ => self.events.push(cancelled(navigation)),
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
        }
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.servo.is_some() && self.failed.is_none() {
            self.spin();
            self.release_closing();
            for view in self.views.values_mut() {
                let wants = view.frame_ready || view.needs_submit || view.driver.is_waiting();
                if wants && view.visible && view.webview.is_some() && !view.frame_requested {
                    view.frame_requested = true;
                    self.events.push(EngineEvent::FrameReady { view: view.id });
                }
            }
        }
        let count = self.events.0.len().min(MAX_EVENTS_PER_PUMP);
        events.extend(self.events.0.drain(..count));
        if self.events.0.is_empty() {
            PumpStatus::Idle
        } else {
            PumpStatus::Pending { deadline: None }
        }
    }

    fn present(&mut self, id: ViewId, permit: FramePermit) -> Result<PresentOutcome, EngineError> {
        self.alive()?;
        let view = self.views.get(&id).ok_or(EngineError::UnknownView(id))?;
        debug_assert_eq!(permit.view(), Some(id), "a permit for another view");
        let driver = Rc::clone(&view.driver);
        let spun = driver.is_waiting();
        if spun {
            // The host issues a permit once the last frame's callback has come: the frame
            // Servo waits for starts now, and its content is usually built already.
            driver.start_frame();
            self.spin();
        }
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        view.frame_requested = false;
        let epoch = permit.metrics().epoch();
        let outcome = match (&view.webview, &view.context) {
            (Some(webview), Some(context))
                if view.visible && context.is_bound() && permit.is_current() =>
            {
                if view.frame_ready || view.needs_submit || view.presented != Some(epoch) {
                    let size = physical(permit.metrics().buffer());
                    if context.size() != size {
                        webview.resize(size);
                    }
                    webview.paint();
                    context.swap();
                    view.frame_ready = false;
                    view.needs_submit = false;
                    view.presented = Some(epoch);
                    self.frames += 1;
                    PresentOutcome::Submitted
                } else {
                    PresentOutcome::Unchanged
                }
            }
            _ => PresentOutcome::NotReady,
        };
        permit.finish(outcome);
        if spun && !self.events.0.is_empty() {
            // What the spin produced is delivered by the next pump.
            self.waker.wake();
        }
        Ok(outcome)
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
            None => self.events.push(EngineEvent::ViewClosed { view: id }),
        }
    }

    fn shutdown(&mut self) {
        let Some(servo) = self.servo.take() else {
            return;
        };
        let mut views: Vec<View> = self.views.drain().map(|(_, view)| view).collect();
        views.append(&mut self.closing);
        for view in &mut views {
            view.popups.clear();
            view.webview = None;
            view.requests.clear();
        }
        self.owners.clear();
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
        // Servo 0.5 shuts down only in `Drop`: it tells the constellation to exit and spins
        // until the constellation, WebGL and paint threads are done, releasing its clones of
        // the rendering contexts. It writes the profile on the way, under the lock.
        drop(servo);
        let mut contexts_leaked = false;
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
            drop(view);
        }
        if let (Some(egl), false) = (self.egl.take(), contexts_leaked) {
            if let Err(error) = egl.terminate() {
                log::warn!("servo: {error}");
            }
        }
        self.profile_lock = None;
        self.events.0.clear();
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
/// reasons its README records (`gpui-baseline:sighurt-engine-servo/README.md`).
fn preferences() -> Preferences {
    let mut prefs = Preferences::default();
    prefs.user_agent = firefox_user_agent(&prefs.user_agent);
    // Web platform features that Servo 0.5 implements but leaves off, and that common sites use.
    // Without grid, sites like mozilla.org and theguardian.com lose their layout.
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
    // Left off because they break common sites: with `IntersectionObserver` script spins forever
    // and the page stays blank (reddit.com, microsoft.com), a Servo 0.5 layout bug fixed upstream
    // in servo/servo#47693, and with `adoptedStyleSheets` layout panics (microsoft.com).
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
/// Servo already claims to be Firefox 140, but some sites turn away a user agent that isn't
/// exactly Firefox's: web.whatsapp.com asks for "Firefox 115+" instead of showing its login.
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
    }

    /// Servo's constellation can't be killed from outside, so this reports its loss the way
    /// Servo does, through `ServoDelegate::notify_error`, to a real Servo. (Servo initializes
    /// once per process: this is the only unit test that builds one.)
    #[test]
    fn losing_servos_backend_fails_the_runtime_once() {
        use servo::{ServoDelegate, ServoError};
        use sighurt_api::engine::EngineCommand;

        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut runtime = ServoRuntime::new(ServoConfig::default(), EngineWaker::new(|| {}));
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
        events.push(EngineEvent::TitleChanged {
            view: a,
            title: Some("1".into()),
        });
        events.push(EngineEvent::ViewReady { view: a });
        events.push(EngineEvent::TitleChanged {
            view: b,
            title: Some("b".into()),
        });
        events.push(EngineEvent::TitleChanged {
            view: a,
            title: Some("2".into()),
        });
        events.push(EngineEvent::ViewReady { view: a });
        let got: Vec<_> = events.0.into_iter().collect();
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
