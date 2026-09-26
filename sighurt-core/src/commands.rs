//! Carrying out browser commands (`sighurt_api::commands::Command`), with the semantics they
//! always had:
//!
//! - Tabs count from 1 in tab-strip order; a number without a tab does nothing.
//! - `tab.new` opens the home page in a new tab at the end, selects it and focuses the address
//!   bar. The last tab can't be closed; closing the active tab selects the one after it, or the
//!   one before if it was the last.
//! - `tab.next` and `tab.previous` wrap around.
//! - Page zoom walks [`ZOOM_STEPS`] and stops at its ends; each tab keeps its own, across
//!   engines.
//! - `browser.home` opens the home page in the active tab.
//! - `location.focus` is the chrome's: the core moves the focus and says so with
//!   `Effect::Focus`. Commands that change what the page area shows give the page the keyboard.
//!
//! Key bindings are matched in [`crate::keys`].

use std::num::NonZeroUsize;

use sighurt_api::commands::Command;

use crate::state::{BrowserState, Focus, Fx};

/// Page zoom levels in percent, as in common browsers.
pub const ZOOM_STEPS: [u32; 13] = [50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// The zoom step after `zoom` (a factor), or before it; the ends stay put.
pub fn zoom_step(zoom: f64, bigger: bool) -> f64 {
    let percent = (zoom * 100.0).round();
    let step = if bigger {
        ZOOM_STEPS.iter().find(|&&s| f64::from(s) > percent)
    } else {
        ZOOM_STEPS.iter().rev().find(|&&s| f64::from(s) < percent)
    };
    step.map_or(zoom, |&s| f64::from(s) / 100.0)
}

/// `zoom` within the ladder's range, 1.0 if it isn't a number.
pub(crate) fn clamp_zoom(zoom: f64) -> f64 {
    let (min, max) = (ZOOM_STEPS[0], ZOOM_STEPS[ZOOM_STEPS.len() - 1]);
    if zoom.is_finite() {
        zoom.clamp(f64::from(min) / 100.0, f64::from(max) / 100.0)
    } else {
        1.0
    }
}

impl BrowserState {
    /// Carries out `command`. Before the browser has started there is nothing to act on.
    pub(crate) fn run_command(&mut self, command: Command, fx: &mut Fx) {
        if self.tabs.is_empty() {
            return;
        }
        let (active, count) = (self.active, self.tabs.len());
        let tab_number = |n: NonZeroUsize| (n.get() <= count).then(|| n.get() - 1);
        let focus = match command {
            Command::TabNew => {
                let tab = self.new_tab(false);
                self.active = tab;
                self.open_home(tab, fx);
                Some(Focus::Location)
            }
            Command::TabClose(n) => {
                if let Some(index) = n.map_or(Some(active), tab_number) {
                    self.close_tab(index, fx);
                }
                Some(Focus::Page)
            }
            Command::TabNext => self.select((active + 1) % count, fx),
            Command::TabPrevious => self.select((active + count - 1) % count, fx),
            Command::TabSelect(n) => self.select(tab_number(n).unwrap_or(active), fx),
            Command::TabLast => self.select(count - 1, fx),
            Command::PageOpen(input) => {
                let url = self.fixup(&input);
                if !url.is_empty() {
                    self.navigate(active, url, None, fx);
                }
                Some(Focus::Page)
            }
            Command::PageReload => {
                self.reload(active, fx);
                None
            }
            Command::PageStop => {
                self.stop(active, fx);
                None
            }
            Command::PageBack | Command::PageForward => {
                self.traverse(active, command == Command::PageBack, fx);
                Some(Focus::Page)
            }
            Command::PageZoomIn | Command::PageZoomOut | Command::PageZoomReset => {
                let tab = &mut self.tabs[active];
                tab.zoom = match command {
                    Command::PageZoomIn => zoom_step(tab.zoom, true),
                    Command::PageZoomOut => zoom_step(tab.zoom, false),
                    _ => 1.0,
                };
                fx.chrome = true;
                fx.session = true;
                None
            }
            Command::LocationFocus => Some(Focus::Location),
            Command::BrowserHome => {
                self.open_home(active, fx);
                Some(Focus::Page)
            }
            Command::BrowserQuit => {
                fx.quit = true;
                None
            }
        };
        if let Some(focus) = focus {
            self.move_focus(focus, fx);
        }
    }

    fn select(&mut self, index: usize, fx: &mut Fx) -> Option<Focus> {
        self.active = index;
        fx.chrome = true;
        fx.session = true;
        Some(Focus::Page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_walks_the_ladder() {
        assert_eq!(zoom_step(1.0, true), 1.1);
        assert_eq!(zoom_step(1.1, true), 1.25);
        assert_eq!(zoom_step(1.0, false), 0.9);
        assert_eq!(zoom_step(0.67, false), 0.5);
        // The ends stay put.
        assert_eq!(zoom_step(3.0, true), 3.0);
        assert_eq!(zoom_step(0.5, false), 0.5);
        // Off the ladder, the next step either way.
        assert_eq!(zoom_step(1.3, true), 1.5);
        assert_eq!(zoom_step(1.3, false), 1.25);
        assert_eq!(clamp_zoom(9.0), 3.0);
        assert_eq!(clamp_zoom(f64::NAN), 1.0);
    }
}
