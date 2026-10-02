//! Chrome layout: the tabs, the toolbar with its address line, the status line and the page
//! rectangle, for the current window size and the standard chrome's options.
//!
//! Explicit code, not a layout language, in window-logical coordinates. The result is both
//! where the standard chrome's surfaces go (the chrome at the top, the tab list, the status line
//! and the page) and the widget rectangles the hit test and painter share. A region with no
//! room gets an empty rectangle, which the host unmaps; that is not how background tabs are
//! hidden (they keep the page rectangle and are hidden through visibility). Every widget
//! rectangle lies inside its surface, so nothing is hit that isn't drawn.
//!
//! With the default options, top to bottom: the tab strip (40 px), the toolbar (44 px) with
//! back, forward, reload-or-stop and the address line, the page, and the status strip (20 px)
//! if there is room. [`LayoutOptions`] (`[chrome.standard]`) can put the tabs in a list left of
//! the page, below the toolbar, or nowhere; put the status line over the page's bottom-left
//! corner instead of in a strip; leave out the buttons; or make the tab strip and toolbar lower
//! (32 and 36 px). Every size scales with `[ui] font-size`: times font-size / 14, rounded to
//! whole logical pixels. Nothing is measured.
//!
//! Must not own: applying placement to surfaces (the host), or which surfaces are listed (the
//! standard chrome).

use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::TabId;

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
/// Tabs at the top share the strip's width equally, between these widths.
const TAB_MIN_WIDTH: u32 = 64;
const TAB_MAX_WIDTH: u32 = 220;
/// The tab list's width on the left, at most half the window's.
const LIST_WIDTH: u32 = 220;
/// The square toolbar buttons and the new-tab button at the top.
const BUTTON: u32 = 28;
/// The close button inside a tab, and its distance from the tab's right edge.
const CLOSE_BUTTON: u32 = 20;
const CLOSE_INSET: u32 = 6;
/// The status overlay is half as wide as the page, but at least this wide.
const OVERLAY_MIN_WIDTH: u32 = 200;

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

/// What the layout includes, and how big it is.
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
            font_size: DEFAULT_FONT_SIZE,
        }
    }
}

impl LayoutOptions {
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
}

/// Where everything goes, in window-logical coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The window's size.
    pub window: LogicalSize,
    /// What it was laid out with.
    pub options: LayoutOptions,
    /// The chrome surface at the top: the tab strip, if the tabs are at the top, and the
    /// toolbar.
    pub chrome: WindowRect,
    /// The tab strip; empty unless the tabs are at the top.
    pub tab_strip: WindowRect,
    /// The tab list's surface; empty unless the tabs are on the left.
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
    /// The back button; empty without buttons.
    pub back: WindowRect,
    /// The forward button; empty without buttons.
    pub forward: WindowRect,
    /// The reload button, which is a stop button while the page loads; empty without buttons.
    pub reload: WindowRect,
    /// The address field, including its border.
    pub address: WindowRect,
    /// The page: the active view's target, or where the chrome draws its own pages.
    pub page: WindowRect,
    /// The status line's surface: the strip at the bottom (empty when there is no room for
    /// it), or the overlay over the page's bottom-left corner, as tall as the strip and half as
    /// wide as the page, but at least 200 px and at most the page's width.
    pub status: WindowRect,
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

