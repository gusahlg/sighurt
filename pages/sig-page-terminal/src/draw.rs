//! What the page draws, as a list of operations the canvas carries out ([`Op`]): the emulator's
//! screen, or a page of text (the start screen, the end notice).
//!
//! Cells are a fixed size from the font's metrics ([`Metrics`]). A row is drawn as runs: one
//! fill for each run of cells with the same background (the default background is the page's,
//! cleared once), and one text for each run with the same colour and attributes, which a
//! monospace face lays out cell by cell. A wide character is drawn alone at its cell, and so is
//! every character when no monospace face was found, so columns stay true. Underlines are thin
//! fills; inverse swaps the colours; dim mixes the colour halfway to the background; bold and
//! italic pick the face. The cursor is a block in the text's colour with its character drawn
//! over it in the background's.
//!
//! Owns: turning a screen into operations, and the colours. Must not own: the canvas calls
//! (`crate::page`) or the screen (`crate::term`).

use vt100::Color;

pub type Rgb = [u8; 3];

/// The page's background and the default text colour.
pub const BACKGROUND: Rgb = [0x1c, 0x1d, 0x21];
pub const FOREGROUND: Rgb = [0xd4, 0xd4, 0xd4];
/// Colours of the page's own text.
pub const ACCENT: Rgb = [0x7a, 0xb8, 0xff];
pub const MUTED: Rgb = [0x90, 0x90, 0x98];
pub const WARNING: Rgb = [0xff, 0xb0, 0x60];

/// The 16 colours of SGR 30–37, 90–97 and `38;5;0`–`15`.
const PALETTE: [Rgb; 16] = [
    [0x1c, 0x1d, 0x21],
    [0xcd, 0x3f, 0x45],
    [0x4e, 0xb3, 0x55],
    [0xd0, 0xa2, 0x15],
    [0x3b, 0x8e, 0xea],
    [0xb5, 0x6e, 0xd8],
    [0x2a, 0xa1, 0xb3],
    [0xcc, 0xcc, 0xcc],
    [0x66, 0x66, 0x6e],
    [0xf1, 0x4c, 0x4c],
    [0x6c, 0xd6, 0x73],
    [0xf5, 0xd1, 0x4c],
    [0x6c, 0xb0, 0xff],
    [0xd6, 0x8f, 0xf5],
    [0x4f, 0xd0, 0xe3],
    [0xf5, 0xf5, 0xf5],
];

/// One thing to draw. Coordinates are canvas pixels; text is placed by the top of its line box.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Fill {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Rgb,
    },
    Text {
        x: f32,
        y: f32,
        text: String,
        color: Rgb,
        bold: bool,
        italic: bool,
    },
}

/// The size of a cell and of the font in it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// The font size, in pixels.
    pub size: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Whether the face lays every character one cell wide, so a run can be drawn as one text.
    pub monospace: bool,
}

impl Metrics {
    /// The columns and rows that fit in `width` × `height` pixels, at least one of each.
    pub fn fit(&self, width: u32, height: u32) -> (u16, u16) {
        let cols = (width as f32 / self.cell_w).floor().clamp(1.0, 1000.0) as u16;
        let rows = (height as f32 / self.cell_h).floor().clamp(1.0, 1000.0) as u16;
        (cols, rows)
    }
}

/// The colour of `color`, `default` for the terminal's default.
pub fn rgb(color: Color, default: Rgb) -> Rgb {
    match color {
        Color::Default => default,
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Idx(n) if n < 16 => PALETTE[usize::from(n)],
        Color::Idx(n) if n < 232 => {
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            let n = n - 16;
            [level(n / 36), level(n / 6 % 6), level(n % 6)]
        }
        Color::Idx(n) => {
            let v = 8 + (n - 232) * 10;
            [v, v, v]
        }
    }
}

fn mix(a: Rgb, b: Rgb) -> Rgb {
    [0, 1, 2].map(|i| ((u16::from(a[i]) + u16::from(b[i])) / 2) as u8)
}

/// How one cell looks.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Look {
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
}

fn look(cell: &vt100::Cell) -> Look {
    let (mut fg, mut bg) = (
        rgb(cell.fgcolor(), FOREGROUND),
        rgb(cell.bgcolor(), BACKGROUND),
    );
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    if cell.dim() {
        fg = mix(fg, bg);
    }
    Look {
        fg,
        bg,
        bold: cell.bold(),
        italic: cell.italic(),
        underline: cell.underline(),
    }
}

