//! Carrying out browser commands (`sighurt_api::commands::Command`), with the semantics they
//! always had:
//!
//! - Tabs count from 1 in tab-strip order; a number without a tab does nothing.
//! - `tab.new` opens the home page in a new tab at the end, selects it and focuses the address
//!   bar. The last tab can't be closed; closing the active tab selects the one after it, or the
//!   one before if it was the last.
//! - `tab.next` and `tab.previous` wrap around.
//! - Page zoom walks [`ZOOM_STEPS`] and stops at its ends; each tab keeps its own, across
//!   engines. New tabs start at the configured `zoom`, and `page.zoom-reset` returns to it.
//! - `page.open-in <engine> [address]` opens the address, or the active tab's own address
//!   again, in the engine named, as a new navigation of the tab (the old page is a segment back
//!   in its history); the view it opens is chosen (`View::chosen`). An engine the build lacks,
//!   or one not enabled, and `sig:` pages are the tab's error, as when typed.
//! - `page.switch-engine` does that for the next web engine ([`Environment::web_engines`],
//!   the ones compiled in and enabled), wrapping around. Fewer than two, or a page that is
//!   not http(s) or a file, shows a notice and leaves the page.
//! - `browser.home` opens the home page in the active tab.
//! - `location.focus` is the chrome's: the core moves the focus and says so with
//!   `Effect::Focus`. Commands that change what the page area shows give the page the keyboard.
//! - `page.send-key` presses and releases its key in the page only while the page has the
//!   keyboard; run by a key press, that counts as the user's key press in the page.
//! - `chrome.<action>` is the chrome's own: the core hands it on (`Effect::ChromeCommand`).
//! - A tab the chrome selects or closes by its identity gets the command for its number now.
//!
//! Key presses become commands in [`crate::keys`].

use std::num::NonZeroUsize;

use sighurt_api::commands::Command;
use sighurt_api::ids::TabId;
use sighurt_api::input::KeyboardFocus;

use crate::navigation::Segment;
use crate::state::{BrowserState, By, Effect, Fx};

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
                Some(KeyboardFocus::Chrome)
            }
            Command::TabClose(n) => {
                if let Some(index) = n.map_or(Some(active), tab_number) {
                    self.close_tab(index, fx);
                }
                Some(KeyboardFocus::Page)
            }
            Command::TabNext => self.select((active + 1) % count, fx),
            Command::TabPrevious => self.select((active + count - 1) % count, fx),
            Command::TabSelect(n) => self.select(tab_number(n).unwrap_or(active), fx),
            Command::TabLast => self.select(count - 1, fx),
            Command::PageOpen(input) => {
                let url = self.fixup(&input);
                if !url.is_empty() {
                    self.navigate(active, url, By::User(None), fx);
                }
                Some(KeyboardFocus::Page)
            }
            Command::PageOpenIn { engine, address } => {
                let url = match address {
                    Some(address) => Some(self.fixup(&address)),
                    None => self.address(active),
                };
                if let Some(url) = url.filter(|url| !url.is_empty()) {
                    self.navigate(active, url, By::User(Some(&engine)), fx);
                }
                Some(KeyboardFocus::Page)
            }
            Command::PageSwitchEngine => self.switch_engine(active, fx),
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
                Some(KeyboardFocus::Page)
            }
            Command::PageZoomIn | Command::PageZoomOut | Command::PageZoomReset => {
                let tab = &mut self.tabs[active];
                tab.zoom = match command {
                    Command::PageZoomIn => zoom_step(tab.zoom, true),
                    Command::PageZoomOut => zoom_step(tab.zoom, false),
                    _ => self.config.zoom(),
                };
                fx.chrome = true;
                fx.session = true;
                None
            }
            Command::PageSendKey(key) => {
                self.send_key(&key, fx);
                None
            }
            Command::LocationFocus => Some(KeyboardFocus::Chrome),
            Command::BrowserHome => {
                self.open_home(active, fx);
                Some(KeyboardFocus::Page)
            }
            Command::BrowserQuit => {
                fx.quit = true;
                None
            }
            Command::Chrome(action) => {
                fx.push(Effect::ChromeCommand(action));
                None
            }
        };
        if let Some(focus) = focus {
            self.move_focus(focus, fx);
        }
    }

    /// Reopens the page in the next enabled web engine, in [`Environment::web_engines`] order,
    /// wrapping around. The navigation is `page.open-in` of that engine. Anything else is a
    /// notice, and the page stays.
    fn switch_engine(&mut self, tab: usize, fx: &mut Fx) -> Option<KeyboardFocus> {
        let url = self.address(tab).filter(|url| !url.is_empty());
        let Some(url) = url else {
            return self.switch_notice(tab, "This tab has no page to open in another engine.", fx);
        };
        let web = url::Url::parse(&url)
            .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https" | "file"));
        if !web {
            return self.switch_notice(
                tab,
                "page.switch-engine opens http, https and file pages. This page stays where it is.",
                fx,
            );
        }
        let enabled: Vec<String> = self
            .web_engines()
            .iter()
            .filter(|name| self.has_engine(name) && self.config.is_enabled(name))
            .cloned()
            .collect();
        if enabled.len() < 2 {
            let message = match enabled.as_slice() {
                [only] => format!(
                    "Only the {only} engine is enabled, so there is no other web engine to open \
                     this page in."
                ),
                _ => "No web engine is enabled, so this page stays where it is.".to_owned(),
            };
            return self.switch_notice(tab, &message, fx);
        }
        let next = match self
            .shown_engine(tab)
            .as_deref()
            .and_then(|name| enabled.iter().position(|engine| engine == name))
        {
            Some(index) => enabled[(index + 1) % enabled.len()].clone(),
            None => enabled[0].clone(),
        };
        self.navigate(tab, url, By::User(Some(&next)), fx);
        Some(KeyboardFocus::Page)
    }

    /// The engine of the page tab `tab` shows. A page still loading its first view, and a
    /// record of one, still have an engine; a replacement that has not committed does not
    /// count, because the page it replaces is what is shown.
    fn shown_engine(&self, tab: usize) -> Option<String> {
        let tab = &self.tabs[tab];
        if let Some(id) = tab.current_view() {
            return self.view(id).map(|view| view.engine().to_owned());
        }
        if let Some(Segment::Record(record)) = tab.current() {
            return Some(record.engine.clone());
        }
        tab.switch
            .and_then(|switch| self.view(switch.view).map(|view| view.engine().to_owned()))
    }

    /// Says `message` about tab `tab` and leaves the keyboard where it is.
    fn switch_notice(&mut self, tab: usize, message: &str, fx: &mut Fx) -> Option<KeyboardFocus> {
        fx.push(Effect::Notice {
            tab: Some(self.tabs[tab].id),
            message: message.to_owned(),
        });
        None
    }

    /// Runs the command `command` makes of tab `tab`'s number, as a tab clicked in the chrome
    /// asks: by its identity, so a tab list that changed since the chrome drew it can't make the
    /// command act on another tab. A tab that is gone does nothing.
    pub(crate) fn tab_command(
        &mut self,
        tab: TabId,
        command: impl FnOnce(NonZeroUsize) -> Command,
        fx: &mut Fx,
    ) {
        if let Some(n) = self.tab_index(tab).and_then(|i| NonZeroUsize::new(i + 1)) {
            self.run_command(command(n), fx);
        }
    }

    fn select(&mut self, index: usize, fx: &mut Fx) -> Option<KeyboardFocus> {
        self.active = index;
        fx.chrome = true;
        fx.session = true;
        Some(KeyboardFocus::Page)
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