impl Layout {
    /// The layout for a window of `window` size showing `tabs`, in tab order, with the tab
    /// list's rows from `first_row` on (as far as there are rows to show).
    pub fn new(
        window: LogicalSize,
        tabs: &[TabId],
        options: LayoutOptions,
        first_row: usize,
    ) -> Self {
        let px = |size| options.px(size);
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
        // A tab's close button: at its right end, centred.
        let close_of = |tab: WindowRect, full: WindowRect| {
            let right = u32::try_from(full.x).unwrap_or(0) + full.width;
            let y = u32::try_from(full.y).unwrap_or(0) + full.height.saturating_sub(close) / 2;
            let x = right.saturating_sub(px(CLOSE_INSET) + close);
            closable.then(|| intersect(rect(x, y, close, close), tab))
        };
        let (tabs, new_tab, first_row, rows) = match options.tabs {
            Tabs::Top => {
                // Tabs share what the new-tab button leaves, equally; the button follows the
                // last tab and stays in the strip when they overflow, and tabs never run under
                // it.
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
                // Rows of the list's width, one per tab, then the new-tab button; only whole
                // rows show.
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

        Self {
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

    /// Where a tab is, if the layout has it.
    pub fn tab(&self, id: TabId) -> Option<&TabLayout> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// A row of the tab list: the tabs, then the new-tab button.
    pub(crate) fn row_pitch(&self) -> u32 {
        let options = self.options;
        options.px(TAB_HEIGHT[usize::from(options.compact)]) + options.px(GAP)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(n: usize) -> Vec<TabId> {
        (0..n).map(|_| TabId::next()).collect()
    }

    fn size(width: u32, height: u32) -> LogicalSize {
        LogicalSize { width, height }
    }

    fn inside(inner: WindowRect, outer: WindowRect) -> bool {
        inner.size().is_empty() || intersect(inner, outer) == inner
    }

    fn with(change: impl FnOnce(&mut LayoutOptions)) -> LayoutOptions {
        let mut options = LayoutOptions::default();
        change(&mut options);
        options
    }

    /// Every combination of the options, at two font sizes.
    fn every_option() -> Vec<LayoutOptions> {
        let mut all = Vec::new();
        for tabs in [Tabs::Top, Tabs::Left, Tabs::Hidden] {
            for status in [Status::Strip, Status::Overlay] {
                for (buttons, compact) in [(true, false), (false, true)] {
                    for font_size in [14, 18] {
                        all.push(LayoutOptions {
                            tabs,
                            status,
                            buttons,
                            compact,
                            font_size,
                        });
                    }
                }
            }
        }
        all
    }

    /// Every widget of `layout` with its name and the region it must stay in.
    fn widgets(layout: &Layout) -> Vec<(String, WindowRect, WindowRect)> {
        let tab_area = match layout.options.tabs {
            Tabs::Left => layout.tab_list,
            _ => layout.tab_strip,
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
        let layout = Layout::new(size(1280, 800), &tabs(3), LayoutOptions::default(), 0);
        assert_eq!(layout.chrome, rect(0, 0, 1280, 84));
        assert_eq!(layout.page, rect(0, 84, 1280, 696));
        assert_eq!(layout.status, rect(0, 780, 1280, 20));
        assert_eq!(layout.address, rect(104, 46, 1168, 32));
        assert_eq!(layout.tabs[0].rect, rect(4, 4, 220, 32));
        assert_eq!(layout.tabs[1].rect.x, 228);
        assert_eq!(layout.new_tab.x, 4 + 3 * 224);
        assert!(layout.tab_list.size().is_empty());

        // The last tab can't close, so it has no close button.
        let layout = Layout::new(size(1280, 800), &tabs(1), LayoutOptions::default(), 0);
        assert_eq!(layout.tabs[0].close, None);
    }

    /// `[chrome.standard]`: each option changes only what it says.
    #[test]
    fn each_option_moves_only_its_part() {
        let window = size(1280, 800);
        let ids = tabs(3);
        let layout = |options| Layout::new(window, &ids, options, 0);

        // The status line over the page: the page keeps the strip's room, and the overlay is
        // half its width at its bottom-left corner.
        let overlay = layout(with(|o| o.status = Status::Overlay));
        assert_eq!(overlay.page, rect(0, 84, 1280, 716));
        assert_eq!(overlay.status, rect(0, 780, 640, 20));
        // At least 200 px, at most the page.
        let narrow = Layout::new(
            size(300, 800),
            &ids,
            with(|o| o.status = Status::Overlay),
            0,
        );
        assert_eq!(narrow.status, rect(0, 780, 200, 20));
        let tiny = Layout::new(
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
            let layout = Layout::new(size(1280, 800), &tabs(2), options, 0);
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
        let style = with(|o| o.font_size = 18).text(TextStyle::SM);
        assert_eq!((style.size, style.line_height), (18, 26));
    }

    #[test]
    fn odd_sizes_keep_everything_inside() {
        for options in every_option() {
            for (width, height) in [
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
            ] {
                for count in [0, 1, 2, 7, 40] {
                    let layout = Layout::new(size(width, height), &tabs(count), options, 0);
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
        let layout = Layout::new(size(800, 600), &tabs(8), LayoutOptions::default(), 0);
        let widths: Vec<u32> = layout.tabs.iter().map(|tab| tab.rect.width).collect();
        assert!(
            widths.iter().all(|&w| w == widths[0] && w < TAB_MAX_WIDTH),
            "{widths:?}"
        );
        let layout = Layout::new(size(800, 600), &tabs(40), LayoutOptions::default(), 0);
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
        let layout = Layout::new(window, &ids, left, 0);
        assert_eq!(layout.rows, 15);
        let shown = |layout: &Layout| -> Vec<usize> {
            let tabs = layout.tabs.iter().enumerate();
            tabs.filter(|(_, tab)| !tab.rect.size().is_empty())
                .map(|(i, _)| i)
                .collect()
        };
        assert_eq!(shown(&layout), (0..15).collect::<Vec<_>>());
        assert!(layout.new_tab.size().is_empty());
        let layout = Layout::new(window, &ids, left, 10);
        assert_eq!(shown(&layout), (10..25).collect::<Vec<_>>());
        assert_eq!(
            layout.tabs[10].rect.y, 48,
            "the first row shown is at the top"
        );
        // Past the end, the last rows show, the new-tab button last.
        let layout = Layout::new(window, &ids, left, 100);
        assert_eq!(layout.first_row, 26);
        assert_eq!(shown(&layout), (26..40).collect::<Vec<_>>());
        assert_eq!(layout.new_tab.y, 48 + 14 * 36);
        // A short list doesn't scroll.
        let layout = Layout::new(window, &ids[..3], left, 5);
        assert_eq!(layout.first_row, 0);
    }

    #[test]
    fn short_windows_drop_the_status_strip_first() {
        let options = LayoutOptions::default();
        let layout = Layout::new(size(400, 100), &tabs(1), options, 0);
        assert_eq!(layout.page.height, 16);
        assert!(layout.status.size().is_empty());
        let layout = Layout::new(size(400, 60), &tabs(1), options, 0);
        assert_eq!(layout.chrome.height, 60);
        assert!(layout.page.size().is_empty());
    }
}
