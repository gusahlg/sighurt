//! The terminal owner loop, built on calloop, and the [`Frontend`] the session drives.
//!
//! Its sources are the terminal (readable for input; writable only while bytes wait in the
//! bounded output), the `SIGWINCH` pipe (resize, and the wake after `SIGCONT`), one ping that
//! engine wakers and service threads share, the signal pipe from `sig` (SIGUSR1 diagnostics,
//! SIGTERM and SIGINT quit) and the configuration watch.
//!
//! One iteration, after the loop wakes, as in the Wayland loop: terminal input (at most
//! [`MAX_EVENTS`] events a pass, each handed to the core), service completions, the layout's
//! metrics to the engines, engine pumps, then one write to the terminal: the chrome if the core
//! said it changed (or the terminal was resized or resumed), and at most one page frame. Passes
//! repeat until one does nothing, at most [`MAX_PASSES`] times; then the loop sleeps until an
//! event or a real deadline: an engine pump, the session save, a notice giving way, a partial
//! escape sequence, or the next frame's time while a frame is waiting. Nothing ticks.
//!
//! Page frames (TUI.md §2 and §8.6): a permit only for the shown view, only when its engine has
//! a new frame, at most `[tui] fps` a second, and only once every byte written before has left
//! the output buffer: nothing is written while any is waiting, so at most one encoded frame is
//! ever pending and no write is cut short. The engine paints into its view's [`TermTarget`] and
//! the frame is encoded as a kitty image over the page rows. A resize is a new layout epoch: a
//! frame for the old one is never sent, though bytes already on their way finish. A hidden or
//! closed view's image is deleted.
//!
//! Logs go to `{state_dir}/sighurt/tui.log` before any engine starts. The terminal is restored
//! on every way out: quit, error, panic (the terminal's hook), SIGHUP (its handler) and
//! [`Frontend::before_forced_exit`]; SIGTSTP suspends and SIGCONT redraws everything.
//!
//! Must not own: browser policy (the core) or which effect runs (the session).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{IsTerminal, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::ping::make_ping;
use calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, RegistrationToken};
use sighurt_api::commands::Command;
use sighurt_api::engine::{EngineCommand, EngineWaker, PresentOutcome};
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{LayoutEpoch, RequestId, SeatId, TargetId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyEvent, KeyboardFocus, PointerButton,
    PointerEvent, ScrollDelta, TextInputEvent, TextInputState,
};
use sighurt_api::services::{ClipboardData, Selection, ServiceError};
use sighurt_api::settings::Table;
use sighurt_core::config::{GraphicsProtocol, TuiConfig, TuiMode, TERMINAL_CHROME_COMMANDS};
use sighurt_core::tabs::{Content, TabSummary};
use sighurt_core::{
    BrowserEvent, BrowserState, ChromeChecks, Frontend as FrontendKind, OpenRequest,
};
use sighurt_runtime::diagnostics::{Diagnostics, Report, TabReport, TargetReport};
use sighurt_runtime::frontend::Frontend;
use sighurt_runtime::persistence::{self, ConfigWatch, Paths, SessionWriter};
use sighurt_runtime::services::{Clipboard, Services};
use sighurt_runtime::session::Session;
use sighurt_runtime::view::ViewEntry;

use crate::chrome::{ChromeState, LineEditor, PageArea, TabLine, MODE_GRAPHICS, MODE_TEXT};
use crate::grid::{Attrs, Color, Grid};
use crate::input::{cell_to_page_pixel, CellPixels, PageCells};
use crate::kitty::{self, CellRect};
use crate::target::{TargetHandle, TermPermit, TermTarget};
use crate::term::{MouseEvent, TermEvent, TermSize, Terminal, Wheel, PROBE_DEADLINE};

/// Most terminal events handled in one pass; the rest wait for the next.
pub const MAX_EVENTS: usize = 256;
/// Most passes in one iteration before the loop goes round without sleeping.
pub const MAX_PASSES: usize = 8;
/// How long a notice of `sig`'s own comes before the page's status text.
const NOTICE_FIRST: Duration = Duration::from_secs(5);
/// The most bytes one write may leave waiting for the terminal: the bound on an encoded frame
/// (8 Mpx of RGBA, compressed, in base64, fits unless the page is noise).
const OUTPUT_CAP: usize = 16 * 1024 * 1024;
/// The most pasted text the address line or a page's field takes.
const MAX_PASTE: usize = 1024 * 1024;
/// The page rows' message when the terminal shows no images.
const NO_GRAPHICS: &str = "This terminal can't show graphics; text mode is coming";
/// The page rows' message in `[tui] mode = "text"`.
const NO_TEXT: &str = "Text mode isn't built yet";
/// The page rows' message when the page area has more pixels than an image may.
const TOO_BIG: &str = "The page area is too big for an image";
/// The page rows' message for a `sig:` page.
const INTERNAL: &str = "sig: pages aren't shown in the terminal yet";
/// The chromes the window has, by name. The terminal ignores `[ui] chrome` and `[chrome.*]`,
/// but one configuration file serves both frontends, so it accepts what the window would.
/// The window checks those tables; here they are taken as they are.
const WINDOW_CHROMES: [&str; 2] = ["standard", "line"];
/// The color names `[ui.colors]` takes: `sighurt_ui::Colors::NAMES`, which this crate can't
/// name (it never depends on sighurt-ui). The terminal chrome doesn't use them yet.
const COLOR_NAMES: &[&str] = &[
    "bg",
    "surface",
    "hover",
    "tab",
    "border",
    "focus",
    "text",
    "text_muted",
    "text_dim",
    "selection",
    "error",
];

/// What the command line asks the terminal to do.
pub struct Options {
    /// URLs and paths to open, as the core takes them.
    pub open: Vec<OpenRequest>,
    /// Where to write the diagnostics as JSON.
    pub diagnostics: Option<PathBuf>,
}

/// `{state_dir}/sighurt/tui.log` (or the data directory's, as for the session file), where the
/// terminal frontend sends stderr.
fn log_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("sighurt")
        .join("tui.log")
}

/// One view's target as the loop sees it.
struct Pixels {
    /// The kitty image id, from 1, never reused.
    image: u32,
    handle: TargetHandle,
    /// The engine has a new frame for the current layout.
    ready: bool,
    /// The terminal shows this view's frame for the current layout.
    on_screen: bool,
    permits: u64,
    presented: u64,
    dropped: u64,
}

