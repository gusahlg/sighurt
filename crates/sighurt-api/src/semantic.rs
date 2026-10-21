//! A document's content as structure, for frontends that don't show the engine's pixels: the
//! terminal's text mode and `sig --dump` (TUI.md §3 and §8).
//!
//! An engine that declares [`EngineCapabilities::semantics`](crate::engine::EngineCapabilities)
//! extracts nothing until it is asked with
//! [`EngineCommand::SetSemantics`](crate::engine::EngineCommand::SetSemantics). Then it reports
//! [`EngineEvent::SemanticsChanged`](crate::events::EngineEvent::SemanticsChanged) with a whole
//! [`SemanticSnapshot`] after a document loads, after each action, and when the document changes
//! in a way it can tell; a snapshot replaces the last. A frontend acts on a node with
//! [`EngineCommand::Semantic`](crate::engine::EngineCommand::Semantic), naming the document of
//! the snapshot it acted on: an engine drops an action whose document is no longer its view's,
//! and refuses one on a node that is gone or doesn't take it.
//!
//! Everything in a snapshot is page data: untrusted, as titles and URLs are. An engine stamps a
//! snapshot with the document it extracted from (never with anything the page said), keeps it
//! within the bounds below while it extracts, and never puts a password field's value in it.
//! [`SemanticSnapshot::check`] verifies the bounds and the tree's shape again; the runtime drops
//! a snapshot that fails it. Strings are not cleaned of control characters here: a frontend that
//! writes them to a terminal does that.
//!
//! Owns: the snapshot's shape, its bounds and the actions. Must not own: extraction (engines),
//! layout into cells (the terminal frontend) or policy (the core).

use std::collections::HashSet;
use std::fmt;

use crate::ids::DocumentGeneration;

/// Most nodes in a snapshot. A bigger document is cut off and marked
/// [`truncated`](SemanticSnapshot::truncated).
pub const MAX_NODES: usize = 20_000;
/// Deepest nesting below the root.
pub const MAX_DEPTH: usize = 256;
/// Most bytes of all strings of a snapshot together.
pub const MAX_TOTAL_BYTES: usize = 1 << 20;
/// Most bytes of one string. Longer text is cut (at a character boundary) before it is sent; a
/// longer [`href`](SemanticNode::href) is left out, never cut, since a cut URL points elsewhere.
pub const MAX_STRING_BYTES: usize = 16 * 1024;
/// Most options of one [`NodeKind::Select`].
pub const MAX_OPTIONS: usize = 1_000;

/// A node of one document, assigned by the engine. Stable while the document lives, so an
/// action can name a node of an older snapshot of the same document; meaningless across
/// documents.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);

/// A document's content at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct SemanticSnapshot {
    /// The document this was extracted from: the view's document when extraction started.
    pub document: DocumentGeneration,
    /// The document node. It is in [`nodes`](Self::nodes) and no node's child.
    pub root: NodeId,
    /// Every node, each once, in document order.
    pub nodes: Vec<SemanticNode>,
    /// The node that has keyboard focus in the page, if any.
    pub focus: Option<NodeId>,
    /// Whether the document had more than the bounds allow and was cut off.
    pub truncated: bool,
}

/// One node.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SemanticNode {
    /// Its id.
    pub id: NodeId,
    /// What it is.
    pub kind: NodeKind,
    /// Its own text: a text run's characters, an image's alternative text, a button's caption.
    /// Text in its children is theirs.
    pub text: Option<String>,
    /// Its children, in order.
    pub children: Vec<NodeId>,
    /// A link's absolute URL.
    pub href: Option<String>,
    /// A form control's current value; never a password field's.
    pub value: Option<String>,
    /// A form control's accessible name (its label), or a landmark's.
    pub label: Option<String>,
    /// A checkbox's or radio button's state.
    pub checked: Option<bool>,
    /// A select's options, in order, and the selected one's index.
    pub options: Vec<String>,
    /// The selected option of a select.
    pub selected: Option<u32>,
    /// Whether the control is disabled.
    pub disabled: bool,
    /// Whether a text field is a password field. Its value is never sent.
    pub password: bool,
}

/// What a node is: how a frontend lays it out and what it can do with it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// The document; the root.
    Document,
    /// A heading of level 1 to 6.
    Heading(u8),
    /// A paragraph.
    Paragraph,
    /// A run of text.
    Text,
    /// A link: [`href`](SemanticNode::href) when it has one.
    Link,
    /// A button, including a submit button.
    Button,
    /// A single-line text field.
    TextField,
    /// A multi-line text field.
    TextArea,
    /// A checkbox.
    Checkbox,
    /// A radio button.
    Radio,
    /// A choice among [`options`](SemanticNode::options).
    Select,
    /// An image, with its alternative text as [`text`](SemanticNode::text).
    Image,
    /// A list.
    List,
    /// An item of a list.
    ListItem,
    /// A table.
    Table,
    /// A row of a table.
    Row,
    /// A cell of a row.
    Cell,
    /// Preformatted text: whitespace and line breaks are kept.
    Pre,
    /// Inline code.
    Code,
    /// A quotation block.
    Quote,
    /// A thematic break.
    Separator,
    /// A landmark region.
    Landmark(Landmark),
    /// Anything else that groups children.
    #[default]
    Container,
}