/// The operations that draw `screen`, after a clear to [`BACKGROUND`].
pub fn screen(screen: &vt100::Screen, m: Metrics) -> Vec<Op> {
    let (rows, cols) = screen.size();
    let mut ops = Vec::new();
    for row in 0..rows {
        let y = f32::from(row) * m.cell_h;
        let cells: Vec<_> = (0..cols)
            .map(|col| screen.cell(row, col).map(|cell| (cell, look(cell))))
            .collect();

        // Backgrounds and underlines: runs of equal colour.
        let mut col = 0;
        while col < cells.len() {
            let Some((_, first)) = cells[col] else {
                col += 1;
                continue;
            };
            let same_bg =
                |c: &Option<(&vt100::Cell, Look)>| c.is_some_and(|(_, l)| l.bg == first.bg);
            let end = col + cells[col..].iter().take_while(|c| same_bg(c)).count();
            if first.bg != BACKGROUND {
                ops.push(fill(m, col, end, y, m.cell_h, first.bg));
            }
            col = end.max(col + 1);
        }
        let mut col = 0;
        while col < cells.len() {
            let Some((_, first)) = cells[col].filter(|(_, l)| l.underline) else {
                col += 1;
                continue;
            };
            let same = |c: &Option<(&vt100::Cell, Look)>| {
                c.is_some_and(|(_, l)| l.underline && l.fg == first.fg)
            };
            let end = col + cells[col..].iter().take_while(|c| same(c)).count();
            let thickness = (m.size / 14.0).max(1.0);
            ops.push(fill(
                m,
                col,
                end,
                y + m.cell_h - 2.0 * thickness,
                thickness,
                first.fg,
            ));
            col = end;
        }

        // Text: runs of one look, drawn as one string from their first visible character.
        let mut run: Option<(usize, Look, String)> = None;
        let flush = |run: &mut Option<(usize, Look, String)>, ops: &mut Vec<Op>| {
            if let Some((start, look, text)) = run.take() {
                let text = text.trim_end().to_string();
                if !text.is_empty() {
                    ops.push(text_op(m, start, y, text, look.fg, look));
                }
            }
        };
        for (col, cell) in cells.iter().enumerate() {
            let Some((cell, look)) = *cell else {
                flush(&mut run, &mut ops);
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let contents = cell.contents();
            let blank = contents.is_empty() || contents == " ";
            let alone = cell.is_wide() || !m.monospace || !contents.is_ascii();
            if alone {
                flush(&mut run, &mut ops);
                if !blank {
                    ops.push(text_op(m, col, y, contents.to_string(), look.fg, look));
                }
                continue;
            }
            match &mut run {
                Some((_, run_look, text)) if same_text(*run_look, look) => {
                    text.push_str(if blank { " " } else { contents });
                }
                _ if blank => flush(&mut run, &mut ops),
                _ => {
                    flush(&mut run, &mut ops);
                    run = Some((col, look, contents.to_string()));
                }
            }
        }
        flush(&mut run, &mut ops);
    }

    if !screen.hide_cursor() {
        let (row, col) = screen.cursor_position();
        if let Some(cell) = screen.cell(row, col) {
            let look = look(cell);
            let width = if cell.is_wide() { 2 } else { 1 };
            let col = usize::from(col);
            let y = f32::from(row) * m.cell_h;
            ops.push(fill(m, col, col + width, y, m.cell_h, look.fg));
            let contents = cell.contents();
            if !contents.is_empty() && contents != " " {
                ops.push(text_op(m, col, y, contents.to_string(), look.bg, look));
            }
        }
    }
    ops
}

/// Whether a cell with `b` continues a text run of `a`: the same colour and face (the background
/// is drawn apart).
fn same_text(a: Look, b: Look) -> bool {
    a.fg == b.fg && a.bold == b.bold && a.italic == b.italic
}

fn fill(m: Metrics, start: usize, end: usize, y: f32, h: f32, color: Rgb) -> Op {
    Op::Fill {
        x: start as f32 * m.cell_w,
        y,
        w: (end - start) as f32 * m.cell_w,
        h,
        color,
    }
}

fn text_op(m: Metrics, col: usize, y: f32, text: String, color: Rgb, look: Look) -> Op {
    let lead = text.len() - text.trim_start().len();
    Op::Text {
        x: (col + lead) as f32 * m.cell_w,
        y: y + (m.cell_h - m.size * 1.2) / 2.0,
        text: text.trim_start().to_string(),
        color,
        bold: look.bold,
        italic: look.italic,
    }
}

/// A line of the page's own text.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub text: String,
    pub color: Rgb,
    pub bold: bool,
}

