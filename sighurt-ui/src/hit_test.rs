//! Which chrome element is under a point: maps window-logical positions to the layout's hit
//! targets (tabs at the top or in the list, their close buttons, the new-tab and toolbar buttons,
//! the address field).
//!
//! The chrome converts the host's surface-local position into window coordinates once, with
//! the placement of the surface the event arrived on, and the painter draws every control at
//! the same layout rectangle snapped to the buffer's pixels, so hit targets stay on what is
//! drawn at every scale.
//!
//! Must not own: deciding what a hit does (see `chrome`).

use sighurt_api::geometry::WindowPoint;
use sighurt_api::ids::TabId;

use crate::layout::Layout;

/// Something in the chrome that reacts to the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    /// A tab, to select it.
    Tab(TabId),
    /// A tab's close button.
    CloseTab(TabId),
    /// The new-tab button.
    NewTab,
    /// The back button.
    Back,
    /// The forward button.
    Forward,
    /// The reload button, or the stop button while the page loads.
    ReloadOrStop,
    /// The address field.
    Address,
}

/// The target at `point`, if any. A close button wins over the tab it sits on.
pub fn hit_test(layout: &Layout, point: WindowPoint) -> Option<Target> {
    for tab in &layout.tabs {
        if tab.close.is_some_and(|close| close.contains(point)) {
            return Some(Target::CloseTab(tab.id));
        }
        if tab.rect.contains(point) {
            return Some(Target::Tab(tab.id));
        }
    }
    [
        (layout.new_tab, Target::NewTab),
        (layout.back, Target::Back),
        (layout.forward, Target::Forward),
        (layout.reload, Target::ReloadOrStop),
        (layout.address, Target::Address),
    ]
    .into_iter()
    .find(|(rect, _)| rect.contains(point))
    .map(|(_, target)| target)
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::LogicalSize;

    use super::*;
    use crate::layout::LayoutOptions;

    fn point(x: f64, y: f64) -> WindowPoint {
        WindowPoint { x, y }
    }

    #[test]
    fn targets() {
        let ids = [TabId::next(), TabId::next()];
        let size = LogicalSize {
            width: 800,
            height: 600,
        };
        let layout = Layout::new(size, &ids, LayoutOptions::default(), 0);
        let close = layout.tabs[1].close.unwrap();
        let center = |r: sighurt_api::geometry::WindowRect| {
            point(
                f64::from(r.x) + f64::from(r.width) / 2.0,
                f64::from(r.y) + f64::from(r.height) / 2.0,
            )
        };
        assert_eq!(
            hit_test(&layout, point(10.0, 20.0)),
            Some(Target::Tab(ids[0]))
        );
        assert_eq!(
            hit_test(&layout, center(close)),
            Some(Target::CloseTab(ids[1]))
        );
        assert_eq!(hit_test(&layout, center(layout.back)), Some(Target::Back));
        assert_eq!(
            hit_test(&layout, center(layout.reload)),
            Some(Target::ReloadOrStop)
        );
        assert_eq!(hit_test(&layout, point(500.0, 60.0)), Some(Target::Address));
        // The gaps between controls, the strip's edge and the page hit nothing.
        assert_eq!(hit_test(&layout, point(2.0, 20.0)), None);
        assert_eq!(hit_test(&layout, point(500.0, 83.5)), None);
        assert_eq!(hit_test(&layout, point(500.0, 300.0)), None);
    }
}
