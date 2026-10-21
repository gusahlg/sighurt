//! The address and command line editor.
//!
//! Cursor movement, selection, deletion, paste and horizontal scrolling by grapheme cluster
//! (Unicode's extended grapheme clusters, from `unicode-segmentation`), never by indexing
//! UTF-8 bytes: every offset the editor keeps lies on a cluster boundary, so å, a letter with
//! combining marks or a family emoji joined by ZWJs moves and deletes as one character. Word
//! movement follows Unicode's word boundaries. Shows an input method's preedit at the cursor,
//! applies its commits and surrounding-text deletions, and reports its own surrounding text for
//! the input method. Where the caret is on screen comes from the shaped text (see
//! [`crate::text`]); this module keeps the model.
//!
//! Must not own: the clipboard (paste arrives as text from a granted clipboard read).

use std::borrow::Cow;
use std::ops::Range;

use sighurt_api::input::{Preedit, SurroundingText, TextInputEvent};
use unicode_segmentation::{GraphemeCursor, UnicodeSegmentation};

/// The most text reported to an input method as surrounding text, as text-input-v3 allows.
const MAX_SURROUNDING: usize = 4000;

/// The most text the editor holds, in bytes. Longer addresses (big `data:` URLs) are cut, as
/// other browsers' address bars cut what they show: shaping megabytes of text on every edit
/// would take hundreds of megabytes and stall the chrome.
pub const MAX_TEXT: usize = 32 * 1024;

/// A single-line editor: its text, selection, input-method composition and scroll position.
///
/// Offsets are UTF-8 byte offsets on grapheme cluster boundaries. The selection runs between
/// the anchor and the cursor; they are equal when nothing is selected.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextEditor {
    text: String,
    cursor: usize,
    anchor: usize,
    preedit: Option<Preedit>,
    /// The user changed the text since [`TextEditor::set_text`].
    modified: bool,
    /// How far the text is scrolled left, in logical pixels.
    scroll: f32,
}

/// The editor's text as shown: with the preedit inserted at the cursor.
#[derive(Clone, Debug, PartialEq)]
pub struct Display<'a> {
    /// The text to draw.
    pub text: Cow<'a, str>,
    /// Where to draw the caret, as an offset into `text`. `None` while an input method hides it.
    pub caret: Option<usize>,
    /// The selected range of `text`.
    pub selection: Range<usize>,
    /// The preedit's range of `text`, drawn underlined.
    pub preedit: Option<Range<usize>>,
}

impl TextEditor {
    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The cursor's offset.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected range, empty when nothing is selected.
    pub fn selection(&self) -> Range<usize> {
        self.cursor.min(self.anchor)..self.cursor.max(self.anchor)
    }

    /// The selected text, if any.
    pub fn selected_text(&self) -> Option<&str> {
        let range = self.selection();
        (!range.is_empty()).then(|| &self.text[range])
    }

    /// Whether the user changed the text since it was last set.
    pub fn is_modified(&self) -> bool {
        self.modified
    }

    /// The input method's composition, if one is under way.
    pub fn preedit(&self) -> Option<&Preedit> {
        self.preedit.as_ref()
    }

    /// How far the text is scrolled left, in logical pixels.
    pub fn scroll(&self) -> f32 {
        self.scroll
    }

    /// Replaces the text, as when the page's address changes, and marks it unmodified. If
    /// everything was selected (as right after focusing), the new text is selected too, ready to
    /// type over; otherwise the cursor goes to the end. Text beyond [`MAX_TEXT`] is cut.
    pub fn set_text(&mut self, text: &str) {
        let all = self.selection() == (0..self.text.len());
        self.text = truncate(&one_line(text), MAX_TEXT).to_owned();
        self.cursor = self.text.len();
        self.anchor = if all { 0 } else { self.cursor };
        self.preedit = None;
        self.modified = false;
    }