impl Line {
    pub fn new(text: impl Into<String>, color: Rgb) -> Self {
        Self {
            text: text.into(),
            color,
            bold: false,
        }
    }

    pub fn bold(mut self) -> Self {
        self.bold = true;
        self
    }
}

/// A page of `lines`, wrapped at words to fit `cols` columns less a margin of two on each side.
pub fn page(lines: &[Line], m: Metrics, cols: u16) -> Vec<Op> {
    let width = usize::from(cols).saturating_sub(4).max(10);
    let mut ops = Vec::new();
    let mut row = 1;
    for line in lines {
        for part in wrap(&line.text, width) {
            if !part.is_empty() {
                let look = Look {
                    fg: line.color,
                    bg: BACKGROUND,
                    bold: line.bold,
                    italic: false,
                    underline: false,
                };
                ops.push(text_op(m, 2, row as f32 * m.cell_h, part, line.color, look));
            }
            row += 1;
        }
    }
    ops
}

/// `text` broken at spaces into lines of at most `width` characters (a longer word is cut).
fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let indent: String = text.chars().take_while(|c| *c == ' ').collect();
    let mut lines = Vec::new();
    let mut line = indent.clone();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let used = line.chars().count();
            let fits = used + usize::from(used > indent.len()) + word.chars().count() <= width;
            if fits {
                if line.chars().count() > indent.len() {
                    line.push(' ');
                }
                line.push_str(&word);
                break;
            }
            if line.chars().count() > indent.len() {
                lines.push(std::mem::replace(&mut line, indent.clone()));
                continue;
            }
            // A word longer than the line: cut it.
            let room = width.saturating_sub(indent.len()).max(1);
            let head: String = word.chars().take(room).collect();
            word = word.chars().skip(room).collect();
            line.push_str(&head);
            lines.push(std::mem::replace(&mut line, indent.clone()));
            if word.is_empty() {
                break;
            }
        }
    }
    if line.chars().count() > indent.len() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::Term;

    const M: Metrics = Metrics {
        size: 10.0,
        cell_w: 6.0,
        cell_h: 12.0,
        monospace: true,
    };

    fn texts(ops: &[Op]) -> Vec<(f32, f32, &str)> {
        ops.iter()
            .filter_map(|op| match op {
                Op::Text { x, y, text, .. } => Some((*x, *y, text.as_str())),
                Op::Fill { .. } => None,
            })
            .collect()
    }

    #[test]
    fn colours() {
        assert_eq!(rgb(Color::Default, FOREGROUND), FOREGROUND);
        assert_eq!(rgb(Color::Idx(1), FOREGROUND), PALETTE[1]);
        assert_eq!(rgb(Color::Idx(16), FOREGROUND), [0, 0, 0]);
        assert_eq!(rgb(Color::Idx(196), FOREGROUND), [255, 0, 0]);
        assert_eq!(rgb(Color::Idx(231), FOREGROUND), [255, 255, 255]);
        assert_eq!(rgb(Color::Idx(232), FOREGROUND), [8, 8, 8]);
        assert_eq!(rgb(Color::Idx(255), FOREGROUND), [238, 238, 238]);
        assert_eq!(rgb(Color::Rgb(1, 2, 3), FOREGROUND), [1, 2, 3]);
    }

    #[test]
    fn rows_are_drawn_in_runs() {
        let mut term = Term::new(3, 20);
        term.feed(b"\x1b[?25lab cd  \x1b[31mred\x1b[0m e\r\n\x1b[44m  blue \x1b[0m");
        let ops = screen(term.screen(), M);
        // "ab cd" is one run, spaces and all; the red run starts where it starts.
        assert_eq!(
            texts(&ops),
            [
                (0.0, 0.0, "ab cd"),
                (42.0, 0.0, "red"),
                (66.0, 0.0, "e"),
                (12.0, 12.0, "blue")
            ]
        );
        let fills: Vec<_> = ops
            .iter()
            .filter(|op| matches!(op, Op::Fill { .. }))
            .collect();
        assert_eq!(
            fills,
            [&Op::Fill {
                x: 0.0,
                y: 12.0,
                w: 42.0,
                h: 12.0,
                color: PALETTE[4]
            }]
        );
    }

    #[test]
    fn wide_and_non_ascii_characters_stand_alone() {
        let mut term = Term::new(2, 10);
        term.feed("\x1b[?25la日b é".as_bytes());
        let ops = screen(term.screen(), M);
        assert_eq!(
            texts(&ops),
            [
                (0.0, 0.0, "a"),
                (6.0, 0.0, "日"),
                (18.0, 0.0, "b"),
                (30.0, 0.0, "é")
            ]
        );
        // Without a monospace face every character is placed at its cell.
        let proportional = Metrics {
            monospace: false,
            ..M
        };
        let mut term = Term::new(1, 10);
        term.feed(b"\x1b[?25lab");
        assert_eq!(
            texts(&screen(term.screen(), proportional)),
            [(0.0, 0.0, "a"), (6.0, 0.0, "b")]
        );
    }

    #[test]
    fn attributes() {
        let mut term = Term::new(1, 10);
        term.feed(b"\x1b[?25l\x1b[1ma\x1b[0;3mb\x1b[0;4mc\x1b[0;7md\x1b[0;2me");
        let ops = screen(term.screen(), M);
        let text = |i: usize| {
            ops.iter()
                .filter(|op| matches!(op, Op::Text { .. }))
                .nth(i)
                .unwrap()
        };
        assert!(matches!(
            text(0),
            Op::Text {
                bold: true,
                italic: false,
                ..
            }
        ));
        assert!(matches!(
            text(1),
            Op::Text {
                bold: false,
                italic: true,
                ..
            }
        ));
        // Underline: a thin fill under "c".
        assert!(ops
            .iter()
            .any(|op| matches!(op, Op::Fill { x, h, .. } if *x == 12.0 && *h == 1.0)));
        // Inverse: the background in the text's colour, the text in the background's.
        assert!(ops.contains(&Op::Fill {
            x: 18.0,
            y: 0.0,
            w: 6.0,
            h: 12.0,
            color: FOREGROUND
        }));
        assert!(matches!(text(3), Op::Text { color, .. } if *color == BACKGROUND));
        assert!(matches!(text(4), Op::Text { color, .. } if *color == mix(FOREGROUND, BACKGROUND)));
    }

    #[test]
    fn the_cursor_is_a_block_with_its_character_over_it() {
        let mut term = Term::new(2, 10);
        term.feed(b"ab\x1b[1;2H");
        let ops = screen(term.screen(), M);
        let n = ops.len();
        assert_eq!(
            ops[n - 2],
            Op::Fill {
                x: 6.0,
                y: 0.0,
                w: 6.0,
                h: 12.0,
                color: FOREGROUND
            }
        );
        assert!(
            matches!(&ops[n - 1], Op::Text { text, color, .. } if text == "b" && *color == BACKGROUND)
        );
        term.feed(b"\x1b[?25l");
        assert!(!screen(term.screen(), M)
            .iter()
            .any(|op| matches!(op, Op::Fill { .. })));
    }

    #[test]
    fn the_grid_fits_the_page() {
        assert_eq!(M.fit(800, 600), (133, 50));
        assert_eq!(M.fit(0, 0), (1, 1));
    }

    #[test]
    fn page_text_wraps_at_words() {
        assert_eq!(wrap("a bb ccc dddd", 6), ["a bb", "ccc", "dddd"]);
        assert_eq!(
            wrap("  indented text here", 10),
            ["  indented", "  text", "  here"]
        );
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        let ops = page(
            &[Line::new("hello world", FOREGROUND), Line::new("", MUTED)],
            M,
            9,
        );
        assert_eq!(texts(&ops), [(12.0, 12.0, "hello"), (12.0, 24.0, "world")]);
    }
}
