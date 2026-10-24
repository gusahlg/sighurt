//! The session: browser state, engine runtimes, and the drive from events to effects.
//!
//! One pass is what the owner loop used to do between the host and the engines. Events go
//! into [`Session::run_core`], which hands each to `sighurt_core::update` and carries the
//! effects out in order. What is not a frontend's job happens here: engine commands, view
//! bookkeeping, services, notices queued back as events, session writes. What is, is asked of
//! the [`Frontend`](crate::frontend::Frontend) before the next effect runs. A view's target
//! exists before any command is sent to that view: [`Effect::OpenView`] starts the engine,
//! asks the frontend for a target, records the view, places it, and only then calls
//! `open_view`.
//!
//! [`Session::pump`] pumps every runtime that was called, woke, or whose deadline passed, and
//! feeds the events back through `run_core`. [`Effect::SiteData`] asks every engine that keeps
//! site data, starting one that has not started; a refusal comes back as
//! [`EngineEvent::SiteDataDone`] so the core always hears. Frame-ready does not go to the core; the frontend
//! is told, and it schedules presentation. [`Session::present`] hands a permit to the view's
//! engine and records the frame. [`Session::shutdown`] runs the part of plan §20 that is not
//! the frontend's own teardown. A bound on that wait ([`Session::set_shutdown_bound`]) ends
//! the process if an engine's shutdown does not return: Servo's `Drop` sleeps until a script
//! thread that a page can keep busy forever (decision 54).
//!
//! Must not own: browser policy (the core), drawing, or windows. The frontend owns those, and
//! the loop that calls in.

use std::collections::{BTreeMap, VecDeque};
use std::marker::PhantomData;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCommand, EngineError, EngineWaker, PresentOutcome, SiteDataRequest,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::BufferSize;
use sighurt_api::ids::{RequestId, TargetId, ViewId};
use sighurt_api::services::{ClipboardData, ServiceError};
use sighurt_core::{BrowserEvent, BrowserState, Effect, Environment};

use crate::diagnostics::{Diagnostics, EngineReport};
use crate::frontend::Frontend;
use crate::persistence::{Paths, SessionWriter};
use crate::registry::{Registry, State};
use crate::services::{Completion, Services};
use crate::view::ViewEntry;

/// Most service completions handled in one pass.
const MAX_COMPLETIONS: usize = 64;
/// How long a changed session waits before it is written, so a burst of changes is one write.
const SESSION_DELAY: Duration = Duration::from_secs(1);
/// How long every frontend lets the engines shut down before the process ends anyway
/// ([`Session::set_shutdown_bound`]). Servo's `Drop` sleeps, 500 µs at a time, until the
/// constellation says every script thread has exited, and a page whose script thread stays busy
/// (chatgpt.com did, for an hour) never gets there. A healthy shutdown is well under it. Servo's
/// profile writer commits cookies at most 5 s apart, so ending here loses at most the last
/// few seconds of cookie changes.
pub const SHUTDOWN_BOUND: Duration = Duration::from_secs(5);

/// A forget `name` did not accept, as the event the core is waiting for.
fn site_refused(engine: &str, request: RequestId, message: String) -> BrowserEvent {
    BrowserEvent::Engine {
        engine: engine.to_owned(),
        event: EngineEvent::SiteDataDone {
            request,
            result: Err(message),
        },
        at: Instant::now(),
    }
}

/// The frontend-neutral half of the owner loop.
///
/// `F` ties the registry's target and permit: they are [`Frontend::Target`] and
/// [`Frontend::Permit`], so a frontend cannot be given a session built for another pair.
///
/// [`Frontend::Target`]: crate::frontend::Frontend::Target
/// [`Frontend::Permit`]: crate::frontend::Frontend::Permit
pub struct Session<F: Frontend> {
    /// Browser policy.
    core: BrowserState,
    /// Events for the core, in order.
    queue: VecDeque<BrowserEvent>,
    /// What the build has.
    registry: Registry<F::Target, F::Permit>,
    /// Live engine views.
    views: BTreeMap<ViewId, ViewEntry>,
    /// Downloads and the page's clipboard bookkeeping. `None` once shutdown has taken it.
    services: Option<Services>,
    /// Counters and the diagnostics file.
    diag: Diagnostics,
    /// Configuration and session paths.
    paths: Paths,
    /// Writes the session, when `[session] restore` is on and the file could be opened.
    session: Option<SessionWriter>,
    /// When the changed session should be written. `None` when nothing is waiting.
    session_due: Option<Instant>,
    /// What configurations are checked against. Kept for reloads.
    env: Environment,
    /// Set when the loop must end, with the exit code.
    quit: Option<ExitCode>,
    /// How long [`Session::shutdown`] may spend in the engines' shutdown. `None` waits as long
    /// as they take, which is what the GUI and the terminal do. `--dump` sets a bound.
    shutdown_bound: Option<Duration>,
    /// When the key event the core is handling happened (`None` for other events): the time its
    /// activation of a page carries.
    handling: Option<Instant>,
    /// Engine wakers and service threads call this.
    wake: Arc<dyn Fn() + Send + Sync>,
    _frontend: PhantomData<fn() -> F>,
}

impl<F: Frontend> Session<F> {
    /// A session that has not started. `wake` is how engines and downloads reach the loop.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core: BrowserState,
        env: Environment,
        registry: Registry<F::Target, F::Permit>,
        services: Services,
        paths: Paths,
        session: Option<SessionWriter>,
        diag: Diagnostics,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Self {
        Self {
            core,
            queue: VecDeque::new(),
            registry,
            views: BTreeMap::new(),
            services: Some(services),
            diag,
            paths,
            session,
            session_due: None,
            env,
            quit: None,
            shutdown_bound: None,
            handling: None,
            wake,
            _frontend: PhantomData,
        }
    }

    /// Browser policy. Effects change it; a frontend only reads it.
    pub fn core(&self) -> &BrowserState {
        &self.core
    }

    /// Events waiting for the core, in order. The frontend pushes what its loop received.
    pub fn queue_mut(&mut self) -> &mut VecDeque<BrowserEvent> {
        &mut self.queue
    }

    /// The build's engines. Starting, pumping and shutting them down go through the session.
    pub fn registry(&self) -> &Registry<F::Target, F::Permit> {
        &self.registry
    }

    /// Live engine views. The frontend places them; the session records them.
    pub fn views(&self) -> &BTreeMap<ViewId, ViewEntry> {
        &self.views
    }

