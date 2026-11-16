//! The page's states and what moves it between them: the start screen, a terminal opening,
//! running, and ended with how it ended.
//!
//! The page starts nothing by itself: Enter on the start screen asks the browser for a terminal
//! (that key press is the deliberate action the browser requires), and the browser may refuse.
//! While it runs, keys, pastes and input-method text go to the program
//! ([`crate::keys`], [`crate::paste`]), its output goes through the emulator ([`crate::term`]),
//! at most [`MAX_OUTPUT_PER_FRAME`] bytes a frame so a flood stays within a frame's fuel (the rest
//! waits in the browser, which stops reading from the program while it is full), and the grid
//! follows the page's size. When the program ends the last screen stays, with a notice of how it
//! ended, and Enter starts it again.
//!
//! On `sig://config` the program is the user's editor on sig's configuration. Its start screen
//! says which editor and which file, from the browser's answer ([`EditorInfo`], asked again
//! when the editor ends, so the file's problems show), and the same screen comes back when the
//! editor ends, since an editor leaves nothing on the terminal worth keeping.
//!
//! The pointer: while the program asked for the mouse ([`crate::mouse`]) its presses, releases,
//! motion and wheel are the program's, except with Shift held, which is always the user's (as in
//! xterm). Otherwise the left button selects text (a drag; a double click a word, a triple
//! click a line), which becomes the primary selection when the button goes up, the middle
//! button pastes the primary selection, and the wheel looks back through the history on the
//! main screen, or sends the arrow keys on the alternate screen (so `less` and `man` scroll, as
//! xterm's alternate scroll does). Shift+Page Up and Shift+Page Down page through the history;
//! typing returns to the live screen. Ctrl+Shift+C copies the selection to the clipboard,
//! Ctrl+Shift+V pastes the clipboard and Shift+Insert the primary selection: through the
//! browser's clipboard service, whose rule for any page holds (a click or key press in the page
//! in the last 5 s, which a read uses up), so nothing the program prints can read or write a
//! clipboard (OSC 52 is not honoured). Selections and copies are bounded
//! ([`MAX_SELECTION_LINES`], [`MAX_COPY`]).
//!
//! Owns: the page's logic. Must not own: the `sig` calls (`crate::page` gives them through
//! [`Host`] and [`Terminal`]) or the drawing ([`crate::draw`]).

use sig_page_kit::sig::modifiers::{ALT, ALT_GRAPH, CONTROL, SHIFT, SUPER};
use sig_page_kit::sig::{buttons, Input, InputKind, Program, Status};

use crate::draw::{self, Line, Metrics, Op, Overlay};
use crate::keys::{self, Modes};
use crate::mouse::{self, Button, Event, Held};
use crate::paste;
use crate::term::Term;

/// Most bytes of output one frame parses (graphics data counts less: [`Term::feed`]).
pub const MAX_OUTPUT_PER_FRAME: usize = 64 * 1024;
/// Most bytes of output one frame reads, graphics data included.
pub const MAX_READ_PER_FRAME: usize = 1 << 20;
/// Most lines a selection copies.
pub const MAX_SELECTION_LINES: i64 = 2000;
/// Most bytes one copy puts on a clipboard.
pub const MAX_COPY: usize = 1 << 20;
/// Most wheel steps one record makes.
const MAX_WHEEL_STEPS: i32 = 50;
/// Two presses at most this far apart (milliseconds) in one cell are a double click.
const DOUBLE_CLICK_MS: u64 = 400;
/// Characters a double click takes as part of a word, besides letters and digits.
const WORD_CHARACTERS: &str = "@-./_~?&=%+#:";
/// Most bytes of input held for the program while its queue in the browser is full; more is
/// dropped, as keys typed into a program that reads nothing.
const MAX_PENDING: usize = 64 * 1024;

/// A terminal of the browser's, as the page uses it.
pub trait Terminal {
    fn status(&self) -> Status;
    /// Output waiting: `Some(0)` none now, `None` ended.
    fn read(&self, buf: &mut [u8]) -> Option<usize>;
    /// Bytes taken, `None` ended.
    fn write(&self, bytes: &[u8]) -> Option<usize>;
    fn resize(&self, cols: u16, rows: u16) -> bool;
}

/// Where terminals come from.
pub trait Host {
    type Pty: Terminal;
    /// Asks for a terminal; `None` when the browser refuses at once.
    fn open(&mut self, program: Program, cols: u16, rows: u16) -> Option<Self::Pty>;
}

/// How a terminal ended, as the notice says it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Exited(u8),
    Signalled,
    /// The program closed the terminal; how it ended is not known yet.
    Closed,
    /// The browser closed it (the page lost it, or it was refused after opening).
    Cancelled,
}

enum Phase<P> {
    /// Nothing runs; `refused` once the browser said no.
    Start {
        refused: bool,
    },
    Opening(P),
    Running(P),
    /// The terminal is kept until a new one starts, for the status the browser reports late.
    Ended(End, P),
}

/// What the browser says `sig://config` would run (`PageInfo::Editor`'s records).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EditorInfo {
    /// `[pages.config] enabled`.
    pub enabled: bool,
    /// The editor's command line; empty when none was found.
    pub editor: String,
    /// Where it comes from: `setting`, `VISUAL`, `EDITOR`, `PATH` or `none`.
    pub from: String,
    /// The configuration file.
    pub file: String,
    /// Whether the file does not exist yet.
    pub new: bool,
    /// The file's problems, which keep sig on the configuration it has.
    pub problems: Vec<String>,
}

impl EditorInfo {
    /// Reads the records of the browser's answer; unknown records are skipped.
    pub fn parse(text: &str) -> Self {
        let mut info = Self::default();
        for line in text.lines() {
            let (key, value) = line.split_once('\t').unwrap_or((line, ""));
            match key {
                "enabled" => info.enabled = value == "yes",
                "editor" => info.editor = value.into(),
                "from" => info.from = value.into(),
                "file" => info.file = value.into(),
                "new" => info.new = value == "yes",
                "problem" => info.problems.push(value.into()),
                _ => {}
            }
        }
        info
    }
}

/// What the page asks the browser's clipboard to do ([`App::take_asks`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ask {
    /// Put `text` on the clipboard, or make it the primary selection.
    Copy { text: String, primary: bool },
    /// The text of the clipboard or the primary selection, for [`App::pasted`].
    Paste { primary: bool },
}

/// How a selection grows: by cells, words or lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Cell,
    Word,
    Line,
}

/// Selected text, from where the button went down (`anchor`) to where it is (`head`), as lines
/// of the screen's content and columns, both included; `range` is the two in order, grown to
/// whole words or lines, `None` while nothing is selected (a click that has not moved).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Selection {
    alt: bool,
    epoch: u64,
    unit: Unit,
    anchor: (i64, u16),
    head: (i64, u16),
    range: Option<((i64, u16), (i64, u16))>,
}

/// The pointer as the program's reports need it.
#[derive(Default)]
struct Pointer {
    /// Buttons whose press was reported, a bit each.
    reported: u8,
    /// The cell the last report was for.
    cell: Option<(u16, u16)>,
    /// Wheel distance not yet a whole step.
    wheel: f32,
    /// The left button is down for a selection.
    selecting: bool,
    /// The last press: when, where, and how many in a row.
    last: Option<(u64, (u16, u16), u8)>,
}

