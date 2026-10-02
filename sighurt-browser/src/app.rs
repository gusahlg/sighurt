//! The application owner loop, built on calloop, and the wiring of core, host, chrome and
//! engines.
//!
//! Its sources are the Wayland connection (calloop-wayland-source's prepare-read, read,
//! dispatch and flush cycle, inside the host), one ping that engine wakers and service threads
//! share, the signal pipe, the configuration watch, and the host's own timers (key repeat,
//! layout grace periods, clipboard transfers). Pump deadlines and the session's save delay are
//! the loop's wait timeout, not timers of their own.
//!
//! One iteration, after the loop wakes (plan §19):
//!
//! 1. Collect ready input and completions: at most [`MAX_HOST_EVENTS`] host events a pass, with
//!    consecutive pointer motion over the same target coalesced to the latest; key transitions
//!    and everything else keep their order.
//! 2. Hand them to the core (`sighurt_core::update`) and carry out its effects ([`effects`]).
//! 3. Deliver engine commands (as effects) and pump every runtime that was called, woke, or
//!    whose deadline passed.
//! 4. Hand their events to the core after the pumps return.
//! 5. Rebuild the chrome's snapshot only when the core said the chrome changed; invalidate only
//!    the chrome surfaces it asks to repaint; apply a placement only when it differs from the
//!    last (or answers a configure).
//! 6. Present whatever the host has permits for: the chrome paints its surfaces, engines present
//!    their views. Presenting is a call, so the runtimes are pumped again; steps 1–6 repeat until
//!    a pass does nothing, at most [`MAX_PASSES`] times.
//! 7. Flush, and sleep until an event or a real deadline. With work left over, the next
//!    iteration starts without sleeping.
//!
//! Nothing ticks at a fixed rate: an idle browser sleeps until the compositor, an engine, a
//! service or a signal wakes it. Downloads and session writes run on threads of their own.
//!
//! The chrome is the one `[ui] chrome` names when `sig` starts ([`registry::CHROMES`]), driven
//! through `sighurt_ui::extension::Chrome`: its surfaces are SHM targets by name
//! ([`ChromeTargets`]), created when it first lists them.
//!
//! Shutdown (plan §20): stop taking input, cancel downloads, write the session (at once, whole),
//! close every view and shut every runtime down (each releases its target leases), wait a moment
//! for the downloads (deleting the files of those still waiting on a server), delete what
//! engines kept only for this run (`storage = "ephemeral"`), retire the chrome's leases, close
//! the window, and only then drop the host. If a runtime could not shut down (Servo after a
//! crash, decision 54), the process ends with `_exit` after that, releasing nothing that runtime
//! may still use. With `[session] restore` off, the session is neither read nor written.
//!
//! Must not own: browser policy (the core) or drawing (the chrome and engines).
//!
//! [`effects`]: crate::effects
//! [`registry::CHROMES`]: crate::registry::CHROMES

use std::collections::{BTreeMap, VecDeque};
use std::os::fd::OwnedFd;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::ping::{make_ping, Ping};
use calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction};
use sighurt_api::engine::{EngineCommand, EngineError, EngineWaker};
use sighurt_api::events::{Cursor, EngineEvent};
use sighurt_api::geometry::LogicalSize;
use sighurt_api::ids::{RequestId, SeatId, TargetId, ViewId, WindowId};
use sighurt_api::input::{InputEvent, InputKind, KeyboardFocus, PointerButton, PointerEvent};
use sighurt_api::services::{ClipboardData, Selection};
use sighurt_core::tabs::{Content, TabSummary};
use sighurt_core::{BrowserEvent, BrowserState, Environment};
use sighurt_ui::extension::{
    Chrome, ChromeAction, ChromeEvent, SurfaceName, TextField, MAX_SURFACES,
};
use sighurt_ui::{EngineRow, HomeInfo, PixelBuffer, Snapshot};
use sighurt_wayland::{
    FramePermit, Host, HostEvent, InputRegion, Placement, PresentMode, TargetLease,
    TargetPlacement, WindowOptions,
};

use crate::chrome::{self, ChromeTargets};
use crate::diagnostics::{Diagnostics, EngineReport, Report, TabReport, TargetReport};
use crate::persistence::{self, ConfigWatch, Paths, SessionWriter};
use crate::registry::{self, Registry, State};
use crate::services::{Completion, Services};
use crate::Args;

/// Most host events handled in one pass.
pub const MAX_HOST_EVENTS: usize = 256;
/// Most passes of steps 1–6 in one iteration before the loop goes round without sleeping.
pub const MAX_PASSES: usize = 8;
/// Most service completions handled in one pass.
const MAX_COMPLETIONS: usize = 64;
/// How long a changed session waits before it is written, so a burst of changes is one write.
const SESSION_DELAY: Duration = Duration::from_secs(1);
/// How long a notice of `sig`'s own comes before the page's status text in the status strip.
const NOTICE_FIRST: Duration = Duration::from_secs(5);
/// The most clipboard text the address line takes.
const MAX_PASTE: usize = 1024 * 1024;
/// Clipboard types for text, most preferred first.
const TEXT_TYPES: [&str; 3] = ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING"];

/// A live engine view, as `sig` knows it.
pub(crate) struct ViewEntry {
    pub engine: String,
    pub target: TargetId,
    /// Whether the core shows it (`SetVisibility`).
    pub visible: bool,
    /// When it was last asked to navigate while its tab was the active one, until its next
    /// frame or until another tab becomes active.
    pub navigated: Option<Instant>,
}

