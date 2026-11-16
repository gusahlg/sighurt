//! The interface: the browser's chrome as a small tree of compound components, one tree for the
//! window and one for the terminal over one vocabulary ([`Interface`]), the whole-value check
//! that says whether a tree can be shown ([`Node::check`]), the operations the default
//! experience, the SDK and the configuration file change a tree with ([`Node::place`],
//! [`Node::remove`], [`Node::overlay`], applied together and checked once by
//! [`Interface::change`]) and the presets ([`Interface::standard`], [`Node::terminal`])
//! (docs/rewrite/interface-2026-11-16.md, §1 and its rulings).
//!
//! A component is a whole part of the chrome with its own internal layout and behaviour: the
//! tabs, the toolbar with its address line, the status line, the command line, the terminal's
//! summary line, the page, a label. Containers arrange them: a [`Node::Column`] top to bottom, a
//! [`Node::Row`] left to right, an [`Node::Overlay`] lays passive components over the page
//! without taking room from it. There is no solver: a component's size along its parent's axis
//! is a function of its kind, the density and the font size ([`Size::Auto`]) or a fixed number
//! ([`Size::Fixed`]), never of its text, and the child holding the page takes the rest. Sizes
//! are logical pixels at a 14 px font in the window (scaled with `[ui] font-size`) and cells in
//! the terminal.
//!
//! What a tree must be ([`Node::check`]), so that its frontend can show it, every component in
//! it is one that frontend's interpreter lays out, and every rule of the chrome holds:
//!
//! - at most [`MAX_DEPTH`] deep, [`MAX_NODES`] nodes and [`MAX_LAYERS`] overlay layers;
//! - exactly one [`PageView`]; every container holds it, so a container's children are
//!   components and the one child holding the page;
//! - each other kind at most once;
//! - exactly one editor ([`AddressBar`] in the window, [`CommandLine`] in the terminal), which
//!   takes `location.focus`, `page.find` and a command's error, and something that shows the
//!   status line ([`StatusBar`] or the terminal's [`CommandLine`]): a chrome shows `sig`'s
//!   notices whenever there are any;
//! - the strips ([`AddressBar`], [`StatusBar`], [`CommandLine`], [`Summary`]) in a column; an
//!   overlay layer only a [`StatusBar`] (layers are passive: never the page, the editor or the
//!   tabs); the toolbar's buttons each at most once;
//! - at most [`MAX_CHROME_SURFACES`] chrome surfaces: a maximal run of adjacent components among
//!   a container's children is one surface, a [`StatusBar`] strip always its own, and each
//!   overlay layer one;
//! - in the window: an [`AddressBar`] and a [`StatusBar`], a [`TabBar`] or none; no
//!   [`CommandLine`] (the
//!   line look is the line chrome's, `[ui] chrome = "line"`, until it is ported) and no
//!   [`Summary`] (the terminal's); in the terminal: a single column of components, a
//!   [`Summary`] and a [`CommandLine`], nothing else yet. No [`Label`] yet in either: no
//!   interpreter shows one.
//!
//! The operations are structural and check nothing: a sequence of them is applied together and
//! the result checked once ([`Interface::change`], [`Node::change`]), so an intermediate tree
//! may be one no frontend could show (`remove(StatusBar)` before `overlay(StatusBar)`). A
//! change whose result fails the check changes nothing. An experience with any problem is not
//! used.
//!
//! Must not own: where anything is in pixels or cells (the frontends' interpreters), what a
//! component draws or does, or state of any kind: this is configuration.

use std::fmt;
use std::sync::Arc;

use crate::keymap::{Browser, Frontend};

/// The deepest a tree may be, counting the root and the components: `Row[Column[Overlay[page]]]`.
pub const MAX_DEPTH: usize = 4;
/// The most nodes a tree may have, containers, components and overlay layers together.
pub const MAX_NODES: usize = 24;
/// The most overlay layers a tree may have.
pub const MAX_LAYERS: usize = 2;
/// The most labels a tree may have, once they are shown.
pub const MAX_LABELS: usize = 3;
/// The most surfaces a tree's chrome may need; the page area is one more.
pub const MAX_CHROME_SURFACES: usize = 7;
/// The longest fixed text of a [`Label`], in bytes.
pub const MAX_LABEL_TEXT: usize = 256;

/// The interface of an experience: the window's tree, the terminal's tree, and the window's
/// density.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Interface {
    /// The window's tree, in logical pixels at a 14 px font.
    pub window: Node,
    /// The terminal's tree, in cells.
    pub terminal: Node,
    /// How tall the window's tab strip and toolbar are.
    pub density: Density,
}

impl Default for Interface {
    /// [`Interface::standard`].
    fn default() -> Self {
        Self::standard()
    }
}

impl Interface {
    /// The standard look: tabs at the top, the toolbar, the page, the status strip; the
    /// terminal's summary line, page and command line.
    pub fn standard() -> Self {
        Self {
            window: Node::standard(),
            terminal: Node::terminal(),
            density: Density::Normal,
        }
    }

    /// The tree of `frontend`.
    pub fn tree(&self, frontend: Frontend) -> &Node {
        match frontend {
            Frontend::Gui => &self.window,
            Frontend::Terminal => &self.terminal,
        }
    }

    /// The tree of `frontend`, to change.
    pub fn tree_mut(&mut self, frontend: Frontend) -> &mut Node {
        match frontend {
            Frontend::Gui => &mut self.window,
            Frontend::Terminal => &mut self.terminal,
        }
    }

    /// Applies `change` to a copy of the interface and keeps the result if both trees pass the
    /// check; otherwise changes nothing and returns every problem of the result.
    pub fn change(&mut self, change: impl FnOnce(&mut Interface)) -> Result<(), Vec<String>> {
        let mut changed = self.clone();
        change(&mut changed);
        let problems = changed.check();
        if !problems.is_empty() {
            return Err(problems);
        }
        *self = changed;
        Ok(())
    }

    /// Everything wrong with either tree, one line each, naming the tree; empty when both can
    /// be shown.
    pub fn check(&self) -> Vec<String> {
        let mut problems = Vec::new();
        for (frontend, name) in [(Frontend::Gui, "window"), (Frontend::Terminal, "terminal")] {
            for problem in self.tree(frontend).check(frontend) {
                problems.push(format!("the {name}'s interface: {problem}"));
            }
        }
        problems
    }
}