pub struct App<P> {
    program: Program,
    phase: Phase<P>,
    /// What `sig://config` would run, once the browser said.
    info: Option<EditorInfo>,
    /// Whether the page should ask the browser for [`EditorInfo`] (again).
    wants_info: bool,
    term: Term,
    /// Columns and rows.
    size: (u16, u16),
    pending: Vec<u8>,
    dirty: bool,
    metrics: Option<Metrics>,
    /// Milliseconds since the page started.
    clock: u64,
    selection: Option<Selection>,
    pointer: Pointer,
    asks: Vec<Ask>,
    /// The history's length when last read.
    history: usize,
}

impl<P: Terminal> App<P> {
    pub fn new(program: Program) -> Self {
        Self {
            program,
            phase: Phase::Start { refused: false },
            info: None,
            wants_info: program == Program::EditConfig,
            term: Term::new(24, 80),
            size: (80, 24),
            pending: Vec::new(),
            dirty: true,
            metrics: None,
            clock: 0,
            selection: None,
            pointer: Pointer::default(),
            asks: Vec::new(),
            history: 0,
        }
    }

    /// The face's cell, once measured.
    pub fn set_metrics(&mut self, m: Metrics) {
        if self.metrics != Some(m) {
            self.metrics = Some(m);
            self.term.set_cell(m.cell_w, m.cell_h);
            self.dirty = true;
        }
    }

    /// `ms` milliseconds passed.
    pub fn advance_clock(&mut self, ms: u32) {
        self.clock += u64::from(ms);
    }

    /// What the page asks the clipboard, since the last call.
    pub fn take_asks(&mut self) -> Vec<Ask> {
        std::mem::take(&mut self.asks)
    }

    /// The text of a clipboard the page asked for ([`Ask::Paste`]): pasted into the program.
    pub fn pasted(&mut self, text: &str) {
        if matches!(self.phase, Phase::Running(_)) {
            let bytes = paste::paste(text, self.term.screen().bracketed_paste());
            self.typed(&bytes);
        }
    }

    /// The PNG of image `key`, to draw it.
    pub fn png(&self, key: u64) -> Option<&[u8]> {
        self.term.graphics().png(key)
    }

    /// Whether the page should ask the browser what the editor is, since the last call: at the
    /// start, and when the editor ended (the file may have changed).
    pub fn take_wants_info(&mut self) -> bool {
        std::mem::take(&mut self.wants_info)
    }

    /// The browser's answer about the editor (`PageInfo::Editor`).
    pub fn editor_info(&mut self, text: &str) {
        self.info = Some(EditorInfo::parse(text));
        self.dirty = true;
    }

    /// Whether something changed since the last call, and the page must be drawn again.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// One record of the browser's input queue.
    pub fn input(&mut self, host: &mut impl Host<Pty = P>, input: &Input) {
        match &self.phase {
            // Only Enter pressed afresh: one held since the program ran, or a key bound to send
            // Enter, repeats, and its repeats start nothing.
            Phase::Start { .. } | Phase::Ended(..) => {
                if input.kind == InputKind::Press && input.is_key("Enter", 0) {
                    self.start(host);
                }
            }
            Phase::Opening(_) => {}
            Phase::Running(_) => {
                if self.page_key(input) {
                    return;
                }
                let screen = self.term.screen();
                let bytes = match input.kind {
                    InputKind::Press | InputKind::Repeat => {
                        let modes = Modes {
                            application_cursor: screen.application_cursor(),
                        };
                        keys::encode(&input.key, &input.text, input.modifiers, modes)
                    }
                    InputKind::Paste => Some(paste::paste(&input.text, screen.bracketed_paste())),
                    InputKind::Commit => Some(paste::commit(&input.text)),
                    _ => None,
                };
                if let Some(bytes) = bytes {
                    self.typed(&bytes);
                }
            }
        }
        if matches!(
            input.kind,
            InputKind::PointerPress
                | InputKind::PointerRelease
                | InputKind::Motion
                | InputKind::Wheel
        ) {
            self.pointer_input(input);
        }
    }

    /// The page's own keys while a program runs: copying, pasting and paging through the
    /// history. Whether the key was one.
    fn page_key(&mut self, input: &Input) -> bool {
        if !matches!(input.kind, InputKind::Press | InputKind::Repeat) {
            return false;
        }
        let held = input.modifiers & (SHIFT | CONTROL | ALT | SUPER | ALT_GRAPH);
        let key = input.key.to_ascii_lowercase();
        let alt_screen = self.term.screen().alternate_screen();
        match (held, key.as_str()) {
            (m, "c") if m == CONTROL | SHIFT => {
                if input.kind == InputKind::Press {
                    if let Some(text) = self.selected_text() {
                        self.asks.push(Ask::Copy {
                            text,
                            primary: false,
                        });
                    }
                }
                true
            }
            (m, "v") if m == CONTROL | SHIFT => {
                if input.kind == InputKind::Press {
                    self.asks.push(Ask::Paste { primary: false });
                }
                true
            }
            (SHIFT, "insert") => {
                if input.kind == InputKind::Press {
                    self.asks.push(Ask::Paste { primary: true });
                }
                true
            }
            (SHIFT, "pageup" | "pagedown") if !alt_screen => {
                let page = self.size.1.saturating_sub(1).max(1) as isize;
                let lines = if key == "pageup" { page } else { -page };
                if self.term.scroll_view(lines) {
                    self.dirty = true;
                }
                true
            }
            _ => false,
        }
    }

    /// Bytes the user typed or pasted, for the program: the view returns to the live screen and
    /// a selection goes, as in any terminal.
    fn typed(&mut self, bytes: &[u8]) {
        let view = self.term.view();
        if view > 0 {
            self.term.scroll_view(-(view as isize));
            self.dirty = true;
        }
        if self.selection.take().is_some() {
            self.dirty = true;
        }
        self.send(bytes);
    }

    /// The cell under a pointer record, within the grid.
    fn cell_at(&self, input: &Input) -> Option<(u16, u16)> {
        let m = self.metrics?;
        let (cols, rows) = self.size;
        let col = (input.x / m.cell_w).floor().clamp(0.0, f32::from(cols - 1)) as u16;
        let row = (input.y / m.cell_h).floor().clamp(0.0, f32::from(rows - 1)) as u16;
        Some((col, row))
    }