    /// Configuration and session paths.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// What configurations are checked against.
    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// Counters and the diagnostics file.
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diag
    }

    /// Counters and the diagnostics file.
    pub fn diagnostics_mut(&mut self) -> &mut Diagnostics {
        &mut self.diag
    }

    /// Whether the loop must end.
    pub fn has_quit(&self) -> bool {
        self.quit.is_some()
    }

    /// The loop must end with `code`. A later [`Effect::Quit`] does not replace it.
    pub fn set_quit(&mut self, code: ExitCode) {
        self.quit = Some(code);
    }

    /// [`Session::shutdown`] ends the process if the engines' shutdown is still running after
    /// `bound` ([`SHUTDOWN_BOUND`] in every frontend). Unset, it waits the shutdown out.
    pub fn set_shutdown_bound(&mut self, bound: Duration) {
        self.shutdown_bound = Some(bound);
    }

    /// When the changed session should be written. `None` when nothing is waiting.
    pub fn session_due(&self) -> Option<Instant> {
        self.session_due
    }

    /// Downloads and the page's clipboard bookkeeping.
    fn services(&mut self) -> &mut Services {
        self.services.as_mut().expect("services are shut down")
    }

    /// Hands queued events to the core and carries out its effects, until nothing is queued.
    /// Frontend work runs inside each effect, before the next one.
    pub fn run_core(&mut self, frontend: &mut F) {
        while let Some(event) = self.queue.pop_front() {
            self.diag.counters.core_events += 1;
            self.handling = match &event {
                BrowserEvent::Keyboard { at, .. } => Some(*at),
                _ => None,
            };
            let effects = sighurt_core::update(&mut self.core, event);
            self.diag.counters.effects += effects.len() as u64;
            for effect in effects {
                self.apply(frontend, effect);
            }
        }
    }

    /// Carries out one effect of the core.
    fn apply(&mut self, frontend: &mut F, effect: Effect) {
        match effect {
            Effect::OpenView {
                view,
                engine,
                options,
            } => {
                if let Err(error) = self.open_view(frontend, view, &engine, options) {
                    self.queue
                        .push_back(BrowserEvent::OpenFailed { view, error });
                }
            }
            Effect::Engine { view, command } => {
                match &command {
                    EngineCommand::SetVisibility(visible) => {
                        if let Some(entry) = self.views.get_mut(&view) {
                            entry.visible = *visible;
                        }
                        frontend.show(view, *visible);
                    }
                    // Navigation latency is measured only where the user waits for it.
                    EngineCommand::Navigate { .. }
                    | EngineCommand::Reload { .. }
                    | EngineCommand::Traverse { .. } => {
                        let active = self.in_active_tab(view);
                        if let Some(entry) = self.views.get_mut(&view) {
                            entry.navigated = active.then(Instant::now);
                        }
                    }
                    // A frontend that shows the page itself may keep the keys from the engine.
                    EngineCommand::Input(event) => {
                        // The user's own key press, or a binding of it, if the core took it as
                        // that: it stamped the view with this event's time.
                        let activation = self
                            .core
                            .view(view)
                            .and_then(|v| v.activation())
                            .filter(|&at| Some(at) == self.handling);
                        if frontend.page_input(view, event, activation, &mut self.queue) {
                            return;
                        }
                    }
                    _ => {}
                }
                self.engine_command(view, command, true);
            }
            Effect::CloseView { view } => {
                let engine = self.views.get(&view).map(|entry| entry.engine.clone());
                self.forget_view(frontend, view);
                if let Some(runtime) = engine.and_then(|e| self.registry.running(&e)) {
                    runtime.close_view(view);
                }
            }
            Effect::Focus(focus) => frontend.focus(focus, &mut self.queue),
            Effect::ChromeInput(event) => frontend.chrome_input(event, &mut self.queue),
            Effect::ChromeCommand(action) => frontend.chrome_command(action, &mut self.queue),
            Effect::PageCursor(cursor) => {
                let target = self.shown_view().map(|(_, entry)| entry.target);
                frontend.page_cursor(target, cursor);
            }
            Effect::PageTextInput(state) => {
                let shown = self.shown_view().map(|(_, entry)| entry.target);
                frontend.page_text_input(shown, state);
            }
            Effect::Service {
                request, service, ..
            } => {
                let downloads = self
                    .core
                    .config()
                    .downloads()
                    .map(std::path::Path::to_owned);
                let performed =
                    self.services()
                        .perform(frontend, request, service, downloads.as_deref());
                if let Some(result) = performed {
                    self.queue
                        .push_back(BrowserEvent::ServiceDone { request, result });
                }
            }
            Effect::CancelService(request) => self.services().cancel(frontend, request),
            Effect::Prompt {
                request,
                origin,
                permission,
                ..
            } => {
                let who = origin.unwrap_or_else(|| "A page".into());
                frontend.notice(format!(
                    "{who} asked for {permission:?}; sig can't ask you yet, so it was refused."
                ));
                self.queue.push_back(BrowserEvent::PermissionAnswered {
                    request,
                    allow: false,
                });
            }
            Effect::SiteData { engine, request } => self.site_data(engine.as_deref(), request),
            Effect::Notice { message, .. } => frontend.notice(message),
            Effect::RedrawChrome => frontend.redraw(),
            Effect::SaveSession => self.session_changed(),
            Effect::Quit => {
                self.save_session();
                self.quit.get_or_insert(ExitCode::SUCCESS);
            }
        }
    }

    /// Opens `view` on `engine` with a new target, placed before the engine gets it.
    fn open_view(
        &mut self,
        frontend: &mut F,
        view: ViewId,
        engine: &str,
        options: sighurt_api::engine::ViewOptions,
    ) -> Result<(), EngineError> {
        if self.registry.engine(engine).is_none() {
            return Err(EngineError::Runtime(format!(
                "the {engine} engine is not part of this build"
            )));
        }
        let wake = Arc::clone(&self.wake);
        self.registry
            .start(engine, || EngineWaker::new(move || wake()))
            .map_err(EngineError::Runtime)?;
        let (target_id, target) = frontend
            .open_target(view, engine)
            .map_err(|message| EngineError::View { view, message })?;
        self.views.insert(
            view,
            ViewEntry {
                engine: engine.to_owned(),
                target: target_id,
                visible: options.visible,
                navigated: None,
            },
        );
        frontend.place(&self.views, &mut self.diag);
        let runtime = self.registry.running(engine).expect("started above");
        let opened = runtime.open_view(view, options, target);
        if opened.is_err() {
            // The engine dropped the target, and no `ViewClosed` follows a refused open: the
            // frontend forgets the target now, and the next placement leaves it out.
            self.forget_view(frontend, view);
        }
        opened
    }

    /// Starts or stops `view`'s content snapshots ([`Frontend::semantics_changed`]), for a
    /// frontend that shows the page as text. A frontend's choice, not the core's: an engine
    /// without the capability refuses, and the refusal is not the core's to handle.
    pub fn set_semantics(&mut self, view: ViewId, on: bool) {
        self.engine_command(view, EngineCommand::SetSemantics(on), false);
    }

    /// Hands `command` to `view`'s engine. A refusal goes to the core if `report`.
    pub fn engine_command(&mut self, view: ViewId, command: EngineCommand, report: bool) {
        let Some(entry) = self.views.get(&view) else {
            return;
        };
        let engine = entry.engine.clone();
        let result = match self.registry.running(&engine) {
            Some(runtime) => runtime.command(view, command.clone()),
            None => Err(EngineError::Runtime(format!(
                "the {engine} engine is not running"
            ))),
        };
        match result {
            Ok(()) => {}
            Err(error) if report => self.queue.push_back(BrowserEvent::CommandFailed {
                view,
                command,
                error,
            }),
            Err(error) => log::debug!("{engine} refused a command for {view:?}: {error}"),
        }
    }

    /// `view` is gone from its engine: forget it, and tell the frontend to drop its target's
    /// keyboard and text field.
    pub fn forget_view(&mut self, frontend: &mut F, view: ViewId) {
        let Some(entry) = self.views.remove(&view) else {
            return;
        };
        frontend.forget_target(entry.target);
    }

    /// The frontend finished a clipboard read that was not the chrome's own paste.
    pub fn clipboard_done(
        &mut self,
        request: RequestId,
        result: Result<ClipboardData, ServiceError>,
    ) {
        if let Some(result) = self.services().clipboard_read(request, result) {
            self.queue
                .push_back(BrowserEvent::ServiceDone { request, result });
        }
    }

    /// Takes what service threads reported. A status line becomes a notice. Returns whether
    /// there was anything.
    pub fn service_completions(&mut self, frontend: &mut F) -> bool {
        let completions = self.services().completions(MAX_COMPLETIONS);
        let worked = !completions.is_empty();
        for completion in completions {
            match completion {
                Completion::Status(line) => frontend.notice(line),
                Completion::Finished(_) => {}
            }
        }
        worked
    }

    /// Pumps the runtimes that are due and hands their events on. Returns whether any was.
    pub fn pump(&mut self, frontend: &mut F) -> bool {
        let now = Instant::now();
        let due = self.registry.due(now);
        for &engine in &due {
            let mut events = Vec::new();
            let started = Instant::now();
            self.registry.pump(engine, &mut events);
            self.diag.pump(events.len(), started.elapsed());
            for event in events {
                self.engine_event(frontend, engine, event, now);
            }
            self.run_core(frontend);
        }
        !due.is_empty()
    }

    fn engine_event(
        &mut self,
        frontend: &mut F,
        engine: &'static str,
        event: EngineEvent,
        at: Instant,
    ) {
        // Page content the core never reads: checked, then the frontend's. A site-data report
        // is checked the same way; a forget the check refuses still answers, so the core's
        // pending request cannot sit forever.
        let event = match event {
            EngineEvent::SitesListed { .. } | EngineEvent::SiteDataDone { .. } => {
                let mut event = event;
                match event.check() {
                    Ok(()) => event,
                    Err(error) => {
                        self.diag.error(format!(
                            "the {engine} engine sent a site-data report that is not usable: {error}"
                        ));
                        if let EngineEvent::SiteDataDone { request, .. } = event {
                            self.queue.push_back(BrowserEvent::Engine {
                                engine: engine.to_owned(),
                                event: EngineEvent::SiteDataDone {
                                    request,
                                    result: Err(format!("the report was not usable ({error})")),
                                },
                                at,
                            });
                        }
                        return;
                    }
                }
            }
            EngineEvent::SemanticsChanged { view, snapshot } => {
                if self.views.get(&view).is_some_and(|e| e.engine == engine) {
                    match snapshot.check() {
                        Ok(()) => frontend.semantics_changed(view, snapshot),
                        Err(error) => self.diag.error(format!(
                            "the {engine} engine sent malformed page content: {error}"
                        )),
                    }
                }
                return;
            }
            event => event,
        };
        match &event {
            // The core ignores it: presentation is scheduled by the frontend.
            EngineEvent::FrameReady { view } => {
                self.diag.counters.frame_ready += 1;
                if let Some(entry) = self.views.get(view).filter(|e| e.engine == engine) {
                    let target = entry.target;
                    frontend.frame_ready(target);
                }
                return;
            }
            EngineEvent::ViewClosed { view } => {
                if self.views.get(view).is_some_and(|e| e.engine == engine) {
                    self.forget_view(frontend, *view);
                }
            }
            EngineEvent::RuntimeFailed { message } => {
                self.diag
                    .error(format!("the {engine} engine failed: {message}"));
                self.registry.failed(engine, message);
                let gone: Vec<ViewId> = self
                    .views
                    .iter()
                    .filter(|(_, e)| e.engine == engine)
                    .map(|(&v, _)| v)
                    .collect();
                for view in gone {
                    self.forget_view(frontend, view);
                }
                frontend.redraw();
            }
            _ => {}
        }
        self.queue.push_back(BrowserEvent::Engine {
            engine: engine.to_owned(),
            event,
            at,
        });
    }

    /// Sends `request` to `engine`, or to every registered engine that keeps any kind of site
    /// data. An engine that has not started is started. A refusal, or a failure to start, is
    /// reported as [`EngineEvent::SiteDataDone`] so the core always hears back.
    fn site_data(&mut self, engine: Option<&str>, request: SiteDataRequest) {
        let targets: Vec<&'static str> = match engine {
            Some(name) => self
                .registry
                .engine(name)
                .map(|engine| engine.name)
                .into_iter()
                .collect(),
            None => self
                .registry
                .engines()
                .filter(|engine| engine.capabilities.site_data.any())
                .map(|engine| engine.name)
                .collect(),
        };
        // A named engine this build lacks still has to answer.
        if let Some(name) = engine.filter(|_| targets.is_empty()) {
            self.queue.push_back(site_refused(
                name,
                request.request(),
                format!("the {name} engine is not part of this build"),
            ));
            return;
        }
        for name in targets {
            if let Err(message) = self.ask_site(name, request.clone()) {
                self.queue
                    .push_back(site_refused(name, request.request(), message));
            }
        }
    }

    /// Starts `name` if it has not started and hands it `request`.
    fn ask_site(&mut self, name: &str, request: SiteDataRequest) -> Result<(), String> {
        let keeps = match self.registry.engine(name) {
            None => return Err(format!("the {name} engine is not part of this build")),
            Some(engine) => engine.capabilities.site_data.any(),
        };
        if !keeps {
            return Err(format!("the {name} engine keeps no site data"));
        }
        let wake = Arc::clone(&self.wake);
        self.registry
            .start(name, || EngineWaker::new(move || wake()))?;
        let runtime = self.registry.running(name).expect("started above");
        runtime
            .site_data(request)
            .map_err(|error| error.to_string())
    }

    /// The view the page area shows, if an engine's.
    pub fn shown_view(&self) -> Option<(ViewId, &ViewEntry)> {
        self.views
            .iter()
            .find(|(_, entry)| entry.visible)
            .map(|(&view, entry)| (view, entry))
    }

    /// The view whose target is `target`.
    pub fn view_of(&self, target: TargetId) -> Option<ViewId> {
        self.views
            .iter()
            .find(|(_, entry)| entry.target == target)
            .map(|(&view, _)| view)
    }

    /// Whether `view` belongs to the active tab.
    fn in_active_tab(&self, view: ViewId) -> bool {
        let tab = self.core.view(view).map(|v| v.tab());
        let active = self.core.tabs().nth(self.core.active()).map(|t| t.id);
        tab.is_some() && tab == active
    }

    /// Hands the session to the writer now, unless `[session] restore` is off: then the file
    /// stays as it is.
    pub fn save_session(&mut self) {
        self.session_due = None;
        let writer = self.session.as_ref();
        if let Some(writer) = writer.filter(|_| self.core.config().restore_session()) {
            writer.save(self.core.session().to_text());
            self.diag.counters.session_saves += 1;
        }
    }

    /// The core changed what the session holds: save it soon.
    pub fn session_changed(&mut self) {
        self.session_due
            .get_or_insert_with(|| Instant::now() + SESSION_DELAY);
    }

    /// Asks the frontend to place every view. Returns whether the placement changed.
    pub fn place(&mut self, frontend: &mut F) -> bool {
        frontend.place(&self.views, &mut self.diag)
    }

    /// A navigation whose tab the user left is no longer waited for: its first frame is not a
    /// `first-frame` line.
    pub fn release_inactive_navigations(&mut self) {
        let active = self.core.tabs().nth(self.core.active()).map(|tab| tab.id);
        let stale: Vec<ViewId> = self
            .views
            .keys()
            .copied()
            .filter(|&view| self.core.view(view).map(|v| v.tab()) != active)
            .collect();
        for view in stale {
            if let Some(entry) = self.views.get_mut(&view) {
                entry.navigated = None;
            }
        }
    }

    /// The engine `view` runs in, and its tab's position in the tab strip, from 1.
    pub fn view_report(&self, view: ViewId) -> (String, Option<usize>) {
        let engine = self.views[&view].engine.clone();
        let tab_id = self.core.view(view).map(|view| view.tab());
        let tab = tab_id.and_then(|id| {
            self.core
                .tabs()
                .position(|tab| tab.id == id)
                .map(|index| index + 1)
        });
        (engine, tab)
    }

    /// Readbacks made since each running engine's last frame, each as big as `bytes_each`.
    /// Called when a report is built, before the runtimes shut down.
    pub fn account_readbacks(&mut self, bytes_each: u64) {
        let pending: Vec<(&'static str, u64)> = self
            .registry
            .engines()
            .filter_map(|engine| match engine.state() {
                State::Running(runtime) => Some((engine.name, runtime.readbacks())),
                _ => None,
            })
            .collect();
        for (name, total) in pending {
            self.diag.readbacks(name, total, bytes_each);
        }
    }

    /// Every compiled-in engine, as a diagnostics report says it.
    pub fn engine_reports(&self) -> Vec<EngineReport> {
        self.registry
            .engines()
            .map(|engine| {
                let running = match engine.state() {
                    State::Running(runtime) => Some(runtime),
                    _ => None,
                };
                EngineReport {
                    name: engine.name,
                    renderer: engine.renderer,
                    gl: running
                        .and_then(|runtime| runtime.renderer())
                        .and_then(|(mode, gl)| gl.map(|gl| format!("{mode}: {gl}"))),
                    readbacks: running.map_or(0, |runtime| runtime.readbacks()),
                    gl_reads: running.and_then(|runtime| runtime.gl_reads()),
                    state: match engine.state() {
                        State::NotStarted => "not started".into(),
                        State::Running(_) => "running".into(),
                        State::ShutDown => "shut down".into(),
                        State::Failed(why) => format!("failed: {why}"),
                    },
                }
            })
            .collect()
    }

    /// Presents `view` with `permit` and records the `frame`, `renderer`, `first-frame` and
    /// readback lines. `buffer_size` is the permit's buffer, which a frontend knows and the
    /// permit type here does not. `None` when the view is gone or its engine is not running:
    /// the permit is dropped and nothing is counted. Otherwise what the engine's `present` did.
    pub fn present(
        &mut self,
        view: ViewId,
        permit: F::Permit,
        buffer_size: BufferSize,
    ) -> Option<Result<PresentOutcome, EngineError>> {
        let Session {
            views,
            registry,
            diag,
            ..
        } = self;
        let entry = views.get_mut(&view)?;
        let runtime = registry.running(&entry.engine)?;
        let started = Instant::now();
        let outcome = runtime.present(view, permit);
        let took = started.elapsed();
        match &outcome {
            Ok(PresentOutcome::Submitted) => {
                if !diag.knows_renderer(&entry.engine) {
                    if let Some((mode, gl)) = runtime.renderer() {
                        diag.renderer(&entry.engine, mode, gl.as_deref());
                    }
                }
                let buffer = u64::from(buffer_size.width) * u64::from(buffer_size.height) * 4;
                let readback_bytes = diag.readbacks(&entry.engine, runtime.readbacks(), buffer);
                let size = (buffer_size.width, buffer_size.height);
                let paint = runtime.paint_time().unwrap_or(took);
                diag.frame(
                    &entry.engine,
                    view.serial(),
                    size,
                    (paint, took),
                    readback_bytes,
                );
                if let Some(asked) = entry.navigated.take() {
                    diag.first_frame(&entry.engine, asked.elapsed());
                }
            }
            Ok(PresentOutcome::NotReady) => diag.counters.page_not_ready += 1,
            Ok(PresentOutcome::Unchanged) => diag.counters.page_unchanged += 1,
            Err(error) => {
                diag.counters.present_errors += 1;
                let message = format!("{} could not present {view:?}: {error}", entry.engine);
                diag.error(message);
            }
        }
        Some(outcome)
    }

    /// Shuts down the part of plan §20 that is not the frontend tearing itself down, and returns
    /// the exit code. The order is fixed: cancel downloads, write the session and finish the
    /// writer, forget and close every view, `report` (the final diagnostics, while the runtimes
    /// can still say how they render), shut the runtimes down, shut services down, delete scratch
    /// directories. If a runtime had to be left running (decision 54), the frontend is asked to
    /// clean up and the process ends with [`exit_now`]; destructors do not run. Otherwise the
    /// caller tears the frontend down after this returns: the session stays alive until the
    /// caller drops it, so those destructors still run after the frontend's own.
    ///
    /// With [`Session::set_shutdown_bound`], a thread ends the process if this is still inside
    /// the engines' shutdown when the bound passes. Servo's `Drop` sleeps until its script
    /// threads exit, and a page that keeps one busy never lets them. Stdout is flushed first.
    /// That `_exit` does not call `before_forced_exit` (it runs on the bound's thread): the
    /// terminal restores itself before calling this, and the window and `--dump` have nothing
    /// to restore.
    pub fn shutdown(
        &mut self,
        frontend: &mut F,
        report: impl FnOnce(&mut Self, &mut F),
    ) -> ExitCode {
        let code = self.quit.take().unwrap_or(ExitCode::SUCCESS);
        self.services().cancel_downloads();
        if let Some(writer) = self.session.take() {
            if self.core.config().restore_session() {
                writer.save(self.core.session().to_text());
            }
            writer.finish();
        }
        // The session file is written. From here a script thread that will not exit must
        // not hold the process: Servo's `Drop` sleeps until one does. The guard disarms
        // the thread when this function returns.
        let _bound = self
            .shutdown_bound
            .map(|bound| arm_shutdown_bound(bound, process_status(&code)));
        let views: Vec<(ViewId, String)> = self
            .views
            .iter()
            .map(|(&view, entry)| (view, entry.engine.clone()))
            .collect();
        for (view, engine) in views {
            self.forget_view(frontend, view);
            if let Some(runtime) = self.registry.running(&engine) {
                runtime.close_view(view);
            }
        }
        report(self, frontend);
        #[cfg(test)]
        stall_shutdown_for_tests();
        self.registry.shutdown();
        let services = self.services.take().expect("shutdown once");
        services.shutdown();
        // Late, so that no guest still finishing its last call writes there again.
        self.registry.remove_scratch();
        if self.registry.leaked() {
            frontend.before_forced_exit();
            exit_now(code);
        }
        code
    }
}

