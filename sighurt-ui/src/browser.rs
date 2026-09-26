//! The default UI's session: its tabs, each a [`Navigable`], and the commands that act on them.
//!
//! [`Browser`] is plain state plus [`Browser::run_command`], and the chrome draws it. What the
//! pages report lives in the navigables; the browser notifies whenever one of them changes, so
//! the chrome redraws when what it shows changes, and an engine painting a frame changes nothing
//! here.

use std::rc::Rc;

use gpui::{AppContext as _, Context, Entity};
use sighurt_core::config::Config;
use sighurt_core::navigable::{Navigable, NavigableEvent};
use sighurt_core::router;

use crate::commands;
use crate::settings::Settings;

/// Zoom levels in percent, as in common browsers.
const ZOOM_STEPS: [u32; 13] = [50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

pub struct Browser {
    config: Rc<Config>,
    settings: Settings,
    /// Never empty: the last tab can't be closed.
    tabs: Vec<Entity<Navigable>>,
    active: usize,
    /// The latest word from a download.
    status: Option<String>,
}

impl Browser {
    /// Opens a tab for each of `urls`, or for the home page if there are none.
    pub fn new(
        config: Config,
        settings: Settings,
        urls: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut browser = Self {
            config: Rc::new(config),
            settings,
            tabs: Vec::new(),
            active: 0,
            status: None,
        };
        let urls = if urls.is_empty() {
            vec![browser.settings.home.clone()]
        } else {
            urls
        };
        for url in urls {
            browser.open_tab(url, cx);
        }
        browser
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn tabs(&self) -> &[Entity<Navigable>] {
        &self.tabs
    }

    /// Index of the active tab in [`Self::tabs`].
    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> &Entity<Navigable> {
        &self.tabs[self.active]
    }

    /// A short line about the latest download, for the status bar.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Runs a command such as `tab.new` or `page.open https://example.com` (see
    /// [`commands::COMMANDS`]). `location.focus` is the root view's to handle.
    pub fn run_command(&mut self, command: &str, cx: &mut Context<Self>) {
        let (name, arg) = commands::split(command);
        let (active, count) = (self.active, self.tabs.len());
        let tab = self.tabs[active].clone();
        match name {
            "tab.new" => self.active = self.open_tab(self.settings.home.clone(), cx),
            "tab.close" => {
                let idx = match arg {
                    "" => Some(active),
                    n => self.tab_number(n),
                };
                if let Some(idx) = idx {
                    self.close_tab(idx);
                }
            }
            "tab.next" => self.active = (active + 1) % count,
            "tab.previous" => self.active = (active + count - 1) % count,
            "tab.select" => self.active = self.tab_number(arg).unwrap_or(active),
            "tab.last" => self.active = count - 1,
            "page.open" => self.open_input(arg, cx),
            "page.reload" => tab.update(cx, |tab, cx| tab.reload(cx)),
            "page.stop" => tab.update(cx, |tab, cx| tab.stop(cx)),
            "page.back" => tab.update(cx, |tab, cx| tab.traverse(false, cx)),
            "page.forward" => tab.update(cx, |tab, cx| tab.traverse(true, cx)),
            "page.zoom-in" | "page.zoom-out" | "page.zoom-reset" => tab.update(cx, |tab, cx| {
                let zoom = match name {
                    "page.zoom-in" => zoom_step(tab.zoom(), true),
                    "page.zoom-out" => zoom_step(tab.zoom(), false),
                    _ => 1.0,
                };
                tab.set_zoom(zoom, cx);
            }),
            "browser.home" => self.open_input(&self.settings.home.clone(), cx),
            "browser.quit" => cx.quit(),
            // The root view's.
            "location.focus" => {}
            _ => tracing::warn!("unknown command {command:?}"),
        }
        cx.notify();
    }

    /// Index of the tab numbered `arg` (counting from 1), if there is one.
    fn tab_number(&self, arg: &str) -> Option<usize> {
        let n: usize = arg.parse().ok()?;
        (1..=self.tabs.len()).contains(&n).then(|| n - 1)
    }

    /// Adds a tab that opens `url` and returns its index. The active tab doesn't change.
    fn open_tab(&mut self, url: String, cx: &mut Context<Self>) -> usize {
        let config = self.config.clone();
        let tab = cx.new(|cx| Navigable::new(config, url, cx));
        // The subscriptions end with the tab.
        cx.observe(&tab, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&tab, Self::navigable_event).detach();
        self.tabs.push(tab);
        self.tabs.len() - 1
    }

    /// Closes tab `idx`, except the last one.
    fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.active > idx || self.active == self.tabs.len() {
            self.active -= 1;
        }
    }

    /// Opens what the user typed in the active tab.
    fn open_input(&mut self, input: &str, cx: &mut Context<Self>) {
        let url = fixup(input, &self.settings.search);
        if !url.is_empty() {
            self.tabs[self.active].update(cx, |tab, cx| tab.open(url, cx));
        }
    }

    fn navigable_event(
        &mut self,
        tab: Entity<Navigable>,
        event: &NavigableEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            NavigableEvent::OpenNew(url) => _ = self.open_tab(url.clone(), cx),
            NavigableEvent::Download(status) => {
                let status = status.clone();
                cx.spawn(async move |this, cx| {
                    while let Ok(line) = status.recv().await {
                        let shown = this.update(cx, |this, cx| {
                            this.status = Some(line);
                            cx.notify();
                        });
                        if shown.is_err() {
                            break;
                        }
                    }
                })
                .detach();
                // A tab opened just for this has nothing to show.
                if tab.read(cx).url().is_empty() {
                    match self.tabs.iter().position(|t| *t == tab) {
                        Some(idx) if self.tabs.len() > 1 => self.close_tab(idx),
                        _ => {
                            let home = self.settings.home.clone();
                            tab.update(cx, |tab, cx| tab.open(home, cx));
                        }
                    }
                }
            }
        }
        cx.notify();
    }
}