    fn pointer_input(&mut self, input: &Input) {
        // The screen is shown while a program runs and after it ended (but the editor's page
        // shows its own screen then).
        let shown = match &self.phase {
            Phase::Running(_) => true,
            Phase::Ended(..) => self.program == Program::Shell,
            _ => false,
        };
        let Some(cell) = self.cell_at(input).filter(|_| shown) else {
            return;
        };
        let running = matches!(self.phase, Phase::Running(_));
        let screen = self.term.screen();
        let (mode, encoding) = (
            screen.mouse_protocol_mode(),
            screen.mouse_protocol_encoding(),
        );
        let alt_screen = screen.alternate_screen();
        let held = Held {
            shift: input.has(SHIFT),
            alt: input.has(ALT),
            control: input.has(CONTROL),
        };
        let wants_mouse = running && mode != vt100::MouseProtocolMode::None;
        let report = wants_mouse && !held.shift;
        let button = match input.code {
            buttons::LEFT => Button::Left,
            buttons::MIDDLE => Button::Middle,
            buttons::RIGHT => Button::Right,
            _ => Button::None,
        };
        let bit = match button {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 4,
            _ => 0,
        };
        let send = |app: &mut Self, event, button, cell: (u16, u16)| {
            if let Some(bytes) = mouse::encode(mode, encoding, event, button, cell.0, cell.1, held)
            {
                app.send(&bytes);
            }
        };
        match input.kind {
            InputKind::PointerPress if bit != 0 => {
                if report {
                    self.pointer.reported |= bit;
                    self.pointer.cell = Some(cell);
                    send(self, Event::Press, button, cell);
                    return;
                }
                match button {
                    Button::Left => self.select_start(cell),
                    Button::Middle if running => self.asks.push(Ask::Paste { primary: true }),
                    _ => {}
                }
            }
            InputKind::PointerRelease if bit != 0 => {
                if self.pointer.reported & bit != 0 {
                    self.pointer.reported &= !bit;
                    send(self, Event::Release, button, cell);
                } else if button == Button::Left && self.pointer.selecting {
                    self.pointer.selecting = false;
                    self.select_to(cell);
                    if let Some(text) = self.selected_text() {
                        self.asks.push(Ask::Copy {
                            text,
                            primary: true,
                        });
                    }
                }
            }
            InputKind::Motion => {
                let reported = self.pointer.reported;
                if reported != 0 || (report && mode == vt100::MouseProtocolMode::AnyMotion) {
                    if self.pointer.cell != Some(cell) {
                        self.pointer.cell = Some(cell);
                        let button = match reported {
                            0 => Button::None,
                            r if r & 1 != 0 => Button::Left,
                            r if r & 2 != 0 => Button::Middle,
                            _ => Button::Right,
                        };
                        send(self, Event::Motion, button, cell);
                    }
                } else if self.pointer.selecting {
                    self.select_to(cell);
                }
            }
            InputKind::Wheel => {
                let m = self.metrics.expect("a cell was measured");
                // A notch, or three lines' worth of a touchpad's pixels, is one step.
                let units = match input.code {
                    1 => input.dy,
                    _ => input.dy / (3.0 * m.cell_h),
                };
                self.pointer.wheel += units;
                let steps =
                    (self.pointer.wheel.trunc() as i32).clamp(-MAX_WHEEL_STEPS, MAX_WHEEL_STEPS);
                self.pointer.wheel -= self.pointer.wheel.trunc();
                if steps == 0 {
                    return;
                }
                let down = steps > 0;
                for _ in 0..steps.unsigned_abs() {
                    if report {
                        let wheel = if down {
                            Button::WheelDown
                        } else {
                            Button::WheelUp
                        };
                        send(self, Event::Press, wheel, cell);
                    } else if alt_screen && running {
                        let modes = Modes {
                            application_cursor: self.term.screen().application_cursor(),
                        };
                        let arrow = if down { "ArrowDown" } else { "ArrowUp" };
                        let bytes = keys::encode(arrow, "", 0, modes).unwrap_or_default();
                        self.send(&bytes.repeat(3));
                    } else if !alt_screen && self.term.scroll_view(if down { -3 } else { 3 }) {
                        self.dirty = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// The line of the screen's content at `row` of the page.
    fn line_at(&self, row: u16) -> i64 {
        self.term.top_line() + i64::from(row)
    }

    fn select_start(&mut self, cell: (u16, u16)) {
        let now = self.clock;
        let clicks = match self.pointer.last {
            Some((at, last, n)) if last == cell && now.saturating_sub(at) <= DOUBLE_CLICK_MS => {
                n % 3 + 1
            }
            _ => 1,
        };
        self.pointer.last = Some((now, cell, clicks));
        self.pointer.selecting = true;
        let unit = match clicks {
            1 => Unit::Cell,
            2 => Unit::Word,
            _ => Unit::Line,
        };
        let at = (self.line_at(cell.1), cell.0);
        let mut selection = Selection {
            alt: self.term.screen().alternate_screen(),
            epoch: self.term.epoch(),
            unit,
            anchor: at,
            head: at,
            range: None,
        };
        selection.range = self.grow(&selection);
        self.selection = Some(selection);
        self.dirty = true;
    }

    fn select_to(&mut self, cell: (u16, u16)) {
        let head = (self.line_at(cell.1), cell.0);
        let Some(mut selection) = self.selection.filter(|s| s.head != head) else {
            return;
        };
        selection.head = head;
        selection.range = self.grow(&selection);
        self.selection = Some(selection);
        self.dirty = true;
    }

    /// The selection's first and last cells, ordered and grown to its unit.
    fn grow(&mut self, s: &Selection) -> Option<((i64, u16), (i64, u16))> {
        let (mut start, mut end) = if s.anchor <= s.head {
            (s.anchor, s.head)
        } else {
            (s.head, s.anchor)
        };
        match s.unit {
            Unit::Cell if start == end => return None,
            Unit::Cell => {}
            Unit::Word => {
                start.1 = self.word_edge(start, false);
                end.1 = self.word_edge(end, true);
            }
            Unit::Line => {
                start.1 = 0;
                end.1 = self.size.0.saturating_sub(1);
            }
        }
        Some((start, end))
    }

    /// The selection still standing: on this screen and with its text still there.
    fn selection(&self) -> Option<Selection> {
        self.selection.filter(|s| {
            s.alt == self.term.screen().alternate_screen() && s.epoch == self.term.epoch()
        })
    }

    /// Where the word at `at` starts (or ends, if `forward`).
    fn word_edge(&mut self, at: (i64, u16), forward: bool) -> u16 {
        let cols = self.size.0;
        let text = self
            .term
            .text((at.0, 0), (at.0, cols), 4 * usize::from(cols) + 16);
        let chars: Vec<char> = text.chars().collect();
        let word = |c: char| c.is_alphanumeric() || WORD_CHARACTERS.contains(c);
        let at_col = usize::from(at.1);
        if !chars.get(at_col).copied().is_some_and(word) {
            return at.1;
        }
        let mut col = at_col;
        if forward {
            while chars.get(col + 1).copied().is_some_and(word) {
                col += 1;
            }
        } else {
            while col > 0 && chars.get(col - 1).copied().is_some_and(word) {
                col -= 1;
            }
        }
        col as u16
    }

    /// The selection's text, bounded.
    fn selected_text(&mut self) -> Option<String> {
        let (start, end) = self.selection()?.range?;
        let start = if end.0 - start.0 >= MAX_SELECTION_LINES {
            (end.0 - MAX_SELECTION_LINES + 1, 0)
        } else {
            start
        };
        let text = self.term.text(start, (end.0, end.1 + 1), MAX_COPY);
        (!text.is_empty()).then_some(text)
    }

    fn start(&mut self, host: &mut impl Host<Pty = P>) {
        let (cols, rows) = self.size;
        self.term = Term::new(rows, cols);
        self.pending.clear();
        // Close the ended terminal first: a document has one at a time.
        self.phase = Phase::Start { refused: false };
        self.phase = match host.open(self.program, cols, rows) {
            Some(pty) => Phase::Opening(pty),
            None => Phase::Start { refused: true },
        };
        self.dirty = true;
    }

    fn send(&mut self, bytes: &[u8]) {
        let room = MAX_PENDING.saturating_sub(self.pending.len());
        self.pending
            .extend_from_slice(&bytes[..bytes.len().min(room)]);
        self.flush();
    }

    fn flush(&mut self) {
        if let Phase::Running(pty) = &self.phase {
            if !self.pending.is_empty() {
                let taken = pty.write(&self.pending).unwrap_or(self.pending.len());
                self.pending.drain(..taken.min(self.pending.len()));
            }
        }
    }

    /// One frame of a page `size` (columns, rows) big: follows the size, reads the program's
    /// output and moves between the phases.
    pub fn frame(&mut self, size: (u16, u16)) {
        if size != self.size {
            self.size = size;
            self.term.resize(size.1, size.0);
            if let Phase::Opening(pty) | Phase::Running(pty) = &self.phase {
                pty.resize(size.0, size.1);
            }
            self.dirty = true;
        }
        let phase = std::mem::replace(&mut self.phase, Phase::Start { refused: false });
        self.phase = match phase {
            Phase::Opening(pty) => match pty.status() {
                Status::Opening => Phase::Opening(pty),
                Status::Running => {
                    self.dirty = true;
                    Phase::Running(pty)
                }
                Status::Cancelled => {
                    self.dirty = true;
                    Phase::Start { refused: true }
                }
                status => {
                    self.dirty = true;
                    self.wants_info = self.program == Program::EditConfig;
                    Phase::Ended(end(status), pty)
                }
            },
            Phase::Running(pty) => {
                let ended = self.read(&pty);
                if self.term.busy() && self.term.step() {
                    self.dirty = true;
                }
                let status = pty.status();
                if ended || !matches!(status, Status::Running | Status::Opening) {
                    self.dirty = true;
                    self.wants_info = self.program == Program::EditConfig;
                    Phase::Ended(end(status), pty)
                } else {
                    Phase::Running(pty)
                }
            }
            Phase::Ended(End::Closed, pty) => {
                let now = end(pty.status());
                self.dirty |= now != End::Closed;
                Phase::Ended(now, pty)
            }
            other => other,
        };
        self.flush();
        if self.selection.is_some() && self.selection().is_none() {
            self.selection = None;
            self.dirty = true;
        }
        if self.dirty {
            self.history = self.term.history();
        }
    }

    /// Reads and parses up to [`MAX_OUTPUT_PER_FRAME`] bytes' worth (and at most
    /// [`MAX_READ_PER_FRAME`] bytes); whether the output has ended.
    fn read(&mut self, pty: &P) -> bool {
        let mut buf = vec![0u8; 16 * 1024];
        let (mut cost, mut total) = (0, 0);
        while cost < MAX_OUTPUT_PER_FRAME && total < MAX_READ_PER_FRAME {
            match pty.read(&mut buf) {
                None => return true,
                Some(0) => break,
                Some(n) => {
                    total += n;
                    let (replies, spent) = self.term.feed(&buf[..n]);
                    cost += spent;
                    self.dirty = true;
                    if !replies.is_empty() {
                        let room = MAX_PENDING.saturating_sub(self.pending.len());
                        self.pending
                            .extend_from_slice(&replies[..replies.len().min(room)]);
                    }
                }
            }
        }
        false
    }

    /// What to draw, after a clear to the background.
    pub fn ops(&self, m: Metrics) -> Vec<Op> {
        let cols = self.size.0;
        match &self.phase {
            Phase::Start { refused } => draw::page(&self.start_page(*refused, None), m, cols),
            Phase::Opening(_) => Vec::new(),
            Phase::Running(_) => draw::screen(self.term.screen(), m, &self.overlay(m)),
            Phase::Ended(how, _) if self.program == Program::EditConfig => {
                draw::page(&self.start_page(false, Some(*how)), m, cols)
            }
            Phase::Ended(how, _) => {
                let overlay = Overlay {
                    cursor: None,
                    ..self.overlay(m)
                };
                let mut ops = draw::screen(self.term.screen(), m, &overlay);
                let said = match how {
                    End::Exited(code) => format!("The program exited with status {code}."),
                    End::Signalled => "A signal ended the program.".into(),
                    End::Closed => "The program closed the terminal.".into(),
                    End::Cancelled => "The terminal was closed.".into(),
                };
                let y = f32::from(self.size.1.saturating_sub(1)) * m.cell_h;
                ops.push(Op::Fill {
                    x: 0.0,
                    y,
                    w: f32::from(cols) * m.cell_w,
                    h: m.cell_h,
                    color: draw::ACCENT,
                });
                ops.push(Op::Text {
                    x: m.cell_w,
                    y: y + (m.cell_h - m.size * 1.2) / 2.0,
                    text: format!("{said} Press Enter to start it again."),
                    color: draw::BACKGROUND,
                    bold: true,
                    italic: false,
                });
                ops
            }
        }
    }

    /// What is drawn with the screen: the selection, the cursor's shape, the images and how far
    /// back the view is.
    fn overlay(&self, m: Metrics) -> Overlay {
        let top = self.term.top_line();
        let selection = self
            .selection()
            .and_then(|s| s.range)
            .map(|(start, end)| ((start.0 - top, start.1), (end.0 - top, end.1 + 1)));
        let view = self.term.view();
        let (below, above) = draw::images(self.term.graphics(), top, self.term.screen(), m);
        Overlay {
            selection,
            cursor: (view == 0).then(|| self.term.cursor_shape()),
            below,
            above,
            scrolled_back: (view > 0).then_some((view, self.history)),
        }
    }

    /// The page's own screen: what runs, that it runs with the user's rights, and Enter; how
    /// the last program `ended`, and why the browser `refused` one.
    fn start_page(&self, refused: bool, ended: Option<End>) -> Vec<Line> {
        use draw::{ACCENT, FOREGROUND, MUTED};
        let blank = || Line::new("", FOREGROUND);
        let mut lines = match self.program {
            Program::Shell => self.shell_lines(),
            Program::EditConfig => self.editor_lines(),
        };
        lines.push(blank());
        if let Some(how) = ended {
            let said = match how {
                End::Exited(code) => format!("The editor exited with status {code}."),
                End::Signalled => "A signal ended the editor.".into(),
                End::Closed => "The editor closed the terminal.".into(),
                End::Cancelled => "The terminal was closed.".into(),
            };
            lines.push(Line::new(format!("{said} sig read the file again."), FOREGROUND).bold());
            lines.push(Line::new("Press Enter to start it again.", ACCENT).bold());
        } else {
            lines.push(Line::new("Press Enter to start.", ACCENT).bold());
        }
        lines.push(blank());
        lines.push(Line::new(
            "With sig's example keymap, keys reach the program in INSERT mode. sig://help/keys \
             lists the keys that enter and leave it on this page.",
            MUTED,
        ));
        lines.push(Line::new(
            "Select text with the mouse (Shift with it when the program takes the mouse); \
             Ctrl+Shift+C copies, Ctrl+Shift+V pastes, Shift+Page Up looks back. Programs may \
             show images (kitty's graphics protocol and sixel). A program in a background tab \
             gets its output read once a second.",
            MUTED,
        ));
        if refused {
            lines.extend(self.refused_lines());
        }
        lines
    }

    fn shell_lines(&self) -> Vec<Line> {
        use draw::{ACCENT, FOREGROUND, MUTED};
        vec![
            Line::new("sig://terminal", ACCENT).bold(),
            Line::new("", FOREGROUND),
            Line::new(
                "This page runs your shell: [pages.terminal] command if the configuration sets \
                 it, else $SHELL, else /bin/sh, in your home directory.",
                FOREGROUND,
            ),
            Line::new("", FOREGROUND),
            Line::new(
                "The program runs with your full rights, as in any terminal: it can read and \
                 change your files and use your credentials. Only the terminal emulator, which \
                 reads what the program prints, runs in sig's Wasm sandbox. The program does not.",
                FOREGROUND,
            ),
            Line::new(
                "sig://config runs your editor on sig's configuration instead.",
                MUTED,
            ),
        ]
    }

    fn editor_lines(&self) -> Vec<Line> {
        use draw::{ACCENT, FOREGROUND, MUTED, WARNING};
        let mut lines = vec![
            Line::new("sig://config", ACCENT).bold(),
            Line::new("", FOREGROUND),
        ];
        match &self.info {
            None => lines.push(Line::new(
                "This page runs your editor on sig's configuration file: [pages.config] editor \
                 if the configuration sets it, else $VISUAL, else $EDITOR, else the first of \
                 nvim, vim, vi and nano found.",
                FOREGROUND,
            )),
            Some(info) => {
                if !info.enabled {
                    lines.push(Line::new("sig://config is off.", WARNING).bold());
                    lines.push(Line::new(
                        "sig's configuration sets [pages.config] enabled = false. Set it to true, \
                         or remove the line, with any editor; the page works again once sig has \
                         read the file:",
                        WARNING,
                    ));
                    lines.push(Line::new(format!("    {}", info.file), FOREGROUND));
                    lines.push(Line::new("", FOREGROUND));
                }
                if info.editor.is_empty() {
                    lines.push(Line::new("No editor found.", WARNING).bold());
                    lines.push(Line::new(
                        "Set [pages.config] editor in sig's configuration, or $VISUAL or $EDITOR, \
                         or install nvim, vim, vi or nano.",
                        WARNING,
                    ));
                } else {
                    lines.push(Line::new(
                        "This page runs your editor on sig's configuration file:",
                        FOREGROUND,
                    ));
                    lines.push(Line::new(
                        format!("    {} {}", info.editor, info.file),
                        ACCENT,
                    ));
                    let from = match info.from.as_str() {
                        "setting" => "[pages.config] editor",
                        "VISUAL" => "$VISUAL",
                        "EDITOR" => "$EDITOR",
                        _ => "found on PATH: neither [pages.config] editor, $VISUAL nor $EDITOR is set",
                    };
                    lines.push(Line::new(format!("(from {from})"), MUTED));
                }
                if info.new {
                    lines.push(Line::new(
                        "The file does not exist yet: the editor makes it when you save.",
                        MUTED,
                    ));
                }
                if !info.problems.is_empty() {
                    lines.push(Line::new("", FOREGROUND));
                    lines.push(
                        Line::new(
                            "The file has problems, so sig keeps the configuration it had:",
                            WARNING,
                        )
                        .bold(),
                    );
                    for problem in &info.problems {
                        lines.push(Line::new(format!("    {problem}"), WARNING));
                    }
                }
            }
        }
        lines.push(Line::new("", FOREGROUND));
        lines.push(Line::new(
            "The editor runs with your full rights, as in any terminal: an editor can run shell \
             commands. Only the terminal emulator on this page, which reads what the editor \
             prints, runs in sig's Wasm sandbox. When the editor ends, sig reads the file again.",
            FOREGROUND,
        ));
        lines
    }

    fn refused_lines(&self) -> Vec<Line> {
        use draw::{FOREGROUND, MUTED, WARNING};
        match self.program {
            Program::Shell => vec![
                Line::new("", FOREGROUND),
                Line::new("The terminal did not start.", WARNING).bold(),
                Line::new(
                    "It is off unless your configuration turns it on. Add these lines to sig's \
                     config.toml (sig://config opens your editor on it), then press Enter again:",
                    WARNING,
                ),
                Line::new("    [pages.terminal]", FOREGROUND),
                Line::new("    enabled = true", FOREGROUND),
                Line::new(
                    "It is also refused while eight terminals run, or when the key press did not \
                     reach this page.",
                    MUTED,
                ),
            ],
            Program::EditConfig => vec![
                Line::new("", FOREGROUND),
                Line::new("The editor did not start.", WARNING).bold(),
                Line::new(
                    "sig refuses it while [pages.config] enabled is false, when it finds no \
                     editor, while eight terminals run, or when the key press did not reach this \
                     page.",
                    MUTED,
                ),
            ],
        }
    }
}

fn end(status: Status) -> End {
    match status {
        Status::Exited(code) => End::Exited(code),
        Status::Signalled => End::Signalled,
        Status::Cancelled => End::Cancelled,
        Status::Opening | Status::Running | Status::EndOfFile => End::Closed,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use sig_page_kit::sig::modifiers::{CONTROL, SHIFT};

    use super::*;

    /// A terminal whose program is this test.
    #[derive(Default)]
    struct Fake {
        status: Option<Status>,
        output: Vec<u8>,
        input: Vec<u8>,
        /// How many bytes `write` takes at a time.
        room: Option<usize>,
        ended: bool,
        sizes: Vec<(u16, u16)>,
    }

    #[derive(Clone, Default)]
    struct Pty(Rc<RefCell<Fake>>);

    impl Terminal for Pty {
        fn status(&self) -> Status {
            self.0.borrow().status.unwrap_or(Status::Running)
        }
        fn read(&self, buf: &mut [u8]) -> Option<usize> {
            let mut fake = self.0.borrow_mut();
            if fake.output.is_empty() {
                return if fake.ended { None } else { Some(0) };
            }
            let n = buf.len().min(fake.output.len());
            buf[..n].copy_from_slice(&fake.output[..n]);
            fake.output.drain(..n);
            Some(n)
        }
        fn write(&self, bytes: &[u8]) -> Option<usize> {
            let mut fake = self.0.borrow_mut();
            let n = fake.room.map_or(bytes.len(), |room| room.min(bytes.len()));
            fake.input.extend_from_slice(&bytes[..n]);
            Some(n)
        }
        fn resize(&self, cols: u16, rows: u16) -> bool {
            self.0.borrow_mut().sizes.push((cols, rows));
            true
        }
    }

    #[derive(Default)]
    struct Browser {
        refuse: bool,
        opened: Vec<(Program, u16, u16)>,
        pty: Pty,
    }

    impl Host for Browser {
        type Pty = Pty;
        fn open(&mut self, program: Program, cols: u16, rows: u16) -> Option<Pty> {
            self.opened.push((program, cols, rows));
            // As the engine: one terminal a document, until the page drops it.
            let held = Rc::strong_count(&self.pty.0) > 1;
            (!self.refuse && !held).then(|| self.pty.clone())
        }
    }

    fn press(key: &str, text: &str, modifiers: u8) -> Input {
        Input {
            kind: InputKind::Press,
            modifiers,
            key: key.into(),
            text: text.into(),
            ..Input::default()
        }
    }

    /// An auto-repeat of `key`, held down.
    fn repeat(key: &str, text: &str) -> Input {
        Input {
            kind: InputKind::Repeat,
            ..press(key, text, 0)
        }
    }

    fn paste_of(text: &str) -> Input {
        Input {
            kind: InputKind::Paste,
            text: text.into(),
            ..Input::default()
        }
    }

    const M: Metrics = Metrics {
        size: 10.0,
        cell_w: 6.0,
        cell_h: 12.0,
        monospace: true,
    };

    fn shown(app: &App<Pty>) -> String {
        app.ops(M)
            .into_iter()
            .filter_map(|op| match op {
                Op::Text { text, .. } => Some(text),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn running(browser: &mut Browser) -> App<Pty> {
        let mut app = App::new(Program::Shell);
        app.set_metrics(M);
        app.frame((40, 10));
        app.input(browser, &press("Enter", "\r", 0));
        app.frame((40, 10));
        app
    }

    #[test]
    fn nothing_starts_without_enter() {
        let mut browser = Browser::default();
        let mut app = App::<Pty>::new(Program::Shell);
        app.frame((200, 50));
        app.input(&mut browser, &press("a", "a", 0));
        app.input(&mut browser, &press("Enter", "\r", CONTROL));
        app.input(&mut browser, &paste_of("\n"));
        app.input(&mut browser, &repeat("Enter", "\r"));
        assert!(browser.opened.is_empty());
        let text = shown(&app);
        assert!(text.contains("Press Enter to start."), "{text}");
        assert!(text.contains("full rights"), "{text}");
        assert!(text.contains("$SHELL"), "{text}");
        app.input(&mut browser, &press("Enter", "\r", 0));
        assert_eq!(browser.opened, [(Program::Shell, 200, 50)]);
    }

    #[test]
    fn a_refusal_says_how_to_turn_it_on() {
        let mut browser = Browser {
            refuse: true,
            ..Browser::default()
        };
        let mut app = App::<Pty>::new(Program::Shell);
        app.frame((200, 50));
        app.input(&mut browser, &press("Enter", "\r", 0));
        let text = shown(&app);
        assert!(
            text.contains("[pages.terminal]") && text.contains("enabled = true"),
            "{text}"
        );
        let mut app = App::<Pty>::new(Program::EditConfig);
        app.frame((200, 50));
        app.input(&mut browser, &press("Enter", "\r", 0));
        let text = shown(&app);
        assert!(text.contains("The editor did not start."), "{text}");
        assert!(!text.contains("[pages.terminal]"), "{text}");
        // Refused after opening too.
        let mut browser = Browser::default();
        let mut app = App::<Pty>::new(Program::Shell);
        app.input(&mut browser, &press("Enter", "\r", 0));
        browser.pty.0.borrow_mut().status = Some(Status::Cancelled);
        app.frame((80, 24));
        assert!(shown(&app).contains("enabled = true"));
    }

    #[test]
    fn keys_and_pastes_reach_the_program_and_output_the_screen() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        app.input(&mut browser, &press("l", "l", 0));
        app.input(&mut browser, &press("s", "s", 0));
        app.input(&mut browser, &press("Enter", "\r", 0));
        app.input(&mut browser, &press("c", "", CONTROL));
        app.input(&mut browser, &press("Tab", "", SHIFT));
        app.input(&mut browser, &paste_of("a\u{1b}b\n"));
        assert_eq!(browser.pty.0.borrow().input, b"ls\r\x03\x1b[Zab\r");
        browser
            .pty
            .0
            .borrow_mut()
            .output
            .extend_from_slice(b"hi there\r\n\x1b[?2004h");
        app.frame((40, 10));
        assert!(app.take_dirty());
        assert!(shown(&app).contains("hi there"));
        browser.pty.0.borrow_mut().input.clear();
        app.input(&mut browser, &paste_of("x"));
        assert_eq!(browser.pty.0.borrow().input, b"\x1b[200~x\x1b[201~");
    }

    #[test]
    fn queries_are_answered_and_application_keys_follow_the_program() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        browser
            .pty
            .0
            .borrow_mut()
            .output
            .extend_from_slice(b"\x1b[c\x1b[?1h");
        app.frame((40, 10));
        assert_eq!(browser.pty.0.borrow().input, b"\x1b[?62;4;22c");
        app.input(&mut browser, &press("ArrowUp", "", 0));
        assert!(browser.pty.0.borrow().input.ends_with(b"\x1bOA"));
    }

    #[test]
    fn a_flood_is_read_a_frame_at_a_time() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        browser.pty.0.borrow_mut().output = b"y\r\n".repeat(100_000);
        app.frame((40, 10));
        let left = browser.pty.0.borrow().output.len();
        assert!(
            left > 0 && left <= 300_000 - MAX_OUTPUT_PER_FRAME + 16 * 1024,
            "{left}"
        );
    }

    #[test]
    fn input_waits_while_the_program_reads_nothing() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        browser.pty.0.borrow_mut().room = Some(2);
        app.input(&mut browser, &paste_of("abcdef"));
        assert_eq!(browser.pty.0.borrow().input, b"ab");
        app.frame((40, 10));
        app.frame((40, 10));
        assert_eq!(browser.pty.0.borrow().input, b"abcdef");
    }

    #[test]
    fn the_grid_follows_the_page() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        app.frame((100, 30));
        assert_eq!(browser.pty.0.borrow().sizes, [(100, 30)]);
        assert_eq!(app.term.screen().size(), (30, 100));
    }

    #[test]
    fn the_end_is_said_and_enter_starts_again() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        {
            let mut fake = browser.pty.0.borrow_mut();
            fake.output.extend_from_slice(b"bye\r\n");
            fake.ended = true;
            fake.status = Some(Status::EndOfFile);
        }
        app.frame((40, 10));
        let text = shown(&app);
        assert!(
            text.contains("bye") && text.contains("closed the terminal"),
            "{text}"
        );
        browser.pty.0.borrow_mut().status = Some(Status::Exited(3));
        app.frame((40, 10));
        assert!(app.take_dirty());
        assert!(shown(&app).contains("exited with status 3. Press Enter"));
        // Typing does nothing, nor Enter held since the program ran; Enter pressed afresh
        // closes the ended terminal and opens a new one.
        app.input(&mut browser, &press("x", "x", 0));
        app.input(&mut browser, &repeat("Enter", "\r"));
        assert_eq!(browser.opened.len(), 1);
        app.input(&mut browser, &press("Enter", "\r", 0));
        assert_eq!(browser.opened.len(), 2);
        assert!(
            matches!(app.phase, Phase::Opening(_)),
            "refused: a terminal still held"
        );
    }

    /// A pointer record of `kind` for `code` at cell (`col`, `row`) of [`M`]'s cells.
    fn pointer(kind: InputKind, code: u32, col: u16, row: u16, modifiers: u8) -> Input {
        Input {
            kind,
            code,
            modifiers,
            x: f32::from(col) * M.cell_w + 1.0,
            y: f32::from(row) * M.cell_h + 1.0,
            ..Input::default()
        }
    }

    /// `notches` of a wheel (positive down).
    fn wheel(notches: f32) -> Input {
        Input {
            dy: notches,
            ..pointer(InputKind::Wheel, 1, 1, 1, 0)
        }
    }

    fn output(browser: &Browser, app: &mut App<Pty>, bytes: &[u8]) {
        browser.pty.0.borrow_mut().output.extend_from_slice(bytes);
        app.frame((40, 10));
    }

    fn typed(browser: &Browser) -> String {
        String::from_utf8_lossy(&std::mem::take(&mut browser.pty.0.borrow_mut().input)).into_owned()
    }

    #[test]
    fn nothing_starts_on_a_click_or_the_wheel() {
        let mut browser = Browser::default();
        let mut app = App::<Pty>::new(Program::Shell);
        app.set_metrics(M);
        app.frame((40, 10));
        for button in [buttons::LEFT, buttons::MIDDLE, buttons::RIGHT] {
            app.input(
                &mut browser,
                &pointer(InputKind::PointerPress, button, 1, 1, 0),
            );
            app.input(
                &mut browser,
                &pointer(InputKind::PointerRelease, button, 1, 1, 0),
            );
        }
        app.input(&mut browser, &wheel(1.0));
        assert!(browser.opened.is_empty());
        assert!(app.take_asks().is_empty(), "no paste on the start screen");
    }

    #[test]
    fn a_drag_selects_and_copies_to_the_primary_selection() {
        use InputKind::{Motion, PointerPress, PointerRelease};
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        output(&browser, &mut app, b"hello world\r\nsecond line");
        // A click alone selects nothing.
        app.input(&mut browser, &pointer(PointerPress, buttons::LEFT, 0, 0, 0));
        app.input(
            &mut browser,
            &pointer(PointerRelease, buttons::LEFT, 0, 0, 0),
        );
        assert!(app.take_asks().is_empty());
        app.advance_clock(1000);
        app.input(&mut browser, &pointer(PointerPress, buttons::LEFT, 6, 0, 0));
        app.input(&mut browser, &pointer(Motion, 1, 3, 1, 0));
        app.input(
            &mut browser,
            &pointer(PointerRelease, buttons::LEFT, 3, 1, 0),
        );
        let copy = |text: &str, primary| Ask::Copy {
            text: text.into(),
            primary,
        };
        assert_eq!(app.take_asks(), [copy("world\nseco", true)]);
        assert!(typed(&browser).is_empty(), "the program gets nothing");
        // Selected cells are drawn inverse.
        assert!(app
            .ops(M)
            .iter()
            .any(|op| matches!(op, Op::Fill { x, y, color, .. }
            if *x == 36.0 && *y == 0.0 && *color == draw::FOREGROUND)));
        // Ctrl+Shift+C puts it on the clipboard; typing ends it.
        app.input(&mut browser, &press("C", "", CONTROL | SHIFT));
        assert_eq!(app.take_asks(), [copy("world\nseco", false)]);
        assert!(typed(&browser).is_empty(), "not a Ctrl+C");
        app.input(&mut browser, &press("x", "x", 0));
        app.input(&mut browser, &press("C", "", CONTROL | SHIFT));
        assert!(app.take_asks().is_empty());
        assert_eq!(typed(&browser), "x");
    }

    #[test]
    fn double_and_triple_clicks_select_a_word_and_a_line() {
        use InputKind::{PointerPress, PointerRelease};
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        output(&browser, &mut app, b"ls ~/my-dir/a.txt now");
        let click = |app: &mut App<Pty>, browser: &mut Browser| {
            app.input(browser, &pointer(PointerPress, buttons::LEFT, 8, 0, 0));
            app.input(browser, &pointer(PointerRelease, buttons::LEFT, 8, 0, 0));
            app.advance_clock(100);
        };
        click(&mut app, &mut browser);
        click(&mut app, &mut browser);
        let copied = |app: &mut App<Pty>| match app.take_asks().pop() {
            Some(Ask::Copy { text, .. }) => text,
            other => panic!("{other:?}"),
        };
        assert_eq!(copied(&mut app), "~/my-dir/a.txt");
        click(&mut app, &mut browser);
        assert_eq!(copied(&mut app), "ls ~/my-dir/a.txt now");
    }

    #[test]
    fn the_program_gets_the_mouse_when_it_asks_but_shift_is_the_users() {
        use InputKind::{Motion, PointerPress, PointerRelease};
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        output(&browser, &mut app, b"\x1b[?1002h\x1b[?1006h");
        app.input(&mut browser, &pointer(PointerPress, buttons::LEFT, 2, 1, 0));
        app.input(&mut browser, &pointer(Motion, 1, 2, 1, 0));
        app.input(&mut browser, &pointer(Motion, 1, 5, 3, 0));
        app.input(
            &mut browser,
            &pointer(PointerRelease, buttons::LEFT, 5, 3, 0),
        );
        assert_eq!(
            typed(&browser),
            "\x1b[<0;3;2M\x1b[<32;6;4M\x1b[<0;6;4m",
            "motion within a cell is not reported again"
        );
        app.input(&mut browser, &wheel(-1.0));
        app.input(&mut browser, &wheel(2.0));
        assert_eq!(typed(&browser), "\x1b[<64;2;2M\x1b[<65;2;2M\x1b[<65;2;2M");
        // Motion with no button is not reported in mode 1002.
        app.input(&mut browser, &pointer(Motion, 0, 9, 9, 0));
        assert!(typed(&browser).is_empty());
        // With Shift, the pointer selects and the program hears nothing.
        output(&browser, &mut app, b"\x1b[Hselect me");
        app.input(
            &mut browser,
            &pointer(PointerPress, buttons::LEFT, 0, 0, SHIFT),
        );
        app.input(&mut browser, &pointer(Motion, 1, 5, 0, SHIFT));
        app.input(
            &mut browser,
            &pointer(PointerRelease, buttons::LEFT, 5, 0, SHIFT),
        );
        assert!(typed(&browser).is_empty());
        assert!(
            matches!(&app.take_asks()[..], [Ask::Copy { text, primary: true }] if text == "select")
        );
        // The original encoding, after the program switched SGR off.
        output(&browser, &mut app, b"\x1b[?1006l\x1b[?1000h");
        app.input(
            &mut browser,
            &pointer(PointerPress, buttons::RIGHT, 0, 0, 0),
        );
        assert_eq!(typed(&browser), "\x1b[M\"!!");
    }

    #[test]
    fn the_wheel_and_shift_page_up_look_back_and_typing_returns() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        let lines: Vec<u8> = (0..40)
            .flat_map(|i| format!("{i}\r\n").into_bytes())
            .collect();
        output(&browser, &mut app, &lines);
        app.input(&mut browser, &wheel(-1.0));
        assert_eq!(app.term.view(), 3);
        app.frame((40, 10));
        let tag = app
            .ops(M)
            .into_iter()
            .any(|op| matches!(op, Op::Text { text, .. } if text.starts_with("3/")));
        assert!(tag, "the view says how far back it is");
        app.input(&mut browser, &press("PageUp", "", SHIFT));
        assert_eq!(app.term.view(), 12);
        app.input(&mut browser, &press("PageDown", "", SHIFT));
        assert_eq!(app.term.view(), 3);
        assert!(typed(&browser).is_empty());
        // Output keeps the view still; typing returns to the live screen.
        output(&browser, &mut app, b"more\r\n");
        assert_eq!(app.term.view(), 4);
        app.input(&mut browser, &press("a", "a", 0));
        assert_eq!(app.term.view(), 0);
        // On the alternate screen the wheel is arrow keys, Shift+Page Up the program's.
        output(&browser, &mut app, b"\x1b[?1049h");
        typed(&browser);
        app.input(&mut browser, &wheel(1.0));
        app.input(&mut browser, &press("PageUp", "", SHIFT));
        assert_eq!(typed(&browser), "\x1b[B\x1b[B\x1b[B\x1b[5;2~");
    }

    #[test]
    fn pastes_come_from_the_browsers_clipboard() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        app.input(&mut browser, &press("V", "", CONTROL | SHIFT));
        app.input(&mut browser, &press("Insert", "", SHIFT));
        app.input(
            &mut browser,
            &pointer(InputKind::PointerPress, buttons::MIDDLE, 0, 0, 0),
        );
        assert_eq!(
            app.take_asks(),
            [
                Ask::Paste { primary: false },
                Ask::Paste { primary: true },
                Ask::Paste { primary: true }
            ]
        );
        assert!(typed(&browser).is_empty());
        output(&browser, &mut app, b"\x1b[?2004h");
        app.pasted("echo \x1b[201~hi\n");
        assert_eq!(typed(&browser), "\x1b[200~echo [201~hi\r\x1b[201~");
    }

    #[test]
    fn only_the_users_own_presses_ask_the_clipboard() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        // Whatever the program prints: OSC 52 reads and writes, the bytes of the paste keys,
        // mouse reports enabled.
        output(
            &browser,
            &mut app,
            b"\x1b]52;c;?\x07\x1b]52;p;?\x1b\\\x1b]52;c;aGk=\x07\x16\x1b[2~\x1b[?1000h",
        );
        assert!(app.take_asks().is_empty());
        // Held keys repeat, and a repeat is no fresh action.
        app.input(
            &mut browser,
            &Input {
                kind: InputKind::Repeat,
                ..press("V", "", CONTROL | SHIFT)
            },
        );
        app.input(
            &mut browser,
            &Input {
                kind: InputKind::Repeat,
                ..press("Insert", "", SHIFT)
            },
        );
        assert!(app.take_asks().is_empty());
        // The program has the mouse: a middle click is its own, not a paste.
        app.input(
            &mut browser,
            &pointer(InputKind::PointerPress, buttons::MIDDLE, 0, 0, 0),
        );
        assert!(app.take_asks().is_empty());
        assert_eq!(typed(&browser), "\x1b[M!!!");
    }

