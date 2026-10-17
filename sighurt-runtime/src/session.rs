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
//! feeds the events back through `run_core`. Frame-ready does not go to the core; the frontend
//! is told, and it schedules presentation.
//!
//! Must not own: browser policy (the core), drawing, or windows. The frontend owns those, and
//! the loop that calls in.

use std::collections::{BTreeMap, VecDeque};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineError, EngineWaker};
use sighurt_api::events::EngineEvent;
use sighurt_api::ids::{RequestId, TargetId, ViewId};
use sighurt_api::services::{ClipboardData, ServiceError};
use sighurt_core::{BrowserEvent, BrowserState, Effect, Environment};

use crate::diagnostics::Diagnostics;
use crate::frontend::Frontend;
use crate::persistence::{Paths, SessionWriter};
use crate::registry::Registry;
use crate::services::{Completion, Services};
use crate::view::ViewEntry;

/// Most service completions handled in one pass.
const MAX_COMPLETIONS: usize = 64;
/// How long a changed session waits before it is written, so a burst of changes is one write.
const SESSION_DELAY: Duration = Duration::from_secs(1);

/// The frontend-neutral half of the owner loop.
pub struct Session<T: 'static, P: 'static> {
    /// Browser policy.
    pub core: BrowserState,
    /// Events for the core, in order.
    pub queue: VecDeque<BrowserEvent>,
    /// What the build has.
    pub registry: Registry<T, P>,
    /// Live engine views.
    pub views: BTreeMap<ViewId, ViewEntry>,
    /// Downloads and the page's clipboard bookkeeping.
    pub services: Services,
    /// Counters and the diagnostics file.
    pub diag: Diagnostics,
    /// Configuration and session paths.
    pub paths: Paths,
    /// Writes the session, when `[session] restore` is on and the file could be opened.
    pub session: Option<SessionWriter>,
    /// When the changed session should be written. `None` when nothing is waiting.
    pub session_due: Option<Instant>,
    /// What configurations are checked against. Kept for reloads.
    pub env: Environment,
    /// Set when the loop must end, with the exit code.
    pub quit: Option<ExitCode>,
    /// Engine wakers and service threads call this.
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl<T: 'static, P: 'static> Session<T, P> {
    /// A session that has not started. `wake` is how engines and downloads reach the loop.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        core: BrowserState,
        env: Environment,
        registry: Registry<T, P>,
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
            services,
            diag,
            paths,
            session,
            session_due: None,
            env,
            quit: None,
            wake,
        }
    }

    /// Hands queued events to the core and carries out its effects, until nothing is queued.
    /// Frontend work runs inside each effect, before the next one.
    pub fn run_core(&mut self, frontend: &mut impl Frontend<Target = T>) {
        while let Some(event) = self.queue.pop_front() {
            self.diag.counters.core_events += 1;
            let effects = sighurt_core::update(&mut self.core, event);
            self.diag.counters.effects += effects.len() as u64;
            for effect in effects {
                self.apply(frontend, effect);
            }
        }
    }

    /// Carries out one effect of the core.
    fn apply(&mut self, frontend: &mut impl Frontend<Target = T>, effect: Effect) {
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
                    self.services
                        .perform(frontend, request, service, downloads.as_deref());
                if let Some(result) = performed {
                    self.queue
                        .push_back(BrowserEvent::ServiceDone { request, result });
                }
            }
            Effect::CancelService(request) => self.services.cancel(frontend, request),
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
        frontend: &mut impl Frontend<Target = T>,
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
            // The engine dropped the target; the next placement leaves it out.
            self.views.remove(&view);
        }
        opened
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
    pub fn forget_view(&mut self, frontend: &mut impl Frontend<Target = T>, view: ViewId) {
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
        if let Some(result) = self.services.clipboard_read(request, result) {
            self.queue
                .push_back(BrowserEvent::ServiceDone { request, result });
        }
    }

    /// Takes what service threads reported. A status line becomes a notice. Returns whether
    /// there was anything.
    pub fn service_completions(&mut self, frontend: &mut impl Frontend<Target = T>) -> bool {
        let completions = self.services.completions(MAX_COMPLETIONS);
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
    pub fn pump(&mut self, frontend: &mut impl Frontend<Target = T>) -> bool {
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
        frontend: &mut impl Frontend<Target = T>,
        engine: &'static str,
        event: EngineEvent,
        at: Instant,
    ) {
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
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let status = if code == ExitCode::SUCCESS { 0 } else { 1 };
    // SAFETY: `_exit` ends the process at once. Nothing here relies on a destructor or exit
    // handler running: the session file and the diagnostics are written and closed, and the
    // downloads have ended or their files are deleted. The frontend has restored what it must.
    unsafe { libc::_exit(status) }
}
