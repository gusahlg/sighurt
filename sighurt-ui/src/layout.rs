//! Chrome layout: the tab strip, the toolbar with its address line, an optional status strip,
//! and the page rectangle, for the current window size.
//!
//! Explicit code, not a layout language, in window-logical coordinates. The result is both the
//! placement snapshot the host validates and applies (the chrome, status and page rectangles)
//! and the widget rectangles the hit test and painter share. A region with no room gets an
//! empty rectangle, which the host unmaps; that is not how background tabs are hidden (they
//! keep the page rectangle and are hidden through visibility). Every widget rectangle lies
//! inside its strip, so nothing is hit that isn't drawn.
//!
//! Top to bottom: the tab strip (40 px), the toolbar (44 px) with back, forward, reload-or-stop
//! and the address line, the page, and the status strip (20 px) if enabled and there is room.
//!
//! Must not own: applying placement to surfaces (the host).

use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::TabId;

/// Height of the tab strip.
pub const TAB_STRIP_HEIGHT: u32 = 40;
/// Height of the toolbar.
pub const TOOLBAR_HEIGHT: u32 = 44;
/// Height of the status strip.
pub const STATUS_HEIGHT: u32 = 20;

/// Space around the tab strip's contents and between tabs.
const STRIP_GAP: u32 = 4;
/// Space around the toolbar's contents.
const TOOLBAR_PADDING: u32 = 8;
/// Tabs share the strip's width equally, between these widths.
const TAB_MIN_WIDTH: u32 = 64;
const TAB_MAX_WIDTH: u32 = 220;
const TAB_HEIGHT: u32 = 32;
/// The square toolbar buttons and the new-tab button.
const BUTTON: u32 = 28;
/// The close button inside a tab, and its distance from the tab's right edge.
const CLOSE_BUTTON: u32 = 20;
const CLOSE_INSET: u32 = 6;
const ADDRESS_HEIGHT: u32 = 32;

/// What the layout may include.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutOptions {
    /// Show a status strip below the page, for hovered links and downloads. It is its own
    /// surface, separate from the chrome at the top.
    pub status_strip: bool,
}

impl Default for LayoutOptions {
    fn default() -> Self {
        Self { status_strip: true }
    }
}

/// Where everything goes, in window-logical coordinates.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    /// The window's size.
    pub window: LogicalSize,
    /// The chrome surface at the top: the tab strip and the toolbar.
    pub chrome: WindowRect,
    /// The tab strip.
    pub tab_strip: WindowRect,
    /// One entry per tab, in tab-strip order. Tabs that don't fit have empty rectangles.
    pub tabs: Vec<TabLayout>,
    /// The new-tab button.
    pub new_tab: WindowRect,
    /// The toolbar below the tab strip.
    pub toolbar: WindowRect,
    /// The back button.
    pub back: WindowRect,
    /// The forward button.
    pub forward: WindowRect,
    /// The reload button, which is a stop button while the page loads.
    pub reload: WindowRect,
    /// The address field, including its border.
    pub address: WindowRect,
    /// The page: the active view's target, or where the chrome draws its own pages.
    pub page: WindowRect,
    /// The status strip surface at the bottom; empty when disabled or without room.
    pub status: WindowRect,
}