    #[test]
    fn images_are_drawn_where_they_are_placed_and_scroll_with_the_view() {
        let mut browser = Browser::default();
        let mut app = running(&mut browser);
        output(
            &browser,
            &mut app,
            b"\r\n\x1b_Ga=T,f=24,s=2,v=2,i=1,z=-1;AAAAAAAAAAAAAAAA\x1b\\",
        );
        let image = |app: &App<Pty>| {
            app.ops(M).into_iter().find_map(|op| match op {
                Op::Image { rect, .. } => Some(rect),
                _ => None,
            })
        };
        for _ in 0..10 {
            app.frame((40, 10));
        }
        assert_eq!(image(&app), Some([0.0, 12.0, 2.0, 2.0]));
        // Below the text: before the first text drawn.
        let lines: Vec<u8> = (0..12)
            .flat_map(|i| format!("{i}\r\n").into_bytes())
            .collect();
        output(&browser, &mut app, &lines);
        assert_eq!(image(&app), None, "scrolled off the page");
        app.input(&mut browser, &wheel(-2.0));
        app.frame((40, 10));
        assert!(image(&app).is_some(), "back in view");
    }

    /// What the browser answers about the editor, as `PageInfo::Editor` documents it.
    const EDITOR_INFO: &str = "enabled\tyes\neditor\t/usr/bin/nvim\nfrom\tPATH\n\
                               file\t/home/u/.config/sighurt/config.toml\nnew\tno\n";

