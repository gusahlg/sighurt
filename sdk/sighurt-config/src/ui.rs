//! The interface of an [`Experience`](crate::Experience), being written: where the window's
//! components go and what they show, and the terminal's lines
//! (docs/rewrite/interface-2026-11-16.md, §1).
//!
//! ```
//! use sighurt_config::prelude::*;
//!
//! let mut sig = Experience::default();
//! sig.ui.tabs.position(Side::Left).new_tab(false);
//! sig.ui.status.overlay(Anchor::BottomRight);
//! sig.ui.address.buttons(&[Button::Back, Button::Reload]);
//! sig.ui.terminal.summary.hide();
//! let (interface, problems) = sig.interface();
//! assert!(problems.is_empty());
//! assert!(interface.window.has(Kind::TabBar));
//! ```
//!
//! Each part ([`UiBuilder::tabs`], [`UiBuilder::address`], [`UiBuilder::status`],
//! [`TerminalUi::summary`], [`TerminalUi::line`]) records what is asked of
//! it; [`UiBuilder::finish`] applies that to the trees it starts from (the standard ones, or
//! those [`UiBuilder::window`] and [`TerminalUi::tree`] set) in a fixed order: the address bar,
//! the tabs, the status line, then the terminal's summary and line, each place first and its
//! fields after. They are the operations of `sighurt_api::ui`, the same the configuration
//! file's `[ui.layout]` and `[tui.layout]` run, so what one can say the other says the same
//! way. The result is checked once, whole (`Interface::check`); an experience with any problem
//! is not used.
//!
//! Must not own: how a tree is shown (the frontends'), or what a tree may be
//! (`sighurt_api::ui`'s check).

use sighurt_api::ui::{
    Anchor, Button, Component, Density, Interface, Kind, Node, Show, Side, Size,
};

/// The interface, being written.
#[derive(Clone, Debug, Default)]
pub struct UiBuilder {
    /// The tabs: a strip above or below the page, or a list beside it.
    pub tabs: TabsPart,
    /// The toolbar with the address line.
    pub address: AddressPart,
    /// The status line.
    pub status: StatusPart,
    /// The terminal's lines.
    pub terminal: TerminalUi,
    window: Option<Node>,
    compact: Option<bool>,
}

/// Where a part goes: at a side of the page, or nowhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    At(Side),
    Hidden,
}

/// The tabs, being written.
#[derive(Clone, Debug, Default)]
pub struct TabsPart {
    place: Option<Place>,
    size: Option<Size>,
    new_tab: Option<bool>,
    close_buttons: Option<bool>,
    engine_label: Option<bool>,
}

impl TabsPart {
    /// At `side` of the page: a strip at the top or bottom, a list on the left or right.
    pub fn position(&mut self, side: Side) -> &mut Self {
        self.place = Some(Place::At(side));
        self
    }

    /// Nowhere: tabs are reached by keys and commands (`chrome.toggle-tabs` shows a strip).
    pub fn hide(&mut self) -> &mut Self {
        self.place = Some(Place::Hidden);
        self
    }

    /// Its height as a strip, its width as a list, in logical pixels at 14 px.
    pub fn size(&mut self, size: u16) -> &mut Self {
        self.size = Some(Size::Fixed(size));
        self
    }

    /// The new-tab button after the last tab.
    pub fn new_tab(&mut self, on: bool) -> &mut Self {
        self.new_tab = Some(on);
        self
    }

    /// A close button on each tab while there is more than one.
    pub fn close_buttons(&mut self, on: bool) -> &mut Self {
        self.close_buttons = Some(on);
        self
    }

    /// The engine's name in each engine page's tab.
    pub fn engine_label(&mut self, on: bool) -> &mut Self {
        self.engine_label = Some(on);
        self
    }

    fn change(&self, component: &mut Component) {
        if let Component::TabBar(tabs) = component {
            set(&mut tabs.size, self.size);
            set(&mut tabs.new_tab, self.new_tab);
            set(&mut tabs.close_buttons, self.close_buttons);
            set(&mut tabs.engine_label, self.engine_label);
        }
    }

    fn changes(&self) -> bool {
        self.size.is_some()
            || self.new_tab.is_some()
            || self.close_buttons.is_some()
            || self.engine_label.is_some()
    }
}

/// The toolbar, being written.
#[derive(Clone, Debug, Default)]
pub struct AddressPart {
    place: Option<Place>,
    buttons: Option<Vec<Button>>,
    indicator: Option<bool>,
    engine_button: Option<bool>,
    find_state: Option<bool>,
}

impl AddressPart {
    /// Above the page ([`Side::Top`]) or below it ([`Side::Bottom`]).
    pub fn position(&mut self, side: Side) -> &mut Self {
        self.place = Some(Place::At(side));
        self
    }

    /// The buttons before the address line, in order.
    pub fn buttons(&mut self, buttons: &[Button]) -> &mut Self {
        self.buttons = Some(buttons.to_vec());
        self
    }