/// How tall the window's tab strip and toolbar are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Density {
    /// 40 and 44 px at 14 px.
    #[default]
    Normal,
    /// 32 and 36 px at 14 px (`[chrome.standard] compact`).
    Compact,
}

/// A node of a tree.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Node {
    /// Children top to bottom, each as wide as the column.
    Column(Vec<Node>),
    /// Children left to right, each as tall as the row.
    Row(Vec<Node>),
    /// Layers over `base`, taking no room from it.
    Overlay {
        /// What the layers lie over: the page, or a container holding it.
        base: Box<Node>,
        /// The layers, bottom to top.
        layers: Vec<Layer>,
    },
    /// A component.
    Leaf(Component),
}

/// A component over the page, at one of its corners.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    /// The component: a [`StatusBar`] (a [`Label`], once labels are shown).
    pub component: Component,
    /// The corner of the base it sits in.
    pub anchor: Anchor,
    /// When it shows.
    pub show: Show,
}

impl Layer {
    /// `component` at `anchor`, shown as `show` says.
    pub fn new(component: Component, anchor: Anchor, show: Show) -> Self {
        Self {
            component,
            anchor,
            show,
        }
    }
}

/// A corner of an overlay's base.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Anchor {
    /// The bottom-left corner.
    BottomLeft,
    /// The bottom-right corner.
    BottomRight,
    /// The top-left corner.
    TopLeft,
    /// The top-right corner.
    TopRight,
}

/// When an overlay layer shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Show {
    /// Always.
    Always,
    /// Only while it has something to say (the status line: while there is status), and then
    /// it takes no pointer input, so the page under it keeps its hover.
    WhenNeeded,
}

/// Where [`Node::place`] puts a component, relative to the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// First in the outermost column, but below a command line that is first.
    Top,
    /// Last in the outermost column, but above a command line or a status strip that is last
    /// (a command line stays outermost, then a status strip).
    Bottom,
    /// Right before the page, in a row around it.
    Left,
    /// Right after the page, in a row around it.
    Right,
}

/// A component's kind, to find, change or remove it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// [`TabBar`].
    TabBar,
    /// [`AddressBar`].
    AddressBar,
    /// [`StatusBar`].
    StatusBar,
    /// [`CommandLine`].
    CommandLine,
    /// [`Summary`].
    Summary,
    /// [`PageView`].
    PageView,
    /// [`Label`].
    Label,
}

impl Kind {
    /// Every kind.
    pub const ALL: [Kind; 7] = [
        Kind::TabBar,
        Kind::AddressBar,
        Kind::StatusBar,
        Kind::CommandLine,
        Kind::Summary,
        Kind::PageView,
        Kind::Label,
    ];

    /// The kind's name, as the documentation and the problems call it.
    pub fn name(self) -> &'static str {
        match self {
            Kind::TabBar => "TabBar",
            Kind::AddressBar => "AddressBar",
            Kind::StatusBar => "StatusBar",
            Kind::CommandLine => "CommandLine",
            Kind::Summary => "Summary",
            Kind::PageView => "PageView",
            Kind::Label => "Label",
        }
    }

    /// How far out at an end of the outermost column [`Node::place`] keeps it: a command line
    /// outermost, then a status strip, then the rest.
    fn edge_rank(self) -> u8 {
        match self {
            Kind::CommandLine => 2,
            Kind::StatusBar => 1,
            _ => 0,
        }
    }

    /// Whether it is a horizontal strip, which only a column can hold.
    pub fn is_strip(self) -> bool {
        matches!(
            self,
            Kind::AddressBar | Kind::StatusBar | Kind::CommandLine | Kind::Summary
        )
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A component.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Component {
    /// The tabs.
    TabBar(TabBar),
    /// The toolbar with the address line.
    AddressBar(AddressBar),
    /// The status line.
    StatusBar(StatusBar),
    /// One line that is the address editor and the find field when focused.
    CommandLine(CommandLine),
    /// The terminal's top line: `[2/5] title — address · engine · MODE`.
    Summary(Summary),
    /// The page.
    PageView(PageView),
    /// Text of the configuration's.
    Label(Label),
}

impl Component {
    /// Its kind.
    pub fn kind(&self) -> Kind {
        match self {
            Component::TabBar(_) => Kind::TabBar,
            Component::AddressBar(_) => Kind::AddressBar,
            Component::StatusBar(_) => Kind::StatusBar,
            Component::CommandLine(_) => Kind::CommandLine,
            Component::Summary(_) => Kind::Summary,
            Component::PageView(_) => Kind::PageView,
            Component::Label(_) => Kind::Label,
        }
    }

    /// A component of `kind` as the standard look has it; a label empty.
    pub fn new(kind: Kind) -> Self {
        match kind {
            Kind::TabBar => Component::TabBar(TabBar::default()),
            Kind::AddressBar => Component::AddressBar(AddressBar::default()),
            Kind::StatusBar => Component::StatusBar(StatusBar::default()),
            Kind::CommandLine => Component::CommandLine(CommandLine::default()),
            Kind::Summary => Component::Summary(Summary::default()),
            Kind::PageView => Component::PageView(PageView::default()),
            Kind::Label => Component::Label(Label::default()),
        }
    }

    /// Its size along its parent's axis, for the kinds that have one.
    pub fn size(&self) -> Size {
        match self {
            Component::TabBar(tabs) => tabs.size,
            Component::Label(label) => label.size,
            _ => Size::Auto,
        }
    }
}

/// How big a component is along its parent's axis: its height in a column, its width in a row.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Size {
    /// The kind's own size: for a [`TabBar`], 40 px (32 compact) in a column and the smaller
    /// of 220 px and half the row in a row; one line for a [`Label`].
    #[default]
    Auto,
    /// This many logical pixels at 14 px (cells in the terminal), or what is left if less; never
    /// so many that the editor or the status line loses its own size (the window lays those out
    /// first).
    Fixed(u16),
}

