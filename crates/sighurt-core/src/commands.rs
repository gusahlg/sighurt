//! Carrying out browser commands (`sighurt_api::commands::Command`), with the semantics they
//! always had:
//!
//! - Tabs count from 1 in tab-strip order; a number without a tab does nothing.
//! - `tab.new` opens the home page in a new tab at the end, selects it and focuses the address
//!   bar; `tab.new <address>` opens the address there as `page.open` would (keywords, search,
//!   routing) and gives the page the keyboard. With `session::MAX_TABS` tabs open, a new tab
//!   (`tab.new`, `tab.duplicate`, `tab.reopen`) is a notice instead. The last tab can't be
//!   closed; closing the active tab selects the one after it, or the one before if it was the
//!   last.
//! - `tab.duplicate` opens the selected tab's committed address in a new tab right after it, at
//!   its zoom, in its engine if the user chose that engine, and selects it; its history is not
//!   copied. A tab with no page duplicates as a home tab.
//! - `tab.move left|right` moves the selected tab one place, wrapping around; `tab.move <n>`
//!   moves it to position `n`, or the end. It stays selected and keeps the keyboard where it is.
//! - `tab.reopen` opens the address of the tab closed last in a new tab at the end, in the
//!   engine the user chose for it and at its zoom, and selects it: its history and the page's
//!   state are not restored. Only the last [`MAX_CLOSED`] closed since the browser started are
//!   kept, a page's own `window.close()` included, and a tab with no address is not. With
//!   none, it is a notice.
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
//! - `site.forget [host]` forgets that site's cookies and stored data in every engine that
//!   keeps any, and its visited pages (`Effect::ForgetVisits`). No argument is the page shown.
//!   A `sig:` page, a tab with no page, or a host that is not one, shows a notice.
//! - `clipboard.copy <text>` puts that text on the clipboard ([`Effect::ClipboardWrite`]).
//!   The runtime refuses text over its own limit and says so; the core does not cut it.
//! - `location.focus` is the chrome's: the core moves the focus and says so with
//!   `Effect::Focus`; `location.focus <text>` then hands the line its text with
//!   `Effect::LineText`. Commands that change what the page area shows give the page the keyboard.
//! - `page.find [text]`, `page.find-next`, `page.find-previous` and `page.find-stop`: no engine
//!   finds in page yet, so `page.find` says the page's engine can't, and the moves that there is
//!   no search. A `page.find` over [`MAX_FIND`] bytes, which only a chrome can make, is refused.
//! - `history.clear` forgets every visited page (`Effect::ForgetVisits`), with `[history]
//!   remember` on or off; the executable's answer is a notice. `browser.fullscreen` says the frontend has no fullscreen yet, and
//!   `page.exit-fullscreen` that the page is not in fullscreen.
//! - `page.send-key` presses and releases its key in the page only while the page has the
//!   keyboard; run by a key press, that counts as the user's key press in the page.
//! - `chrome.<action>` is the chrome's own: the core hands it on (`Effect::ChromeCommand`).
//! - A tab the chrome selects or closes by its identity gets the command for its number now.
//!
//! Key presses become commands in [`crate::keys`].

use std::num::NonZeroUsize;

use sighurt_api::commands::{Command, TabMove, MAX_FIND};
use sighurt_api::ids::TabId;
use sighurt_api::input::KeyboardFocus;

