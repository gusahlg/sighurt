//! A cell grid and the damage diff that paints it.
//!
//! Owns: the cells the terminal should show, and the bytes that move it from
//! the last frame to this one. Must not own: the terminal file descriptor, the
//! input parser, page pixels, or which strings are safe to draw (callers
//! sanitize before they put text).
//!
//! A cell holds one grapheme cluster. Width comes from `unicode-width`. A wide
//! cluster that does not fit before the right edge is stored as a single
//! space, so it never spills onto the next row. The diff emits only the cells
//! that changed, with the shortest cursor move and one SGR sequence per style
//! change, inside synchronized output.

use std::io::Write;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::term::{SYNC_BEGIN, SYNC_END};

/// A colour the cell can ask the terminal for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Color {
    /// The terminal's default.
    #[default]
    Default,
    /// A 256-colour palette index.
    Indexed(u8),
    /// 24-bit RGB.
    Rgb(u8, u8, u8),
}

/// Attributes drawn with SGR. Reverse is the reverse-video attribute, not a
/// swap of the stored colours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Attrs {
    /// Bold (`SGR 1`).
    pub bold: bool,
    /// Italic (`SGR 3`).
    pub italic: bool,
    /// Underline (`SGR 4`).
    pub underline: bool,
    /// Reverse video (`SGR 7`).
    pub reverse: bool,
}

/// One column of one row. A wide cluster's continuation has `width == 0`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// The cluster, or empty for a wide continuation. A blank is a space.
    pub grapheme: String,
    /// Display columns: 0 continuation, 1 narrow, 2 or more wide.
    pub width: u8,
    /// Foreground.
    pub fg: Color,
    /// Background.
    pub bg: Color,
    /// SGR attributes.
    pub attrs: Attrs,
}