    /// Selects everything.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.cursor = self.text.len();
    }

    /// Moves the cursor to `offset` (or the cluster boundary before it), extending the
    /// selection if `select`.
    pub fn move_to(&mut self, offset: usize, select: bool) {
        self.cursor = snap(&self.text, offset);
        if !select {
            self.anchor = self.cursor;
        }
    }

    /// Moves one grapheme cluster, extending the selection if `select`. Without `select`, a
    /// selection collapses to its start or end instead.
    pub fn move_grapheme(&mut self, forward: bool, select: bool) {
        let range = self.selection();
        let to = match (forward, select || range.is_empty()) {
            (true, false) => range.end,
            (false, false) => range.start,
            (true, true) => next_grapheme(&self.text, self.cursor),
            (false, true) => previous_grapheme(&self.text, self.cursor),
        };
        self.move_to(to, select);
    }

    /// Moves to the end of the next word or the start of the previous one, extending the
    /// selection if `select`.
    pub fn move_word(&mut self, forward: bool, select: bool) {
        let to = if forward {
            next_word_end(&self.text, self.cursor)
        } else {
            previous_word_start(&self.text, self.cursor)
        };
        self.move_to(to, select);
    }

    /// Replaces the selection (or inserts at the cursor) with `text`, dropping control
    /// characters, which a single line can't hold, and what doesn't fit in [`MAX_TEXT`].
    pub fn insert(&mut self, text: &str) {
        let range = self.selection();
        let text = one_line(text);
        let text = truncate(&text, MAX_TEXT - (self.text.len() - range.len()));
        if range.is_empty() && text.is_empty() {
            return;
        }
        self.text.replace_range(range.clone(), text);
        self.modified = true;
        self.move_to(range.start + text.len(), false);
    }

    /// Inserts pasted text: trimmed, with line breaks removed rather than turned into spaces,
    /// so a URL broken across lines comes back whole.
    pub fn paste(&mut self, text: &str) {
        let text: String = text
            .trim()
            .chars()
            .filter(|c| !matches!(c, '\n' | '\r'))
            .collect();
        self.insert(&text);
    }

    /// Deletes the selection, or else the grapheme cluster after or before the cursor.
    pub fn delete_grapheme(&mut self, forward: bool) {
        if self.selection().is_empty() {
            self.anchor = match forward {
                true => next_grapheme(&self.text, self.cursor),
                false => previous_grapheme(&self.text, self.cursor),
            };
        }
        self.insert("");
    }

    /// Deletes the selection, or else up to the end of the next word or the start of the
    /// previous one.
    pub fn delete_word(&mut self, forward: bool) {
        if self.selection().is_empty() {
            self.anchor = match forward {
                true => next_word_end(&self.text, self.cursor),
                false => previous_word_start(&self.text, self.cursor),
            };
        }
        self.insert("");
    }

    /// Applies one input-method update in the order text-input-v3 prescribes: delete around the
    /// cursor, insert the commit, show the preedit.
    pub fn input_method(&mut self, event: &TextInputEvent) {
        if let Some(delete) = event.delete_surrounding {
            if delete.before > 0 || delete.after > 0 {
                let range = self.selection();
                let start = range.start.saturating_sub(delete.before as usize);
                let end = range.end.saturating_add(delete.after as usize);
                self.anchor = floor_char(&self.text, start);
                self.cursor = ceil_char(&self.text, end);
                self.insert("");
            }
        }
        if let Some(commit) = &event.commit {
            self.insert(commit);
        }
        self.preedit = match &event.preedit {
            Some(preedit) if !preedit.text.is_empty() => {
                // Composing replaces the selection, as typing would.
                self.insert("");
                let text = one_line(&preedit.text).into_owned();
                let cursor = preedit.cursor.as_ref().map(|range| {
                    floor_char(&text, range.start)..floor_char(&text, range.end.max(range.start))
                });
                Some(Preedit { text, cursor })
            }
            _ => None,
        };
    }

    /// Drops the composition, as when focus leaves the editor.
    pub fn clear_preedit(&mut self) {
        self.preedit = None;
    }

    /// The text to draw, with the preedit at the cursor.
    pub fn display(&self) -> Display<'_> {
        let Some(preedit) = &self.preedit else {
            return Display {
                text: Cow::Borrowed(&self.text),
                caret: Some(self.cursor),
                selection: self.selection(),
                preedit: None,
            };
        };
        let at = self.cursor;
        let mut text = String::with_capacity(self.text.len() + preedit.text.len());
        text.push_str(&self.text[..at]);
        text.push_str(&preedit.text);
        text.push_str(&self.text[at..]);
        Display {
            text: Cow::Owned(text),
            caret: preedit.cursor.as_ref().map(|range| at + range.start),
            selection: at..at,
            preedit: Some(at..at + preedit.text.len()),
        }
    }

    /// The text around the cursor for the input method: all of it when short, otherwise at most
    /// 4000 bytes around the cursor, cut on character boundaries.
    pub fn surrounding(&self) -> SurroundingText {
        let len = self.text.len();
        let (start, end) = if len <= MAX_SURROUNDING {
            (0, len)
        } else {
            let start = self.cursor.saturating_sub(MAX_SURROUNDING / 2);
            let start = ceil_char(&self.text, start.min(len - MAX_SURROUNDING));
            (start, floor_char(&self.text, start + MAX_SURROUNDING))
        };
        let clamp = |offset: usize| offset.clamp(start, end) - start;
        SurroundingText {
            text: self.text[start..end].to_owned(),
            cursor: clamp(self.cursor),
            anchor: clamp(self.anchor),
        }
    }

    /// Scrolls so the caret at `caret_x` (in unscrolled text coordinates) stays visible in a
    /// field `field_width` wide, holding text `text_width` wide, and so no space is wasted after
    /// the text's end. `caret_width` is room kept for the caret itself.
    pub fn scroll_to_caret(
        &mut self,
        caret_x: f32,
        caret_width: f32,
        text_width: f32,
        field_width: f32,
    ) {
        let mut scroll = self.scroll;
        if caret_x + caret_width - scroll > field_width {
            scroll = caret_x + caret_width - field_width;
        }
        if caret_x < scroll {
            scroll = caret_x;
        }
        let max = (text_width.max(caret_x) + caret_width - field_width).max(0.0);
        self.scroll = scroll.clamp(0.0, max);
    }

    /// Shows the text from its start, as when the editor isn't focused.
    pub fn reset_scroll(&mut self) {
        self.scroll = 0.0;
    }
}

