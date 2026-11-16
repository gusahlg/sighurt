//! The emulator: the program's output, which is untrusted, parsed into a screen by the `vt100`
//! crate inside the sandbox, its history, its images, and the answers to the few queries a
//! program waits for.
//!
//! The output first goes through [`crate::scan`], which takes out the graphics strings `vt100`
//! would swallow and hands them to [`crate::graphics`]; everything else reaches the parser in
//! order. Images are placed at the cursor as it is when their command arrives, which is why the
//! parser is fed up to each command first.
//!
//! *History.* Lines the main screen scrolls off its top are kept, at most [`HISTORY_BYTES`] of
//! cells (between [`MIN_HISTORY`] and [`MAX_HISTORY`] lines, from the width at the start), and
//! the user may look back through them ([`Term::scroll_view`]); new output keeps the view where
//! it is, as kitty, foot and alacritty do. The alternate screen has none. `vt100` counts no
//! scrolled lines, which images and selections need to stay with their text, so [`Term`]
//! counts them itself: it sets the history's view one line up before each piece of output and
//! reads how far `vt100` moved it (it keeps a view still by moving it one line per line
//! scrolled), then gives the user's view back. When more lines scroll in one piece than the
//! history holds, the count is the history's size: everything older is gone then anyway.
//!
//! *Answers.* Only queries with a fixed answer, or one made of the terminal's own state: primary
//! device attributes (DA1, which nvim and others wait for at startup; it says sixel), device
//! status (DSR 5), the cursor position (CPR, DSR 6), the text area's and a cell's size in pixels
//! and the size in cells (XTWINOPS 14, 16 and 18, which image programs size their pictures by),
//! and the graphics protocol's own answers. Nothing the program sent is ever sent back: no title
//! report, no clipboard (OSC 52, read or write), no DECRQSS or terminfo echo, no colour report.
//! Answers are bounded per call, so a program asking in a loop gets no more than
//! [`MAX_REPLIES`] bytes back each time.
//!
//! Owns: the screen, its history, its images and the answers. Must not own: drawing
//! ([`crate::draw`]) or the terminal's bytes ([`crate::app`]).

use crate::graphics::{Advance, Context, Graphics};
use crate::scan::{Mark, Scanner, Sink};

/// Most bytes of answers one [`Term::feed`] gives.
pub const MAX_REPLIES: usize = 4096;
/// Fewest and most lines of history.
pub const MIN_HISTORY: usize = 1000;
pub const MAX_HISTORY: usize = 10_000;
/// Most bytes of cells the history holds (a cell is 32 bytes): [`MAX_HISTORY`] lines of 80
/// columns.
pub const HISTORY_BYTES: usize = MAX_HISTORY * 80 * 32;
/// How much cheaper than a byte of text a byte of graphics data is to read, for a frame's budget.
pub const GRAPHICS_WEIGHT: usize = 8;

/// What the page answers to DA1: a VT220 with sixel graphics and ANSI colour.
const DEVICE_ATTRIBUTES: &[u8] = b"\x1b[?62;4;22c";

/// How the program asked the cursor to look (DECSCUSR).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

/// The screen and the parser that writes it.
pub struct Term {
    parser: vt100::Parser<Replies>,
    scanner: Scanner,
    graphics: Graphics,
    /// Lines the main screen scrolled into its history since the start.
    scrolled: i64,
    /// Lines of history kept.
    history: usize,
    /// Text for the parser not given to it yet.
    pending: Vec<u8>,
    /// Cost of the current [`Term::feed`].
    cost: usize,
    /// Bumped when the screen's content is replaced wholesale (a reset, the history erased,
    /// more scrolled than the history holds), so selections know they are gone.
    epoch: u64,
}