/// The terminal, the page targets and the chrome's state: the [`Frontend`].
struct Tui {
    terminal: Terminal,
    grid: Grid,
    seat: SeatId,
    settings: TuiConfig,
    /// The page rows show kitty images.
    graphics: bool,
    /// The page rows.
    page: PageArea,
    /// What every view is given: the page rows' pixels, or nothing.
    metrics: Metrics,
    /// `metrics` changed and the views have not been told.
    metrics_due: bool,
    targets: HashMap<TargetId, Pixels>,
    /// Each view's target, for the core's show and hide.
    views: HashMap<ViewId, TargetId>,
    next_image: u32,
    /// The address line, while `location.focus` has it.
    editing: Option<LineEditor>,
    /// The active tab's address when the core last ran, for the address line to start from.
    active_url: String,
    notice: Option<String>,
    notice_first: Option<Instant>,
    /// The shown page has a text field: a paste goes there.
    page_text: bool,
    /// The earliest time the next page frame may be presented.
    next_frame: Option<Instant>,
    /// Page clipboard reads, refused on the next pass: the terminal has no clipboard yet.
    refused_reads: Vec<RequestId>,
    /// Bytes for the next write, before the chrome: mode changes and image deletions.
    control: Vec<u8>,
    chrome_dirty: bool,
    /// The next write clears the screen and its images and draws everything again.
    clear: bool,
    cursor_shown: bool,
    mouse_on: bool,
}

/// The owner loop's state.
struct App {
    tui: Tui,
    session: Session<Tui>,
    /// Terminal events read and not yet handled.
    input: VecDeque<TermEvent>,
    readable: bool,
    config_changed: bool,
    signals: Vec<u8>,
    /// The terminal's write source, enabled only while bytes are waiting.
    writable: Option<RegistrationToken>,
}

/// Runs the terminal frontend until it quits.
///
/// Stdout must be a terminal. Stderr goes to [`log_path`] before anything else happens, and
/// comes back if this fails, so the caller can say why.
pub fn run(options: Options, signals: OwnedFd) -> Result<ExitCode, String> {
    if !std::io::stdout().is_terminal() {
        return Err("the terminal frontend needs a terminal on stdout".into());
    }
    let saved = redirect_stderr(&log_path())?;
    let result = run_redirected(options, signals);
    if result.is_err() {
        let _ = rustix::stdio::dup2_stderr(&saved);
    }
    result
}

fn run_redirected(options: Options, signals: OwnedFd) -> Result<ExitCode, String> {
    let mut event_loop: EventLoop<'static, App> =
        EventLoop::try_new().map_err(|e| format!("cannot create the event loop: {e}"))?;
    let handle = event_loop.handle();
    let (ping, wake_source) = make_ping().map_err(|e| e.to_string())?;
    handle
        .insert_source(wake_source, |(), &mut (), _| {})
        .map_err(|e| e.error.to_string())?;
    // From here on the terminal is raw; dropping `app` restores it.
    let mut app = App::new(ping, options)?;
    if let Err(error) = watch(&handle, signals, &mut app) {
        app.fail(error);
    } else {
        app.start();
    }
    let mut busy = true;
    while !app.session.has_quit() {
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
            app.session.diagnostics_mut().counters.wakeups += 1;
        }
        busy = app.iterate();
        app.watch_writes(&handle);
    }
    let code = app.shutdown();
    drop(event_loop);
    Ok(code)
}

/// Points fd 2 at `path`, creating its directory. Returns the previous stderr.
fn redirect_stderr(path: &Path) -> Result<OwnedFd, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let saved = std::io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|e| format!("cannot keep stderr: {e}"))?;
    rustix::stdio::dup2_stderr(&file).map_err(|e| format!("cannot redirect stderr: {e}"))?;
    let _ = writeln!(
        std::io::stderr(),
        "---- sig --tui, pid {} ----",
        std::process::id()
    );
    Ok(saved)
}

