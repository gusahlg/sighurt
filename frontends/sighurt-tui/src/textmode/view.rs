//! The viewport over a [`TextDocument`]: scrolling, which item has focus, and what activating it
//! means.
//!
//! Owns: the scroll offset, the focused item, their rules (wrap-around, scroll into view,
//! anchoring across a new snapshot), and the [`Intent`] an activation produces. Must not own:
//! which keys do these (the app maps them), the terminal, the line editor or the engine; the app
//! turns an [`Intent`] into a `Semantic` command.

use sighurt_api::ids::DocumentGeneration;
use sighurt_api::semantic::{NodeId, SemanticAction};

use super::{ItemKind, TextDocument};

/// What activating the focused item asks of the app.
#[derive(Clone, Debug, PartialEq)]
pub enum Intent {
    /// Send this action. The link is the URL of a followed link, for the app's routing.
    Action(SemanticAction, Option<String>),
    /// Open the line editor on the field; on commit send `SetValue`.
    EditField {
        /// The field.
        node: NodeId,
        /// Its current value (empty for a password field).
        value: String,
    },
    /// Offer the options; on a choice send `SelectOption`.
    ChooseOption {
        /// The select.
        node: NodeId,
        /// The options, in order.
        options: Vec<String>,
        /// The selected one.
        selected: Option<u32>,
    },
}

/// Where the user is in a text document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextView {
    /// The first visible line.
    pub scroll: usize,
    /// The focused item, an index into [`TextDocument::items`].
    pub focused_item: Option<usize>,
    /// The width the document was laid out to.
    pub width: u16,
    document: Option<DocumentGeneration>,
}

impl TextView {
    /// A view at the top with nothing focused.
    pub fn new(width: u16) -> Self {
        Self {
            width,
            ..Self::default()
        }
    }

    /// The most the view can scroll with `rows` visible.
    pub fn max_scroll(doc: &TextDocument, rows: u16) -> usize {
        doc.lines.len().saturating_sub(usize::from(rows))
    }

    /// Scroll by `delta` lines, stopping at either end.
    pub fn scroll_lines(&mut self, doc: &TextDocument, rows: u16, delta: isize) {
        let max = Self::max_scroll(doc, rows);
        self.scroll = self.scroll.min(max).saturating_add_signed(delta).min(max);
    }

    /// Scroll by `pages` pages of `rows - 1` lines.
    pub fn scroll_pages(&mut self, doc: &TextDocument, rows: u16, pages: isize) {
        let page = isize::try_from(rows.saturating_sub(1).max(1)).unwrap_or(1);
        self.scroll_lines(doc, rows, page.saturating_mul(pages));
    }

    /// Scroll to the first line.
    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    /// Scroll so the last line is the last row.
    pub fn scroll_bottom(&mut self, doc: &TextDocument, rows: u16) {
        self.scroll = Self::max_scroll(doc, rows);
    }

    /// Scroll the least that shows line `line`.
    pub fn show_line(&mut self, doc: &TextDocument, rows: u16, line: usize) {
        let rows = usize::from(rows.max(1));
        if line < self.scroll {
            self.scroll = line;
        } else if line >= self.scroll + rows {
            self.scroll = line + 1 - rows;
        }
        self.scroll = self.scroll.min(Self::max_scroll(doc, rows as u16));
    }

    fn first_line(doc: &TextDocument, item: usize) -> usize {
        doc.items[item].ranges.first().map_or(0, |r| r.line)
    }

    /// Focus `item` and scroll it into view.
    pub fn focus(&mut self, doc: &TextDocument, rows: u16, item: usize) {
        if item < doc.items.len() {
            self.focused_item = Some(item);
            self.show_line(doc, rows, Self::first_line(doc, item));
        }
    }

    /// Focus the next item, wrapping to the first. With nothing focused, the first item on or
    /// below the top row.
    pub fn focus_next(&mut self, doc: &TextDocument, rows: u16) {
        if doc.items.is_empty() {
            return;
        }
        let next = match self.focused_item {
            Some(i) => (i + 1) % doc.items.len(),
            None => (0..doc.items.len())
                .find(|&i| Self::first_line(doc, i) >= self.scroll)
                .unwrap_or(0),
        };
        self.focus(doc, rows, next);
    }