    #[test]
    fn sig_config_says_what_it_runs_and_that_it_has_the_users_rights() {
        let mut browser = Browser::default();
        let mut app = App::<Pty>::new(Program::EditConfig);
        app.frame((200, 50));
        assert!(app.take_wants_info(), "it asks at the start");
        assert!(!app.take_wants_info());
        // Before the answer: the rule.
        assert!(shown(&app).contains("else $VISUAL, else $EDITOR"));
        app.editor_info(EDITOR_INFO);
        let text = shown(&app);
        assert!(text.contains("sig://config"), "{text}");
        assert!(
            text.contains("/usr/bin/nvim /home/u/.config/sighurt/config.toml"),
            "{text}"
        );
        assert!(text.contains("found on PATH"), "{text}");
        assert!(
            text.contains("full rights") && text.contains("run shell"),
            "{text}"
        );
        assert!(!text.contains("ctrl-"), "no key named: {text}");
        assert!(browser.opened.is_empty());
        app.input(&mut browser, &press("Enter", "\r", 0));
        assert_eq!(browser.opened, [(Program::EditConfig, 200, 50)]);
    }

    #[test]
    fn sig_config_says_when_the_file_is_new_off_or_without_an_editor() {
        let mut app = App::<Pty>::new(Program::EditConfig);
        app.frame((200, 50));
        app.editor_info(&EDITOR_INFO.replace("new\tno", "new\tyes"));
        assert!(shown(&app).contains("does not exist yet"));
        app.editor_info("enabled\tno\neditor\tvi\nfrom\tVISUAL\nfile\t/c.toml\nnew\tno\n");
        let text = shown(&app);
        assert!(text.contains("sig://config is off."), "{text}");
        assert!(text.contains("[pages.config] enabled = false"), "{text}");
        app.editor_info("enabled\tyes\neditor\t\nfrom\tnone\nfile\t/c.toml\nnew\tno\n");
        let text = shown(&app);
        assert!(text.contains("No editor found."), "{text}");
        assert!(
            text.contains("[pages.config] editor") && text.contains("$VISUAL"),
            "{text}"
        );
    }

