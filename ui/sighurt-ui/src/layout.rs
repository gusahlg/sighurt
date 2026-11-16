//! Chrome layout: where the interface's tree ([`sighurt_api::ui::Node`]) puts the tabs, the
//! toolbar with its address line, the status line and the page, for the current window size,
//! and the surfaces the chrome draws them on.
//!
//! Explicit code, not a solver, in window-logical coordinates. A container gives its children
//! one pass along its axis: each component takes its own size ([`Size`]) or what is left if
//! that is less, in tree order, and is laid out at its full size and clipped to what it got; a
//! status strip is allocated last and only if what is left is more than its height (short
//! windows drop the status strip first); the child holding the page takes the rest. An
//! overlay's layers lie over its base without taking room from it. A component's size is a
//! function of its kind, the density and the font size, never of its text.
//!
//! The result is both where the surfaces go and the widget rectangles the hit test and the
//! painter share. A region with no room gets an empty rectangle, which the host unmaps; that is
//! not how background tabs are hidden (they keep the page rectangle and are hidden through
//! visibility). Every widget rectangle lies inside its surface, so nothing is hit that isn't
//! drawn.
//!
//! The surfaces: a maximal run of adjacent components among a container's children is one
//! surface, a status strip always one of its own, and each overlay layer one. Names come from a
//! fixed pool, by the run's principal component: [`CHROME`] (the run with the address bar),
//! [`TABS`] (the tabs without it), [`STATUS`], and [`PAGE`] for the page area, so the standard
//! look has exactly the surfaces it always had.
//!
//! With the standard tree, top to bottom: the tab strip (40 px), the toolbar (44 px) with back,
//! forward, reload-or-stop and the address line, the page, and the status strip (20 px) if there
//! is room. Compact density makes the tab strip and toolbar lower (32 and 36 px). Every size
//! scales with `[ui] font-size`: times font-size / 14, rounded to whole logical pixels. Nothing
//! is measured. [`LayoutOptions`] are `[chrome.standard]`'s options as a tree.
//!
//! Must not own: applying placement to surfaces (the host), which surfaces are listed now (the
//! standard chrome), or what a tree may be (`sighurt_api::ui`'s check).

use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::TabId;
use sighurt_api::keymap::Frontend;
pub use sighurt_api::ui::{
    AddressBar, Anchor, Button, Component, Density, Kind, Layer, Node, Show, Side, Size, TabBar,
};

use crate::extension::SurfaceName;
use crate::text::{at_size, TextStyle, DEFAULT_FONT_SIZE};

/// Heights at the default font size: normal, then compact.
const TAB_STRIP_HEIGHT: [u32; 2] = [40, 32];
const TOOLBAR_HEIGHT: [u32; 2] = [44, 36];
const TAB_HEIGHT: [u32; 2] = [32, 28];
const ADDRESS_HEIGHT: [u32; 2] = [32, 28];
/// Height of the status line.
const STATUS_HEIGHT: u32 = 20;

/// Space around the tab strip's and the tab list's contents, and between tabs.
const GAP: u32 = 4;
/// Space around the toolbar's contents.
const PADDING: u32 = 8;
/// Tabs in a strip share its width equally, between these widths.
const TAB_MIN_WIDTH: u32 = 64;
const TAB_MAX_WIDTH: u32 = 220;
/// The tab list's width, at most half the row's.
const LIST_WIDTH: u32 = 220;
/// The square toolbar buttons and the new-tab button in a strip.
const BUTTON: u32 = 28;
/// The close button inside a tab, and its distance from the tab's right edge.
const CLOSE_BUTTON: u32 = 20;
const CLOSE_INSET: u32 = 6;
/// The status overlay is half as wide as the page, but at least this wide.
const OVERLAY_MIN_WIDTH: u32 = 200;

/// The surface with the address bar, and the tabs when they are next to it: [`Layout::chrome`].
pub const CHROME: SurfaceName = "chrome";
/// The tabs' surface, when they are not next to the address bar.
pub const TABS: SurfaceName = "tabs";
/// The status line's surface, a strip or an overlay: [`Layout::status`].
pub const STATUS: SurfaceName = "status";
/// The page area's surface, while the chrome draws a page there: [`Layout::page`].
pub const PAGE: SurfaceName = "page";

/// Where the tabs go: `[chrome.standard] tabs`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tabs {
    /// A strip across the top.
    #[default]
    Top,
    /// A list left of the page, below the toolbar, in a surface of its own.
    Left,
    /// Nowhere: tabs are reached by keys and commands.
    Hidden,
}

/// Where the status line goes: `[chrome.standard] status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// A strip below the page.
    #[default]
    Strip,
    /// Over the page's bottom-left corner, without taking room from it.
    Overlay,
}

/// The standard chrome's options, `[chrome.standard]`: a tree ([`LayoutOptions::tree`]) and a
/// density, in the words the configuration file used before trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutOptions {
    /// Where the tabs go.
    pub tabs: Tabs,
    /// Where the status line goes, for hovered links and `sig`'s notices. It is its own
    /// surface, separate from the chrome at the top.
    pub status: Status,
    /// Back, forward and reload buttons before the address line.
    pub buttons: bool,
    /// A lower tab strip and toolbar: 32 and 36 px instead of 40 and 44, at 14 px.
    pub compact: bool,
    /// The engine's name in each engine page's tab, and at the address field's right end. It
    /// takes room from the text beside it, never from the layout.
    pub engine_label: bool,
    /// `[ui] font-size`, in logical pixels: every size and text style scales with it.
    pub font_size: u16,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self {
            tabs: Tabs::Top,
            status: Status::Strip,
            buttons: true,
            compact: false,
            engine_label: true,
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

impl LayoutOptions {
    /// The tree these options describe: the standard one with the tabs placed, the status
    /// line placed and the components' fields set.
    pub fn tree(&self) -> Node {
        let mut tree = Node::standard();
        let mut tabs = TabBar::default();
        tabs.engine_label = self.engine_label;
        let mut bar = AddressBar::default();
        bar.engine_button = self.engine_label;
        if !self.buttons {
            bar.buttons.clear();
        }
        let changed = tree.change(Frontend::Gui, |tree| {
            tree.place(Component::AddressBar(bar), Side::Top);
            match self.tabs {
                Tabs::Top => tree.place(Component::TabBar(tabs), Side::Top),
                Tabs::Left => tree.place(Component::TabBar(tabs), Side::Left),
                Tabs::Hidden => tree.remove(Kind::TabBar),
            }
            if self.status == Status::Overlay {
                let status = Component::StatusBar(Default::default());
                tree.overlay(status, Anchor::BottomLeft, Show::WhenNeeded);
            }
        });
        debug_assert!(changed.is_ok(), "{changed:?}");
        tree
    }

    /// The density these options describe.
    pub fn density(&self) -> Density {
        match self.compact {
            true => Density::Compact,
            false => Density::Normal,
        }
    }
}

/// What the chrome lays out: the window's tree, its density and the font size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arrangement {
    /// The window's tree; one that passes `Node::check` for the window.
    pub tree: Node,
    /// How tall the tab strip and toolbar are.
    pub density: Density,
    /// `[ui] font-size`, in logical pixels.
    pub font_size: u16,
}