/// Everything the owner loop owns.
pub struct App {
    pub(crate) host: Host,
    pub(crate) window: WindowId,
    pub(crate) core: BrowserState,
    /// Events for the core, in order.
    pub(crate) queue: VecDeque<BrowserEvent>,
    /// What the build has, which configurations are checked against.
    env: Environment,
    pub(crate) chrome: Box<dyn Chrome>,
    /// The chrome's name: the configuration's `[ui] chrome` when `sig` started.
    pub(crate) chrome_name: &'static str,
    /// The chrome's surfaces as targets, and their leases by name.
    chrome_targets: ChromeTargets,
    leases: BTreeMap<SurfaceName, TargetLease>,
    /// The chrome's latest text field for the input method, and the target it was set on.
    chrome_text: Option<TextField>,
    chrome_text_target: Option<TargetId>,
    /// The pointer's shape over the chrome, for targets made later.
    chrome_cursor: Cursor,
    pub(crate) registry: Registry,
    pub(crate) views: BTreeMap<ViewId, ViewEntry>,
    pub(crate) services: Services,
    paths: Paths,
    pub(crate) session: Option<SessionWriter>,
    pub(crate) session_due: Option<Instant>,
    pub(crate) diag: Diagnostics,
    /// Engine wakers and service threads ping this.
    pub(crate) wake: Ping,
    /// The window's size, from its latest configure.
    size: Option<LogicalSize>,
    /// The placement last applied, and whether the next must be applied even if equal.
    placed: Option<Placement>,
    place_now: bool,
    pub(crate) snapshot_dirty: bool,
    /// The latest notice or download status, for the status strip.
    pub(crate) notice: Option<String>,
    /// Until when that notice comes before the page's own status text.
    notice_first: Option<Instant>,
    /// The chrome's clipboard read in flight.
    paste: Option<RequestId>,
    keyboard_target: Option<TargetId>,
    /// The target whose text field the input method has.
    pub(crate) page_text_target: Option<TargetId>,
    config_changed: bool,
    signals: Vec<u8>,
    /// Set when the loop must end, with the exit code.
    pub(crate) quit: Option<ExitCode>,
}

/// Runs `sig` until it quits.
pub fn run(args: Args, signals: OwnedFd) -> Result<ExitCode, String> {
    let mut event_loop: EventLoop<'static, App> =
        EventLoop::try_new().map_err(|e| format!("cannot create the event loop: {e}"))?;
    let handle = event_loop.handle();
    let host = Host::new(&handle, |app: &mut App| &mut app.host).map_err(|e| e.to_string())?;
    let (wake, wake_source) = make_ping().map_err(|e| e.to_string())?;
    handle
        .insert_source(wake_source, |(), &mut (), _| {})
        .map_err(|e| e.error.to_string())?;
    let mut app = App::new(host, wake, args)?;
    // From here on every way out goes through `App::shutdown`, which releases in §20's order.
    match watch(&handle, signals, &app.paths) {
        Ok(()) => app.start(),
        Err(error) => app.fail(error),
    }
    let mut busy = true;
    while app.quit.is_none() {
        let timeout = if busy {
            Some(Duration::ZERO)
        } else {
            app.timeout()
        };
        if let Err(error) = event_loop.dispatch(timeout, &mut app) {
            app.fail(format!("the event loop failed: {error}"));
            break;
        }
        if timeout != Some(Duration::ZERO) {
            app.diag.counters.wakeups += 1;
        }
        busy = app.iterate();
    }
    let code = app.shutdown();
    drop(event_loop);
    Ok(code)
}

/// Adds the signal pipe and the configuration watch to the loop.
fn watch(handle: &LoopHandle<'static, App>, signals: OwnedFd, paths: &Paths) -> Result<(), String> {
    let source = Generic::new(signals, Interest::READ, Mode::Level);
    handle
        .insert_source(source, |_, fd, app| {
            let mut bytes = [0u8; 16];
            while let Ok(n) = rustix::io::read(&*fd, &mut bytes) {
                if n == 0 {
                    break;
                }
                app.signals.extend_from_slice(&bytes[..n]);
            }
            Ok(PostAction::Continue)
        })
        .map_err(|e| e.error.to_string())?;
    let Some(config) = &paths.config else {
        return Ok(());
    };
    match ConfigWatch::new(config) {
        Ok(watch) => {
            let source = Generic::new(watch, Interest::READ, Mode::Level);
            handle
                .insert_source(source, |_, watch, app| {
                    app.config_changed |= watch.changed();
                    Ok(PostAction::Continue)
                })
                .map_err(|e| e.error.to_string())?;
        }
        // No directory yet: the file is read at startup only.
        Err(error) => log::info!("not watching {}: {error}", config.display()),
    }
    Ok(())
}