/// The answers queued while parsing, and what the parser's callbacks learn.
#[derive(Default)]
struct Replies {
    out: Vec<u8>,
    /// A cell's width and height, in pixels, for XTWINOPS.
    cell: (f32, f32),
    cursor: CursorShape,
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
        let first = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        if (i1, i2, c) == (Some(b' '), None, 'q') {
            self.cursor = match first {
                3 | 4 => CursorShape::Underline,
                5 | 6 => CursorShape::Bar,
                _ => CursorShape::Block,
            };
            return;
        }
        if i1.is_some() || i2.is_some() {
            return;
        }
        let (rows, cols) = screen.size();
        let (cw, ch) = self.cell;
        match (c, first) {
            ('c', 0) => self.push(DEVICE_ATTRIBUTES),
            ('n', 5) => self.push(b"\x1b[0n"),
            ('n', 6) => {
                let (row, col) = screen.cursor_position();
                self.push(format!("\x1b[{};{}R", row + 1, col + 1).as_bytes());
            }
            ('t', 14) => {
                let (h, w) = (f32::from(rows) * ch, f32::from(cols) * cw);
                self.push(format!("\x1b[4;{};{}t", h.round(), w.round()).as_bytes());
            }
            ('t', 16) => {
                self.push(format!("\x1b[6;{};{}t", ch.round(), cw.round()).as_bytes());
            }
            ('t', 18) => self.push(format!("\x1b[8;{rows};{cols}t").as_bytes()),
            _ => {}
        }
    }
}

/// The history's length: its view moved as far as it goes.
fn history_len(screen: &mut vt100::Screen) -> usize {
    let view = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let len = screen.scrollback();
    screen.set_scrollback(view);
    len
}

impl Term {
    /// A blank screen of `rows` by `cols` cells, with an empty history.
    pub fn new(rows: u16, cols: u16) -> Self {
        let history =
            (HISTORY_BYTES / (usize::from(cols.max(80)) * 32)).clamp(MIN_HISTORY, MAX_HISTORY);
        Self {
            parser: vt100::Parser::new_with_callbacks(rows, cols, history, Replies::default()),
            scanner: Scanner::default(),
            graphics: Graphics::default(),
            scrolled: 0,
            history,
            pending: Vec::new(),
            cost: 0,
            epoch: 0,
        }
    }

    /// Parses `bytes` of the program's output; returns the answers to send it and what parsing
    /// cost, in bytes of text (graphics data counts [`GRAPHICS_WEIGHT`] times less).
    pub fn feed(&mut self, bytes: &[u8]) -> (Vec<u8>, usize) {
        self.cost = 0;
        let mut scanner = std::mem::take(&mut self.scanner);
        scanner.scan(bytes, self);
        self.scanner = scanner;
        self.flush();
        let replies = std::mem::take(&mut self.parser.callbacks_mut().out);
        (replies, self.cost)
    }

    /// Works on images being made ready; whether one became ready to draw.
    pub fn step(&mut self) -> bool {
        self.graphics.step()
    }

    /// Whether work is left for [`Term::step`].
    pub fn busy(&self) -> bool {
        self.graphics.building()
    }

    /// Tells the emulator a cell's size in canvas pixels, which images are measured in.
    pub fn set_cell(&mut self, width: f32, height: f32) {
        self.parser.callbacks_mut().cell = (width, height);
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.parser.screen_mut().set_size(rows, cols);
    }

    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    pub fn graphics(&self) -> &Graphics {
        &self.graphics
    }

    pub fn cursor_shape(&self) -> CursorShape {
        self.parser.callbacks().cursor
    }

    /// Lines the main screen has scrolled into its history: the line of its first row.
    pub fn scrolled(&self) -> i64 {
        self.scrolled
    }

