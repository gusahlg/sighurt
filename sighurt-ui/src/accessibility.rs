//! The chrome's accessibility tree as plain data: roles, names, values, states and bounds of
//! its widgets, with stable ids, and a document node for the page area.
//!
//! When an engine view shows the page, the document node is the container its accessibility
//! subtree will hang under once engines provide one; none does yet, and the node says only
//! what the chrome knows (the page's title). Pages the chrome draws itself are described in
//! full. A bridge to the desktop (such as AccessKit's Unix adapter) comes later; this tree is
//! what it would translate.
//!
//! Must not own: engine content semantics.

use sighurt_api::geometry::WindowRect;
use sighurt_api::ids::TabId;

use crate::chrome::{Chrome, Tone};
use crate::hit_test::Target;

/// A node's identity. It stays the same while the thing it names exists, so an assistive
/// technology can follow a tab across tree updates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NodeId {
    /// The window.
    Window,
    /// The tab strip.
    TabList,
    /// A tab.
    Tab(TabId),
    /// A tab's close button.
    CloseTab(TabId),
    /// The new-tab button.
    NewTab,
    /// The toolbar.
    Toolbar,
    /// The back button.
    Back,
    /// The forward button.
    Forward,
    /// The reload or stop button.
    ReloadOrStop,
    /// The address line.
    Address,
    /// The page area.
    Page,
    /// The n-th text block of a page the chrome draws.
    PageText(usize),
    /// The status strip.
    Status,
}

/// What a node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The browser window.
    Window,
    /// A list of tabs.
    TabList,
    /// A tab.
    Tab,
    /// A group of controls.
    Toolbar,
    /// A button.
    Button,
    /// A one-line text field that takes a URL (or a command).
    UrlInput,
    /// A web page, or the container for an engine's tree.
    Document,
    /// A heading.
    Heading,
    /// A block of text.
    Label,
    /// A live status line.
    Status,
}

/// A node's states.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct States {
    /// The selected tab.
    pub selected: bool,
    /// A button that does nothing now.
    pub disabled: bool,
    /// Has keyboard focus.
    pub focused: bool,
    /// Loading.
    pub busy: bool,
    /// Shows an error.
    pub invalid: bool,
}

/// One node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Its identity.
    pub id: NodeId,
    /// What it is.
    pub role: Role,
    /// Its accessible name.
    pub name: String,
    /// Its value: the address line's text, or the committed address of the page the document
    /// node stands for (which the line may not show while the user types or opens another).
    pub value: Option<String>,
    /// Extra text, such as why a command failed.
    pub description: Option<String>,
    /// Its states.
    pub states: States,
    /// Where it is, in window-logical coordinates.
    pub bounds: WindowRect,
    /// Its children, in reading order.
    pub children: Vec<NodeId>,
}

/// The chrome's tree.
#[derive(Clone, Debug, PartialEq)]
pub struct Tree {
    /// Every node; the first is the root, [`NodeId::Window`].
    pub nodes: Vec<Node>,
    /// The node with keyboard focus, if the chrome has it.
    pub focus: Option<NodeId>,
}

