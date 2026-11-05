//! The plain-text editor: the caret and the selection, the commands keys run, the history of
//! changes, and which part of the text is in view.
//!
//! Every change goes through [`Editor::replace`], which refuses one that would take the text
//! over [`MAX_TEXT`] and records it for undo. The history keeps at most [`MAX_UNDO`] steps
//! and [`MAX_UNDO_BYTES`] of text; the oldest go first. Typing and deleting next to the last
//! change of the same kind extend that step, up to a word.
//!
//! Must not own: the file, saving or checking (the page), how keys arrive (the input
//! queue's), or drawing.

use std::collections::VecDeque;

use crate::buffer::{Buffer, Pos};
use crate::layout::Widths;

/// The largest text the editor holds, in bytes: the most the runtime writes.
pub const MAX_TEXT: usize = 256 * 1024;
/// Most undo steps kept.
pub const MAX_UNDO: usize = 1000;
/// Most text, removed plus inserted, the undo steps keep.
pub const MAX_UNDO_BYTES: usize = 4 * 1024 * 1024;

/// A key the editor knows, as the input queue names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A character key, for shortcuts: what it types goes through [`Input::Text`].
    Char(char),
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
}

/// The modifiers held with a key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Mods {
    pub const NONE: Self = Self {
        shift: false,
        ctrl: false,
        alt: false,
    };
    pub const SHIFT: Self = Self {
        shift: true,
        ctrl: false,
        alt: false,
    };
    pub const CTRL: Self = Self {
        shift: false,
        ctrl: true,
        alt: false,
    };
    pub const CTRL_SHIFT: Self = Self {
        shift: true,
        ctrl: true,
        alt: false,
    };
}

/// One input, in the order it happened. Pointer positions are in the text area's pixels.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    /// A key press, or its repeat.
    Key(Key, Mods),
    /// What a key press or an input method typed.
    Text(String),
    /// What the user pasted.
    Paste(String),
    /// The primary button went down at a point, with Shift held or not.
    Press { x: f32, y: f32, shift: bool },
    /// The pointer moved with the button down.
    Drag { x: f32, y: f32 },
    /// The wheel or touchpad scrolled, in pixels.
    Wheel { dx: f32, dy: f32 },
}

/// What a key asked of the page besides editing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Put this text on the clipboard.
    Copy(String),
    /// Save the file.
    Save,
    /// Say this to the user.
    Say(&'static str),
}

/// What a step of the history changed: at `at`, `removed` was replaced by `inserted`.
#[derive(Clone, Debug)]
struct Change {
    at: Pos,
    removed: String,
    inserted: String,
    /// The caret and anchor before the change.
    before: (Pos, Pos),
    kind: Kind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Typing,
    Deleting,
    Other,
}