/// Sleeps for `SIGHURT_TEST_SHUTDOWN_STALL_MS` so a test can hold shutdown inside the bound.
#[cfg(test)]
fn stall_shutdown_for_tests() {
    let Ok(ms) = std::env::var("SIGHURT_TEST_SHUTDOWN_STALL_MS") else {
        return;
    };
    let Ok(ms) = ms.parse::<u64>() else {
        return;
    };
    if ms > 0 {
        thread::sleep(Duration::from_millis(ms));
    }
}

/// Set while a shutdown bound is in force. Dropping it disarms the thread: shutdown returned.
struct ShutdownBound {
    done: Arc<AtomicBool>,
}

impl Drop for ShutdownBound {
    fn drop(&mut self) {
        self.done.store(true, Ordering::Release);
    }
}

/// Ends the process if `shutdown` is still running after `bound`. The thread does not log:
/// the owner thread may hold the log lock inside Servo's `Drop`.
fn arm_shutdown_bound(bound: Duration, status: i32) -> ShutdownBound {
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    if let Err(error) = thread::Builder::new()
        .name("sig-shutdown".into())
        .spawn(move || {
            thread::sleep(bound);
            if flag.swap(true, Ordering::AcqRel) {
                return;
            }
            let message =
                b"sig: an engine did not finish shutting down; ending without exit handlers\n";
            // SAFETY: write(2) of a fixed string. Not the log: the owner may hold that lock.
            unsafe {
                libc::write(
                    libc::STDERR_FILENO,
                    message.as_ptr().cast::<libc::c_void>(),
                    message.len(),
                );
            }
            end_without_destructors(status);
        })
    {
        log::error!("cannot bound shutdown: {error}");
    }
    ShutdownBound { done }
}