/// `text` with control characters (line breaks, tabs, ...) removed.
fn one_line(text: &str) -> Cow<'_, str> {
    match text.contains(char::is_control) {
        true => Cow::Owned(text.chars().filter(|c| !c.is_control()).collect()),
        false => Cow::Borrowed(text),
    }
}

/// `text` cut to at most `max` bytes, on a grapheme cluster boundary.
fn truncate(text: &str, max: usize) -> &str {
    match text.len() > max {
        true => &text[..snap(text, max)],
        false => text,
    }
}

/// The grapheme cluster boundary after `offset`, or the end.
pub(crate) fn next_grapheme(text: &str, offset: usize) -> usize {
    GraphemeCursor::new(offset, text.len(), true)
        .next_boundary(text, 0)
        .ok()
        .flatten()
        .unwrap_or(text.len())
}

/// The grapheme cluster boundary before `offset`, or the start.
pub(crate) fn previous_grapheme(text: &str, offset: usize) -> usize {
    GraphemeCursor::new(offset, text.len(), true)
        .prev_boundary(text, 0)
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// `offset` if it is a grapheme cluster boundary, else the boundary before it. Offsets past
/// the end become the end.
pub(crate) fn snap(text: &str, offset: usize) -> usize {
    let offset = floor_char(text, offset);
    let on_boundary = GraphemeCursor::new(offset, text.len(), true)
        .is_boundary(text, 0)
        .unwrap_or(true);
    match on_boundary {
        true => offset,
        false => previous_grapheme(text, offset),
    }
}

/// Whether a word segment is a word rather than spaces or punctuation.
fn is_word(segment: &str) -> bool {
    segment.chars().any(char::is_alphanumeric)
}

/// The start of the word before `offset`, skipping spaces and punctuation, or the start.
fn previous_word_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .split_word_bound_indices()
        .rev()
        .find(|(_, segment)| is_word(segment))
        .map_or(0, |(start, _)| start)
}

/// The end of the word after `offset`, skipping spaces and punctuation, or the end.
fn next_word_end(text: &str, offset: usize) -> usize {
    text[offset..]
        .split_word_bound_indices()
        .find(|(_, segment)| is_word(segment))
        .map_or(text.len(), |(start, segment)| {
            offset + start + segment.len()
        })
}

