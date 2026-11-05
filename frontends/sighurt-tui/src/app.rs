//! The terminal owner loop, built on calloop, and the [`Frontend`] the session drives.
//!
//! Its sources are the terminal (readable for input; writable only while bytes wait in the
//! bounded output), the `SIGWINCH` pipe (resize, and the wake after `SIGCONT`), one ping that
//! engine wakers and service threads share, the signal pipe from `sig` (SIGUSR1 diagnostics,
//! SIGTERM and SIGINT quit, and SIGHUP until the terminal's handler takes it over; that handler
//! only sets a flag and wakes the `SIGWINCH` pipe, and the loop quits the same way) and the
//! configuration watch.
//!
//! One iteration, after the loop wakes, as in the Wayland loop: terminal input (at most
//! [`MAX_EVENTS`] events a pass, each handed to the core), service completions, the layout's
//! metrics to the engines, engine pumps, then one write to the terminal: the chrome if the core
//! said it changed (or the terminal was resized or resumed), and at most one page frame. Passes
//! repeat until one does nothing, at most [`MAX_PASSES`] times; then the loop sleeps until an
//! event or a real deadline: an engine pump, the session save, a notice giving way, a partial
//! escape sequence, a clipboard read, a held pointer motion (at most one per
//! [`MOTION_INTERVAL`] reaches the core), or the next frame's time while a frame is waiting.
//! Nothing ticks.
//!
//! The clipboard is the terminal's OSC 52. One unsent write is kept, and a newer write
//! replaces it. A write over [`MAX_CLIPBOARD`], or whose sequence does not fit the output
//! cap, is refused before any of it is queued; a write that was queued is not dropped or
//! cut short by a later frame. One read at a time waits up to a second. A read that is
//! cancelled or times out is given up, and the next read is sent at once: a terminal that
//! ignores OSC 52 reads (tmux with `set-clipboard off` or no paste buffer, Alacritty) fails
//! each read after its second and never blocks the next. OSC 52 replies carry no id, so a
//! reply goes to the read waiting when it arrives, if that read named its selection, and is
//! dropped when none waits. A late reply (kitty answers after its permission prompt) is the
//! same clipboard, which the waiting read was allowed to have. A reply that names no
//! selection (tmux) answers the waiting read. A reply for the address line belongs to that
//! line and is dropped when the line closes or is replaced.
//!
//! The address line completes what is typed from the visited pages: after each pass the loop
//! asks the runtime's history ([`Session::complete`]) about the line's text, while the cursor is
//! at its end, nothing is selected, the text is not the address the line opened with and is not
//! a `:command`, and the line is not the find field; the rest of the first address it completes to shows in grey after the cursor,
//! and Right or Tab takes that address, exactly. Any change of the line, or the history
//! forgetting visits, asks again.
//!
//! Find in page (TUI.md §4): `page.find` without text opens the find field on the bottom line
//! (the address line's editor behind a `/`); each edit runs `page.find <text>`, an empty one
//! `page.find-stop`; Enter keeps the search and gives the page the keyboard, Escape ends it, and
//! the core moving the focus closes it and keeps the search. In text mode every search of the
//! shown view is the frontend's ([`Frontend::find`]): it searches the page's text and answers
//! the core with `BrowserEvent::FindResult`, as an engine would; in graphics mode the engine
//! searches. A search made before the page's first snapshot is answered when it comes. Changing
//! the mode ends every search, with one notice: the shown page's at once, a hidden tab's when it
//! is shown (`page.find-stop` acts on the tab shown), its moves going nowhere until then. What
//! the search found, from the core, is right-aligned on the bottom line.
//!
//! Text mode (TUI.md §4, [`crate::text`]): `[tui] mode`, or `chrome.toggle-mode` for the run,
//! decides whether the page rows show pixels or the page's text. In text mode the shown view's
//! engine is asked for snapshots ([`Session::set_semantics`]), the others' are not, and the
//! keys the keymap left for the page, and the mouse, act on the text: scrolling is the
//! frontend's, an activation goes to the core as `BrowserEvent::Semantic`. Text mode tells the
//! core `BrowserEvent::PageTyping(false)`, so a focused field does not swallow character
//! bindings; graphics tells it `true`. A press in the page in graphics mode captures the
//! pointer until that button is released. A paste goes to the address line while it is open,
//! to text mode's field editor while it is open, else to the page's text field only while the
//! core offers the host one (`Frontend::page_text_input`, which is `None` while the keymap gives
//! the page no text); text mode opens a field's editor only while the keymap gives the page
//! text, and shows a notice otherwise.
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
//! unsent. At most one image is written per frame interval, a frame sent again for a tab just
//! shown included, and a hidden or closed view's image is deleted or written over only once the
//! last image has been up for its interval and at least [`MIN_IMAGE_LIFE`] (Rio frees an
//! image's GPU texture at once, and lost its device when that came a moment after the upload);
//! the loop wakes when that time comes. Kitty then deletes it by id; a sixel, which has no id,
//! is written over: with the shown view's frame when it has one for this place and layout,
//! else with spaces. So a hidden tab's picture stays under the shown tab's title for at most
//! that long; a resize or a redraw clears the screen at once. A sixel frame for the place and
//! layout of the one already on the page rows is written over it with nothing in between, and
//! not at all when it is the same picture: tmux frees a stored image when a cell is written on
//! it and then redraws the whole pane, so blanking before every frame showed the page black for
//! a moment each time (decision 133). tmux keeps the sixels written over each other and sends
//! them all again when it redraws the pane, so inside tmux (`TERM`) a page that has been quiet
//! for [`SIXEL_QUIET`] with more than one stacked has its picture written again after a blank,
//! once, and tmux keeps that one; a page that keeps changing is never blanked.
//!
//! Logs go to `{state_dir}/sighurt/tui.log` before any engine starts. The terminal's modes are
//! turned off before the engines shut down (its images are deleted first, while the alternate
//! screen is up), and restored on every other way out: error, panic (the terminal's hook) and
//! [`Frontend::before_forced_exit`]; SIGTSTP suspends and SIGCONT redraws everything. SIGHUP
//! (the terminal closed, or a `kill -HUP`) quits like SIGTERM, saving the session and the
//! history; a hung-up terminal fails each write at once (EIO), so its restore cannot wait.
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
use sighurt_api::commands::{Command, MAX_FIND};
use sighurt_api::engine::{EngineCommand, EngineWaker, FindCommand, PresentOutcome};
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
use sighurt_core::tabs::{Content, Find, LoadError, TabSummary};
use sighurt_core::{
    BrowserEvent, BrowserState, ChromeChecks, Frontend as FrontendKind, OpenRequest,
};
use sighurt_runtime::diagnostics::{Diagnostics, FindReport, Report, TabReport, TargetReport};
use sighurt_runtime::frontend::Frontend;
use sighurt_runtime::history;
use sighurt_runtime::persistence::{self, ConfigWatch, Paths};
use sighurt_runtime::services::{Clipboard, Services, MAX_CLIPBOARD};
use sighurt_runtime::session::{Session, SHUTDOWN_BOUND};
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
    clipboard, ClipboardReply, MouseEvent, TermEvent, TermSize, Terminal, Wheel, PROBE_DEADLINE,
    SYNC_BEGIN, SYNC_END,
};
use crate::text::{Act, Count, Query, Step, TextPages};
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
/// The least time an image stays on the screen before a tab switch deletes it or writes over
/// it. Rio 0.5.28 frees an image's GPU texture at once, and lost its Vulkan device when that
/// came a moment after the upload (decision 133); a page's own frames, which replace each
/// other at `[tui] fps`, did not do that.
const MIN_IMAGE_LIFE: Duration = Duration::from_millis(250);
/// How long the page stays without a new sixel before, inside tmux, the sixels stacked there
/// are replaced by the one picture on the rows ([`App::quiet_stack`]). tmux keeps every sixel
/// written over another (at most 20 in the server) and sends them all again, oldest first,
/// when it redraws the pane (a window switched back to). No cap applies while the page keeps
/// changing: the blank it would take every few frames is a blink each time, and in Rio 0.5.28
/// spaces over a sixel it shows are what loses its Vulkan device (decision 133).
const SIXEL_QUIET: Duration = Duration::from_millis(400);
/// An empty last chunk. Ends a kitty image upload a resize cut after its `m=1`.
const KITTY_UPLOAD_END: &[u8] = b"\x1b_Gq=2,m=0;\x1b\\";
/// The least time between two pointer motions handed to the core. Terminals report motion
/// per cell; a motion inside the interval waits, and a newer one replaces it.
const MOTION_INTERVAL: Duration = Duration::from_millis(16);
/// The most pasted text the address line or a page's field takes, and the most text
/// `location.focus <text>` puts in the line.
const MAX_PASTE: usize = 1024 * 1024;