    #[test]
    fn when_the_editor_ends_the_page_asks_again_and_shows_the_files_problems() {
        let mut browser = Browser::default();
        let mut app = App::<Pty>::new(Program::EditConfig);
        app.frame((200, 50));
        app.take_wants_info();
        app.editor_info(EDITOR_INFO);
        app.input(&mut browser, &press("Enter", "\r", 0));
        app.frame((200, 50));
        {
            let mut fake = browser.pty.0.borrow_mut();
            fake.output.extend_from_slice(b"\x1b[?1049h~\r\n~");
            fake.ended = true;
            fake.status = Some(Status::Exited(0));
        }
        app.frame((200, 50));
        assert!(app.take_wants_info(), "the file may have changed");
        app.editor_info(&format!(
            "{EDITOR_INFO}problem\t[ui] zoom: expected a number\n"
        ));
        let text = shown(&app);
        assert!(
            text.contains("The editor exited with status 0. sig read the file again."),
            "{text}"
        );
        assert!(
            text.contains("sig keeps the configuration it had"),
            "{text}"
        );
        assert!(text.contains("[ui] zoom: expected a number"), "{text}");
        assert!(text.contains("Press Enter to start it again."), "{text}");
        // The shell's page never asks.
        let mut shell = App::<Pty>::new(Program::Shell);
        assert!(!shell.take_wants_info());
    }

    #[test]
    fn editor_info_parses_the_records() {
        let info = EditorInfo::parse(&format!("{EDITOR_INFO}problem\ta\nproblem\tb\nlater\tx\n"));
        assert_eq!(
            info,
            EditorInfo {
                enabled: true,
                editor: "/usr/bin/nvim".into(),
                from: "PATH".into(),
                file: "/home/u/.config/sighurt/config.toml".into(),
                new: false,
                problems: vec!["a".into(), "b".into()],
            }
        );
    }
}