/// `offset`, or the character boundary before it, within `text`.
fn floor_char(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// `offset`, or the character boundary after it, within `text`.
fn ceil_char(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset += 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use sighurt_api::input::DeleteSurrounding;

    use super::*;

    /// An editor holding `text`, with the cursor at its end.
    fn line(text: &str) -> TextEditor {
        let mut editor = TextEditor::default();
        editor.set_text(text);
        editor.move_to(text.len(), false);
        editor
    }

    /// The grapheme clusters the cursor visits from the start, stepping right.
    fn stops(text: &str) -> Vec<usize> {
        let mut editor = line(text);
        editor.move_to(0, false);
        let mut stops = vec![0];
        while editor.cursor() < text.len() {
            editor.move_grapheme(true, false);
            stops.push(editor.cursor());
        }
        stops
    }

    #[test]
    fn swedish_letters_are_single_characters() {
        // Precomposed å, ä, ö are two bytes each.
        let text = "fåäö";
        assert_eq!(stops(text), [0, 1, 3, 5, 7]);
        let mut editor = line(text);
        editor.delete_grapheme(false);
        assert_eq!(editor.text(), "fåä");
        editor.move_grapheme(false, false);
        editor.move_grapheme(false, true);
        assert_eq!(editor.selected_text(), Some("å"));
        editor.insert("ö");
        assert_eq!(editor.text(), "föä");
        assert!(editor.is_modified());
    }

    #[test]
    fn combining_marks_stay_with_their_letter() {
        // a + ring above, o + diaeresis: decomposed å and ö.
        let text = "a\u{30a}o\u{308}!";
        assert_eq!(stops(text), [0, 3, 6, 7]);
        let mut editor = line(text);
        editor.move_to(6, false);
        editor.delete_grapheme(false);
        assert_eq!(editor.text(), "a\u{30a}!");
        // An offset inside a cluster snaps to its start.
        editor.move_to(2, false);
        assert_eq!(editor.cursor(), 0);
        // Typing a combining mark after a letter joins it.
        let mut editor = line("e");
        editor.insert("\u{301}");
        assert_eq!(stops(editor.text()), [0, 3]);
    }

    #[test]
    fn emoji_sequences_are_single_characters() {
        let family = "👩\u{200d}👩\u{200d}👧\u{200d}👦";
        let flag = "🇸🇪";
        let thumbs = "👍🏽";
        let text = format!("x{family}{flag}{thumbs}");
        let expected = [
            0,
            1,
            1 + family.len(),
            1 + family.len() + flag.len(),
            text.len(),
        ];
        assert_eq!(stops(&text), expected);
        let mut editor = line(&text);
        editor.delete_grapheme(false);
        editor.delete_grapheme(false);
        assert_eq!(editor.text(), format!("x{family}"));
        editor.delete_grapheme(false);
        assert_eq!(editor.text(), "x");
    }

    #[test]
    fn words() {
        let text = "https://www.exempel.se/sök?q=blåbär sylt";
        let mut editor = line(text);
        editor.move_word(false, false);
        assert_eq!(&text[editor.cursor()..], "sylt");
        editor.move_word(false, false);
        assert_eq!(&text[editor.cursor()..], "blåbär sylt");
        editor.move_to(text.len(), false);
        editor.move_word(false, true);
        editor.move_word(false, true);
        assert_eq!(editor.selected_text(), Some("blåbär sylt"));
        editor.move_to(0, false);
        editor.move_word(true, false);
        assert_eq!(&text[..editor.cursor()], "https");
        editor.move_word(true, false);
        assert_eq!(&text[..editor.cursor()], "https://www.exempel.se");

        let mut editor = line("rust  borrow checker");
        editor.delete_word(false);
        assert_eq!(editor.text(), "rust  borrow ");
        editor.delete_word(false);
        assert_eq!(editor.text(), "rust  ");
        editor.move_to(0, false);
        editor.delete_word(true);
        assert_eq!(editor.text(), "  ");
    }

    #[test]
    fn arrows_collapse_a_selection() {
        let mut editor = line("abcd");
        editor.select_all();
        editor.move_grapheme(false, false);
        assert_eq!((editor.cursor, editor.anchor), (0, 0));
        editor.move_to(4, true);
        editor.move_grapheme(true, false);
        assert_eq!((editor.cursor, editor.anchor), (4, 4));
        // With nothing selected, the cursor stops at the ends.
        editor.move_grapheme(true, false);
        assert_eq!(editor.cursor(), 4);
        editor.move_grapheme(false, true);
        assert_eq!(editor.selected_text(), Some("d"));
    }

    #[test]
    fn selection_with_paste() {
        let mut editor = line("https://example.com/gammal");
        // Select "gammal" backwards and paste over it; line breaks go, spaces inside stay.
        editor.move_word(false, true);
        assert_eq!(editor.selected_text(), Some("gammal"));
        editor.paste("  ny\r\nsida med å \n");
        assert_eq!(editor.text(), "https://example.com/nysida med å");
        assert_eq!(editor.cursor(), editor.text().len());
        assert_eq!(editor.selection(), editor.cursor()..editor.cursor());
        // Tabs and other control characters never enter the line.
        editor.select_all();
        editor.paste("a\tb\u{7}c");
        assert_eq!(editor.text(), "abc");
        // Pasting nothing over a selection deletes it, like typing would replace it.
        editor.select_all();
        editor.paste("\n");
        assert_eq!(editor.text(), "");
    }

    #[test]
    fn a_new_address_stays_selected() {
        // Focusing selects everything; a redirect meanwhile must not undo that.
        let mut editor = line("https://a.test");
        editor.select_all();
        editor.set_text("https://a.test/");
        assert_eq!(editor.selected_text(), Some("https://a.test/"));
        editor.move_grapheme(false, false);
        editor.set_text("https://b.test/");
        assert_eq!((editor.cursor, editor.anchor), (15, 15));
        assert!(!editor.is_modified());
        // An empty line counts as selected, so a new tab's first address arrives selected.
        let mut editor = line("");
        editor.set_text("sig://home");
        assert_eq!(editor.selected_text(), Some("sig://home"));
    }

    #[test]
    fn input_method_updates() {
        let mut editor = line("sök ");
        // Composing shows the preedit at the cursor without changing the text.
        editor.input_method(&TextInputEvent {
            preedit: Some(Preedit {
                text: "ka".into(),
                cursor: Some(2..2),
            }),
            ..Default::default()
        });
        assert_eq!(editor.text(), "sök ");
        let display = editor.display();
        assert_eq!(display.text, "sök ka");
        assert_eq!(display.caret, Some(7));
        assert_eq!(display.preedit, Some(5..7));
        // A commit replaces the preedit; a deletion before it removes what the method asks.
        editor.input_method(&TextInputEvent {
            delete_surrounding: Some(DeleteSurrounding {
                before: 1,
                after: 0,
            }),
            commit: Some("か".into()),
            preedit: None,
        });
        assert_eq!(editor.text(), "sökか");
        assert_eq!(editor.display().preedit, None);
        // Deletions that would cut a character in half delete the whole character.
        editor.input_method(&TextInputEvent {
            delete_surrounding: Some(DeleteSurrounding {
                before: 2,
                after: 99,
            }),
            ..Default::default()
        });
        assert_eq!(editor.text(), "sök");
        // A hidden preedit cursor hides the caret; an out-of-range one is clamped.
        editor.input_method(&TextInputEvent {
            preedit: Some(Preedit {
                text: "å".into(),
                cursor: None,
            }),
            ..Default::default()
        });
        assert_eq!(editor.display().caret, None);
        editor.input_method(&TextInputEvent {
            preedit: Some(Preedit {
                text: "å".into(),
                cursor: Some(1..9),
            }),
            ..Default::default()
        });
        assert_eq!(editor.preedit().unwrap().cursor, Some(0..2));
    }

    #[test]
    fn the_text_is_bounded() {
        // A huge data: URL is cut, on a character boundary.
        let mut editor = line(&format!("data:,{}", "å".repeat(MAX_TEXT)));
        assert!(editor.text().len() <= MAX_TEXT);
        assert!(editor.text().len() > MAX_TEXT - 2);
        // Typing and pasting stop at the limit; replacing a selection makes room.
        editor.insert("x");
        assert!(editor.text().len() <= MAX_TEXT);
        editor.move_to(0, false);
        editor.move_grapheme(true, true);
        editor.paste("ab");
        assert!(
            editor.text().starts_with("aata:,"),
            "{}",
            &editor.text()[..8]
        );
    }

    #[test]
    fn surrounding_text_is_bounded() {
        let mut editor = line(&"å".repeat(3000));
        editor.move_to(3000, false);
        let surrounding = editor.surrounding();
        assert!(surrounding.text.len() <= MAX_SURROUNDING);
        assert_eq!(&surrounding.text[..surrounding.cursor], "å".repeat(1000));
        assert_eq!(surrounding.anchor, surrounding.cursor);
        let short = line("abc").surrounding();
        assert_eq!((short.text.as_str(), short.cursor), ("abc", 3));
    }

    #[test]
    fn scrolling_keeps_the_caret_visible() {
        let mut editor = line("x");
        // Text 300 wide in a 100 wide field, caret at 250: scroll so it shows at the right edge.
        editor.scroll_to_caret(250.0, 2.0, 300.0, 100.0);
        assert_eq!(editor.scroll(), 152.0);
        // Moving left past the visible start scrolls back.
        editor.scroll_to_caret(120.0, 2.0, 300.0, 100.0);
        assert_eq!(editor.scroll(), 120.0);
        // Text shrinking below the field width scrolls back to the start.
        editor.scroll_to_caret(40.0, 2.0, 40.0, 100.0);
        assert_eq!(editor.scroll(), 0.0);
    }
}