impl Default for Arrangement {
    fn default() -> Self {
        LayoutOptions::default().into()
    }
}

impl From<LayoutOptions> for Arrangement {
    fn from(options: LayoutOptions) -> Self {
        Self {
            tree: options.tree(),
            density: options.density(),
            font_size: options.font_size,
        }
    }
}

impl Arrangement {
    /// The sizes it lays out with.
    pub fn sizing(&self) -> Sizing {
        Sizing {
            font_size: self.font_size,
            density: self.density,
        }
    }
}

/// The font size and density every size and text style scales with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizing {
    /// `[ui] font-size`, in logical pixels.
    pub font_size: u16,
    /// How tall the tab strip and toolbar are.
    pub density: Density,
}

impl Default for Sizing {
    fn default() -> Self {
        Self {
            font_size: DEFAULT_FONT_SIZE,
            density: Density::Normal,
        }
    }
}

impl Sizing {
    /// `px` logical pixels at the default font size, at this one, rounded to whole pixels.
    pub fn px(&self, px: u32) -> u32 {
        at_size(px, self.font_size)
    }

    /// `px` logical pixels at the default font size, at this one, unrounded: for drawing.
    pub fn scaled(&self, px: f32) -> f32 {
        px * f32::from(self.font_size) / f32::from(DEFAULT_FONT_SIZE)
    }

    /// `style` at this font size.
    pub fn text(&self, style: TextStyle) -> TextStyle {
        style.at(self.font_size)
    }

    /// The index of the density in the height tables.
    fn index(&self) -> usize {
        usize::from(self.density == Density::Compact)
    }
}

/// The edge of a region that faces the page, where its separator line is drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Edge {
    /// Its top edge: it is below the page.
    Top,
    /// Its bottom edge: it is above the page.
    #[default]
    Bottom,
    /// Its left edge: it is right of the page.
    Left,
    /// Its right edge: it is left of the page.
    Right,
}

/// One of the chrome's surfaces: its name from the pool, where it is, and what is on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Surface {
    /// Its name.
    pub name: SurfaceName,
    /// Where it is.
    pub rect: WindowRect,
    /// The components on it.
    pub kinds: Vec<Kind>,
    /// When it shows, if it is an overlay layer.
    pub layer: Option<Show>,
}

/// Where everything goes, in window-logical coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The window's size.
    pub window: LogicalSize,
    /// The sizes it was laid out with.
    pub sizing: Sizing,
    /// The tab bar shown, if any.
    pub tab_bar: Option<TabBar>,
    /// The address bar shown, if any.
    pub address_bar: Option<AddressBar>,
    /// Whether the tabs are a list (in a row) rather than a strip (in a column).
    pub tabs_vertical: bool,
    /// Where the status line lies over the page and when it shows, if it is an overlay layer.
    pub status_layer: Option<(Anchor, Show)>,
    /// The chrome's surfaces, the page area's not among them, bottom to top: the components
    /// inside a row around the page before those outside it, overlay layers last.
    pub surfaces: Vec<Surface>,
    /// The surface with the address bar ([`CHROME`]), and the tabs when they are next to it.
    pub chrome: WindowRect,
    /// The tab strip; empty unless the tabs are a strip.
    pub tab_strip: WindowRect,
    /// The tab list; empty unless the tabs are a list.
    pub tab_list: WindowRect,
    /// One entry per tab, in tab order, unless the tabs are hidden. Tabs that don't fit have
    /// empty rectangles.
    pub tabs: Vec<TabLayout>,
    /// The tab list's first row shown: it scrolls by whole rows. The rows are the tabs, then
    /// the new-tab button.
    pub first_row: usize,
    /// How many rows the tab list shows at once.
    pub rows: usize,
    /// The new-tab button: after the last tab, in the strip or as the list's last row.
    pub new_tab: WindowRect,
    /// The toolbar.
    pub toolbar: WindowRect,
    /// The back button; empty without it.
    pub back: WindowRect,
    /// The forward button; empty without it.
    pub forward: WindowRect,
    /// The reload button, which is a stop button while the page loads; empty without it.
    pub reload: WindowRect,
    /// The address field, including its border.
    pub address: WindowRect,
    /// The page: the active view's target, or where the chrome draws its own pages.
    pub page: WindowRect,
    /// The status line: the strip (empty when there is no room for it), or the overlay over
    /// the page's corner, as tall as the strip and half as wide as the page, but at least
    /// 200 px and at most the page's width.
    pub status: WindowRect,
    /// The edges of the tab strip or list, the toolbar and the status strip that face the page.
    pub edges: Edges,
}

/// The edge facing the page of each strip and list ([`Edge`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Edges {
    /// The tab strip's or the tab list's.
    pub tabs: Edge,
    /// The toolbar's.
    pub toolbar: Edge,
    /// The status strip's.
    pub status: Edge,
}

/// One tab's place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabLayout {
    /// The tab.
    pub id: TabId,
    /// The whole tab; empty if it doesn't fit.
    pub rect: WindowRect,
    /// Its close button, shown while there is more than one tab (the last tab can't close).
    pub close: Option<WindowRect>,
}

/// A component as the container placed it.
struct Placed {
    component: Component,
    /// What it got, clipped.
    rect: WindowRect,
    /// Its size along its parent's axis before clipping.
    full: u32,
    /// Whether its parent is a column.
    in_column: bool,
    /// Whether it comes before the page in its parent.
    before_page: bool,
}

/// What a walk of the tree collects.
#[derive(Default)]
struct Walk {
    placed: Vec<Placed>,
    page: Option<WindowRect>,
    /// Runs of components: their indexes in `placed`, depth, and the layer's show.
    runs: Vec<(Vec<usize>, usize, Option<Show>)>,
    status_layer: Option<(Anchor, Show)>,
}

