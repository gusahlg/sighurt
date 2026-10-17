//! The terminal chrome: one line above the page and one line below it.
//!
//! Owns: the plain data the lines are drawn from, the grapheme editor for the
//! address, and which rows are left for the page. Must not own: tabs, the
//! committed URL, the keymap, or the terminal. Every page-supplied string is
//! passed through [`crate::sanitize`] before it is drawn, and cut to the row's
//! width.
//!
//! The top line is `[2/5] title — address · engine · MODE · keymap`, with `…`
//! after the title while that tab is loading. The bottom line is the address
//! editor while it is open, otherwise the notice, otherwise the status.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::grid::{Attrs, Color, Grid};
use crate::sanitize::sanitize;

/// Shown in [`ChromeState::mode_label`] while the page is graphics.
pub const MODE_GRAPHICS: &str = "GFX";
/// Shown in [`ChromeState::mode_label`] while the page is text.
pub const MODE_TEXT: &str = "TEXT";

/// One tab, as the top line shows it. Strings are page data until [`ChromeState::contain`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLine {
    /// The page's title.
    pub title: String,
    /// The address.
    pub url: String,
    /// Whether the page is loading. Draws a `…` after the title.
    pub loading: bool,
    /// The engine's name, when the page has one.
    pub engine: Option<String>,
}

/// What the two chrome lines show. Every string is plain data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeState {
    /// The tabs, in order.
    pub tabs: Vec<TabLine>,
    /// Index into [`Self::tabs`] of the selected tab.
    pub active: usize,
    /// The address editor, while the user is editing it.
    pub address_editing: Option<LineEditor>,
    /// `GFX` or `TEXT`. Any other short label is drawn as it is.
    pub mode_label: String,
    /// What the keymap says about itself, such as `NORMAL`.
    pub keymap_indicator: Option<String>,
    /// A transient notice. It takes the bottom line over [`Self::status`].
    pub notice: Option<String>,
    /// The status line: a hovered link, or the page's own status.
    pub status: Option<String>,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: 0,
            address_editing: None,
            mode_label: MODE_GRAPHICS.to_string(),
            keymap_indicator: None,
            notice: None,
            status: None,
        }
    }
}

impl ChromeState {
    /// Run every page-supplied string through [`sanitize`].
    pub fn contain(&mut self) {
        for tab in &mut self.tabs {
            tab.title = sanitize(&tab.title);
            tab.url = sanitize(&tab.url);
            if let Some(engine) = &mut tab.engine {
                *engine = sanitize(engine);
            }
        }
        self.mode_label = sanitize(&self.mode_label);
        if let Some(keymap) = &mut self.keymap_indicator {
            *keymap = sanitize(keymap);
        }
        if let Some(notice) = &mut self.notice {
            *notice = sanitize(notice);
        }
        if let Some(status) = &mut self.status {
            *status = sanitize(status);
        }
        if let Some(editor) = &mut self.address_editing {
            editor.resanitize();
        }
    }

    /// Draw the chrome into `grid`.
    ///
    /// The page area is the rows strictly between the top line and the bottom
    /// line. A terminal with fewer than three rows has no page rows.
    pub fn draw(&self, grid: &mut Grid) -> PageArea {
        let cols = grid.cols();
        let rows = grid.rows();
        if rows >= 1 {
            grid.clear_row(0);
            put_fitted(grid, 0, &top_line(self, cols));
        }
        let cursor = if rows >= 2 {
            let row = rows - 1;
            grid.clear_row(row);
            let (text, column) = bottom_line(self, cols);
            put_fitted(grid, row, &text);
            column.map(|column| Cursor { column, row })
        } else {
            None
        };
        let page_rows = rows.saturating_sub(2);
        PageArea {
            row: u16::from(page_rows > 0),
            rows: page_rows,
            cols,
            cursor,
        }
    }
}

/// The rows left for the page, and the terminal cursor while the address is editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageArea {
    /// First page row. `0` when [`Self::rows`] is 0.
    pub row: u16,
    /// How many rows the page gets.
    pub rows: u16,
    /// Columns in the page, the terminal's width.
    pub cols: u16,
    /// Where the terminal cursor sits while the address editor is open.
    pub cursor: Option<Cursor>,
}

/// A cursor cell, 0-based.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// Column.
    pub column: u16,
    /// Row.
    pub row: u16,
}