/// The kind of a landmark region.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Landmark {
    /// The page's banner (`header`).
    Banner,
    /// Navigation (`nav`).
    Navigation,
    /// The main content (`main`).
    Main,
    /// Complementary content (`aside`).
    Complementary,
    /// The page's footer (`footer`).
    ContentInfo,
    /// A search region.
    Search,
    /// A form.
    Form,
    /// A named region.
    Region,
}

/// What a frontend asks a node to do.
#[derive(Clone, Debug, PartialEq)]
pub enum SemanticAction {
    /// Activate it as a click would: follow a link, press a button, toggle a checkbox.
    Activate(NodeId),
    /// Give it keyboard focus.
    Focus(NodeId),
    /// Set a text field's or text area's value, as typing would (input and change events).
    SetValue {
        /// The field.
        node: NodeId,
        /// The new value, at most [`MAX_STRING_BYTES`].
        value: String,
    },
    /// Choose a select's option by index.
    SelectOption {
        /// The select.
        node: NodeId,
        /// The option.
        index: u32,
    },
    /// Submit the form a control belongs to, as pressing Enter in it would.
    Submit(NodeId),
    /// Scroll the page so the node is in view.
    ScrollIntoView(NodeId),
}

impl SemanticAction {
    /// The node acted on.
    pub fn node(&self) -> NodeId {
        match self {
            Self::Activate(node)
            | Self::Focus(node)
            | Self::Submit(node)
            | Self::ScrollIntoView(node)
            | Self::SetValue { node, .. }
            | Self::SelectOption { node, .. } => *node,
        }
    }

    /// Whether this is an action only a user's input may cause (it counts as a click), as
    /// opposed to one a frontend takes on its own (focus, scrolling).
    pub fn is_activation(&self) -> bool {
        matches!(
            self,
            Self::Activate(_) | Self::SetValue { .. } | Self::SelectOption { .. } | Self::Submit(_)
        )
    }
}