/// The editor.
pub struct Editor {
    buf: Buffer,
    caret: Pos,
    /// The other end of the selection; the caret itself when nothing is selected.
    anchor: Pos,
    /// The x the caret keeps while it moves up and down.
    goal: Option<f32>,
    undo: VecDeque<Change>,
    redo: Vec<Change>,
    undo_bytes: usize,
    /// Whether the next change may extend the last step.
    extend: bool,
    /// Bumped by every change of the text.
    revision: u64,
    /// The first line in view.
    pub top: usize,
    /// Fractional lines the wheel has scrolled and not yet moved `top` by.
    wheel: f32,
    /// How far the text is scrolled sideways, in pixels.
    pub scroll_x: f32,
    /// Lines in view, and the text area's width.
    rows: usize,
    width: f32,
    line_height: f32,
    /// Whether the view must bring the caret into it.
    follow: bool,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        Self {
            buf: Buffer::new(text),
            caret: Pos::default(),
            anchor: Pos::default(),
            goal: None,
            undo: VecDeque::new(),
            redo: Vec::new(),
            undo_bytes: 0,
            extend: false,
            revision: 0,
            top: 0,
            wheel: 0.0,
            scroll_x: 0.0,
            rows: 1,
            width: 1.0,
            line_height: 1.0,
            follow: true,
        }
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buf
    }

    pub fn text(&self) -> String {
        self.buf.text()
    }

    pub fn caret(&self) -> Pos {
        self.caret
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The selection, ordered; `None` when nothing is selected.
    pub fn selection(&self) -> Option<(Pos, Pos)> {
        (self.caret != self.anchor).then(|| order(self.caret, self.anchor))
    }

    /// Sets the text area: lines in view, width and line height in pixels.
    pub fn set_view(&mut self, rows: usize, width: f32, line_height: f32) {
        self.rows = rows.max(1);
        self.width = width.max(1.0);
        self.line_height = line_height.max(1.0);
    }

    /// Replaces the whole text, as one step that undo takes back, and puts the caret at the
    /// same line.
    pub fn replace_all(&mut self, text: &str) -> bool {
        let line = self.caret.line;
        self.extend = false;
        if !self.replace(Pos::default(), self.buf.end(), text, Kind::Other) {
            return false;
        }
        self.extend = false;
        self.set_caret(self.buf.clamp(Pos::new(line, 0)), false);
        true
    }

    /// Puts the caret on `line` (0-based), at its first column.
    pub fn go_to_line(&mut self, line: usize) {
        self.set_caret(self.buf.clamp(Pos::new(line, 0)), false);
    }

    /// Handles one input; what the page should do about it, if anything.
    pub fn input(&mut self, input: Input, widths: &mut Widths) -> Option<Action> {
        match input {
            Input::Key(key, mods) => self.key(key, mods, widths),
            Input::Text(text) => {
                let text: String = text.chars().filter(|c| !c.is_control()).collect();
                if text.is_empty() {
                    return None;
                }
                self.type_text(&text, Kind::Typing)
            }
            Input::Paste(text) => {
                let text: String = text
                    .replace("\r\n", "\n")
                    .chars()
                    .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
                    .collect();
                self.extend = false;
                let said = self.type_text(&text, Kind::Other);
                self.extend = false;
                said
            }
            Input::Press { x, y, shift } => {
                let pos = self.pos_at(x, y, widths);
                self.set_caret(pos, shift);
                None
            }
            Input::Drag { x, y } => {
                let pos = self.pos_at(x, y, widths);
                self.set_caret(pos, true);
                None
            }
            Input::Wheel { dx, dy } => {
                self.scroll(dx, dy);
                None
            }
        }
    }

    fn key(&mut self, key: Key, mods: Mods, widths: &mut Widths) -> Option<Action> {
        let shift = mods.shift;
        if mods.ctrl && !mods.alt {
            if let Key::Char(c) = key {
                return self.shortcut(c.to_ascii_lowercase(), shift);
            }
        }
        let b = &self.buf;
        let caret = self.caret;
        let to = match key {
            Key::Char(_) => return None,
            Key::Enter => {
                let indent = b.indent(caret.line).min(caret.col);
                let text = format!("\n{}", &b.line(caret.line)[..indent]);
                self.extend = false;
                return self.type_text(&text, Kind::Other);
            }
            Key::Tab => return self.type_text("  ", Kind::Typing),
            Key::Escape => {
                self.anchor = self.caret;
                return None;
            }
            Key::Backspace | Key::Delete => return self.delete(key == Key::Delete, mods.ctrl),
            Key::Left => match self.selection() {
                Some((start, _)) if !shift => start,
                _ if mods.ctrl => b.prev_word(caret),
                _ => b.prev_grapheme(caret),
            },
            Key::Right => match self.selection() {
                Some((_, end)) if !shift => end,
                _ if mods.ctrl => b.next_word(caret),
                _ => b.next_grapheme(caret),
            },
            Key::Home if mods.ctrl => Pos::default(),
            Key::End if mods.ctrl => b.end(),
            Key::Home => {
                let indent = b.indent(caret.line);
                Pos::new(caret.line, if caret.col == indent { 0 } else { indent })
            }
            Key::End => Pos::new(caret.line, b.line(caret.line).len()),
            Key::Up | Key::Down | Key::PageUp | Key::PageDown => {
                let page = self.rows.saturating_sub(1).max(1);
                let (up, lines) = match key {
                    Key::Up => (true, 1),
                    Key::Down => (false, 1),
                    Key::PageUp => (true, page),
                    _ => (false, page),
                };
                return {
                    self.vertical(up, lines, shift, widths);
                    if matches!(key, Key::PageUp | Key::PageDown) {
                        self.top = if up {
                            self.top.saturating_sub(lines)
                        } else {
                            (self.top + lines).min(self.buf.line_count().saturating_sub(1))
                        };
                    }
                    None
                };
            }
        };
        self.set_caret(to, shift);
        None
    }

    fn shortcut(&mut self, c: char, shift: bool) -> Option<Action> {
        match c {
            'a' => {
                self.anchor = Pos::default();
                self.caret = self.buf.end();
                self.goal = None;
                self.follow = true;
                None
            }
            'c' => self
                .selection()
                .map(|(s, e)| Action::Copy(self.buf.slice(s, e))),
            'x' => {
                let (s, e) = self.selection()?;
                let text = self.buf.slice(s, e);
                self.extend = false;
                self.replace(s, e, "", Kind::Other);
                self.extend = false;
                Some(Action::Copy(text))
            }
            'z' if shift => self.redo(),
            'z' => self.undo(),
            'y' => self.redo(),
            's' => Some(Action::Save),
            _ => None,
        }
    }

    /// Inserts `text` over the selection.
    fn type_text(&mut self, text: &str, kind: Kind) -> Option<Action> {
        let (start, end) = self.selection().unwrap_or((self.caret, self.caret));
        if start != end {
            self.extend = false;
        }
        if !self.replace(start, end, text, kind) {
            return Some(Action::Say(TOO_LARGE));
        }
        None
    }

    /// Backspace or Delete: the selection, or a cluster (a word with Ctrl) before or after
    /// the caret.
    fn delete(&mut self, forward: bool, word: bool) -> Option<Action> {
        let (start, end) = match self.selection() {
            Some(range) => {
                self.extend = false;
                range
            }
            None => {
                let c = self.caret;
                let b = &self.buf;
                match (forward, word) {
                    (true, true) => (c, b.next_word(c)),
                    (true, false) => (c, b.next_grapheme(c)),
                    (false, true) => (b.prev_word(c), c),
                    (false, false) => (b.prev_grapheme(c), c),
                }
            }
        };
        if start != end {
            self.replace(start, end, "", Kind::Deleting);
        }
        None
    }

    /// Replaces `start..end` with `text`, records it, and puts the caret after it. False,
    /// changing nothing, when the text would grow over [`MAX_TEXT`].
    fn replace(&mut self, start: Pos, end: Pos, text: &str, kind: Kind) -> bool {
        let removed_len = self.buf.slice(start, end).len();
        if self.buf.len() - removed_len + text.len() > MAX_TEXT && text.len() > removed_len {
            return false;
        }
        let before = (self.caret, self.anchor);
        let removed = self.buf.remove(start, end);
        let after = self.buf.insert(start, text);
        self.record(Change {
            at: start,
            removed,
            inserted: text.to_owned(),
            before,
            kind,
        });
        self.redo.clear();
        self.revision += 1;
        self.caret = after;
        self.anchor = after;
        self.goal = None;
        self.follow = true;
        true
    }

    fn record(&mut self, change: Change) {
        let extends = self.extend
            && change.kind != Kind::Other
            && self.undo.back().is_some_and(|last| {
                last.kind == change.kind
                    && match change.kind {
                        // Typing on from where the last typing ended, up to a space.
                        Kind::Typing => {
                            end_of(last.at, &last.inserted) == change.at
                                && change.removed.is_empty()
                                && !(change.inserted.starts_with(' ')
                                    && !last.inserted.ends_with(' '))
                        }
                        // Deleting backwards from where the last deletion was, or forwards
                        // at the same place.
                        _ => {
                            change.inserted.is_empty()
                                && (end_of(change.at, &change.removed) == last.at
                                    || change.at == last.at)
                        }
                    }
            });
        self.extend = true;
        let bytes = change.removed.len() + change.inserted.len();
        self.undo_bytes += bytes;
        if extends {
            let last = self.undo.back_mut().expect("checked");
            if change.kind == Kind::Typing {
                last.inserted.push_str(&change.inserted);
            } else if change.at == last.at {
                last.removed.push_str(&change.removed);
            } else {
                last.removed.insert_str(0, &change.removed);
                last.at = change.at;
            }
        } else {
            self.undo.push_back(change);
        }
        while self.undo.len() > MAX_UNDO
            || (self.undo_bytes > MAX_UNDO_BYTES && self.undo.len() > 1)
        {
            let old = self.undo.pop_front().expect("not empty");
            self.undo_bytes -= old.removed.len() + old.inserted.len();
        }
    }

    fn undo(&mut self) -> Option<Action> {
        let Some(change) = self.undo.pop_back() else {
            return Some(Action::Say("Nothing to undo."));
        };
        self.undo_bytes -= change.removed.len() + change.inserted.len();
        let end = end_of(change.at, &change.inserted);
        self.buf.remove(change.at, end);
        self.buf.insert(change.at, &change.removed);
        (self.caret, self.anchor) = change.before;
        self.caret = self.buf.clamp(self.caret);
        self.anchor = self.buf.clamp(self.anchor);
        self.redo.push(change);
        self.after_history();
        None
    }

    fn redo(&mut self) -> Option<Action> {
        let Some(change) = self.redo.pop() else {
            return Some(Action::Say("Nothing to redo."));
        };
        let end = end_of(change.at, &change.removed);
        if self.buf.len() - change.removed.len() + change.inserted.len() > MAX_TEXT {
            self.redo.push(change);
            return Some(Action::Say(TOO_LARGE));
        }
        self.buf.remove(change.at, end);
        let after = self.buf.insert(change.at, &change.inserted);
        self.caret = after;
        self.anchor = after;
        self.undo_bytes += change.removed.len() + change.inserted.len();
        self.undo.push_back(change);
        self.after_history();
        None
    }

    fn after_history(&mut self) {
        self.extend = false;
        self.revision += 1;
        self.goal = None;
        self.follow = true;
    }

    /// Moves the caret, extending the selection or not. A caret that moves ends the step
    /// typing extends.
    fn set_caret(&mut self, pos: Pos, extend: bool) {
        if pos != self.caret {
            self.extend = false;
        }
        self.caret = pos;
        if !extend {
            self.anchor = pos;
        }
        self.goal = None;
        self.follow = true;
    }

    fn vertical(&mut self, up: bool, lines: usize, shift: bool, widths: &mut Widths) {
        let caret = self.caret;
        let goal = match self.goal {
            Some(goal) => goal,
            None => widths.x_of(self.buf.line(caret.line), caret.col),
        };
        let last = self.buf.line_count() - 1;
        let to = if up {
            match caret.line.checked_sub(lines) {
                Some(line) => line,
                None if caret.line == 0 => {
                    self.set_caret(Pos::default(), shift);
                    return;
                }
                None => 0,
            }
        } else if caret.line == last {
            self.set_caret(self.buf.end(), shift);
            return;
        } else {
            (caret.line + lines).min(last)
        };
        let col = widths.col_at(self.buf.line(to), goal);
        self.set_caret(Pos::new(to, col), shift);
        self.goal = Some(goal);
    }

    /// The position at a point of the text area.
    fn pos_at(&mut self, x: f32, y: f32, widths: &mut Widths) -> Pos {
        let row = (y / self.line_height).floor();
        let line = if row < 0.0 {
            self.top.saturating_sub(1)
        } else {
            self.top + row as usize
        };
        let line = line.min(self.buf.line_count() - 1);
        let col = widths.col_at(self.buf.line(line), x + self.scroll_x);
        Pos::new(line, col)
    }

    fn scroll(&mut self, dx: f32, dy: f32) {
        self.wheel += dy / self.line_height;
        let whole = self.wheel.trunc();
        self.wheel -= whole;
        let last = self.buf.line_count().saturating_sub(1);
        self.top = if whole < 0.0 {
            self.top.saturating_sub(-whole as usize)
        } else {
            (self.top + whole as usize).min(last)
        };
        self.scroll_x = (self.scroll_x + dx).max(0.0);
        self.follow = false;
    }

    /// Brings the caret into view when it moved since the last call, keeping a little room
    /// at the right.
    pub fn follow_caret(&mut self, widths: &mut Widths) {
        if !std::mem::take(&mut self.follow) {
            return;
        }
        let line = self.caret.line;
        if line < self.top {
            self.top = line;
        } else if line >= self.top + self.rows {
            self.top = line + 1 - self.rows;
        }
        let x = widths.x_of(self.buf.line(line), self.caret.col);
        let room = (widths.space() * 4.0).min(self.width / 4.0);
        if x < self.scroll_x {
            self.scroll_x = (x - room).max(0.0);
        } else if x > self.scroll_x + self.width - room {
            self.scroll_x = x + room - self.width;
        }
    }
}