    /// Changes when what was on the screen is gone (a reset, the history erased).
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// How many lines back the user looks (0: the live screen).
    pub fn view(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// The history's length in lines.
    pub fn history(&mut self) -> usize {
        history_len(self.parser.screen_mut())
    }

    /// Looks `lines` further back into the history (forward when negative), within it; never on
    /// the alternate screen. Whether the view moved.
    pub fn scroll_view(&mut self, lines: isize) -> bool {
        let screen = self.parser.screen_mut();
        if screen.alternate_screen() {
            return false;
        }
        let view = screen.scrollback();
        screen.set_scrollback(view.saturating_add_signed(lines));
        screen.scrollback() != view
    }

    /// The line shown at the top of the page ([`Context::line`]'s lines).
    pub fn top_line(&self) -> i64 {
        let screen = self.parser.screen();
        match screen.alternate_screen() {
            true => 0,
            false => self.scrolled - screen.scrollback() as i64,
        }
    }

    /// The text from `start` to `end` (line and column, `end` excluded), lines joined by
    /// newlines unless one wrapped into the next, trailing blanks dropped, at most `max` bytes.
    pub fn text(&mut self, start: (i64, u16), end: (i64, u16), max: usize) -> String {
        let scrolled = self.scrolled;
        let screen = self.parser.screen_mut();
        let view = screen.scrollback();
        let alt = screen.alternate_screen();
        let (rows, cols) = screen.size();
        let held = history_len(screen) as i64;
        let mut out = String::new();
        for line in start.0..=end.0 {
            // The row `line` is on, with the view moved so it shows.
            let row = if alt {
                u16::try_from(line).ok().filter(|row| *row < rows)
            } else if line < scrolled {
                let depth = scrolled - line;
                (depth <= held).then(|| {
                    screen.set_scrollback(depth as usize);
                    0
                })
            } else {
                screen.set_scrollback(0);
                u16::try_from(line - scrolled)
                    .ok()
                    .filter(|row| *row < rows)
            };
            let Some(row) = row else {
                continue;
            };
            let from = if line == start.0 { start.1 } else { 0 };
            let to = if line == end.0 { end.1.min(cols) } else { cols };
            let mut text = String::new();
            for col in from..to {
                match screen.cell(row, col) {
                    Some(cell) if cell.is_wide_continuation() => {}
                    Some(cell) if cell.has_contents() => text.push_str(cell.contents()),
                    _ => text.push(' '),
                }
            }
            let wrapped = to == cols && screen.row_wrapped(row);
            out.push_str(if wrapped { &text } else { text.trim_end() });
            if line != end.0 && !wrapped {
                out.push('\n');
            }
            if out.len() >= max {
                let mut cut = max;
                while !out.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.truncate(cut);
                break;
            }
        }
        screen.set_scrollback(view);
        out
    }

    fn context(&self) -> Context {
        let screen = self.parser.screen();
        Context {
            cursor: screen.cursor_position(),
            size: screen.size(),
            alt: screen.alternate_screen(),
            scrolled: self.scrolled,
            cell: self.parser.callbacks().cell,
        }
    }

    /// Gives the parser the text waiting for it.
    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        self.process(&pending);
        self.pending = pending;
        self.pending.clear();
    }

    /// Parses `bytes`, counting the lines the main screen scrolls into its history and keeping
    /// the user's view of it where it was.
    fn process(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let screen = self.parser.screen_mut();
        if screen.alternate_screen() {
            self.parser.process(bytes);
            return;
        }
        let view = screen.scrollback();
        let before = history_len(screen);
        // vt100 moves a view that is not at the bottom one line up for each line scrolled.
        screen.set_scrollback(1);
        self.parser.process(bytes);
        let screen = self.parser.screen_mut();
        let counted = if before == 0 {
            history_len(screen)
        } else {
            screen.scrollback().saturating_sub(1)
        };
        // Past the history's size the count stops: everything older is gone then anyway.
        let full = match before {
            0 => counted >= self.history,
            _ => counted + 1 >= self.history,
        };
        let scrolled = if full { self.history } else { counted };
        screen.set_scrollback(match view {
            0 => 0,
            view => view + scrolled,
        });
        if scrolled > 0 {
            self.scrolled += scrolled as i64;
            let held = history_len(screen);
            self.graphics.scrolled(self.scrolled, held);
            if full {
                self.epoch += 1;
            }
        }
    }

    /// Parses `bytes` as they are, with no counting: a switch of screens, which scrolls nothing
    /// (the other screen's view is unreadable from here).
    fn process_plain(&mut self, bytes: &[u8]) {
        let screen = self.parser.screen_mut();
        if !screen.alternate_screen() {
            // Entering the alternate screen: the main one is live again when it comes back.
            screen.set_scrollback(0);
        }
        self.parser.process(bytes);
    }

    /// Moves the cursor past an image placed at it.
    fn advance(&mut self, advance: Advance) {
        let (_, cols) = self.parser.screen().size();
        let (_, col) = self.parser.screen().cursor_position();
        let bytes = match advance {
            Advance::Kitty { cols: n, rows } => {
                let mut bytes = b"\n".repeat(usize::from(rows.saturating_sub(1)));
                if u32::from(col) + u32::from(n) >= u32::from(cols) {
                    bytes.extend_from_slice(b"\r\n");
                } else {
                    bytes.extend_from_slice(format!("\x1b[{n}C").as_bytes());
                }
                bytes
            }
            Advance::Below { rows } => {
                let mut bytes = b"\r".to_vec();
                bytes.extend(b"\n".repeat(usize::from(rows)));
                bytes
            }
        };
        self.process(&bytes);
    }