use crate::navigation::Segment;
use crate::state::{BrowserState, By, Effect, Fx};
use crate::tabs::MAX_CLOSED;

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
            Command::TabNew(address) => match self.new_tab(false, fx) {
                Some(tab) => {
                    self.active = tab;
                    let url = address.map(|address| self.fixup(&address));
                    match url.filter(|url| !url.is_empty()) {
                        Some(url) => {
                            self.navigate(tab, url, By::User(None), fx);
                            Some(KeyboardFocus::Page)
                        }
                        None => {
                            self.open_home(tab, fx);
                            Some(KeyboardFocus::Chrome)
                        }
                    }
                }
                None => None,
            },
            Command::TabDuplicate => self.duplicate(active, fx),
            Command::TabMove(to) => {
                let to = match to {
                    TabMove::Left => (active + count - 1) % count,
                    TabMove::Right => (active + 1) % count,
                    TabMove::To(n) => n.get().min(count) - 1,
                };
                if to != active {
                    let tab = self.tabs.remove(active);
                    self.tabs.insert(to, tab);
                    self.active = to;
                    fx.chrome = true;
                    fx.session = true;
                }
                None
            }
            Command::TabReopen => self.reopen(fx),
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
            Command::LocationFocus(text) => {
                self.move_focus(KeyboardFocus::Chrome, fx);
                if let Some(text) = text {
                    fx.push(Effect::LineText(text));
                }
                None
            }
            Command::PageFind(_)
            | Command::PageFindNext
            | Command::PageFindPrevious
            | Command::PageFindStop => {
                self.find(active, &command, fx);
                None
            }
            Command::HistoryClear => {
                self.forget_visits(None, fx);
                None
            }
            Command::PageExitFullscreen => {
                self.tab_notice(active, "The page shown is not in fullscreen.", fx);
                None
            }
            Command::BrowserFullscreen => {
                self.tab_notice(
                    active,
                    "browser.fullscreen is not available in this frontend.",
                    fx,
                );
                None
            }
            Command::BrowserHome => {
                self.open_home(active, fx);
                Some(KeyboardFocus::Page)
            }
            Command::BrowserQuit => {
                fx.quit = true;
                None
            }
            Command::SiteForget(host) => {
                self.site_forget(host, fx);
                None
            }
            Command::ClipboardCopy(text) => {
                fx.push(Effect::ClipboardWrite(text));
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

    /// Opens tab `tab`'s committed address in a new tab right after it, at its zoom and in the
    /// engine the user chose for it if they chose one, and selects it. Nothing committed: a home
    /// tab.
    fn duplicate(&mut self, tab: usize, fx: &mut Fx) -> Option<KeyboardFocus> {
        let url = self
            .tabs()
            .nth(tab)
            .and_then(|summary| summary.url.map(str::to_owned));
        let engine = self.chosen_engine(tab);
        let zoom = self.tabs[tab].zoom;
        let new = self.new_tab(false, fx)?;
        let mut duplicate = self.tabs.remove(new);
        duplicate.zoom = zoom;
        self.tabs.insert(tab + 1, duplicate);
        self.active = tab + 1;
        match url {
            Some(url) => self.navigate(tab + 1, url, By::User(engine.as_deref()), fx),
            None => self.open_home(tab + 1, fx),
        }
        fx.chrome = true;
        fx.session = true;
        Some(KeyboardFocus::Page)
    }

    /// Opens the address of the tab closed last in a new tab at the end, in the engine the
    /// user had chosen for it and at its zoom, and selects it. Its history is not restored.
    fn reopen(&mut self, fx: &mut Fx) -> Option<KeyboardFocus> {
        if self.closed.is_empty() {
            let message = format!(
                "No closed tab to reopen: tab.reopen brings back the last {MAX_CLOSED} tabs closed \
                 since sig started."
            );
            return self.tab_notice(self.active, &message, fx);
        }
        // A full tab strip says so and keeps the closed tab.
        let tab = self.new_tab(false, fx)?;
        let closed = self.closed.pop_back().expect("not empty");
        self.tabs[tab].zoom = closed.zoom;
        self.active = tab;
        self.navigate(tab, closed.url, By::User(closed.engine.as_deref()), fx);
        fx.session = true;
        Some(KeyboardFocus::Page)
    }

    /// `page.find` and its moves. No engine finds in page yet: `page.find` names the page's
    /// engine as unable, and the moves have no search to move in.
    fn find(&mut self, tab: usize, command: &Command, fx: &mut Fx) {
        let engine = self
            .shown_engine(tab)
            .filter(|_| self.tabs[tab].current_view().is_some());
        let message = match (command, engine) {
            (Command::PageFind(Some(text)), _) if text.len() > MAX_FIND => {
                format!("page.find searches for at most {MAX_FIND} bytes of text.")
            }
            (Command::PageFind(_), Some(engine)) => format!("{engine} can't find in page."),
            (Command::PageFind(_), None) => "This tab shows no page to find in.".to_owned(),
            (Command::PageFindNext | Command::PageFindPrevious, _) => {
                "Nothing to find: page.find <text> first.".to_owned()
            }
            _ => return,
        };
        self.tab_notice(tab, &message, fx);
    }

    /// Reopens the page in the next enabled web engine, in [`Environment::web_engines`] order,
    /// wrapping around. The navigation is `page.open-in` of that engine. Anything else is a
    /// notice, and the page stays. While the tab is already switching, nothing happens.
    fn switch_engine(&mut self, tab: usize, fx: &mut Fx) -> Option<KeyboardFocus> {
        // One switch at a time: a repeat while the view it opened is still loading does nothing,
        // rather than cancelling that view and starting over. A tab still loading its first
        // page shows nothing to switch from, and may change its mind.
        if self.tabs[tab].switch.is_some() && self.tabs[tab].current().is_some() {
            return None;
        }
        let url = self.address(tab).filter(|url| !url.is_empty());
        let Some(url) = url else {
            return self.tab_notice(tab, "This tab has no page to open in another engine.", fx);
        };
        let web = url::Url::parse(&url)
            .is_ok_and(|parsed| matches!(parsed.scheme(), "http" | "https" | "file"));
        if !web {
            return self.tab_notice(
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
            return self.tab_notice(tab, &message, fx);
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
    fn tab_notice(&mut self, tab: usize, message: &str, fx: &mut Fx) -> Option<KeyboardFocus> {
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
