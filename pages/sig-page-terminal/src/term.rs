//! The emulator: the program's output, which is untrusted, parsed into a screen by the `vt100`
//! crate inside the sandbox, and the answers to the few queries a program waits for.
//!
//! Only queries with a fixed answer are answered: primary device attributes (DA1, which nvim and
//! others wait for at startup), device status (DSR 5) and the cursor position (CPR, DSR 6).
//! Nothing the program sent is ever sent back: no title report, no clipboard (OSC 52), no
//! DECRQSS or terminfo echo, no colour report. The window title the program sets is kept by the
//! parser and shown nowhere. Answers are bounded per call, so a program asking in a loop gets no
//! more than [`MAX_REPLIES`] bytes back each time.
//!
//! Owns: the screen and the answers. Must not own: drawing ([`crate::draw`]) or the terminal's
//! bytes ([`crate::app`]).

/// Most bytes of answers one [`Term::feed`] gives.
pub const MAX_REPLIES: usize = 4096;

/// What the page answers to DA1: a VT220 with ANSI colour.
const DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;22c";

/// The screen and the parser that writes it.
pub struct Term {
    parser: vt100::Parser<Replies>,
}

/// The answers queued while parsing.
#[derive(Default)]
struct Replies {
    out: Vec<u8>,
}

impl Replies {
    fn push(&mut self, bytes: &[u8]) {
        if self.out.len() + bytes.len() <= MAX_REPLIES {
            self.out.extend_from_slice(bytes);
        }
    }
}

impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        i2: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        if i1.is_some() || i2.is_some() {
            return;
        }
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        match (c, first) {
            ('c', 0) => self.push(DEVICE_ATTRIBUTES),
            ('n', 5) => self.push(b"\x1b[0n"),
            ('n', 6) => {
                let (row, col) = screen.cursor_position();
                self.push(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            _ => {}
        }
    }
}

impl Term {
    /// A blank screen of `rows` by `cols` cells, with no scrollback.
    pub fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new_with_callbacks(rows, cols, 0, Replies::default()),
        }
    }

    /// Parses `bytes` of the program's output; returns the answers to send it.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        self.parser.process(bytes);
        std::mem::take(&mut self.parser.callbacks_mut().out)
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_queries_are_answered() {
        let mut term = Term::new(24, 80);
        assert_eq!(term.feed(b"\x1b[c"), DEVICE_ATTRIBUTES);
        assert_eq!(term.feed(b"\x1b[0c"), DEVICE_ATTRIBUTES);
        assert_eq!(term.feed(b"\x1b[5n"), b"\x1b[0n");
        assert_eq!(term.feed(b"\x1b[3;7H\x1b[6n"), b"\x1b[3;7R");
        assert_eq!(term.feed(b"hello"), b"");
    }

    #[test]
    fn nothing_is_reflected() {
        let mut term = Term::new(24, 80);
        for query in [
            &b"\x1b]0;my title\x07\x1b[21t"[..], // title report
            b"\x1b]52;c;?\x07",                  // clipboard read
            b"\x1b]52;c;aGk=\x07",               // clipboard write
            b"\x1bP$qm\x1b\\",                   // DECRQSS
            b"\x1bP+q544e\x1b\\",                // XTGETTCAP
            b"\x1b]11;?\x07",                    // background colour
            b"\x1b[>c",                          // DA2
            b"\x1b[>0q",                         // XTVERSION
            b"\x1b[?2026$p",                     // DECRQM
            b"\x1b[18t",                         // size report
        ] {
            assert_eq!(
                term.feed(query),
                b"",
                "{:?}",
                String::from_utf8_lossy(query)
            );
        }
    }

    #[test]
    fn a_query_flood_is_bounded() {
        let mut term = Term::new(24, 80);
        let flood = b"\x1b[6n".repeat(10_000);
        let replies = term.feed(&flood);
        assert!(replies.len() <= MAX_REPLIES);
        assert!(!replies.is_empty());
    }

    #[test]
    fn what_nvim_needs_is_on_the_screen() {
        let mut term = Term::new(5, 20);
        // Alternate screen, a scroll region, colours, attributes, insert and delete.
        term.feed(b"main");
        term.feed(
            b"\x1b[?1049h\x1b[H\x1b[2J\x1b[1;31mred\x1b[0m \x1b[38;5;208mo\x1b[38;2;1;2;3mt\x1b[m",
        );
        let screen = term.screen();
        assert!(screen.alternate_screen());
        assert_eq!(screen.cell(0, 0).unwrap().fgcolor(), vt100::Color::Idx(1));
        assert!(screen.cell(0, 0).unwrap().bold());
        assert_eq!(screen.cell(0, 4).unwrap().fgcolor(), vt100::Color::Idx(208));
        assert_eq!(
            screen.cell(0, 5).unwrap().fgcolor(),
            vt100::Color::Rgb(1, 2, 3)
        );
        // A scroll region of rows 2 to 4: line feeds at its bottom scroll only it.
        term.feed(b"\x1b[2;4r\x1b[4;1Hx\r\n\r\ny");
        let contents = term.screen().contents();
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!((lines[1], lines[3]), ("x", "y"), "{contents:?}");
        assert!(lines[0].starts_with("red"));
        term.feed(b"\x1b[1;1H\x1b[2@");
        assert!(term.screen().contents().starts_with("  red"));
        term.feed(b"\x1b[?1049l");
        assert!(term.screen().contents().starts_with("main"));
        term.feed(b"\x1b[?1h\x1b[?2004h");
        assert!(term.screen().application_cursor() && term.screen().bracketed_paste());
        term.feed("\x1b[H\x1b[2J日本\tx".as_bytes());
        let screen = term.screen();
        assert!(screen.cell(0, 0).unwrap().is_wide());
        assert_eq!(screen.cursor_position(), (0, 9), "a tab stop at 8, then x");
    }
}