    /// Focus the previous item, wrapping to the last. With nothing focused, the last item that
    /// starts above the bottom row.
    pub fn focus_previous(&mut self, doc: &TextDocument, rows: u16) {
        if doc.items.is_empty() {
            return;
        }
        let n = doc.items.len();
        let prev = match self.focused_item {
            Some(i) => (i + n - 1) % n,
            None => {
                let bottom = self.scroll + usize::from(rows);
                (0..n)
                    .rev()
                    .find(|&i| Self::first_line(doc, i) < bottom)
                    .unwrap_or(n - 1)
            }
        };
        self.focus(doc, rows, prev);
    }

    /// The item drawn at `col` of page row `row` (0 is the first visible line).
    pub fn item_at(&self, doc: &TextDocument, col: u16, row: u16) -> Option<usize> {
        let line = doc.lines.get(self.scroll + usize::from(row))?;
        let mut at = 0usize;
        for span in &line.spans {
            let end = at + unicode_width::UnicodeWidthStr::width(span.text.as_str());
            if usize::from(col) < end {
                return span.item;
            }
            at = end;
        }
        None
    }

    /// Focus the item at a cell, as a click does. Returns it.
    pub fn click(&mut self, doc: &TextDocument, col: u16, row: u16) -> Option<usize> {
        let item = self.item_at(doc, col, row)?;
        self.focused_item = Some(item);
        Some(item)
    }

    /// What activating the focused item asks for; `None` with nothing focused or a disabled item.
    pub fn activate(&self, doc: &TextDocument) -> Option<Intent> {
        let item = doc.items.get(self.focused_item?)?;
        if item.disabled {
            return None;
        }
        Some(match item.kind {
            ItemKind::Link => {
                Intent::Action(SemanticAction::Activate(item.node), item.href.clone())
            }
            ItemKind::Button | ItemKind::Checkbox | ItemKind::Radio => {
                Intent::Action(SemanticAction::Activate(item.node), None)
            }
            ItemKind::TextField | ItemKind::TextArea => Intent::EditField {
                node: item.node,
                value: item.value.clone(),
            },
            ItemKind::Select => Intent::ChooseOption {
                node: item.node,
                options: item.options.clone(),
                selected: item.selected,
            },
        })
    }

    /// A new layout of the same document or a new document replaced `old`. The same document
    /// keeps the focused node and the content at the top row; another one starts at the top.
    pub fn retarget(&mut self, old: &TextDocument, new: &TextDocument) {
        self.width = new.width;
        // The first snapshot of a view has no document yet and counts as this one. A different
        // document, or a view that was showing another one, starts at the top.
        let same =
            old.document == new.document && self.document.is_none_or(|shown| shown == new.document);
        if !same {
            self.scroll = 0;
            self.focused_item = None;
            self.document = Some(new.document);
            return;
        }
        self.document = Some(new.document);
        self.focused_item = self
            .focused_item
            .and_then(|i| old.items.get(i))
            .and_then(|item| new.item_index(item.node));
        // Anchor on the first node at or below the top row, and how far below the top it starts.
        let top = self.scroll;
        let anchor =
            (top..old.lines.len()).find_map(|i| old.lines[i].node.map(|node| (node, i - top)));
        if let Some((node, below)) = anchor {
            // Keep the part of the node that was above the top row, if the top row is inside it.
            let first_old = old
                .lines
                .iter()
                .position(|l| l.node == Some(node))
                .unwrap_or(top);
            let inside = if below == 0 { top - first_old } else { 0 };
            if let Some(first) = new.lines.iter().position(|l| l.node == Some(node)) {
                self.scroll = (first + inside).saturating_sub(below);
            }
        }
        self.scroll = self.scroll.min(new.lines.len().saturating_sub(1));
    }
}