impl App {
    fn new(mut host: Host, wake: Ping, args: Args) -> Result<Self, String> {
        let paths = Paths::from_env();
        let mut registry = Registry::compiled();
        let env = registry.environment();
        let mut notice = None;
        let config = match persistence::load_config(paths.config.as_deref(), &env) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("the configuration can't be used: {error}");
                notice = Some(format!(
                    "{error}\nsig runs with the built-in configuration until the file is fixed."
                ));
                persistence::load_config(None, &env).map_err(|e| e.to_string())?
            }
        };
        let servo_wake = wake.clone();
        registry.start_at_launch(&config, || EngineWaker::new(move || servo_wake.ping()));
        // Servo's logger, if it started; otherwise env_logger.
        crate::registry::install_logger();
        let kind = registry::chrome(config.chrome());
        let mut chrome = (kind.new)();
        let window = WindowId::next();
        let bg = chrome::theme(&config).colors.bg;
        host.create_window(
            window,
            WindowOptions {
                title: "Sighurt".into(),
                app_id: "sighurt".into(),
                background: [bg.r, bg.g, bg.b],
                ..WindowOptions::default()
            },
        );
        let configured = chrome.handle(chrome::configure(&config, kind.name));
        let session = match &paths.session {
            Some(path) => match SessionWriter::new(path.clone()) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    log::warn!("cannot save the session: {error}");
                    None
                }
            },
            None => None,
        };
        let services_wake = wake.clone();
        let mut app = Self {
            host,
            window,
            core: BrowserState::new(config, env.clone()),
            queue: VecDeque::new(),
            env,
            chrome,
            chrome_name: kind.name,
            chrome_targets: ChromeTargets::default(),
            leases: BTreeMap::new(),
            chrome_text: None,
            chrome_text_target: None,
            chrome_cursor: Cursor::Default,
            registry,
            views: BTreeMap::new(),
            services: Services::new(move || services_wake.ping()),
            paths,
            session,
            session_due: None,
            diag: Diagnostics::new(args.diagnostics),
            wake,
            size: None,
            placed: None,
            place_now: false,
            snapshot_dirty: true,
            notice: None,
            notice_first: None,
            paste: None,
            keyboard_target: None,
            page_text_target: None,
            config_changed: false,
            signals: Vec::new(),
            quit: None,
        };
        app.chrome_actions(configured);
        app.queue.push_back(BrowserEvent::Start {
            session: None,
            open: args.open,
        });
        if let Some(notice) = notice {
            app.show_notice(notice);
        }
        Ok(app)
    }

    /// Restores the session, unless `[session] restore` is off, and opens the command line's
    /// pages.
    fn start(&mut self) {
        let path = self.paths.session.as_deref();
        let path = path.filter(|_| self.core.config().restore_session());
        let session = match path.map(persistence::load_session) {
            Some(Ok(session)) => session,
            Some(Err(error)) => {
                self.show_notice(format!("The saved tabs can't be restored: {error}"));
                None
            }
            None => None,
        };
        if let Some(BrowserEvent::Start { session: slot, .. }) = self.queue.front_mut() {
            *slot = session;
        }
        self.run_core();
    }

    /// One iteration of the loop, after it woke. Returns whether work is left that shouldn't
    /// wait: the next iteration then starts at once.
    fn iterate(&mut self) -> bool {
        self.diag.counters.iterations += 1;
        self.handle_signals();
        if std::mem::take(&mut self.config_changed) {
            self.reload_config();
        }
        if self
            .notice_first
            .is_some_and(|until| until <= Instant::now())
        {
            self.notice_first = None;
            self.snapshot_dirty = true;
        }
        let mut busy = true;
        for _ in 0..MAX_PASSES {
            let mut worked = self.drain_host_events();
            worked |= self.service_completions();
            worked |= self.pump_engines();
            worked |= self.refresh_chrome();
            worked |= self.present();
            if !worked {
                busy = false;
                break;
            }
            if self.quit.is_some() {
                break;
            }
        }
        if self.session_due.is_some_and(|due| due <= Instant::now()) {
            self.save_session();
        }
        self.host.flush();
        busy && self.quit.is_none()
    }

    /// How long the loop may sleep: until the earliest pump deadline, session save or notice
    /// giving way to the page's status, or until something happens.
    fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let pump = match self.registry.next_pump() {
            Some(None) => return Some(Duration::ZERO),
            Some(Some(at)) => Some(at),
            None => None,
        };
        let next = [pump, self.session_due, self.notice_first]
            .into_iter()
            .flatten()
            .min();
        next.map(|at| at.saturating_duration_since(now))
    }

    /// `sig` can't go on: reports `error` and quits with a failure.
    fn fail(&mut self, error: String) {
        self.diag.error(error);
        self.quit = Some(ExitCode::FAILURE);
    }

    fn handle_signals(&mut self) {
        for signal in std::mem::take(&mut self.signals) {
            match i32::from(signal) {
                libc::SIGUSR1 => self.report(false),
                libc::SIGTERM | libc::SIGINT => {
                    self.queue.push_back(BrowserEvent::Command(
                        sighurt_api::commands::Command::BrowserQuit,
                    ));
                    self.run_core();
                }
                _ => {}
            }
        }
    }

    // ── Steps 1 and 2: input and completions ─────────────────────────────────────────────

    /// Handles the host's events, at most [`MAX_HOST_EVENTS`]. Returns whether there were any.
    fn drain_host_events(&mut self) -> bool {
        let mut batch: Vec<HostEvent> = Vec::new();
        while batch.len() < MAX_HOST_EVENTS {
            let Some(event) = self.host.next_event() else {
                break;
            };
            if push_coalesced(&mut batch, event) {
                self.diag.counters.motions_coalesced += 1;
            }
        }
        let peaks = &mut self.diag.peaks;
        peaks.max_host_queue = peaks.max_host_queue.max(batch.len() as u64);
        let worked = !batch.is_empty();
        for event in batch {
            self.host_event(event);
            self.run_core();
        }
        worked
    }

    fn host_event(&mut self, event: HostEvent) {
        let at = Instant::now();
        match event {
            HostEvent::WindowConfigured { size, .. } => {
                self.size = Some(size);
                let actions = self.chrome.handle(ChromeEvent::Resize(size));
                self.chrome_actions(actions);
                self.place_now = true;
            }
            HostEvent::CloseRequested { .. } => self.queue.push_back(BrowserEvent::Command(
                sighurt_api::commands::Command::BrowserQuit,
            )),
            HostEvent::MetricsChanged { target, metrics } => {
                if let Some(view) = self.view_of(target) {
                    self.engine_command(view, EngineCommand::SetMetrics(metrics), false);
                }
            }
            HostEvent::Input { target, event, .. } => {
                self.diag.counters.input_events += 1;
                self.input(target, event, at);
            }
            HostEvent::ClipboardRead { request, result } => {
                if self.paste == Some(request) {
                    self.paste = None;
                    if let Ok(data) = result {
                        let text = String::from_utf8_lossy(&data.data).into_owned();
                        let actions = self.chrome.handle(ChromeEvent::Pasted { request, text });
                        self.chrome_actions(actions);
                    }
                } else if let Some(result) = self.services.clipboard_read(request, result) {
                    self.queue
                        .push_back(BrowserEvent::ServiceDone { request, result });
                }
            }
            HostEvent::ConnectionLost { error } => {
                self.fail(format!("the Wayland connection is gone: {error}"))
            }
        }
    }

    /// Routes input: pointer input to the chrome surface or page it is over, except the mouse's
    /// side buttons, which go to the core wherever the pointer is (`[input.buttons]`); keys,
    /// input-method text and window focus to the core, which binds keys and hands the rest to
    /// the page or the chrome.
    fn input(&mut self, target: Option<TargetId>, event: InputEvent, at: Instant) {
        match &event.kind {
            InputKind::Pointer(pointer) => {
                let side = matches!(
                    pointer,
                    PointerEvent::Button {
                        button: PointerButton::Back | PointerButton::Forward,
                        ..
                    }
                );
                if side {
                    let view = target.and_then(|t| self.view_of(t));
                    self.queue
                        .push_back(BrowserEvent::Button { view, event, at });
                } else if let Some(surface) = target.and_then(|t| self.surface_of(t)) {
                    let event = pointer.clone();
                    let actions = self.chrome.handle(ChromeEvent::Pointer { surface, event });
                    self.chrome_actions(actions);
                } else if let Some(view) = target.and_then(|t| self.view_of(t)) {
                    self.queue
                        .push_back(BrowserEvent::Pointer { view, event, at });
                }
            }
            InputKind::Focus(focus) => {
                let actions = self.chrome.handle(ChromeEvent::WindowFocus(*focus));
                self.chrome_actions(actions);
                self.queue.push_back(BrowserEvent::Keyboard { event, at });
            }
            InputKind::Key(_) | InputKind::TextInput(_) => {
                self.queue.push_back(BrowserEvent::Keyboard { event, at })
            }
        }
    }

    /// Carries out what the chrome asks for.
    pub(crate) fn chrome_actions(&mut self, actions: Vec<ChromeAction>) {
        for action in actions {
            match action {
                ChromeAction::Command(command) => {
                    self.queue.push_back(BrowserEvent::Command(command))
                }
                ChromeAction::SelectTab(tab) => self.queue.push_back(BrowserEvent::SelectTab(tab)),
                ChromeAction::CloseTab(tab) => self.queue.push_back(BrowserEvent::CloseTab(tab)),
                ChromeAction::Focus(focus) => self.queue.push_back(BrowserEvent::Focus(focus)),
                ChromeAction::Copy(text) => {
                    let item = ClipboardData {
                        mime_type: TEXT_TYPES[0].into(),
                        data: text.into_bytes(),
                    };
                    if let Err(error) = self.host.clipboard_write(Selection::Clipboard, vec![item])
                    {
                        self.show_notice(format!("Copying failed: {error}"));
                    }
                }
                ChromeAction::Paste(request) => {
                    if let Some(old) = self.paste.replace(request) {
                        self.host.clipboard_cancel(old);
                    }
                    let types = TEXT_TYPES.map(String::from);
                    self.host
                        .clipboard_read(request, Selection::Clipboard, &types, MAX_PASTE);
                }
                ChromeAction::TextInput(field) => {
                    self.chrome_text = field;
                    self.sync_chrome_text();
                }
                ChromeAction::Cursor(cursor) => {
                    self.chrome_cursor = cursor;
                    for lease in self.leases.values() {
                        self.host.set_cursor(lease.id(), cursor);
                    }
                }
                ChromeAction::Repaint(surface) => {
                    if let Some(target) = self.target_of(surface) {
                        self.host.invalidate(target);
                    }
                }
            }
        }
    }

    /// Gives the host the chrome's text field, on its surface's target once that has one, and
    /// takes it from the target it was on before.
    fn sync_chrome_text(&mut self) {
        let wanted = self
            .chrome_text
            .as_ref()
            .and_then(|field| self.target_of(field.surface));
        if let Some(old) = self.chrome_text_target.filter(|&old| Some(old) != wanted) {
            self.host.set_text_input(self.window, old, None);
        }
        if let Some(target) = wanted {
            let state = self.chrome_text.as_ref().map(|field| field.state.clone());
            self.host.set_text_input(self.window, target, state);
        }
        self.chrome_text_target = wanted;
    }

    /// Hands queued events to the core and carries out its effects, until nothing is queued.
    pub(crate) fn run_core(&mut self) {
        while let Some(event) = self.queue.pop_front() {
            self.diag.counters.core_events += 1;
            let effects = sighurt_core::update(&mut self.core, event);
            self.diag.counters.effects += effects.len() as u64;
            for effect in effects {
                self.apply(effect);
            }
        }
    }

    /// Takes what service threads reported. Returns whether there was anything.
    fn service_completions(&mut self) -> bool {
        let completions = self.services.completions(MAX_COMPLETIONS);
        let worked = !completions.is_empty();
        for completion in completions {
            match completion {
                Completion::Status(line) => self.show_notice(line),
                Completion::Finished(_) => {}
            }
        }
        worked
    }

    // ── Steps 3 and 4: engines ────────────────────────────────────────────────────────────

    /// Pumps the runtimes that are due and hands their events on. Returns whether any was.
    fn pump_engines(&mut self) -> bool {
        let now = Instant::now();
        let due = self.registry.due(now);
        for &engine in &due {
            let mut events = Vec::new();
            let started = Instant::now();
            self.registry.pump(engine, &mut events);
            self.diag.pump(events.len(), started.elapsed());
            for event in events {
                self.engine_event(engine, event, now);
            }
            self.run_core();
        }
        !due.is_empty()
    }

    fn engine_event(&mut self, engine: &'static str, event: EngineEvent, at: Instant) {
        match &event {
            // The core ignores it: presentation is scheduled here.
            EngineEvent::FrameReady { view } => {
                self.diag.counters.frame_ready += 1;
                if let Some(entry) = self.views.get(view).filter(|e| e.engine == engine) {
                    self.host.invalidate(entry.target);
                }
                return;
            }
            EngineEvent::ViewClosed { view } => {
                if self.views.get(view).is_some_and(|e| e.engine == engine) {
                    self.forget_view(*view);
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
                    self.forget_view(view);
                }
                self.snapshot_dirty = true;
            }
            _ => {}
        }
        self.queue.push_back(BrowserEvent::Engine {
            engine: engine.to_owned(),
            event,
            at,
        });
    }

    /// Hands `command` to `view`'s engine. A refusal goes to the core if `report`.
    pub(crate) fn engine_command(&mut self, view: ViewId, command: EngineCommand, report: bool) {
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

    /// `view` is gone from its engine: forget it and its target.
    pub(crate) fn forget_view(&mut self, view: ViewId) {
        let Some(entry) = self.views.remove(&view) else {
            return;
        };
        if self.page_text_target == Some(entry.target) {
            self.host.set_text_input(self.window, entry.target, None);
            self.page_text_target = None;
        }
        if self.keyboard_target == Some(entry.target) {
            self.host.set_keyboard_focus(self.window, None);
            self.keyboard_target = None;
        }
    }

    // ── Step 5: the chrome and the layout ─────────────────────────────────────────────────

    /// Brings the chrome, the placement and the keyboard focus up to date. Returns whether
    /// anything changed.
    fn refresh_chrome(&mut self) -> bool {
        let mut worked = false;
        // The chrome's actions can change the core, which can change the snapshot again.
        for _ in 0..4 {
            if !std::mem::take(&mut self.snapshot_dirty) {
                break;
            }
            let snapshot = self.snapshot();
            let actions = self.chrome.handle(ChromeEvent::Snapshot(snapshot));
            self.chrome_actions(actions);
            self.run_core();
            worked = true;
            // A navigation whose tab the user left is no longer waited for.
            let active = self.active_tab().map(|tab| tab.id);
            for (view, entry) in &mut self.views {
                if self.core.view(*view).map(|v| v.tab()) != active {
                    entry.navigated = None;
                }
            }
        }
        worked |= self.place();
        self.sync_keyboard_focus();
        worked
    }

    /// What the chrome shows: the core's tabs, the status line and the home page, contained.
    fn snapshot(&self) -> Snapshot {
        let status = |tab: &TabSummary<'_>| self.status(tab).map(|(_, text)| text.to_owned());
        chrome::snapshot(&self.core, self.home_info(), status)
    }

    /// What `sig://home` says: the configuration file and the engines of this build.
    fn home_info(&self) -> HomeInfo {
        let config = self.core.config();
        let engines = self
            .registry
            .engines()
            .map(|engine| {
                let mut detail = vec![format!("compiled in, {}", engine.renderer)];
                if engine.name == config.default_engine() {
                    detail.push("default".into());
                }
                let enabled = config.is_enabled(engine.name);
                if !enabled {
                    detail.push("disabled in the configuration".into());
                }
                let failed = match engine.state() {
                    State::Failed(why) => Some(why),
                    _ => None,
                };
                if let Some(why) = failed {
                    detail.push(format!("failed: {why}"));
                }
                EngineRow {
                    name: engine.name.to_owned(),
                    detail: detail.join(" · "),
                    available: enabled && failed.is_none(),
                }
            })
            .collect();
        HomeInfo {
            config_path: self
                .paths
                .config
                .as_ref()
                .map_or_else(String::new, |p| p.display().to_string()),
            engines,
        }
    }

    /// What the status strip shows for `tab`, and where it comes from. A tab's error is shown
    /// there unless the page area shows it (a tab with nothing else to show).
    fn status<'a>(&'a self, tab: &TabSummary<'a>) -> Option<(&'static str, &'a str)> {
        let error = tab
            .error
            .filter(|_| !matches!(tab.content, Content::Blank))
            .map(|e| e.message.as_str());
        status_line(
            self.notice.as_deref(),
            self.notice_first.is_some(),
            tab.status,
            error,
        )
    }

    /// The active tab.
    fn active_tab(&self) -> Option<TabSummary<'_>> {
        self.core.tabs().nth(self.core.active())
    }

    /// Whether `view` belongs to the active tab.
    pub(crate) fn in_active_tab(&self, view: ViewId) -> bool {
        let tab = self.core.view(view).map(|v| v.tab());
        tab.is_some() && tab == self.active_tab().map(|t| t.id)
    }

    /// Shows `message` in the status strip until another replaces it, for [`NOTICE_FIRST`]
    /// before anything the page says there. Not logged: notices can hold page data (an origin,
    /// a URL, a downloaded file's name).
    pub(crate) fn show_notice(&mut self, message: String) {
        self.notice = Some(message);
        self.notice_first = Some(Instant::now() + NOTICE_FIRST);
        self.snapshot_dirty = true;
    }

    /// The window's layout for the host: every view (only the shown one visible) at the page
    /// rectangle, the chrome's surfaces above them in its order. A surface name the chrome
    /// hasn't used before gets a target here.
    fn placement(&mut self) -> Option<Placement> {
        let size = self.size?;
        let placing = self.chrome_targets.place(self.chrome.layout(), size);
        for name in placing.refused {
            self.diag.error(format!(
                "the chrome uses more than {MAX_SURFACES} surfaces: {name} is not shown"
            ));
        }
        let mut targets: Vec<TargetPlacement> = self
            .views
            .values()
            .map(|entry| TargetPlacement {
                target: entry.target,
                rect: placing.page,
                visible: entry.visible,
                input: InputRegion::Whole,
            })
            .collect();
        let mut created = false;
        for placed in placing.surfaces {
            let target = match self.leases.get(placed.name) {
                Some(lease) => lease.id(),
                None => match self.host.create_target(self.window, PresentMode::Shm, None) {
                    Ok(lease) => {
                        let target = lease.id();
                        self.host.set_cursor(target, self.chrome_cursor);
                        // Its first frame, whatever the chrome asked to repaint before.
                        self.host.invalidate(target);
                        self.leases.insert(placed.name, lease);
                        created = true;
                        target
                    }
                    Err(error) => {
                        let name = placed.name;
                        self.diag
                            .error(format!("no target for the chrome's {name}: {error}"));
                        continue;
                    }
                },
            };
            targets.push(TargetPlacement {
                target,
                rect: placed.rect,
                visible: placed.visible,
                input: placed.input,
            });
        }
        if created {
            self.sync_chrome_text();
        }
        Some(Placement {
            window: self.window,
            size,
            targets,
        })
    }

    /// Applies the placement if it changed, or must answer a configure. Returns whether it did.
    pub(crate) fn place(&mut self) -> bool {
        let Some(placement) = self.placement() else {
            return false;
        };
        if !self.place_now && self.placed.as_ref() == Some(&placement) {
            return false;
        }
        match self.host.apply_placement(placement.clone()) {
            Ok(()) => {
                self.diag.counters.placements += 1;
                self.placed = Some(placement);
                self.place_now = false;
                true
            }
            Err(error) => {
                // A stale size means a newer configure is queued; it brings its own placement.
                self.diag.counters.placement_errors += 1;
                self.diag.error(format!("placement refused: {error}"));
                false
            }
        }
    }

    /// Gives the host's logical keyboard focus to whatever the core says has the keyboard: the
    /// chrome's surface with its text field (or its first surface), the shown view, or the
    /// chrome's surface at the page's rectangle while it draws the page.
    fn sync_keyboard_focus(&mut self) {
        let layout = self.chrome.layout();
        let listed = |rect: Option<_>| {
            let surfaces = layout.surfaces.iter();
            let mut surfaces = surfaces.filter(|s| rect.is_none_or(|rect| s.rect == rect));
            surfaces.find_map(|surface| self.target_of(surface.name))
        };
        let wanted = match self.core.focus() {
            KeyboardFocus::Chrome => self.chrome_text_target.or_else(|| listed(None)),
            KeyboardFocus::Page => self
                .shown_view()
                .map(|(_, entry)| entry.target)
                .or_else(|| listed(Some(layout.page).filter(|page| !page.size().is_empty()))),
        };
        if wanted != self.keyboard_target {
            self.keyboard_target = wanted;
            self.host.set_keyboard_focus(self.window, wanted);
        }
    }

    /// The view the page area shows, if an engine's.
    pub(crate) fn shown_view(&self) -> Option<(ViewId, &ViewEntry)> {
        self.views
            .iter()
            .find(|(_, entry)| entry.visible)
            .map(|(&view, entry)| (view, entry))
    }

    fn view_of(&self, target: TargetId) -> Option<ViewId> {
        self.views
            .iter()
            .find(|(_, entry)| entry.target == target)
            .map(|(&view, _)| view)
    }

    /// The chrome's surface that `target` is.
    fn surface_of(&self, target: TargetId) -> Option<SurfaceName> {
        let mut leases = self.leases.iter();
        leases
            .find(|(_, lease)| lease.id() == target)
            .map(|(&name, _)| name)
    }

    /// The target of the chrome's surface `surface`, once it has one.
    fn target_of(&self, surface: SurfaceName) -> Option<TargetId> {
        self.leases.get(surface).map(TargetLease::id)
    }

    // ── Step 6: presenting ────────────────────────────────────────────────────────────────

    /// Hands each permit the host issues to its presenter. Returns whether there were any.
    fn present(&mut self) -> bool {
        let permits = self.host.permits();
        let worked = !permits.is_empty();
        for permit in permits {
            let target = permit.target();
            if let Some(surface) = self.surface_of(target) {
                self.paint_chrome(surface, permit);
            } else if let Some(view) = self.view_of(target) {
                self.present_view(view, permit);
            }
            // Otherwise the target is being retired: dropping the permit presents nothing.
        }
        if worked {
            // Buffers are allocated while presenting, so this sees every peak.
            let host = self.host.diagnostics();
            let per_target: Vec<_> = host
                .targets
                .iter()
                .map(|t| {
                    let chrome = self.surface_of(t.target).is_some();
                    (t.buffers.buffers, t.buffers.retiring_generations, chrome)
                })
                .collect();
            let bytes = host.targets.iter().map(|t| t.buffers.bytes).sum();
            self.diag.buffers(per_target.into_iter(), bytes);
        }
        worked
    }

    fn paint_chrome(&mut self, surface: SurfaceName, permit: FramePermit) {
        let metrics = permit.metrics();
        let mut frame = match permit.shm_frame() {
            Ok(frame) => frame,
            Err(error) => {
                self.diag
                    .error(format!("no buffer for the chrome's {surface}: {error}"));
                return;
            }
        };
        let (size, stride) = (frame.size(), frame.stride());
        let started = Instant::now();
        let buffer = PixelBuffer {
            data: frame.pixels(),
            width: size.width,
            height: size.height,
            stride,
        };
        match self.chrome.paint(surface, &metrics, buffer) {
            Ok(()) => {
                frame.submit();
                self.diag.chrome(surface, started.elapsed());
            }
            // The buffer goes back unsubmitted.
            Err(error) => self
                .diag
                .error(format!("painting the chrome's {surface} failed: {error}")),
        }
    }

    fn present_view(&mut self, view: ViewId, permit: FramePermit) {
        let Some(entry) = self.views.get_mut(&view) else {
            return;
        };
        let size = permit.metrics().buffer();
        let Some(runtime) = self.registry.running(&entry.engine) else {
            return;
        };
        let started = Instant::now();
        let outcome = runtime.present(view, permit);
        let took = started.elapsed();
        match outcome {
            Ok(sighurt_api::engine::PresentOutcome::Submitted) => {
                if !self.diag.knows_renderer(&entry.engine) {
                    if let Some((mode, gl)) = runtime.renderer() {
                        self.diag.renderer(&entry.engine, mode, gl.as_deref());
                    }
                }
                let buffer = u64::from(size.width) * u64::from(size.height) * 4;
                let readback_bytes =
                    self.diag
                        .readbacks(&entry.engine, runtime.readbacks(), buffer);
                let size = (size.width, size.height);
                let paint = runtime.paint_time().unwrap_or(took);
                self.diag.frame(
                    &entry.engine,
                    view.serial(),
                    size,
                    (paint, took),
                    readback_bytes,
                );
                if let Some(asked) = entry.navigated.take() {
                    self.diag.first_frame(&entry.engine, asked.elapsed());
                }
            }
            Ok(sighurt_api::engine::PresentOutcome::NotReady) => {
                self.diag.counters.page_not_ready += 1
            }
            Ok(sighurt_api::engine::PresentOutcome::Unchanged) => {
                self.diag.counters.page_unchanged += 1
            }
            Err(error) => {
                self.diag.counters.present_errors += 1;
                let message = format!("{} could not present {view:?}: {error}", entry.engine);
                self.diag.error(message);
            }
        }
    }

    // ── Files ─────────────────────────────────────────────────────────────────────────────

    fn reload_config(&mut self) {
        self.diag.counters.config_reloads += 1;
        match persistence::load_config(self.paths.config.as_deref(), &self.env) {
            Ok(config) => {
                // Saving the session again: the tabs as they are now go to the file soon.
                if config.restore_session() && !self.core.config().restore_session() {
                    self.session_changed();
                }
                let actions = self
                    .chrome
                    .handle(chrome::configure(&config, self.chrome_name));
                self.chrome_actions(actions);
                self.queue
                    .push_back(BrowserEvent::ConfigReloaded(Ok(Box::new(config))));
                self.notice_first = None;
                if self.notice.take().is_some() {
                    self.snapshot_dirty = true;
                }
            }
            Err(persistence::LoadError::Config(error)) => {
                log::warn!("the configuration in use stays: {error}");
                self.queue
                    .push_back(BrowserEvent::ConfigReloaded(Err(error)));
            }
            Err(persistence::LoadError::Other(message)) => {
                log::warn!("the configuration in use stays: {message}");
                self.show_notice(format!("{message}\nThe configuration in use stays."))
            }
        }
        self.snapshot_dirty = true;
        self.run_core();
    }

    /// Hands the session to the writer now, unless `[session] restore` is off: then the file
    /// stays as it is.
    pub(crate) fn save_session(&mut self) {
        self.session_due = None;
        let writer = self.session.as_ref();
        if let Some(writer) = writer.filter(|_| self.core.config().restore_session()) {
            writer.save(self.core.session().to_text());
            self.diag.counters.session_saves += 1;
        }
    }

    /// The core changed what the session holds: save it soon.
    pub(crate) fn session_changed(&mut self) {
        self.session_due
            .get_or_insert_with(|| Instant::now() + SESSION_DELAY);
    }

    // ── Diagnostics and shutdown ──────────────────────────────────────────────────────────

    fn report(&mut self, at_exit: bool) {
        // Readbacks made since the engines' last frames, each as big as the page area.
        let scale = self
            .leases
            .values()
            .next()
            .map(|lease| lease.metrics().scale());
        let page = scale.map(|scale| scale.buffer_size(self.chrome.layout().page.size()));
        let page_bytes = page.map_or(0, |b| u64::from(b.width) * u64::from(b.height) * 4);
        for engine in self.registry.engines() {
            if let State::Running(runtime) = engine.state() {
                self.diag
                    .readbacks(engine.name, runtime.readbacks(), page_bytes);
            }
        }
        let host = self.host.diagnostics();
        let targets = host
            .targets
            .iter()
            .map(|t| {
                let (role, engine, tab) = match (self.surface_of(t.target), self.view_of(t.target))
                {
                    (Some(surface), _) => (surface, None, None),
                    (None, Some(view)) => (
                        "view",
                        Some(self.views[&view].engine.clone()),
                        self.core
                            .view(view)
                            .and_then(|v| self.core.tabs().position(|tab| tab.id == v.tab())),
                    ),
                    (None, None) => ("retiring", None, None),
                };
                let (rect, buffer) = (t.metrics.rect(), t.metrics.buffer());
                TargetReport {
                    role,
                    engine,
                    tab: tab.map(|i| i + 1),
                    width: rect.width,
                    height: rect.height,
                    buffer_width: buffer.width,
                    buffer_height: buffer.height,
                    scale: t.metrics.scale().numerator(),
                    mapped: t.mapped,
                    shown: t.shown,
                    awaiting_callback: t.awaiting_callback,
                    permits: t.stats.permits,
                    submissions: t.stats.submissions,
                    frame_callbacks: t.stats.frame_callbacks,
                    releases: t.stats.releases,
                    presented: t.stats.presented,
                    discarded: t.stats.discarded,
                    late: t.stats.late,
                    buffers: t.buffers.buffers,
                    buffer_bytes: t.buffers.bytes,
                    retiring_generations: t.buffers.retiring_generations,
                    retiring_bytes: t.buffers.retiring_bytes,
                }
            })
            .collect();
        let engines = self
            .registry
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
                        .and_then(|r| r.renderer())
                        .and_then(|(mode, gl)| gl.map(|gl| format!("{mode}: {gl}"))),
                    readbacks: running.map_or(0, |r| r.readbacks()),
                    gl_reads: running.and_then(|r| r.gl_reads()),
                    state: match engine.state() {
                        State::NotStarted => "not started".into(),
                        State::Running(_) => "running".into(),
                        State::ShutDown => "shut down".into(),
                        State::Failed(why) => format!("failed: {why}"),
                    },
                }
            })
            .collect();
        let tabs = self
            .core
            .tabs()
            .map(|tab| TabReport {
                loading: tab.loading,
                can_go_back: tab.can_go_back,
                can_go_forward: tab.can_go_forward,
                reloads: tab.reloads,
                error: tab.error.is_some(),
            })
            .collect();
        let report = Report {
            counters: self.diag.counters.clone(),
            peaks: self.diag.peaks.clone(),
            targets,
            engines,
            tabs,
            active_tab: self.core.active() + 1,
            status: self
                .active_tab()
                .and_then(|tab| self.status(&tab).map(|(from, _)| from)),
            capabilities: format!("{:?}", host.capabilities),
            pending_host_events: host.pending_events,
            last_error: self.diag.last_error(),
        };
        self.diag.report(&report, at_exit);
    }

    /// Shuts down in the order of plan §20 and returns the exit code. If an engine could not
    /// shut down (decision 54), the process ends here with `_exit` once the session is written
    /// and the downloads have ended or been deleted.
    fn shutdown(mut self) -> ExitCode {
        let code = self.quit.take().unwrap_or(ExitCode::SUCCESS);
        self.services.cancel_downloads();
        if let Some(writer) = self.session.take() {
            if self.core.config().restore_session() {
                writer.save(self.core.session().to_text());
            }
            writer.finish();
        }
        let views: Vec<(ViewId, String)> = self
            .views
            .iter()
            .map(|(&view, entry)| (view, entry.engine.clone()))
            .collect();
        for (view, engine) in views {
            self.forget_view(view);
            if let Some(runtime) = self.registry.running(&engine) {
                runtime.close_view(view);
            }
        }
        // Before the runtimes go: they know their renderer and readbacks.
        self.report(true);
        self.registry.shutdown();
        let Self {
            mut host,
            window,
            leases,
            services,
            mut registry,
            ..
        } = self;
        services.shutdown();
        // Late, so that no guest still finishing its last call writes there again.
        registry.remove_scratch();
        if registry.leaked() {
            exit_now(code);
        }
        drop(leases);
        host.close_window(window);
        host.flush();
        drop(host);
        code
    }
}