    /// The mode's indicator at the line's right end.
    pub fn indicator(&mut self, on: bool) -> &mut Self {
        self.indicator = Some(on);
        self
    }

    /// The active tab's engine as a button at the line's right end.
    pub fn engine_button(&mut self, on: bool) -> &mut Self {
        self.engine_button = Some(on);
        self
    }

    /// The search's state ("3 of 12") while a search is under way.
    pub fn find_state(&mut self, on: bool) -> &mut Self {
        self.find_state = Some(on);
        self
    }

    fn change(&self, component: &mut Component) {
        if let Component::AddressBar(bar) = component {
            if let Some(buttons) = &self.buttons {
                bar.buttons = buttons.clone();
            }
            set(&mut bar.indicator, self.indicator);
            set(&mut bar.engine_button, self.engine_button);
            set(&mut bar.find_state, self.find_state);
        }
    }

    fn changes(&self) -> bool {
        self.buttons.is_some()
            || self.indicator.is_some()
            || self.engine_button.is_some()
            || self.find_state.is_some()
    }
}

/// The status line, being written.
#[derive(Clone, Debug, Default)]
pub struct StatusPart {
    overlay: Option<Option<Anchor>>,
}

impl StatusPart {
    /// A strip below the page.
    pub fn strip(&mut self) -> &mut Self {
        self.overlay = Some(None);
        self
    }

    /// Over the page's corner `anchor`, taking no room, shown while there is status.
    pub fn overlay(&mut self, anchor: Anchor) -> &mut Self {
        self.overlay = Some(Some(anchor));
        self
    }
}

/// The terminal's command line, being written.
#[derive(Clone, Debug, Default)]
pub struct LinePart {
    place: Option<Place>,
    engine_label: Option<bool>,
}

impl LinePart {
    /// Above the page ([`Side::Top`]) or below it ([`Side::Bottom`]).
    pub fn position(&mut self, side: Side) -> &mut Self {
        self.place = Some(Place::At(side));
        self
    }

    /// The active tab's engine on the line.
    pub fn engine_label(&mut self, on: bool) -> &mut Self {
        self.engine_label = Some(on);
        self
    }

    fn change(&self, component: &mut Component) {
        if let Component::CommandLine(line) = component {
            set(&mut line.engine_label, self.engine_label);
        }
    }

    fn changes(&self) -> bool {
        self.engine_label.is_some()
    }
}

/// The terminal's summary line, being written.
#[derive(Clone, Debug, Default)]
pub struct SummaryPart {
    place: Option<Place>,
}

impl SummaryPart {
    /// At the top ([`Side::Top`]) or bottom ([`Side::Bottom`]) of the terminal.
    pub fn position(&mut self, side: Side) -> &mut Self {
        self.place = Some(Place::At(side));
        self
    }

    /// Not shown.
    pub fn hide(&mut self) -> &mut Self {
        self.place = Some(Place::Hidden);
        self
    }
}

/// The terminal's interface, being written.
#[derive(Clone, Debug, Default)]
pub struct TerminalUi {
    /// The summary line: the tab's place, title, address, engine and mode.
    pub summary: SummaryPart,
    /// The command line: the address, status and notices, and the editor.
    pub line: LinePart,
    tree: Option<Node>,
}

impl TerminalUi {
    /// Starts from `tree` instead of the standard terminal tree.
    pub fn tree(&mut self, tree: Node) -> &mut Self {
        self.tree = Some(tree);
        self
    }
}

impl UiBuilder {
    /// Starts the window from `tree` (such as `Node::line()`) instead of the standard tree.
    pub fn window(&mut self, tree: Node) -> &mut Self {
        self.window = Some(tree);
        self
    }

    /// A lower tab strip and toolbar: 32 and 36 px instead of 40 and 44, at 14 px.
    pub fn compact(&mut self, on: bool) -> &mut Self {
        self.compact = Some(on);
        self
    }

    /// The interface this describes, and every problem applying it found.
    pub fn finish(&self) -> (Interface, Vec<String>) {
        let mut interface = Interface::standard();
        if let Some(tree) = &self.window {
            interface.window = tree.clone();
        }
        if let Some(tree) = &self.terminal.tree {
            interface.terminal = tree.clone();
        }
        if let Some(compact) = self.compact {
            interface.density = match compact {
                true => Density::Compact,
                false => Density::Normal,
            };
        }
        let mut problems = Vec::new();
        let window = &mut interface.window;
        let (address, tabs) = (&self.address, &self.tabs);
        let changes = |c: &mut Component| address.change(c);
        part(
            window,
            "ui.address",
            Kind::AddressBar,
            address.place,
            address.changes(),
            changes,
            &mut problems,
        );
        let changes = |c: &mut Component| tabs.change(c);
        part(
            window,
            "ui.tabs",
            Kind::TabBar,
            tabs.place,
            tabs.changes(),
            changes,
            &mut problems,
        );
        let status = Component::new(Kind::StatusBar);
        match self.status.overlay {
            Some(None) => window.place(status, Side::Bottom),
            Some(Some(anchor)) => window.overlay(status, anchor, Show::WhenNeeded),
            None => {}
        }
        let terminal = &mut interface.terminal;
        let summary = self.terminal.summary.place;
        part(
            terminal,
            "ui.terminal.summary",
            Kind::Summary,
            summary,
            false,
            |_| {},
            &mut problems,
        );
        let line = &self.terminal.line;
        let changes = |c: &mut Component| line.change(c);
        part(
            terminal,
            "ui.terminal.line",
            Kind::CommandLine,
            line.place,
            line.changes(),
            changes,
            &mut problems,
        );
        // Checked once, whole: a part may leave a tree no frontend shows until a later one.
        problems.extend(interface.check());
        (interface, problems)
    }
}