impl Layout {
    /// The layout of `tree` for a window of `window` size showing `tabs`, in tab order, with
    /// the tab list's rows from `first_row` on (as far as there are rows to show).
    pub fn from_tree(
        window: LogicalSize,
        tabs: &[TabId],
        tree: &Node,
        sizing: Sizing,
        first_row: usize,
    ) -> Self {
        let mut walk = Walk::default();
        let whole = rect(0, 0, window.width, window.height);
        lay(tree, whole, 0, sizing, &mut walk);

        let page = walk.page.unwrap_or_default();
        let (page_x, page_y) = origin(page);
        // What isn't shown is empty, where the layout before trees put it.
        let mut layout = Layout {
            window,
            sizing,
            page,
            status_layer: walk.status_layer,
            tab_strip: rect(0, 0, window.width, 0),
            tab_list: rect(page_x, page_y, 0, page.height),
            new_tab: rect(sizing.px(GAP), 0, 0, 0),
            ..Layout::default()
        };
        for placed in &walk.placed {
            match &placed.component {
                Component::TabBar(bar) => {
                    layout.tab_bar = Some(*bar);
                    layout.tabs_vertical = !placed.in_column;
                    layout.edges.tabs = edge(placed);
                    if placed.in_column {
                        layout.tab_strip = placed.rect;
                        layout.strip_tabs(tabs, bar, placed.full);
                    } else {
                        layout.tab_list = placed.rect;
                        layout.list_tabs(tabs, bar, first_row);
                    }
                }
                Component::AddressBar(bar) => {
                    layout.toolbar = placed.rect;
                    layout.edges.toolbar = edge(placed);
                    layout.toolbar_widgets(bar, placed.full);
                    layout.address_bar = Some(bar.clone());
                }
                Component::StatusBar(_) => {
                    layout.status = placed.rect;
                    layout.edges.status = edge(placed);
                }
                _ => {}
            }
        }

        // The runs become surfaces, named from the pool, innermost first, layers last.
        let mut runs = walk.runs;
        runs.sort_by_key(|(_, depth, layer)| (layer.is_some(), std::cmp::Reverse(*depth)));
        for (members, _, layer) in runs {
            let kinds: Vec<Kind> = members
                .iter()
                .map(|&i| walk.placed[i].component.kind())
                .collect();
            let name = if kinds.contains(&Kind::AddressBar) {
                CHROME
            } else if kinds.contains(&Kind::TabBar) {
                TABS
            } else if kinds.contains(&Kind::StatusBar) {
                STATUS
            } else {
                // Labels and the command line have no surface yet.
                continue;
            };
            // From the first component's corner, as long as what they got together: a
            // component after a cut one starts at its unclipped end, outside the run.
            let first = &walk.placed[members[0]];
            let along: u32 = members
                .iter()
                .map(|&i| {
                    let rect = walk.placed[i].rect;
                    if first.in_column {
                        rect.height
                    } else {
                        rect.width
                    }
                })
                .sum();
            let rect = match first.in_column {
                true => WindowRect {
                    height: along,
                    ..first.rect
                },
                false => WindowRect {
                    width: along,
                    ..first.rect
                },
            };
            if name == CHROME {
                layout.chrome = rect;
            }
            layout.surfaces.push(Surface {
                name,
                rect,
                kinds,
                layer,
            });
        }
        layout
    }

    /// Lays out the tabs in a strip `full` high, at [`Layout::tab_strip`].
    fn strip_tabs(&mut self, tabs: &[TabId], bar: &TabBar, full: u32) {
        let sizing = self.sizing;
        let px = |size| sizing.px(size);
        let strip = self.tab_strip;
        let (x0, y0) = origin(strip);
        let width = strip.width;
        let tab_height = px(TAB_HEIGHT[sizing.index()]);
        let (gap, button) = (px(GAP), px(BUTTON));
        let close_of = Self::close_of(sizing, tabs.len(), *bar);
        // Tabs share what the new-tab button leaves, equally; the button follows the last tab
        // and stays in the strip when they overflow, and tabs never run under it.
        let content_end = width.saturating_sub(gap);
        let count = u32::try_from(tabs.len()).unwrap_or(u32::MAX);
        let room = match bar.new_tab {
            true => content_end.saturating_sub(gap + gap + button),
            false => content_end.saturating_sub(gap),
        };
        let tab_width = match count {
            0 => 0,
            n => {
                (room.saturating_sub(gap * (n - 1)) / n).clamp(px(TAB_MIN_WIDTH), px(TAB_MAX_WIDTH))
            }
        };
        let tabs_end = gap.saturating_add(count.saturating_mul(tab_width + gap));
        let new_tab_x = tabs_end.min(content_end.saturating_sub(button)).max(gap);
        let area_end = match bar.new_tab {
            true => new_tab_x.saturating_sub(gap),
            false => content_end,
        };
        let tab_area = intersect(rect(x0, y0, area_end, full), strip);
        let tab_top = y0 + full.saturating_sub(tab_height) / 2;
        self.tabs = tabs
            .iter()
            .zip(0u32..)
            .map(|(&id, i)| {
                let x = gap.saturating_add(i.saturating_mul(tab_width + gap));
                let whole = rect(x0.saturating_add(x), tab_top, tab_width, tab_height);
                let tab = intersect(whole, tab_area);
                let close = close_of(tab, whole);
                TabLayout {
                    id,
                    rect: tab,
                    close,
                }
            })
            .collect();
        let y = y0 + full.saturating_sub(button) / 2;
        self.new_tab = match bar.new_tab {
            true => intersect(rect(x0 + new_tab_x, y, button, button), strip),
            false => rect(x0 + gap, y0, 0, 0),
        };
    }

    /// Lays out the tabs in a list at [`Layout::tab_list`]: rows of its width, one per tab, then
    /// the new-tab button; only whole rows show.
    fn list_tabs(&mut self, tabs: &[TabId], bar: &TabBar, first_row: usize) {
        let sizing = self.sizing;
        let px = |size| sizing.px(size);
        let list = self.tab_list;
        let (x0, y0) = origin(list);
        let tab_height = px(TAB_HEIGHT[sizing.index()]);
        let gap = px(GAP);
        let close_of = Self::close_of(sizing, tabs.len(), *bar);
        let pitch = tab_height + gap;
        let rows = (list.height.saturating_sub(gap) / pitch) as usize;
        let count = tabs.len() + usize::from(bar.new_tab);
        let first_row = first_row.min(count.saturating_sub(rows));
        let row = |index: usize| match index.checked_sub(first_row) {
            Some(row) if row < rows => {
                let y = y0 + gap + row as u32 * pitch;
                rect(x0 + gap, y, list.width.saturating_sub(2 * gap), tab_height)
            }
            _ => rect(x0 + gap, y0, 0, 0),
        };
        self.tabs = tabs
            .iter()
            .enumerate()
            .map(|(i, &id)| {
                let tab = row(i);
                TabLayout {
                    id,
                    rect: tab,
                    close: close_of(tab, tab),
                }
            })
            .collect();
        self.new_tab = match bar.new_tab {
            true => row(tabs.len()),
            false => rect(x0 + gap, y0, 0, 0),
        };
        self.first_row = first_row;
        self.rows = rows;
    }

    /// A tab's close button, at its right end, centred: from the tab as clipped and as whole.
    fn close_of(
        sizing: Sizing,
        tabs: usize,
        bar: TabBar,
    ) -> impl Fn(WindowRect, WindowRect) -> Option<WindowRect> {
        let close = sizing.px(CLOSE_BUTTON);
        let inset = sizing.px(CLOSE_INSET);
        let closable = tabs > 1 && bar.close_buttons;
        move |tab: WindowRect, whole: WindowRect| {
            let right = u32::try_from(whole.x).unwrap_or(0) + whole.width;
            let y = u32::try_from(whole.y).unwrap_or(0) + whole.height.saturating_sub(close) / 2;
            let x = right.saturating_sub(inset + close);
            closable.then(|| intersect(rect(x, y, close, close), tab))
        }
    }