/// Ends the process now, with `code`, without exit handlers or destructors: what decision 54
/// prescribes after Servo was left running. Its threads still use GL contexts, the EGL display
/// and the Wayland connection, which exit handlers (the driver's) and the host's destructors
/// would tear down under them; the compositor cleans up after the closed connection.
fn exit_now(code: ExitCode) -> ! {
    log::error!("an engine could not shut down; sig ends without running exit handlers");
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let status = if code == ExitCode::SUCCESS { 0 } else { 1 };
    // SAFETY: `_exit` ends the process at once. Nothing here relies on a destructor or exit
    // handler running: the session file and the diagnostics are written and closed, and the
    // downloads have ended or their files are deleted.
    unsafe { libc::_exit(status) }
}

/// What the status strip shows, and where it comes from: a notice of `sig`'s own while it is
/// `fresh`, so no page can hide it; then the page's status text (its hovered link), then why a
/// navigation failed while the page stayed, then an older notice.
fn status_line<'a>(
    notice: Option<&'a str>,
    fresh: bool,
    page: Option<&'a str>,
    error: Option<&'a str>,
) -> Option<(&'static str, &'a str)> {
    let notice = notice.map(|text| ("notice", text));
    let rest = page
        .map(|text| ("page", text))
        .or(error.map(|text| ("error", text)));
    if fresh {
        notice.or(rest)
    } else {
        rest.or(notice)
    }
}

