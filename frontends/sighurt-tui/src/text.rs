//! The terminal's text mode: each view's page as reflowed text, and what keys and the mouse do
//! to it.
//!
//! Owns: the snapshot, laid-out document and [`TextView`] of every view that has one, the
//! one-line field editor and the option chooser that overlay the page rows, the mapping from
//! keys the keymap left unconsumed to scrolling, focus and activation, and from a click to
//! the same, and drawing it all. Must not own: which keys are bound (`page.send-key` and the
//! `[tui.keys]` table choose what arrives here), the engine (an activation is an [`Act`] the
//! app sends the core as `BrowserEvent::Semantic`), the layout itself
//! ([`crate::textmode`]) or when text mode is on (the app).
//!
//! Enter in the field editor sends `SetValue` when the text changed and `Submit`
//! when that field is inside a form. A value with a newline is not edited here:
//! the one-line editor would have to drop it, so a notice is shown instead.

use std::collections::HashMap;

use sighurt_api::ids::{DocumentGeneration, ViewId};
use sighurt_api::input::{ButtonState, Key, KeyEvent};
use sighurt_api::semantic::{
    Landmark, NodeId, NodeKind, SemanticAction, SemanticSnapshot, MAX_STRING_BYTES,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::chrome::LineEditor;
use crate::grid::{Attrs, Color, Grid};
use crate::sanitize::sanitize;
use crate::textmode::{self, Area, Intent, TextDocument, TextView};

/// The most option rows the chooser shows at once.
const CHOOSER_ROWS: u16 = 8;

/// The longest prefix of `text` that fits `room` bytes, cut at a character boundary.
fn fit(text: &str, room: usize) -> &str {
    let mut end = text.len().min(room);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// How many bytes `editor` can still take: a selected line is replaced by what comes next.
fn room(editor: &LineEditor) -> usize {
    match editor.selected() {
        true => MAX_STRING_BYTES,
        false => MAX_STRING_BYTES.saturating_sub(editor.text().len()),
    }
}

/// A newline cannot be shown in the one-line editor without dropping it.
fn has_newline(text: &str) -> bool {
    text.as_bytes().iter().any(|b| *b == b'\n' || *b == b'\r')
}

/// Whether `target` sits inside a form landmark. The snapshot has no separate
/// flag; the action script still finds a control's form owner from the node.
fn in_form(snapshot: &SemanticSnapshot, target: NodeId) -> bool {
    let mut nodes = HashMap::with_capacity(snapshot.nodes.len());
    for node in &snapshot.nodes {
        nodes.insert(node.id, node);
    }
    let mut stack = vec![(snapshot.root, false)];
    while let Some((id, inside)) = stack.pop() {
        let Some(node) = nodes.get(&id) else {
            continue;
        };
        let inside = inside || matches!(node.kind, NodeKind::Landmark(Landmark::Form));
        if id == target {
            return inside;
        }
        for &child in node.children.iter().rev() {
            stack.push((child, inside));
        }
    }
    false
}

/// Graphemes of `text` whose columns start at `start` and cover at most `room`.
fn slice_columns(text: &str, start: usize, room: usize) -> String {
    let mut skipped = 0usize;
    let mut used = 0usize;
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
    out
}

/// The slice of `text` that fits in `room` columns with `cursor` inside it, and
/// the cursor's column in that slice.
fn window(text: &str, cursor: usize, room: usize) -> (String, usize) {
    if room == 0 {
        return (String::new(), 0);
    }
    let start = cursor.saturating_add(1).saturating_sub(room).min(cursor);
    let visible = slice_columns(text, start, room);
    let cursor_in = cursor.saturating_sub(start);
    (visible, cursor_in.min(room.saturating_sub(1)))
}

/// The editor row and the cursor's column in it. A password is one `*` per
/// grapheme, which is also how far the cursor moves. Anything else is the
/// sanitized value, so a control never reaches the terminal.
fn field_editor_line(editor: &LineEditor, password: bool, cols: usize) -> (String, usize) {
    let prefix = "Edit: ";
    let prefix_cols = prefix.width();
    let shown = if password {
        "*".repeat(editor.text().graphemes(true).count())
    } else {
        sanitize(editor.text())
    };
    let cursor = if password {
        editor.before_cursor().graphemes(true).count()
    } else {
        sanitize(editor.before_cursor()).width()
    };
    if cols == 0 {
        return (String::new(), 0);
    }
    if prefix_cols >= cols {
        let visible = slice_columns(prefix, 0, cols);
        let at = visible
            .width()
            .saturating_sub(1)
            .min(cols.saturating_sub(1));
        return (visible, at);
    }
    let (body, cursor_in) = window(&shown, cursor, cols - prefix_cols);
    let cursor_col = prefix_cols + cursor_in;
    (
        format!("{prefix}{body}"),
        cursor_col.min(cols.saturating_sub(1)),
    )
}

/// An action the user chose in a view's page, for the app to hand the core.
#[derive(Clone, Debug, PartialEq)]
pub struct Act {
    /// The view shown.
    pub view: ViewId,
    /// The document of the snapshot the user saw.
    pub document: DocumentGeneration,
    /// What to do.
    pub action: SemanticAction,
    /// The address of the link it follows, if it does.
    pub link: Option<String>,
}

/// What a key does to the page, when it reaches text mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nav {
    /// One line down.
    Down,
    /// One line up.
    Up,
    /// A page down.
    PageDown,
    /// A page up.
    PageUp,
    /// To the first line.
    Top,
    /// To the last line.
    Bottom,
    /// Focus the next link or control.
    Next,
    /// Focus the previous link or control.
    Previous,
    /// Activate the focused one.
    Activate,
}

impl Nav {
    /// The navigation a key press means, by its W3C key name: the names `page.send-key` presses
    /// and a terminal reports.
    pub fn of(key: &KeyEvent) -> Option<Self> {
        if key.state != ButtonState::Pressed {
            return None;
        }
        let Key::Named(name) = &key.key else {
            return None;
        };
        let shift = key.modifiers.shift;
        Some(match name.as_str() {
            "ArrowDown" => Self::Down,
            "ArrowUp" => Self::Up,
            "PageDown" => Self::PageDown,
            "PageUp" => Self::PageUp,
            "Home" => Self::Top,
            "End" => Self::Bottom,
            "Tab" if shift => Self::Previous,
            "Tab" => Self::Next,
            "Enter" => Self::Activate,
            _ => return None,
        })
    }
}

/// What is being asked of the user over the page.
enum Prompt {
    /// A field's new value. `original` is the snapshot's string, so an edit
    /// that changes nothing does not send `SetValue`.
    Edit {
        node: NodeId,
        editor: LineEditor,
        password: bool,
        original: String,
        in_form: bool,
    },
    /// One of a select's options.
    Choose {
        node: NodeId,
        options: Vec<String>,
        at: usize,
    },
    /// The one-line editor cannot take this field. Any key dismisses it.
    Notice(String),
}

/// One view's page.
struct Page {
    snapshot: Box<SemanticSnapshot>,
    doc: TextDocument,
    view: TextView,
}

/// Every view's page in text mode, and the prompt over the shown one.
pub struct TextPages {
    pages: HashMap<ViewId, Page>,
    prompt: Option<(ViewId, Prompt)>,
    cols: u16,
    rows: u16,
}

impl Default for TextPages {
    fn default() -> Self {
        Self::new(80, 24)
    }
}

impl TextPages {
    /// No pages, for page rows of `cols` by `rows`.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            pages: HashMap::new(),
            prompt: None,
            cols,
            rows,
        }
    }

    /// The page rows changed size: pages are laid out again, keeping their place.
    pub fn set_area(&mut self, cols: u16, rows: u16) {
        self.rows = rows;
        if cols != self.cols {
            self.cols = cols;
            for page in self.pages.values_mut() {
                let doc = textmode::layout(&page.snapshot, cols.max(1));
                page.view.retarget(&page.doc, &doc);
                page.doc = doc;
            }
        }
        for page in self.pages.values_mut() {
            page.view.scroll_lines(&page.doc, rows, 0);
        }
    }

    /// `view` has a new snapshot. The same document keeps its place and focus; another one
    /// starts at the top and drops a prompt that was open on the old one.
    pub fn snapshot(&mut self, view: ViewId, snapshot: Box<SemanticSnapshot>) {
        let doc = textmode::layout(&snapshot, self.cols.max(1));
        match self.pages.get_mut(&view) {
            Some(page) => {
                if page.doc.document != doc.document
                    && self.prompt.as_ref().is_some_and(|(v, _)| *v == view)
                {
                    self.prompt = None;
                }
                page.view.retarget(&page.doc, &doc);
                page.doc = doc;
                page.snapshot = snapshot;
            }
            None => {
                let mut text = TextView::new(doc.width);
                text.scroll_lines(&doc, self.rows, 0);
                self.pages.insert(
                    view,
                    Page {
                        snapshot,
                        doc,
                        view: text,
                    },
                );
            }
        }
    }

    /// `view` has no page any more: it closed, was hidden or text mode ended.
    pub fn forget(&mut self, view: ViewId) {
        self.pages.remove(&view);
        if self.prompt.as_ref().is_some_and(|(v, _)| *v == view) {
            self.prompt = None;
        }
    }

    /// Whether `view` has a page to show.
    pub fn has(&self, view: ViewId) -> bool {
        self.pages.contains_key(&view)
    }

    /// Whether a prompt has the keyboard.
    pub fn prompting(&self) -> bool {
        self.prompt.is_some()
    }

    /// The view's scroll offset and focused item.
    pub fn place(&self, view: ViewId) -> Option<(usize, Option<usize>)> {
        let page = self.pages.get(&view)?;
        Some((page.view.scroll, page.view.focused_item))
    }

    /// A key the keymap left for `view`'s page. Returns the action it chose, if any.
    pub fn page_key(&mut self, view: ViewId, key: &KeyEvent) -> Option<Act> {
        let nav = Nav::of(key)?;
        let rows = self.rows;
        let page = self.pages.get_mut(&view)?;
        let (doc, text) = (&page.doc, &mut page.view);
        match nav {
            Nav::Down => text.scroll_lines(doc, rows, 1),
            Nav::Up => text.scroll_lines(doc, rows, -1),
            Nav::PageDown => text.scroll_pages(doc, rows, 1),
            Nav::PageUp => text.scroll_pages(doc, rows, -1),
            Nav::Top => text.scroll_top(),
            Nav::Bottom => text.scroll_bottom(doc, rows),
            Nav::Next => text.focus_next(doc, rows),
            Nav::Previous => text.focus_previous(doc, rows),
            Nav::Activate => {
                let intent = text.activate(doc)?;
                return self.intent(view, intent);
            }
        }
        None
    }

    /// A click at a cell of the page rows: focuses the link or control there and activates it.
    pub fn click(&mut self, view: ViewId, col: u16, row: u16) -> Option<Act> {
        let page = self.pages.get_mut(&view)?;
        page.view.click(&page.doc, col, row)?;
        let intent = page.view.activate(&page.doc)?;
        self.intent(view, intent)
    }

    /// The wheel: `lines` down, or up when negative.
    pub fn wheel(&mut self, view: ViewId, lines: isize) {
        let rows = self.rows;
        if let Some(page) = self.pages.get_mut(&view) {
            page.view.scroll_lines(&page.doc, rows, lines);
        }
    }

    /// An activation's [`Intent`]: an action to send, or a prompt to open.
    fn intent(&mut self, view: ViewId, intent: Intent) -> Option<Act> {
        let page = self.pages.get(&view)?;
        let document = page.doc.document;
        match intent {
            Intent::Action(action, link) => Some(Act {
                view,
                document,
                action,
                link,
            }),
            Intent::EditField { node, value } => {
                let password = page
                    .doc
                    .items
                    .iter()
                    .any(|item| item.node == node && item.password);
                // A one-line editor would drop the newline on the way into
                // `LineEditor` and commit that. Refuse, so the value stays.
                if !password && has_newline(&value) {
                    self.prompt =
                        Some((view, Prompt::Notice("Can't edit more than one line".into())));
                    return None;
                }
                let in_form = in_form(&page.snapshot, node);
                let original = value;
                let mut editor = LineEditor::from_raw(original.clone());
                editor.select_all();
                self.prompt = Some((
                    view,
                    Prompt::Edit {
                        node,
                        editor,
                        password,
                        original,
                        in_form,
                    },
                ));
                None
            }
            Intent::ChooseOption {
                node,
                options,
                selected,
            } => {
                if !options.is_empty() {
                    let at = selected.map_or(0, |i| (i as usize).min(options.len() - 1));
                    self.prompt = Some((view, Prompt::Choose { node, options, at }));
                }
                None
            }
        }
    }

    /// Pasted text for the open field editor. Returns whether a prompt took it (or refused it).
    pub fn paste(&mut self, text: &str) -> bool {
        match &mut self.prompt {
            Some((_, Prompt::Edit { editor, .. })) => {
                editor.paste(fit(text, room(editor)));
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    /// A key for the open prompt (the keymap never sees it: typing `q` into a field is typing).
    /// Returns the actions the prompt finished with, in the order to send them. Empty when it
    /// is still open, or it took the key and sent nothing. Without a prompt, does nothing.
    pub fn prompt_key(&mut self, key: &KeyEvent) -> Vec<Act> {
        let (view, prompt) = match self.prompt.as_mut() {
            Some(prompt) => prompt,
            None => return Vec::new(),
        };
        if key.state == ButtonState::Released {
            return Vec::new();
        }
        let view = *view;
        let named = match &key.key {
            Key::Named(name) => Some(name.as_str()),
            _ => None,
        };
        if named == Some("Escape") {
            self.prompt = None;
            return Vec::new();
        }
        let done = match prompt {
            Prompt::Notice(_) => Some(Vec::new()),
            Prompt::Edit {
                node,
                editor,
                original,
                in_form,
                ..
            } => {
                let word = key.modifiers.control && !key.consumed.control;
                let alt = key.modifiers.alt && !key.consumed.alt;
                let mut done = None;
                match named {
                    // SetValue and Submit are activations: only this key, which the user
                    // pressed, queues them. An unchanged value sends no SetValue, so a field
                    // is not told it changed. Submit names the field; the action finds its form.
                    Some("Enter") if editor.text().len() <= MAX_STRING_BYTES => {
                        let mut actions = Vec::new();
                        if editor.text() != original.as_str() {
                            actions.push(SemanticAction::SetValue {
                                node: *node,
                                value: editor.text().to_owned(),
                            });
                        }
                        if *in_form {
                            actions.push(SemanticAction::Submit(*node));
                        }
                        done = Some(actions);
                    }
                    Some("Enter") => {}
                    Some("ArrowLeft") if word => editor.word_left(),
                    Some("ArrowRight") if word => editor.word_right(),
                    Some("ArrowLeft") => editor.left(),
                    Some("ArrowRight") => editor.right(),
                    Some("Home") => editor.home(),
                    Some("End") => editor.end(),
                    Some("Backspace") if word => editor.delete_word(false),
                    Some("Delete") if word => editor.delete_word(true),
                    Some("Backspace") => editor.backspace(),
                    Some("Delete") => editor.delete(),
                    Some(_) => {}
                    None => {
                        if let Some(text) = key.text.as_deref().filter(|_| !alt && !word) {
                            editor.insert(fit(text, room(editor)));
                        }
                    }
                }
                done
            }
            Prompt::Choose { node, options, at } => {
                let mut done = None;
                match named {
                    Some("Enter") => {
                        done = Some(vec![SemanticAction::SelectOption {
                            node: *node,
                            index: u32::try_from(*at).unwrap_or(0),
                        }]);
                    }
                    Some("ArrowDown") => *at = (*at + 1).min(options.len() - 1),
                    Some("ArrowUp") => *at = at.saturating_sub(1),
                    Some("Home") => *at = 0,
                    Some("End") => *at = options.len() - 1,
                    _ => {}
                }
                done
            }
        };
        match done {
            Some(actions) => self.finish(view, actions),
            None => Vec::new(),
        }
    }

    /// Closes the prompt and returns `actions` for the document its view shows.
    fn finish(&mut self, view: ViewId, actions: Vec<SemanticAction>) -> Vec<Act> {
        self.prompt = None;
        let Some(document) = self.pages.get(&view).map(|page| page.doc.document) else {
            return Vec::new();
        };
        actions
            .into_iter()
            .map(|action| Act {
                view,
                document,
                action,
                link: None,
            })
            .collect()
    }

    /// Paints `view`'s page into `area`, and its prompt over the bottom rows. Returns the cell
    /// the terminal cursor belongs at while a field is being edited.
    pub fn draw(&self, view: ViewId, grid: &mut Grid, area: Area) -> Option<(u16, u16)> {
        let page = self.pages.get(&view)?;
        textmode::draw(&page.doc, &page.view, grid, area);
        let (prompt_view, prompt) = self.prompt.as_ref()?;
        if *prompt_view != view || area.rows == 0 {
            return None;
        }
        let right = area.col.saturating_add(area.cols).min(grid.cols());
        let blank = |grid: &mut Grid, row: u16, attrs: Attrs| {
            for col in area.col..right {
                grid.put_cluster(col, row, " ", Color::Default, Color::Default, attrs);
            }
        };
        let last = area.row + area.rows - 1;
        match prompt {
            Prompt::Edit {
                editor, password, ..
            } => {
                blank(grid, last, Attrs::default());
                let cols = usize::from(right.saturating_sub(area.col));
                let (text, cursor_col) = field_editor_line(editor, *password, cols);
                grid.put_text(
                    area.col,
                    last,
                    &text,
                    Color::Default,
                    Color::Default,
                    Attrs::default(),
                );
                let column = area
                    .col
                    .saturating_add(u16::try_from(cursor_col).unwrap_or(0));
                Some((column.min(right.saturating_sub(1)), last))
            }
            Prompt::Notice(text) => {
                blank(grid, last, Attrs::default());
                grid.put_text(
                    area.col,
                    last,
                    &sanitize(text),
                    Color::Default,
                    Color::Default,
                    Attrs::default(),
                );
                None
            }
            Prompt::Choose { options, at, .. } => {
                let shown = options
                    .len()
                    .min(usize::from(CHOOSER_ROWS))
                    .min(usize::from(area.rows));
                let first = (*at + 1).saturating_sub(shown);
                for (line, option) in options.iter().skip(first).take(shown).enumerate() {
                    let row = last + 1 - u16::try_from(shown - line).unwrap_or(1);
                    let current = first + line == *at;
                    let attrs = Attrs {
                        reverse: current,
                        ..Attrs::default()
                    };
                    blank(grid, row, attrs);
                    let text = format!("{} {}", if current { ">" } else { " " }, sanitize(option));
                    grid.put_text(area.col, row, &text, Color::Default, Color::Default, attrs);
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_api::input::{KeyCode, Modifiers};
    use sighurt_api::semantic::{Landmark, NodeKind, SemanticNode};

    fn key(name: &str, shift: bool) -> KeyEvent {
        KeyEvent {
            code: KeyCode(0),
            key: Key::Named(name.into()),
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers {
                shift,
                ..Modifiers::default()
            },
            consumed: Modifiers::default(),
            text: None,
        }
    }

    fn typed(text: &str) -> KeyEvent {
        KeyEvent {
            key: Key::Character(text.into()),
            text: Some(text.into()),
            ..key("", false)
        }
    }

    fn node(id: u64, kind: NodeKind) -> SemanticNode {
        SemanticNode {
            id: NodeId(id),
            kind,
            ..SemanticNode::default()
        }
    }

    /// Thirty paragraphs, then a link, a field and a select.
    fn snapshot(document: DocumentGeneration) -> Box<SemanticSnapshot> {
        let mut root = node(1, NodeKind::Document);
        let mut nodes = Vec::new();
        for i in 0..30u64 {
            let mut p = node(100 + i, NodeKind::Paragraph);
            p.text = Some(format!("paragraph {i}"));
            root.children.push(p.id);
            nodes.push(p);
        }
        let mut link = node(2, NodeKind::Link);
        link.text = Some("go".into());
        link.href = Some("https://x.test/".into());
        let mut field = node(3, NodeKind::TextField);
        field.label = Some("Name".into());
        field.value = Some("ab".into());
        let mut select = node(4, NodeKind::Select);
        select.options = vec!["one".into(), "two".into()];
        select.selected = Some(0);
        root.children.extend([link.id, field.id, select.id]);
        nodes.extend([link, field, select]);
        nodes.insert(0, root);
        Box::new(SemanticSnapshot {
            document,
            root: NodeId(1),
            nodes,
            focus: None,
            truncated: false,
        })
    }

    fn pages() -> (TextPages, ViewId, DocumentGeneration) {
        let (view, document) = (ViewId::next(), DocumentGeneration::next());
        let mut pages = TextPages::new(40, 10);
        pages.snapshot(view, snapshot(document));
        (pages, view, document)
    }

    #[test]
    fn named_keys_navigate_and_shift_tab_goes_back() {
        assert_eq!(Nav::of(&key("ArrowDown", false)), Some(Nav::Down));
        assert_eq!(Nav::of(&key("Tab", false)), Some(Nav::Next));
        assert_eq!(Nav::of(&key("Tab", true)), Some(Nav::Previous));
        assert_eq!(Nav::of(&key("Enter", false)), Some(Nav::Activate));
        assert_eq!(Nav::of(&key("F5", false)), None);
        assert_eq!(Nav::of(&typed("j")), None);
        let mut release = key("ArrowDown", false);
        release.state = ButtonState::Released;
        assert_eq!(Nav::of(&release), None);
    }

    #[test]
    fn keys_scroll_and_focus_and_enter_follows_a_link() {
        let (mut pages, view, document) = pages();
        assert!(pages.page_key(view, &key("ArrowDown", false)).is_none());
        assert_eq!(pages.place(view), Some((1, None)));
        pages.page_key(view, &key("End", false));
        assert!(pages.place(view).unwrap().0 > 1);
        pages.page_key(view, &key("Home", false));
        assert_eq!(pages.place(view), Some((0, None)));
        // The first Tab focuses the link, which Enter follows.
        pages.page_key(view, &key("Tab", false));
        let act = pages.page_key(view, &key("Enter", false)).unwrap();
        assert_eq!(act.view, view);
        assert_eq!(act.document, document);
        assert_eq!(act.action, SemanticAction::Activate(NodeId(2)));
        assert_eq!(act.link.as_deref(), Some("https://x.test/"));
        // Shift-Tab wraps from the first item to the last.
        pages.page_key(view, &key("Tab", true));
        assert_eq!(pages.place(view).unwrap().1, Some(2));
    }

    #[test]
    fn a_field_opens_an_editor_whose_enter_sets_the_value() {
        let (mut pages, view, document) = pages();
        pages.page_key(view, &key("Tab", false));
        pages.page_key(view, &key("Tab", false));
        assert!(pages.page_key(view, &key("Enter", false)).is_none());
        assert!(pages.prompting());
        // The value starts selected, so typing replaces it. `q` is text here, not a command.
        assert!(pages.prompt_key(&typed("q")).is_empty());
        assert!(pages.prompt_key(&typed("r")).is_empty());
        pages.prompt_key(&key("Backspace", false));
        let acts = pages.prompt_key(&key("Enter", false));
        assert_eq!(acts.len(), 1);
        assert_eq!(
            acts[0].action,
            SemanticAction::SetValue {
                node: NodeId(3),
                value: "q".into()
            }
        );
        assert_eq!(acts[0].document, document);
        assert!(!pages.prompting());
    }

    #[test]
    fn a_field_never_takes_more_than_the_value_bound() {
        let (mut pages, view, _) = pages();
        pages.page_key(view, &key("Tab", false));
        pages.page_key(view, &key("Tab", false));
        pages.page_key(view, &key("Enter", false));
        // Pastes of 10 KiB of two-byte characters, three times, and typing after.
        let paste = "é".repeat(5 * 1024);
        for _ in 0..3 {
            assert!(pages.paste(&paste));
        }
        pages.prompt_key(&typed("x"));
        let acts = pages.prompt_key(&key("Enter", false));
        let SemanticAction::SetValue { value, .. } = &acts[0].action else {
            panic!("not a value");
        };
        assert!(value.len() <= MAX_STRING_BYTES && value.len() > MAX_STRING_BYTES - 4);
        assert!(value.starts_with('é'));
        assert_eq!(fit("aé", 2), "a");
    }

    #[test]
    fn escape_cancels_a_prompt_and_the_chooser_picks_an_option() {
        let (mut pages, view, _) = pages();
        pages.page_key(view, &key("Tab", true));
        assert!(pages.page_key(view, &key("Enter", false)).is_none());
        assert!(pages.prompting());
        pages.prompt_key(&key("Escape", false));
        assert!(!pages.prompting());
        pages.page_key(view, &key("Enter", false));
        pages.prompt_key(&key("ArrowDown", false));
        pages.prompt_key(&key("ArrowDown", false));
        let acts = pages.prompt_key(&key("Enter", false));
        assert_eq!(
            acts[0].action,
            SemanticAction::SelectOption {
                node: NodeId(4),
                index: 1
            }
        );
    }

    fn row_text(grid: &Grid, row: u16) -> String {
        (0..grid.cols())
            .filter_map(|col| grid.cell(col, row))
            .map(|cell| cell.grapheme.as_str())
            .collect()
    }

    #[test]
    fn a_click_activates_what_is_under_it_and_the_wheel_scrolls() {
        let (mut pages, view, _) = pages();
        pages.wheel(view, 3);
        assert_eq!(pages.place(view), Some((3, None)));
        assert!(pages.click(view, 0, 0).is_none());
        pages.wheel(view, 1000);
        let mut grid = Grid::new(40, 10);
        let area = Area {
            col: 0,
            row: 0,
            cols: 40,
            rows: 10,
        };
        pages.draw(view, &mut grid, area);
        let row = (0..10u16)
            .find(|&r| row_text(&grid, r).starts_with("go"))
            .expect("the link is on screen");
        let act = pages.click(view, 1, row).unwrap();
        assert_eq!(act.action, SemanticAction::Activate(NodeId(2)));
    }

    #[test]
    fn the_editor_overlays_the_last_row_and_masks_a_password() {
        let (mut pages, view, _) = pages();
        pages.page_key(view, &key("Tab", false));
        pages.page_key(view, &key("Tab", false));
        pages.page_key(view, &key("Enter", false));
        let mut grid = Grid::new(40, 10);
        let area = Area {
            col: 0,
            row: 0,
            cols: 40,
            rows: 10,
        };
        let cursor = pages.draw(view, &mut grid, area);
        assert!(row_text(&grid, 9).starts_with("Edit: ab"));
        assert_eq!(cursor, Some((8, 9)));
    }

    #[test]
    fn a_new_document_drops_a_prompt_and_resets_the_place() {
        let (mut pages, view, _) = pages();
        pages.page_key(view, &key("Tab", false));
        pages.page_key(view, &key("Tab", false));
        pages.page_key(view, &key("Enter", false));
        assert!(pages.prompting());
        pages.snapshot(view, snapshot(DocumentGeneration::next()));
        assert!(!pages.prompting());
        assert_eq!(pages.place(view), Some((0, None)));
        pages.forget(view);
        assert!(!pages.has(view));
    }

    /// One field, inside a form landmark when `form` is set.
    fn field_page(
        kind: NodeKind,
        value: &str,
        password: bool,
        form: bool,
    ) -> (TextPages, ViewId, NodeId) {
        let view = ViewId::next();
        let field_id = if form { 3 } else { 2 };
        let mut field = node(field_id, kind);
        if password {
            field.password = true;
        } else {
            field.value = Some(value.to_string());
        }
        let mut root = node(1, NodeKind::Document);
        let nodes = if form {
            let mut form_node = node(2, NodeKind::Landmark(Landmark::Form));
            form_node.children.push(NodeId(field_id));
            root.children.push(NodeId(2));
            vec![root, form_node, field]
        } else {
            root.children.push(NodeId(field_id));
            vec![root, field]
        };
        let mut pages = TextPages::new(40, 10);
        pages.snapshot(
            view,
            Box::new(SemanticSnapshot {
                document: DocumentGeneration::next(),
                root: NodeId(1),
                nodes,
                focus: None,
                truncated: false,
            }),
        );
        (pages, view, NodeId(field_id))
    }

    fn open_field(pages: &mut TextPages, view: ViewId) {
        pages.page_key(view, &key("Tab", false));
        assert!(pages.page_key(view, &key("Enter", false)).is_none());
        assert!(pages.prompting());
    }

    fn draw_last(pages: &TextPages, view: ViewId) -> (String, Option<(u16, u16)>) {
        let mut grid = Grid::new(40, 10);
        let cursor = pages.draw(
            view,
            &mut grid,
            Area {
                col: 0,
                row: 0,
                cols: 40,
                rows: 10,
            },
        );
        (row_text(&grid, 9), cursor)
    }

    #[test]
    fn spaces_and_newlines_keep_their_value_through_edit_and_commit() {
        let (mut pages, view, node) =
            field_page(NodeKind::TextField, "  two  spaces", false, false);
        open_field(&mut pages, view);
        let (row, _) = draw_last(&pages, view);
        assert!(row.starts_with("Edit:   two  spaces"), "{row}");
        assert!(pages.prompt_key(&key("End", false)).is_empty());
        assert!(pages.prompt_key(&typed("!")).is_empty());
        let acts = pages.prompt_key(&key("Enter", false));
        assert_eq!(acts.len(), 1);
        assert_eq!(
            acts[0].action,
            SemanticAction::SetValue {
                node,
                value: "  two  spaces!".into()
            }
        );

        // A tab is a control on screen and a real character in the value.
        let (mut pages, view, node) =
            field_page(NodeKind::TextField, "  two\tspaces", false, false);
        open_field(&mut pages, view);
        let (row, _) = draw_last(&pages, view);
        assert!(!row.chars().any(|ch| ch.is_control()), "{row:?}");
        pages.prompt_key(&key("End", false));
        pages.prompt_key(&typed("!"));
        let acts = pages.prompt_key(&key("Enter", false));
        assert_eq!(
            acts[0].action,
            SemanticAction::SetValue {
                node,
                value: "  two\tspaces!".into()
            }
        );

        // A newline is not edited on one line, so commit cannot flatten it.
        let (mut pages, view, _) = field_page(NodeKind::TextArea, "one\ntwo", false, true);
        open_field(&mut pages, view);
        let (row, cursor) = draw_last(&pages, view);
        assert!(row.starts_with("Can't edit more than one line"), "{row}");
        assert!(cursor.is_none());
        assert!(!row.chars().any(|ch| ch.is_control()), "{row:?}");
        assert!(pages.prompt_key(&typed("!")).is_empty());
        assert!(!pages.prompting());
        assert!(pages.prompt_key(&key("Enter", false)).is_empty());
    }

    #[test]
    fn enter_without_edits_sends_no_set_value() {
        let (mut pages, view, node) = field_page(NodeKind::TextField, "  two  spaces", false, true);
        open_field(&mut pages, view);
        let (row, _) = draw_last(&pages, view);
        assert!(row.starts_with("Edit:   two  spaces"), "{row}");
        let acts = pages.prompt_key(&key("Enter", false));
        assert!(acts
            .iter()
            .all(|act| !matches!(act.action, SemanticAction::SetValue { .. })));
        assert_eq!(acts, vec![expected_submit(&acts, node)]);
        assert!(!pages.prompting());

        let (mut pages, view, _) = field_page(NodeKind::TextArea, "  two  spaces", false, false);
        open_field(&mut pages, view);
        assert!(pages.prompt_key(&key("Enter", false)).is_empty());
        assert!(!pages.prompting());
    }

    fn expected_submit(acts: &[Act], node: NodeId) -> Act {
        let mut act = acts[0].clone();
        act.action = SemanticAction::Submit(node);
        act
    }

    #[test]
    fn enter_in_a_form_field_submits_after_the_value() {
        let (mut pages, view, node) = field_page(NodeKind::TextField, "old", false, true);
        open_field(&mut pages, view);
        assert!(pages.prompt_key(&typed("n")).is_empty());
        let acts = pages.prompt_key(&key("Enter", false));
        assert_eq!(
            acts.iter().map(|act| &act.action).collect::<Vec<_>>(),
            vec![
                &SemanticAction::SetValue {
                    node,
                    value: "n".into()
                },
                &SemanticAction::Submit(node),
            ]
        );
        assert!(acts.iter().all(|act| act.action.is_activation()));
        assert_eq!(acts[0].document, acts[1].document);
        assert!(acts[0].link.is_none());
    }

    #[test]
    fn the_editor_window_follows_the_cursor() {
        let value = format!("<{}>", "x".repeat(298));
        assert_eq!(value.chars().count(), 300);
        let (mut pages, view, _) = field_page(NodeKind::TextField, &value, false, false);
        open_field(&mut pages, view);
        let (row, cursor) = draw_last(&pages, view);
        let (col, at) = cursor.expect("cursor");
        assert_eq!(at, 9);
        assert!(col < 40, "{col}");
        assert!(row.contains('>'), "{row}");
        assert!(!row.contains('<'), "{row}");

        assert!(pages.prompt_key(&key("Home", false)).is_empty());
        let (row, cursor) = draw_last(&pages, view);
        assert_eq!(cursor, Some((6, 9)));
        assert!(row.contains('<'), "{row}");
        assert!(!row.contains('>'), "{row}");

        pages.prompt_key(&key("End", false));
        pages.prompt_key(&typed("Z"));
        let (row, cursor) = draw_last(&pages, view);
        let (col, _) = cursor.expect("cursor");
        assert!(col < 40, "{col}");
        assert!(row.contains('Z'), "{row}");
        assert!(!row.contains('<'), "{row}");

        // One star per grapheme. `😀` is two columns and one star, so the
        // cursor is not the display width of the password.
        let (mut pages, view, _) = field_page(NodeKind::TextField, "", true, false);
        open_field(&mut pages, view);
        pages.prompt_key(&typed("a"));
        pages.prompt_key(&typed("😀"));
        pages.prompt_key(&typed("b"));
        let (row, cursor) = draw_last(&pages, view);
        assert_eq!(row.trim_end(), "Edit: ***");
        assert_eq!(cursor, Some((9, 9)));

        let (mut pages, view, _) = field_page(NodeKind::TextField, "", true, false);
        open_field(&mut pages, view);
        assert!(pages.paste(&"a".repeat(300)));
        let (row, cursor) = draw_last(&pages, view);
        let (col, _) = cursor.expect("cursor");
        assert!(col < 40, "{col}");
        let stars = row.chars().filter(|ch| *ch == '*').count();
        assert!(stars > 0 && stars < 40, "{stars} in {row:?}");
        assert_eq!(col as usize, "Edit: ".len() + stars);
        assert!(!row.contains('a'), "{row}");
    }
}