/// The zoom step after `zoom` (a multiplier), or before it; the ends stay put.
fn zoom_step(zoom: f32, bigger: bool) -> f32 {
    let percent = (zoom * 100.0).round() as u32;
    let step = if bigger {
        ZOOM_STEPS.iter().find(|&&s| s > percent)
    } else {
        ZOOM_STEPS.iter().rev().find(|&&s| s < percent)
    };
    step.map_or(zoom, |&s| s as f32 / 100.0)
}

/// Turns what the user typed (or passed on the command line) into a URL, searching with the
/// `search` template for what isn't one.
pub fn fixup(input: &str, search: &str) -> String {
    let input = input.trim();
    let has_scheme = |scheme: &str| {
        input
            .get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    };
    if input.is_empty()
        || input.contains("://")
        || ["sig:", "data:", "about:"].into_iter().any(has_scheme)
    {
        return input.to_string();
    }
    if let Some(path) = input.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return router::file_url(&home.join(path));
        }
    }
    if input.starts_with('/') {
        return router::file_url(std::path::Path::new(input));
    }
    let host = input.split(['/', '?', '#']).next().unwrap_or_default();
    let host_name = host.rsplit_once(':').map_or(host, |(name, _)| name);
    if !input.contains(char::is_whitespace) {
        if matches!(host_name, "localhost" | "127.0.0.1" | "[::1]") {
            return format!("http://{input}");
        }
        if host_name.contains('.') {
            return format!("https://{input}");
        }
    }
    if search.contains("{}") {
        let query =
            percent_encoding::utf8_percent_encode(input, percent_encoding::NON_ALPHANUMERIC);
        return search.replace("{}", &query.to_string());
    }
    format!("https://{input}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_keeps_a_sensible_tab_active() {
        let cx = &mut gpui::TestAppContext::single();
        let (config, settings) = (
            Config::parse(None).unwrap(),
            Settings::new(&Default::default()),
        );
        let urls = vec!["sig://home".to_string(); 3];
        let browser = cx.new(|cx| Browser::new(config, settings, urls, cx));
        browser.update(cx, |browser, _| {
            let second = browser.tabs[1].entity_id();
            browser.active = 2;
            browser.close_tab(2);
            assert_eq!(browser.active, 1);
            browser.close_tab(0);
            assert_eq!((browser.active, browser.tabs[0].entity_id()), (0, second));
            // The last tab stays.
            browser.close_tab(0);
            assert_eq!(browser.tabs.len(), 1);
            assert_eq!(browser.tab_number("1"), Some(0));
            assert_eq!(browser.tab_number("2"), None);
            assert_eq!(browser.tab_number("0"), None);
        });
    }

    #[test]
    fn zoom_walks_the_ladder() {
        assert_eq!(zoom_step(1.0, true), 1.1);
        assert_eq!(zoom_step(1.1, true), 1.25);
        assert_eq!(zoom_step(1.0, false), 0.9);
        assert_eq!(zoom_step(0.67, false), 0.5);
        // The ends stay put.
        assert_eq!(zoom_step(3.0, true), 3.0);
        assert_eq!(zoom_step(0.5, false), 0.5);
    }

    #[test]
    fn fixes_up_input() {
        let search = "https://s.test/?q={}";
        assert_eq!(
            fixup("  https://example.com ", search),
            "https://example.com"
        );
        assert_eq!(fixup("sig://settings", search), "sig://settings");
        assert_eq!(fixup("Sig:settings", search), "Sig:settings");
        assert_eq!(fixup("data:text/plain,hi", search), "data:text/plain,hi");
        assert_eq!(fixup("example.com/a?b", search), "https://example.com/a?b");
        assert_eq!(fixup("localhost:8000", search), "http://localhost:8000");
        assert_eq!(fixup("127.0.0.1/x", search), "http://127.0.0.1/x");
        assert_eq!(fixup("/tmp/app.wasm", search), "file:///tmp/app.wasm");
        assert_eq!(
            fixup("rust servo", search),
            "https://s.test/?q=rust%20servo"
        );
        assert_eq!(fixup("sighurt", search), "https://s.test/?q=sighurt");
        assert_eq!(fixup("", search), "");
    }
}