/// Adds `event` to `batch`, or replaces the batch's last event with it when both are pointer
/// motion over the same target from the same seat (step 1 of plan §19): only the latest
/// position matters. Anything else, a button or key in between included, keeps its place and
/// order. Returns whether it replaced an event.
fn push_coalesced(batch: &mut Vec<HostEvent>, event: HostEvent) -> bool {
    if let (Some(last), Some(motion)) = (batch.last_mut(), motion_of(&event)) {
        if motion_of(last) == Some(motion) {
            *last = event;
            return true;
        }
    }
    batch.push(event);
    false
}

/// The target and seat of a pointer motion event, for coalescing.
fn motion_of(event: &HostEvent) -> Option<(Option<TargetId>, SeatId)> {
    match event {
        HostEvent::Input {
            target,
            event:
                InputEvent {
                    seat,
                    kind: InputKind::Pointer(PointerEvent::Motion { .. }),
                },
            ..
        } => Some((*target, *seat)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::SurfacePoint;
    use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, Modifiers, PointerButton};

    use super::*;

    fn input(target: TargetId, seat: SeatId, pointer: PointerEvent) -> HostEvent {
        HostEvent::Input {
            window: WindowId::next(),
            target: Some(target),
            event: InputEvent {
                seat,
                kind: InputKind::Pointer(pointer),
            },
        }
    }

    fn motion(target: TargetId, seat: SeatId, x: f64) -> HostEvent {
        let position = SurfacePoint { x, y: 0.0 };
        let modifiers = Modifiers::default();
        input(
            target,
            seat,
            PointerEvent::Motion {
                position,
                modifiers,
            },
        )
    }

    fn press(target: TargetId, seat: SeatId) -> HostEvent {
        input(
            target,
            seat,
            PointerEvent::Button {
                button: PointerButton::Left,
                state: ButtonState::Pressed,
                position: SurfacePoint::default(),
                modifiers: Modifiers::default(),
            },
        )
    }

    /// The x of each motion in `batch`, and `None` for anything else.
    fn xs(batch: &[HostEvent]) -> Vec<Option<f64>> {
        batch
            .iter()
            .map(|event| match event {
                HostEvent::Input {
                    event:
                        InputEvent {
                            kind: InputKind::Pointer(PointerEvent::Motion { position, .. }),
                            ..
                        },
                    ..
                } => Some(position.x),
                _ => None,
            })
            .collect()
    }

    fn coalesce(events: Vec<HostEvent>) -> (Vec<HostEvent>, usize) {
        let mut batch = Vec::new();
        let dropped = events
            .into_iter()
            .map(|event| push_coalesced(&mut batch, event))
            .filter(|&replaced| replaced)
            .count();
        (batch, dropped)
    }

    #[test]
    fn motion_over_one_target_collapses_to_the_latest() {
        let (a, seat) = (TargetId::next(), SeatId::next());
        let (batch, dropped) = coalesce((1..=5).map(|x| motion(a, seat, x.into())).collect());
        assert_eq!((xs(&batch), dropped), (vec![Some(5.0)], 4));
    }

    #[test]
    fn a_button_between_motions_keeps_its_place() {
        let (a, seat) = (TargetId::next(), SeatId::next());
        let events = vec![
            motion(a, seat, 1.0),
            motion(a, seat, 2.0),
            press(a, seat),
            motion(a, seat, 3.0),
            motion(a, seat, 4.0),
        ];
        let (batch, dropped) = coalesce(events);
        assert_eq!(xs(&batch), [Some(2.0), None, Some(4.0)]);
        assert_eq!(dropped, 2);
    }

    #[test]
    fn a_key_between_motions_keeps_its_place() {
        let (a, seat) = (TargetId::next(), SeatId::next());
        let key = HostEvent::Input {
            window: WindowId::next(),
            target: None,
            event: InputEvent {
                seat,
                kind: InputKind::Key(KeyEvent {
                    code: KeyCode(30),
                    key: Key::Character("a".into()),
                    state: ButtonState::Pressed,
                    repeat: false,
                    modifiers: Modifiers::default(),
                    consumed: Modifiers::default(),
                    text: Some("a".into()),
                }),
            },
        };
        let (batch, _) = coalesce(vec![motion(a, seat, 1.0), key, motion(a, seat, 2.0)]);
        assert_eq!(xs(&batch), [Some(1.0), None, Some(2.0)]);
    }

    #[test]
    fn other_targets_and_seats_are_not_merged() {
        let (a, b, seat, other) = (
            TargetId::next(),
            TargetId::next(),
            SeatId::next(),
            SeatId::next(),
        );
        let events = vec![
            motion(a, seat, 1.0),
            motion(b, seat, 2.0),
            motion(b, other, 3.0),
            motion(a, seat, 4.0),
        ];
        let (batch, dropped) = coalesce(events);
        assert_eq!(xs(&batch), [Some(1.0), Some(2.0), Some(3.0), Some(4.0)]);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn a_fresh_notice_comes_before_the_page() {
        let (notice, page, error) = (Some("bad config"), Some("typed x"), Some("failed"));
        assert_eq!(
            status_line(notice, true, page, error),
            Some(("notice", "bad config"))
        );
        assert_eq!(
            status_line(notice, false, page, error),
            Some(("page", "typed x"))
        );
        assert_eq!(
            status_line(notice, false, None, error),
            Some(("error", "failed"))
        );
        assert_eq!(
            status_line(notice, false, None, None),
            Some(("notice", "bad config"))
        );
        assert_eq!(
            status_line(None, true, page, error),
            Some(("page", "typed x"))
        );
        assert_eq!(status_line(None, false, None, None), None);
    }
}
