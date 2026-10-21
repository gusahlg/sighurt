//! The laid-out document and the styles on its spans.
//!
//! Owns: the line and item types [`layout`](super::layout) fills. Must not
//! own: how a snapshot becomes those lines, how the viewport scrolls, or how
//! cells are painted.

use sighurt_api::ids::DocumentGeneration;
use sighurt_api::semantic::NodeId;

/// How a span is drawn. [`Style::focused`] is left false by layout; drawing
/// sets reverse video for the focused item.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct Style {
    /// Bold (`SGR 1`). Headings set this.
    pub bold: bool,
    /// Underline (`SGR 4`). Headings and links set this.
    pub underline: bool,
    /// A dim foreground. Disabled controls, images and the truncation note.
    pub dim: bool,
    /// A link: coloured as well as underlined.
    pub link: bool,
    /// Reverse video. Layout leaves this false.
    pub focused: bool,
    /// A form control (the brackets, box, or its label).
    pub field: bool,
}

/// One run of text that shares a style.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    /// Sanitized text. Display width is `unicode-width`, graphemes intact.
    pub text: String,
    /// How to draw it.
    pub style: Style,
    /// Index into [`TextDocument::items`] when this run is part of a control.
    pub item: Option<usize>,
}

/// One row of the laid-out page, already wrapped to [`TextDocument::width`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLine {
    /// Left to right. Empty for a blank line.
    pub spans: Vec<Span>,
    /// The block this row came from, when it has one. Scroll anchoring uses it.
    pub node: Option<NodeId>,
}

impl TextLine {
    pub(crate) fn is_blank(&self) -> bool {
        self.spans
            .iter()
            .all(|span| span.text.chars().all(|ch| ch == ' '))
    }
}

/// Columns a control occupies on one line. `end` is exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellRange {
    /// Index into [`TextDocument::lines`].
    pub line: usize,
    /// First column, including indent.
    pub start: u16,
    /// Column just past the control.
    pub end: u16,
}

/// What an interactive item is. Tab order is [`TextDocument::items`] order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemKind {
    /// A link. [`Item::number`] is set when it has an href.
    Link,
    /// A button.
    Button,
    /// A single-line text field.
    TextField,
    /// A multi-line text field.
    TextArea,
    /// A checkbox. Activation toggles it.
    Checkbox,
    /// A radio button. Activation selects it.
    Radio,
    /// A select. Activation asks the app to choose an option.
    Select,
}

/// One link or form control in the laid-out page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The snapshot node. Stable for the document's life.
    pub node: NodeId,
    /// What activating it does.
    pub kind: ItemKind,
    /// Every stretch of columns the control occupies, in line order.
    pub ranges: Vec<CellRange>,
    /// A link's sanitized URL. `None` when it has no href.
    pub href: Option<String>,
    /// A field's sanitized value. Always empty for a password field.
    pub value: String,
    /// A select's sanitized options, in order. Indexes match the snapshot.
    pub options: Vec<String>,
    /// A select's selected option.
    pub selected: Option<u32>,
    /// The control is disabled. It can be focused; activation does nothing.
    pub disabled: bool,
    /// The field is a password. [`Self::value`] is empty and the layout shows
    /// asterisks.
    pub password: bool,
    /// 1-based footnote number of a link that has an href.
    pub number: Option<u32>,
}

/// A snapshot reflowed to a width.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextDocument {
    /// The document the snapshot was stamped with.
    pub document: DocumentGeneration,
    /// The width that was asked for, at least 1.
    pub width: u16,
    /// Wrapped rows, top to bottom. Blank rows have no spans.
    pub lines: Vec<TextLine>,
    /// Links and controls in document order. That order is the Tab order.
    pub items: Vec<Item>,
    /// The snapshot was cut off at the engine's bounds.
    pub truncated: bool,
}

impl TextDocument {
    /// The item built for `node`, if it is interactive.
    pub fn item_index(&self, node: NodeId) -> Option<usize> {
        self.items.iter().position(|item| item.node == node)
    }
}