impl Cell {
    fn blank() -> Self {
        Self {
            grapheme: " ".to_string(),
            width: 1,
            fg: Color::Default,
            bg: Color::Default,
            attrs: Attrs::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pen {
    attrs: Attrs,
    fg: Color,
    bg: Color,
}

impl Pen {
    fn from_cell(cell: &Cell) -> Self {
        Self {
            attrs: cell.attrs,
            fg: cell.fg,
            bg: cell.bg,
        }
    }

    fn is_default(self) -> bool {
        self.attrs == Attrs::default() && self.fg == Color::Default && self.bg == Color::Default
    }
}

impl Default for Pen {
    fn default() -> Self {
        Self {
            attrs: Attrs::default(),
            fg: Color::Default,
            bg: Color::Default,
        }
    }
}

/// The screen the terminal should show, and the screen it was last sent.
pub struct Grid {
    cols: u16,
    rows: u16,
    cells: Vec<Cell>,
    published: Vec<Cell>,
    /// Cursor after the last diff. `None` means the next write must address it.
    cursor: Option<(u16, u16)>,
    /// The terminal's current attributes. `None` means unknown: the next write resets them first.
    pen: Option<Pen>,
}

impl Grid {
    /// A blank grid. The cursor is unknown until the first diff places it.
    pub fn new(cols: u16, rows: u16) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let n = usize::from(cols) * usize::from(rows);
        Self {
            cols,
            rows,
            cells: vec![Cell::blank(); n],
            published: vec![Cell::blank(); n],
            cursor: None,
            pen: Some(Pen::default()),
        }
    }

    /// Columns.
    pub fn cols(&self) -> u16 {
        self.cols
    }

    /// Rows.
    pub fn rows(&self) -> u16 {
        self.rows
    }

    /// The cell at `col`, `row`, when it is on the grid.
    pub fn cell(&self, col: u16, row: u16) -> Option<&Cell> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        Some(&self.cells[self.index(col, row)])
    }

    /// Blank one row. A later [`Self::diff`] paints the cells that changed.
    pub fn clear_row(&mut self, row: u16) {
        if row >= self.rows {
            return;
        }
        for col in 0..self.cols {
            let index = self.index(col, row);
            self.cells[index] = Cell::blank();
        }
    }

    /// Place one cluster. A width of 0 (a combining mark on its own) is not a
    /// cell. Returns the columns actually occupied.
    pub fn put_cluster(
        &mut self,
        col: u16,
        row: u16,
        cluster: &str,
        fg: Color,
        bg: Color,
        attrs: Attrs,
    ) -> u16 {
        if row >= self.rows || col >= self.cols || cluster.is_empty() {
            return 0;
        }
        let measured = cluster.width();
        if measured == 0 {
            return 0;
        }
        let (text, width) = if measured >= 2 && usize::from(col) + measured > usize::from(self.cols)
        {
            (" ".to_string(), 1u16)
        } else {
            (
                cluster.to_string(),
                u16::try_from(measured).unwrap_or(u16::MAX),
            )
        };
        self.clear_overlap(col, row, width);
        let index = self.index(col, row);
        self.cells[index] = Cell {
            grapheme: text,
            width: u8::try_from(width).unwrap_or(u8::MAX),
            fg,
            bg,
            attrs,
        };
        for offset in 1..width {
            let index = self.index(col + offset, row);
            let cell = &mut self.cells[index];
            cell.grapheme.clear();
            cell.width = 0;
            cell.fg = fg;
            cell.bg = bg;
            cell.attrs = attrs;
        }
        width
    }

    /// Place each grapheme cluster of `text`, starting at `col`. Returns the
    /// column after the last cluster that fit.
    pub fn put_text(
        &mut self,
        mut col: u16,
        row: u16,
        text: &str,
        fg: Color,
        bg: Color,
        attrs: Attrs,
    ) -> u16 {
        for cluster in text.graphemes(true) {
            if col >= self.cols {
                break;
            }
            let width = self.put_cluster(col, row, cluster, fg, bg, attrs);
            if width == 0 {
                continue;
            }
            col = col.saturating_add(width);
        }
        col
    }

    /// Bytes that bring the terminal from the last diff to the current grid.
    /// Empty when nothing changed, with no synchronized-output wrapper.
    pub fn diff(&mut self) -> Vec<u8> {
        let mut body = Vec::new();
        let mut wrote = false;
        let len = self.cells.len();
        for index in 0..len {
            if self.cells[index] == self.published[index] {
                continue;
            }
            let width = self.cells[index].width;
            if width == 0 {
                self.published[index] = self.cells[index].clone();
                continue;
            }
            let col = u16::try_from(index % usize::from(self.cols)).unwrap_or(0);
            let row = u16::try_from(index / usize::from(self.cols)).unwrap_or(0);
            self.move_to(&mut body, col, row);
            let pen = Pen::from_cell(&self.cells[index]);
            let from = self.pen.unwrap_or_else(|| {
                body.extend_from_slice(b"\x1b[0m");
                Pen::default()
            });
            write_sgr(&mut body, from, pen);
            self.pen = Some(pen);
            body.extend(self.cells[index].grapheme.as_bytes());
            let next = col.saturating_add(u16::from(width));
            self.cursor = if next >= self.cols {
                None
            } else {
                Some((next, row))
            };
            self.published[index] = self.cells[index].clone();
            wrote = true;
        }
        if !wrote {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(SYNC_BEGIN.len() + body.len() + SYNC_END.len());
        out.extend_from_slice(SYNC_BEGIN);
        out.extend_from_slice(&body);
        out.extend_from_slice(SYNC_END);
        out
    }

    /// Something else moved the terminal's cursor: the next diff addresses its first cell
    /// absolutely instead of moving from where the last diff left the cursor.
    pub fn forget_cursor(&mut self) {
        self.cursor = None;
    }

    /// Something else wrote to the terminal and left its attributes unknown: the next write
    /// starts from a reset and sends its own.
    pub fn forget_pen(&mut self) {
        self.pen = None;
    }

    fn index(&self, col: u16, row: u16) -> usize {
        usize::from(row) * usize::from(self.cols) + usize::from(col)
    }

    /// Blank any wide cluster that overlaps the columns we are about to write.
    fn clear_overlap(&mut self, col: u16, row: u16, width: u16) {
        let mut cursor = col;
        while cursor < col.saturating_add(width) && cursor < self.cols {
            let start = self.span_start(cursor, row);
            let span = u16::from(self.cells[self.index(start, row)].width.max(1));
            if self.cells[self.index(start, row)].width >= 2
                || self.cells[self.index(cursor, row)].width == 0
            {
                for offset in 0..span {
                    let at = start + offset;
                    if at >= self.cols {
                        break;
                    }
                    let index = self.index(at, row);
                    self.cells[index] = Cell::blank();
                }
            }
            cursor = cursor.saturating_add(1);
        }
    }

    fn span_start(&self, col: u16, row: u16) -> u16 {
        if self.cells[self.index(col, row)].width != 0 {
            return col;
        }
        let mut start = col;
        while start > 0 && self.cells[self.index(start, row)].width == 0 {
            start -= 1;
        }
        let cover = u16::from(self.cells[self.index(start, row)].width);
        if cover > col - start {
            start
        } else {
            col
        }
    }

    fn move_to(&mut self, out: &mut Vec<u8>, col: u16, row: u16) {
        if self.cursor == Some((col, row)) {
            return;
        }
        if let Some((from_col, from_row)) = self.cursor {
            if from_row == row && from_col != col {
                let horizontal = if col > from_col {
                    cuf(col - from_col)
                } else {
                    cub(from_col - col)
                };
                let absolute = cup(col, row);
                if horizontal.len() <= absolute.len() {
                    out.extend_from_slice(&horizontal);
                    self.cursor = Some((col, row));
                    return;
                }
            }
        }
        let absolute = cup(col, row);
        out.extend_from_slice(&absolute);
        self.cursor = Some((col, row));
    }
}

fn cup(col: u16, row: u16) -> Vec<u8> {
    let column = col + 1;
    let line = row + 1;
    if line == 1 && column == 1 {
        b"\x1b[H".to_vec()
    } else if line == 1 {
        format!("\x1b[;{column}H").into_bytes()
    } else if column == 1 {
        format!("\x1b[{line}H").into_bytes()
    } else {
        format!("\x1b[{line};{column}H").into_bytes()
    }
}

fn cuf(n: u16) -> Vec<u8> {
    if n == 1 {
        b"\x1b[C".to_vec()
    } else {
        format!("\x1b[{n}C").into_bytes()
    }
}

fn cub(n: u16) -> Vec<u8> {
    if n == 1 {
        b"\x1b[D".to_vec()
    } else {
        format!("\x1b[{n}D").into_bytes()
    }
}

fn write_sgr(out: &mut Vec<u8>, from: Pen, to: Pen) {
    if from == to {
        return;
    }
    if to.is_default() {
        out.extend_from_slice(b"\x1b[0m");
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    if from.attrs.bold != to.attrs.bold {
        parts.push(if to.attrs.bold { "1" } else { "22" }.to_string());
    }
    if from.attrs.italic != to.attrs.italic {
        parts.push(if to.attrs.italic { "3" } else { "23" }.to_string());
    }
    if from.attrs.underline != to.attrs.underline {
        parts.push(if to.attrs.underline { "4" } else { "24" }.to_string());
    }
    if from.attrs.reverse != to.attrs.reverse {
        parts.push(if to.attrs.reverse { "7" } else { "27" }.to_string());
    }
    if from.fg != to.fg {
        parts.push(color_params(to.fg, true));
    }
    if from.bg != to.bg {
        parts.push(color_params(to.bg, false));
    }
    let _ = write!(out, "\x1b[{}m", parts.join(";"));
}

fn color_params(color: Color, foreground: bool) -> String {
    match color {
        Color::Default => {
            if foreground {
                "39".to_string()
            } else {
                "49".to_string()
            }
        }
        Color::Indexed(index) => {
            if foreground {
                format!("38;5;{index}")
            } else {
                format!("48;5;{index}")
            }
        }
        Color::Rgb(red, green, blue) => {
            if foreground {
                format!("38;2;{red};{green};{blue}")
            } else {
                format!("48;2;{red};{green};{blue}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bold_red() -> (Color, Attrs) {
        (
            Color::Rgb(1, 2, 3),
            Attrs {
                bold: true,
                ..Attrs::default()
            },
        )
    }

    #[test]
    fn one_changed_cell_matches_the_golden_stream() {
        let mut grid = Grid::new(8, 3);
        let (fg, attrs) = bold_red();
        grid.put_cluster(3, 1, "A", fg, Color::Default, attrs);
        let diff = grid.diff();
        assert_eq!(diff, b"\x1b[?2026h\x1b[2;4H\x1b[1;38;2;1;2;3mA\x1b[?2026l");
        assert!(grid.diff().is_empty());
    }

    #[test]
    fn a_forgotten_pen_is_reset_before_the_next_write() {
        let mut grid = Grid::new(8, 3);
        let (fg, attrs) = bold_red();
        grid.put_cluster(0, 0, "A", fg, Color::Default, attrs);
        grid.diff();
        // Same attributes as the pen: nothing is re-sent until the pen is forgotten.
        grid.put_cluster(1, 0, "B", fg, Color::Default, attrs);
        assert_eq!(grid.diff(), b"\x1b[?2026hB\x1b[?2026l");
        grid.forget_pen();
        grid.put_cluster(2, 0, "C", fg, Color::Default, attrs);
        assert_eq!(
            grid.diff(),
            b"\x1b[?2026h\x1b[0m\x1b[1;38;2;1;2;3mC\x1b[?2026l"
        );
    }

    #[test]
    fn wide_character_at_the_origin() {
        let mut grid = Grid::new(4, 1);
        grid.put_cluster(0, 0, "あ", Color::Default, Color::Default, Attrs::default());
        let diff = grid.diff();
        assert_eq!(diff, b"\x1b[?2026h\x1b[H\xe3\x81\x82\x1b[?2026l");
    }

    #[test]
    fn wide_character_cut_at_the_right_edge_becomes_a_space() {
        let mut grid = Grid::new(2, 1);
        grid.put_cluster(1, 0, "Z", Color::Default, Color::Default, Attrs::default());
        let first = grid.diff();
        assert!(first.contains(&b'Z'));
        grid.put_cluster(1, 0, "あ", Color::Default, Color::Default, Attrs::default());
        let second = grid.diff();
        assert!(
            !second.windows(3).any(|window| window == [0xe3, 0x81, 0x82]),
            "cut wide char leaked: {second:?}"
        );
        assert!(second.contains(&b' '));
    }

    #[test]
    fn indexed_colour_and_reverse_share_one_sgr() {
        let mut grid = Grid::new(2, 1);
        grid.put_cluster(
            0,
            0,
            "Q",
            Color::Indexed(196),
            Color::Indexed(17),
            Attrs {
                reverse: true,
                italic: true,
                ..Attrs::default()
            },
        );
        let diff = grid.diff();
        let text = String::from_utf8(diff).unwrap();
        assert!(text.contains("3;7;38;5;196;48;5;17"));
    }

    #[test]
    fn overwriting_a_continuation_clears_the_wide_primary() {
        let mut grid = Grid::new(4, 1);
        grid.put_cluster(0, 0, "あ", Color::Default, Color::Default, Attrs::default());
        let _ = grid.diff();
        grid.put_cluster(1, 0, "X", Color::Default, Color::Default, Attrs::default());
        let diff = String::from_utf8(grid.diff()).unwrap();
        assert!(!diff.contains('あ'));
        assert!(diff.contains('X'));
        assert!(diff.contains(' '));
    }
}