    /// Lays out the toolbar's buttons and address field, in a toolbar `full` high.
    fn toolbar_widgets(&mut self, bar: &AddressBar, full: u32) {
        let sizing = self.sizing;
        let px = |size| sizing.px(size);
        let toolbar = self.toolbar;
        let (x0, y0) = origin(toolbar);
        let (gap, button, padding) = (px(GAP), px(BUTTON), px(PADDING));
        let button_y = y0 + full.saturating_sub(button) / 2;
        let none = rect(x0 + padding, button_y, 0, 0);
        (self.back, self.forward, self.reload) = (none, none, none);
        let mut index = 0;
        for &kind in &bar.buttons {
            let x = x0 + padding + index * (button + gap);
            let place = intersect(rect(x, button_y, button, button), toolbar);
            let slot = match kind {
                Button::Back => &mut self.back,
                Button::Forward => &mut self.forward,
                Button::Reload => &mut self.reload,
            };
            *slot = place;
            index += 1;
        }
        let address_x = padding + index * (button + gap);
        let address_height = px(ADDRESS_HEIGHT[sizing.index()]);
        let address = rect(
            x0 + address_x,
            y0 + full.saturating_sub(address_height) / 2,
            toolbar.width.saturating_sub(address_x + padding),
            address_height,
        );
        self.address = intersect(address, toolbar);
    }

    /// Where a tab is, if the layout has it.
    pub fn tab(&self, id: TabId) -> Option<&TabLayout> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// The surface `kind` is drawn on, if it is shown.
    pub fn surface_of(&self, kind: Kind) -> Option<SurfaceName> {
        if kind == Kind::PageView {
            return Some(PAGE);
        }
        let surface = self.surfaces.iter().find(|s| s.kinds.contains(&kind))?;
        Some(surface.name)
    }

    /// The surface called `name`, if the layout has it.
    pub fn surface(&self, name: SurfaceName) -> Option<&Surface> {
        self.surfaces.iter().find(|surface| surface.name == name)
    }

    /// A row of the tab list: the tabs, then the new-tab button.
    pub(crate) fn row_pitch(&self) -> u32 {
        let sizing = self.sizing;
        sizing.px(TAB_HEIGHT[sizing.index()]) + sizing.px(GAP)
    }
}

/// Lays `node` out in `area`, `depth` containers deep.
fn lay(node: &Node, area: WindowRect, depth: usize, sizing: Sizing, walk: &mut Walk) {
    match node {
        // Only the page stands alone: anything else is a component without a container,
        // which no tree that passes the check has.
        Node::Leaf(component) => {
            if component.kind() == Kind::PageView {
                walk.page = Some(area);
            }
        }
        Node::Column(children) | Node::Row(children) => {
            let column = matches!(node, Node::Column(_));
            let length = if column { area.height } else { area.width };
            let page = children.iter().position(Node::holds_page);
            let component = |i: usize| match &children[i] {
                Node::Leaf(component) if Some(i) != page => Some(component),
                _ => None,
            };
            let is_status =
                |i: usize| column && component(i).is_some_and(|c| c.kind() == Kind::StatusBar);
            // What each child gets along the axis: components in order, a status strip last
            // and only if more than its height is left, the page the rest.
            let full: Vec<u32> = (0..children.len())
                .map(|i| component(i).map_or(0, |c| natural(c, column, length, sizing)))
                .collect();
            let mut got = vec![0; children.len()];
            // Where each component would end unclipped: its own size, or what a fixed size was
            // clamped to.
            let mut nominal_size = full.clone();
            let mut left = length;
            // What the editor and the status line need of what is left after child `i`, along
            // a column: a fixed size never takes that. In a row a fixed size takes at most
            // half, so the branch with the editor keeps room.
            let reserved_after = |i: usize| -> u32 {
                let later = (0..children.len()).filter(|&j| {
                    j != i && (Some(j) == page || is_status(j) || j > i && component(j).is_some())
                });
                later.map(|j| reserve(&children[j], sizing)).sum()
            };
            for i in 0..children.len() {
                if component(i).is_some() && !is_status(i) {
                    let fixed = component(i).is_some_and(|c| matches!(c.size(), Size::Fixed(_)));
                    let room = match (fixed, column) {
                        (false, _) => left,
                        (true, true) => left.saturating_sub(reserved_after(i)),
                        (true, false) => left.min(length / 2),
                    };
                    got[i] = full[i].min(room);
                    if room < left {
                        nominal_size[i] = got[i];
                    }
                    left -= got[i];
                }
            }
            for i in 0..children.len() {
                if is_status(i) && left > full[i] {
                    got[i] = full[i];
                    left -= full[i];
                }
            }
            if let Some(i) = page {
                got[i] = left;
            }
            let (x0, y0) = origin(area);
            // Where the next child starts: after what the ones before it got, except that a
            // component after another in a run starts where that one would end unclipped, as
            // the layout before trees placed the toolbar below a cut tab strip.
            let mut at = 0;
            let mut nominal: Option<u32> = None;
            let mut run: Vec<usize> = Vec::new();
            let end_run = |run: &mut Vec<usize>, walk: &mut Walk| {
                if !run.is_empty() {
                    walk.runs.push((std::mem::take(run), depth, None));
                }
            };
            for (i, child) in children.iter().enumerate() {
                let starts = match component(i) {
                    Some(_) if !is_status(i) => nominal.unwrap_or(at),
                    _ => at,
                };
                let rect = match column {
                    true => rect(x0, y0 + starts, area.width, got[i]),
                    false => rect(x0 + starts, y0, got[i], area.height),
                };
                at += got[i];
                nominal = match component(i) {
                    Some(_) if !is_status(i) => Some(starts + nominal_size[i]),
                    _ => None,
                };
                let Some(component) = component(i) else {
                    end_run(&mut run, walk);
                    lay(child, rect, depth + 1, sizing, walk);
                    continue;
                };
                // A status strip is a surface of its own.
                if is_status(i) {
                    end_run(&mut run, walk);
                }
                walk.placed.push(Placed {
                    component: component.clone(),
                    rect,
                    full: full[i],
                    in_column: column,
                    before_page: page.is_some_and(|page| i < page),
                });
                run.push(walk.placed.len() - 1);
                if is_status(i) {
                    end_run(&mut run, walk);
                }
            }
            end_run(&mut run, walk);
        }
        Node::Overlay { base, layers } => {
            lay(base, area, depth, sizing, walk);
            for layer in layers {
                let Component::StatusBar(_) = layer.component else {
                    // Labels have no surface yet.
                    continue;
                };
                let status = overlay_rect(area, layer.anchor, sizing);
                walk.placed.push(Placed {
                    component: layer.component.clone(),
                    rect: status,
                    full: status.height,
                    in_column: true,
                    before_page: false,
                });
                walk.status_layer = Some((layer.anchor, layer.show));
                let index = walk.placed.len() - 1;
                walk.runs.push((vec![index], depth, Some(layer.show)));
            }
        }
        // A kind of node this interpreter doesn't know yet shows nothing.
        _ => {}
    }
}

