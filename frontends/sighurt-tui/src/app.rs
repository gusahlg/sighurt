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
//! Text mode (TUI.md §4, [`crate::text`]): `[tui] mode`, or `chrome.toggle-mode` for the run,
//! decides whether the page rows show pixels or the page's text. In text mode the shown view's
//! engine is asked for snapshots ([`Session::set_semantics`]), the others' are not, and the
//! keys the keymap left for the page, and the mouse, act on the text: scrolling is the
//! frontend's, an activation goes to the core as `BrowserEvent::Semantic`. Text mode tells the
//! core `BrowserEvent::PageTyping(false)`, so a focused field does not swallow character
//! bindings; graphics tells it `true`. A press in the page in graphics mode captures the
//! pointer until that button is released.
//!
//! Page frames (TUI.md §2 and §8.6): a permit only for the shown view, only when its engine has
//! a new frame, at most `[tui] fps` a second (in text mode at most [`TEXT_FPS`], and only when
//! that engine gives page text), and only once every byte written before has left the output
//! buffer: nothing is written while any is waiting, so at most one encoded frame is ever
//! pending and no write is cut short. The engine paints into its view's [`TermTarget`] and the
//! frame is encoded as a kitty or sixel image over the page rows. Text mode takes the frame and
//! does not encode it, so switching back to graphics shows the latest one. A resize is a new
//! layout epoch: output queued for the old layout is discarded, and a kitty upload the cut
//! interrupted is ended with an empty `m=0` chunk. Bytes the kernel already accepted cannot be
//! unsent. A hidden or closed view's image is deleted; a sixel, which has no id, by rewriting
//! the page rows.
//!
//! Logs go to `{state_dir}/sighurt/tui.log` before any engine starts. The terminal's modes are
//! turned off before the engines shut down (its images are deleted first, while the alternate
//! screen is up), and restored on every other way out: error, panic (the terminal's hook),
//! SIGHUP (its handler) and [`Frontend::before_forced_exit`]; SIGTSTP suspends and SIGCONT
//! redraws everything.
//!
//! Must not own: browser policy (the core) or which effect runs (the session).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
use sighurt_api::semantic::SemanticSnapshot;
use sighurt_api::services::{ClipboardData, Selection, ServiceError};
use sighurt_api::settings::Table;
use sighurt_core::config::{GraphicsProtocol, TuiConfig, TuiMode, TERMINAL_CHROME_COMMANDS};
use sighurt_core::tabs::{Content, LoadError, TabSummary};
use sighurt_core::{
    BrowserEvent, BrowserState, ChromeChecks, Frontend as FrontendKind, OpenRequest,
};
use sighurt_runtime::diagnostics::{Diagnostics, Report, TabReport, TargetReport};
use sighurt_runtime::frontend::Frontend;
use sighurt_runtime::persistence::{self, ConfigWatch, Paths, SessionWriter};
use sighurt_runtime::services::{Clipboard, Services};
use sighurt_runtime::session::Session;
use sighurt_runtime::view::ViewEntry;

use crate::chrome::{
    ChromeState, Cursor as CellCursor, LineEditor, PageArea, TabLine, MODE_GRAPHICS, MODE_TEXT,
};
use crate::grid::{Attrs, Color, Grid};
use crate::input::{
    cell_to_page_offset, cell_to_page_pixel, CellPixels, PageCells, PointerCapture, Release,
};
use crate::kitty::{self, CellRect};
use crate::sixel;
use crate::target::{TargetHandle, TermPermit, TermTarget};
use crate::term::{
    MouseEvent, TermEvent, TermSize, Terminal, Wheel, PROBE_DEADLINE, SYNC_BEGIN, SYNC_END,
};
use crate::text::{Act, TextPages};
use crate::textmode::Area;

/// Most terminal events handled in one pass; the rest wait for the next.
pub const MAX_EVENTS: usize = 256;
/// Most passes in one iteration before the loop goes round without sleeping.
pub const MAX_PASSES: usize = 8;
/// How long a notice of `sig`'s own comes before the page's status text.
const NOTICE_FIRST: Duration = Duration::from_secs(5);
/// The most bytes one write may leave waiting for the terminal: the bound on an encoded frame
/// (8 Mpx of RGBA, compressed, in base64, fits unless the page is noise).
const OUTPUT_CAP: usize = 16 * 1024 * 1024;
/// How often text mode takes a frame. Still at most `[tui] fps`. The frame is taken so a page
/// keeps drawing and graphics shows the latest one, not so it can be read back at `[tui] fps`.
const TEXT_FPS: u32 = 2;
/// An empty last chunk. Ends a kitty image upload a resize cut after its `m=1`.
const KITTY_UPLOAD_END: &[u8] = b"\x1b_Gq=2,m=0;\x1b\\";
/// The most pasted text the address line or a page's field takes.
const MAX_PASTE: usize = 1024 * 1024;
/// The page rows' message when the terminal shows no images.
const NO_GRAPHICS: &str =
    "This terminal can't show graphics; chrome.text-mode shows the page as text";
/// The page rows' message in text mode when the shown page's engine gives no page text. The
/// keys are the built-in `[tui.keys]` ones.
const NO_SEMANTICS: &str = "No page text from this engine: e switches engine, m shows graphics";
/// The page rows' message in text mode until the first snapshot of the page arrives.
const READING: &str = "Reading the page…";
/// Lines a wheel notch scrolls in text mode.
const WHEEL_LINES: isize = 3;
/// The page rows' message when the page area has more pixels than an image may.
const TOO_BIG: &str = "The page area is too big for an image";
/// The page rows' message for a `sig:` page.
const INTERNAL: &str = "sig: pages aren't shown in the terminal yet";
/// The most characters of an address the page rows' loading message shows.
const MESSAGE_URL: usize = 256;
/// The chromes the window has, by name. The terminal ignores `[ui] chrome` and `[chrome.*]`,
/// but one configuration file serves both frontends, so it accepts what the window would.
/// The window checks those tables; here they are taken as they are.
const WINDOW_CHROMES: [&str; 2] = ["standard", "line"];
/// The color names `[ui.colors]` takes: `sighurt_ui::Colors::NAMES`, which this crate can't
/// name (it never depends on sighurt-ui). The terminal chrome doesn't use them yet.
pub(crate) const COLOR_NAMES: &[&str] = &[
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
pub(crate) fn log_path() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("sighurt")
        .join("tui.log")
}

