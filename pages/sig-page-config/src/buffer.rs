//! The text being edited: lines of UTF-8, positions in them, and the boundaries the caret
//! moves by (grapheme clusters, words).
//!
//! A position's column is a byte offset into its line, always on a character boundary, so
//! nothing here can split a code point. Lines hold no line break; the text is the lines
//! joined with `\n` (a `\r\n` file reads with `\r` removed and is written with `\n`).
//!
//! Must not own: the caret, the selection or the history (the editor), or drawing.

use unicode_segmentation::UnicodeSegmentation;

/// A place in the text: a line, and a byte offset into it on a character boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// The text as lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Buffer {
    lines: Vec<String>,
    /// The text's length in bytes, line breaks included.
    len: usize,
}

impl Default for Buffer {
    fn default() -> Self {
        Self::new("")
    }
}

impl Buffer {
    pub fn new(text: &str) -> Self {
        let lines: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
            .collect();
        let len = lines.iter().map(String::len).sum::<usize>() + lines.len() - 1;
        Self { lines, len }
    }

    /// The whole text.
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    /// The text's length in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, line: usize) -> &str {
        self.lines.get(line).map_or("", String::as_str)
    }

    /// The last position.
    pub fn end(&self) -> Pos {
        let line = self.lines.len() - 1;
        Pos::new(line, self.lines[line].len())
    }

    /// `pos` moved into the text: a line that exists, a column on a character boundary.
    pub fn clamp(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.lines.len() - 1);
        let text = &self.lines[line];
        let mut col = pos.col.min(text.len());
        while !text.is_char_boundary(col) {
            col -= 1;
        }
        Pos::new(line, col)
    }

    /// The text from `start` to `end` (ordered).
    pub fn slice(&self, start: Pos, end: Pos) -> String {
        if start.line == end.line {
            return self.lines[start.line][start.col..end.col].to_owned();
        }
        let mut out = self.lines[start.line][start.col..].to_owned();
        for line in &self.lines[start.line + 1..end.line] {
            out.push('\n');
            out.push_str(line);
        }
        out.push('\n');
        out.push_str(&self.lines[end.line][..end.col]);
        out
    }

    /// Inserts `text` at `at` and returns the position after it. `\r` is dropped.
    pub fn insert(&mut self, at: Pos, text: &str) -> Pos {
        let text: std::borrow::Cow<str> = if text.contains('\r') {
            text.replace("\r\n", "\n").replace('\r', "\n").into()
        } else {
            text.into()
        };
        self.len += text.len();
        let tail = self.lines[at.line].split_off(at.col);
        let mut parts = text.split('\n');
        self.lines[at.line].push_str(parts.next().unwrap_or(""));
        let mut line = at.line;
        for part in parts {
            line += 1;
            self.lines.insert(line, part.to_owned());
        }
        let col = self.lines[line].len();
        self.lines[line].push_str(&tail);
        Pos::new(line, col)
    }

    /// Removes the text from `start` to `end` (ordered) and returns it.
    pub fn remove(&mut self, start: Pos, end: Pos) -> String {
        let removed = self.slice(start, end);
        self.len -= removed.len();
        let tail = self.lines[end.line][end.col..].to_owned();
        self.lines.drain(start.line + 1..=end.line);
        let first = &mut self.lines[start.line];
        first.truncate(start.col);
        first.push_str(&tail);
        removed
    }

    /// The position one grapheme cluster after `pos`; the next line's start at a line's end.
    pub fn next_grapheme(&self, pos: Pos) -> Pos {
        let text = self.line(pos.line);
        if pos.col >= text.len() {
            return if pos.line + 1 < self.lines.len() {
                Pos::new(pos.line + 1, 0)
            } else {
                pos
            };
        }
        let step = text[pos.col..].graphemes(true).next().map_or(0, str::len);
        Pos::new(pos.line, pos.col + step)
    }

    /// The position one grapheme cluster before `pos`; the previous line's end at a line's
    /// start.
    pub fn prev_grapheme(&self, pos: Pos) -> Pos {
        if pos.col == 0 {
            return if pos.line > 0 {
                Pos::new(pos.line - 1, self.lines[pos.line - 1].len())
            } else {
                pos
            };
        }
        let text = &self.line(pos.line)[..pos.col];
        let step = text.graphemes(true).next_back().map_or(0, str::len);
        Pos::new(pos.line, pos.col - step)
    }

    /// The start of the next word after `pos` (Ctrl+Right): past the rest of the word or run
    /// of punctuation the caret is in, then past spaces. At a line's end, the next line's
    /// start.
    pub fn next_word(&self, pos: Pos) -> Pos {
        let text = self.line(pos.line);
        if pos.col >= text.len() {
            return self.next_grapheme(pos);
        }
        let mut chars = text[pos.col..].char_indices().peekable();
        let first = chars.peek().map_or(Class::Space, |&(_, c)| class(c));
        let mut col = text.len();
        let mut in_first = first != Class::Space;
        for (i, c) in chars {
            let class = class(c);
            if in_first && class == first {
                continue;
            }
            in_first = false;
            if class != Class::Space {
                col = pos.col + i;
                break;
            }
        }
        Pos::new(pos.line, col)
    }

    /// The start of the word before `pos` (Ctrl+Left): back past spaces, then past the word
    /// or run of punctuation before them. At a line's start, the previous line's end.
    pub fn prev_word(&self, pos: Pos) -> Pos {
        if pos.col == 0 {
            return self.prev_grapheme(pos);
        }
        let text = &self.line(pos.line)[..pos.col];
        let mut col = 0;
        let mut word = None;
        for (i, c) in text.char_indices().rev() {
            let class = class(c);
            match word {
                None if class == Class::Space => continue,
                None => word = Some(class),
                Some(w) if w == class => {}
                Some(_) => {
                    col = i + c.len_utf8();
                    break;
                }
            }
        }
        Pos::new(pos.line, col)
    }

    /// The column after the line's leading spaces and tabs.
    pub fn indent(&self, line: usize) -> usize {
        let text = self.line(line);
        text.len() - text.trim_start_matches([' ', '\t']).len()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' || c == '-' {
        Class::Word
    } else {
        Class::Punct
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_insert_and_remove_keep_the_text_and_its_length() {
        let mut b = Buffer::new("one\r\ntwo\nthree");
        assert_eq!(b.text(), "one\ntwo\nthree");
        assert_eq!(b.len(), 13);
        let end = b.insert(Pos::new(1, 1), "X\nY\n");
        assert_eq!(end, Pos::new(3, 0));
        assert_eq!(b.text(), "one\ntX\nY\nwo\nthree");
        assert_eq!(b.len(), b.text().len());
        let removed = b.remove(Pos::new(0, 2), Pos::new(3, 1));
        assert_eq!(removed, "e\ntX\nY\nw");
        assert_eq!(b.text(), "ono\nthree");
        assert_eq!(b.len(), b.text().len());
        b.insert(Pos::new(0, 0), "a\r\nb\rc");
        assert_eq!(b.text(), "a\nb\ncono\nthree");
        assert_eq!(Buffer::new("").line_count(), 1);
        assert_eq!(Buffer::new("x\n").line_count(), 2);
    }

    #[test]
    fn the_caret_moves_by_grapheme_and_never_splits_a_code_point() {
        // e + combining acute, a flag (two regional indicators), a ZWJ family, CJK.
        let b = Buffer::new("e\u{301}🇸🇪👨‍👩‍👧中\nx");
        let mut pos = Pos::new(0, 0);
        let mut steps = Vec::new();
        loop {
            let next = b.next_grapheme(pos);
            if next.line != 0 {
                break;
            }
            steps.push(next.col - pos.col);
            pos = next;
        }
        assert_eq!(steps, [3, 8, 18, 3]);
        assert_eq!(b.next_grapheme(pos), Pos::new(1, 0));
        assert_eq!(b.prev_grapheme(Pos::new(1, 0)), pos);
        assert_eq!(b.prev_grapheme(pos), Pos::new(0, 29));
        assert_eq!(b.prev_grapheme(Pos::new(0, 3)), Pos::new(0, 0));
        assert_eq!(b.clamp(Pos::new(0, 2)), Pos::new(0, 1));
        assert_eq!(b.clamp(Pos::new(0, 1)), Pos::new(0, 1));
        assert_eq!(b.clamp(Pos::new(9, 9)), Pos::new(1, 1));
    }

    #[test]
    fn words() {
        let b = Buffer::new("  font-size = \"14\" # ü\nnext");
        let mut stops = vec![];
        let mut pos = Pos::new(0, 0);
        while pos.line == 0 {
            pos = b.next_word(pos);
            stops.push(pos);
        }
        let cols: Vec<_> = stops.iter().map(|p| (p.line, p.col)).collect();
        assert_eq!(
            cols,
            [
                (0, 2),
                (0, 12),
                (0, 14),
                (0, 15),
                (0, 17),
                (0, 19),
                (0, 21),
                (0, 23),
                (1, 0)
            ]
        );
        assert_eq!(b.prev_word(Pos::new(0, 11)), Pos::new(0, 2));
        assert_eq!(b.prev_word(Pos::new(0, 2)), Pos::new(0, 0));
        assert_eq!(b.prev_word(Pos::new(1, 0)), Pos::new(0, 23));
        assert_eq!(b.indent(0), 2);
    }
}