/// The height `node` needs for the editor and the status line it holds (its own size for an
/// address bar or a status strip, the sum down a column, the most across a row).
fn reserve(node: &Node, sizing: Sizing) -> u32 {
    match node {
        Node::Leaf(Component::AddressBar(_)) => sizing.px(TOOLBAR_HEIGHT[sizing.index()]),
        // One more than the strip, which is kept only while more than its height is left.
        Node::Leaf(Component::StatusBar(_)) => sizing.px(STATUS_HEIGHT) + 1,
        Node::Column(children) => children.iter().map(|c| reserve(c, sizing)).sum(),
        Node::Row(children) => children
            .iter()
            .map(|c| reserve(c, sizing))
            .max()
            .unwrap_or(0),
        Node::Overlay { base, .. } => reserve(base, sizing),
        _ => 0,
    }
}

/// A component's own size along its parent's axis (a column's: `column`), `length` long.
fn natural(component: &Component, column: bool, length: u32, sizing: Sizing) -> u32 {
    let px = |size| sizing.px(size);
    match (component, component.size()) {
        (_, Size::Fixed(size)) => px(u32::from(size)),
        (Component::TabBar(_), _) if column => px(TAB_STRIP_HEIGHT[sizing.index()]),
        // A list: at most half the row.
        (Component::TabBar(_), _) => px(LIST_WIDTH).min(length / 2),
        (Component::AddressBar(_), _) => px(TOOLBAR_HEIGHT[sizing.index()]),
        (Component::StatusBar(_), _) => px(STATUS_HEIGHT),
        _ => 0,
    }
}

/// The status overlay over `base`, at `anchor`: as tall as the strip and half as wide as the
/// base, but at least 200 px and at most its width; empty when the base is shorter than it.
fn overlay_rect(base: WindowRect, anchor: Anchor, sizing: Sizing) -> WindowRect {
    let status_height = sizing.px(STATUS_HEIGHT);
    let width = (base.width / 2)
        .max(sizing.px(OVERLAY_MIN_WIDTH))
        .min(base.width);
    let height = if base.height >= status_height {
        status_height
    } else {
        0
    };
    let (x0, y0) = origin(base);
    let x = match anchor {
        Anchor::BottomLeft | Anchor::TopLeft => x0,
        Anchor::BottomRight | Anchor::TopRight => x0 + base.width - width,
    };
    let y = match anchor {
        Anchor::BottomLeft | Anchor::BottomRight => y0 + base.height - height,
        Anchor::TopLeft | Anchor::TopRight => y0,
    };
    rect(x, y, width, height)
}

/// The edge of `placed` that faces the page.
fn edge(placed: &Placed) -> Edge {
    match (placed.in_column, placed.before_page) {
        (true, true) => Edge::Bottom,
        (true, false) => Edge::Top,
        (false, true) => Edge::Right,
        (false, false) => Edge::Left,
    }
}

/// `rect`'s corner, which the layout never lets go negative.
fn origin(rect: WindowRect) -> (u32, u32) {
    (
        u32::try_from(rect.x).unwrap_or(0),
        u32::try_from(rect.y).unwrap_or(0),
    )
}

/// A rectangle from unsigned coordinates, which the layout never lets go negative.
fn rect(x: u32, y: u32, width: u32, height: u32) -> WindowRect {
    WindowRect {
        x: i32::try_from(x).unwrap_or(i32::MAX),
        y: i32::try_from(y).unwrap_or(i32::MAX),
        width,
        height,
    }
}

/// The part of `a` inside `b`, or an empty rectangle at `a`'s corner.
pub(crate) fn intersect(a: WindowRect, b: WindowRect) -> WindowRect {
    let left = i64::from(a.x).max(i64::from(b.x));
    let top = i64::from(a.y).max(i64::from(b.y));
    let right = (i64::from(a.x) + i64::from(a.width)).min(i64::from(b.x) + i64::from(b.width));
    let bottom = (i64::from(a.y) + i64::from(a.height)).min(i64::from(b.y) + i64::from(b.height));
    if right <= left || bottom <= top {
        return WindowRect {
            width: 0,
            height: 0,
            ..a
        };
    }
    // Both corners lie within `a` or `b`, so they fit an i32 and the sizes a u32.
    WindowRect {
        x: left as i32,
        y: top as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    }
}

/// The layout before trees, `[chrome.standard]`'s options straight to rectangles: kept as the
/// oracle that [`Layout::from_tree`] is held to.
#[cfg(test)]
mod oracle {
    use super::*;

    /// The old layout's fields.
    #[derive(Clone, Debug, Default, PartialEq, Eq)]
    pub struct OldLayout {
        pub window: LogicalSize,
        pub options: LayoutOptions,
        pub chrome: WindowRect,
        pub tab_strip: WindowRect,
        pub tab_list: WindowRect,
        pub tabs: Vec<TabLayout>,
        pub first_row: usize,
        pub rows: usize,
        pub new_tab: WindowRect,
        pub toolbar: WindowRect,
        pub back: WindowRect,
        pub forward: WindowRect,
        pub reload: WindowRect,
        pub address: WindowRect,
        pub page: WindowRect,
        pub status: WindowRect,
    }