/// Adds the signal pipe, the terminal, its resize pipe, its write source (disabled) and the
/// configuration watch to the loop.
fn watch(handle: &LoopHandle<'static, App>, signals: OwnedFd, app: &mut App) -> Result<(), String> {
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
    let terminal = &app.tui.terminal;
    let fds = [
        terminal.try_clone_fd(),
        terminal.winch_fd().try_clone_to_owned(),
    ];
    for fd in fds {
        let fd = fd.map_err(|e| format!("cannot watch the terminal: {e}"))?;
        handle
            .insert_source(
                Generic::new(fd, Interest::READ, Mode::Level),
                |_, _, app| {
                    app.readable = true;
                    Ok(PostAction::Continue)
                },
            )
            .map_err(|e| e.error.to_string())?;
    }
    let fd = terminal
        .try_clone_fd()
        .map_err(|e| format!("cannot watch the terminal: {e}"))?;
    // Draining happens in `iterate`; waking is all this does.
    let token = handle
        .insert_source(Generic::new(fd, Interest::WRITE, Mode::Level), |_, _, _| {
            Ok(PostAction::Continue)
        })
        .map_err(|e| e.error.to_string())?;
    handle.disable(&token).map_err(|e| e.to_string())?;
    app.writable = Some(token);
    let Some(config) = &app.session.paths().config else {
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

/// The window's chromes, accepted with whatever tables they have (see [`WINDOW_CHROMES`]).
fn chrome_checks() -> Vec<ChromeChecks> {
    fn accept(_: &Table) -> Vec<String> {
        Vec::new()
    }
    WINDOW_CHROMES
        .into_iter()
        .map(|name| ChromeChecks {
            name,
            check: accept,
            // Not used: in the terminal, `chrome.<action>` is checked against the terminal's.
            commands: &[],
        })
        .collect()
}

impl App {
    fn new(ping: calloop::ping::Ping, options: Options) -> Result<Self, String> {
        let fd = std::io::stdout()
            .as_fd()
            .try_clone_to_owned()
            .map_err(|e| format!("cannot use stdout: {e}"))?;
        let mut terminal = Terminal::enter_with(fd, OUTPUT_CAP)
            .map_err(|e| format!("cannot take the terminal: {e}"))?;
        terminal
            .probe(PROBE_DEADLINE)
            .map_err(|e| format!("cannot ask the terminal what it supports: {e}"))?;
        terminal
            .install_handlers()
            .map_err(|e| format!("cannot handle the terminal's signals: {e}"))?;

        let paths = Paths::from_env();
        let mut registry = crate::engines::build();
        let mut env = registry.environment(chrome_checks(), COLOR_NAMES);
        env.frontend = FrontendKind::Terminal;
        let mut notice = None;
        let config = match persistence::load_config(paths.config.as_deref(), &env) {
            Ok(config) => config,
            Err(error) => {
                log::warn!("the configuration can't be used: {error}");
                notice = Some(format!(
                    "{error} sig runs with the built-in configuration until the file is fixed."
                ));
                persistence::load_config(None, &env).map_err(|e| e.to_string())?
            }
        };
        let servo_wake = ping.clone();
        registry.start_at_launch(&config, || EngineWaker::new(move || servo_wake.ping()));
        // Servo's logger, if it started; otherwise env_logger. Either writes to the log file.
        sighurt_runtime::registry::install_logger();
        let session_writer = match &paths.session {
            Some(path) => match SessionWriter::new(path.clone()) {
                Ok(writer) => Some(writer),
                Err(error) => {
                    log::warn!("cannot save the session: {error}");
                    None
                }
            },
            None => None,
        };
        let services_wake = ping.clone();
        let services = Services::new(move || services_wake.ping());
        let wake = std::sync::Arc::new(move || ping.ping());
        let settings = *config.tui();
        let mut session = Session::new(
            BrowserState::new(config, env.clone()),
            env,
            registry,
            services,
            paths,
            session_writer,
            Diagnostics::new(options.diagnostics),
            wake,
        );
        session.queue_mut().push_back(BrowserEvent::Start {
            session: None,
            open: options.open,
        });
        let size = terminal.size();
        let mut tui = Tui {
            grid: Grid::new(size.cols, size.rows),
            terminal,
            seat: SeatId::next(),
            settings,
            graphics: false,
            page: page_area(size),
            metrics: empty_metrics(),
            metrics_due: false,
            targets: HashMap::new(),
            views: HashMap::new(),
            next_image: 1,
            editing: None,
            active_url: String::new(),
            notice: None,
            notice_first: None,
            page_text: false,
            next_frame: None,
            refused_reads: Vec::new(),
            control: Vec::new(),
            chrome_dirty: true,
            clear: false,
            cursor_shown: false,
            // The terminal's enter sequence turns the mouse on.
            mouse_on: true,
        };
        tui.adopt(settings);
        if let Some(notice) = notice {
            tui.show_notice(notice);
        }
        Ok(Self {
            tui,
            session,
            input: VecDeque::new(),
            readable: false,
            config_changed: false,
            signals: Vec::new(),
            writable: None,
        })
    }

    /// Restores the session, unless `[session] restore` is off, and opens the command line's
    /// pages.
    fn start(&mut self) {
        let restore = self.session.core().config().restore_session();
        let path = self.session.paths().session.as_deref().filter(|_| restore);
        let restored = match path.map(persistence::load_session) {
            Some(Ok(session)) => session,
            Some(Err(error)) => {
                self.tui
                    .show_notice(format!("The saved tabs can't be restored: {error}"));
                None
            }
            None => None,
        };
        if let Some(BrowserEvent::Start { session: slot, .. }) =
            self.session.queue_mut().front_mut()
        {
            *slot = restored;
        }
        self.run_core();
    }

    /// Hands the queued events to the core. The address line starts from the active tab's
    /// address as it was before them.
    fn run_core(&mut self) {
        self.tui.active_url = active_url(self.session.core());
        self.session.run_core(&mut self.tui);
    }

    /// One iteration of the loop, after it woke. Returns whether work is left that shouldn't
    /// wait.
    fn iterate(&mut self) -> bool {
        self.session.diagnostics_mut().counters.iterations += 1;
        if self.tui.terminal.take_resumed() {
            self.resumed();
        }
        self.handle_signals();
        if std::mem::take(&mut self.config_changed) {
            self.reload_config();
        }
        if self
            .tui
            .notice_first
            .is_some_and(|until| until <= Instant::now())
        {
            self.tui.notice_first = None;
            self.tui.chrome_dirty = true;
        }
        let mut busy = true;
        for _ in 0..MAX_PASSES {
            let mut worked = self.drain_input();
            worked |= self.refuse_clipboard_reads();
            worked |= self.session.service_completions(&mut self.tui);
            worked |= self.send_metrics();
            self.tui.active_url = active_url(self.session.core());
            worked |= self.session.pump(&mut self.tui);
            worked |= self.write();
            if !worked {
                busy = false;
                break;
            }
            if self.session.has_quit() {
                break;
            }
        }
        if self
            .session
            .session_due()
            .is_some_and(|due| due <= Instant::now())
        {
            self.session.save_session();
        }
        if let Err(error) = self.tui.terminal.write_frame(&[]) {
            self.fail(format!("writing to the terminal failed: {error}"));
        }
        busy && !self.session.has_quit()
    }

    /// Enables the terminal's write source while bytes are waiting, and disables it otherwise.
    fn watch_writes(&self, handle: &LoopHandle<'static, App>) {
        if let Some(token) = &self.writable {
            let _ = match self.tui.terminal.pending_len() {
                0 => handle.disable(token),
                _ => handle.enable(token),
            };
        }
    }

    /// How long the loop may sleep.
    fn timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let pump = match self.session.registry().next_pump() {
            Some(None) => return Some(Duration::ZERO),
            Some(Some(at)) => Some(at),
            None => None,
        };
        // A waiting frame needs its time and an empty output buffer, which the write source
        // wakes the loop for.
        let frame = self
            .tui
            .next_frame
            .filter(|_| self.frame_waiting() && self.tui.terminal.pending_len() == 0);
        let escape = self.tui.terminal.poll_timeout().map(|wait| now + wait);
        [
            pump,
            self.session.session_due(),
            self.tui.notice_first,
            frame,
            escape,
        ]
        .into_iter()
        .flatten()
        .min()
        .map(|at| at.saturating_duration_since(now))
    }

    /// The shown view has a frame the terminal could show.
    fn frame_waiting(&self) -> bool {
        self.tui.graphics
            && self
                .shown()
                .and_then(|(_, target)| self.tui.targets.get(&target))
                .is_some_and(|pixels| pixels.ready)
    }

    /// `sig` can't go on: reports `error` and quits with a failure.
    fn fail(&mut self, error: String) {
        self.session.diagnostics_mut().error(error);
        self.session.set_quit(ExitCode::FAILURE);
    }

    fn handle_signals(&mut self) {
        for signal in std::mem::take(&mut self.signals) {
            match i32::from(signal) {
                libc::SIGUSR1 => report(&self.tui, &mut self.session, false),
                libc::SIGTERM | libc::SIGINT => {
                    self.session
                        .queue_mut()
                        .push_back(BrowserEvent::Command(Command::BrowserQuit));
                    self.run_core();
                }
                _ => {}
            }
        }
    }

    /// SIGCONT entered the terminal again: what was waiting is dropped and everything is drawn
    /// again, mouse reporting as configured.
    fn resumed(&mut self) {
        self.tui.terminal.discard_pending();
        self.tui.mouse_on = true;
        let settings = self.tui.settings;
        self.tui.adopt(settings);
        self.tui.redraw_all();
    }

    /// Reads the terminal if it woke the loop, and hands at most [`MAX_EVENTS`] events to the
    /// core, one at a time. Returns whether there were any.
    fn drain_input(&mut self) -> bool {
        if std::mem::take(&mut self.readable) {
            match self.tui.terminal.read_events() {
                Ok(events) => self.input.extend(events),
                Err(error) => {
                    self.fail(format!("reading the terminal failed: {error}"));
                    return false;
                }
            }
        }
        self.input.extend(self.tui.terminal.expire_if_due());
        let peaks = &mut self.session.diagnostics_mut().peaks;
        peaks.max_host_queue = peaks.max_host_queue.max(self.input.len() as u64);
        let count = self.input.len().min(MAX_EVENTS);
        for event in self.input.drain(..count).collect::<Vec<_>>() {
            self.event(event);
            self.run_core();
        }
        // Read again next pass, in case the terminal had more than one read took.
        self.readable |= count > 0;
        count > 0
    }

    fn event(&mut self, event: TermEvent) {
        let at = Instant::now();
        let seat = self.tui.seat;
        let keyboard = |kind| BrowserEvent::Keyboard {
            event: InputEvent { seat, kind },
            at,
        };
        let event = match event {
            TermEvent::Resize(_) => {
                self.tui.relayout();
                return self.tui.redraw_all();
            }
            TermEvent::Key(key) => keyboard(InputKind::Key(key)),
            TermEvent::Focus(inside) => keyboard(InputKind::Focus(match inside {
                true => FocusEvent::Activated,
                false => FocusEvent::Deactivated,
            })),
            TermEvent::Paste(text) => match self.paste(text) {
                Some(event) => keyboard(event),
                None => return,
            },
            TermEvent::Mouse(mouse) => match self.mouse(mouse, at) {
                Some(event) => event,
                None => return,
            },
        };
        self.session.diagnostics_mut().counters.input_events += 1;
        self.session.queue_mut().push_back(event);
    }

    /// A pasted text: the address line's while it is open, the page's if it has a text field,
    /// and otherwise nobody's.
    fn paste(&mut self, mut text: String) -> Option<InputKind> {
        if text.len() > MAX_PASTE {
            let mut end = MAX_PASTE;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
        }
        if let Some(editor) = &mut self.tui.editing {
            editor.paste(&text);
            self.tui.chrome_dirty = true;
            return None;
        }
        self.tui.page_text.then(|| {
            InputKind::TextInput(TextInputEvent {
                commit: Some(text),
                ..TextInputEvent::default()
            })
        })
    }

    /// Mouse input, when `[tui] mouse` is on: the side buttons go to the core wherever they are
    /// (`[input.buttons]`), and the rest to the shown page while over its rows, as positions on
    /// it. The chrome's rows take no mouse input: they show no tab list to click.
    fn mouse(&mut self, mouse: MouseEvent, at: Instant) -> Option<BrowserEvent> {
        if !self.tui.settings.mouse {
            return None;
        }
        let point = self.page_point(&mouse);
        let view = point.and(self.shown()).map(|(view, _)| view);
        let modifiers = mouse.modifiers;
        let pointer = |kind| InputEvent {
            seat: self.tui.seat,
            kind: InputKind::Pointer(kind),
        };
        if let (Some(button @ (PointerButton::Back | PointerButton::Forward)), None, false) =
            (mouse.button, mouse.wheel, mouse.motion)
        {
            let event = pointer(PointerEvent::Button {
                button,
                state: mouse.state,
                position: point.unwrap_or_default(),
                modifiers,
            });
            return Some(BrowserEvent::Button { view, event, at });
        }
        let (view, position) = (view?, point?);
        let kind = if let Some(wheel) = mouse.wheel {
            let (x, y) = match wheel {
                Wheel::Up => (0, -120),
                Wheel::Down => (0, 120),
                Wheel::Left => (-120, 0),
                Wheel::Right => (120, 0),
            };
            PointerEvent::Scroll {
                delta: ScrollDelta::Wheel { x, y },
                position,
                modifiers,
            }
        } else if mouse.motion {
            PointerEvent::Motion {
                position,
                modifiers,
            }
        } else {
            PointerEvent::Button {
                button: mouse.button?,
                state: mouse.state,
                position,
                modifiers,
            }
        };
        let event = pointer(kind);
        Some(BrowserEvent::Pointer { view, event, at })
    }

    /// Where on the page the mouse's cell is, in the page's pixels.
    fn page_point(&self, mouse: &MouseEvent) -> Option<SurfacePoint> {
        let size = self.tui.terminal.size();
        let page = PageCells {
            column: 0,
            row: self.tui.page.row,
            columns: self.tui.page.cols,
            rows: self.tui.page.rows,
        };
        let cell = CellPixels {
            width: size.cell_width,
            height: size.cell_height,
        };
        let (x, y) = cell_to_page_pixel(mouse.column, mouse.row, page, cell)?;
        Some(SurfacePoint {
            x: f64::from(x),
            y: f64::from(y),
        })
    }

    /// The shown view and its target.
    fn shown(&self) -> Option<(ViewId, TargetId)> {
        self.session
            .shown_view()
            .map(|(view, entry)| (view, entry.target))
    }

    /// Refuses the page clipboard reads asked for since the last pass.
    fn refuse_clipboard_reads(&mut self) -> bool {
        let reads = std::mem::take(&mut self.tui.refused_reads);
        let worked = !reads.is_empty();
        for request in reads {
            self.session
                .clipboard_done(request, Err(ServiceError::Unsupported));
        }
        worked
    }

    /// Gives every view the page area's metrics, if they changed.
    fn send_metrics(&mut self) -> bool {
        if !std::mem::take(&mut self.tui.metrics_due) {
            return false;
        }
        let metrics = self.tui.metrics;
        for pixels in self.tui.targets.values_mut() {
            pixels.handle.set_metrics(metrics);
            pixels.ready = false;
            pixels.on_screen = false;
        }
        let views: Vec<ViewId> = self.session.views().keys().copied().collect();
        for &view in &views {
            self.session
                .engine_command(view, EngineCommand::SetMetrics(metrics), false);
        }
        !views.is_empty()
    }

    /// Writes what changed: control bytes, the chrome, and at most one page frame, as one
    /// write, and only once everything written before has left. Returns whether it wrote.
    fn write(&mut self) -> bool {
        // What the terminal takes now of what was waiting.
        if let Err(error) = self.tui.terminal.write_frame(&[]) {
            self.fail(format!("writing to the terminal failed: {error}"));
        }
        if self.tui.terminal.pending_len() > 0 {
            return false;
        }
        let mut out = std::mem::take(&mut self.tui.control);
        if std::mem::take(&mut self.tui.clear) {
            out.extend_from_slice(b"\x1b[H\x1b[2J");
            // Also when the images were just turned off.
            if self.tui.terminal.capabilities().kitty_graphics_ok == Some(true) {
                out.extend_from_slice(kitty::DELETE_ALL);
            }
            let size = self.tui.terminal.size();
            self.tui.grid = Grid::new(size.cols, size.rows);
            self.tui.cursor_shown = false;
            self.tui.chrome_dirty = true;
            for pixels in self.tui.targets.values_mut() {
                pixels.on_screen = false;
            }
        }
        if std::mem::take(&mut self.tui.chrome_dirty) {
            self.draw_chrome(&mut out);
        }
        self.frame(&mut out);
        if out.is_empty() {
            return false;
        }
        if let Err(error) = self.tui.terminal.write_frame(&out) {
            self.fail(format!("writing to the terminal failed: {error}"));
        }
        true
    }

    /// Draws the chrome and the page rows' message into the grid, and the difference into
    /// `out`, with the cursor at the address line's while it is open.
    fn draw_chrome(&mut self, out: &mut Vec<u8>) {
        self.session.diagnostics_mut().counters.chrome_paints += 1;
        let mut state = self.chrome_state();
        state.contain();
        let area = state.draw(&mut self.tui.grid);
        self.tui.page = area;
        let message = self.page_message();
        for row in area.row..area.row + area.rows {
            self.tui.grid.clear_row(row);
        }
        if let Some(message) = message {
            let message = crate::sanitize::sanitize(&message);
            let width = unicode_width::UnicodeWidthStr::width(message.as_str());
            let column = usize::from(area.cols).saturating_sub(width) / 2;
            let row = area.row + area.rows / 2;
            if area.rows > 0 {
                let (fg, bg) = (Color::Default, Color::Default);
                let column = u16::try_from(column).unwrap_or(0);
                self.tui
                    .grid
                    .put_text(column, row, &message, fg, bg, Attrs::default());
            }
        }
        out.extend(self.tui.grid.diff());
        match area.cursor {
            Some(cursor) => {
                let (row, column) = (cursor.row + 1, cursor.column + 1);
                out.extend(format!("\x1b[{row};{column}H\x1b[?25h").into_bytes());
                self.tui.grid.forget_cursor();
                self.tui.cursor_shown = true;
            }
            None if self.tui.cursor_shown => {
                out.extend_from_slice(b"\x1b[?25l");
                self.tui.cursor_shown = false;
            }
            None => {}
        }
    }

    /// What the page rows say instead of showing an image, if anything.
    fn page_message(&self) -> Option<String> {
        let core = self.session.core();
        let tab = core.tabs().nth(core.active())?;
        match tab.content {
            Content::Blank => tab.error.map(|error| error.message.clone()),
            Content::Internal(_) => Some(INTERNAL.into()),
            Content::View(_) if self.tui.graphics => None,
            Content::View(_) => Some(
                match self.tui.settings.mode {
                    TuiMode::Text => NO_TEXT,
                    _ if self.tui.can_show_images() => TOO_BIG,
                    _ => NO_GRAPHICS,
                }
                .into(),
            ),
        }
    }

    /// The chrome's lines, from the same facts the window's snapshot has. Contained by the
    /// caller.
    fn chrome_state(&self) -> ChromeState {
        let core = self.session.core();
        let tabs = core
            .tabs()
            .map(|tab| TabLine {
                engine: tab_engine(core, &tab).map(str::to_owned),
                title: tab.title.into_owned(),
                url: tab.url.unwrap_or_default().to_owned(),
                loading: tab.loading,
            })
            .collect();
        let active = core.active();
        let status = core.tabs().nth(active).and_then(|tab| {
            let error = tab_error(&tab);
            status_line(
                self.tui.notice.as_deref(),
                self.tui.notice_first.is_some(),
                tab.status,
                error,
            )
        });
        let (notice, status) = match status {
            Some(("notice", text)) => (Some(text.to_owned()), None),
            other => (None, other.map(|(_, text)| text.to_owned())),
        };
        ChromeState {
            tabs,
            active,
            address_editing: self.tui.editing.clone(),
            mode_label: match self.tui.settings.mode {
                TuiMode::Text => MODE_TEXT,
                _ => MODE_GRAPHICS,
            }
            .into(),
            keymap_indicator: core.keymap_indicator().map(str::to_owned),
            notice,
            status,
        }
    }

    /// Presents the shown view if its engine has a new frame and its time has come, or sends
    /// its last frame again if the screen lost it, as a kitty image into `out`.
    fn frame(&mut self, out: &mut Vec<u8>) {
        let Some((view, target)) = self.shown() else {
            return;
        };
        let metrics = self.tui.metrics;
        let Some(pixels) = self
            .tui
            .targets
            .get_mut(&target)
            .filter(|_| self.tui.graphics)
        else {
            return;
        };
        let now = Instant::now();
        if pixels.ready && self.tui.next_frame.is_none_or(|at| at <= now) {
            pixels.ready = false;
            pixels.permits += 1;
            let permit = TermPermit {
                epoch: metrics.epoch(),
                size: metrics.buffer(),
            };
            let outcome = self.session.present(view, permit, metrics.buffer());
            let Some(pixels) = self.tui.targets.get_mut(&target) else {
                return;
            };
            match outcome {
                Some(Ok(PresentOutcome::Submitted)) => {
                    pixels.presented += 1;
                    pixels.on_screen = false;
                    self.tui.next_frame = Some(now + frame_interval(self.tui.settings));
                }
                Some(Ok(PresentOutcome::NotReady | PresentOutcome::Unchanged)) => {}
                // The session counted and logged it.
                Some(Err(_)) | None => {
                    pixels.dropped += 1;
                    self.session.diagnostics_mut().counters.frames_dropped += 1;
                    return;
                }
            }
        }
        let Some(pixels) = self.tui.targets.get_mut(&target) else {
            return;
        };
        if pixels.on_screen {
            return;
        }
        let page = self.tui.page;
        let place = CellRect {
            column: 0,
            row: page.row,
            columns: page.cols,
            rows: page.rows,
        };
        let max = OUTPUT_CAP.saturating_sub(out.len());
        let image = pixels.image;
        let Some(encoded) = pixels.handle.with_frame(metrics.epoch(), |frame| {
            kitty::encode(frame, image, place, max)
        }) else {
            // No frame for this layout yet: the engine's next one brings it.
            return;
        };
        let diag = self.session.diagnostics_mut();
        match encoded {
            Ok(bytes) => {
                diag.counters.encoded_bytes += bytes.len() as u64;
                out.extend(bytes);
                pixels.on_screen = true;
            }
            Err(error) => {
                diag.counters.frames_dropped += 1;
                diag.error(format!("a page frame was not sent: {error}"));
                pixels.dropped += 1;
                // Not again until the engine has another frame.
                pixels.on_screen = true;
            }
        }
    }

    fn reload_config(&mut self) {
        self.session.diagnostics_mut().counters.config_reloads += 1;
        match persistence::load_config(
            self.session.paths().config.as_deref(),
            self.session.environment(),
        ) {
            Ok(config) => {
                // Saving the session again: the tabs as they are now go to the file soon.
                if config.restore_session() && !self.session.core().config().restore_session() {
                    self.session.session_changed();
                }
                let settings = *config.tui();
                self.session
                    .queue_mut()
                    .push_back(BrowserEvent::ConfigReloaded(Ok(Box::new(config))));
                self.tui.notice_first = None;
                self.tui.notice = None;
                self.tui.adopt(settings);
            }
            Err(persistence::LoadError::Config(error)) => {
                log::warn!("the configuration in use stays: {error}");
                self.session
                    .queue_mut()
                    .push_back(BrowserEvent::ConfigReloaded(Err(error)));
            }
            Err(persistence::LoadError::Other(message)) => {
                log::warn!("the configuration in use stays: {message}");
                self.tui
                    .show_notice(format!("{message} The configuration in use stays."));
            }
        }
        self.tui.chrome_dirty = true;
        self.run_core();
    }

    /// Shuts down in plan §20's order ([`Session::shutdown`]), then restores the terminal. If an
    /// engine could not shut down, the session restores it through
    /// [`Frontend::before_forced_exit`] and ends the process.
    fn shutdown(mut self) -> ExitCode {
        let code = self
            .session
            .shutdown(&mut self.tui, |session, tui| report(tui, session, true));
        self.tui.leave();
        code
    }
}

impl Tui {
    /// Takes `[tui]`'s settings: the mouse, and the layout they imply.
    fn adopt(&mut self, settings: TuiConfig) {
        if settings.mouse != self.mouse_on {
            self.control.extend_from_slice(match settings.mouse {
                true => b"\x1b[?1000h\x1b[?1006h",
                false => b"\x1b[?1006l\x1b[?1000l",
            });
            self.mouse_on = settings.mouse;
        }
        let interval = frame_interval(settings);
        self.next_frame = self.next_frame.map(|at| at.min(Instant::now() + interval));
        self.settings = settings;
        self.relayout();
    }

    /// Whether the terminal shows kitty images, as `[tui]` asks.
    fn can_show_images(&self) -> bool {
        let size = self.terminal.size();
        let kitty = self.terminal.capabilities().kitty_graphics_ok == Some(true);
        let wanted = match self.settings.graphics {
            GraphicsProtocol::Auto | GraphicsProtocol::Kitty => kitty,
            // Not implemented yet.
            GraphicsProtocol::Sixel | GraphicsProtocol::None => false,
        };
        wanted && self.settings.mode != TuiMode::Text && size.cell_width > 0 && size.cell_height > 0
    }

    /// The page rows and their pixels for the terminal's size. A changed size is a new layout
    /// epoch, which the views are told of on the next pass.
    fn relayout(&mut self) {
        let size = self.terminal.size();
        let page = page_area(size);
        let width = u32::from(page.cols) * u32::from(size.cell_width);
        let height = u32::from(page.rows) * u32::from(size.cell_height);
        let fits = u64::from(width) * u64::from(height) <= kitty::MAX_PIXELS;
        let graphics = self.can_show_images() && fits && width > 0 && height > 0;
        let (width, height) = if graphics { (width, height) } else { (0, 0) };
        if graphics != self.graphics {
            self.graphics = graphics;
            self.clear = true;
        }
        let rect = self.metrics.rect();
        if (rect.width, rect.height) != (width, height) {
            let rect = WindowRect {
                x: 0,
                y: 0,
                width,
                height,
            };
            self.metrics = Metrics::new(LayoutEpoch::next(), rect, Scale::ONE);
            self.metrics_due = true;
        }
        self.page = page;
        self.chrome_dirty = true;
    }

    /// Clears the screen and draws the chrome and the shown page again.
    fn redraw_all(&mut self) {
        self.clear = true;
    }

    fn show_notice(&mut self, message: String) {
        self.notice = Some(message);
        self.notice_first = Some(Instant::now() + NOTICE_FIRST);
        self.chrome_dirty = true;
    }

    /// `location.focus`: opens the address line on the active tab's address, selected.
    fn open_address(&mut self) {
        let mut editor = LineEditor::from_text(self.active_url.as_str());
        editor.select_all();
        self.editing = Some(editor);
        self.chrome_dirty = true;
    }

    fn close_address(&mut self) {
        if self.editing.take().is_some() {
            self.chrome_dirty = true;
        }
    }

    /// A key for the address line, as the window's address bar takes it.
    fn edit_key(&mut self, event: &KeyEvent, queue: &mut VecDeque<BrowserEvent>) {
        let Some(editor) = self.editing.as_mut() else {
            return;
        };
        if event.state == ButtonState::Released {
            return;
        }
        // Control as a modifier, not as a key the layout used to make a character.
        let word = event.modifiers.control && !event.consumed.control;
        // Alt the same way: a shortcut the core didn't bind types nothing.
        let alt = event.modifiers.alt && !event.consumed.alt;
        match &event.key {
            Key::Named(name) => match name.as_str() {
                "Escape" => {
                    self.close_address();
                    queue.push_back(BrowserEvent::Focus(KeyboardFocus::Page));
                    return;
                }
                "Enter" => return self.submit(queue),
                "ArrowLeft" if word => editor.word_left(),
                "ArrowRight" if word => editor.word_right(),
                "ArrowLeft" => editor.left(),
                "ArrowRight" => editor.right(),
                "Home" => editor.home(),
                "End" => editor.end(),
                "Backspace" | "Delete" if word => editor.delete_word(name == "Delete"),
                "Backspace" => editor.backspace(),
                "Delete" => editor.delete(),
                _ => return,
            },
            Key::Character(letter) if word => match letter.as_str() {
                "a" | "A" => editor.select_all(),
                _ => return,
            },
            _ => match event.text.as_deref().filter(|_| !alt && !word) {
                Some(text) => editor.insert(text),
                None => return,
            },
        }
        self.chrome_dirty = true;
    }

    /// Enter: open what was typed, or run it as a command after a `:`. The window's address
    /// bar does the same; a command that doesn't parse closes the line here, with its error as
    /// a notice, since the line covers the notice while it is open.
    fn submit(&mut self, queue: &mut VecDeque<BrowserEvent>) {
        let line = self
            .editing
            .as_ref()
            .map_or("", |editor| editor.text().trim())
            .to_owned();
        if line.is_empty() {
            self.close_address();
            queue.push_back(BrowserEvent::Focus(KeyboardFocus::Page));
        } else if let Some(command) = line.strip_prefix(':') {
            self.close_address();
            queue.push_back(BrowserEvent::Focus(KeyboardFocus::Page));
            match command.parse::<Command>() {
                Ok(command) => queue.push_back(BrowserEvent::Command(command)),
                Err(error) => self.show_notice(format!(":{}: {error}", command.trim())),
            }
        } else {
            // The core gives the page the keyboard, which closes the line.
            queue.push_back(BrowserEvent::Command(Command::PageOpen(line)));
        }
    }

    /// Deletes every image and gives the terminal back. Bytes still waiting are dropped: what
    /// they would have drawn is about to be left behind with the alternate screen.
    fn leave(&mut self) {
        self.terminal.discard_pending();
        if self.graphics {
            let _ = self.terminal.write_frame(kitty::DELETE_ALL);
        }
        let _ = self.terminal.restore();
    }
}

impl Clipboard for Tui {
    /// Refused on the next pass: the terminal has no clipboard to read yet.
    fn read(&mut self, request: RequestId, _: Selection, _: &[String], _: usize) {
        self.refused_reads.push(request);
    }

    fn write(&mut self, _: Selection, _: Vec<ClipboardData>) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn cancel(&mut self, request: RequestId) {
        self.refused_reads.retain(|&read| read != request);
    }
}

impl Frontend for Tui {
    type Target = TermTarget;
    type Permit = TermPermit;

    fn open_target(
        &mut self,
        view: ViewId,
        _engine: &str,
    ) -> Result<(TargetId, TermTarget), String> {
        let (target, handle) = TermTarget::new(self.metrics);
        let id = TargetId::next();
        let image = self.next_image;
        self.next_image = self
            .next_image
            .checked_add(1)
            .ok_or("no kitty image ids left")?;
        self.targets.insert(
            id,
            Pixels {
                image,
                handle,
                ready: false,
                on_screen: false,
                permits: 0,
                presented: 0,
                dropped: 0,
            },
        );
        self.views.insert(view, id);
        Ok((id, target))
    }

    /// The target was made with the current metrics; a later change reaches every view as
    /// `SetMetrics` on the next pass.
    fn place(&mut self, _views: &BTreeMap<ViewId, ViewEntry>, _diag: &mut Diagnostics) -> bool {
        false
    }

    fn forget_target(&mut self, target: TargetId) {
        self.views.retain(|_, &mut kept| kept != target);
        if let Some(pixels) = self.targets.remove(&target) {
            self.control.extend(kitty::delete(pixels.image));
        }
    }

    fn focus(&mut self, focus: KeyboardFocus, _queue: &mut VecDeque<BrowserEvent>) {
        match focus {
            KeyboardFocus::Chrome => self.open_address(),
            KeyboardFocus::Page => self.close_address(),
        }
    }

    fn chrome_input(&mut self, event: InputEvent, queue: &mut VecDeque<BrowserEvent>) {
        match event.kind {
            InputKind::Key(key) => self.edit_key(&key, queue),
            InputKind::TextInput(TextInputEvent {
                commit: Some(text), ..
            }) => {
                if let Some(editor) = &mut self.editing {
                    editor.insert(&text);
                    self.chrome_dirty = true;
                }
            }
            _ => {}
        }
    }

    fn chrome_command(&mut self, action: String, _queue: &mut VecDeque<BrowserEvent>) {
        match action.as_str() {
            "toggle-mode" | "text-mode" => self.show_notice("text mode isn't built yet".into()),
            "graphics-mode" if !self.graphics => self.show_notice(NO_GRAPHICS.into()),
            "graphics-mode" => {}
            "refresh" => self.redraw_all(),
            other => {
                let has = TERMINAL_CHROME_COMMANDS.join(", chrome.");
                self.show_notice(format!(
                    "The terminal's chrome has no command chrome.{other}; it has chrome.{has}."
                ));
            }
        }
    }

    /// A terminal has no pointer shapes.
    fn page_cursor(&mut self, _target: Option<TargetId>, _cursor: Cursor) {}

    fn page_text_input(&mut self, shown: Option<TargetId>, state: Option<TextInputState>) {
        self.page_text = shown.is_some() && state.is_some();
    }

    fn notice(&mut self, message: String) {
        self.show_notice(message);
    }

    fn redraw(&mut self) {
        self.chrome_dirty = true;
    }

    /// A shown view's last frame is sent again if it is still the layout's, or its next one
    /// is; a hidden view's image is deleted.
    fn show(&mut self, view: ViewId, visible: bool) {
        let Some(pixels) = self.views.get(&view).and_then(|t| self.targets.get_mut(t)) else {
            return;
        };
        if !visible && pixels.on_screen {
            self.control.extend(kitty::delete(pixels.image));
        }
        pixels.on_screen = false;
        pixels.ready &= visible;
        self.chrome_dirty = true;
    }

    fn frame_ready(&mut self, target: TargetId) {
        if let Some(pixels) = self.targets.get_mut(&target) {
            pixels.ready = true;
        }
    }

    fn before_forced_exit(&mut self) {
        self.leave();
    }
}

/// The page rows for a terminal of `size`: everything between the chrome's top and bottom
/// lines, as [`ChromeState::draw`] leaves them.
fn page_area(size: TermSize) -> PageArea {
    let rows = size.rows.saturating_sub(2);
    PageArea {
        row: u16::from(rows > 0),
        rows,
        cols: size.cols,
        cursor: None,
    }
}

fn empty_metrics() -> Metrics {
    Metrics::new(LayoutEpoch::next(), WindowRect::default(), Scale::ONE)
}

fn frame_interval(settings: TuiConfig) -> Duration {
    Duration::from_secs(1) / settings.fps.max(1)
}

/// The active tab's address, or what it is loading, for the address line.
fn active_url(core: &BrowserState) -> String {
    core.tabs()
        .nth(core.active())
        .and_then(|tab| tab.url.or(tab.requested))
        .unwrap_or_default()
        .to_owned()
}

/// The engine `tab`'s page runs in: its view's, and none for `sig:` pages and blank tabs.
fn tab_engine<'a>(core: &'a BrowserState, tab: &TabSummary<'_>) -> Option<&'a str> {
    match tab.content {
        Content::View(view) => core.view(view).map(|view| view.engine()),
        Content::Internal(_) | Content::Blank => None,
    }
}

/// Why `tab`'s navigation failed, for the bottom line: unless the page rows say it, as they do
/// for a tab with nothing else to show ([`App::page_message`]), as the window's page area does.
fn tab_error<'a>(tab: &TabSummary<'a>) -> Option<&'a str> {
    let error = tab.error.filter(|_| !matches!(tab.content, Content::Blank));
    error.map(|error| error.message.as_str())
}

