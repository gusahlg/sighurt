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
//! Owns: the page's logic. Must not own: the `sig` calls (`crate::page` gives them through
//! [`Host`] and [`Terminal`]) or the drawing ([`crate::draw`]).

use sig_page_kit::sig::{Input, InputKind, Program, Status};

use crate::draw::{self, Line, Metrics, Op};
use crate::keys::{self, Modes};
use crate::paste;
use crate::term::Term;

/// Most bytes of output one frame parses.
pub const MAX_OUTPUT_PER_FRAME: usize = 64 * 1024;
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

pub struct App<P> {
    program: Program,
    phase: Phase<P>,
    term: Term,
    /// Columns and rows.
    size: (u16, u16),
    pending: Vec<u8>,
    dirty: bool,
}

impl<P: Terminal> App<P> {
    pub fn new(program: Program) -> Self {
        Self {
            program,
            phase: Phase::Start { refused: false },
            term: Term::new(24, 80),
            size: (80, 24),
            pending: Vec::new(),
            dirty: true,
        }
    }

    /// Whether something changed since the last call, and the page must be drawn again.
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// One record of the browser's input queue.
    pub fn input(&mut self, host: &mut impl Host<Pty = P>, input: &Input) {
        match &self.phase {
            Phase::Start { .. } | Phase::Ended(..) => {
                if input.is_key("Enter", 0) {
                    self.start(host);
                }
            }
            Phase::Opening(_) => {}
            Phase::Running(_) => {
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
                };
                if let Some(bytes) = bytes {
                    self.send(&bytes);
                }
            }
        }
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
                    Phase::Ended(end(status), pty)
                }
            },
            Phase::Running(pty) => {
                let ended = self.read(&pty);
                let status = pty.status();
                if ended || !matches!(status, Status::Running | Status::Opening) {
                    self.dirty = true;
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
    }

    /// Reads and parses up to [`MAX_OUTPUT_PER_FRAME`] bytes; whether the output has ended.
    fn read(&mut self, pty: &P) -> bool {
        let mut buf = vec![0u8; 16 * 1024];
        let mut total = 0;
        while total < MAX_OUTPUT_PER_FRAME {
            match pty.read(&mut buf) {
                None => return true,
                Some(0) => break,
                Some(n) => {
                    total += n;
                    let replies = self.term.feed(&buf[..n]);
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
            Phase::Start { refused } => draw::page(&self.start_page(*refused), m, cols),
            Phase::Opening(_) => Vec::new(),
            Phase::Running(_) => draw::screen(self.term.screen(), m),
            Phase::Ended(how, _) => {
                let mut ops = draw::screen(self.term.screen(), m);
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

    fn start_page(&self, refused: bool) -> Vec<Line> {
        use draw::{ACCENT, FOREGROUND, MUTED, WARNING};
        let (what, other) = match self.program {
            Program::Shell => (
                "your shell: [pages.terminal] command if the configuration sets it, else $SHELL, \
                 else /bin/sh, in your home directory.",
                "sig://terminal/config opens your editor on sig's configuration instead.",
            ),
            Program::EditConfig => (
                "your editor ($VISUAL, else $EDITOR, else vi) on sig's configuration file, in its \
                 directory.",
                "sig://terminal opens your shell instead.",
            ),
        };
        let mut lines = vec![
            Line::new("sig://terminal", ACCENT).bold(),
            Line::new("", FOREGROUND),
            Line::new(format!("This page runs {what}"), FOREGROUND),
            Line::new("", FOREGROUND),
            Line::new(
                "The program runs with your full rights, as in any terminal: it can read and \
                 change your files and use your credentials. Only the terminal emulator, which \
                 reads what the program prints, runs in sig's Wasm sandbox. The program does not.",
                FOREGROUND,
            ),
            Line::new("", FOREGROUND),
            Line::new("Press Enter to start.", ACCENT).bold(),
            Line::new("", FOREGROUND),
            Line::new(
                "With sig's example keymap, keys reach the program in INSERT mode, which Escape \
                 enters. On this page Escape then goes to the program; ctrl-\\ ctrl-n leaves \
                 INSERT.",
                MUTED,
            ),
            Line::new(other, MUTED),
            Line::new(
                "Limits: no scrollback, no mouse, and a program in a background tab gets its \
                 output read once a second.",
                MUTED,
            ),
        ];
        if refused {
            lines.extend([
                Line::new("", FOREGROUND),
                Line::new("The terminal did not start.", WARNING).bold(),
                Line::new(
                    "It is off unless your configuration turns it on. Add these lines to sig's \
                     config.toml (sig://config edits it), then press Enter again:",
                    WARNING,
                ),
                Line::new("    [pages.terminal]", FOREGROUND),
                Line::new("    enabled = true", FOREGROUND),
                Line::new(
                    "It is also refused while eight terminals run, or when the key press did not \
                     reach this page.",
                    MUTED,
                ),
            ]);
        }
        lines
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
            code: 0,
            key: key.into(),
            text: text.into(),
        }
    }

    fn paste_of(text: &str) -> Input {
        Input {
            kind: InputKind::Paste,
            modifiers: 0,
            code: 0,
            key: String::new(),
            text: text.into(),
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
                Op::Fill { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn running(browser: &mut Browser) -> App<Pty> {
        let mut app = App::new(Program::Shell);
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
        let mut app = App::<Pty>::new(Program::EditConfig);
        app.input(&mut browser, &press("Enter", "\r", 0));
        let text = shown(&app);
        assert!(
            text.contains("[pages.terminal]") && text.contains("enabled = true"),
            "{text}"
        );
        assert!(text.contains("$VISUAL"), "{text}");
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
        assert_eq!(browser.pty.0.borrow().input, b"\x1b[?62;22c");
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
        // Typing does nothing; Enter closes the ended terminal and opens a new one.
        app.input(&mut browser, &press("x", "x", 0));
        app.input(&mut browser, &press("Enter", "\r", 0));
        assert_eq!(browser.opened.len(), 2);
        assert!(
            matches!(app.phase, Phase::Opening(_)),
            "refused: a terminal still held"
        );
    }
}