const TOO_LARGE: &str = "The file can't grow beyond 256 KiB.";

fn order(a: Pos, b: Pos) -> (Pos, Pos) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// Where `text` ends when it starts at `at`.
fn end_of(at: Pos, text: &str) -> Pos {
    match text.rfind('\n') {
        Some(i) => Pos::new(at.line + text.matches('\n').count(), text.len() - i - 1),
        None => Pos::new(at.line, at.col + text.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::testing::Cells;

    fn editor(text: &str) -> (Editor, Widths) {
        let mut e = Editor::new(text);
        e.set_view(10, 400.0, 20.0);
        (e, Widths::new(Box::new(Cells)))
    }

    fn keys(e: &mut Editor, w: &mut Widths, keys: &[(Key, Mods)]) {
        for &(key, mods) in keys {
            e.input(Input::Key(key, mods), w);
        }
    }

    fn typed(e: &mut Editor, w: &mut Widths, text: &str) {
        e.input(Input::Text(text.into()), w);
    }

    #[test]
    fn typing_enter_keeps_the_indent_and_tab_is_two_spaces() {
        let (mut e, mut w) = editor("[ui]\n  chrome = \"x\"");
        keys(
            &mut e,
            &mut w,
            &[
                (Key::Down, Mods::NONE),
                (Key::End, Mods::NONE),
                (Key::Enter, Mods::NONE),
            ],
        );
        typed(&mut e, &mut w, "font");
        assert_eq!(e.text(), "[ui]\n  chrome = \"x\"\n  font");
        keys(
            &mut e,
            &mut w,
            &[(Key::Home, Mods::NONE), (Key::Tab, Mods::NONE)],
        );
        assert_eq!(e.buffer().line(2), "    font");
        // Home goes to the indent, then to the line's start.
        keys(
            &mut e,
            &mut w,
            &[(Key::End, Mods::NONE), (Key::Home, Mods::NONE)],
        );
        assert_eq!(e.caret(), Pos::new(2, 4));
        keys(&mut e, &mut w, &[(Key::Home, Mods::NONE)]);
        assert_eq!(e.caret(), Pos::new(2, 0));
        // Control characters in typed text are dropped.
        typed(&mut e, &mut w, "\u{1b}a\u{7}");
        assert_eq!(e.buffer().line(2), "a    font");
    }

    #[test]
    fn selection_with_shift_and_the_mouse_replaced_by_typing() {
        let (mut e, mut w) = editor("hello world");
        keys(&mut e, &mut w, &[(Key::Right, Mods::CTRL_SHIFT)]);
        assert_eq!(e.selection(), Some((Pos::new(0, 0), Pos::new(0, 6))));
        typed(&mut e, &mut w, "big ");
        assert_eq!(e.text(), "big world");
        // The mouse: press at the 4th cell, drag to the end.
        e.input(
            Input::Press {
                x: 41.0,
                y: 5.0,
                shift: false,
            },
            &mut w,
        );
        e.input(Input::Drag { x: 500.0, y: 5.0 }, &mut w);
        assert_eq!(e.selection(), Some((Pos::new(0, 4), Pos::new(0, 9))));
        // Left without Shift collapses to the selection's start.
        keys(&mut e, &mut w, &[(Key::Left, Mods::NONE)]);
        assert_eq!((e.caret(), e.selection()), (Pos::new(0, 4), None));
        e.input(
            Input::Press {
                x: 0.0,
                y: 0.0,
                shift: true,
            },
            &mut w,
        );
        assert_eq!(e.selection(), Some((Pos::new(0, 0), Pos::new(0, 4))));
        keys(&mut e, &mut w, &[(Key::Backspace, Mods::NONE)]);
        assert_eq!(e.text(), "world");
        // Below the last line is the last line.
        e.input(
            Input::Press {
                x: 500.0,
                y: 999.0,
                shift: false,
            },
            &mut w,
        );
        assert_eq!(e.caret(), Pos::new(0, 5));
    }

    #[test]
    fn copy_cut_paste_and_select_all() {
        let (mut e, mut w) = editor("one two");
        assert_eq!(
            e.input(Input::Key(Key::Char('c'), Mods::CTRL), &mut w),
            None,
            "nothing selected"
        );
        keys(&mut e, &mut w, &[(Key::End, Mods::SHIFT)]);
        assert_eq!(
            e.input(Input::Key(Key::Char('c'), Mods::CTRL), &mut w),
            Some(Action::Copy("one two".into()))
        );
        keys(
            &mut e,
            &mut w,
            &[(Key::Home, Mods::NONE), (Key::Right, Mods::CTRL_SHIFT)],
        );
        assert_eq!(
            e.input(Input::Key(Key::Char('x'), Mods::CTRL), &mut w),
            Some(Action::Copy("one ".into()))
        );
        assert_eq!(e.text(), "two");
        e.input(Input::Paste("a\r\nb\u{1b}[200~\tc\n".into()), &mut w);
        assert_eq!(e.text(), "a\nb[200~\tc\ntwo");
        assert_eq!(e.caret(), Pos::new(2, 0));
        keys(&mut e, &mut w, &[(Key::Char('A'), Mods::CTRL)]);
        assert_eq!(e.selection(), Some((Pos::new(0, 0), Pos::new(2, 3))));
        assert_eq!(
            e.input(Input::Key(Key::Char('s'), Mods::CTRL), &mut w),
            Some(Action::Save)
        );
        // A character key without Ctrl does nothing: its text comes as text.
        assert_eq!(
            e.input(Input::Key(Key::Char('q'), Mods::NONE), &mut w),
            None
        );
        assert_eq!(e.text(), "a\nb[200~\tc\ntwo");
    }

    #[test]
    fn undo_and_redo_take_back_words_and_pastes() {
        let (mut e, mut w) = editor("");
        for c in "zoom = 2".chars() {
            typed(&mut e, &mut w, &c.to_string());
        }
        e.input(Input::Paste("\nx".into()), &mut w);
        let undo = (Key::Char('z'), Mods::CTRL);
        let redo = (Key::Char('Z'), Mods::CTRL_SHIFT);
        keys(&mut e, &mut w, &[undo]);
        assert_eq!(e.text(), "zoom = 2");
        keys(&mut e, &mut w, &[undo]);
        assert_eq!(e.text(), "zoom =");
        keys(&mut e, &mut w, &[undo]);
        assert_eq!(e.text(), "zoom");
        keys(&mut e, &mut w, &[undo]);
        assert_eq!(e.text(), "");
        assert_eq!(
            e.input(Input::Key(undo.0, undo.1), &mut w),
            Some(Action::Say("Nothing to undo."))
        );
        keys(&mut e, &mut w, &[redo, redo]);
        assert_eq!(e.text(), "zoom =");
        assert_eq!(e.caret(), Pos::new(0, 6));
        keys(&mut e, &mut w, &[(Key::Char('y'), Mods::CTRL), redo]);
        assert_eq!(e.text(), "zoom = 2\nx");
        // An edit after undo drops what could be redone.
        keys(&mut e, &mut w, &[undo]);
        typed(&mut e, &mut w, "!");
        assert_eq!(
            e.input(Input::Key(redo.0, redo.1), &mut w),
            Some(Action::Say("Nothing to redo."))
        );
        // Backspaces in a row are one step; moving the caret starts another.
        let (mut e, mut w) = editor("abcdef");
        keys(&mut e, &mut w, &[(Key::End, Mods::NONE)]);
        keys(
            &mut e,
            &mut w,
            &[(Key::Backspace, Mods::NONE), (Key::Backspace, Mods::NONE)],
        );
        keys(
            &mut e,
            &mut w,
            &[
                (Key::Home, Mods::NONE),
                (Key::Delete, Mods::NONE),
                (Key::Delete, Mods::NONE),
            ],
        );
        assert_eq!(e.text(), "cd");
        keys(&mut e, &mut w, &[undo]);
        assert_eq!(e.text(), "abcd");
        keys(&mut e, &mut w, &[undo]);
        assert_eq!(e.text(), "abcdef");
        assert_eq!(e.caret(), Pos::new(0, 6));
    }

    #[test]
    fn the_history_is_bounded() {
        let (mut e, mut w) = editor("");
        for _ in 0..MAX_UNDO + 50 {
            e.input(Input::Paste("x".into()), &mut w);
        }
        assert_eq!(e.undo.len(), MAX_UNDO);
        let (mut e, mut w) = editor("");
        let big = "y".repeat(200 * 1024);
        for _ in 0..40 {
            e.input(Input::Paste(big.clone()), &mut w);
            keys(
                &mut e,
                &mut w,
                &[(Key::Char('a'), Mods::CTRL), (Key::Delete, Mods::NONE)],
            );
        }
        assert!(e.undo_bytes <= MAX_UNDO_BYTES, "{}", e.undo_bytes);
        assert_eq!(
            e.undo_bytes,
            e.undo
                .iter()
                .map(|c| c.removed.len() + c.inserted.len())
                .sum::<usize>()
        );
    }

    #[test]
    fn the_text_is_capped() {
        let (mut e, mut w) = editor(&"x".repeat(MAX_TEXT - 2));
        keys(&mut e, &mut w, &[(Key::End, Mods::CTRL)]);
        typed(&mut e, &mut w, "ab");
        assert_eq!(e.buffer().len(), MAX_TEXT);
        assert_eq!(
            e.input(Input::Text("c".into()), &mut w),
            Some(Action::Say(TOO_LARGE))
        );
        assert_eq!(
            e.input(Input::Paste("cd".into()), &mut w),
            Some(Action::Say(TOO_LARGE))
        );
        assert_eq!(e.buffer().len(), MAX_TEXT);
        // Replacing a selection with something no longer still works.
        keys(&mut e, &mut w, &[(Key::Left, Mods::SHIFT)]);
        typed(&mut e, &mut w, "z");
        assert!(e.text().ends_with("az"));
    }

    #[test]
    fn up_and_down_keep_the_column_across_short_and_wide_lines() {
        let (mut e, mut w) = editor("abcdef\nab\n中中中\nabcdef");
        keys(
            &mut e,
            &mut w,
            &[(Key::End, Mods::NONE), (Key::Left, Mods::NONE)],
        );
        assert_eq!(e.caret(), Pos::new(0, 5));
        keys(&mut e, &mut w, &[(Key::Down, Mods::NONE)]);
        assert_eq!(e.caret(), Pos::new(1, 2));
        keys(&mut e, &mut w, &[(Key::Down, Mods::NONE)]);
        // x 50 is the middle of the third 20-wide character (40..60): the caret goes after it.
        assert_eq!(e.caret(), Pos::new(2, 9));
        keys(&mut e, &mut w, &[(Key::Down, Mods::NONE)]);
        assert_eq!(e.caret(), Pos::new(3, 5));
        keys(&mut e, &mut w, &[(Key::Down, Mods::NONE)]);
        assert_eq!(
            e.caret(),
            Pos::new(3, 6),
            "down on the last line goes to its end"
        );
        keys(
            &mut e,
            &mut w,
            &[(Key::Up, Mods::SHIFT), (Key::Up, Mods::SHIFT)],
        );
        assert_eq!(e.selection(), Some((Pos::new(1, 2), Pos::new(3, 6))));
        keys(&mut e, &mut w, &[(Key::Home, Mods::CTRL)]);
        keys(&mut e, &mut w, &[(Key::Up, Mods::NONE)]);
        assert_eq!(e.caret(), Pos::new(0, 0));
        keys(&mut e, &mut w, &[(Key::End, Mods::CTRL)]);
        assert_eq!(e.caret(), Pos::new(3, 6));
    }

    #[test]
    fn the_view_follows_the_caret_and_the_wheel_scrolls_it() {
        let text: Vec<String> = (0..100).map(|n| format!("line {n}")).collect();
        let (mut e, mut w) = editor(&text.join("\n"));
        keys(&mut e, &mut w, &[(Key::PageDown, Mods::NONE)]);
        assert_eq!(e.caret().line, 9);
        e.follow_caret(&mut w);
        assert_eq!(e.top, 9, "page down moves the view by a page too");
        keys(&mut e, &mut w, &[(Key::End, Mods::CTRL)]);
        e.follow_caret(&mut w);
        assert_eq!(e.top, 90);
        keys(&mut e, &mut w, &[(Key::PageUp, Mods::NONE)]);
        e.follow_caret(&mut w);
        assert_eq!((e.caret().line, e.top), (90, 81));
        // The wheel moves the view and leaves the caret, by whole lines, fractions kept.
        e.input(Input::Wheel { dx: 0.0, dy: -30.0 }, &mut w);
        e.input(Input::Wheel { dx: 0.0, dy: -10.0 }, &mut w);
        e.follow_caret(&mut w);
        assert_eq!((e.caret().line, e.top), (90, 79));
        e.input(
            Input::Wheel {
                dx: 0.0,
                dy: -99999.0,
            },
            &mut w,
        );
        e.follow_caret(&mut w);
        assert_eq!(e.top, 0);
        // Sideways: a long line scrolls to keep the caret in view, and back.
        let (mut e, mut w) = editor(&"x".repeat(100));
        keys(&mut e, &mut w, &[(Key::End, Mods::NONE)]);
        e.follow_caret(&mut w);
        assert_eq!(e.scroll_x, 1000.0 + 40.0 - 400.0);
        keys(&mut e, &mut w, &[(Key::Home, Mods::NONE)]);
        e.follow_caret(&mut w);
        assert_eq!(e.scroll_x, 0.0);
        e.input(Input::Wheel { dx: 50.0, dy: 0.0 }, &mut w);
        assert_eq!(e.scroll_x, 50.0);
        // A click in a scrolled line lands on the right column.
        e.input(
            Input::Press {
                x: 1.0,
                y: 0.0,
                shift: false,
            },
            &mut w,
        );
        assert_eq!(e.caret(), Pos::new(0, 5));
    }

    #[test]
    fn word_deletion_and_replacing_everything() {
        let (mut e, mut w) = editor("alpha beta");
        keys(
            &mut e,
            &mut w,
            &[(Key::End, Mods::NONE), (Key::Backspace, Mods::CTRL)],
        );
        assert_eq!(e.text(), "alpha ");
        keys(
            &mut e,
            &mut w,
            &[(Key::Home, Mods::NONE), (Key::Delete, Mods::CTRL)],
        );
        assert_eq!(
            e.text(),
            "",
            "to the next word's start, here the line's end"
        );
        assert!(e.replace_all("version = 1\nzoom = 2\n"));
        assert_eq!(e.text(), "version = 1\nzoom = 2\n");
        keys(&mut e, &mut w, &[(Key::Char('z'), Mods::CTRL)]);
        assert_eq!(
            e.text(),
            "",
            "replacing everything is one step undo takes back"
        );
        let r = e.revision();
        keys(&mut e, &mut w, &[(Key::Right, Mods::NONE)]);
        assert_eq!(e.revision(), r, "moving changes no text");
    }
}