/// The tabs: a strip in a column, a list in a row.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabBar {
    /// Its height in a column, its width in a row.
    pub size: Size,
    /// The new-tab button after the last tab.
    pub new_tab: bool,
    /// A close button on each tab while there is more than one.
    pub close_buttons: bool,
    /// The engine's name in each engine page's tab.
    pub engine_label: bool,
}

impl Default for TabBar {
    fn default() -> Self {
        Self {
            size: Size::Auto,
            new_tab: true,
            close_buttons: true,
            engine_label: true,
        }
    }
}

/// The toolbar: buttons and the address line, which doubles as the command line and the find
/// field.
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct AddressBar {
    /// The buttons before the address line, in order, each at most once.
    pub buttons: Vec<Button>,
    /// The mode's indicator ("NORMAL") at the line's right end.
    pub indicator: bool,
    /// The active tab's engine as a button before the indicator.
    pub engine_button: bool,
    /// The search's state ("3 of 12") while a search is under way.
    pub find_state: bool,
}

impl Default for AddressBar {
    fn default() -> Self {
        Self {
            buttons: vec![Button::Back, Button::Forward, Button::Reload],
            indicator: true,
            engine_button: true,
            find_state: true,
        }
    }
}

/// A toolbar button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    /// `page.back`.
    Back,
    /// `page.forward`.
    Forward,
    /// `page.reload`, or `page.stop` while the page loads.
    Reload,
}

/// The status line: hovered links and `sig`'s notices. A strip in a column, a box at a corner
/// as an overlay layer.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct StatusBar {}

/// One line that shows the address and the status, and is the address editor and the find
/// field while it has the keyboard.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CommandLine {
    /// The active tab's engine on the line.
    pub engine_label: bool,
}

impl Default for CommandLine {
    fn default() -> Self {
        Self { engine_label: true }
    }
}

/// The terminal's summary line: the tab's place, title, address, engine and mode.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Summary {}

/// The page: the shown view, or what the chrome draws where no engine shows.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PageView {
    /// Which page this is: [`PageSlot::MAIN`], the only one until there are split views.
    pub slot: PageSlot,
}

/// A page's identity in the tree: the frontends place the pages of a tree as a list of these,
/// of one today.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PageSlot(u8);

impl PageSlot {
    /// The page: the shown tab's view.
    pub const MAIN: PageSlot = PageSlot(0);
}

/// Text of the configuration's, fixed or computed from the browser's state.
#[non_exhaustive]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Label {
    /// Its identity among the tree's labels, for operations that name one.
    pub id: u16,
    /// Its height in a column (one line by default), its width in a row.
    pub size: Size,
    /// What it says.
    pub text: LabelText,
}

impl Label {
    /// The label `id` saying `text`.
    pub fn new(id: u16, text: LabelText) -> Self {
        Self {
            id,
            size: Size::Auto,
            text,
        }
    }
}

/// What a [`Label`] says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LabelText {
    /// This text.
    Fixed(String),
    /// Computed from the browser's state, again whenever what the chrome shows changes.
    Custom(LabelFn),
}

impl Default for LabelText {
    fn default() -> Self {
        LabelText::Fixed(String::new())
    }
}

/// The function of a computed label.
pub type SpansFn = dyn Fn(&dyn Browser) -> Vec<Span> + Send + Sync;

/// A named function of the browser's state that says what a label shows. Like a key's custom
/// action, it does no I/O, reads no clock, starts no thread and logs nothing; what it reads of a
/// page is page text, which the chrome contains before it draws it.
#[derive(Clone)]
pub struct LabelFn {
    /// What it shows, for the documentation ("the tab count").
    pub describe: String,
    /// The function.
    pub run: Arc<SpansFn>,
}

impl LabelFn {
    /// A computed label text described as `describe`.
    pub fn new(
        describe: impl Into<String>,
        run: impl Fn(&dyn Browser) -> Vec<Span> + Send + Sync + 'static,
    ) -> Self {
        Self {
            describe: describe.into(),
            run: Arc::new(run),
        }
    }
}

impl fmt::Debug for LabelFn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LabelFn({:?})", self.describe)
    }
}

/// Equal when they are the same function with the same description.
impl PartialEq for LabelFn {
    fn eq(&self, other: &Self) -> bool {
        self.describe == other.describe && Arc::ptr_eq(&self.run, &other.run)
    }
}

impl Eq for LabelFn {}

/// A run of a label's text in one tone.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Span {
    /// The text.
    pub text: String,
    /// Its tone.
    pub tone: Tone,
}

impl Span {
    /// `text` in `tone`.
    pub fn new(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone,
        }
    }
}

/// The tone of a run of text, in the chrome's colours.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Tone {
    /// Text.
    #[default]
    Text,
    /// Secondary text.
    Muted,
    /// Hints.
    Dim,
    /// Errors.
    Error,
}

impl Node {
    /// The standard window: `Column[TabBar, AddressBar, PageView, StatusBar]`.
    pub fn standard() -> Self {
        Node::Column(vec![
            leaf(Kind::TabBar),
            leaf(Kind::AddressBar),
            leaf(Kind::PageView),
            leaf(Kind::StatusBar),
        ])
    }

    /// The terminal: `Column[Summary, PageView, CommandLine]`.
    pub fn terminal() -> Self {
        Node::Column(vec![
            leaf(Kind::Summary),
            leaf(Kind::PageView),
            leaf(Kind::CommandLine),
        ])
    }

    /// Whether this node is the page, or holds it.
    pub fn holds_page(&self) -> bool {
        match self {
            Node::Leaf(component) => component.kind() == Kind::PageView,
            Node::Column(children) | Node::Row(children) => children.iter().any(Node::holds_page),
            Node::Overlay { base, .. } => base.holds_page(),
        }
    }

    /// Every component of the tree, in tree order, overlay layers after their base, with the
    /// layer each is, if it is one.
    pub fn components(&self) -> Vec<(&Component, Option<&Layer>)> {
        let mut all = Vec::new();
        self.walk(&mut |component, layer| all.push((component, layer)));
        all
    }

    fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Component, Option<&'a Layer>)) {
        match self {
            Node::Leaf(component) => f(component, None),
            Node::Column(children) | Node::Row(children) => {
                for child in children {
                    child.walk(f);
                }
            }
            Node::Overlay { base, layers } => {
                base.walk(f);
                for layer in layers {
                    f(&layer.component, Some(layer));
                }
            }
        }
    }

    /// The first component of `kind`.
    pub fn get(&self, kind: Kind) -> Option<&Component> {
        self.components()
            .into_iter()
            .map(|(component, _)| component)
            .find(|component| component.kind() == kind)
    }

    /// Whether the tree has a component of `kind`.
    pub fn has(&self, kind: Kind) -> bool {
        self.get(kind).is_some()
    }

    /// The first component of `kind`, to change. A change that makes the tree fail
    /// [`Node::check`] (a button twice) is the check's to report.
    pub fn get_mut(&mut self, kind: Kind) -> Option<&mut Component> {
        match self {
            Node::Leaf(component) => (component.kind() == kind).then_some(component),
            Node::Column(children) | Node::Row(children) => {
                children.iter_mut().find_map(|child| child.get_mut(kind))
            }
            Node::Overlay { base, layers } => base.get_mut(kind).or_else(|| {
                layers
                    .iter_mut()
                    .map(|layer| &mut layer.component)
                    .find(|component| component.kind() == kind)
            }),
        }
    }

    /// Removes every component of `kind`, wherever it is, and what that leaves empty. Checks
    /// nothing: see [`Node::change`].
    pub fn remove(&mut self, kind: Kind) {
        *self = self.without(kind);
    }

    /// Puts `component` at `side` of the page ([`Side`]), removing the component of its kind
    /// wherever it is (a label keeps the other labels). Checks nothing: see [`Node::change`].
    pub fn place(&mut self, component: Component, side: Side) {
        if component.kind() != Kind::Label {
            self.remove(component.kind());
        }
        self.insert(component, side);
    }

    /// Lays `component` over the page at `anchor`, removing the component of its kind wherever
    /// it is (a label keeps the other labels). Checks nothing: see [`Node::change`].
    pub fn overlay(&mut self, component: Component, anchor: Anchor, show: Show) {
        if component.kind() != Kind::Label {
            self.remove(component.kind());
        }
        self.layer(Layer::new(component, anchor, show));
    }

    /// Moves the component of `kind` to `side`, keeping what it says about itself, or puts one
    /// as the standard look has it there if the tree has none.
    pub fn position(&mut self, kind: Kind, side: Side) {
        let component = self
            .get(kind)
            .cloned()
            .unwrap_or_else(|| Component::new(kind));
        self.place(component, side);
    }

    /// Applies `change` to a copy of the tree and keeps the result if `frontend` can show it;
    /// otherwise changes nothing and returns every problem of the result.
    pub fn change(
        &mut self,
        frontend: Frontend,
        change: impl FnOnce(&mut Node),
    ) -> Result<(), Vec<String>> {
        let mut changed = self.clone();
        change(&mut changed);
        let problems = changed.check(frontend);
        if !problems.is_empty() {
            return Err(problems);
        }
        *self = changed;
        Ok(())
    }

    /// The tree without components of `kind`, and without the containers and overlays that
    /// leaves empty; a container left with one child is that child.
    fn without(&self, kind: Kind) -> Node {
        fn strip(node: &Node, kind: Kind) -> Option<Node> {
            match node {
                Node::Leaf(component) => (component.kind() != kind).then(|| node.clone()),
                Node::Column(children) | Node::Row(children) => {
                    let mut kept: Vec<Node> = children
                        .iter()
                        .filter_map(|child| strip(child, kind))
                        .collect();
                    match kept.len() {
                        0 => None,
                        1 => kept.pop(),
                        _ if matches!(node, Node::Column(_)) => Some(Node::Column(kept)),
                        _ => Some(Node::Row(kept)),
                    }
                }
                Node::Overlay { base, layers } => {
                    let base = strip(base, kind)?;
                    let layers: Vec<Layer> = layers
                        .iter()
                        .filter(|layer| layer.component.kind() != kind)
                        .cloned()
                        .collect();
                    Some(match layers.is_empty() {
                        true => base,
                        false => Node::Overlay {
                            base: Box::new(base),
                            layers,
                        },
                    })
                }
            }
        }
        strip(self, kind).unwrap_or_else(|| Node::Column(Vec::new()))
    }

    /// Inserts `component` at `side` of the page.
    fn insert(&mut self, component: Component, side: Side) {
        match side {
            Side::Top | Side::Bottom => {
                if !matches!(self, Node::Column(_)) {
                    let inner = std::mem::replace(self, Node::Column(Vec::new()));
                    *self = Node::Column(vec![inner]);
                }
                let Node::Column(children) = self else {
                    unreachable!("made a column above")
                };
                // The edge's own lines stay outermost: a command line, then a status strip.
                let rank = component.kind().edge_rank();
                let outer = |child: &Node| match child {
                    Node::Leaf(c) => c.kind().edge_rank() > rank,
                    _ => false,
                };
                let at = match side {
                    Side::Top => children.iter().take_while(|child| outer(child)).count(),
                    _ => children.len() - children.iter().rev().take_while(|c| outer(c)).count(),
                };
                children.insert(at, Node::Leaf(component));
            }
            Side::Left | Side::Right => {
                let left = side == Side::Left;
                let leaf = Node::Leaf(component);
                if let Some((children, index)) = self.page_slot() {
                    children.insert(if left { index } else { index + 1 }, leaf);
                    return;
                }
                // The page node is the root, or a column's child: a row around it. A tree
                // without its page is refused by the check, whatever happens here.
                let Some(page) = self.page_node_mut() else {
                    return;
                };
                let inner = std::mem::replace(page, Node::Row(Vec::new()));
                *page = Node::Row(match left {
                    true => vec![leaf, inner],
                    false => vec![inner, leaf],
                });
            }
        }
    }

    /// The children of the row the page node (the page, with the overlays directly around it)
    /// is in, and its index there; `None` when it is not in a row.
    fn page_slot(&mut self) -> Option<(&mut Vec<Node>, usize)> {
        if !self.page_parent_is_row() {
            return None;
        }
        fn find(node: &mut Node) -> Option<(&mut Vec<Node>, usize)> {
            match node {
                Node::Row(children) => {
                    let index = children.iter().position(Node::holds_page)?;
                    if is_page_node(&children[index]) {
                        return Some((children, index));
                    }
                    find(&mut children[index])
                }
                Node::Column(children) => children.iter_mut().find_map(find),
                Node::Overlay { base, .. } => find(base),
                Node::Leaf(_) => None,
            }
        }
        find(self)
    }

    /// Whether the page node's parent is a row.
    fn page_parent_is_row(&self) -> bool {
        match self {
            Node::Row(children) | Node::Column(children) => {
                let Some(child) = children.iter().find(|child| child.holds_page()) else {
                    return false;
                };
                if is_page_node(child) {
                    matches!(self, Node::Row(_))
                } else {
                    child.page_parent_is_row()
                }
            }
            Node::Overlay { base, .. } if !is_page_node(self) => base.page_parent_is_row(),
            _ => false,
        }
    }

    /// The page node: the page with the overlays directly around it.
    fn page_node_mut(&mut self) -> Option<&mut Node> {
        if is_page_node(self) {
            return Some(self);
        }
        match self {
            Node::Column(children) | Node::Row(children) => children
                .iter_mut()
                .find(|child| child.holds_page())?
                .page_node_mut(),
            Node::Overlay { base, .. } => base.page_node_mut(),
            Node::Leaf(_) => None,
        }
    }

    /// Adds `layer` over the page: to the overlay directly around it, or one made for it.
    fn layer(&mut self, layer: Layer) {
        fn find(node: &mut Node) -> Option<&mut Node> {
            let here = match &*node {
                Node::Leaf(component) => component.kind() == Kind::PageView,
                Node::Overlay { base, .. } => {
                    matches!(&**base, Node::Leaf(c) if c.kind() == Kind::PageView)
                }
                _ => false,
            };
            if here {
                return Some(node);
            }
            match node {
                Node::Overlay { base, .. } => find(base),
                Node::Column(children) | Node::Row(children) => children.iter_mut().find_map(find),
                Node::Leaf(_) => None,
            }
        }
        // A tree without its page is refused by the check, whatever happens here.
        let Some(page) = find(self) else { return };
        match page {
            Node::Overlay { layers, .. } => layers.push(layer),
            leaf => {
                let base = std::mem::replace(leaf, Node::Column(Vec::new()));
                *leaf = Node::Overlay {
                    base: Box::new(base),
                    layers: vec![layer],
                };
            }
        }
    }

    /// Everything that keeps `frontend` from showing this tree, one line each; empty when it
    /// can (see the module documentation).
    pub fn check(&self, frontend: Frontend) -> Vec<String> {
        let mut problems = Vec::new();
        let mut note = |problem: String| {
            if !problems.contains(&problem) {
                problems.push(problem);
            }
        };

        let depth = self.depth();
        if depth > MAX_DEPTH {
            note(format!("{depth} levels deep; at most {MAX_DEPTH}"));
        }
        let nodes = self.nodes();
        if nodes > MAX_NODES {
            note(format!("{nodes} nodes; at most {MAX_NODES}"));
        }

        let components = self.components();
        let count = |kind: Kind| components.iter().filter(|(c, _)| c.kind() == kind).count();
        let layers = components
            .iter()
            .filter(|(_, layer)| layer.is_some())
            .count();
        if layers > MAX_LAYERS {
            note(format!("{layers} overlay layers; at most {MAX_LAYERS}"));
        }
        match count(Kind::PageView) {
            1 => {}
            n => note(format!("{n} PageViews; exactly one")),
        }
        for kind in Kind::ALL {
            let n = count(kind);
            let most = if kind == Kind::Label { MAX_LABELS } else { 1 };
            if kind != Kind::PageView && n > most {
                note(format!("{n} {kind}s; at most {most}"));
            }
        }
        if count(Kind::Label) > 0 {
            note("a Label: no frontend shows labels yet".into());
        }

        for (component, layer) in &components {
            let kind = component.kind();
            if layer.is_some() && !matches!(kind, Kind::StatusBar | Kind::Label) {
                note(format!(
                    "a {kind} as an overlay layer; layers are passive, only a StatusBar"
                ));
            }
            match component {
                Component::AddressBar(bar) => {
                    for (i, button) in bar.buttons.iter().enumerate() {
                        if bar.buttons[..i].contains(button) {
                            note(format!("the {button:?} button twice"));
                        }
                    }
                }
                Component::Label(Label {
                    text: LabelText::Fixed(text),
                    ..
                }) if text.len() > MAX_LABEL_TEXT => {
                    note(format!(
                        "a label of {} bytes; at most {MAX_LABEL_TEXT}",
                        text.len()
                    ));
                }
                _ => {}
            }
        }

        self.check_shape(None, &mut note);
        let surfaces = self.chrome_surfaces();
        if surfaces > MAX_CHROME_SURFACES {
            note(format!(
                "{surfaces} chrome surfaces; at most {MAX_CHROME_SURFACES}"
            ));
        }

        match frontend {
            Frontend::Gui => {
                if count(Kind::AddressBar) != 1 {
                    note(
                        "no AddressBar: the window needs it for the address, the find field \
                          and a command's error"
                            .into(),
                    );
                }
                if count(Kind::StatusBar) != 1 {
                    note("no StatusBar: the window shows sig's notices on it".into());
                }
                if count(Kind::CommandLine) > 0 {
                    note(
                        "a CommandLine in the window: the line look is the line chrome's \
                          ([ui] chrome = \"line\") until it is a component"
                            .into(),
                    );
                }
                if count(Kind::Summary) > 0 {
                    note("a Summary in the window; it is the terminal's".into());
                }
            }
            Frontend::Terminal => {
                let flat = match self {
                    Node::Column(children) => {
                        children.iter().all(|child| matches!(child, Node::Leaf(_)))
                    }
                    _ => false,
                };
                if !flat {
                    note("the terminal's is a single Column of components".into());
                }
                if count(Kind::CommandLine) != 1 {
                    note(
                        "no CommandLine: the terminal needs it for the address, the find field, \
                          the status and sig's notices"
                            .into(),
                    );
                }
                for kind in [Kind::TabBar, Kind::AddressBar, Kind::StatusBar] {
                    if count(kind) > 0 {
                        note(format!("a {kind} in the terminal; it has none yet"));
                    }
                }
            }
        }
        problems
    }

    /// Notes what is wrong with the shape below this node, whose parent is `parent` (`Some(true)`
    /// for a column).
    fn check_shape(&self, parent: Option<bool>, note: &mut impl FnMut(String)) {
        match self {
            Node::Leaf(component) => {
                let kind = component.kind();
                if kind.is_strip() && parent != Some(true) {
                    note(format!("a {kind} outside a Column; it is a strip"));
                }
            }
            Node::Column(children) | Node::Row(children) => {
                let column = matches!(self, Node::Column(_));
                if children.is_empty() {
                    note("an empty container".into());
                } else if !self.holds_page() {
                    note(
                        "a container without the page; components go directly in the \
                          containers around the page"
                            .into(),
                    );
                }
                for child in children {
                    child.check_shape(Some(column), note);
                }
            }
            Node::Overlay { base, layers } => {
                if !base.holds_page() {
                    note("an Overlay over something other than the page".into());
                }
                if layers.is_empty() {
                    note("an Overlay without layers".into());
                }
                base.check_shape(parent, note);
            }
        }
    }

    /// How deep the tree is, counting this node and the components.
    fn depth(&self) -> usize {
        1 + match self {
            Node::Leaf(_) => 0,
            Node::Column(children) | Node::Row(children) => {
                children.iter().map(Node::depth).max().unwrap_or(0)
            }
            Node::Overlay { base, .. } => base.depth(),
        }
    }

    /// How many nodes the tree has, overlay layers included.
    fn nodes(&self) -> usize {
        1 + match self {
            Node::Leaf(_) => 0,
            Node::Column(children) | Node::Row(children) => children.iter().map(Node::nodes).sum(),
            Node::Overlay { base, layers } => base.nodes() + layers.len(),
        }
    }

    /// How many chrome surfaces the tree needs: each maximal run of adjacent components among
    /// a container's children, a status strip on its own, and each overlay layer.
    pub fn chrome_surfaces(&self) -> usize {
        match self {
            Node::Leaf(component) => usize::from(component.kind() != Kind::PageView),
            Node::Column(children) | Node::Row(children) => {
                let mut surfaces = 0;
                let mut in_run = false;
                for child in children {
                    match child {
                        Node::Leaf(c) if c.kind() == Kind::StatusBar => {
                            surfaces += 1;
                            in_run = false;
                        }
                        Node::Leaf(c) if c.kind() != Kind::PageView => {
                            surfaces += usize::from(!in_run);
                            in_run = true;
                        }
                        other => {
                            surfaces += other.chrome_surfaces();
                            in_run = false;
                        }
                    }
                }
                surfaces
            }
            Node::Overlay { base, layers } => base.chrome_surfaces() + layers.len(),
        }
    }
}