/// Cuts `text` to at most `max` bytes, at a character boundary.
fn cut(text: &mut String, max: usize) {
    if text.len() > max {
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}
/// How long an OSC 52 read waits for the terminal.
const CLIPBOARD_WAIT: Duration = Duration::from_secs(1);
/// What a read reports. The terminal's clipboard is text.
const CLIPBOARD_TYPE: &str = "text/plain;charset=utf-8";
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

/// One OSC 52 read waiting for the terminal. A page's is answered through the session; the
/// address line's is inserted when the reply comes, and only into the line that asked
/// ([`PendingRead::line_gen`]).
struct PendingRead {
    request: RequestId,
    /// When the read fails if the terminal has not answered.
    deadline: Instant,
    /// The address line asked, rather than a page.
    for_line: bool,
    /// [`Tui::line_gen`] when the line asked. A page read leaves this unused.
    line_gen: u64,
    /// The selection the query named. The reply has to name it too.
    selection: Selection,
    /// The most bytes a page accepts. The line also caps at [`MAX_PASTE`].
    limit: usize,
}

/// Whether a reply that named `named` answers a read of `asked`. A reply that names no
/// selection (tmux leaves the field empty) answers whatever was asked.
fn answers(named: Option<Selection>, asked: Selection) -> bool {
    named.is_none_or(|named| named == asked)
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
    /// The sixel on the page rows: where it was written, for which layout, and a fingerprint
    /// of its bytes. `None` once the rows were blanked, cleared or drawn as text.
    sixel_placed: Option<(CellRect, LayoutEpoch, u64)>,
    /// The sixel on the page rows is a view's that is no longer shown. The first write once
    /// [`Self::drop_due`] has passed puts the shown view's frame over it, or blanks the rows.
    sixel_stale: bool,
    /// Kitty images of views no longer shown, deleted once [`Self::drop_due`] has passed.
    held_deletes: Vec<u32>,
    /// The end of the last image's frame interval: the next image is not written before it.
    /// A frame sent again (a tab shown) counts.
    image_due: Option<Instant>,
    /// When the last image was written.
    image_written: Option<Instant>,
    /// `sig` runs inside tmux (`TERM`), which stores the sixels written over each other in the
    /// pane and sends them all again when it redraws the pane.
    sixel_stack: bool,
    /// The sixels written over each other since the page rows were last blanked or cleared:
    /// what tmux holds for them.
    sixel_depth: u32,
    /// The next sixel goes after a blank, so tmux keeps it alone ([`App::quiet_stack`]).
    consolidate: bool,
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
    /// The address line, while `location.focus` has it, or the find field.
    editing: Option<LineEditor>,
    /// [`Self::editing`] is the find field (`page.find` without text).
    finding: bool,
    /// The find field's text as last searched for, trimmed: an edit that leaves it searches
    /// nothing again.
    find_sent: String,
    /// Each view's search, as the core last started it ([`Frontend::find`] sees every one).
    searches: HashMap<ViewId, Query>,
    /// Hidden views whose search began before the mode last changed, so the finder of the
    /// mode now (the engine, or the page's text) never got it. Its moves go nowhere, and it
    /// ends (`page.find-stop`) when its view is shown again.
    orphaned: HashSet<ViewId>,
    /// Events for the core from outside an effect: what the find field typed, a search that
    /// found anew in a new snapshot, the stop a mode change makes. [`App::run_core`] sends them.
    find_events: Vec<BrowserEvent>,
    /// The view the core shows, as [`Frontend::show`] last said.
    shown_view: Option<ViewId>,
    /// Bumped when the address line opens or closes. A paste in flight names the line it
    /// asked, and a reply for another line is dropped.
    line_gen: u64,
    /// The address line opened during this run of the core and nothing was typed into it yet:
    /// it starts from the active tab's address as the run left it (`tab.new` opens it).
    editing_fresh: bool,
    /// The active tab's address when the core last ran, for the address line to start from.
    active_url: String,
    /// The text the history was last asked to complete, if the line could take one.
    completed_for: Option<String>,
    /// The visited address the line's text completes to, and that text.
    ghost: Option<(String, String)>,
    /// `Session::history_generation` when the history was last asked.
    history_seen: u64,
    notice: Option<String>,
    notice_first: Option<Instant>,
    /// The shown page has a text field: a paste goes there.
    page_text: bool,
    /// The earliest time the next page frame may be presented.
    next_frame: Option<Instant>,
    /// The OSC 52 read waiting for the terminal's answer, if there is one.
    outstanding: Option<PendingRead>,
    /// Reads that failed before a query was sent. The next pass answers them.
    failed_reads: Vec<(RequestId, ServiceError)>,
    /// One OSC 52 write not yet accepted by the terminal. A newer write replaces it.
    clipboard_out: Option<Vec<u8>>,
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
    /// The latest pointer motion not yet handled, held until `motion_due`.
    motion: Option<MouseEvent>,
    /// The earliest time the next pointer motion is handled: at most one per
    /// [`MOTION_INTERVAL`].
    motion_due: Option<Instant>,
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
        // A configuration file that can't be used stops sig before anything starts.
        let config = persistence::startup_config(paths.config.as_deref(), &env)?;
        let servo_wake = ping.clone();
        registry.start_at_launch(&config, || EngineWaker::new(move || servo_wake.ping()));
        // Servo's logger, if it started; otherwise env_logger. Either writes to the log file.
        sighurt_runtime::registry::install_logger();
        let writer_wake = ping.clone();
        let (session_writer, writer_notice) =
            persistence::session_writer(paths.session.as_deref(), move || writer_wake.ping());
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
            sixel_placed: None,
            sixel_stale: false,
            held_deletes: Vec::new(),
            image_due: None,
            image_written: None,
            sixel_stack: crate::term::inside_multiplexer(std::env::var_os("TERM").as_deref()),
            sixel_depth: 0,
            consolidate: false,
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
            finding: false,
            find_sent: String::new(),
            searches: HashMap::new(),
            orphaned: HashSet::new(),
            find_events: Vec::new(),
            shown_view: None,
            line_gen: 0,
            editing_fresh: false,
            active_url: String::new(),
            completed_for: None,
            ghost: None,
            history_seen: 0,
            notice: None,
            notice_first: None,
            page_text: false,
            next_frame: None,
            outstanding: None,
            failed_reads: Vec::new(),
            clipboard_out: None,
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
        if let Some(notice) = writer_notice {
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
            motion: None,
            motion_due: None,
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
        self.flush_find();
        self.session.run_core(&mut self.tui);
        // A mode the core changed (`m`) ended a search: the stop goes in this turn.
        while self.flush_find() {
            self.session.run_core(&mut self.tui);
        }
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

    /// Queues for the core what the find field typed and what [`Tui::find_events`] holds.
    /// Returns whether there was anything.
    fn flush_find(&mut self) -> bool {
        let mut events = std::mem::take(&mut self.tui.find_events);
        events.extend(self.tui.find_query().map(BrowserEvent::Command));
        let any = !events.is_empty();
        self.session.queue_mut().extend(events);
        any
    }

    /// One iteration of the loop, after it woke. Returns whether work is left that shouldn't
    /// wait.
    fn iterate(&mut self) -> bool {
        self.session.diagnostics_mut().counters.iterations += 1;
        if self.tui.terminal.take_resumed() {
            self.resumed();
        }
        if self.tui.terminal.take_hangup() {
            self.signals.push(libc::SIGHUP as u8);
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
        if let Some((request, result)) = self.tui.expire_clipboard(Instant::now()) {
            self.session.clipboard_done(request, result);
            self.run_core();
        }
        let mut busy = true;
        for _ in 0..MAX_PASSES {
            let mut worked = self.drain_input();
            worked |= self.deliver_clipboard_failures();
            worked |= self.session.service_completions(&mut self.tui);
            worked |= self.send_metrics();
            self.tui.active_url = active_url(self.session.core());
            worked |= self.session.pump(&mut self.tui);
            // After the pump, which may have opened a view: nothing is drawn for it before the
            // engine is asked for its text.
            worked |= self.sync_semantics();
            worked |= self.sync_completion();
            // A snapshot that found the search anew, a mode change that ended it.
            if !self.tui.find_events.is_empty() {
                self.run_core();
                worked = true;
            }
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
        self.session.write_due();
        if let Err(error) = self.tui.terminal.write_frame(&[]) {
            self.fail(format!("writing to the terminal failed: {error}"));
        }
        busy && !self.session.has_quit()
    }

    /// Asks the history what the address line's text completes to, when the text changed or the
    /// history forgot visits since it last asked (see the module's documentation). Returns
    /// whether the completion changed.
    fn sync_completion(&mut self) -> bool {
        let generation = self.session.history_generation();
        let forgot = std::mem::replace(&mut self.tui.history_seen, generation) != generation;
        let opened_with = self.tui.active_url.as_str();
        let editing = self.tui.editing.as_ref().filter(|_| !self.tui.finding);
        let wanted = editing.and_then(|editor| {
            let text = editor.text();
            let takes = editor.at_end()
                && !editor.selected()
                && !text.is_empty()
                && !text.trim_start().starts_with(':')
                && text != opened_with;
            takes.then(|| text.to_owned())
        });
        if wanted == self.tui.completed_for && !forgot {
            return false;
        }
        let ghost = wanted.as_ref().and_then(|text| {
            let found = self
                .session
                .complete(text)
                .into_iter()
                .map(|found| found.url);
            let mut found =
                found.filter(|url| history::suffix(text, url).is_some_and(|rest| !rest.is_empty()));
            Some((text.clone(), found.next()?))
        });
        self.tui.completed_for = wanted;
        let changed = ghost != self.tui.ghost;
        self.tui.ghost = ghost;
        self.tui.chrome_dirty |= changed;
        changed
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
        // A frame the engine already gave, or a hidden view's image, waits for the image
        // interval. Only a time still to come: one that has passed was handled in the pass.
        let image = match self.tui.holding() {
            true => self.tui.drop_due(),
            false => self.tui.image_due.filter(|_| self.image_waiting()),
        };
        let quiet = self.quiet_stack_at();
        let image = [image, quiet]
            .into_iter()
            .flatten()
            .min()
            .filter(|&at| at > now && self.tui.terminal.pending_len() == 0);
        let escape = self.tui.terminal.poll_timeout().map(|wait| now + wait);
        [
            pump,
            self.session.writes_due(),
            self.tui.notice_first,
            self.tui.clipboard_deadline(),
            self.motion_due.filter(|_| self.motion.is_some()),
            frame,
            image,
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

    /// The page rows show images and the shown view has a frame for this layout that the
    /// terminal does not show yet.
    fn image_waiting(&self) -> bool {
        let Some((_, target)) = self.shown() else {
            return false;
        };
        self.tui.graphics
            && self.tui.targets.get(&target).is_some_and(|pixels| {
                !pixels.on_screen
                    && pixels
                        .handle
                        .with_frame(self.tui.metrics.epoch(), |_| ())
                        .is_some()
            })
    }

    /// The shown view's frame goes over the hidden view's sixel in this write: it is waiting,
    /// its time has come, and it is for the place and layout of the sixel on the page rows.
    fn sixel_replaces_now(&self) -> bool {
        self.tui.protocol == Protocol::Sixel
            && self.tui.image_due.is_none_or(|at| at <= Instant::now())
            && self.tui.drop_due().is_none_or(|at| at <= Instant::now())
            && self.tui.sixel_placed.is_some_and(|(at, epoch, _)| {
                at == page_place(self.tui.page) && epoch == self.tui.metrics.epoch()
            })
            && self.image_waiting()
    }

    /// Inside tmux, when the sixels written over each other since the last blank may be
    /// replaced by the one on the rows: the shown view's picture is on the screen, more than
    /// one sixel is stacked, and none has come for [`SIXEL_QUIET`]. `None` when that does not
    /// apply.
    fn quiet_stack_at(&self) -> Option<Instant> {
        let tui = &self.tui;
        if !tui.sixel_stack
            || tui.protocol != Protocol::Sixel
            || !tui.graphics
            || tui.sixel_depth < 2
            || tui.consolidate
            || tui.holding()
        {
            return None;
        }
        let (_, target) = self.shown()?;
        let shown = tui
            .targets
            .get(&target)
            .is_some_and(|pixels| pixels.on_screen);
        let placed = tui.sixel_placed.is_some_and(|(at, epoch, _)| {
            at == page_place(tui.page) && epoch == tui.metrics.epoch()
        });
        let written = tui.image_written?;
        (shown && placed).then(|| written + SIXEL_QUIET.max(MIN_IMAGE_LIFE))
    }

    /// The page went quiet with sixels stacked in tmux: the shown view's frame is written
    /// again after a blank, in one write, so tmux frees the stack and keeps one image. The
    /// blank marks the pane for a redraw, and tmux holds a sixel back from the outer terminal
    /// while a redraw is pending, then draws the lines and the image in that redraw
    /// (`screen_write_set_client_cb`): one short blink once the page has stopped changing.
    fn quiet_stack(&mut self) {
        if self.quiet_stack_at().is_none_or(|at| at > Instant::now()) {
            return;
        }
        let Some((_, target)) = self.shown() else {
            return;
        };
        if let Some(pixels) = self.tui.targets.get_mut(&target) {
            pixels.on_screen = false;
            self.tui.consolidate = true;
        }
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
                // SIGHUP from `sig`'s pipe before the terminal's handler is installed, from
                // that handler's flag after.
                libc::SIGTERM | libc::SIGINT | libc::SIGHUP => {
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
        let now = Instant::now();
        let motion_due = self.motion.is_some() && self.motion_due.is_none_or(|due| due <= now);
        if motion_due {
            self.flush_motion(now);
            self.run_core();
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
        count > 0 || motion_due
    }

    /// Hands one terminal event on. Pointer motion is paced: one per [`MOTION_INTERVAL`], the
    /// latest kept meanwhile. Any other event first hands on the motion held before it, so the
    /// order is kept (a drag's last motion before its release).
    fn event(&mut self, event: TermEvent) {
        let now = Instant::now();
        if let TermEvent::Mouse(mouse) = &event {
            if mouse.motion && mouse.wheel.is_none() {
                if self.motion_due.is_some_and(|due| due > now) {
                    self.motion = Some(mouse.clone());
                    return;
                }
                self.motion = None;
                self.motion_due = Some(now + MOTION_INTERVAL);
                self.handle(event);
                return;
            }
        }
        self.flush_motion(now);
        self.handle(event);
    }

    /// Hands on the held pointer motion, if there is one, as of `now`.
    fn flush_motion(&mut self, now: Instant) {
        if let Some(mouse) = self.motion.take() {
            self.motion_due = Some(now + MOTION_INTERVAL);
            self.handle(TermEvent::Mouse(mouse));
        }
    }

    fn handle(&mut self, event: TermEvent) {
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
            TermEvent::Clipboard(data) => {
                if let Some((request, result)) = self.tui.clipboard_reply(data) {
                    self.session.clipboard_done(request, result);
                }
                return;
            }
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
        cut(&mut text, MAX_PASTE);
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
        let act = self.tui.text.click(
            view,
            mouse.column,
            mouse.row - page.row,
            self.session.core().page_takes_text(),
        )?;
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

    /// Answers clipboard reads that failed before a query was sent, and hands those answers to
    /// the core in this pass.
    fn deliver_clipboard_failures(&mut self) -> bool {
        let failed = std::mem::take(&mut self.tui.failed_reads);
        let worked = !failed.is_empty();
        for (request, error) in failed {
            self.session.clipboard_done(request, Err(error));
        }
        if worked {
            self.run_core();
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

    /// Writes what changed: a clipboard sequence on its own, then control bytes, the chrome,
    /// and at most one page frame. Nothing new goes out while bytes are still waiting, except
    /// a clipboard write replacing one that has not started. Returns whether it wrote.
    fn write(&mut self) -> bool {
        // What the terminal takes now of what was waiting.
        if let Err(error) = self.tui.terminal.write_frame(&[]) {
            self.fail(format!("writing to the terminal failed: {error}"));
        }
        if self.tui.terminal.pending_len() == 0 {
            self.tui.outbound = None;
        }
        let mut wrote = false;
        if let Some(osc) = self.tui.clipboard_out.take() {
            match self.tui.terminal.write_reliable(&osc) {
                Ok(true) => wrote = true,
                // The queue is busy with something else, or this does not fit. Leave it
                // unsent. An oversized sequence was refused before it was stored here.
                Ok(false) => self.tui.clipboard_out = Some(osc),
                Err(error) => {
                    self.tui.clipboard_out = Some(osc);
                    self.fail(format!("writing to the terminal failed: {error}"));
                }
            }
        }
        if self.tui.terminal.pending_len() > 0 {
            return wrote;
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
            self.tui.forget_sixel();
            self.tui.sixel_stale = false;
            // Deleted with all the others.
            self.tui.held_deletes.clear();
        }
        // A hidden view's image stays until the last image's frame interval is over: Rio frees
        // an image's GPU texture as soon as it is deleted or covered, and lost its device when
        // that came a moment after the upload (tabs switched at key repeat, decision 133).
        let due = self.tui.drop_due().is_none_or(|at| at <= Instant::now());
        if due {
            for image in std::mem::take(&mut self.tui.held_deletes) {
                out.extend(kitty::delete(image));
            }
            // The shown view's frame goes over a hidden view's sixel in this write if it can
            // (no blank: under tmux a blank is a black flash); otherwise the rows are blanked
            // now, before the chrome.
            if self.tui.sixel_stale && !self.sixel_replaces_now() {
                self.tui.sixel_stale = false;
                self.tui.blank_placed(&mut out);
            }
        }
        self.quiet_stack();
        if std::mem::take(&mut self.tui.chrome_dirty) {
            self.draw_chrome(&mut out);
        }
        self.frame(&mut out);
        // The frame did not go over it after all (it failed, or was not presented): the next
        // pass blanks the rows, and draws the chrome again for its cursor.
        if due && std::mem::take(&mut self.tui.sixel_stale) {
            let mut blank = std::mem::take(&mut self.tui.control);
            self.tui.blank_placed(&mut blank);
            self.tui.control = blank;
            self.tui.chrome_dirty = true;
            wrote = true;
        }
        if out.is_empty() {
            return wrote;
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
        if let Some(message) = &message {
            let message = crate::sanitize::sanitize(message);
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
        if message.is_some() || self.tui.text_mode {
            // The page rows hold text now, not the picture.
            self.tui.forget_sixel();
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
    /// caller. The top line draws only the active tab, so only its title, address and engine
    /// are copied, and only as many characters as the line could show (an address may be
    /// 2 MiB); the other tabs are counted.
    fn chrome_state(&self) -> ChromeState {
        let core = self.session.core();
        let active = core.active();
        let room = usize::from(self.tui.terminal.size().cols).saturating_mul(4);
        let tabs = core
            .tabs()
            .enumerate()
            .map(|(index, tab)| match index == active {
                true => TabLine {
                    engine: tab_engine(core, &tab).map(str::to_owned),
                    title: tab.title.chars().take(room).collect(),
                    // What it is loading until something commits: a new tab's first page.
                    url: tab
                        .url
                        .or(tab.requested)
                        .unwrap_or_default()
                        .chars()
                        .take(room)
                        .collect(),
                    loading: tab.loading,
                },
                false => TabLine {
                    title: String::new(),
                    url: String::new(),
                    loading: tab.loading,
                    engine: None,
                },
            })
            .collect();
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
        let find = core.tabs().nth(active).and_then(|tab| {
            let engine = tab_engine(core, &tab);
            tab.find
                .and_then(|find| find_indicator(find, engine, self.tui.finding))
        });
        ChromeState {
            tabs,
            active,
            address_editing: self.tui.editing.clone(),
            address_ghost: self.tui.ghost_suffix().map(str::to_owned),
            finding: self.tui.finding,
            find,
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
        // At most one image per frame interval, a frame sent again (a tab shown) included, and
        // a hidden view's image is not deleted or written over before [`Tui::drop_due`]: the
        // terminal uploads each image to its GPU, and Rio lost its Vulkan device while tabs
        // were switched at key repeat (decision 133). The loop wakes for it ([`App::timeout`]).
        // A blank inside tmux frees every stacked sixel: it waits as a drop does.
        let blanks_stack = self.tui.consolidate;
        let held =
            (self.tui.holding() || blanks_stack) && self.tui.drop_due().is_some_and(|at| at > now);
        let Some(pixels) = self.tui.targets.get_mut(&target) else {
            return;
        };
        // Text mode takes the frame and shows none of it.
        if pixels.on_screen || !self.tui.graphics {
            return;
        }
        if held || self.tui.image_due.is_some_and(|at| at > now) {
            return;
        }
        let page = self.tui.page;
        let place = page_place(page);
        // A sixel for the place and layout of the one on the page rows goes over it as it
        // is. Blanking first would make tmux free its stored image and redraw the pane, which
        // shows the rows black until the new image has arrived: a flicker on every frame.
        // tmux keeps the images written over each other (at most 20 in all, the oldest
        // freed) and draws them in order when it redraws the pane, the newest last.
        let placed = self.tui.sixel_placed;
        let same_place =
            placed.is_some_and(|(at, epoch, _)| at == place && epoch == metrics.epoch());
        // Inside tmux, once the page went quiet, its picture goes again after the blank: tmux
        // then keeps that one alone.
        let replaces = same_place && !blanks_stack;
        // The blank and its synchronized update sit outside the encoder, so the image
        // itself still has to fit in what remains of the output cap.
        let mut prefix = Vec::new();
        if protocol == Protocol::Sixel && !replaces {
            prefix.extend_from_slice(SYNC_BEGIN);
            blank_page(&mut prefix, page);
            prefix.extend_from_slice(SYNC_END);
        }
        let max = OUTPUT_CAP
            .saturating_sub(out.len())
            .saturating_sub(prefix.len());
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
                    let print = fingerprint(&bytes);
                    // The same picture is on the page rows already: nothing to send (unless
                    // it is sent again on purpose, to stand alone in tmux).
                    let same = same_place && placed.is_some_and(|(.., sent)| sent == print);
                    if !same || self.tui.consolidate {
                        // When the rows held another layout's image (or none), spaces first:
                        // they update tmux's grid, which drops what it stored there. The
                        // image comes after the synchronized update that holds them: tmux
                        // 3.8 writes a sixel through at once but holds a synchronized
                        // pane's lines until the update ends, and then redraws them over
                        // the image, which erases it in the outer terminal. The cursor
                        // save inside the sixel restores the position after the spaces,
                        // not the grid's cursor.
                        out.extend_from_slice(&prefix);
                        out.extend(bytes);
                        self.tui.grid.forget_cursor();
                        self.tui.grid.forget_pen();
                        self.tui.sixel_placed = Some((place, metrics.epoch(), print));
                        self.tui.image_due = Some(now + interval);
                        self.tui.image_written = Some(now);
                        self.tui.sixel_depth = match prefix.is_empty() {
                            true => self.tui.sixel_depth + 1,
                            false => 1,
                        };
                        self.tui.consolidate = false;
                    }
                    // A hidden view's sixel is under this one, or was this very picture.
                    self.tui.sixel_stale = false;
                } else {
                    out.extend(bytes);
                    self.tui.image_due = Some(now + interval);
                    self.tui.image_written = Some(now);
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
        // A page can keep Servo from ever shutting down; the terminal is already given back.
        self.session.set_shutdown_bound(SHUTDOWN_BOUND);
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
                // As in the terminal's enter and leave sequences (`term::modes`).
                true => b"\x1b[?1000h\x1b[?1002h\x1b[?1006h",
                false => b"\x1b[?1006l\x1b[?1002l\x1b[?1000l",
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
        self.image_due = self.image_due.map(|at| at.min(Instant::now() + interval));
    }

    /// The page rows hold no sixel: blanked, cleared or drawn as text, so tmux keeps none.
    fn forget_sixel(&mut self) {
        self.sixel_placed = None;
        self.sixel_depth = 0;
        self.consolidate = false;
    }

    /// A hidden view's image is waiting to be deleted or written over.
    fn holding(&self) -> bool {
        self.sixel_stale || !self.held_deletes.is_empty()
    }

    /// When a hidden view's image may be deleted or written over: the last image has been on
    /// the screen for its frame interval, and for at least [`MIN_IMAGE_LIFE`].
    fn drop_due(&self) -> Option<Instant> {
        let interval = frame_interval(self.settings, self.pace_sixel(), self.text_mode);
        self.image_written
            .map(|at| at + interval.max(MIN_IMAGE_LIFE))
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

    /// Drops the image a shown view left on the page, once the last image's frame interval is
    /// over. Kitty deletes by id. A sixel, which has no id, is written over: with the shown
    /// view's frame, or spaces.
    fn drop_placed(&mut self, image: u32) {
        match self.protocol {
            Protocol::Kitty => self.held_deletes.push(image),
            Protocol::Sixel => self.sixel_stale = true,
            Protocol::None => {}
        }
    }

    /// Writes spaces over the page rows into `out`, so tmux drops the sixel it is holding
    /// there and the terminal shows no picture.
    fn blank_placed(&mut self, out: &mut Vec<u8>) {
        blank_page(out, self.page);
        self.forget_sixel();
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
            // Every search was the other mode's finder's. The shown page's ends now; a hidden
            // one's when its tab is shown, since `page.find-stop` acts on the tab shown. Said
            // once.
            if text != self.text_mode && !self.searches.is_empty() {
                let shown = self.shown_view;
                let shown_search = shown.is_some_and(|view| self.searches.contains_key(&view));
                if shown_search {
                    self.find_events
                        .push(BrowserEvent::Command(Command::PageFindStop));
                }
                let hidden = self.searches.keys().filter(|&&view| Some(view) != shown);
                self.orphaned.extend(hidden);
                let now = if text { "as text" } else { "as graphics" };
                self.show_notice(format!(
                    "Searches in pages ended: pages are shown {now} now."
                ));
            }
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
        self.finding = false;
        self.line_gen = self.line_gen.wrapping_add(1);
        let mut editor = LineEditor::from_text(self.active_url.as_str());
        editor.select_all();
        self.editing = Some(editor);
        self.editing_fresh = true;
        self.chrome_dirty = true;
    }

    fn close_address(&mut self) {
        self.editing_fresh = false;
        self.finding = false;
        if self.editing.take().is_some() {
            self.line_gen = self.line_gen.wrapping_add(1);
            self.chrome_dirty = true;
        }
    }

    /// A key for the address line, as the window's address bar takes it.
    fn edit_key(&mut self, event: &KeyEvent, queue: &mut VecDeque<BrowserEvent>) {
        if self.editing.is_none() || event.state == ButtonState::Released {
            return;
        }
        if self.clipboard_keys(event) {
            return;
        }
        // Right or Tab at the end of the line takes the completion shown, exactly.
        let takes = matches!(&event.key, Key::Named(name) if name == "ArrowRight" || name == "Tab");
        let plain = !(event.modifiers.shift || event.modifiers.control || event.modifiers.alt);
        if takes && plain && self.ghost_suffix().is_some() {
            if let Some((_, url)) = self.ghost.take() {
                self.editing = Some(LineEditor::from_text(url));
                self.editing_fresh = false;
                self.chrome_dirty = true;
                return;
            }
        }
        let Some(editor) = self.editing.as_mut() else {
            return;
        };
        self.editing_fresh = false;
        // Control as a modifier, not as a key the layout used to make a character.
        let word = event.modifiers.control && !event.consumed.control;
        // Alt the same way: a shortcut the core didn't bind types nothing.
        let alt = event.modifiers.alt && !event.consumed.alt;
        match &event.key {
            Key::Named(name) => match name.as_str() {
                // The find field: Escape ends the search, Enter keeps it for the page.
                "Escape" | "Enter" if self.finding => {
                    let stop = name == "Escape";
                    self.close_address();
                    queue.push_back(BrowserEvent::Focus(KeyboardFocus::Page));
                    if stop {
                        queue.push_back(BrowserEvent::Command(Command::PageFindStop));
                    }
                    return;
                }
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

    /// The rest of the visited address the line completes to, while the line has the text it
    /// was found for, its cursor at the end and nothing selected.
    fn ghost_suffix(&self) -> Option<&str> {
        let editor = self.editing.as_ref().filter(|_| !self.finding)?;
        let (text, url) = self.ghost.as_ref()?;
        if editor.text() != text || !editor.at_end() || editor.selected() {
            return None;
        }
        history::suffix(text, url).filter(|rest| !rest.is_empty())
    }

    /// What the find field's text asks of the core since it last asked: `page.find <text>`, or
    /// `page.find-stop` for text that is empty or only whitespace. `None` when the field is
    /// closed or its text, trimmed, is what was last searched for.
    fn find_query(&mut self) -> Option<Command> {
        if !self.finding {
            return None;
        }
        let mut text = self.editing.as_ref()?.text().trim().to_owned();
        cut(&mut text, MAX_FIND);
        let text = text.trim_end().to_owned();
        if text == self.find_sent {
            return None;
        }
        self.find_sent.clone_from(&text);
        Some(match text.is_empty() {
            true => Command::PageFindStop,
            false => Command::PageFind(Some(text)),
        })
    }

    /// Ctrl+C copies the line, Ctrl+X copies it and clears it, Ctrl+V asks the terminal.
    /// Ctrl+A is not one of these: the editor selects the line itself. The terminal's own
    /// bracketed paste still arrives as [`TermEvent::Paste`].
    fn clipboard_keys(&mut self, event: &KeyEvent) -> bool {
        if !(event.modifiers.control && !event.consumed.control) {
            return false;
        }
        let Key::Character(letter) = &event.key else {
            return false;
        };
        match letter.as_str() {
            "c" | "C" => self.copy_line(false),
            "x" | "X" => self.copy_line(true),
            "v" | "V" => self.paste_line(),
            _ => return false,
        }
        true
    }

    /// OSC 52 of the whole line. The editor's selection is the whole line or nothing, so a copy
    /// with nothing selected is the line. `cut` clears it only when the copy was accepted.
    fn copy_line(&mut self, cut: bool) {
        let Some(text) = self.editing.as_ref().map(|editor| editor.text().to_owned()) else {
            return;
        };
        if self
            .queue_clipboard(Selection::Clipboard, text.as_bytes())
            .is_err()
        {
            return;
        }
        self.editing_fresh = false;
        if !cut {
            return;
        }
        if let Some(editor) = &mut self.editing {
            editor.select_all();
            editor.backspace();
        }
        self.chrome_dirty = true;
    }

    /// Ask the terminal for the clipboard and insert the answer into the line. A read that is
    /// already waiting is left alone: one query at a time.
    fn paste_line(&mut self) {
        self.editing_fresh = false;
        if self.outstanding.is_some() {
            return;
        }
        self.control.extend(clipboard::query(Selection::Clipboard));
        self.outstanding = Some(PendingRead {
            request: RequestId::next(),
            deadline: Instant::now() + CLIPBOARD_WAIT,
            for_line: true,
            line_gen: self.line_gen,
            selection: Selection::Clipboard,
            limit: MAX_PASTE,
        });
    }

    /// The terminal answered the waiting read, or one given up on (see the module's
    /// documentation): either way the reply is the terminal's clipboard now.
    ///
    /// A reply with nothing waiting is dropped. A reply for another selection leaves the
    /// waiting read waiting. A truncated reply is a failure, not a prefix. A page's answer goes
    /// back to the session. The line's is inserted only when `line_gen` is still the line that
    /// asked.
    fn clipboard_reply(
        &mut self,
        reply: ClipboardReply,
    ) -> Option<(RequestId, Result<ClipboardData, ServiceError>)> {
        let pending = self
            .outstanding
            .take_if(|pending| answers(reply.selection, pending.selection))?;
        if reply.truncated {
            if pending.for_line {
                return None;
            }
            return Some((
                pending.request,
                Err(ServiceError::Failed(
                    "the clipboard reply was cut short".into(),
                )),
            ));
        }
        if pending.for_line {
            if pending.line_gen == self.line_gen {
                self.insert_clipboard(reply.data, pending.limit);
            }
            None
        } else {
            Some((pending.request, clipboard_result(reply.data, pending.limit)))
        }
    }

    /// `now` is past the waiting read. A page's fails; the line's is dropped, with no notice
    /// (the open line would hide one). Either way the read is given up, and the next one may
    /// be sent at once.
    fn expire_clipboard(
        &mut self,
        now: Instant,
    ) -> Option<(RequestId, Result<ClipboardData, ServiceError>)> {
        let pending = self
            .outstanding
            .take_if(|pending| pending.deadline <= now)?;
        if pending.for_line {
            return None;
        }
        Some((
            pending.request,
            Err(ServiceError::Failed("the terminal didn't answer".into())),
        ))
    }

    /// When the waiting read must be finished. `None` when nothing is waiting.
    fn clipboard_deadline(&self) -> Option<Instant> {
        self.outstanding.as_ref().map(|pending| pending.deadline)
    }

    /// Queue one OSC 52 write, or refuse it before any byte is queued.
    ///
    /// Over [`MAX_CLIPBOARD`], or a sequence that does not fit the output cap, is
    /// [`ServiceError::TooLarge`]. A newer write replaces one not yet given to the terminal.
    fn queue_clipboard(&mut self, selection: Selection, data: &[u8]) -> Result<(), ServiceError> {
        if data.len() > MAX_CLIPBOARD || clipboard::set_len(data.len()) > self.terminal.output_cap()
        {
            return Err(ServiceError::TooLarge);
        }
        let bytes = clipboard::set(selection, data);
        debug_assert_eq!(bytes.len(), clipboard::set_len(data.len()));
        self.clipboard_out = Some(bytes);
        Ok(())
    }

    /// Insert a clipboard reply into the address line. Empty text is left out: inserting it
    /// would clear a selected line.
    fn insert_clipboard(&mut self, data: Option<Vec<u8>>, limit: usize) {
        let Some(bytes) = data else {
            return;
        };
        let owned = String::from_utf8_lossy(&bytes).into_owned();
        let mut end = limit.min(MAX_PASTE).min(owned.len());
        while end > 0 && !owned.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return;
        }
        let Some(editor) = &mut self.editing else {
            return;
        };
        editor.insert(&owned[..end]);
        self.editing_fresh = false;
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

/// A page's clipboard: text, or too much of it, or an empty clipboard when the terminal sent none.
fn clipboard_result(data: Option<Vec<u8>>, limit: usize) -> Result<ClipboardData, ServiceError> {
    match data {
        Some(bytes) if bytes.len() > limit => Err(ServiceError::TooLarge),
        Some(data) => Ok(ClipboardData {
            mime_type: CLIPBOARD_TYPE.into(),
            data,
        }),
        None => Ok(ClipboardData {
            mime_type: CLIPBOARD_TYPE.into(),
            data: Vec::new(),
        }),
    }
}

impl Clipboard for Tui {
    /// One OSC 52 query at a time. A second read fails at once while the first waits; one
    /// started after a cancel or a timeout is sent at once. A read that asks
    /// for no `text/*` type is unsupported. The answer comes later, as [`Tui::clipboard_reply`]
    /// or [`Tui::expire_clipboard`].
    fn read(
        &mut self,
        request: RequestId,
        selection: Selection,
        mime_types: &[String],
        limit: usize,
    ) {
        if !mime_types.iter().any(|mime| clipboard::is_text(mime)) {
            self.failed_reads.push((request, ServiceError::Unsupported));
            return;
        }
        if self.outstanding.is_some() {
            self.failed_reads.push((
                request,
                ServiceError::Failed("another clipboard read is waiting".into()),
            ));
            return;
        }
        self.control.extend(clipboard::query(selection));
        self.outstanding = Some(PendingRead {
            request,
            deadline: Instant::now() + CLIPBOARD_WAIT,
            for_line: false,
            line_gen: self.line_gen,
            selection,
            limit,
        });
    }

    /// The first `text/*` item, as one OSC 52. Anything else is unsupported. A payload over
    /// [`MAX_CLIPBOARD`], or whose sequence does not fit the output cap, is refused before it
    /// is queued.
    fn write(
        &mut self,
        selection: Selection,
        items: Vec<ClipboardData>,
    ) -> Result<(), ServiceError> {
        let Some(item) = items
            .into_iter()
            .find(|item| clipboard::is_text(&item.mime_type))
        else {
            return Err(ServiceError::Unsupported);
        };
        self.queue_clipboard(selection, &item.data)
    }

    /// A read that is still waiting for `request` is given up: nothing is reported for it,
    /// and the next read is sent at once. A failure not yet delivered is dropped too.
    fn cancel(&mut self, request: RequestId) {
        self.outstanding
            .take_if(|pending| pending.request == request);
        self.failed_reads.retain(|(id, _)| *id != request);
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
            self.searches.remove(&view);
            self.orphaned.remove(&view);
            self.shown_view.take_if(|shown| *shown == view);
            self.text.forget(view);
            self.semantic_on.remove(&view);
            self.capture.forget(view);
        }
        self.views.retain(|_, &mut kept| kept != target);
        if let Some(pixels) = self.targets.remove(&target) {
            if self.protocol == Protocol::Kitty || pixels.on_screen {
                self.drop_placed(pixels.image);
            }
        }
    }

    fn focus(&mut self, focus: KeyboardFocus, _queue: &mut VecDeque<BrowserEvent>) {
        match focus {
            KeyboardFocus::Chrome => self.open_address(),
            KeyboardFocus::Page => self.close_address(),
        }
    }

    fn line_text(&mut self, mut text: String, _queue: &mut VecDeque<BrowserEvent>) {
        if let Some(editor) = &mut self.editing {
            cut(&mut text, MAX_PASTE);
            editor.select_all();
            editor.insert(&text);
            self.editing_fresh = false;
            self.chrome_dirty = true;
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
        // An edit of the find field searches before the next key is handled.
        queue.extend(self.find_query().map(BrowserEvent::Command));
    }

    /// `page.find` without text: the address line, just opened, becomes the find field holding
    /// `text`, selected, so typing replaces it.
    fn find_field(&mut self, text: String, _queue: &mut VecDeque<BrowserEvent>) {
        let mut editor = LineEditor::from_text(text);
        editor.select_all();
        self.find_sent = editor.text().trim().to_owned();
        self.editing = Some(editor);
        self.finding = true;
        self.editing_fresh = false;
        self.chrome_dirty = true;
    }

    /// In text mode the shown page's searches are the frontend's: it searches the page's text
    /// and answers as an engine would. In graphics mode the engine searches. A stop also goes
    /// to the engine, which ignores one for a search it does not have.
    fn find(
        &mut self,
        view: ViewId,
        command: &FindCommand,
        queue: &mut VecDeque<BrowserEvent>,
    ) -> bool {
        let (search, step) = match command {
            FindCommand::Search {
                search,
                text,
                case_sensitive,
                ..
            } => {
                let query = Query {
                    search: *search,
                    text: text.clone(),
                    case_sensitive: *case_sensitive,
                };
                self.searches.insert(view, query);
                self.orphaned.remove(&view);
                (*search, Step::Start)
            }
            FindCommand::Next { search } => (*search, Step::Next),
            FindCommand::Previous { search } => (*search, Step::Previous),
            FindCommand::Stop { search } => {
                if self
                    .searches
                    .get(&view)
                    .is_some_and(|q| q.search == *search)
                {
                    self.searches.remove(&view);
                    self.orphaned.remove(&view);
                    self.text.stop_find(view);
                    self.chrome_dirty = true;
                }
                return false;
            }
        };
        // A search neither finder of this mode got: its moves go nowhere until it ends.
        if self.orphaned.contains(&view) {
            return true;
        }
        if !self.text_mode || !self.semantic_on.contains(&view) {
            return false;
        }
        // A move in a search that is not the view's does nothing, as in an engine. Before the
        // page's first snapshot there is nothing to search: the snapshot answers.
        let query = self.searches.get(&view).filter(|q| q.search == search);
        if let Some(query) = query.filter(|_| self.text.has(view)) {
            let count = self.text.find(view, query, step);
            queue.push_back(find_result(view, count));
            self.chrome_dirty = true;
        }
        true
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

    fn withdraw_notice(&mut self, message: &str) {
        if self.notice.as_deref() == Some(message) {
            self.notice = None;
            self.notice_first = None;
            self.chrome_dirty = true;
        }
    }

    fn redraw(&mut self) {
        self.chrome_dirty = true;
    }

    /// A shown view's last frame is sent again if it is still the layout's, or its next one
    /// is; a hidden view's image is deleted.
    fn show(&mut self, view: ViewId, visible: bool) {
        if visible {
            self.shown_view = Some(view);
            // Its search began in the other mode: it ends now that its tab is shown.
            if self.orphaned.remove(&view) {
                self.find_events
                    .push(BrowserEvent::Command(Command::PageFindStop));
            }
        } else {
            self.release_pointer(Some(view));
            self.shown_view.take_if(|shown| *shown == view);
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
        takes_text: bool,
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
        if let Some(act) = self.text.page_key(view, key, takes_text) {
            if let Some(at) = activation {
                queue.push_back(semantic_event(act, at));
            }
        }
        true
    }

    fn semantics_changed(&mut self, view: ViewId, snapshot: Box<SemanticSnapshot>) {
        if self.semantic_on.contains(&view) {
            // The same document found anew, or a search that waited for the page: the core
            // hears the new count.
            let mut count = self.text.snapshot(view, snapshot);
            if count.is_none() {
                let query = self
                    .searches
                    .get(&view)
                    .filter(|_| !self.orphaned.contains(&view));
                count = query.and_then(|query| self.text.resume(view, query));
            }
            if let Some(count) = count {
                self.find_events.push(find_result(view, count));
            }
            self.chrome_dirty = true;
        }
    }
}

/// Where a page image goes: all of the page rows.
fn page_place(page: PageArea) -> CellRect {
    CellRect {
        column: 0,
        row: page.row,
        columns: page.cols,
        rows: page.rows,
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

/// The core's event for what a search in a page's text found.
fn find_result(view: ViewId, count: Count) -> BrowserEvent {
    BrowserEvent::FindResult {
        view,
        search: count.search,
        found: count.total > 0,
        current: count.current,
        total: Some(count.total),
        supported: true,
    }
}

/// The most characters of a search's text the bottom line shows beside what it found.
const FIND_TEXT: usize = 16;

/// What the bottom line says of a search: "3 of 12", "12 matches", "no matches", or that the
/// page's engine can't find, after the search's text (cut) when the find field is closed.
/// Nothing while a search waits for its first result with the field open.
fn find_indicator(find: &Find, engine: Option<&str>, field_open: bool) -> Option<String> {
    let result = match (find.supported, find.found, find.current, find.total) {
        (false, ..) => Some(format!(
            "not supported by {}",
            engine.unwrap_or("this page")
        )),
        (true, None, ..) => None,
        (true, Some(false), ..) => Some("no matches".to_owned()),
        (true, Some(true), Some(current), Some(total)) => Some(format!("{current} of {total}")),
        (true, Some(true), _, Some(1)) => Some("1 match".to_owned()),
        (true, Some(true), _, Some(total)) => Some(format!("{total} matches")),
        (true, Some(true), ..) => Some("found".to_owned()),
    };
    if field_open {
        return result;
    }
    let mut text: String = find.text.chars().take(FIND_TEXT).collect();
    if find.text.chars().nth(FIND_TEXT).is_some() {
        text.push('…');
    }
    Some(match result {
        Some(result) => format!("/{text} {result}"),
        None => format!("/{text}"),
    })
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

/// A fingerprint of an encoded frame, to tell the picture on the page rows from a new one.
fn fingerprint(bytes: &[u8]) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
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
            find: tab.find.map(FindReport::from),
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
    // What the top line shows of the keymap's mode, as the window's report has it.
    let indicator = session.core().keymap_indicator().map(str::to_owned);
    session.diagnostics_mut().keymap_indicator = indicator;
    let report = Report {
        counters: session.diagnostics().counters.clone(),
        peaks: session.diagnostics().peaks.clone(),
        targets,
        engines: session.engine_reports(),
        renderer: None,
        tabs,
        active_tab,
        status,
        fullscreen: session.core().fullscreen().into(),
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
    use sighurt_api::input::{KeyCode, Modifiers};

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

    fn bare_tui(terminal: Terminal) -> Tui {
        let size = terminal.size();
        Tui {
            terminal,
            grid: Grid::new(size.cols, size.rows),
            seat: SeatId::next(),
            settings: TuiConfig::default(),
            graphics: false,
            protocol: Protocol::None,
            sixel_placed: None,
            sixel_stale: false,
            held_deletes: Vec::new(),
            image_due: None,
            image_written: None,
            sixel_stack: false,
            sixel_depth: 0,
            consolidate: false,
            page: page_area(size),
            text_mode: false,
            typing_told: None,
            mode_override: None,
            text: TextPages::new(size.cols, page_area(size).rows),
            semantic_on: HashSet::new(),
            capture: PointerCapture::default(),
            releases: Vec::new(),
            metrics: empty_metrics(),
            metrics_due: false,
            targets: HashMap::new(),
            views: HashMap::new(),
            next_image: 1,
            editing: None,
            finding: false,
            find_sent: String::new(),
            searches: HashMap::new(),
            orphaned: HashSet::new(),
            find_events: Vec::new(),
            shown_view: None,
            line_gen: 0,
            editing_fresh: false,
            active_url: String::new(),
            completed_for: None,
            ghost: None,
            history_seen: 0,
            notice: None,
            notice_first: None,
            page_text: false,
            next_frame: None,
            outstanding: None,
            failed_reads: Vec::new(),
            clipboard_out: None,
            control: Vec::new(),
            chrome_dirty: false,
            clear: false,
            cursor_shown: false,
            mouse_on: true,
            outbound: None,
            left: false,
        }
    }

    fn on_pty() -> (std::os::fd::OwnedFd, Tui) {
        let (master, slave) = crate::term::open_pty().expect("pty");
        rustix::termios::tcsetwinsize(
            &slave,
            rustix::termios::Winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .expect("winsize");
        let terminal = Terminal::enter(slave).expect("enter");
        (master, bare_tui(terminal))
    }

    fn ctrl(letter: &str) -> KeyEvent {
        KeyEvent {
            code: KeyCode(0),
            key: Key::Character(letter.into()),
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers {
                control: true,
                ..Modifiers::default()
            },
            consumed: Modifiers::default(),
            text: None,
        }
    }

    fn text_item(mime: &str, data: &[u8]) -> ClipboardData {
        ClipboardData {
            mime_type: mime.into(),
            data: data.to_vec(),
        }
    }

    fn clip_reply(data: Option<Vec<u8>>) -> ClipboardReply {
        ClipboardReply {
            selection: Some(Selection::Clipboard),
            data,
            truncated: false,
        }
    }

    #[test]
    fn write_sends_osc52_for_text_and_refuses_other_types() {
        let (_master, mut tui) = on_pty();
        let refused = tui.write(
            Selection::Clipboard,
            vec![text_item("image/png", &[1, 2, 3])],
        );
        assert_eq!(refused, Err(ServiceError::Unsupported));
        assert!(tui.control.is_empty());
        assert!(tui.clipboard_out.is_none());
        tui.write(
            Selection::Clipboard,
            vec![
                text_item("image/png", &[1]),
                text_item("text/plain", b"hej"),
            ],
        )
        .expect("text");
        assert!(tui.control.is_empty());
        assert_eq!(tui.terminal.pending_len(), 0);
        assert_eq!(
            tui.clipboard_out.as_deref(),
            Some(clipboard::set(Selection::Clipboard, b"hej").as_slice())
        );
    }

    #[test]
    fn an_oversized_clipboard_write_is_refused_before_it_is_queued() {
        let (_master, mut tui) = on_pty();
        let cap = tui.terminal.output_cap();
        let big = vec![b'x'; cap];
        assert!(clipboard::set_len(big.len()) > cap);
        let refused = tui.write(Selection::Clipboard, vec![text_item("text/plain", &big)]);
        assert_eq!(refused, Err(ServiceError::TooLarge));
        assert!(tui.clipboard_out.is_none());
        assert!(tui.control.is_empty());
        assert_eq!(tui.terminal.pending_len(), 0);

        let text = "x".repeat(cap);
        tui.editing = Some(LineEditor::from_text(&text));
        tui.chrome_dirty = false;
        let mut queue = VecDeque::new();
        tui.edit_key(&ctrl("x"), &mut queue);
        assert_eq!(tui.editing.as_ref().unwrap().text(), text);
        assert!(tui.clipboard_out.is_none());
        assert!(tui.control.is_empty());
        assert_eq!(tui.terminal.pending_len(), 0);
        assert!(!tui.chrome_dirty);
    }

    #[test]
    fn a_newer_clipboard_write_replaces_an_unsent_one() {
        let (_master, mut tui) = on_pty();
        tui.write(Selection::Clipboard, vec![text_item("text/plain", b"one")])
            .expect("one");
        tui.write(Selection::Clipboard, vec![text_item("text/plain", b"two")])
            .expect("two");
        assert_eq!(
            tui.clipboard_out.as_deref(),
            Some(clipboard::set(Selection::Clipboard, b"two").as_slice())
        );
        assert!(tui.control.is_empty());
        assert_eq!(tui.terminal.pending_len(), 0);
    }

    #[test]
    fn a_second_read_fails_while_one_waits() {
        let (_master, mut tui) = on_pty();
        let first = RequestId::next();
        let second = RequestId::next();
        tui.read(first, Selection::Clipboard, &["text/plain".into()], 1024);
        assert_eq!(tui.control, clipboard::query(Selection::Clipboard));
        tui.read(second, Selection::Clipboard, &["text/plain".into()], 1024);
        assert_eq!(
            tui.failed_reads,
            vec![(
                second,
                ServiceError::Failed("another clipboard read is waiting".into())
            )]
        );
        assert_eq!(tui.control, clipboard::query(Selection::Clipboard));
        let pending = tui
            .outstanding
            .as_ref()
            .expect("the first read still waits");
        assert_eq!(pending.request, first);
        assert!(!pending.for_line);
        assert_eq!(pending.selection, Selection::Clipboard);
        let left = pending.deadline.saturating_duration_since(Instant::now());
        assert!(left > Duration::from_millis(500) && left <= CLIPBOARD_WAIT);
        let image = RequestId::next();
        tui.read(image, Selection::Clipboard, &["image/png".into()], 8);
        assert_eq!(tui.failed_reads[1], (image, ServiceError::Unsupported));
        assert!(matches!(
            &tui.outstanding,
            Some(read) if read.request == first
        ));
        // A reply for another selection does not satisfy this read.
        assert!(tui
            .clipboard_reply(ClipboardReply {
                selection: Some(Selection::Primary),
                data: Some(b"no".to_vec()),
                truncated: false,
            })
            .is_none());
        assert!(matches!(
            &tui.outstanding,
            Some(read) if read.request == first
        ));
    }

    #[test]
    fn a_read_past_its_deadline_is_answered_failed() {
        let (_master, mut tui) = on_pty();
        let request = RequestId::next();
        tui.read(request, Selection::Clipboard, &["text/plain".into()], 2);
        assert!(tui.expire_clipboard(Instant::now()).is_none());
        let (id, result) = tui
            .expire_clipboard(Instant::now() + Duration::from_secs(2))
            .expect("due");
        assert_eq!(id, request);
        assert_eq!(
            result,
            Err(ServiceError::Failed("the terminal didn't answer".into()))
        );
        assert!(tui.outstanding.is_none());
        assert!(tui
            .expire_clipboard(Instant::now() + Duration::from_secs(2))
            .is_none());
        // A late reply with nothing waiting is dropped.
        assert!(tui
            .clipboard_reply(clip_reply(Some(b"late".to_vec())))
            .is_none());

        // The next read is sent at once, and takes the next reply.
        let next = RequestId::next();
        tui.control.clear();
        tui.read(next, Selection::Clipboard, &["text/plain".into()], 2);
        assert!(tui.failed_reads.is_empty());
        assert_eq!(tui.control, clipboard::query(Selection::Clipboard));
        let (id, result) = tui
            .clipboard_reply(clip_reply(Some(b"hej".to_vec())))
            .expect("over the limit");
        assert_eq!(id, next);
        assert_eq!(result, Err(ServiceError::TooLarge));
        tui.read(
            RequestId::next(),
            Selection::Clipboard,
            &["text/plain".into()],
            8,
        );
        let (_, result) = tui.clipboard_reply(clip_reply(None)).expect("empty reply");
        assert_eq!(
            result,
            Ok(ClipboardData {
                mime_type: CLIPBOARD_TYPE.into(),
                data: Vec::new(),
            })
        );
        let waiting = RequestId::next();
        tui.read(waiting, Selection::Clipboard, &["text/plain".into()], 8);
        tui.cancel(waiting);
        assert!(tui.outstanding.is_none());
        assert!(tui
            .clipboard_reply(clip_reply(Some(b"hej".to_vec())))
            .is_none());
    }

    /// A terminal that ignores OSC 52 reads (tmux with `set-clipboard off` or no paste
    /// buffer, Alacritty) fails each read after its wait, and never blocks the next one.
    #[test]
    fn a_terminal_that_never_answers_fails_each_read_and_keeps_asking() {
        let (_master, mut tui) = on_pty();
        let mut at = Instant::now();
        for _ in 0..12 {
            let request = RequestId::next();
            tui.control.clear();
            tui.read(request, Selection::Clipboard, &["text/plain".into()], 64);
            assert!(tui.failed_reads.is_empty());
            assert_eq!(tui.control, clipboard::query(Selection::Clipboard));
            at += Duration::from_secs(2);
            let (id, result) = tui.expire_clipboard(at).expect("due");
            assert_eq!(id, request);
            assert_eq!(
                result,
                Err(ServiceError::Failed("the terminal didn't answer".into()))
            );
        }
        // The terminal starts answering (tmux has a paste buffer now): the read that asked
        // gets it.
        let request = RequestId::next();
        tui.read(request, Selection::Clipboard, &["text/plain".into()], 64);
        let (id, result) = tui
            .clipboard_reply(clip_reply(Some(b"ok".to_vec())))
            .expect("answered");
        assert_eq!(id, request);
        assert_eq!(
            result,
            Ok(ClipboardData {
                mime_type: CLIPBOARD_TYPE.into(),
                data: b"ok".to_vec(),
            })
        );
    }

    /// tmux answers with an empty selection field, which names no selection: it answers the
    /// read that is waiting.
    #[test]
    fn a_reply_naming_no_selection_answers_the_read() {
        let (_master, mut tui) = on_pty();
        let request = RequestId::next();
        tui.read(request, Selection::Clipboard, &["text/plain".into()], 64);
        let (id, result) = tui
            .clipboard_reply(ClipboardReply {
                selection: None,
                data: Some(b"tmux".to_vec()),
                truncated: false,
            })
            .expect("answered");
        assert_eq!(id, request);
        assert_eq!(
            result,
            Ok(ClipboardData {
                mime_type: CLIPBOARD_TYPE.into(),
                data: b"tmux".to_vec(),
            })
        );
        assert!(tui.outstanding.is_none());
    }

    /// A reply with no id: after a cancel, the next read is sent at once and takes the next
    /// reply of its selection, which is the terminal's clipboard whichever query it answers.
    #[test]
    fn after_a_cancel_the_next_read_is_sent_and_answered() {
        let (_master, mut tui) = on_pty();
        let first = RequestId::next();
        tui.read(first, Selection::Clipboard, &["text/plain".into()], 64);
        tui.cancel(first);
        assert!(tui.outstanding.is_none());
        let second = RequestId::next();
        tui.control.clear();
        tui.read(second, Selection::Clipboard, &["text/plain".into()], 64);
        assert!(tui.failed_reads.is_empty());
        assert_eq!(tui.control, clipboard::query(Selection::Clipboard));
        // Another selection's reply answers neither.
        assert!(tui
            .clipboard_reply(ClipboardReply {
                selection: Some(Selection::Primary),
                data: Some(b"no".to_vec()),
                truncated: false,
            })
            .is_none());
        assert!(matches!(&tui.outstanding, Some(read) if read.request == second));
        let (id, result) = tui
            .clipboard_reply(clip_reply(Some(b"ok".to_vec())))
            .expect("the new read");
        assert_eq!(id, second);
        assert_eq!(
            result,
            Ok(ClipboardData {
                mime_type: CLIPBOARD_TYPE.into(),
                data: b"ok".to_vec(),
            })
        );
    }

    #[test]
    fn a_truncated_reply_fails_the_read() {
        let (_master, mut tui) = on_pty();
        let request = RequestId::next();
        tui.read(request, Selection::Clipboard, &["text/plain".into()], 1024);
        let (id, result) = tui
            .clipboard_reply(ClipboardReply {
                selection: Some(Selection::Clipboard),
                data: Some(vec![b'A'; 8]),
                truncated: true,
            })
            .expect("the read fails");
        assert_eq!(id, request);
        assert_eq!(
            result,
            Err(ServiceError::Failed(
                "the clipboard reply was cut short".into()
            ))
        );
        assert!(tui.outstanding.is_none());

        tui.editing = Some(LineEditor::from_text("ab"));
        let mut queue = VecDeque::new();
        tui.edit_key(&ctrl("v"), &mut queue);
        assert!(tui
            .clipboard_reply(ClipboardReply {
                selection: Some(Selection::Clipboard),
                data: Some(b"xy".to_vec()),
                truncated: true,
            })
            .is_none());
        assert_eq!(tui.editing.as_ref().unwrap().text(), "ab");
        assert!(tui.outstanding.is_none());
    }

    #[test]
    fn the_line_copies_the_whole_line_and_cut_clears_it() {
        let (_master, mut tui) = on_pty();
        let line = "https://example.test/a";
        tui.editing = Some(LineEditor::from_text(line));
        let mut queue = VecDeque::new();
        let mut release = ctrl("c");
        release.state = ButtonState::Released;
        tui.edit_key(&release, &mut queue);
        assert!(tui.control.is_empty());
        assert!(tui.clipboard_out.is_none());
        tui.edit_key(&ctrl("c"), &mut queue);
        assert_eq!(tui.editing.as_ref().unwrap().text(), line);
        assert!(!tui.editing.as_ref().unwrap().selected());
        assert!(tui.control.is_empty());
        assert_eq!(
            tui.clipboard_out.as_deref(),
            Some(clipboard::set(Selection::Clipboard, line.as_bytes()).as_slice())
        );
        assert!(!tui.editing_fresh);
        tui.clipboard_out = None;
        tui.edit_key(&ctrl("a"), &mut queue);
        assert!(tui.editing.as_ref().unwrap().selected());
        assert!(tui.control.is_empty());
        assert!(tui.clipboard_out.is_none());
        tui.edit_key(&ctrl("x"), &mut queue);
        assert_eq!(tui.editing.as_ref().unwrap().text(), "");
        assert!(tui.control.is_empty());
        assert_eq!(
            tui.clipboard_out.as_deref(),
            Some(clipboard::set(Selection::Clipboard, line.as_bytes()).as_slice())
        );
        assert!(tui.chrome_dirty);
    }

    #[test]
    fn location_focus_with_text_types_it_over_the_address() {
        let (_master, mut tui) = on_pty();
        let mut queue = VecDeque::new();
        // No line open: nothing to fill.
        tui.line_text(":".into(), &mut queue);
        assert!(tui.editing.is_none());
        tui.active_url = "https://example.test/a".into();
        tui.focus(KeyboardFocus::Chrome, &mut queue);
        tui.chrome_dirty = false;
        tui.line_text("/".into(), &mut queue);
        let editor = tui.editing.as_ref().unwrap();
        assert_eq!((editor.text(), editor.selected()), ("/", false));
        assert_eq!(editor.before_cursor(), "/");
        // Typed, so the address of a page the run opened does not replace it.
        assert!(!tui.editing_fresh);
        assert!(tui.chrome_dirty);
        assert!(queue.is_empty());
    }

    #[test]
    fn the_line_pastes_the_terminals_reply() {
        let (_master, mut tui) = on_pty();
        tui.editing = Some(LineEditor::from_text("ab"));
        let mut queue = VecDeque::new();
        tui.edit_key(&ctrl("v"), &mut queue);
        let pending = tui.outstanding.as_ref().expect("waiting");
        assert!(pending.for_line);
        assert_eq!(pending.line_gen, tui.line_gen);
        assert_eq!(tui.control, clipboard::query(Selection::Clipboard));
        let again = tui.control.clone();
        tui.edit_key(&ctrl("v"), &mut queue);
        assert_eq!(tui.control, again);
        assert!(tui
            .clipboard_reply(clip_reply(Some(b"xy".to_vec())))
            .is_none());
        assert_eq!(tui.editing.as_ref().unwrap().text(), "abxy");
        let mut selected = LineEditor::from_text("ab");
        selected.select_all();
        tui.editing = Some(selected);
        tui.edit_key(&ctrl("v"), &mut queue);
        assert!(tui.clipboard_reply(clip_reply(None)).is_none());
        assert_eq!(tui.editing.as_ref().unwrap().text(), "ab");
        assert!(tui.editing.as_ref().unwrap().selected());
        let due = Instant::now() + Duration::from_secs(2);
        tui.edit_key(&ctrl("v"), &mut queue);
        assert!(tui.expire_clipboard(due).is_none());
        assert!(tui.outstanding.is_none());
        // A paste after the timeout asks again, and the reply is pasted.
        tui.control.clear();
        tui.edit_key(&ctrl("v"), &mut queue);
        assert_eq!(tui.control, clipboard::query(Selection::Clipboard));
        assert!(tui
            .clipboard_reply(clip_reply(Some(b"yes".to_vec())))
            .is_none());
        assert!(tui.outstanding.is_none());
        // The line was selected, so the paste replaces it. A second reply has no read.
        assert_eq!(tui.editing.as_ref().unwrap().text(), "yes");
        assert!(tui
            .clipboard_reply(clip_reply(Some(b"nope".to_vec())))
            .is_none());
        assert_eq!(tui.editing.as_ref().unwrap().text(), "yes");
    }

    #[test]
    fn a_line_reply_is_dropped_when_the_line_closes() {
        let (_master, mut tui) = on_pty();
        tui.active_url = "https://one.test".into();
        tui.open_address();
        let asked = tui.line_gen;
        let mut queue = VecDeque::new();
        tui.edit_key(&ctrl("v"), &mut queue);
        assert!(matches!(
            &tui.outstanding,
            Some(pending) if pending.for_line && pending.line_gen == asked
        ));
        tui.close_address();
        tui.active_url = "https://two.test".into();
        tui.open_address();
        assert_ne!(tui.line_gen, asked);
        assert!(tui
            .clipboard_reply(clip_reply(Some(b"secret".to_vec())))
            .is_none());
        assert_eq!(tui.editing.as_ref().unwrap().text(), "https://two.test");
        assert!(tui.editing.as_ref().unwrap().selected());
        assert!(tui.outstanding.is_none());
    }

    #[cfg(feature = "engine-demo")]
    const DEMO_CONFIG: &str = "\
version = 1
[engines]
default = \"demo\"
[routing.demo]
schemes = [\"demo\"]
";

    /// The pty master stays open for as long as the app does. Closing it would drop the terminal.
    #[cfg(feature = "engine-demo")]
    struct DemoApp {
        _master: std::os::fd::OwnedFd,
        app: App,
    }

    #[cfg(feature = "engine-demo")]
    fn demo_app() -> DemoApp {
        demo_app_with(Paths {
            config: None,
            session: None,
        })
    }

    #[cfg(feature = "engine-demo")]
    fn demo_app_with(paths: Paths) -> DemoApp {
        let (master, slave) = crate::term::open_pty().expect("pty");
        rustix::termios::tcsetwinsize(
            &slave,
            rustix::termios::Winsize {
                ws_row: 24,
                ws_col: 80,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .expect("winsize");
        let terminal = Terminal::enter(slave).expect("enter");
        let registry = crate::engines::build();
        let mut env = registry.environment(chrome_checks(), COLOR_NAMES);
        env.frontend = FrontendKind::Terminal;
        let config = sighurt_core::config::Config::parse(Some(DEMO_CONFIG), &env)
            .unwrap_or_else(|error| panic!("config: {error}"));
        let session = Session::new(
            BrowserState::new(config, env.clone()),
            env,
            registry,
            Services::new(|| {}),
            paths,
            None,
            Diagnostics::new(None),
            std::sync::Arc::new(|| {}),
        );
        DemoApp {
            _master: master,
            app: App {
                tui: bare_tui(terminal),
                session,
                input: VecDeque::new(),
                readable: false,
                config_changed: false,
                signals: Vec::new(),
                writable: None,
                motion: None,
                motion_due: None,
            },
        }
    }

    #[cfg(feature = "engine-demo")]
    fn shown_document(app: &App) -> Option<sighurt_api::ids::DocumentGeneration> {
        let view = app.session.shown_view().map(|(view, _)| view)?;
        app.session.core().view(view)?.document()
    }

    #[cfg(feature = "engine-demo")]
    fn settle_document(app: &mut App) -> sighurt_api::ids::DocumentGeneration {
        for _ in 0..8 {
            app.run_core();
            let _ = app.session.pump(&mut app.tui);
            if let Some(document) = shown_document(app) {
                return document;
            }
        }
        panic!("demo://pattern did not commit: {}", page_debug(app));
    }

    #[cfg(feature = "engine-demo")]
    fn page_debug(app: &mut App) -> String {
        let queued = app
            .session
            .queue_mut()
            .iter()
            .map(|event| format!("{event:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        let core = app.session.core();
        let tab = core.tabs().nth(core.active());
        format!(
            "tabs={} url={:?} error={:?} views={} queue={queued}",
            core.tabs().len(),
            tab.as_ref().and_then(|tab| tab.url),
            tab.as_ref().and_then(|tab| tab.error),
            core.views().count(),
        )
    }

    #[cfg(feature = "engine-demo")]
    fn press_page(app: &mut App, letter: &str) {
        let seat = app.tui.seat;
        app.session.queue_mut().push_back(BrowserEvent::Keyboard {
            event: InputEvent {
                seat,
                kind: InputKind::Key(KeyEvent {
                    code: KeyCode(0),
                    key: Key::Character(letter.into()),
                    state: ButtonState::Pressed,
                    repeat: false,
                    modifiers: Modifiers::default(),
                    consumed: Modifiers::default(),
                    text: Some(letter.into()),
                }),
            },
            at: Instant::now(),
        });
        app.run_core();
    }

    #[cfg(feature = "engine-demo")]
    fn request_clipboard(
        app: &mut App,
        document: sighurt_api::ids::DocumentGeneration,
    ) -> RequestId {
        let view = app.session.shown_view().expect("shown").0;
        let request = RequestId::next();
        app.session.queue_mut().push_back(BrowserEvent::Engine {
            engine: "demo".into(),
            event: sighurt_api::events::EngineEvent::ServiceRequested(
                sighurt_api::services::ServiceRequest {
                    id: request,
                    view,
                    document,
                    origin: Some("demo://pattern".into()),
                    service: sighurt_api::services::Service::ClipboardRead {
                        selection: Selection::Clipboard,
                        mime_types: vec!["text/plain".into()],
                        limit: 1024,
                    },
                },
            ),
            at: Instant::now(),
        });
        app.tui.control.clear();
        app.run_core();
        request
    }

    #[cfg(feature = "engine-demo")]
    #[test]
    fn a_page_read_reaches_clipboard_done() {
        let mut held = demo_app();
        let app = &mut held.app;
        app.session.queue_mut().push_back(BrowserEvent::Start {
            session: None,
            open: vec![OpenRequest {
                input: "demo://pattern".into(),
                engine: Some("demo".into()),
            }],
        });
        let document = settle_document(app);
        // A key the user pressed in the page: `j`, which NORMAL sends the page as a key of its
        // own (an unbound key would be taken, and activate nothing).
        press_page(app, "j");
        let request = request_clipboard(app, document);
        let control = app.tui.control.clone();
        let query = clipboard::query(Selection::Clipboard);
        if control != query {
            panic!(
                "the read never asked the terminal: {} bytes={control:?}",
                page_debug(app)
            );
        }
        let (id, result) = app
            .tui
            .expire_clipboard(Instant::now() + Duration::from_secs(2))
            .expect("the read is due");
        assert_eq!(id, request);
        app.session.clipboard_done(id, result);
        let failed = clipboard_answer(app, request);
        assert_eq!(
            failed,
            Some(Err(ServiceError::Failed(
                "the terminal didn't answer".into()
            )))
        );
        app.run_core();
        press_page(app, "j");
        // The timed-out read is given up, so the page may ask again at once.
        let request = request_clipboard(app, document);
        assert_eq!(app.tui.control, clipboard::query(Selection::Clipboard));
        app.event(TermEvent::Clipboard(clip_reply(Some(b"hej".to_vec()))));
        assert_eq!(
            clipboard_answer(app, request),
            Some(Ok(sighurt_api::services::ServiceReply::Clipboard(
                ClipboardData {
                    mime_type: CLIPBOARD_TYPE.into(),
                    data: b"hej".to_vec(),
                }
            )))
        );
    }

    /// One pointer motion per [`MOTION_INTERVAL`]: the latest waits, and goes before the next
    /// event of another kind, or once its time has come.
    #[cfg(feature = "engine-demo")]
    #[test]
    fn pointer_motion_is_paced_and_keeps_its_order() {
        let mut held = demo_app();
        let app = &mut held.app;
        let mouse = |column, motion, state| MouseEvent {
            column,
            row: 5,
            button: Some(PointerButton::Left),
            state,
            modifiers: Modifiers::default(),
            motion,
            wheel: None,
        };
        app.event(TermEvent::Mouse(mouse(1, false, ButtonState::Pressed)));
        assert!(app.motion.is_none() && app.motion_due.is_none());
        app.event(TermEvent::Mouse(mouse(1, true, ButtonState::Pressed)));
        assert!(app.motion.is_none());
        let due = app.motion_due.expect("paced");
        assert!(due <= Instant::now() + MOTION_INTERVAL);
        // Far enough ahead that a slow machine does not reach it during the test.
        let due = Instant::now() + Duration::from_secs(60);
        app.motion_due = Some(due);
        app.event(TermEvent::Mouse(mouse(2, true, ButtonState::Pressed)));
        app.event(TermEvent::Mouse(mouse(3, true, ButtonState::Pressed)));
        assert_eq!(app.motion, Some(mouse(3, true, ButtonState::Pressed)));
        assert_eq!(app.motion_due, Some(due));
        // The release hands the held motion on first.
        app.event(TermEvent::Mouse(mouse(3, false, ButtonState::Released)));
        assert!(app.motion.is_none());
        // A held motion goes once its time has come, with no other event.
        app.motion_due = Some(due);
        app.event(TermEvent::Mouse(mouse(4, true, ButtonState::Pressed)));
        assert!(app.motion.is_some());
        app.motion_due = Some(Instant::now());
        assert!(app.drain_input());
        assert!(app.motion.is_none());
    }

    #[cfg(feature = "engine-demo")]
    fn clipboard_answer(
        app: &mut App,
        request: RequestId,
    ) -> Option<Result<sighurt_api::services::ServiceReply, ServiceError>> {
        app.session
            .queue_mut()
            .iter()
            .find_map(|event| match event {
                BrowserEvent::ServiceDone {
                    request: id,
                    result,
                } if *id == request => Some(result.clone()),
                _ => None,
            })
    }

    fn typed(text: &str) -> KeyEvent {
        KeyEvent {
            code: KeyCode(0),
            key: Key::Character(text.into()),
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers::default(),
            consumed: Modifiers::default(),
            text: Some(text.into()),
        }
    }

    fn named(name: &str) -> KeyEvent {
        KeyEvent {
            key: Key::Named(name.into()),
            text: None,
            ..typed("x")
        }
    }

    fn commands(queue: &VecDeque<BrowserEvent>) -> Vec<String> {
        queue
            .iter()
            .filter_map(|event| match event {
                BrowserEvent::Command(command) => Some(command.to_string()),
                _ => None,
            })
            .collect()
    }

    /// What is typed in the address line completes from the history: the rest of the address
    /// shows while the cursor is at the end, Tab takes the exact address, and neither the
    /// address the line opened with, a command line nor the find field completes.
    #[cfg(feature = "engine-demo")]
    #[test]
    fn the_address_line_completes_from_the_history() {
        const URL: &str = "https://www.example.com/page";
        let dir = std::env::temp_dir().join(format!("sighurt-tui-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let seeded = format!(
            "version = 1\n[[visits]]\nurl = \"{URL}\"\ntitle = \"Example\"\nlast = {now}\ncount = 2\n"
        );
        std::fs::write(dir.join("history.toml"), seeded).unwrap();
        let mut demo = demo_app_with(Paths {
            config: None,
            session: Some(dir.join("session.toml")),
        });
        let app = &mut demo.app;
        let mut queue = VecDeque::new();
        let key = |app: &mut App, key: KeyEvent, queue: &mut VecDeque<BrowserEvent>| {
            let event = InputEvent {
                seat: app.tui.seat,
                kind: InputKind::Key(key),
            };
            app.tui.chrome_input(event, queue);
            app.sync_completion();
        };
        app.tui.focus(KeyboardFocus::Chrome, &mut queue);
        assert!(
            !app.sync_completion(),
            "the address it opened with is not typed"
        );
        for letter in ["e", "x", "a"] {
            key(app, typed(letter), &mut queue);
        }
        assert_eq!(app.tui.ghost_suffix(), Some("mple.com/page"));
        key(app, named("ArrowLeft"), &mut queue);
        assert_eq!(app.tui.ghost_suffix(), None, "the cursor is off the end");
        key(app, named("ArrowRight"), &mut queue);
        assert_eq!(
            app.tui.editing.as_ref().unwrap().text(),
            "exa",
            "Right only moved"
        );
        assert_eq!(app.tui.ghost_suffix(), Some("mple.com/page"));
        key(app, named("Tab"), &mut queue);
        assert_eq!(app.tui.editing.as_ref().unwrap().text(), URL);
        assert_eq!(app.tui.ghost_suffix(), None);
        key(app, named("Enter"), &mut queue);
        assert_eq!(commands(&queue), [format!("page.open {URL}")]);
        queue.clear();

        // A command line, and the find field, never complete.
        app.tui.focus(KeyboardFocus::Chrome, &mut queue);
        for letter in [":", "e", "x"] {
            key(app, typed(letter), &mut queue);
        }
        assert_eq!(app.tui.ghost_suffix(), None);
        app.tui.find_field(String::new(), &mut queue);
        for letter in ["e", "x"] {
            key(app, typed(letter), &mut queue);
        }
        assert!(app.tui.finding);
        assert_eq!(app.tui.ghost_suffix(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_find_field_searches_as_it_is_typed_and_enter_and_escape_close_it() {
        let (_master, mut tui) = on_pty();
        let mut queue = VecDeque::new();
        tui.focus(KeyboardFocus::Chrome, &mut queue);
        tui.find_field("old".into(), &mut queue);
        assert!(tui.finding);
        let editor = tui.editing.as_ref().unwrap();
        assert_eq!((editor.text(), editor.selected()), ("old", true));
        assert!(queue.is_empty(), "opening searches nothing");
        let key = |tui: &mut Tui, key: KeyEvent, queue: &mut VecDeque<BrowserEvent>| {
            let event = InputEvent {
                seat: tui.seat,
                kind: InputKind::Key(key),
            };
            tui.chrome_input(event, queue);
        };
        // Typing replaces the selected text and searches each edit.
        key(&mut tui, typed("p"), &mut queue);
        key(&mut tui, typed("a"), &mut queue);
        assert_eq!(commands(&queue), ["page.find p", "page.find pa"]);
        queue.clear();
        // A space at the end searches nothing new; motion neither.
        key(&mut tui, typed(" "), &mut queue);
        key(&mut tui, named("ArrowLeft"), &mut queue);
        assert!(queue.is_empty(), "{queue:?}");
        // Emptied: the search stops.
        for _ in 0..3 {
            key(&mut tui, named("Backspace"), &mut queue);
        }
        assert_eq!(commands(&queue), ["page.find p", "page.find-stop"]);
        queue.clear();
        // Enter keeps the search and gives the page the keyboard.
        key(&mut tui, typed("q"), &mut queue);
        key(&mut tui, named("Enter"), &mut queue);
        assert_eq!(commands(&queue), ["page.find q"]);
        assert!(queue.contains(&BrowserEvent::Focus(KeyboardFocus::Page)));
        assert!(tui.editing.is_none() && !tui.finding);
        queue.clear();
        // Escape ends it.
        tui.focus(KeyboardFocus::Chrome, &mut queue);
        tui.find_field("q".into(), &mut queue);
        key(&mut tui, named("Escape"), &mut queue);
        assert_eq!(
            queue.iter().collect::<Vec<_>>(),
            [
                &BrowserEvent::Focus(KeyboardFocus::Page),
                &BrowserEvent::Command(Command::PageFindStop)
            ]
        );
        assert!(tui.editing.is_none() && !tui.finding);
        queue.clear();
        // The core moving the focus closes it and keeps the search; location.focus is the
        // address line again.
        tui.focus(KeyboardFocus::Chrome, &mut queue);
        tui.find_field("q".into(), &mut queue);
        tui.focus(KeyboardFocus::Page, &mut queue);
        assert!(tui.editing.is_none() && !tui.finding && queue.is_empty());
        tui.focus(KeyboardFocus::Chrome, &mut queue);
        tui.find_field("q".into(), &mut queue);
        tui.focus(KeyboardFocus::Chrome, &mut queue);
        assert!(tui.editing.is_some() && !tui.finding);
        assert!(queue.is_empty());
    }

    #[cfg(feature = "engine-demo")]
    #[test]
    fn text_mode_finds_in_the_pages_text_and_a_mode_change_ends_the_search() {
        let mut held = demo_app();
        let app = &mut held.app;
        app.tui.relayout();
        assert!(app.tui.text_mode, "a pty without pixels is text");
        app.session.queue_mut().push_back(BrowserEvent::Start {
            session: None,
            open: vec![OpenRequest {
                input: "demo://pattern".into(),
                engine: Some("demo".into()),
            }],
        });
        settle_document(app);
        let view = app.session.shown_view().expect("shown").0;
        for _ in 0..8 {
            app.sync_semantics();
            let _ = app.session.pump(&mut app.tui);
            app.run_core();
            if app.tui.text.has(view) {
                break;
            }
        }
        assert!(app.tui.text.has(view), "{}", page_debug(app));
        let find = |app: &mut App, command: &str| {
            let command = command.parse::<Command>().unwrap();
            app.session
                .queue_mut()
                .push_back(BrowserEvent::Command(command));
            app.run_core();
            let core = app.session.core();
            let tab = core.tabs().nth(core.active()).unwrap();
            let engine = tab_engine(core, &tab);
            tab.find.and_then(|find| find_indicator(find, engine, true))
        };
        // "page": the paragraph and the two links' labels.
        assert_eq!(find(app, "page.find page").as_deref(), Some("1 of 3"));
        assert_eq!(find(app, "page.find-next").as_deref(), Some("2 of 3"));
        assert_eq!(find(app, "page.find-previous").as_deref(), Some("1 of 3"));
        assert_eq!(find(app, "page.find-previous").as_deref(), Some("3 of 3"));
        assert_eq!(find(app, "page.find Page").as_deref(), Some("no matches"));
        assert_eq!(find(app, "page.find PAGE").as_deref(), Some("no matches"));
        assert_eq!(find(app, "page.find pAge").as_deref(), Some("no matches"));
        assert_eq!(find(app, "page.find text").as_deref(), Some("1 of 1"));
        // The match is drawn: the bottom line and the highlight.
        let mut out = Vec::new();
        app.draw_chrome(&mut out);
        let rows = app.tui.grid.rows();
        let bottom: String = (0..app.tui.grid.cols())
            .filter_map(|col| app.tui.grid.cell(col, rows - 1))
            .map(|cell| cell.grapheme.clone())
            .collect();
        assert!(bottom.trim_end().ends_with("/text 1 of 1"), "{bottom}");
        let reversed: String = (0..rows)
            .flat_map(|row| (0..app.tui.grid.cols()).map(move |col| (col, row)))
            .filter_map(|(col, row)| app.tui.grid.cell(col, row))
            .filter(|cell| cell.attrs.reverse)
            .map(|cell| cell.grapheme.clone())
            .collect();
        assert_eq!(reversed, "text");
        // Leaving text mode ends the search and says so.
        app.tui.mode_override = Some(false);
        app.tui.relayout();
        assert!(!app.tui.text_mode);
        assert!(app
            .tui
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("Searches in pages ended")));
        app.run_core();
        let core = app.session.core();
        assert!(core.tabs().nth(core.active()).unwrap().find.is_none());
    }

    /// The text page of the shown view, once its snapshot came.
    #[cfg(feature = "engine-demo")]
    fn wait_for_page(app: &mut App) -> ViewId {
        let view = app.session.shown_view().expect("shown").0;
        for _ in 0..8 {
            app.sync_semantics();
            let _ = app.session.pump(&mut app.tui);
            app.run_core();
            if app.tui.text.has(view) {
                break;
            }
        }
        assert!(app.tui.text.has(view), "{}", page_debug(app));
        view
    }

    /// The active tab's search as the bottom line says it with the field open.
    #[cfg(feature = "engine-demo")]
    fn found(app: &App) -> Option<String> {
        let core = app.session.core();
        let tab = core.tabs().nth(core.active()).unwrap();
        let engine = tab_engine(core, &tab);
        tab.find.and_then(|find| find_indicator(find, engine, true))
    }

    #[cfg(feature = "engine-demo")]
    fn run(app: &mut App, command: &str) {
        let command = command.parse::<Command>().unwrap();
        app.session
            .queue_mut()
            .push_back(BrowserEvent::Command(command));
        app.run_core();
    }

    #[cfg(feature = "engine-demo")]
    fn start_demo(app: &mut App, pages: &[&str]) {
        app.tui.relayout();
        assert!(app.tui.text_mode, "a pty without pixels is text");
        app.session.queue_mut().push_back(BrowserEvent::Start {
            session: None,
            open: pages
                .iter()
                .map(|page| OpenRequest {
                    input: (*page).into(),
                    engine: Some("demo".into()),
                })
                .collect(),
        });
        settle_document(app);
    }

    #[cfg(feature = "engine-demo")]
    #[test]
    fn a_search_before_the_pages_text_is_answered_when_the_text_comes() {
        let mut held = demo_app();
        let app = &mut held.app;
        start_demo(app, &["demo://pattern"]);
        let view = app.session.shown_view().expect("shown").0;
        assert!(!app.tui.text.has(view), "no snapshot yet");
        run(app, "page.find page");
        assert_eq!(found(app), None, "nothing is said before the text comes");
        wait_for_page(app);
        assert_eq!(found(app).as_deref(), Some("1 of 3"));
        // A new snapshot of the same document with other matches is answered again.
        run(app, "page.find pressed");
        assert_eq!(found(app).as_deref(), Some("no matches"));
        let document = app.session.core().view(view).unwrap().document().unwrap();
        let semantic = |action| BrowserEvent::Semantic {
            view,
            document,
            action,
            link: None,
            at: Instant::now(),
        };
        app.session.queue_mut().push_back(semantic(
            sighurt_api::semantic::SemanticAction::Activate(sighurt_api::semantic::NodeId(10)),
        ));
        app.run_core();
        for _ in 0..4 {
            let _ = app.session.pump(&mut app.tui);
            app.run_core();
        }
        assert_eq!(
            found(app).as_deref(),
            Some("1 of 1"),
            "the field says pressed"
        );
    }

    #[cfg(feature = "engine-demo")]
    #[test]
    fn a_mode_change_ends_every_search_and_a_hidden_tabs_moves_go_nowhere() {
        let mut held = demo_app();
        let app = &mut held.app;
        start_demo(app, &["demo://pattern", "demo://resize"]);
        let first = wait_for_page(app);
        run(app, "page.find page");
        assert_eq!(found(app).as_deref(), Some("1 of 3"));
        let search = app.tui.searches[&first].search;
        // The second tab, then graphics: the hidden first tab's search was the text finder's.
        run(app, "tab.select 2");
        settle_document(app);
        app.tui.mode_override = Some(false);
        app.tui.relayout();
        assert!(app
            .tui
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("Searches in pages ended")));
        app.run_core();
        // Its moves reach neither finder.
        let mut queue = VecDeque::new();
        assert!(app
            .tui
            .find(first, &FindCommand::Next { search }, &mut queue));
        assert!(queue.is_empty());
        // Shown again, it ends.
        run(app, "tab.select 1");
        settle_document(app);
        app.run_core();
        assert_eq!(found(app), None);
        assert!(app.session.core().view(first).unwrap().find().is_none());
        assert!(!app.tui.searches.contains_key(&first));
        assert!(!app.tui.orphaned.contains(&first));
        // A new search there is the engine's now, and works.
        run(app, "page.find page");
        for _ in 0..4 {
            let _ = app.session.pump(&mut app.tui);
            app.run_core();
        }
        assert_eq!(found(app).as_deref(), Some("1 of 3"));
    }
}