/// Why a snapshot is malformed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// More than [`MAX_NODES`] nodes, or none.
    NodeCount(usize),
    /// Two nodes share an id.
    DuplicateId(NodeId),
    /// The root, the focus or a child names a node that isn't there.
    UnknownNode(NodeId),
    /// A node is a child twice, the root is a child, or a node can't be reached from the root.
    NotATree(NodeId),
    /// Nesting deeper than [`MAX_DEPTH`].
    TooDeep,
    /// A string over [`MAX_STRING_BYTES`], or all of them over [`MAX_TOTAL_BYTES`].
    TooMuchText,
    /// More than [`MAX_OPTIONS`] options, or a selected index past them.
    Options(NodeId),
    /// A password field with a value.
    PasswordValue(NodeId),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NodeCount(n) => write!(f, "{n} nodes (1 to {MAX_NODES} allowed)"),
            Self::DuplicateId(id) => write!(f, "node {} appears twice", id.0),
            Self::UnknownNode(id) => write!(f, "node {} is named but not there", id.0),
            Self::NotATree(id) => write!(f, "node {} breaks the tree", id.0),
            Self::TooDeep => write!(f, "nesting deeper than {MAX_DEPTH}"),
            Self::TooMuchText => write!(f, "text over its bounds"),
            Self::Options(id) => write!(f, "node {} has too many options or a bad selection", id.0),
            Self::PasswordValue(id) => write!(f, "password field {} carries a value", id.0),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl SemanticSnapshot {
    /// The node with id `id`.
    pub fn node(&self, id: NodeId) -> Option<&SemanticNode> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Checks the bounds and that the nodes form one tree under the root, every node reachable
    /// once. Linear in the snapshot's size.
    pub fn check(&self) -> Result<(), SnapshotError> {
        let count = self.nodes.len();
        if count == 0 || count > MAX_NODES {
            return Err(SnapshotError::NodeCount(count));
        }
        let mut index = std::collections::HashMap::with_capacity(count);
        let mut total = 0usize;
        for (i, node) in self.nodes.iter().enumerate() {
            if index.insert(node.id, i).is_some() {
                return Err(SnapshotError::DuplicateId(node.id));
            }
            for text in [&node.text, &node.href, &node.value, &node.label]
                .into_iter()
                .flatten()
                .chain(&node.options)
            {
                if text.len() > MAX_STRING_BYTES {
                    return Err(SnapshotError::TooMuchText);
                }
                total += text.len();
            }
            if node.options.len() > MAX_OPTIONS
                || node
                    .selected
                    .is_some_and(|selected| selected as usize >= node.options.len())
            {
                return Err(SnapshotError::Options(node.id));
            }
            if node.password && node.value.is_some() {
                return Err(SnapshotError::PasswordValue(node.id));
            }
        }
        if total > MAX_TOTAL_BYTES {
            return Err(SnapshotError::TooMuchText);
        }
        for id in [Some(self.root), self.focus].into_iter().flatten() {
            if !index.contains_key(&id) {
                return Err(SnapshotError::UnknownNode(id));
            }
        }
        // Walk from the root: every child must exist, be reached once, and nothing may be left.
        let mut seen = HashSet::with_capacity(count);
        seen.insert(self.root);
        let mut stack = vec![(self.root, 0usize)];
        while let Some((id, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                return Err(SnapshotError::TooDeep);
            }
            for &child in &self.nodes[index[&id]].children {
                if !index.contains_key(&child) {
                    return Err(SnapshotError::UnknownNode(child));
                }
                if !seen.insert(child) {
                    return Err(SnapshotError::NotATree(child));
                }
                stack.push((child, depth + 1));
            }
        }
        if seen.len() != count {
            let stray = self.nodes.iter().find(|node| !seen.contains(&node.id));
            return Err(SnapshotError::NotATree(
                stray.map_or(self.root, |node| node.id),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: u64, kind: NodeKind, children: &[u64]) -> SemanticNode {
        SemanticNode {
            id: NodeId(id),
            kind,
            children: children.iter().map(|&c| NodeId(c)).collect(),
            ..SemanticNode::default()
        }
    }

    fn snapshot(nodes: Vec<SemanticNode>) -> SemanticSnapshot {
        SemanticSnapshot {
            document: DocumentGeneration::next(),
            root: NodeId(1),
            nodes,
            focus: None,
            truncated: false,
        }
    }

    #[test]
    fn a_tree_passes() {
        let s = snapshot(vec![
            node(1, NodeKind::Document, &[2, 3]),
            node(2, NodeKind::Heading(1), &[4]),
            node(3, NodeKind::Link, &[]),
            node(4, NodeKind::Text, &[]),
        ]);
        assert_eq!(s.check(), Ok(()));
        assert_eq!(s.node(NodeId(3)).map(|n| n.kind), Some(NodeKind::Link));
    }

    #[test]
    fn malformed_trees_fail() {
        let shared = snapshot(vec![
            node(1, NodeKind::Document, &[2, 3]),
            node(2, NodeKind::Paragraph, &[4]),
            node(3, NodeKind::Paragraph, &[4]),
            node(4, NodeKind::Text, &[]),
        ]);
        assert_eq!(shared.check(), Err(SnapshotError::NotATree(NodeId(4))));
        let cycle = snapshot(vec![
            node(1, NodeKind::Document, &[2]),
            node(2, NodeKind::Container, &[1]),
        ]);
        assert_eq!(cycle.check(), Err(SnapshotError::NotATree(NodeId(1))));
        let stray = snapshot(vec![
            node(1, NodeKind::Document, &[]),
            node(2, NodeKind::Text, &[]),
        ]);
        assert_eq!(stray.check(), Err(SnapshotError::NotATree(NodeId(2))));
        let missing = snapshot(vec![node(1, NodeKind::Document, &[9])]);
        assert_eq!(missing.check(), Err(SnapshotError::UnknownNode(NodeId(9))));
        let twice = snapshot(vec![
            node(1, NodeKind::Document, &[]),
            node(1, NodeKind::Text, &[]),
        ]);
        assert_eq!(twice.check(), Err(SnapshotError::DuplicateId(NodeId(1))));
        assert_eq!(snapshot(vec![]).check(), Err(SnapshotError::NodeCount(0)));
    }

    #[test]
    fn bounds_hold() {
        let mut deep: Vec<SemanticNode> = (1..=MAX_DEPTH as u64 + 2)
            .map(|i| node(i, NodeKind::Container, &[i + 1]))
            .collect();
        deep.last_mut().unwrap().children.clear();
        assert_eq!(snapshot(deep).check(), Err(SnapshotError::TooDeep));

        let mut long = node(2, NodeKind::Text, &[]);
        long.text = Some("x".repeat(MAX_STRING_BYTES + 1));
        let s = snapshot(vec![node(1, NodeKind::Document, &[2]), long]);
        assert_eq!(s.check(), Err(SnapshotError::TooMuchText));

        let mut password = node(2, NodeKind::TextField, &[]);
        password.password = true;
        password.value = Some("hunter2".into());
        let s = snapshot(vec![node(1, NodeKind::Document, &[2]), password]);
        assert_eq!(s.check(), Err(SnapshotError::PasswordValue(NodeId(2))));

        let mut select = node(2, NodeKind::Select, &[]);
        select.options = vec!["a".into()];
        select.selected = Some(1);
        let s = snapshot(vec![node(1, NodeKind::Document, &[2]), select]);
        assert_eq!(s.check(), Err(SnapshotError::Options(NodeId(2))));
    }

    #[test]
    fn activations_are_what_a_user_does() {
        assert!(SemanticAction::Activate(NodeId(1)).is_activation());
        assert!(SemanticAction::Submit(NodeId(1)).is_activation());
        assert!(!SemanticAction::Focus(NodeId(1)).is_activation());
        assert!(!SemanticAction::ScrollIntoView(NodeId(1)).is_activation());
        assert_eq!(
            SemanticAction::SetValue {
                node: NodeId(7),
                value: String::new()
            }
            .node(),
            NodeId(7)
        );
    }
}