/// Applies a part to `tree`: its place, then its fields (`changes`, by `change`). A part that
/// changes a component the tree doesn't have is a problem.
fn part(
    tree: &mut Node,
    name: &str,
    kind: Kind,
    place: Option<Place>,
    changes: bool,
    change: impl Fn(&mut Component),
    problems: &mut Vec<String>,
) {
    match place {
        Some(Place::Hidden) => tree.remove(kind),
        Some(Place::At(side)) => {
            let mut placed = tree
                .get(kind)
                .cloned()
                .unwrap_or_else(|| Component::new(kind));
            change(&mut placed);
            tree.place(placed, side);
        }
        None => match tree.get_mut(kind) {
            Some(component) => change(component),
            None if changes => problems.push(format!("{name}: there is no {kind} to change")),
            None => {}
        },
    }
}

fn set<T: Copy>(field: &mut T, value: Option<T>) {
    if let Some(value) = value {
        *field = value;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Experience;

    #[test]
    fn tabs_on_the_left_are_one_call() {
        let mut sig = Experience::default();
        sig.ui.tabs.position(Side::Left);
        let (interface, problems) = sig.interface();
        assert_eq!(problems, Vec::<String>::new());
        let mut expected = Interface::standard();
        expected.window.position(Kind::TabBar, Side::Left);
        assert_eq!(interface, expected);
    }

    #[test]
    fn the_default_interface_is_the_standard_one() {
        let (interface, problems) = Experience::default().interface();
        assert_eq!(problems, Vec::<String>::new());
        assert_eq!(interface, Interface::standard());
        let (empty, _) = Experience::empty().interface();
        assert_eq!(empty, Interface::standard());
    }

    #[test]
    fn fields_follow_the_component_and_mistakes_are_problems() {
        let mut sig = Experience::default();
        sig.ui.tabs.engine_label(false).position(Side::Bottom);
        sig.ui
            .address
            .buttons(&[Button::Reload])
            .position(Side::Bottom);
        sig.ui.compact(true);
        let (interface, problems) = sig.interface();
        assert_eq!(problems, Vec::<String>::new());
        let Some(Component::TabBar(tabs)) = interface.window.get(Kind::TabBar) else {
            panic!()
        };
        assert!(!tabs.engine_label);
        let Some(Component::AddressBar(bar)) = interface.window.get(Kind::AddressBar) else {
            panic!()
        };
        assert_eq!(bar.buttons, [Button::Reload]);
        assert_eq!(interface.density, Density::Compact);

        let mut sig = Experience::default();
        sig.ui.tabs.hide().new_tab(false);
        sig.ui.terminal.summary.position(Side::Left);
        sig.ui.address.buttons(&[Button::Back, Button::Back]);
        let (interface, problems) = sig.interface();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("Summary outside a Column")),
            "{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("the Back button twice")),
            "{problems:?}"
        );
        assert!(!interface.window.has(Kind::TabBar));
        // Changing what isn't there.
        let mut sig = Experience::empty();
        sig.ui.window(Node::Column(vec![
            Node::Leaf(Component::new(Kind::AddressBar)),
            Node::Leaf(Component::new(Kind::PageView)),
            Node::Leaf(Component::new(Kind::StatusBar)),
        ]));
        sig.ui.tabs.new_tab(false);
        let (_, problems) = sig.interface();
        assert_eq!(problems, ["ui.tabs: there is no TabBar to change"]);
    }

    /// A tree can be invalid between parts: the status line taken out of the column and laid
    /// over the page, which only the whole result says is fine.
    #[test]
    fn the_result_is_checked_once() {
        let mut sig = Experience::default();
        sig.ui.status.overlay(Anchor::BottomRight);
        sig.ui.terminal.line.position(Side::Top);
        sig.ui.terminal.summary.position(Side::Top);
        let (interface, problems) = sig.interface();
        assert_eq!(problems, Vec::<String>::new());
        assert!(interface
            .window
            .components()
            .iter()
            .any(|(_, layer)| layer.is_some()));
    }
}