/// A component of `kind` as a leaf, as the standard look has it.
fn leaf(kind: Kind) -> Node {
    Node::Leaf(Component::new(kind))
}

/// Whether `node` is the page with only overlays around it.
fn is_page_node(node: &Node) -> bool {
    match node {
        Node::Leaf(component) => component.kind() == Kind::PageView,
        Node::Overlay { base, .. } => is_page_node(base),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GUI: Frontend = Frontend::Gui;
    const TUI: Frontend = Frontend::Terminal;

    fn c(kind: Kind) -> Component {
        Component::new(kind)
    }

    fn l(kind: Kind) -> Node {
        leaf(kind)
    }

    fn layer(kind: Kind, anchor: Anchor, show: Show) -> Layer {
        Layer::new(c(kind), anchor, show)
    }

    fn overlay_status() -> Node {
        Node::Overlay {
            base: Box::new(l(Kind::PageView)),
            layers: vec![layer(Kind::StatusBar, Anchor::BottomLeft, Show::WhenNeeded)],
        }
    }

    fn label(id: u16, size: Size) -> Node {
        let mut label = Label::new(id, LabelText::Fixed("x".into()));
        label.size = size;
        Node::Leaf(Component::Label(label))
    }

    #[test]
    fn the_presets_pass() {
        assert_eq!(Interface::standard().check(), Vec::<String>::new());
        assert_eq!(Interface::default(), Interface::standard());
        assert_eq!(Node::standard().chrome_surfaces(), 2);
        assert_eq!(Node::terminal().chrome_surfaces(), 2);
        let Some(Component::PageView(page)) = Node::standard().get(Kind::PageView).cloned() else {
            panic!()
        };
        assert_eq!(page.slot, PageSlot::MAIN);
    }

    /// The design's trees, built by operations from the standard one.
    #[test]
    fn the_looks_are_one_operation_away() {
        let placed = |kind, side| {
            let mut tree = Node::standard();
            tree.change(GUI, |t| t.position(kind, side)).unwrap();
            tree
        };
        assert_eq!(
            placed(Kind::TabBar, Side::Left),
            Node::Column(vec![
                l(Kind::AddressBar),
                Node::Row(vec![l(Kind::TabBar), l(Kind::PageView)]),
                l(Kind::StatusBar),
            ])
        );
        assert_eq!(placed(Kind::TabBar, Side::Left).chrome_surfaces(), 3);
        assert_eq!(
            placed(Kind::TabBar, Side::Bottom),
            Node::Column(vec![
                l(Kind::AddressBar),
                l(Kind::PageView),
                l(Kind::TabBar),
                l(Kind::StatusBar),
            ])
        );
        assert_eq!(
            placed(Kind::TabBar, Side::Right),
            Node::Column(vec![
                l(Kind::AddressBar),
                Node::Row(vec![l(Kind::PageView), l(Kind::TabBar)]),
                l(Kind::StatusBar),
            ])
        );
        assert_eq!(
            placed(Kind::AddressBar, Side::Bottom),
            Node::Column(vec![
                l(Kind::TabBar),
                l(Kind::PageView),
                l(Kind::AddressBar),
                l(Kind::StatusBar),
            ])
        );

        let mut overlay = Node::standard();
        let status = c(Kind::StatusBar);
        overlay
            .change(GUI, |t| {
                t.overlay(status, Anchor::BottomLeft, Show::WhenNeeded)
            })
            .unwrap();
        assert_eq!(
            overlay,
            Node::Column(vec![l(Kind::TabBar), l(Kind::AddressBar), overlay_status()])
        );
        // Both: the list beside the page, the status over the page alone.
        overlay
            .change(GUI, |t| t.position(Kind::TabBar, Side::Left))
            .unwrap();
        assert_eq!(
            overlay,
            Node::Column(vec![
                l(Kind::AddressBar),
                Node::Row(vec![l(Kind::TabBar), overlay_status()]),
            ])
        );
        // The full-height sidebar is a tree written by hand.
        let sidebar = Node::Row(vec![
            l(Kind::TabBar),
            Node::Column(vec![
                l(Kind::AddressBar),
                l(Kind::PageView),
                l(Kind::StatusBar),
            ]),
        ]);
        assert_eq!(sidebar.check(GUI), Vec::<String>::new());

        // The terminal's summary at the bottom, under the page and over the command line, or
        // gone; the command line stays outermost at either end.
        let mut terminal = Node::terminal();
        terminal
            .change(TUI, |t| t.position(Kind::Summary, Side::Bottom))
            .unwrap();
        assert_eq!(
            terminal,
            Node::Column(vec![
                l(Kind::PageView),
                l(Kind::Summary),
                l(Kind::CommandLine)
            ])
        );
        terminal.change(TUI, |t| t.remove(Kind::Summary)).unwrap();
        assert_eq!(
            terminal,
            Node::Column(vec![l(Kind::PageView), l(Kind::CommandLine)])
        );
        let mut top = Node::terminal();
        top.change(TUI, |t| {
            t.position(Kind::CommandLine, Side::Top);
            t.position(Kind::Summary, Side::Top);
        })
        .unwrap();
        assert_eq!(
            top,
            Node::Column(vec![
                l(Kind::CommandLine),
                l(Kind::Summary),
                l(Kind::PageView)
            ])
        );
    }

    #[test]
    fn place_and_remove_round_trip() {
        let standard = Node::standard();
        for side in [Side::Left, Side::Right, Side::Bottom, Side::Top] {
            let mut tree = standard.clone();
            tree.change(GUI, |t| t.position(Kind::TabBar, side))
                .unwrap();
            tree.change(GUI, |t| t.position(Kind::TabBar, Side::Top))
                .unwrap();
            assert_eq!(tree, standard, "{side:?}");
        }
        let mut tree = standard.clone();
        tree.change(GUI, |t| t.remove(Kind::TabBar)).unwrap();
        assert!(!tree.has(Kind::TabBar));
        tree.change(GUI, |t| t.position(Kind::TabBar, Side::Top))
            .unwrap();
        assert_eq!(tree, standard);
        // The status line over the page and back into its strip.
        let status = c(Kind::StatusBar);
        tree.change(GUI, |t| {
            t.overlay(status.clone(), Anchor::BottomLeft, Show::WhenNeeded)
        })
        .unwrap();
        tree.change(GUI, |t| t.place(status, Side::Bottom)).unwrap();
        assert_eq!(tree, standard);
        // Moving a component keeps what it says about itself.
        let mut tree = standard.clone();
        let Some(Component::TabBar(tabs)) = tree.get_mut(Kind::TabBar) else {
            panic!()
        };
        tabs.new_tab = false;
        let moved = tree.get(Kind::TabBar).unwrap().clone();
        tree.change(GUI, |t| t.position(Kind::TabBar, Side::Left))
            .unwrap();
        assert_eq!(tree.get(Kind::TabBar), Some(&moved));
    }

    /// A change is checked once, at the end: an intermediate tree may be invalid (no status
    /// line between its removal and its overlay), an invalid result changes nothing.
    #[test]
    fn changes_are_checked_once_and_whole() {
        let mut tree = Node::standard();
        tree.change(GUI, |t| {
            t.remove(Kind::StatusBar);
            assert!(!t.check(GUI).is_empty(), "invalid in between");
            t.overlay(c(Kind::StatusBar), Anchor::BottomRight, Show::WhenNeeded);
        })
        .unwrap();
        assert!(tree.components().iter().any(|(_, layer)| layer.is_some()));
        for (change, what) in [
            (
                Box::new(|t: &mut Node| t.remove(Kind::PageView)) as Box<dyn Fn(&mut Node)>,
                "0 PageViews",
            ),
            (
                Box::new(|t: &mut Node| t.remove(Kind::AddressBar)),
                "no AddressBar",
            ),
            (
                Box::new(|t: &mut Node| t.remove(Kind::StatusBar)),
                "no StatusBar",
            ),
            (
                Box::new(|t: &mut Node| t.position(Kind::AddressBar, Side::Left)),
                "outside a Column",
            ),
            (
                Box::new(|t: &mut Node| t.position(Kind::Summary, Side::Top)),
                "Summary in the window",
            ),
            (
                Box::new(|t: &mut Node| t.position(Kind::CommandLine, Side::Top)),
                "CommandLine in the window",
            ),
            (
                Box::new(|t: &mut Node| t.overlay(c(Kind::TabBar), Anchor::TopLeft, Show::Always)),
                "layers are passive",
            ),
        ] {
            let before = tree.clone();
            let problems = tree.change(GUI, |t| change(t)).unwrap_err();
            assert!(
                problems.iter().any(|p| p.contains(what)),
                "{what}: {problems:?}"
            );
            assert_eq!(tree, before, "{what}: a refused change changed the tree");
        }
        let mut terminal = Node::terminal();
        for change in [
            Box::new(|t: &mut Node| t.position(Kind::Summary, Side::Left))
                as Box<dyn Fn(&mut Node)>,
            Box::new(|t: &mut Node| t.position(Kind::TabBar, Side::Top)),
            Box::new(|t: &mut Node| t.remove(Kind::CommandLine)),
        ] {
            assert!(terminal.change(TUI, |t| change(t)).is_err());
        }
        assert_eq!(terminal, Node::terminal());
        let mut interface = Interface::standard();
        let refused = interface.change(|i| i.terminal = Node::standard());
        assert!(refused
            .unwrap_err()
            .iter()
            .all(|p| p.starts_with("the terminal's interface: ")));
        assert_eq!(interface, Interface::standard());
    }

    /// Each rule of the check is reported, once, and every counterexample tree of the outside
    /// review (reviews-interface-astra.md) is refused or, where it is a valid tree, accepted for
    /// the interpreter to keep usable.
    #[test]
    fn each_violation_is_reported() {
        let page = || l(Kind::PageView);
        let addr = || l(Kind::AddressBar);
        let status = || l(Kind::StatusBar);
        let tabs = || l(Kind::TabBar);
        let cases: Vec<(Node, Frontend, &str)> = vec![
            (Node::Column(vec![addr(), status()]), GUI, "0 PageViews"),
            (
                Node::Column(vec![addr(), page(), page(), status()]),
                GUI,
                "2 PageViews",
            ),
            (
                Node::Column(vec![tabs(), tabs(), addr(), page(), status()]),
                GUI,
                "2 TabBars",
            ),
            (Node::Column(vec![page(), status()]), GUI, "no AddressBar"),
            (Node::Column(vec![addr(), page()]), GUI, "no StatusBar"),
            (
                Node::Row(vec![addr(), page(), status()]),
                GUI,
                "outside a Column",
            ),
            (
                Node::Column(vec![addr(), Node::Row(vec![]), page(), status()]),
                GUI,
                "empty",
            ),
            (
                Node::Column(vec![Node::Row(vec![tabs()]), addr(), page(), status()]),
                GUI,
                "without the page",
            ),
            (
                Node::Column(vec![
                    addr(),
                    Node::Overlay {
                        base: Box::new(page()),
                        layers: vec![],
                    },
                    status(),
                ]),
                GUI,
                "without layers",
            ),
            (
                Node::Column(vec![
                    addr(),
                    Node::Overlay {
                        base: Box::new(page()),
                        layers: vec![
                            layer(Kind::StatusBar, Anchor::BottomLeft, Show::WhenNeeded),
                            layer(Kind::StatusBar, Anchor::TopLeft, Show::Always),
                            layer(Kind::StatusBar, Anchor::TopRight, Show::Always),
                        ],
                    },
                ]),
                GUI,
                "3 overlay layers",
            ),
            (
                Node::Column(vec![
                    Node::Row(vec![Node::Column(vec![Node::Row(vec![Node::Column(
                        vec![page()],
                    )])])]),
                    addr(),
                    status(),
                ]),
                GUI,
                "levels deep",
            ),
            (
                Node::Column(vec![l(Kind::Summary), addr(), page(), status()]),
                GUI,
                "Summary in the window",
            ),
            (Node::terminal(), GUI, "Summary in the window"),
            (Node::standard(), TUI, "TabBar in the terminal"),
            (
                Node::Column(vec![
                    l(Kind::Summary),
                    Node::Row(vec![page()]),
                    l(Kind::CommandLine),
                ]),
                TUI,
                "single Column",
            ),
            (
                Node::Column(vec![l(Kind::Summary), page()]),
                TUI,
                "no CommandLine",
            ),
            // Review 2: the page as a layer above the chrome, and an interactive layer.
            (
                Node::Overlay {
                    base: Box::new(Node::Column(vec![addr(), status()])),
                    layers: vec![layer(Kind::PageView, Anchor::BottomLeft, Show::Always)],
                },
                GUI,
                "layers are passive",
            ),
            (
                Node::Overlay {
                    base: Box::new(Node::Column(vec![tabs(), page(), status()])),
                    layers: vec![layer(Kind::AddressBar, Anchor::TopLeft, Show::Always)],
                },
                GUI,
                "layers are passive",
            ),
            // Reviews 3, 4 and 13: the line look in the window, alone or with tabs.
            (
                Node::Column(vec![l(Kind::CommandLine), page()]),
                GUI,
                "CommandLine in the window",
            ),
            (
                Node::Column(vec![tabs(), l(Kind::CommandLine), page()]),
                GUI,
                "CommandLine in the window",
            ),
            // Review 6: a label (no interpreter shows one), bars in a row, a tall terminal label.
            (
                Node::Column(vec![label(1, Size::Auto), addr(), page(), status()]),
                GUI,
                "a Label",
            ),
            (
                Node::Row(vec![addr(), page(), status()]),
                GUI,
                "AddressBar outside a Column",
            ),
            (
                Node::Column(vec![label(1, Size::Fixed(3)), page(), l(Kind::CommandLine)]),
                TUI,
                "a Label",
            ),
            // Review 8: six runs, four of them labels and a summary.
            (
                Node::Column(vec![
                    label(1, Size::Fixed(10)),
                    Node::Row(vec![
                        label(2, Size::Fixed(10)),
                        Node::Column(vec![l(Kind::Summary), page(), addr(), status()]),
                        label(3, Size::Fixed(10)),
                    ]),
                    tabs(),
                ]),
                GUI,
                "a Label",
            ),
            // Review 11: two pages.
            (Node::Row(vec![page(), page()]), GUI, "2 PageViews"),
        ];
        for (tree, frontend, expected) in cases {
            let problems = tree.check(frontend);
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{expected:?} not in {problems:?} for {tree:?}"
            );
            let mut unique = problems.clone();
            unique.dedup();
            assert_eq!(unique, problems);
        }

        // Review 7: a button twice.
        let mut buttons = Node::standard();
        let Some(Component::AddressBar(bar)) = buttons.get_mut(Kind::AddressBar) else {
            panic!()
        };
        bar.buttons = vec![Button::Back, Button::Back];
        let problems = buttons.check(GUI);
        assert!(problems.iter().any(|p| p.contains("twice")), "{problems:?}");

        // Valid trees of the review, which the interpreters keep usable: tabs as tall as the
        // window (review 1) and tabs at the bottom beside the status strip (review 3).
        let mut tall = Node::standard();
        let Some(Component::TabBar(bar)) = tall.get_mut(Kind::TabBar) else {
            panic!()
        };
        bar.size = Size::Fixed(600);
        assert_eq!(tall.check(GUI), Vec::<String>::new());
        let bottom = Node::Column(vec![addr(), page(), tabs(), status()]);
        assert_eq!(bottom.check(GUI), Vec::<String>::new());
        assert_eq!(
            bottom.chrome_surfaces(),
            3,
            "the status strip is a surface of its own"
        );
    }
}