/// A single-line editor. The cursor is always on a grapheme boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineEditor {
    text: String,
    /// Byte index into [`Self::text`].
    cursor: usize,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl LineEditor {
    /// An empty line, cursor at the start.
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
        }
    }

    /// `text` with the cursor at the end. Controls and bidi marks are removed.
    pub fn from_text(text: impl Into<String>) -> Self {
        let text = sanitize(&text.into());
        let cursor = text.len();
        Self { text, cursor }
    }

    /// The text, already sanitized.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Display columns before the cursor.
    pub fn cursor_column(&self) -> usize {
        self.text[..self.cursor].width()
    }

    /// Insert `text` at the cursor. Controls and bidi marks are removed.
    pub fn insert(&mut self, text: &str) {
        let text = sanitize(text);
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    /// Delete the grapheme before the cursor.
    pub fn backspace(&mut self) {
        let previous = prev_boundary(&self.text, self.cursor);
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
    }

    /// Delete the grapheme after the cursor.
    pub fn delete(&mut self) {
        let next = next_boundary(&self.text, self.cursor);
        self.text.replace_range(self.cursor..next, "");
    }

    /// Move one grapheme left.
    pub fn left(&mut self) {
        self.cursor = prev_boundary(&self.text, self.cursor);
    }

    /// Move one grapheme right.
    pub fn right(&mut self) {
        self.cursor = next_boundary(&self.text, self.cursor);
    }

    /// Move to the start.
    pub fn home(&mut self) {
        self.cursor = 0;
    }

    /// Move to the end.
    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    /// Move to the start of the previous word. A word is a run of non-whitespace graphemes.
    pub fn word_left(&mut self) {
        let marks = boundaries(&self.text);
        let mut index = marks
            .iter()
            .position(|mark| *mark == self.cursor)
            .unwrap_or(0);
        if index == 0 {
            return;
        }
        index -= 1;
        while index > 0 && is_space(&self.text[marks[index]..marks[index + 1]]) {
            index -= 1;
        }
        while index > 0 && !is_space(&self.text[marks[index - 1]..marks[index]]) {
            index -= 1;
        }
        self.cursor = marks[index];
    }

    /// Move to the end of the next word.
    pub fn word_right(&mut self) {
        let marks = boundaries(&self.text);
        let mut index = marks
            .iter()
            .position(|mark| *mark == self.cursor)
            .unwrap_or(0);
        let last = marks.len() - 1;
        while index < last && is_space(&self.text[marks[index]..marks[index + 1]]) {
            index += 1;
        }
        while index < last && !is_space(&self.text[marks[index]..marks[index + 1]]) {
            index += 1;
        }
        self.cursor = marks[index];
    }

    /// Insert `text` at the cursor, same as [`Self::insert`].
    pub fn paste(&mut self, text: &str) {
        self.insert(text);
    }

    fn resanitize(&mut self) {
        let column = self.cursor_column();
        self.text = sanitize(&self.text);
        self.cursor = byte_at_column(&self.text, column);
    }
}

fn top_line(state: &ChromeState, cols: u16) -> String {
    let count = state.tabs.len();
    let shown = if count == 0 {
        0
    } else {
        (state.active + 1).min(count)
    };
    let tab = state.tabs.get(state.active);
    let title = tab
        .map(|tab| {
            let title = sanitize(&tab.title);
            if tab.loading {
                format!("{title}…")
            } else {
                title
            }
        })
        .unwrap_or_default();
    let url = tab.map(|tab| sanitize(&tab.url)).unwrap_or_default();
    let mut line = format!("[{shown}/{count}] {title} — {url}");
    if let Some(engine) = tab.and_then(|tab| tab.engine.as_deref()) {
        let engine = sanitize(engine);
        if !engine.is_empty() {
            line.push_str(" · ");
            line.push_str(&engine);
        }
    }
    let mode = sanitize(&state.mode_label);
    if !mode.is_empty() {
        line.push_str(" · ");
        line.push_str(&mode);
    }
    if let Some(keymap) = &state.keymap_indicator {
        let keymap = sanitize(keymap);
        if !keymap.is_empty() {
            line.push_str(" · ");
            line.push_str(&keymap);
        }
    }
    fit_cols(&line, cols)
}