fn process_status(code: &ExitCode) -> i32 {
    if *code == ExitCode::SUCCESS {
        0
    } else {
        1
    }
}

/// Ends the process now, with `code`, without exit handlers or destructors: what decision 54
/// prescribes after an engine was left running. Its threads may still use GL contexts, the EGL
/// display and the Wayland connection, which exit handlers (the driver's) and the host's
/// destructors would tear down under them; the compositor cleans up after the closed connection.
///
/// The frontend's [`Frontend::before_forced_exit`](crate::frontend::Frontend::before_forced_exit)
/// has already returned: a terminal restores itself there, and this function does not.
pub fn exit_now(code: ExitCode) -> ! {
    log::error!("an engine could not shut down; sig ends without running exit handlers");
    end_without_destructors(process_status(&code));
}

/// `_exit` after flushing stdout. The page text is already in the kernel when `--dump` takes
/// this path (it writes with write(2)); the flush covers anything still in Rust's buffer.
fn end_without_destructors(status: i32) -> ! {
    let _ = std::io::Write::flush(&mut std::io::stdout());
    // SAFETY: `_exit` ends the process at once. Nothing here relies on a destructor or exit
    // handler running: the session file and the diagnostics are written and closed, and the
    // downloads have ended or their files are deleted.
    unsafe { libc::_exit(status) }
}