    fn reply(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.parser.callbacks_mut().push(bytes);
        }
    }
}

impl Sink for Term {
    fn text(&mut self, bytes: &[u8]) {
        self.cost += bytes.len();
        self.pending.extend_from_slice(bytes);
    }

    fn mark(&mut self, mark: Mark) {
        match mark {
            Mark::Switch { len } => {
                let pending = std::mem::take(&mut self.pending);
                let split = pending.len().saturating_sub(len);
                self.process(&pending[..split]);
                let alt = self.parser.screen().alternate_screen();
                self.process_plain(&pending[split..]);
                if alt && !self.parser.screen().alternate_screen() {
                    self.graphics.left_alternate();
                }
                self.pending = pending;
                self.pending.clear();
            }
            Mark::Erase(n) => {
                self.flush();
                let ctx = self.context();
                self.graphics.erased(&ctx, n == 3);
                if n == 3 {
                    self.epoch += 1;
                }
            }
            Mark::Reset => {
                self.flush();
                self.graphics.reset();
                self.epoch += 1;
            }
        }
    }

    fn kitty_begin(&mut self, control: &[u8]) {
        self.flush();
        self.cost += control.len();
        self.graphics.kitty_begin(control);
    }

    fn kitty_data(&mut self, data: &[u8]) {
        self.cost += data.len().div_ceil(GRAPHICS_WEIGHT);
        self.graphics.kitty_data(data);
    }

    fn kitty_end(&mut self, complete: bool) {
        let ctx = self.context();
        let outcome = self.graphics.kitty_end(complete, &ctx);
        self.reply(&outcome.reply);
        if let Some(advance) = outcome.advance {
            self.advance(advance);
        }
    }

    fn sixel_begin(&mut self, params: &[u16]) {
        self.flush();
        self.graphics.sixel_begin(params);
    }

    fn sixel_data(&mut self, data: &[u8]) {
        self.cost += data.len().div_ceil(GRAPHICS_WEIGHT);
        self.graphics.sixel_data(data);
    }