fn bottom_line(state: &ChromeState, cols: u16) -> (String, Option<u16>) {
    if let Some(editor) = &state.address_editing {
        return editor_line(editor, cols);
    }
    if let Some(notice) = state.notice.as_deref().map(sanitize) {
        if !notice.is_empty() {
            return (fit_cols(&notice, cols), None);
        }
    }
    if let Some(status) = state.status.as_deref().map(sanitize) {
        if !status.is_empty() {
            return (fit_cols(&status, cols), None);
        }
    }
    (String::new(), None)
}

const PROMPT: &str = "→ ";

fn editor_line(editor: &LineEditor, cols: u16) -> (String, Option<u16>) {
    let cols = usize::from(cols);
    let prompt_width = PROMPT.width();
    if cols == 0 {
        return (String::new(), None);
    }
    if prompt_width >= cols {
        return (
            fit_cols(PROMPT, u16::try_from(cols).unwrap_or(u16::MAX)),
            Some(0),
        );
    }
    let room = cols - prompt_width;
    let cursor = editor.cursor_column();
    let start = cursor.saturating_add(1).saturating_sub(room).min(cursor);
    let (visible, _) = slice_columns(editor.text(), start, room);
    let cursor_col = prompt_width + cursor - start;
    let cursor_col = u16::try_from(cursor_col.min(cols.saturating_sub(1))).unwrap_or(u16::MAX);
    (format!("{PROMPT}{visible}"), Some(cursor_col))
}

fn put_fitted(grid: &mut Grid, row: u16, text: &str) {
    grid.put_text(
        0,
        row,
        text,
        Color::Default,
        Color::Default,
        Attrs::default(),
    );
}