/// One tab's place in the strip.
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
    /// The layout for a window of `window` size showing `tabs`, in tab-strip order.
    pub fn new(window: LogicalSize, tabs: &[TabId], options: LayoutOptions) -> Self {
        let (width, height) = (window.width, window.height);
        let tab_strip = rect(0, 0, width, TAB_STRIP_HEIGHT.min(height));
        let toolbar_height = TOOLBAR_HEIGHT.min(height - tab_strip.height);
        let toolbar = rect(0, TAB_STRIP_HEIGHT, width, toolbar_height);
        let chrome = rect(0, 0, width, tab_strip.height + toolbar_height);

        let rest = height - chrome.height;
        let status_height = match options.status_strip && rest > STATUS_HEIGHT {
            true => STATUS_HEIGHT,
            false => 0,
        };
        let page = rect(0, chrome.height, width, rest - status_height);
        let status = rect(0, height - status_height, width, status_height);

        // Tabs share what the new-tab button leaves, equally; the button follows the last tab
        // and stays in the strip when they overflow, and tabs never run under it.
        let content_end = width.saturating_sub(STRIP_GAP);
        let count = u32::try_from(tabs.len()).unwrap_or(u32::MAX);
        let room = content_end.saturating_sub(STRIP_GAP + STRIP_GAP + BUTTON);
        let tab_width = match count {
            0 => 0,
            n => (room.saturating_sub(STRIP_GAP * (n - 1)) / n).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH),
        };
        let tabs_end = STRIP_GAP.saturating_add(count.saturating_mul(tab_width + STRIP_GAP));
        let new_tab_x = tabs_end
            .min(content_end.saturating_sub(BUTTON))
            .max(STRIP_GAP);
        let tab_area = intersect(
            rect(0, 0, new_tab_x.saturating_sub(STRIP_GAP), TAB_STRIP_HEIGHT),
            tab_strip,
        );
        let tab_top = (TAB_STRIP_HEIGHT - TAB_HEIGHT) / 2;
        let tabs = tabs
            .iter()
            .zip(0u32..)
            .map(|(&id, i)| {
                let x = STRIP_GAP.saturating_add(i.saturating_mul(tab_width + STRIP_GAP));
                let full = rect(x, tab_top, tab_width, TAB_HEIGHT);
                let tab = intersect(full, tab_area);
                let close = rect(
                    (x + tab_width).saturating_sub(CLOSE_INSET + CLOSE_BUTTON),
                    tab_top + (TAB_HEIGHT - CLOSE_BUTTON) / 2,
                    CLOSE_BUTTON,
                    CLOSE_BUTTON,
                );
                TabLayout {
                    id,
                    rect: tab,
                    close: (count > 1).then(|| intersect(close, tab)),
                }
            })
            .collect();
        let new_tab = intersect(
            rect(new_tab_x, (TAB_STRIP_HEIGHT - BUTTON) / 2, BUTTON, BUTTON),
            tab_strip,
        );

        let button_y = TAB_STRIP_HEIGHT + (TOOLBAR_HEIGHT - BUTTON) / 2;
        let button = |i: u32| {
            let x = TOOLBAR_PADDING + i * (BUTTON + STRIP_GAP);
            intersect(rect(x, button_y, BUTTON, BUTTON), toolbar)
        };
        let address_x = TOOLBAR_PADDING + 3 * (BUTTON + STRIP_GAP);
        let address = rect(
            address_x,
            TAB_STRIP_HEIGHT + (TOOLBAR_HEIGHT - ADDRESS_HEIGHT) / 2,
            width.saturating_sub(address_x + TOOLBAR_PADDING),
            ADDRESS_HEIGHT,
        );

        Self {
            window,
            chrome,
            tab_strip,
            tabs,
            new_tab,
            toolbar,
            back: button(0),
            forward: button(1),
            reload: button(2),
            address: intersect(address, toolbar),
            page,
            status,
        }
    }

    /// Where a tab is, if the layout has it.
    pub fn tab(&self, id: TabId) -> Option<&TabLayout> {
        self.tabs.iter().find(|tab| tab.id == id)
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

    /// Every widget of `layout` with its name.
    fn widgets(layout: &Layout) -> Vec<(String, WindowRect, WindowRect)> {
        let mut widgets = vec![
            ("new tab".into(), layout.new_tab, layout.tab_strip),
            ("back".into(), layout.back, layout.toolbar),
            ("forward".into(), layout.forward, layout.toolbar),
            ("reload".into(), layout.reload, layout.toolbar),
            ("address".into(), layout.address, layout.toolbar),
        ];
        for (i, tab) in layout.tabs.iter().enumerate() {
            widgets.push((format!("tab {i}"), tab.rect, layout.tab_strip));
            if let Some(close) = tab.close {
                widgets.push((format!("close {i}"), close, tab.rect));
            }
        }
        widgets
    }

    #[test]
    fn regions_tile_the_window() {
        let layout = Layout::new(size(1280, 800), &tabs(3), LayoutOptions::default());
        assert_eq!(layout.chrome, rect(0, 0, 1280, 84));
        assert_eq!(layout.page, rect(0, 84, 1280, 696));
        assert_eq!(layout.status, rect(0, 780, 1280, 20));
        assert_eq!(layout.address, rect(104, 46, 1168, 32));
        assert_eq!(layout.tabs[0].rect, rect(4, 4, 220, 32));
        assert_eq!(layout.tabs[1].rect.x, 228);
        assert_eq!(layout.new_tab.x, 4 + 3 * 224);

        let layout = Layout::new(
            size(1280, 800),
            &tabs(1),
            LayoutOptions {
                status_strip: false,
            },
        );
        assert_eq!(layout.page, rect(0, 84, 1280, 716));
        assert!(layout.status.size().is_empty());
        // The last tab can't close, so it has no close button.
        assert_eq!(layout.tabs[0].close, None);
    }

    #[test]
    fn odd_sizes_keep_everything_inside() {
        let options = LayoutOptions::default();
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
                let layout = Layout::new(size(width, height), &tabs(count), options);
                let window = rect(0, 0, width, height);
                let what = format!("{width}×{height}, {count} tabs");
                // The surfaces are inside the window and don't overlap.
                for region in [layout.chrome, layout.page, layout.status] {
                    assert!(inside(region, window), "{what}: {region:?}");
                }
                let total = layout.chrome.height + layout.page.height + layout.status.height;
                assert_eq!(
                    total, height,
                    "{what}: the regions cover the window's height"
                );
                assert_eq!(layout.page.y, layout.chrome.height as i32, "{what}");
                // Widgets stay inside their strip, and tabs never overlap each other or the
                // new-tab button.
                for (name, widget, parent) in widgets(&layout) {
                    assert!(
                        inside(widget, parent),
                        "{what}: {name} {widget:?} in {parent:?}"
                    );
                    assert!(inside(widget, layout.chrome), "{what}: {name}");
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

    #[test]
    fn many_tabs_shrink_then_overflow() {
        let layout = Layout::new(size(800, 600), &tabs(8), LayoutOptions::default());
        let widths: Vec<u32> = layout.tabs.iter().map(|tab| tab.rect.width).collect();
        assert!(
            widths.iter().all(|&w| w == widths[0] && w < TAB_MAX_WIDTH),
            "{widths:?}"
        );
        let layout = Layout::new(size(800, 600), &tabs(40), LayoutOptions::default());
        // The new-tab button stays reachable at the right end.
        let button = layout.new_tab;
        assert_eq!(button.x + button.width as i32, 800 - STRIP_GAP as i32);
        assert!(layout.tabs.last().unwrap().rect.size().is_empty());
        assert_eq!(layout.tabs[0].rect.width, TAB_MIN_WIDTH);
    }

    #[test]
    fn short_windows_drop_the_status_strip_first() {
        let options = LayoutOptions::default();
        let layout = Layout::new(size(400, 100), &tabs(1), options);
        assert_eq!(layout.page.height, 16);
        assert!(layout.status.size().is_empty());
        let layout = Layout::new(size(400, 60), &tabs(1), options);
        assert_eq!(layout.chrome.height, 60);
        assert!(layout.page.size().is_empty());
    }
}