/// Which image protocol the current layout uses. Chosen in [`Tui::relayout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    /// Text, or graphics the terminal cannot show.
    None,
    /// Kitty graphics protocol.
    Kitty,
    /// Sixel. Full frame, at most 5 a second.
    Sixel,
}

/// One write that has not finished leaving the output buffer.
struct Outbound {
    /// The write's length. Bytes still queued are its suffix.
    len: usize,
    /// A chunked kitty upload in that write, if it has an `m=1` chunk.
    kitty: Option<ChunkedSpan>,
}

/// Where a kitty upload that started with `m=1` sits in a write.
///
/// `open_at` is the first byte after that `m=1`. `close_at` is the first byte after the `m=0`
/// chunk, or the write's length when the upload does not finish in it. A resize whose
/// already-sent prefix sits in `[open_at, close_at)` ends the upload, or the terminal waits.
struct ChunkedSpan {
    open_at: usize,
    close_at: usize,
}

impl ChunkedSpan {
    /// `sent` bytes of the write have already been accepted, and the cut is inside the upload.
    fn cut(self, sent: usize) -> bool {
        sent >= self.open_at && sent < self.close_at
    }
}

/// One view's target as the loop sees it.
struct Pixels {
    /// The kitty image id, from 1, never reused. Sixel does not use it.
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
    /// The page rows show images.
    graphics: bool,
    /// The protocol [`Self::relayout`] chose for this layout.
    protocol: Protocol,
    /// The page rows.
    page: PageArea,
    /// The page rows show pages as text.
    text_mode: bool,
    /// The last `PageTyping` sent to the core. `None` until the first.
    typing_told: Option<bool>,
    /// `chrome.toggle-mode`'s choice for this run (true: text), over `[tui] mode`.
    mode_override: Option<bool>,
    /// Each view's page as text, and the field editor over it.
    text: TextPages,
    /// The views whose engines were asked for page content.
    semantic_on: HashSet<ViewId>,
    /// The view and button of a press in the page, until that button is released.
    capture: PointerCapture,
    /// Releases of captures ended by the frontend, for the app to send.
    releases: Vec<Release>,
    /// What every view is given: the page rows' pixels (a default area in text mode with no pixel
    /// size), or nothing when no page is shown that way.
    metrics: Metrics,
    /// `metrics` changed and the views have not been told.
    metrics_due: bool,
    targets: HashMap<TargetId, Pixels>,
    /// Each view's target, for the core's show and hide.
    views: HashMap<ViewId, TargetId>,
    next_image: u32,
    /// The address line, while `location.focus` has it.
    editing: Option<LineEditor>,
    /// The address line opened during this run of the core and nothing was typed into it yet:
    /// it starts from the active tab's address as the run left it (`tab.new` opens it).
    editing_fresh: bool,
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
    /// The last write still draining, so a resize can see whether it cut a kitty upload.
    outbound: Option<Outbound>,
    /// Images were deleted on the way out. A later [`Self::leave`] only restores again.
    left: bool,
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
pub(crate) fn redirect_stderr(path: &Path) -> Result<OwnedFd, String> {
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
pub(crate) fn chrome_checks() -> Vec<ChromeChecks> {
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
            protocol: Protocol::None,
            text_mode: false,
            typing_told: None,
            mode_override: None,
            text: TextPages::new(size.cols, page_area(size).rows),
            semantic_on: HashSet::new(),
            capture: PointerCapture::default(),
            releases: Vec::new(),
            page: page_area(size),
            metrics: empty_metrics(),
            metrics_due: false,
            targets: HashMap::new(),
            views: HashMap::new(),
            next_image: 1,
            editing: None,
            editing_fresh: false,
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
            outbound: None,
            left: false,
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

    /// Hands the queued events to the core. An address line they opened starts from the active
    /// tab's address as they left it, unless something was typed into it meanwhile.
    fn run_core(&mut self) {
        self.tui.active_url = active_url(self.session.core());
        self.session.run_core(&mut self.tui);
        if std::mem::take(&mut self.tui.editing_fresh) {
            let url = active_url(self.session.core());
            if url != self.tui.active_url {
                self.tui.active_url = url;
                self.tui.open_address();
                self.tui.editing_fresh = false;
            }
        }
        // A view the core just hid: its captured buttons are released in this turn.
        if !self.tui.releases.is_empty() {
            self.flush_releases();
        }
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
            // After the pump, which may have opened a view: nothing is drawn for it before the
            // engine is asked for its text.
            worked |= self.sync_semantics();
            // Startup in text mode, a resize that loses images, a reload of `[tui] mode`.
            // `chrome_command` already told the core when `m` changed the mode.
            let mut typing = VecDeque::new();
            if self.tui.sync_typing(&mut typing) {
                self.session.queue_mut().extend(typing);
                self.run_core();
                worked = true;
            }
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
        let Some((view, target)) = self.shown() else {
            return false;
        };
        takes_frames(
            self.tui.graphics,
            self.tui.text_mode,
            self.tui.semantic_on.contains(&view),
        ) && self
            .tui
            .targets
            .get(&target)
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
        // The span belonged to the screen `SIGCONT` already left.
        self.tui.outbound = None;
        self.tui.terminal.discard_pending();
        self.tui.mouse_on = true;
        let settings = self.tui.settings;
        self.tui.adopt(settings);
        self.tui.redraw_all();
    }

    /// Reads the terminal if it woke the loop, and hands at most [`MAX_EVENTS`] events to the
    /// core, one at a time. Returns whether there were any.
    fn drain_input(&mut self) -> bool {
        // One read can hold more events than a pass handles. The terminal is read again only
        // once those are handled, so the queue stays within one read's bound.
        if self.input.is_empty() && std::mem::take(&mut self.readable) {
            match self.tui.terminal.read_events() {
                Ok(events) => self.input.extend(events),
                Err(error) => {
                    self.fail(format!("reading the terminal failed: {error}"));
                    return false;
                }
            }
        }
        if self.input.is_empty() {
            self.input.extend(self.tui.terminal.expire_if_due());
        }
        let peaks = &mut self.session.diagnostics_mut().peaks;
        peaks.max_host_queue = peaks.max_host_queue.max(self.input.len() as u64);
        let count = self.input.len().min(MAX_EVENTS);
        for event in self.input.drain(..count).collect::<Vec<_>>() {
            self.event(event);
            self.run_core();
        }
        // Read again next pass, in case the terminal had more than one read took.
        self.readable |= count > 0;
        self.flush_releases();
        count > 0
    }

    fn event(&mut self, event: TermEvent) {
        let at = Instant::now();
        let seat = self.tui.seat;
        let keyboard = |kind| BrowserEvent::Keyboard {
            event: InputEvent { seat, kind },
            at,
        };
        // A pointer held when the terminal loses focus will not be released where we see it.
        if event == TermEvent::Focus(false) {
            self.tui.release_pointer(None);
        }
        let event = match event {
            TermEvent::Resize(_) => {
                // The queued bytes are the old layout's. A kitty upload already begun is ended
                // so the terminal does not wait for the rest of it.
                self.tui.discard_for_resize();
                self.tui.relayout();
                return self.tui.redraw_all();
            }
            // A field being edited in text mode or an option being chosen has the keyboard: the
            // keymap never sees these, so typing `q` into a field is typing.
            TermEvent::Key(key) if self.tui.text.prompting() => {
                self.tui.chrome_dirty = true;
                for act in self.tui.text.prompt_key(&key) {
                    self.session.queue_mut().push_back(semantic_event(act, at));
                }
                return;
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
        if self.tui.text.paste(&text) {
            self.tui.chrome_dirty = true;
            return None;
        }
        if let Some(editor) = &mut self.tui.editing {
            editor.paste(&text);
            self.tui.editing_fresh = false;
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
        let seat = self.tui.seat;
        let pointer = |kind| InputEvent {
            seat,
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
        if self.tui.text_mode {
            return self.text_mouse(&mouse, at);
        }
        // A button pressed over the page owns the pointer until it is released, wherever the
        // pointer goes: the engine would otherwise stay in a drag.
        if mouse.wheel.is_none() {
            if let Some(captured) = self.tui.capture.held() {
                if !self.session.views().contains_key(&captured) {
                    self.tui.capture.forget(captured);
                } else if let Some((captured, held)) =
                    self.tui
                        .capture
                        .route(mouse.button, mouse.motion, mouse.state)
                {
                    let position = self.page_offset(&mouse)?;
                    self.tui.capture.moved(position);
                    let kind = if mouse.motion {
                        PointerEvent::Motion {
                            position,
                            modifiers,
                        }
                    } else {
                        PointerEvent::Button {
                            button: held,
                            state: mouse.state,
                            position,
                            modifiers,
                        }
                    };
                    let event = pointer(kind);
                    return Some(BrowserEvent::Pointer {
                        view: captured,
                        event,
                        at,
                    });
                }
            }
        }
        let (view, position) = (view?, point?);
        self.tui.capture.moved(position);
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
            let button = mouse.button?;
            if mouse.state == ButtonState::Pressed {
                self.tui.capture.press(view, button);
            }
            PointerEvent::Button {
                button,
                state: mouse.state,
                position,
                modifiers,
            }
        };
        let event = pointer(kind);
        Some(BrowserEvent::Pointer { view, event, at })
    }

    /// Sends the releases of pointer captures the frontend ended (see [`Tui::release_pointer`]).
    fn flush_releases(&mut self) {
        let releases = std::mem::take(&mut self.tui.releases);
        let (seat, at) = (self.tui.seat, Instant::now());
        for release in releases {
            let event = InputEvent {
                seat,
                kind: InputKind::Pointer(PointerEvent::Button {
                    button: release.button,
                    state: ButtonState::Released,
                    position: release.position,
                    modifiers: Default::default(),
                }),
            };
            let view = release.view;
            self.session
                .queue_mut()
                .push_back(BrowserEvent::PointerRelease { view, event, at });
        }
        self.run_core();
    }

    /// Text mode's mouse: the wheel scrolls the page, a left press focuses and activates the
    /// link or control under it. Side buttons were the core's already.
    fn text_mouse(&mut self, mouse: &MouseEvent, at: Instant) -> Option<BrowserEvent> {
        let (view, _) = self.shown()?;
        let page = self.tui.page;
        let inside =
            (page.row..page.row + page.rows).contains(&mouse.row) && mouse.column < page.cols;
        if !inside || !self.tui.semantic_on.contains(&view) || self.tui.text.prompting() {
            return None;
        }
        if let Some(wheel) = mouse.wheel {
            let lines = match wheel {
                Wheel::Up => -WHEEL_LINES,
                Wheel::Down => WHEEL_LINES,
                Wheel::Left | Wheel::Right => 0,
            };
            self.tui.text.wheel(view, lines);
            self.tui.chrome_dirty = true;
            return None;
        }
        if mouse.motion
            || mouse.state != ButtonState::Pressed
            || mouse.button != Some(PointerButton::Left)
        {
            return None;
        }
        self.tui.chrome_dirty = true;
        let act = self
            .tui
            .text
            .click(view, mouse.column, mouse.row - page.row)?;
        Some(semantic_event(act, at))
    }

    /// Where the mouse's cell is relative to the page's top-left, in pixels, also when it is off
    /// the page.
    fn page_offset(&self, mouse: &MouseEvent) -> Option<SurfacePoint> {
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
        let (x, y) = cell_to_page_offset(mouse.column, mouse.row, page, cell)?;
        Some(SurfacePoint {
            x: x as f64,
            y: y as f64,
        })
    }

    /// Asks the engines for page content where text mode shows it (the shown view, if its
    /// engine has it), and stops where it no longer does. Returns whether it changed any.
    fn sync_semantics(&mut self) -> bool {
        let live: HashSet<ViewId> = self.session.views().keys().copied().collect();
        self.tui.semantic_on.retain(|view| live.contains(view));
        let wants: Vec<(ViewId, bool)> = self
            .session
            .views()
            .iter()
            .map(|(&view, entry)| {
                let has = self
                    .session
                    .registry()
                    .engine(&entry.engine)
                    .is_some_and(|engine| engine.capabilities.semantics);
                (view, self.tui.text_mode && entry.visible && has)
            })
            .collect();
        let mut changed = false;
        for (view, want) in wants {
            if want == self.tui.semantic_on.contains(&view) {
                continue;
            }
            self.session.set_semantics(view, want);
            if want {
                self.tui.semantic_on.insert(view);
            } else {
                self.tui.semantic_on.remove(&view);
                self.tui.text.forget(view);
            }
            self.tui.chrome_dirty = true;
            changed = true;
        }
        changed
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
        if self.tui.terminal.pending_len() == 0 {
            self.tui.outbound = None;
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
        self.tui.note_outbound(&out);
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
        let mut cursor = area.cursor;
        if self.tui.text_mode {
            if let Some((view, _)) = self.shown() {
                let text_area = Area {
                    col: 0,
                    row: area.row,
                    cols: area.cols,
                    rows: area.rows,
                };
                let edit = self.tui.text.draw(view, &mut self.tui.grid, text_area);
                cursor = cursor.or(edit.map(|(column, row)| CellCursor { column, row }));
            }
        }
        out.extend(self.tui.grid.diff());
        match cursor {
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
        enum Kind {
            Blank,
            Internal,
            View,
        }
        let (kind, error, loading) = {
            let core = self.session.core();
            let tab = core.tabs().nth(core.active())?;
            let error = tab.error.map(error_line);
            let loading = match (tab.loading, tab.requested) {
                (true, Some(url)) => Some(url.chars().take(MESSAGE_URL).collect::<String>()),
                _ => None,
            };
            let kind = match tab.content {
                Content::Blank => Kind::Blank,
                Content::Internal(_) => Kind::Internal,
                Content::View(_) => Kind::View,
            };
            (kind, error, loading)
        };
        match kind {
            Kind::Blank => match (error, loading) {
                (Some(error), _) => Some(error),
                // A new tab's first page, until it has something to show.
                (None, Some(url)) => Some(format!("Loading {url}…")),
                (None, None) => None,
            },
            Kind::Internal => Some(error.unwrap_or_else(|| INTERNAL.into())),
            // The page's text covers these rows once its first snapshot is in.
            Kind::View if self.tui.text_mode => {
                let view = self.shown().map(|(view, _)| view)?;
                if self.tui.text.has(view) {
                    return None;
                }
                let reading = self.tui.semantic_on.contains(&view);
                Some(error.unwrap_or_else(|| if reading { READING } else { NO_SEMANTICS }.into()))
            }
            // A frame covers these rows. With none, they would be empty: say what failed.
            Kind::View if self.tui.graphics && self.shown_has_frame() => None,
            Kind::View if self.tui.graphics => error,
            Kind::View => Some(error.unwrap_or_else(|| {
                match self.tui.images_possible() {
                    true => TOO_BIG,
                    false => NO_GRAPHICS,
                }
                .into()
            })),
        }
    }

    /// Whether the shown view has a frame for this layout, so the page rows are not empty.
    fn shown_has_frame(&self) -> bool {
        let Some((_, target)) = self.shown() else {
            return false;
        };
        let Some(pixels) = self.tui.targets.get(&target) else {
            return false;
        };
        pixels
            .handle
            .with_frame(self.tui.metrics.epoch(), |_| ())
            .is_some()
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
                // What it is loading until something commits: a new tab's first page.
                url: tab.url.or(tab.requested).unwrap_or_default().to_owned(),
                loading: tab.loading,
            })
            .collect();
        let active = core.active();
        let error_text = core
            .tabs()
            .nth(active)
            .and_then(|tab| tab_error(&tab).map(error_line));
        let status = core.tabs().nth(active).and_then(|tab| {
            status_line(
                self.tui.notice.as_deref(),
                self.tui.notice_first.is_some(),
                tab.status,
                error_text.as_deref(),
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
            mode_label: match self.tui.text_mode {
                true => MODE_TEXT,
                false => MODE_GRAPHICS,
            }
            .into(),
            keymap_indicator: core.keymap_indicator().map(str::to_owned),
            notice,
            status,
        }
    }

    /// Presents the shown view if its engine has a new frame and its time has come, or sends
    /// its last frame again if the screen lost it, as an image into `out`.
    fn frame(&mut self, out: &mut Vec<u8>) {
        let Some((view, target)) = self.shown() else {
            return;
        };
        let metrics = self.tui.metrics;
        let interval = frame_interval(self.tui.settings, self.tui.pace_sixel(), self.tui.text_mode);
        let protocol = self.tui.protocol;
        let (cell_width, cell_height) = {
            let size = self.tui.terminal.size();
            (size.cell_width, size.cell_height)
        };
        let takes = takes_frames(
            self.tui.graphics,
            self.tui.text_mode,
            self.tui.semantic_on.contains(&view),
        );
        let Some(pixels) = self.tui.targets.get_mut(&target).filter(|_| takes) else {
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
                    self.tui.next_frame = Some(now + interval);
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
        // Text mode takes the frame and shows none of it.
        if pixels.on_screen || !self.tui.graphics {
            return;
        }
        let page = self.tui.page;
        let place = CellRect {
            column: 0,
            row: page.row,
            columns: page.cols,
            rows: page.rows,
        };
        // The blank and the synchronized-output brackets sit outside the encoder, so the
        // image itself still has to fit in what remains of the output cap.
        let mut prefix = Vec::new();
        if protocol == Protocol::Sixel {
            prefix.extend_from_slice(SYNC_BEGIN);
            blank_page(&mut prefix, page);
        }
        let suffix = if protocol == Protocol::Sixel {
            SYNC_END.len()
        } else {
            0
        };
        let max = OUTPUT_CAP
            .saturating_sub(out.len())
            .saturating_sub(prefix.len())
            .saturating_sub(suffix);
        let image = pixels.image;
        let Some(encoded) = pixels
            .handle
            .with_frame(metrics.epoch(), |frame| match protocol {
                Protocol::Kitty => {
                    kitty::encode(frame, image, place, max).map_err(|e| e.to_string())
                }
                Protocol::Sixel => sixel::encode(frame, place, cell_width, cell_height, max)
                    .map_err(|e| e.to_string()),
                Protocol::None => Err("no graphics protocol".into()),
            })
        else {
            // No frame for this layout yet: the engine's next one brings it.
            return;
        };
        let diag = self.session.diagnostics_mut();
        match encoded {
            Ok(bytes) => {
                diag.counters.encoded_bytes += bytes.len() as u64;
                if protocol == Protocol::Sixel {
                    // Spaces update tmux's grid, which drops the previous sixel, and the
                    // synchronized update keeps those spaces off the outer terminal. The
                    // sixel itself is written through. The cursor save inside the sixel
                    // restores the position after the spaces, not the grid's cursor.
                    out.extend_from_slice(&prefix);
                    out.extend(bytes);
                    out.extend_from_slice(SYNC_END);
                    self.tui.grid.forget_cursor();
                    self.tui.grid.forget_pen();
                } else {
                    out.extend(bytes);
                }
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

    /// Turns the terminal's modes off, then shuts the engines down ([`Session::shutdown`]).
    /// Images are deleted while the alternate screen is still up. Servo can take a while, and
    /// raw mode would leave the quit key's release in the tty for the shell. The second
    /// restore discards input that arrived during shutdown. If an engine could not shut down,
    /// the session restores through [`Frontend::before_forced_exit`] and ends the process.
    fn shutdown(mut self) -> ExitCode {
        self.tui.leave();
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
            if !settings.mouse {
                self.release_pointer(None);
            }
        }
        if settings.mode != self.settings.mode {
            self.mode_override = None;
        }
        self.settings = settings;
        self.relayout();
        // After relayout, so a switch onto sixel picks up the 5 fps cap, and text mode its own.
        let interval = frame_interval(self.settings, self.pace_sixel(), self.text_mode);
        self.next_frame = self.next_frame.map(|at| at.min(Instant::now() + interval));
    }

    /// Sixel frames are capped at 5 fps. Text mode on a sixel terminal is not.
    fn pace_sixel(&self) -> bool {
        self.graphics && self.protocol == Protocol::Sixel
    }

    /// Tells the core whether a focused field takes character keys. Text mode shows the page
    /// itself, so it does not. Idempotent: nothing is queued when the core was already told.
    /// Returns whether it queued `PageTyping`.
    fn sync_typing(&mut self, queue: &mut VecDeque<BrowserEvent>) -> bool {
        let typing = !self.text_mode;
        if self.typing_told == Some(typing) {
            return false;
        }
        self.typing_told = Some(typing);
        queue.push_back(BrowserEvent::PageTyping(typing));
        true
    }

    /// Remember `frame` while any of it is still queued, so a resize can tell a cut kitty
    /// upload from one the terminal has not started.
    fn note_outbound(&mut self, frame: &[u8]) {
        let pending = self.terminal.pending_len();
        // A queued terminator from an earlier sequence makes `pending` not a suffix of `frame`.
        self.outbound = (pending > 0 && pending <= frame.len()).then(|| Outbound {
            len: frame.len(),
            kitty: chunked_kitty_span(frame),
        });
    }

    /// Drop output queued for the layout being replaced. If the bytes already accepted began a
    /// chunked kitty upload and did not finish it, end it with an empty `m=0` chunk.
    fn discard_for_resize(&mut self) {
        let tail = self.outbound.take().is_some_and(|out| {
            let sent = out.len.saturating_sub(self.terminal.pending_len());
            out.kitty.is_some_and(|span| span.cut(sent))
        });
        self.terminal.discard_pending();
        if tail {
            let _ = self.terminal.write_frame(KITTY_UPLOAD_END);
        }
    }

    /// Drops the image a shown view left on the page. Kitty deletes by id. Sixel rewrites
    /// the page rows.
    fn drop_placed(&mut self, image: u32) {
        match self.protocol {
            Protocol::Kitty => self.control.extend(kitty::delete(image)),
            Protocol::Sixel => self.blank_placed(),
            Protocol::None => {}
        }
    }

    /// Writes spaces over the page rows so tmux drops the sixel it is holding there.
    fn blank_placed(&mut self) {
        blank_page(&mut self.control, self.page);
        self.grid.forget_cursor();
        self.grid.forget_pen();
    }

    /// Whether the page rows are text: what `chrome.toggle-mode` chose, else `[tui] mode`, which
    /// `auto` leaves to whether the terminal can show images.
    fn wants_text(&self) -> bool {
        self.mode_override.unwrap_or(match self.settings.mode {
            TuiMode::Text => true,
            TuiMode::Graphics => false,
            TuiMode::Auto => !self.images_possible(),
        })
    }

    /// Whether the terminal can show images the way `[tui] graphics` asks, at a known cell size.
    fn images_possible(&self) -> bool {
        let size = self.terminal.size();
        self.protocol != Protocol::None && size.cell_width > 0 && size.cell_height > 0
    }

    /// The page rows and their pixels for the terminal's size. A changed size is a new layout
    /// epoch, which the views are told of on the next pass.
    fn relayout(&mut self) {
        let (kitty_ok, sixel_ok) = {
            let caps = self.terminal.capabilities();
            (caps.kitty_graphics_ok, caps.sixel)
        };
        let protocol = choose_protocol(self.settings.graphics, kitty_ok, sixel_ok);
        // Sixel has no id to delete, and a kitty image would be the wrong picture.
        if protocol != self.protocol && self.graphics {
            self.clear = true;
        }
        self.protocol = protocol;
        let size = self.terminal.size();
        let page = page_area(size);
        let width = u32::from(page.cols) * u32::from(size.cell_width);
        let height = u32::from(page.rows) * u32::from(size.cell_height);
        let fits = u64::from(width) * u64::from(height) <= kitty::MAX_PIXELS;
        let text = self.wants_text();
        let graphics = !text && self.images_possible() && fits && width > 0 && height > 0;
        // Text mode shows no pixels but still gives the engine a real page area and takes its
        // frames (decision 105): pages that draw or commit only for a view whose frames are
        // taken give no text at none. A terminal without a pixel size gets a desktop's.
        let (width, height) = if graphics {
            (width, height)
        } else if text {
            match fits && width > 0 && height > 0 {
                true => (width, height),
                false => TEXT_AREA,
            }
        } else {
            (0, 0)
        };
        if graphics != self.graphics || text != self.text_mode {
            self.graphics = graphics;
            self.text_mode = text;
            self.clear = true;
            // The press was in the other mode's page: its release would not reach it now.
            self.release_pointer(None);
        }
        self.text.set_area(page.cols, page.rows);
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

    /// Ends the pointer captures of `view` (all of them for `None`) whose release will not come
    /// from the user: the mode changed, the view hid, the mouse turned off or focus left. The
    /// releases are sent on the next pass.
    fn release_pointer(&mut self, view: Option<ViewId>) {
        let cancelled = self.capture.cancel(view);
        self.releases.extend(cancelled);
    }

    /// `chrome.text-mode` and `chrome.graphics-mode`: this run's mode over `[tui] mode`.
    fn set_mode(&mut self, text: bool) {
        if !text && !self.images_possible() {
            return self.show_notice("This terminal can't show graphics".into());
        }
        self.mode_override = Some(text);
        self.relayout();
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
        self.editing_fresh = true;
        self.chrome_dirty = true;
    }

    fn close_address(&mut self) {
        self.editing_fresh = false;
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
        self.editing_fresh = false;
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

    /// Deletes every image, once, and gives the terminal back. Bytes still waiting are dropped:
    /// what they would have drawn is left behind with the alternate screen. A later call only
    /// restores again, which discards input queued since the first.
    fn leave(&mut self) {
        if !self.left {
            self.left = true;
            self.outbound = None;
            self.terminal.discard_pending();
            if self.graphics {
                match self.protocol {
                    Protocol::Kitty => {
                        let _ = self.terminal.write_frame(kitty::DELETE_ALL);
                    }
                    // The alternate screen's images are not dropped by leaving it.
                    Protocol::Sixel => {
                        let _ = self.terminal.write_frame(b"\x1b[H\x1b[2J");
                    }
                    Protocol::None => {}
                }
            }
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
        let gone: Vec<ViewId> = self
            .views
            .iter()
            .filter(|&(_, &kept)| kept == target)
            .map(|(&view, _)| view)
            .collect();
        for view in gone {
            self.text.forget(view);
            self.semantic_on.remove(&view);
            self.capture.forget(view);
        }
        self.views.retain(|_, &mut kept| kept != target);
        if let Some(pixels) = self.targets.remove(&target) {
            let on_screen = pixels.on_screen;
            match self.protocol {
                Protocol::Kitty => self.control.extend(kitty::delete(pixels.image)),
                Protocol::Sixel if on_screen => self.blank_placed(),
                Protocol::Sixel | Protocol::None => {}
            }
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
                    self.editing_fresh = false;
                    self.chrome_dirty = true;
                }
            }
            _ => {}
        }
    }

    fn chrome_command(&mut self, action: String, queue: &mut VecDeque<BrowserEvent>) {
        match action.as_str() {
            "toggle-mode" => self.set_mode(!self.text_mode),
            "text-mode" => self.set_mode(true),
            "graphics-mode" => self.set_mode(false),
            "refresh" => self.redraw_all(),
            other => {
                let has = TERMINAL_CHROME_COMMANDS.join(", chrome.");
                self.show_notice(format!(
                    "The terminal's chrome has no command chrome.{other}; it has chrome.{has}."
                ));
            }
        }
        // `m` then `q` in one read: this queue is drained before the next key is handled.
        self.sync_typing(queue);
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
        if !visible {
            self.release_pointer(Some(view));
        }
        let Some(&target) = self.views.get(&view) else {
            return;
        };
        let placed = self
            .targets
            .get(&target)
            .map(|pixels| (pixels.image, pixels.on_screen));
        let Some((image, on_screen)) = placed else {
            return;
        };
        if !visible && on_screen {
            self.drop_placed(image);
        }
        if let Some(pixels) = self.targets.get_mut(&target) {
            pixels.on_screen = false;
            pixels.ready &= visible;
        }
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

    /// In text mode the shown page's keys scroll, focus and activate its text; nothing reaches
    /// the engine (TUI.md §8.11).
    fn page_input(
        &mut self,
        view: ViewId,
        event: &InputEvent,
        activation: Option<Instant>,
        queue: &mut VecDeque<BrowserEvent>,
    ) -> bool {
        let InputKind::Key(key) = &event.kind else {
            return false;
        };
        if !self.text_mode || !self.semantic_on.contains(&view) {
            return false;
        }
        self.chrome_dirty = true;
        // An action takes the user's activation along only if the core took this key as the
        // user's: a key a command made (`:page.send-key enter`) scrolls and focuses but follows
        // nothing.
        if let Some(act) = self.text.page_key(view, key) {
            if let Some(at) = activation {
                queue.push_back(semantic_event(act, at));
            }
        }
        true
    }

    fn semantics_changed(&mut self, view: ViewId, snapshot: Box<SemanticSnapshot>) {
        if self.semantic_on.contains(&view) {
            self.text.snapshot(view, snapshot);
            self.chrome_dirty = true;
        }
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

/// The core's event for an action the user chose in the page's text.
fn semantic_event(act: Act, at: Instant) -> BrowserEvent {
    BrowserEvent::Semantic {
        view: act.view,
        document: act.document,
        action: act.action,
        link: act.link,
        at,
    }
}

/// The page area in text mode when the terminal reports no pixel size.
const TEXT_AREA: (u32, u32) = (1280, 800);

fn empty_metrics() -> Metrics {
    Metrics::new(LayoutEpoch::next(), WindowRect::default(), Scale::ONE)
}

fn frame_interval(settings: TuiConfig, sixel: bool, text: bool) -> Duration {
    let mut fps = match sixel {
        true => settings.fps.clamp(1, 5),
        false => settings.fps.max(1),
    };
    if text {
        fps = fps.min(TEXT_FPS);
    }
    Duration::from_secs(1) / fps
}

/// Graphics always takes frames. Text mode takes them only for an engine that gives page text:
/// nothing else reads the pixels.
fn takes_frames(graphics: bool, text_mode: bool, semantics: bool) -> bool {
    graphics || (text_mode && semantics)
}

#[cfg(test)]
/// Whether `sent` bytes of `frame` already began a chunked kitty upload and have not finished it.
fn resize_discard_tail(frame: &[u8], sent: usize) -> bool {
    chunked_kitty_span(frame).is_some_and(|span| span.cut(sent))
}

/// The first `m=1` kitty chunk in `frame` through its `m=0` chunk, if the upload is chunked.
fn chunked_kitty_span(frame: &[u8]) -> Option<ChunkedSpan> {
    let mut index = 0;
    let mut open_at = None;
    while let Some(at) = frame[index..]
        .windows(3)
        .position(|window| window == b"\x1b_G")
    {
        let body = index + at + 3;
        let rest = &frame[body..];
        let semi = rest.iter().position(|byte| *byte == b';');
        let st = rest.windows(2).position(|window| window == b"\x1b\\");
        // The control is the keys before `;`. A chunk with no `;` stops at `ST`, so a later
        // chunk's `m=1` is not read as this one's.
        let control_end = match (semi, st) {
            (Some(semi), Some(st)) => semi.min(st),
            (Some(semi), None) => semi,
            (None, Some(st)) => st,
            (None, None) => rest.len(),
        };
        let control = &rest[..control_end];
        let chunk_end = st.map_or(frame.len(), |at| body + at + 2);
        if open_at.is_none() {
            open_at = control
                .windows(3)
                .position(|window| window == b"m=1")
                .map(|at| body + at + 3);
        }
        if let Some(open_at) = open_at {
            if control.windows(3).any(|window| window == b"m=0") {
                return Some(ChunkedSpan {
                    open_at,
                    close_at: chunk_end,
                });
            }
        }
        if chunk_end <= body {
            break;
        }
        index = chunk_end;
        if index >= frame.len() {
            break;
        }
    }
    open_at.map(|open_at| ChunkedSpan {
        open_at,
        close_at: frame.len(),
    })
}

/// Kitty where the terminal answered the graphics query and `[tui] graphics` allows it,
/// otherwise sixel where DA1 lists it and the setting allows it, otherwise none.
fn choose_protocol(graphics: GraphicsProtocol, kitty_ok: Option<bool>, sixel: bool) -> Protocol {
    if matches!(graphics, GraphicsProtocol::Auto | GraphicsProtocol::Kitty)
        && kitty_ok == Some(true)
    {
        return Protocol::Kitty;
    }
    if matches!(graphics, GraphicsProtocol::Auto | GraphicsProtocol::Sixel) && sixel {
        return Protocol::Sixel;
    }
    Protocol::None
}

/// Spaces over every page row. tmux drops a sixel when a character is written on its
/// cells; erasing a row that is already blank does not. The spaces are written with plain
/// attributes, so the caller makes the grid forget its pen.
fn blank_page(out: &mut Vec<u8>, page: PageArea) {
    if page.cols == 0 || page.rows == 0 {
        return;
    }
    out.extend_from_slice(b"\x1b[0m");
    for row in 0..page.rows {
        let line = u32::from(page.row) + u32::from(row) + 1;
        let _ = write!(out, "\x1b[{line};1H");
        out.extend(std::iter::repeat_n(b' ', usize::from(page.cols)));
    }
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
fn tab_error<'a>(tab: &TabSummary<'a>) -> Option<&'a LoadError> {
    tab.error.filter(|_| !matches!(tab.content, Content::Blank))
}

/// The error, and the address that failed when the message does not already name it.
pub(crate) fn error_line(error: &LoadError) -> String {
    match error.url.as_deref() {
        Some(url) if !url.is_empty() && !error.message.contains(url) => {
            let url: String = url.chars().take(MESSAGE_URL).collect();
            format!("{} ({url})", error.message)
        }
        _ => error.message.clone(),
    }
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
        let error_text = tab_error(&tab).map(error_line);
        status_line(
            tui.notice.as_deref(),
            tui.notice_first.is_some(),
            tab.status,
            error_text.as_deref(),
        )
        .map(|(from, _)| from)
    });
    let size = tui.terminal.size();
    let graphics = match (tui.graphics, tui.protocol) {
        (true, Protocol::Sixel) => "sixel",
        (true, Protocol::Kitty) => "kitty",
        _ => "none",
    };
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
    fn blanking_the_page_resets_attributes_and_the_chrome_sends_its_own_again() {
        let mut grid = Grid::new(20, 6);
        let state = ChromeState::default();
        let page = state.draw(&mut grid);
        let first = grid.diff();
        assert!(!first.is_empty());
        let mut blank = Vec::new();
        blank_page(&mut blank, page);
        assert!(blank.starts_with(b"\x1b[0m"));
        grid.forget_cursor();
        grid.forget_pen();
        // A redraw that changes a chrome cell must not rely on the pen before the blank.
        grid.put_cluster(0, 0, "Z", Color::Default, Color::Default, Attrs::default());
        let diff = grid.diff();
        assert!(diff.windows(4).any(|w| w == b"\x1b[0m"), "{diff:?}");
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
        assert_eq!(
            frame_interval(fps(10), false, false),
            Duration::from_millis(100)
        );
        assert_eq!(frame_interval(fps(0), false, false), Duration::from_secs(1));
        assert_eq!(
            frame_interval(fps(10), true, false),
            Duration::from_millis(200)
        );
        assert_eq!(
            frame_interval(fps(5), true, false),
            Duration::from_millis(200)
        );
        assert_eq!(
            frame_interval(fps(4), true, false),
            Duration::from_millis(250)
        );
        assert_eq!(frame_interval(fps(0), true, false), Duration::from_secs(1));
        // Text mode is at most TEXT_FPS, and still at most the sixel cap.
        assert_eq!(
            frame_interval(fps(10), false, true),
            Duration::from_millis(500)
        );
        assert_eq!(
            frame_interval(fps(2), false, true),
            Duration::from_millis(500)
        );
        assert_eq!(frame_interval(fps(1), false, true), Duration::from_secs(1));
        assert_eq!(frame_interval(fps(0), false, true), Duration::from_secs(1));
        assert_eq!(
            frame_interval(fps(10), true, true),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn text_mode_takes_frames_only_when_the_engine_gives_page_text() {
        assert!(takes_frames(true, false, false));
        assert!(takes_frames(true, true, false));
        assert!(takes_frames(true, false, true));
        assert!(takes_frames(false, true, true));
        assert!(!takes_frames(false, true, false));
        assert!(!takes_frames(false, false, true));
        assert!(!takes_frames(false, false, false));
    }

    #[test]
    fn a_resize_ends_a_chunked_kitty_upload_only_when_the_cut_is_inside_it() {
        let chunk = |control: &str, payload: &str| {
            let mut out = b"\x1b_G".to_vec();
            out.extend_from_slice(control.as_bytes());
            out.push(b';');
            out.extend_from_slice(payload.as_bytes());
            out.extend_from_slice(b"\x1b\\");
            out
        };
        let first = chunk("a=T,q=2,m=1", "AAAA");
        let last = chunk("q=2,m=0", "BBBB");
        let mut frame = first.clone();
        frame.extend_from_slice(&last);
        let after_m1 = frame
            .windows(3)
            .position(|window| window == b"m=1")
            .unwrap()
            + 3;
        assert!(!resize_discard_tail(&frame, 0), "nothing sent");
        assert!(
            !resize_discard_tail(&frame, frame.len()),
            "the m=0 chunk was sent"
        );
        assert!(!resize_discard_tail(&frame, after_m1 - 1));
        assert!(resize_discard_tail(&frame, after_m1));
        assert!(resize_discard_tail(&frame, first.len()), "between chunks");
        assert!(resize_discard_tail(&frame, frame.len() - 1));

        let mut prefixed = b"\x1b[H\x1b[2J".to_vec();
        let prefix = prefixed.len();
        prefixed.extend_from_slice(&frame);
        assert!(!resize_discard_tail(&prefixed, prefix), "chrome only");
        assert!(resize_discard_tail(&prefixed, prefix + after_m1));

        let mut with_cursor = frame.clone();
        let close = with_cursor.len();
        with_cursor.extend_from_slice(b"\x1b[u");
        assert!(!resize_discard_tail(&with_cursor, close));
        assert!(resize_discard_tail(&with_cursor, close - 1));

        let one = chunk("a=T,q=2,m=0", "AAAA");
        assert!(!resize_discard_tail(&one, 0));
        assert!(!resize_discard_tail(&one, one.len() / 2));
        assert!(!resize_discard_tail(&one, one.len()));
    }

    #[test]
    fn graphics_protocol_follows_the_terminal_and_the_setting() {
        use GraphicsProtocol::{Auto, Kitty, None as NoGraphics, Sixel};
        assert_eq!(choose_protocol(Auto, Some(true), true), Protocol::Kitty);
        assert_eq!(choose_protocol(Auto, Some(false), true), Protocol::Sixel);
        assert_eq!(choose_protocol(Auto, None, true), Protocol::Sixel);
        assert_eq!(choose_protocol(Auto, None, false), Protocol::None);
        assert_eq!(choose_protocol(Kitty, Some(true), true), Protocol::Kitty);
        assert_eq!(choose_protocol(Kitty, None, true), Protocol::None);
        assert_eq!(choose_protocol(Sixel, Some(true), true), Protocol::Sixel);
        assert_eq!(choose_protocol(Sixel, None, false), Protocol::None);
        assert_eq!(
            choose_protocol(NoGraphics, Some(true), true),
            Protocol::None
        );
    }
}