    /// `Layout::new` as it was.
    pub fn old_layout(
        window: LogicalSize,
        tabs: &[TabId],
        options: LayoutOptions,
        first_row: usize,
    ) -> OldLayout {
        let px = |size| at_size(size, options.font_size);
        let size = usize::from(options.compact);
        let (width, height) = (window.width, window.height);

        let strip_height = match options.tabs {
            Tabs::Top => px(TAB_STRIP_HEIGHT[size]),
            Tabs::Left | Tabs::Hidden => 0,
        };
        let tab_strip = rect(0, 0, width, strip_height.min(height));
        let toolbar_full = px(TOOLBAR_HEIGHT[size]);
        let toolbar_height = toolbar_full.min(height - tab_strip.height);
        let toolbar = rect(0, strip_height, width, toolbar_height);
        let chrome = rect(0, 0, width, tab_strip.height + toolbar_height);

        let rest = height - chrome.height;
        let status_height = px(STATUS_HEIGHT);
        let strip = options.status == Status::Strip && rest > status_height;
        let page_height = rest - if strip { status_height } else { 0 };
        let list_width = match options.tabs {
            Tabs::Left => px(LIST_WIDTH).min(width / 2),
            Tabs::Top | Tabs::Hidden => 0,
        };
        let page_width = width - list_width;
        let tab_list = rect(0, chrome.height, list_width, page_height);
        let page = rect(list_width, chrome.height, page_width, page_height);
        let status = match options.status {
            Status::Strip => {
                let strip_height = if strip { status_height } else { 0 };
                rect(0, height - strip_height, width, strip_height)
            }
            Status::Overlay => {
                let overlay_width = (page_width / 2).max(px(OVERLAY_MIN_WIDTH)).min(page_width);
                let overlay_height = if page_height >= status_height {
                    status_height
                } else {
                    0
                };
                let y = chrome.height + page_height - overlay_height;
                rect(list_width, y, overlay_width, overlay_height)
            }
        };

        let tab_height = px(TAB_HEIGHT[size]);
        let (gap, button, close) = (px(GAP), px(BUTTON), px(CLOSE_BUTTON));
        let closable = tabs.len() > 1;
        let close_of = |tab: WindowRect, full: WindowRect| {
            let right = u32::try_from(full.x).unwrap_or(0) + full.width;
            let y = u32::try_from(full.y).unwrap_or(0) + full.height.saturating_sub(close) / 2;
            let x = right.saturating_sub(px(CLOSE_INSET) + close);
            closable.then(|| intersect(rect(x, y, close, close), tab))
        };
        let (tabs, new_tab, first_row, rows) = match options.tabs {
            Tabs::Top => {
                let content_end = width.saturating_sub(gap);
                let count = u32::try_from(tabs.len()).unwrap_or(u32::MAX);
                let room = content_end.saturating_sub(gap + gap + button);
                let tab_width = match count {
                    0 => 0,
                    n => (room.saturating_sub(gap * (n - 1)) / n)
                        .clamp(px(TAB_MIN_WIDTH), px(TAB_MAX_WIDTH)),
                };
                let tabs_end = gap.saturating_add(count.saturating_mul(tab_width + gap));
                let new_tab_x = tabs_end.min(content_end.saturating_sub(button)).max(gap);
                let tab_area = intersect(
                    rect(0, 0, new_tab_x.saturating_sub(gap), strip_height),
                    tab_strip,
                );
                let tab_top = (strip_height - tab_height) / 2;
                let tabs = tabs
                    .iter()
                    .zip(0u32..)
                    .map(|(&id, i)| {
                        let x = gap.saturating_add(i.saturating_mul(tab_width + gap));
                        let full = rect(x, tab_top, tab_width, tab_height);
                        let tab = intersect(full, tab_area);
                        let close = close_of(tab, full);
                        TabLayout {
                            id,
                            rect: tab,
                            close,
                        }
                    })
                    .collect();
                let new_tab = rect(new_tab_x, (strip_height - button) / 2, button, button);
                (tabs, intersect(new_tab, tab_strip), 0, 0)
            }
            Tabs::Left => {
                let pitch = tab_height + gap;
                let rows = (tab_list.height.saturating_sub(gap) / pitch) as usize;
                let first_row = first_row.min((tabs.len() + 1).saturating_sub(rows));
                let row = |index: usize| match index.checked_sub(first_row) {
                    Some(row) if row < rows => {
                        let y = chrome.height + gap + row as u32 * pitch;
                        rect(gap, y, list_width.saturating_sub(2 * gap), tab_height)
                    }
                    _ => rect(gap, chrome.height, 0, 0),
                };
                let tabs: Vec<_> = tabs
                    .iter()
                    .enumerate()
                    .map(|(i, &id)| {
                        let tab = row(i);
                        TabLayout {
                            id,
                            rect: tab,
                            close: close_of(tab, tab),
                        }
                    })
                    .collect();
                let new_tab = row(tabs.len());
                (tabs, new_tab, first_row, rows)
            }
            Tabs::Hidden => (Vec::new(), rect(gap, 0, 0, 0), 0, 0),
        };

        let padding = px(PADDING);
        let button_y = strip_height + (toolbar_full - button) / 2;
        let button_at = |i: u32| match options.buttons {
            true => {
                let x = padding + i * (button + gap);
                intersect(rect(x, button_y, button, button), toolbar)
            }
            false => rect(padding, button_y, 0, 0),
        };
        let address_x = padding
            + if options.buttons {
                3 * (button + gap)
            } else {
                0
            };
        let address_height = px(ADDRESS_HEIGHT[size]);
        let address = rect(
            address_x,
            strip_height + (toolbar_full - address_height) / 2,
            width.saturating_sub(address_x + padding),
            address_height,
        );

        OldLayout {
            window,
            options,
            chrome,
            tab_strip,
            tab_list,
            tabs,
            first_row,
            rows,
            new_tab,
            toolbar,
            back: button_at(0),
            forward: button_at(1),
            reload: button_at(2),
            address: intersect(address, toolbar),
            page,
            status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::oracle::{old_layout, OldLayout};
    use super::*;

    fn tabs(n: usize) -> Vec<TabId> {
        (0..n).map(|_| TabId::next()).collect()
    }

    fn size(width: u32, height: u32) -> LogicalSize {
        LogicalSize { width, height }
    }

    /// The layout of `options`, through their tree.
    fn layout_of(
        window: LogicalSize,
        tabs: &[TabId],
        options: LayoutOptions,
        first_row: usize,
    ) -> Layout {
        let arrangement = Arrangement::from(options);
        Layout::from_tree(
            window,
            tabs,
            &arrangement.tree,
            arrangement.sizing(),
            first_row,
        )
    }

    fn inside(inner: WindowRect, outer: WindowRect) -> bool {
        inner.size().is_empty() || intersect(inner, outer) == inner
    }

    fn with(change: impl FnOnce(&mut LayoutOptions)) -> LayoutOptions {
        let mut options = LayoutOptions::default();
        change(&mut options);
        options
    }

    /// Every combination of the options, at every font size from 12 to 24.
    fn every_option() -> Vec<LayoutOptions> {
        let mut all = Vec::new();
        for tabs in [Tabs::Top, Tabs::Left, Tabs::Hidden] {
            for status in [Status::Strip, Status::Overlay] {
                for bits in 0..8 {
                    for font_size in 12..=24 {
                        all.push(LayoutOptions {
                            tabs,
                            status,
                            buttons: bits & 1 != 0,
                            compact: bits & 2 != 0,
                            engine_label: bits & 4 != 0,
                            font_size,
                        });
                    }
                }
            }
        }
        all
    }

    const ODD_SIZES: [(u32, u32); 10] = [
        (0, 0),
        (1, 1),
        (37, 39),
        (99, 83),
        (101, 85),
        (157, 104),
        (333, 105),
        (641, 481),
        (1001, 777),
        (3441, 1439),
    ];

    /// The old layout's fields of `layout`, with the options its facts say.
    fn old_fields(layout: &Layout, options: LayoutOptions) -> OldLayout {
        OldLayout {
            window: layout.window,
            options,
            chrome: layout.chrome,
            tab_strip: layout.tab_strip,
            tab_list: layout.tab_list,
            tabs: layout.tabs.clone(),
            first_row: layout.first_row,
            rows: layout.rows,
            new_tab: layout.new_tab,
            toolbar: layout.toolbar,
            back: layout.back,
            forward: layout.forward,
            reload: layout.reload,
            address: layout.address,
            page: layout.page,
            status: layout.status,
        }
    }

    /// The tree interpreter puts everything where the options did before trees, field for
    /// field, empty rectangles at the same place too: over every combination of the options,
    /// font sizes 12 to 24, the odd sizes and tiny windows, tab counts and scroll positions.
    #[test]
    fn trees_lay_out_as_the_options_did() {
        let mut cases = 0;
        for options in every_option() {
            let arrangement = Arrangement::from(options);
            let sizing = arrangement.sizing();
            for (width, height) in ODD_SIZES.into_iter().chain([(1280, 800), (800, 620)]) {
                for count in [0, 1, 2, 7, 40] {
                    let ids = tabs(count);
                    for first_row in [0, 3, 100] {
                        let window = size(width, height);
                        let old = old_layout(window, &ids, options, first_row);
                        let new =
                            Layout::from_tree(window, &ids, &arrangement.tree, sizing, first_row);
                        let what = format!(
                            "{width}×{height}, {count} tabs from row {first_row}, {options:?}"
                        );
                        assert_eq!(old_fields(&new, options), old, "{what}");
                        // The new facts say what the options said.
                        assert_eq!(new.sizing.font_size, options.font_size, "{what}");
                        assert_eq!(new.sizing.density, options.density(), "{what}");
                        assert_eq!(new.tabs_vertical, options.tabs == Tabs::Left, "{what}");
                        assert_eq!(new.tab_bar.is_some(), options.tabs != Tabs::Hidden);
                        let labelled = new.tab_bar.is_none_or(|bar| bar.engine_label);
                        assert_eq!(labelled, options.engine_label || new.tab_bar.is_none());
                        let overlay = options.status == Status::Overlay;
                        assert_eq!(new.status_layer.is_some(), overlay, "{what}");
                        let bar = new.address_bar.as_ref().unwrap();
                        assert_eq!(bar.engine_button, options.engine_label);
                        assert_eq!(bar.buttons.is_empty(), !options.buttons);
                        cases += 1;
                    }
                }
            }
        }
        assert_eq!(cases, 624 * 12 * 5 * 3);
    }

    /// A fixed size never takes the editor's or the status line's room (review 1): tabs as
    /// tall as the window leave the toolbar and the status strip their heights, and the page a
    /// pixel; a list as wide as the window takes half of it.
    #[test]
    fn a_fixed_size_leaves_the_editor_and_the_status_their_room() {
        let mut tree = Node::standard();
        let Some(Component::TabBar(bar)) = tree.get_mut(Kind::TabBar) else {
            panic!()
        };
        bar.size = Size::Fixed(600);
        let layout = Layout::from_tree(size(800, 600), &tabs(3), &tree, Sizing::default(), 0);
        assert_eq!(layout.toolbar.height, 44);
        assert_eq!(layout.status.height, 20);
        assert_eq!(layout.page.height, 1);
        assert_eq!(layout.tab_strip.height, 600 - 44 - 20 - 1);
        assert_eq!(
            layout.toolbar.y,
            600 - 44 - 20 - 1,
            "below the clamped strip"
        );
        assert_eq!(layout.page.y, 600 - 20 - 1);
        let mut wide = tree.clone();
        wide.change(Frontend::Gui, |t| t.position(Kind::TabBar, Side::Left))
            .unwrap();
        let layout = Layout::from_tree(size(800, 600), &tabs(3), &wide, Sizing::default(), 0);
        assert_eq!((layout.tab_list.width, layout.page.width), (400, 400));
        // In a window too small for the standard tree it clips as before.
        let layout = Layout::from_tree(size(800, 50), &tabs(3), &tree, Sizing::default(), 0);
        assert_eq!(layout.toolbar.height, 44);
    }

    /// Every widget of `layout` with its name and the region it must stay in.
    fn widgets(layout: &Layout) -> Vec<(String, WindowRect, WindowRect)> {
        let tab_area = match layout.tabs_vertical {
            true => layout.tab_list,
            false => layout.tab_strip,
        };
        let mut widgets = vec![
            ("new tab".into(), layout.new_tab, tab_area),
            ("back".into(), layout.back, layout.toolbar),
            ("forward".into(), layout.forward, layout.toolbar),
            ("reload".into(), layout.reload, layout.toolbar),
            ("address".into(), layout.address, layout.toolbar),
        ];
        for (i, tab) in layout.tabs.iter().enumerate() {
            widgets.push((format!("tab {i}"), tab.rect, tab_area));
            if let Some(close) = tab.close {
                widgets.push((format!("close {i}"), close, tab.rect));
            }
        }
        widgets
    }

    #[test]
    fn regions_tile_the_window() {
        let layout = layout_of(size(1280, 800), &tabs(3), LayoutOptions::default(), 0);
        assert_eq!(layout.chrome, rect(0, 0, 1280, 84));
        assert_eq!(layout.page, rect(0, 84, 1280, 696));
        assert_eq!(layout.status, rect(0, 780, 1280, 20));
        assert_eq!(layout.address, rect(104, 46, 1168, 32));
        assert_eq!(layout.tabs[0].rect, rect(4, 4, 220, 32));
        assert_eq!(layout.tabs[1].rect.x, 228);
        assert_eq!(layout.new_tab.x, 4 + 3 * 224);
        assert!(layout.tab_list.size().is_empty());

        // The last tab can't close, so it has no close button.
        let layout = layout_of(size(1280, 800), &tabs(1), LayoutOptions::default(), 0);
        assert_eq!(layout.tabs[0].close, None);
    }

    /// `[chrome.standard]`: each option changes only what it says.
    #[test]
    fn each_option_moves_only_its_part() {
        let window = size(1280, 800);
        let ids = tabs(3);
        let layout = |options| layout_of(window, &ids, options, 0);

        // The status line over the page: the page keeps the strip's room, and the overlay is
        // half its width at its bottom-left corner.
        let overlay = layout(with(|o| o.status = Status::Overlay));
        assert_eq!(overlay.page, rect(0, 84, 1280, 716));
        assert_eq!(overlay.status, rect(0, 780, 640, 20));
        // At least 200 px, at most the page.
        let narrow = layout_of(
            size(300, 800),
            &ids,
            with(|o| o.status = Status::Overlay),
            0,
        );
        assert_eq!(narrow.status, rect(0, 780, 200, 20));
        let tiny = layout_of(
            size(150, 800),
            &ids,
            with(|o| o.status = Status::Overlay),
            0,
        );
        assert_eq!(tiny.status.width, 150);

        // No buttons: the address line starts where the back button was.
        let bare = layout(with(|o| o.buttons = false));
        assert_eq!(bare.address, rect(8, 46, 1264, 32));
        for button in [bare.back, bare.forward, bare.reload] {
            assert!(button.size().is_empty(), "{button:?}");
        }

        // Compact: 32 and 36 px, controls centred in them.
        let compact = layout(with(|o| o.compact = true));
        assert_eq!(compact.tab_strip.height, 32);
        assert_eq!(compact.toolbar, rect(0, 32, 1280, 36));
        assert_eq!(compact.tabs[0].rect, rect(4, 2, 220, 28));
        assert_eq!(compact.address, rect(104, 36, 1168, 28));
        assert_eq!(compact.page, rect(0, 68, 1280, 712));

        // Hidden tabs: the toolbar alone at the top, and no tab anywhere.
        let hidden = layout(with(|o| o.tabs = Tabs::Hidden));
        assert_eq!(hidden.chrome, rect(0, 0, 1280, 44));
        assert_eq!(hidden.toolbar, hidden.chrome);
        assert_eq!(hidden.address, rect(104, 6, 1168, 32));
        assert!(hidden.tabs.is_empty() && hidden.new_tab.size().is_empty());
        assert_eq!(hidden.page, rect(0, 44, 1280, 736));

        // Tabs on the left: a list below the toolbar, rows of its width, then the new-tab row.
        let left = layout(with(|o| o.tabs = Tabs::Left));
        assert_eq!(left.chrome, rect(0, 0, 1280, 44));
        assert_eq!(left.tab_list, rect(0, 44, 220, 736));
        assert_eq!(left.page, rect(220, 44, 1060, 736));
        assert_eq!(left.status, rect(0, 780, 1280, 20));
        assert_eq!(left.tabs[0].rect, rect(4, 48, 212, 32));
        assert_eq!(left.tabs[1].rect, rect(4, 84, 212, 32));
        assert_eq!(left.tabs[1].close, Some(rect(190, 90, 20, 20)));
        assert_eq!(left.new_tab, rect(4, 156, 212, 32));
        assert_eq!(left.rows, 20);
    }

    /// `[ui] font-size`: today's heights times font-size / 14, rounded (extension-api.md §1.8).
    #[test]
    fn heights_follow_the_font_size() {
        let heights = |options: LayoutOptions| {
            let layout = layout_of(size(1280, 800), &tabs(2), options, 0);
            (
                layout.tab_strip.height,
                layout.toolbar.height,
                layout.status.height,
            )
        };
        assert_eq!(heights(LayoutOptions::default()), (40, 44, 20));
        assert_eq!(heights(with(|o| o.font_size = 18)), (51, 57, 26));
        assert_eq!(heights(with(|o| o.compact = true)), (32, 36, 20));
        let compact_18 = with(|o| (o.compact, o.font_size) = (true, 18));
        assert_eq!(heights(compact_18), (41, 46, 26));
        assert_eq!(heights(with(|o| o.font_size = 9)), (26, 28, 13));
        let sizing = Arrangement::from(with(|o| o.font_size = 18)).sizing();
        let style = sizing.text(TextStyle::SM);
        assert_eq!((style.size, style.line_height), (18, 26));
    }

    #[test]
    fn odd_sizes_keep_everything_inside() {
        for options in every_option() {
            for (width, height) in ODD_SIZES {
                for (count, first_row) in
                    [(0, 0), (1, 0), (2, 3), (7, 0), (7, 5), (40, 0), (40, 30)]
                {
                    let layout = layout_of(size(width, height), &tabs(count), options, first_row);
                    let window = rect(0, 0, width, height);
                    let what = format!("{width}×{height}, {count} tabs, {options:?}");
                    // The surfaces are inside the window and don't overlap, except the status
                    // overlay, which lies over the page.
                    let surfaces = [layout.chrome, layout.tab_list, layout.page, layout.status];
                    for region in surfaces {
                        assert!(inside(region, window), "{what}: {region:?}");
                    }
                    let strip = match options.status {
                        Status::Strip => layout.status.height,
                        Status::Overlay => {
                            assert!(inside(layout.status, layout.page), "{what}");
                            0
                        }
                    };
                    let total = layout.chrome.height + layout.page.height + strip;
                    assert_eq!(
                        total, height,
                        "{what}: the regions cover the window's height"
                    );
                    assert_eq!(layout.page.y, layout.chrome.height as i32, "{what}");
                    assert_eq!(layout.tab_list.width + layout.page.width, width, "{what}");
                    assert!(intersect(layout.tab_list, layout.page).size().is_empty());
                    // Widgets stay inside their surface, and tabs never overlap each other or
                    // the new-tab button.
                    for (name, widget, parent) in widgets(&layout) {
                        assert!(
                            inside(widget, parent),
                            "{what}: {name} {widget:?} in {parent:?}"
                        );
                    }
                    for pair in layout.tabs.windows(2) {
                        let (a, b) = (pair[0].rect, pair[1].rect);
                        assert!(intersect(a, b).size().is_empty(), "{what}");
                    }
                    for tab in &layout.tabs {
                        assert!(
                            intersect(tab.rect, layout.new_tab).size().is_empty(),
                            "{what}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn many_tabs_shrink_then_overflow() {
        let layout = layout_of(size(800, 600), &tabs(8), LayoutOptions::default(), 0);
        let widths: Vec<u32> = layout.tabs.iter().map(|tab| tab.rect.width).collect();
        assert!(
            widths.iter().all(|&w| w == widths[0] && w < TAB_MAX_WIDTH),
            "{widths:?}"
        );
        let layout = layout_of(size(800, 600), &tabs(40), LayoutOptions::default(), 0);
        // The new-tab button stays reachable at the right end.
        let button = layout.new_tab;
        assert_eq!(button.x + button.width as i32, 800 - GAP as i32);
        assert!(layout.tabs.last().unwrap().rect.size().is_empty());
        assert_eq!(layout.tabs[0].rect.width, TAB_MIN_WIDTH);
    }

    /// The list on the left shows whole rows from its first one, as far as there are rows.
    #[test]
    fn the_tab_list_scrolls_by_whole_rows() {
        let left = with(|o| o.tabs = Tabs::Left);
        // 40 tabs and the new-tab row in a list with room for 15 rows (4 + 15 × 36 ≤ 556).
        let window = size(800, 44 + 556 + 20);
        let ids = tabs(40);
        let layout = layout_of(window, &ids, left, 0);
        assert_eq!(layout.rows, 15);
        let shown = |layout: &Layout| -> Vec<usize> {
            let tabs = layout.tabs.iter().enumerate();
            tabs.filter(|(_, tab)| !tab.rect.size().is_empty())
                .map(|(i, _)| i)
                .collect()
        };
        assert_eq!(shown(&layout), (0..15).collect::<Vec<_>>());
        assert!(layout.new_tab.size().is_empty());
        let layout = layout_of(window, &ids, left, 10);
        assert_eq!(shown(&layout), (10..25).collect::<Vec<_>>());
        assert_eq!(
            layout.tabs[10].rect.y, 48,
            "the first row shown is at the top"
        );
        // Past the end, the last rows show, the new-tab button last.
        let layout = layout_of(window, &ids, left, 100);
        assert_eq!(layout.first_row, 26);
        assert_eq!(shown(&layout), (26..40).collect::<Vec<_>>());
        assert_eq!(layout.new_tab.y, 48 + 14 * 36);
        // A short list doesn't scroll.
        let layout = layout_of(window, &ids[..3], left, 5);
        assert_eq!(layout.first_row, 0);
    }

    #[test]
    fn short_windows_drop_the_status_strip_first() {
        let options = LayoutOptions::default();
        let layout = layout_of(size(400, 100), &tabs(1), options, 0);
        assert_eq!(layout.page.height, 16);
        assert!(layout.status.size().is_empty());
        let layout = layout_of(size(400, 60), &tabs(1), options, 0);
        assert_eq!(layout.chrome.height, 60);
        assert!(layout.page.size().is_empty());
    }
}