    fn sixel_end(&mut self, complete: bool) {
        let ctx = self.context();
        let outcome = self.graphics.sixel_end(complete, &ctx);
        if let Some(advance) = outcome.advance {
            self.advance(advance);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term() -> Term {
        let mut term = Term::new(24, 80);
        term.set_cell(9.0, 18.0);
        term
    }

    #[test]
    fn fixed_queries_are_answered() {
        let mut term = term();
        let feed = |term: &mut Term, bytes: &[u8]| term.feed(bytes).0;
        assert_eq!(feed(&mut term, b"\x1b[c"), DEVICE_ATTRIBUTES);
        assert_eq!(feed(&mut term, b"\x1b[0c"), DEVICE_ATTRIBUTES);
        assert_eq!(feed(&mut term, b"\x1b[5n"), b"\x1b[0n");
        assert_eq!(feed(&mut term, b"\x1b[3;7H\x1b[6n"), b"\x1b[3;7R");
        assert_eq!(feed(&mut term, b"hello"), b"");
        // Sizes: the text area in pixels, a cell in pixels, the grid in cells.
        assert_eq!(feed(&mut term, b"\x1b[14t"), b"\x1b[4;432;720t");
        assert_eq!(feed(&mut term, b"\x1b[16t"), b"\x1b[6;18;9t");
        assert_eq!(feed(&mut term, b"\x1b[18t"), b"\x1b[8;24;80t");
    }

    #[test]
    fn nothing_is_reflected() {
        let mut term = term();
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
            b"\x1b_Ga=q;AAAA\x1b\\",             // a graphics query without an id
        ] {
            assert_eq!(
                term.feed(query).0,
                b"",
                "{:?}",
                String::from_utf8_lossy(query)
            );
        }
    }

    #[test]
    fn a_query_flood_is_bounded() {
        let mut term = term();
        let flood = b"\x1b[6n".repeat(10_000);
        let replies = term.feed(&flood).0;
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
        // The cursor's shape.
        term.feed(b"\x1b[6 q");
        assert_eq!(term.cursor_shape(), CursorShape::Bar);
        term.feed(b"\x1b[4 q");
        assert_eq!(term.cursor_shape(), CursorShape::Underline);
        term.feed(b"\x1b[0 q");
        assert_eq!(term.cursor_shape(), CursorShape::Block);
    }

    /// `n` numbered lines.
    fn lines(n: usize) -> Vec<u8> {
        (0..n)
            .flat_map(|i| format!("line {i}\r\n").into_bytes())
            .collect()
    }

    #[test]
    fn scrolled_lines_are_counted_and_kept() {
        let mut term = Term::new(5, 20);
        term.feed(&lines(3));
        assert_eq!((term.scrolled(), term.history()), (0, 0));
        term.feed(&lines(10));
        // 13 lines and the cursor's empty one on 5 rows: 9 scrolled.
        assert_eq!((term.scrolled(), term.history()), (9, 9));
        // In pieces, the count is the same.
        let mut pieces = Term::new(5, 20);
        for chunk in [&lines(3)[..], &lines(10)[..]].concat().chunks(7) {
            pieces.feed(chunk);
        }
        assert_eq!(pieces.scrolled(), 9);
        // The history is read back by line.
        assert_eq!(term.text((0, 0), (0, 20), 100), "line 0");
        assert_eq!(term.text((8, 0), (9, 20), 100), "line 5\nline 6");
        // A scroll region scrolls nothing into the history.
        term.feed(b"\x1b[2;4r\x1b[4;1H\n\n\n\x1b[r");
        assert_eq!(term.scrolled(), 9);
        // Nor does the alternate screen.
        term.feed(b"\x1b[?1049h");
        term.feed(&lines(20));
        term.feed(b"\x1b[?1049l");
        assert_eq!(term.scrolled(), 9);
    }

    #[test]
    fn the_view_stays_where_the_user_looks_while_output_comes() {
        let mut term = Term::new(5, 20);
        term.feed(&lines(20));
        assert!(term.scroll_view(3));
        assert_eq!(term.view(), 3);
        let top = term.top_line();
        term.feed(&lines(4));
        assert_eq!(term.view(), 7, "moved with the output");
        assert_eq!(term.top_line(), top, "showing the same lines");
        assert!(term.scroll_view(-100));
        assert_eq!(term.view(), 0);
        assert!(!term.scroll_view(-1), "the live screen is the bottom");
        // More than the history holds: the view is at its top, and the count says the size.
        let mut small = Term::new(3, 20);
        let history = small.history;
        small.feed(&lines(history * 3));
        assert!(small.scrolled() >= history as i64);
        assert!(small.history() <= history);
    }

    /// A kitty command transmitting and showing a `w` × `h` RGB image with these keys.
    fn rgb(w: u32, h: u32, keys: &str) -> Vec<u8> {
        let data = vec![200u8; (w * h * 3) as usize];
        let mut out = format!("\x1b_Ga=T,f=24,s={w},v={h}{keys};").into_bytes();
        out.extend_from_slice(base64(&data).as_bytes());
        out.extend_from_slice(b"\x1b\\");
        out
    }

    fn base64(data: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in data.chunks(3) {
            let b = [
                chunk[0],
                *chunk.get(1).unwrap_or(&0),
                *chunk.get(2).unwrap_or(&0),
            ];
            let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    /// Steps until images are ready.
    fn ready(term: &mut Term) {
        for _ in 0..1000 {
            if !term.busy() {
                return;
            }
            term.step();
        }
        panic!("never ready");
    }

    #[test]
    fn the_graphics_query_is_answered_before_the_device_attributes() {
        let mut term = term();
        let (replies, _) = term.feed(b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c");
        assert_eq!(replies, b"\x1b_Gi=31;OK\x1b\\\x1b[?62;4;22c");
        // Files and shared memory are refused, so a program falls back to sending the data.
        let (replies, _) = term.feed(b"\x1b_Gi=32,s=1,v=1,a=q,t=f,f=24;L3RtcC94\x1b\\");
        assert!(replies.starts_with(b"\x1b_Gi=32;EINVAL"), "{replies:?}");
        // Quiet: no OK with q=1, nothing at all with q=2.
        assert_eq!(
            term.feed(b"\x1b_Gi=33,s=1,v=1,a=q,q=1,f=24;AAAA\x1b\\").0,
            b""
        );
        assert_eq!(term.feed(b"\x1b_Gi=34,a=q,q=2,t=f;AAAA\x1b\\").0, b"");
    }

    #[test]
    fn an_image_is_placed_at_the_cursor_and_moves_it() {
        let mut term = term();
        term.feed(b"ab");
        // 20 x 40 pixels at 9 x 18 per cell: 3 columns and 3 rows.
        let (replies, _) = term.feed(&rgb(20, 40, ",i=7"));
        assert_eq!(replies, b"\x1b_Gi=7;OK\x1b\\");
        let placements = term.graphics().placements().to_vec();
        assert_eq!(placements.len(), 1);
        let p = &placements[0];
        assert_eq!((p.line, p.col, p.cols, p.rows), (0, 2, 3, 3));
        assert_eq!(term.screen().cursor_position(), (2, 5));
        assert!(term.graphics().png(p.image).is_none(), "not made yet");
        ready(&mut term);
        let png = term.graphics().png(p.image).unwrap();
        assert_eq!(crate::png::dimensions(png), Some((20, 40)));
        // C=1 leaves the cursor; c and r give the size in cells.
        term.feed(&rgb(4, 4, ",i=8,c=10,r=2,C=1"));
        let p = term.graphics().placements()[1].clone();
        assert_eq!((p.cols, p.rows, p.size), (10, 2, Some((90.0, 36.0))));
        assert_eq!(term.screen().cursor_position(), (2, 5));
    }

    #[test]
    fn placements_scroll_with_the_text_and_leave_with_it() {
        let mut term = Term::new(5, 20);
        term.set_cell(9.0, 18.0);
        term.feed(&rgb(9, 18, ",i=1"));
        let line = term.graphics().placements()[0].line;
        term.feed(&lines(3));
        assert_eq!(
            term.graphics().placements()[0].line,
            line,
            "anchored to its line"
        );
        assert_eq!(term.top_line(), 0);
        term.feed(&lines(10));
        assert!(term.top_line() > line, "it is in the history now");
        assert_eq!(term.graphics().placements().len(), 1);
        let history = term.history;
        term.feed(&lines(history + 10));
        assert!(
            term.graphics().placements().is_empty(),
            "gone with its line"
        );
    }

    #[test]
    fn chunks_deletes_and_puts() {
        let mut term = term();
        let data = base64(&[9u8; 2 * 2 * 4]);
        let (first, rest) = data.split_at(8);
        term.feed(format!("\x1b_Ga=t,f=32,s=2,v=2,i=5,m=1;{first}\x1b\\").as_bytes());
        term.feed(b"text between chunks");
        let (replies, _) = term.feed(format!("\x1b_Gm=0;{rest}\x1b\\").as_bytes());
        assert_eq!(replies, b"\x1b_Gi=5;OK\x1b\\");
        assert!(term.graphics().placements().is_empty(), "a=t shows nothing");
        let (replies, _) = term.feed(b"\x1b_Ga=p,i=5,p=2\x1b\\\x1b_Ga=p,i=5,p=2\x1b\\");
        assert_eq!(replies, b"\x1b_Gi=5,p=2;OK\x1b\\\x1b_Gi=5,p=2;OK\x1b\\");
        assert_eq!(
            term.graphics().placements().len(),
            1,
            "the same placement id replaces"
        );
        let (replies, _) = term.feed(b"\x1b_Ga=p,i=99\x1b\\");
        assert!(replies.starts_with(b"\x1b_Gi=99;ENOENT"));
        // A lower-case delete keeps the image, an upper-case one frees it.
        term.feed(b"\x1b_Ga=d,d=i,i=5\x1b\\");
        assert!(term.graphics().placements().is_empty());
        assert_eq!(term.feed(b"\x1b_Ga=p,i=5\x1b\\").0, b"\x1b_Gi=5;OK\x1b\\");
        term.feed(b"\x1b_Ga=d,d=I,i=5\x1b\\");
        assert!(term
            .feed(b"\x1b_Ga=p,i=5\x1b\\")
            .0
            .starts_with(b"\x1b_Gi=5;ENOENT"));
        // A number without an id gets one, and the answer names both.
        let (replies, _) = term.feed(&rgb(1, 1, ",I=13"));
        let replies = String::from_utf8(replies).unwrap();
        assert!(replies.contains(",I=13;OK"), "{replies}");
    }

    #[test]
    fn bad_data_is_refused_and_bounded() {
        let mut term = term();
        // Too little data for s and v.
        let (replies, _) = term.feed(b"\x1b_Ga=T,f=24,s=10,v=10,i=1;AAAA\x1b\\");
        assert!(replies.starts_with(b"\x1b_Gi=1;ENODATA"), "{replies:?}");
        // Too large.
        let (replies, _) = term.feed(b"\x1b_Ga=T,f=32,s=5000,v=5000,i=2;AAAA\x1b\\");
        assert!(replies.starts_with(b"\x1b_Gi=2;EFBIG"), "{replies:?}");
        // Not base64, not a PNG, compressed, animation, placeholders.
        for (command, error) in [
            (
                &b"\x1b_Ga=T,f=24,s=1,v=1,i=3;!!!!\x1b\\"[..],
                &b"EINVAL"[..],
            ),
            (b"\x1b_Ga=T,f=100,i=4;AAAA\x1b\\", b"EINVAL"),
            (b"\x1b_Ga=T,f=24,s=1,v=1,o=z,i=5;AAAA\x1b\\", b"EINVAL"),
            (b"\x1b_Ga=f,i=6;AAAA\x1b\\", b"EINVAL"),
            (b"\x1b_Ga=T,U=1,f=24,s=1,v=1,i=7;AAAA\x1b\\", b"EINVAL"),
        ] {
            let (replies, _) = term.feed(command);
            let at = replies.iter().position(|b| *b == b';').unwrap() + 1;
            assert!(
                replies[at..].starts_with(error),
                "{:?}",
                String::from_utf8_lossy(&replies)
            );
        }
        assert!(term.graphics().is_empty());
        // Many images: the bounds hold, the oldest going first.
        for i in 0..crate::graphics::MAX_IMAGES + 20 {
            term.feed(&rgb(1, 1, &format!(",i={},a=t", i + 100)));
        }
        assert!(term.graphics().stored() <= crate::graphics::MAX_STORED);
        let answer = term.feed(b"\x1b_Ga=p,i=100\x1b\\").0;
        assert!(answer.starts_with(b"\x1b_Gi=100;ENOENT"), "evicted");
    }

    #[test]
    fn erasing_the_display_and_leaving_the_alternate_screen_remove_images() {
        let mut term = term();
        term.feed(&rgb(9, 18, ",i=1"));
        term.feed(b"\x1b[2J");
        assert!(term.graphics().placements().is_empty());
        term.feed(b"\x1b[?1049h");
        term.feed(&rgb(9, 18, ",i=2"));
        assert!(term.graphics().placements()[0].alt);
        term.feed(b"\x1b[?1049l");
        assert!(term.graphics().placements().is_empty());
        term.feed(&rgb(9, 18, ",i=3"));
        term.feed(b"\x1bc");
        assert!(term.graphics().is_empty());
    }

    #[test]
    fn a_sixel_image_is_placed_and_the_cursor_goes_below_it() {
        let mut term = term();
        term.feed(b"x\r\n");
        // Two bands, 12 pixels tall: one row of 18-pixel cells.
        term.feed(b"\x1bPq#1;2;100;0;0#1~~~~-~~~~\x1b\\");
        let p = term.graphics().placements()[0].clone();
        assert_eq!((p.line, p.col, p.rows), (1, 0, 1));
        assert_eq!(term.screen().cursor_position(), (2, 0));
        ready(&mut term);
        let png = term.graphics().png(p.image).unwrap();
        assert_eq!(crate::png::dimensions(png), Some((4, 12)));
    }

    #[test]
    fn graphics_data_costs_less_than_text() {
        let mut term = term();
        let command = rgb(64, 64, ",i=1");
        let (_, cost) = term.feed(&command);
        assert!(cost < command.len() / 4, "{cost} of {}", command.len());
        let (_, cost) = term.feed(&b"y\r\n".repeat(1000));
        assert_eq!(cost, 3000);
    }
}