/// What the bottom line shows, and where it comes from, as the window's status strip: a notice
/// of `sig`'s own while it is `fresh`; then the page's status text, then why a navigation failed
/// while the page stayed; then an older notice.
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

/// The diagnostics report. Called on SIGUSR1 and, from [`Session::shutdown`], at exit.
fn report(tui: &Tui, session: &mut Session<Tui>, at_exit: bool) {
    let buffer = tui.metrics.buffer();
    session.account_readbacks(u64::from(buffer.width) * u64::from(buffer.height) * 4);
    let targets = session
        .views()
        .iter()
        .filter_map(|(&view, entry)| {
            let pixels = tui.targets.get(&entry.target)?;
            let (engine, tab) = session.view_report(view);
            let rect = tui.metrics.rect();
            Some(TargetReport {
                role: "view",
                engine: Some(engine),
                tab,
                width: rect.width,
                height: rect.height,
                buffer_width: buffer.width,
                buffer_height: buffer.height,
                scale: tui.metrics.scale().numerator(),
                mapped: tui.graphics,
                shown: entry.visible,
                awaiting_callback: false,
                permits: pixels.permits,
                submissions: pixels.presented,
                frame_callbacks: 0,
                releases: 0,
                presented: pixels.presented,
                discarded: pixels.dropped,
                late: 0,
                buffers: usize::from(pixels.handle.buffer_bytes() > 0),
                buffer_bytes: pixels.handle.buffer_bytes(),
                retiring_generations: 0,
                retiring_bytes: 0,
            })
        })
        .collect();
    let core = session.core();
    let tabs = core
        .tabs()
        .map(|tab| TabReport {
            loading: tab.loading,
            can_go_back: tab.can_go_back,
            can_go_forward: tab.can_go_forward,
            reloads: tab.reloads,
            error: tab.error.is_some(),
            engine: tab_engine(core, &tab).map(str::to_owned),
        })
        .collect();
    let active_tab = core.active() + 1;
    let status = core.tabs().nth(core.active()).and_then(|tab| {
        let error = tab_error(&tab);
        status_line(
            tui.notice.as_deref(),
            tui.notice_first.is_some(),
            tab.status,
            error,
        )
        .map(|(from, _)| from)
    });
    let size = tui.terminal.size();
    let graphics = if tui.graphics { "kitty" } else { "none" };
    let report = Report {
        counters: session.diagnostics().counters.clone(),
        peaks: session.diagnostics().peaks.clone(),
        targets,
        engines: session.engine_reports(),
        tabs,
        active_tab,
        status,
        capabilities: format!(
            "terminal graphics={graphics} cells={}x{} cell={}x{}",
            size.cols, size.rows, size.cell_width, size.cell_height
        ),
        pending_host_events: 0,
        last_error: session.diagnostics().last_error(),
    };
    session.diagnostics_mut().report(&report, at_exit);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_notice_comes_before_the_page_and_errors_show() {
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
        assert_eq!(status_line(None, false, None, None), None);
    }

    #[test]
    fn the_page_is_between_the_chrome_lines() {
        let size = |rows| TermSize {
            cols: 80,
            rows,
            cell_width: 10,
            cell_height: 20,
        };
        assert_eq!((page_area(size(24)).row, page_area(size(24)).rows), (1, 22));
        assert_eq!((page_area(size(2)).row, page_area(size(2)).rows), (0, 0));
        let mut grid = Grid::new(80, 24);
        let drawn = ChromeState::default().draw(&mut grid);
        assert_eq!((drawn.row, drawn.rows, drawn.cols), (1, 22, 80));
    }

    #[test]
    fn frames_are_paced_by_fps() {
        let fps = |fps| TuiConfig {
            fps,
            ..TuiConfig::default()
        };
        assert_eq!(frame_interval(fps(10)), Duration::from_millis(100));
        assert_eq!(frame_interval(fps(0)), Duration::from_secs(1));
    }
}