fn fit_cols(text: &str, cols: u16) -> String {
    let cols = usize::from(cols);
    if text.width() <= cols {
        return text.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let budget = cols - 1;
    let (body, _) = slice_columns(text, 0, budget);
    format!("{body}…")
}

/// Graphemes whose display columns start at `start` and cover at most `room` columns.
fn slice_columns(text: &str, start: usize, room: usize) -> (String, usize) {
    let mut skipped = 0;
    let mut used = 0;
    let mut out = String::new();
    for cluster in text.graphemes(true) {
        let width = cluster.width();
        if skipped < start {
            skipped += width;
            continue;
        }
        if used + width > room {
            break;
        }
        out.push_str(cluster);
        used += width;
    }
    (out, used)
}

fn boundaries(text: &str) -> Vec<usize> {
    let mut marks: Vec<usize> = text
        .grapheme_indices(true)
        .map(|(index, _)| index)
        .collect();
    marks.push(text.len());
    marks
}

fn prev_boundary(text: &str, cursor: usize) -> usize {
    text[..cursor]
        .grapheme_indices(true)
        .next_back()
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn next_boundary(text: &str, cursor: usize) -> usize {
    match text[cursor..].graphemes(true).next() {
        Some(cluster) => cursor + cluster.len(),
        None => cursor,
    }
}

fn byte_at_column(text: &str, column: usize) -> usize {
    let mut used = 0;
    for (index, cluster) in text.grapheme_indices(true) {
        if used >= column {
            return index;
        }
        used += cluster.width();
    }
    text.len()
}

fn is_space(cluster: &str) -> bool {
    cluster.chars().all(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hostile() -> ChromeState {
        ChromeState {
            tabs: vec![TabLine {
                title: "\u{1b}[31m\u{202e}漢字😀\u{7}".into(),
                url: "http://a.\u{1b}[0mco".into(),
                loading: true,
                engine: Some("ser\u{202e}vo".into()),
            }],
            active: 0,
            address_editing: None,
            mode_label: MODE_GRAPHICS.into(),
            keymap_indicator: Some("NOR\u{1b}[0mMAL".into()),
            notice: None,
            status: Some("ok".into()),
        }
    }

    fn row_text(grid: &Grid, row: u16) -> String {
        let mut text = String::new();
        let mut col = 0;
        while col < grid.cols() {
            let cell = grid.cell(col, row).expect("cell");
            if cell.width == 0 {
                col += 1;
                continue;
            }
            text.push_str(&cell.grapheme);
            col += u16::from(cell.width.max(1));
        }
        text.trim_end().to_string()
    }

    fn assert_safe(grid: &Grid) {
        for row in 0..grid.rows() {
            let mut width = 0usize;
            for col in 0..grid.cols() {
                let cell = grid.cell(col, row).expect("cell");
                assert!(
                    !cell.grapheme.chars().any(|ch| {
                        ch == '\u{1b}' || ch == '\u{202e}' || ch == '\u{7}' || ch.is_control()
                    }),
                    "hostile cell {:?} at {col},{row}",
                    cell.grapheme
                );
                if cell.width > 0 {
                    width += usize::from(cell.width);
                }
            }
            assert!(width <= usize::from(grid.cols()));
        }
    }

    #[test]
    fn layout_at_20_80_and_200_keeps_hostile_text_in_the_cells() {
        let state = hostile();
        for cols in [20u16, 80, 200] {
            let mut grid = Grid::new(cols, 24);
            let page = state.draw(&mut grid);
            assert_eq!(page.row, 1);
            assert_eq!(page.rows, 22);
            assert_eq!(page.cols, cols);
            assert!(page.cursor.is_none());
            assert_safe(&grid);
            let top = row_text(&grid, 0);
            assert!(top.starts_with("[1/1]"), "{top}");
            assert!(top.contains("漢字"), "{cols} {top}");
            assert!(top.contains('😀'), "{cols} {top}");
            assert!(top.contains('…'), "{cols} {top}");
            if cols >= 80 {
                assert!(top.contains("GFX"), "{top}");
                assert!(top.contains("servo"), "{top}");
                assert!(top.contains("NORMAL"), "{top}");
                assert!(top.contains("http://a.co"), "{top}");
            } else {
                assert!(top.ends_with('…'), "{top}");
            }
            assert_eq!(row_text(&grid, 23), "ok");
            assert!(top.width() <= usize::from(cols), "{cols} {top}");
        }
    }

    #[test]
    fn a_long_title_is_cut_and_a_short_terminal_has_no_page() {
        let mut state = hostile();
        state.tabs[0].title = format!("{}{}", state.tabs[0].title, "A".repeat(400));
        state.notice = Some("\u{1b}[2Jnotice".into());
        let mut wide = Grid::new(80, 6);
        state.draw(&mut wide);
        let top = row_text(&wide, 0);
        assert!(top.width() <= 80, "{top}");
        assert!(top.ends_with('…'), "{top}");
        assert_safe(&wide);
        assert_eq!(row_text(&wide, 5), "notice");

        let mut flat = Grid::new(40, 2);
        let page = state.draw(&mut flat);
        assert_eq!(page.rows, 0);
        assert_eq!(page.row, 0);
        let mut one = Grid::new(20, 1);
        let page = state.draw(&mut one);
        assert_eq!(page.rows, 0);
    }

    #[test]
    fn the_editor_is_grapheme_aware_and_places_the_cursor() {
        let mut editor = LineEditor::new();
        editor.insert("ab😀c");
        assert_eq!(editor.text(), "ab😀c");
        editor.left();
        editor.backspace();
        assert_eq!(editor.text(), "abc");
        editor.home();
        editor.delete();
        assert_eq!(editor.text(), "bc");
        editor.insert("one two ");
        editor.word_left();
        editor.insert("X");
        assert_eq!(editor.text(), "one Xtwo bc");
        editor.end();
        editor.word_left();
        editor.word_left();
        assert_eq!(&editor.text()[..editor.cursor_column()], "one ");
        editor.paste("\u{1b}[31m\tZ");
        assert!(!editor.text().contains('\u{1b}'));
        assert!(!editor.text().chars().any(|ch| ch.is_control()));
        assert!(editor.text().contains('Z'));

        editor.home();
        editor.insert("h");
        // "hone Xtwo bc" after the paste landed at the word boundary. The
        // cursor column is what the chrome places, so rebuild a known line.
        let editor = LineEditor::from_text("hi");
        assert_eq!(editor.cursor_column(), 2);
        let mut state = ChromeState {
            address_editing: Some(editor),
            notice: Some("hidden while editing".into()),
            status: Some("also hidden".into()),
            ..ChromeState::default()
        };
        let mut grid = Grid::new(40, 4);
        let page = state.draw(&mut grid);
        let bottom = row_text(&grid, 3);
        assert!(bottom.starts_with("→ hi"), "{bottom}");
        assert_eq!(page.cursor, Some(Cursor { column: 4, row: 3 }));
        state.address_editing.as_mut().unwrap().left();
        let page = state.draw(&mut grid);
        assert_eq!(page.cursor, Some(Cursor { column: 3, row: 3 }));
    }
}