impl Tree {
    /// The node with `id`.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

fn node(id: NodeId, role: Role, name: impl Into<String>, bounds: WindowRect) -> Node {
    Node {
        id,
        role,
        name: name.into(),
        value: None,
        description: None,
        states: States::default(),
        bounds,
        children: Vec::new(),
    }
}

impl Chrome {
    /// The chrome's accessibility tree as it is now.
    pub fn accessibility(&mut self) -> Tree {
        let layout = self.layout().clone();
        let snapshot = self.snapshot().clone();
        let state = self.state().clone();
        let mut nodes = Vec::new();

        let mut tab_list = node(NodeId::TabList, Role::TabList, "Tabs", layout.tab_strip);
        for (tab, place) in snapshot.tabs.iter().zip(&layout.tabs) {
            let mut entry = node(
                NodeId::Tab(tab.id),
                Role::Tab,
                tab.display_title(),
                place.rect,
            );
            entry.states.selected = tab.active;
            entry.states.busy = tab.loading;
            if let Some(close) = place.close {
                entry.children.push(NodeId::CloseTab(tab.id));
                nodes.push(node(
                    NodeId::CloseTab(tab.id),
                    Role::Button,
                    "Close tab",
                    close,
                ));
            }
            tab_list.children.push(entry.id);
            nodes.push(entry);
        }
        tab_list.children.push(NodeId::NewTab);
        nodes.push(node(
            NodeId::NewTab,
            Role::Button,
            "New tab",
            layout.new_tab,
        ));

        let loading = snapshot.active().is_some_and(|tab| tab.loading);
        let buttons = [
            (NodeId::Back, "Back", layout.back, snapshot.can_go_back),
            (
                NodeId::Forward,
                "Forward",
                layout.forward,
                snapshot.can_go_forward,
            ),
            (
                NodeId::ReloadOrStop,
                if loading { "Stop" } else { "Reload" },
                layout.reload,
                true,
            ),
        ];
        let mut toolbar = node(NodeId::Toolbar, Role::Toolbar, "Navigation", layout.toolbar);
        for (id, name, bounds, enabled) in buttons {
            let mut button = node(id, Role::Button, name, bounds);
            button.states.disabled = !enabled;
            toolbar.children.push(id);
            nodes.push(button);
        }
        let mut address = node(NodeId::Address, Role::UrlInput, "Address", layout.address);
        address.value = Some(state.address.text().to_owned());
        address.states.focused = state.focused;
        address.states.invalid = state.command_error.is_some();
        address.description = state.command_error.clone();
        toolbar.children.push(NodeId::Address);
        nodes.push(address);

        let title = snapshot.active().map(|tab| tab.display_title());
        let mut page = node(
            NodeId::Page,
            Role::Document,
            title.unwrap_or_default(),
            layout.page,
        );
        page.value = snapshot.active().map(|tab| tab.url.clone());
        page.states.busy = loading;
        if let Some(layout) = self.page_layout() {
            for (i, text) in layout.texts.iter().enumerate() {
                let content = text.shaped.runs().map(|run| run.text).next().unwrap_or("");
                let bounds = WindowRect {
                    x: text.x as i32,
                    y: text.y as i32,
                    width: text.shaped.width.ceil() as u32,
                    height: text.shaped.height.ceil() as u32,
                };
                let role = if text.heading {
                    Role::Heading
                } else {
                    Role::Label
                };
                let mut label = node(NodeId::PageText(i), role, content, bounds);
                label.states.invalid = text.tone == Tone::Error;
                page.children.push(label.id);
                nodes.push(label);
            }
        }

        let mut window = node(NodeId::Window, Role::Window, "Sighurt", {
            let size = layout.window;
            WindowRect {
                x: 0,
                y: 0,
                width: size.width,
                height: size.height,
            }
        });
        window.children = vec![NodeId::TabList, NodeId::Toolbar, NodeId::Page];
        let mut roots = vec![window, tab_list, toolbar, page];
        if !layout.status.size().is_empty() {
            let mut status = node(NodeId::Status, Role::Status, "Status", layout.status);
            status.value = self.status_line().map(|(line, _)| line);
            roots[0].children.push(NodeId::Status);
            roots.push(status);
        }
        roots.append(&mut nodes);
        Tree {
            nodes: roots,
            focus: state.focused.then_some(NodeId::Address),
        }
    }
}

/// The node a hit target belongs to.
pub fn node_for(target: Target) -> NodeId {
    match target {
        Target::Tab(id) => NodeId::Tab(id),
        Target::CloseTab(id) => NodeId::CloseTab(id),
        Target::NewTab => NodeId::NewTab,
        Target::Back => NodeId::Back,
        Target::Forward => NodeId::Forward,
        Target::ReloadOrStop => NodeId::ReloadOrStop,
        Target::Address => NodeId::Address,
    }
}