#[cfg(all(test, feature = "engine-demo"))]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use sighurt_api::commands::Command;
    use sighurt_api::engine::{
        EngineCapabilities, EngineCommand, EngineError, EngineRuntime, PresentOutcome, PumpStatus,
        SiteDataKinds, SiteDataRequest, SiteKey, ViewOptions,
    };
    use sighurt_api::events::{Cursor, EngineEvent, SiteDataOutcome};
    use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, TargetId, ViewId};
    use sighurt_api::input::{InputEvent, KeyboardFocus, TextInputState};
    use sighurt_api::services::{ClipboardData, Selection, ServiceError};
    use sighurt_core::config::Config;
    use sighurt_core::{BrowserEvent, BrowserState, OpenRequest};

    use super::Session;
    use crate::diagnostics::Diagnostics;
    use crate::frontend::Frontend;
    use crate::persistence::Paths;
    use crate::registry::{Engine, Registry, Running, Runtime, State};
    use crate::services::{Clipboard, Services};
    use crate::view::ViewEntry;

    /// An engine that refuses every view, as Servo does when it can't make a GL context.
    struct Refusing;

    impl EngineRuntime for Refusing {
        type Target = ();
        type FramePermit = ();

        fn capabilities(&self) -> EngineCapabilities {
            EngineCapabilities::default()
        }

        fn open_view(&mut self, view: ViewId, _: ViewOptions, _: ()) -> Result<(), EngineError> {
            Err(EngineError::View {
                view,
                message: "no GL context".into(),
            })
        }

        fn command(&mut self, view: ViewId, _: EngineCommand) -> Result<(), EngineError> {
            Err(EngineError::UnknownView(view))
        }

        fn pump(&mut self, _: &mut Vec<EngineEvent>) -> PumpStatus {
            PumpStatus::Idle
        }

        fn present(&mut self, view: ViewId, _: ()) -> Result<PresentOutcome, EngineError> {
            Err(EngineError::UnknownView(view))
        }

        fn close_view(&mut self, _: ViewId) {}

        fn shutdown(&mut self) {}
    }

    impl Running<(), ()> for Refusing {}

    /// Keeps a record per target, as the terminal frontend does, and counts what it forgets.
    #[derive(Default)]
    struct Recording {
        targets: BTreeMap<TargetId, ViewId>,
        forgotten: Vec<TargetId>,
        notices: Vec<String>,
    }

    impl Clipboard for Recording {
        fn read(&mut self, _: RequestId, _: Selection, _: &[String], _: usize) {}

        fn write(&mut self, _: Selection, _: Vec<ClipboardData>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn cancel(&mut self, _: RequestId) {}
    }

    impl Frontend for Recording {
        type Target = ();
        type Permit = ();

        fn open_target(&mut self, view: ViewId, _: &str) -> Result<(TargetId, ()), String> {
            let target = TargetId::next();
            self.targets.insert(target, view);
            Ok((target, ()))
        }

        fn place(&mut self, _: &BTreeMap<ViewId, ViewEntry>, _: &mut Diagnostics) -> bool {
            false
        }

        fn forget_target(&mut self, target: TargetId) {
            self.targets.remove(&target);
            self.forgotten.push(target);
        }

        fn focus(&mut self, _: KeyboardFocus, _: &mut VecDeque<BrowserEvent>) {}
        fn chrome_input(&mut self, _: InputEvent, _: &mut VecDeque<BrowserEvent>) {}
        fn chrome_command(&mut self, _: String, _: &mut VecDeque<BrowserEvent>) {}
        fn page_cursor(&mut self, _: Option<TargetId>, _: Cursor) {}
        fn page_text_input(&mut self, _: Option<TargetId>, _: Option<TextInputState>) {}
        fn notice(&mut self, message: String) {
            self.notices.push(message);
        }
        fn redraw(&mut self) {}
        fn show(&mut self, _: ViewId, _: bool) {}
        fn frame_ready(&mut self, _: TargetId) {}
        fn before_forced_exit(&mut self) {}
    }

    #[test]
    fn a_refused_open_forgets_the_frontends_target() {
        let registry = Registry::new(vec![Engine::new(
            "demo",
            EngineCapabilities::default(),
            "cpu",
            &[],
            |_, _| Ok(Runtime::Demo(Box::new(Refusing))),
        )]);
        let env = registry.environment(Vec::new(), &[]);
        let text = "version = 1\n[engines]\ndefault = \"demo\"\nenabled = [\"demo\"]";
        let config = Config::parse(Some(text), &env).expect("config");
        let mut session = Session::<Recording>::new(
            BrowserState::new(config, env.clone()),
            env,
            registry,
            Services::new(|| {}),
            Paths {
                config: None,
                session: None,
            },
            None,
            Diagnostics::new(None),
            Arc::new(|| {}),
        );
        let mut frontend = Recording::default();
        let view = ViewId::next();

        let opened = session.open_view(&mut frontend, view, "demo", ViewOptions::default());

        assert!(
            matches!(opened, Err(EngineError::View { .. })),
            "{opened:?}"
        );
        assert_eq!(frontend.forgotten.len(), 1, "forget_target once");
        assert!(frontend.targets.is_empty(), "no target record is left");
        assert!(session.views.is_empty(), "no view entry is left");
        // A second forget finds nothing to forget.
        session.forget_view(&mut frontend, view);
        assert_eq!(frontend.forgotten.len(), 1);
    }

    /// Forgets and reloads recorded by the scripted engines, shared because a runtime is
    /// built by a function pointer.
    static SITE_CALLS: Mutex<Vec<(&'static str, SiteDataRequest)>> = Mutex::new(Vec::new());
    static RELOADS: Mutex<Vec<String>> = Mutex::new(Vec::new());

    struct Scripted {
        name: &'static str,
        caps: EngineCapabilities,
        views: BTreeMap<ViewId, String>,
        queue: Vec<EngineEvent>,
        /// `Some` answers on the next pump with these removed names. `None` refuses.
        reply: Option<Vec<String>>,
        pending: Option<RequestId>,
    }

    impl Scripted {
        fn commit(&mut self, view: ViewId, navigation: NavigationId, url: String) {
            self.queue.push(EngineEvent::NavigationCommitted {
                view,
                navigation,
                url,
                document: DocumentGeneration::next(),
                history: None,
            });
        }
    }

    impl EngineRuntime for Scripted {
        type Target = ();
        type FramePermit = ();

        fn capabilities(&self) -> EngineCapabilities {
            self.caps
        }

        fn open_view(&mut self, view: ViewId, _: ViewOptions, _: ()) -> Result<(), EngineError> {
            self.views.insert(view, String::new());
            self.queue.push(EngineEvent::ViewReady { view });
            Ok(())
        }

        fn command(&mut self, view: ViewId, command: EngineCommand) -> Result<(), EngineError> {
            match command {
                EngineCommand::Navigate { navigation, url } => {
                    self.views.insert(view, url.clone());
                    self.commit(view, navigation, url);
                }
                EngineCommand::Reload { navigation } => {
                    let url = self.views.get(&view).cloned().unwrap_or_default();
                    RELOADS.lock().unwrap().push(url.clone());
                    self.commit(view, navigation, url);
                }
                _ => {}
            }
            Ok(())
        }

        fn site_data(&mut self, request: SiteDataRequest) -> Result<(), EngineError> {
            SITE_CALLS
                .lock()
                .unwrap()
                .push((self.name, request.clone()));
            match self.reply.is_some() {
                true => {
                    self.pending = Some(request.request());
                    Ok(())
                }
                false => Err(EngineError::Unsupported("site data")),
            }
        }

        fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
            if let Some(request) = self.pending.take() {
                self.queue.push(EngineEvent::SiteDataDone {
                    request,
                    result: Ok(SiteDataOutcome {
                        removed: self.reply.clone().unwrap_or_default(),
                        kinds: self.caps.site_data,
                        durable: false,
                        profile: true,
                    }),
                });
            }
            events.append(&mut self.queue);
            PumpStatus::Idle
        }

        fn present(&mut self, view: ViewId, _: ()) -> Result<PresentOutcome, EngineError> {
            Err(EngineError::UnknownView(view))
        }

        fn close_view(&mut self, view: ViewId) {
            self.views.remove(&view);
        }

        fn shutdown(&mut self) {}
    }

    impl Running<(), ()> for Scripted {}

    fn site_caps(keeps: bool) -> EngineCapabilities {
        EngineCapabilities {
            history_traversal: true,
            site_data: SiteDataKinds {
                cookies: keeps,
                storage: keeps,
                caches: false,
                hsts: false,
            },
            ..EngineCapabilities::default()
        }
    }

    fn scripted(name: &'static str, keeps: bool, reply: Option<Vec<String>>) -> Scripted {
        Scripted {
            name,
            caps: site_caps(keeps),
            views: BTreeMap::new(),
            queue: Vec::new(),
            reply,
            pending: None,
        }
    }

    fn start_web(
        _: sighurt_api::engine::EngineWaker,
        _: crate::registry::Launch<'_>,
    ) -> Result<Runtime<(), ()>, String> {
        let mut long = "DROPME".to_owned();
        long.extend(std::iter::repeat_n('x', 248));
        Ok(Runtime::Demo(Box::new(scripted(
            "web",
            true,
            Some(vec![long, "example.com".into(), "Local files".into()]),
        ))))
    }

    fn start_idle(
        _: sighurt_api::engine::EngineWaker,
        _: crate::registry::Launch<'_>,
    ) -> Result<Runtime<(), ()>, String> {
        Ok(Runtime::Demo(Box::new(scripted("idle", true, None))))
    }

    fn start_plain(
        _: sighurt_api::engine::EngineWaker,
        _: crate::registry::Launch<'_>,
    ) -> Result<Runtime<(), ()>, String> {
        Ok(Runtime::Demo(Box::new(scripted("plain", false, None))))
    }

    #[test]
    fn site_forget_reaches_every_engine_that_keeps_any_and_a_refusal_answers() {
        SITE_CALLS.lock().unwrap().clear();
        RELOADS.lock().unwrap().clear();
        let registry = Registry::new(vec![
            Engine::new("web", site_caps(true), "cpu", &[], start_web),
            Engine::new("idle", site_caps(true), "cpu", &[], start_idle),
            Engine::new("plain", site_caps(false), "cpu", &[], start_plain),
        ]);
        let env = registry.environment(Vec::new(), &[]);
        let text =
            "version = 1\n[engines]\ndefault = \"web\"\nenabled = [\"web\", \"idle\", \"plain\"]";
        let config = Config::parse(Some(text), &env).expect("config");
        let mut session = Session::<Recording>::new(
            BrowserState::new(config, env.clone()),
            env,
            registry,
            Services::new(|| {}),
            Paths {
                config: None,
                session: None,
            },
            None,
            Diagnostics::new(None),
            Arc::new(|| {}),
        );
        let mut frontend = Recording::default();
        session.queue_mut().push_back(BrowserEvent::Start {
            session: None,
            open: [
                "https://example.com/",
                "https://www.example.com/a",
                "https://other.test/",
            ]
            .into_iter()
            .map(|input| OpenRequest {
                input: input.into(),
                engine: None,
            })
            .collect(),
        });
        session.run_core(&mut frontend);
        assert!(session.pump(&mut frontend), "the pages' engine was called");
        let urls: Vec<_> = session
            .core()
            .tabs()
            .map(|tab| tab.url.map(str::to_owned))
            .collect();
        assert_eq!(
            urls,
            vec![
                Some("https://example.com/".into()),
                Some("https://www.example.com/a".into()),
                Some("https://other.test/".into()),
            ]
        );
        assert!(matches!(
            session.registry().engine("idle").unwrap().state(),
            State::NotStarted
        ));

        session.queue_mut().push_back(BrowserEvent::Command(
            "site.forget".parse::<Command>().unwrap(),
        ));
        session.run_core(&mut frontend);
        assert!(matches!(
            session.registry().engine("idle").unwrap().state(),
            State::Running(_)
        ));
        assert!(
            matches!(
                session.registry().engine("plain").unwrap().state(),
                State::NotStarted
            ),
            "an engine that keeps nothing is not started"
        );
        let calls = std::mem::take(&mut *SITE_CALLS.lock().unwrap());
        assert_eq!(calls.len(), 2);
        assert_eq!((calls[0].0, calls[1].0), ("web", "idle"));
        assert_eq!(calls[0].1.request(), calls[1].1.request());
        assert!(matches!(
            &calls[0].1,
            SiteDataRequest::Forget { site: SiteKey::Host(host), .. } if host == "example.com"
        ));
        assert_eq!(
            frontend.notices,
            ["example.com: nothing on disk changed in idle (site data is not supported)"]
        );
        assert!(
            RELOADS.lock().unwrap().is_empty(),
            "an error does not reload"
        );

        assert!(session.pump(&mut frontend));
        assert_eq!(
            frontend.notices,
            [
                "example.com: nothing on disk changed in idle (site data is not supported)",
                "example.com forgotten in web: cookies, storage (removed: example.com, Local files)",
            ]
        );
        assert_eq!(
            std::mem::take(&mut *RELOADS.lock().unwrap()),
            ["https://example.com/", "https://www.example.com/a"]
        );

        // The request is finished. A late report for it changes nothing.
        let request = calls[0].1.request();
        session.queue_mut().push_back(BrowserEvent::Engine {
            engine: "web".into(),
            event: EngineEvent::SiteDataDone {
                request,
                result: Ok(SiteDataOutcome {
                    removed: vec!["example.com".into()],
                    kinds: site_caps(true).site_data,
                    durable: false,
                    profile: true,
                }),
            },
            at: std::time::Instant::now(),
        });
        session.run_core(&mut frontend);
        assert_eq!(frontend.notices.len(), 2);
        assert!(RELOADS.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod shutdown_bound {
    use std::collections::{BTreeMap, VecDeque};
    use std::process::{Command, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sighurt_api::events::Cursor;
    use sighurt_api::ids::{RequestId, TargetId, ViewId};
    use sighurt_api::input::{InputEvent, KeyboardFocus, TextInputState};
    use sighurt_api::services::{ClipboardData, Selection, ServiceError};
    use sighurt_core::config::Config;
    use sighurt_core::{BrowserEvent, BrowserState};

    use super::Session;
    use crate::diagnostics::Diagnostics;
    use crate::frontend::Frontend;
    use crate::persistence::Paths;
    use crate::registry::Registry;
    use crate::services::{Clipboard, Services};
    use crate::view::ViewEntry;

    /// No targets: the test never opens a view. Shutdown still runs the bound.
    #[derive(Default)]
    struct Quiet;

    impl Clipboard for Quiet {
        fn read(&mut self, _: RequestId, _: Selection, _: &[String], _: usize) {}

        fn write(&mut self, _: Selection, _: Vec<ClipboardData>) -> Result<(), ServiceError> {
            Ok(())
        }

        fn cancel(&mut self, _: RequestId) {}
    }

    impl Frontend for Quiet {
        type Target = ();
        type Permit = ();

        fn open_target(&mut self, _: ViewId, _: &str) -> Result<(TargetId, ()), String> {
            unreachable!("the bound tests open no view");
        }

        fn place(&mut self, _: &BTreeMap<ViewId, ViewEntry>, _: &mut Diagnostics) -> bool {
            false
        }

        fn forget_target(&mut self, _: TargetId) {}
        fn focus(&mut self, _: KeyboardFocus, _: &mut VecDeque<BrowserEvent>) {}
        fn chrome_input(&mut self, _: InputEvent, _: &mut VecDeque<BrowserEvent>) {}
        fn chrome_command(&mut self, _: String, _: &mut VecDeque<BrowserEvent>) {}
        fn page_cursor(&mut self, _: Option<TargetId>, _: Cursor) {}
        fn page_text_input(&mut self, _: Option<TargetId>, _: Option<TextInputState>) {}
        fn notice(&mut self, _: String) {}
        fn redraw(&mut self) {}
        fn show(&mut self, _: ViewId, _: bool) {}
        fn frame_ready(&mut self, _: TargetId) {}
        fn before_forced_exit(&mut self) {}
    }

    fn session() -> Session<Quiet> {
        let registry = Registry::new(vec![]);
        let env = registry.environment(Vec::new(), &[]);
        let config = Config::parse(Some("version = 1\n"), &env).expect("config");
        Session::new(
            BrowserState::new(config, env.clone()),
            env,
            registry,
            Services::new(|| {}),
            Paths {
                config: None,
                session: None,
            },
            None,
            Diagnostics::new(None),
            Arc::new(|| {}),
        )
    }

    #[test]
    fn a_finished_shutdown_disarms_the_bound() {
        let mut session = session();
        session.set_shutdown_bound(Duration::from_millis(150));
        let code = session.shutdown(&mut Quiet, |_, _| {});
        assert_eq!(code, std::process::ExitCode::SUCCESS);
        // Past the bound: the thread must have been disarmed, or this process would be gone.
        std::thread::sleep(Duration::from_millis(400));
    }

    /// Holds shutdown with `SIGHURT_TEST_SHUTDOWN_STALL_MS` and expects `_exit`.
    fn block_until_the_bound_ends_the_process() {
        let mut session = session();
        session.set_shutdown_bound(Duration::from_millis(200));
        let _ = session.shutdown(&mut Quiet, |_, _| {});
        panic!("the shutdown bound returned");
    }

    #[test]
    fn a_shutdown_that_does_not_return_ends_the_process_at_the_bound() {
        if std::env::var_os("SIGHURT_SHUTDOWN_BOUND_CHILD").is_some() {
            block_until_the_bound_ends_the_process();
        }
        let mut child = Command::new(std::env::current_exe().expect("test binary"))
            .arg("session::shutdown_bound::a_shutdown_that_does_not_return_ends_the_process_at_the_bound")
            .arg("--exact")
            .env("SIGHURT_SHUTDOWN_BOUND_CHILD", "1")
            .env("SIGHURT_TEST_SHUTDOWN_STALL_MS", "30000")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn");
        let started = Instant::now();
        // The stall is 30 s. Anything under that is the bound, not the stall, including
        // the time the test binary takes to start.
        let limit = Duration::from_secs(12);
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > limit => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("the bound did not end the process within {limit:?}");
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(error) => panic!("wait: {error}"),
            }
        };
        let mut err = String::new();
        let _ = std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err);
        assert!(
            status.success(),
            "bound exit {status:?} after {:?}\n{err}",
            started.elapsed()
        );
        assert!(
            err.contains("did not finish shutting down"),
            "the process ended without the bound's _exit:\n{err}"
        );
        assert!(
            started.elapsed() < limit,
            "exited only after {:?}\n{err}",
            started.elapsed()
        );
    }
}
