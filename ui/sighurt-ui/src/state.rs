//! `ChromeState`: the chrome's own interaction state, and the browser [`Snapshot`] it reads.
//!
//! The snapshot is read-only input: the executable fills it from the core's state after every
//! change, and contains every string in it first ([`Snapshot::contain`]), within the bound its
//! field states. `ChromeState` holds what only the chrome needs: the address editor's text,
//! selection, composition and scroll, whether it has focus, whether the find field is open in
//! its place, what the pointer hovers over and presses, and how the tabs are shown and
//! scrolled.
//!
//! Must not own: the committed URL, the tab list, tab titles, which commands move the keyboard
//! focus, the search's state (the core's, in [`Snapshot::find`]), or anything else the core
//! owns. Editing the address never changes the committed address; submitting it issues a
//! command.

use sighurt_api::ids::{RequestId, TabId};

use crate::extension::{contained, is_contained, InlineCompletion};
use crate::hit_test::Target;
use crate::text_edit::TextEditor;

/// What the chrome shows of the browser: the core's state, as plain data.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    /// The tabs in tab-strip order. At most one is active.
    pub tabs: Vec<TabSnapshot>,
    /// Whether the active tab can go back.
    pub can_go_back: bool,
    /// Whether the active tab can go forward.
    pub can_go_forward: bool,
    /// The status line: the active page's hovered link, or `sig`'s own notice. Untrusted;
    /// at most 2 000 characters. While the window is fullscreen, only a fresh notice of `sig`'s
    /// own, for a few seconds.
    pub status: Option<String>,
    /// What the keymap says about itself ("NORMAL"), if anything; at most 64 characters.
    pub keymap: Option<String>,
    /// What `sig://home` shows.
    pub home: HomeInfo,
    /// The search in the active tab's page, if one is under way: what the find field shows
    /// beside it.
    pub find: Option<FindSnapshot>,
    /// The window is fullscreen (the user's `browser.fullscreen`, or a page's element). A chrome
    /// then lists its surfaces only while they are needed: while it has the keyboard, while
    /// [`Snapshot::status`] is `Some`, and while it draws the page area; otherwise it lists none
    /// and puts the page at the whole window.
    pub fullscreen: bool,
    /// The configuration's own commands (`[commands]`), by name: the bare words the command
    /// line runs besides the built-in commands.
    pub commands: Vec<String>,
}

/// A search in the page (`page.find`), as the chrome shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FindSnapshot {
    /// What it looks for. Typed by the user, but contained like the rest; at most 1 024
    /// characters.
    pub text: String,
    /// Whether there is a match; `None` until the first result.
    pub found: Option<bool>,
    /// Which match is selected, from 1, when the finder knows.
    pub current: Option<u32>,
    /// How many matches there are, when the finder knows.
    pub total: Option<u32>,
    /// `false` when nothing could search the page: the frontend did not, and its engine
    /// can't.
    pub supported: bool,
}

impl FindSnapshot {
    /// What a chrome says about the search beside its field: "3 of 12", "12 matches" when the
    /// finder doesn't say which is selected, "no matches", "not supported by `engine`", or
    /// nothing before the first result.
    pub fn indicator(&self, engine: Option<&str>) -> String {
        if !self.supported {
            return match engine {
                Some(engine) => format!("not supported by {engine}"),
                None => "not supported".into(),
            };
        }
        match (self.found, self.current, self.total) {
            (None, ..) => String::new(),
            (Some(false), ..) | (Some(true), _, Some(0)) => "no matches".into(),
            (Some(true), Some(current), Some(total)) => format!("{current} of {total}"),
            (Some(true), None, Some(1)) => "1 match".into(),
            (Some(true), None, Some(total)) => format!("{total} matches"),
            (Some(true), Some(current), None) => format!("match {current}"),
            (Some(true), None, None) => "found".into(),
        }
    }
}

impl Snapshot {
    /// The active tab.
    pub fn active(&self) -> Option<&TabSnapshot> {
        self.tabs.iter().find(|tab| tab.active)
    }

    /// Makes every string safe to show on one line ([`contained`]) and cuts it to the bound its
    /// field states. The executable does this before a chrome sees a snapshot.
    pub fn contain(&mut self) {
        let contain = |text: &mut String, max: usize| {
            if !is_contained(text, max) {
                *text = contained(text, max);
            }
        };
        for tab in &mut self.tabs {
            contain(&mut tab.title, MAX_TITLE);
            contain(&mut tab.url, MAX_URL);
            if let Some(engine) = &mut tab.engine {
                contain(engine, MAX_ENGINE_NAME);
            }
            if let Some(engine) = &mut tab.switching_to {
                contain(engine, MAX_ENGINE_NAME);
            }
            if let TabContent::Error(message) = &mut tab.content {
                contain(message, MAX_MESSAGE);
            }
        }
        if let Some(status) = &mut self.status {
            contain(status, MAX_STATUS);
        }
        if let Some(keymap) = &mut self.keymap {
            contain(keymap, MAX_INDICATOR);
        }
        if let Some(find) = &mut self.find {
            contain(&mut find.text, MAX_FIND_TEXT);
        }
        let home = &mut self.home;
        contain(&mut home.version, MAX_VERSION);
        contain(&mut home.config_path, MAX_PATH);
        for (links, max) in [
            (&mut home.pages, MAX_HOME_PAGES),
            (&mut home.recent, MAX_RECENT),
        ] {
            links.truncate(max);
            // An address is opened as it is: one containing would change is no link.
            links.retain(|link| is_contained(&link.url, MAX_LINK_URL));
            for link in links {
                contain(&mut link.title, MAX_TITLE);
                contain(&mut link.detail, MAX_ENGINE_DETAIL);
            }
        }
        home.keys.truncate(MAX_HOME_KEYS);
        for key in &mut home.keys {
            contain(&mut key.keys, MAX_INDICATOR);
            contain(&mut key.action, MAX_TITLE);
        }
        home.engines.truncate(MAX_ENGINES);
        for engine in &mut home.engines {
            contain(&mut engine.name, MAX_ENGINE_NAME);
            contain(&mut engine.detail, MAX_ENGINE_DETAIL);
        }
    }
}

/// Characters kept of a title: more than a tab can show.
const MAX_TITLE: usize = 200;
/// Characters kept of a URL: the address editor's limit.
const MAX_URL: usize = 32 * 1024;
/// Characters kept of the status line: more than the strip can show.
pub(crate) const MAX_STATUS: usize = 2000;
/// Characters kept of an error message.
pub(crate) const MAX_MESSAGE: usize = 4000;
/// Characters kept of the keymap's indicator.
const MAX_INDICATOR: usize = 64;
/// Characters kept of the search's text.
const MAX_FIND_TEXT: usize = 1024;
/// Characters kept of the configuration's path.
const MAX_PATH: usize = 1000;
/// Characters kept of an engine's name and of its line on the home page (and of a link's).
pub(crate) const MAX_ENGINE_NAME: usize = 100;
pub(crate) const MAX_ENGINE_DETAIL: usize = 500;
/// Characters kept of the version on the home page.
const MAX_VERSION: usize = 64;
/// Characters of a home page link's address: the longest the history keeps.
const MAX_LINK_URL: usize = 8 * 1024;
/// The most pages, keys, visited pages and engines kept on the home page.
const MAX_HOME_PAGES: usize = 8;
const MAX_HOME_KEYS: usize = 12;
const MAX_RECENT: usize = 8;
const MAX_ENGINES: usize = 16;

/// One tab.
#[derive(Clone, Debug, PartialEq)]
pub struct TabSnapshot {
    /// The tab's identity.
    pub id: TabId,
    /// The core's title for it (`TabSummary::title`): the page's own, or one made from its
    /// address. Untrusted; at most 200 characters. Pages the chrome draws have titles of its
    /// own.
    pub title: String,
    /// The committed URL: what the tab shows now, not what is being typed or requested. Empty
    /// while nothing has committed. At most 32 768 characters.
    pub url: String,
    /// Whether the page is loading.
    pub loading: bool,
    /// Whether this is the selected tab.
    pub active: bool,
    /// What the page area shows for this tab.
    pub content: TabContent,
    /// The engine its page runs in, by its name in the configuration ("servo"): for an engine
    /// page, and `None` for `sig:` pages, blank tabs and errors. At most 100 characters.
    pub engine: Option<String>,
    /// The engine the tab is switching to (`page.switch-engine`, `page.open-in`) while that
    /// switch is under way: the new page has not committed yet and the old one is still shown.
    /// `None` when no switch is pending. At most 100 characters.
    pub switching_to: Option<String>,
}

/// What a tab's page area shows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum TabContent {
    /// Nothing yet: a new tab before its first commit.
    #[default]
    Blank,
    /// A `sig:` page, which the chrome draws itself: `sig://home`, or an error for any other.
    Internal,
    /// An engine's view.
    Engine,
    /// The address couldn't be shown; the chrome draws the error. The message is for the user,
    /// at most 4 000 characters.
    Error(String),
}

/// What the home page says. The executable fills it from `sighurt_runtime::home`; each list is
/// at most as long as its field says, and longer ones are cut by [`Snapshot::contain`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeInfo {
    /// The build's version; at most 64 characters.
    pub version: String,
    /// Where the configuration file is; at most 1 000 characters.
    pub config_path: String,
    /// Sighurt's own pages that open in this build, as links; at most 8.
    pub pages: Vec<HomeLink>,
    /// Keys of the keymap in use and what they do; at most 12.
    pub keys: Vec<HomeKey>,
    /// The pages visited last, the newest first, as links; at most 8.
    pub recent: Vec<HomeLink>,
    /// The engines this build has, in the configuration's order; at most 16.
    pub engines: Vec<EngineRow>,
}

/// A link on the home page: a click runs `page.open` with its address.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeLink {
    /// What it opens, exactly: printable ASCII of at most 8 192 characters. A link whose
    /// address is anything else is left out when contained, never changed.
    pub url: String,
    /// What it is called (a visited page's title, which may be empty); at most 200 characters.
    pub title: String,
    /// One line about it, or empty; at most 500 characters.
    pub detail: String,
}

/// A key on the home page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeKey {
    /// Its keystrokes as the configuration writes them ("o, ctrl-o"); at most 64 characters.
    pub keys: String,
    /// What it does; at most 200 characters.
    pub action: String,
}

/// One engine on the home page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRow {
    /// Its name in the configuration; at most 100 characters.
    pub name: String,
    /// One line about it, such as "compiled in · default" or "disabled in the configuration";
    /// at most 500 characters.
    pub detail: String,
    /// Whether it can open pages; the detail is shown as an error otherwise.
    pub available: bool,
}

/// What the page area says about `sig:` URLs other than the home page.
pub const NO_SUCH_PAGE: &str = "Sighurt has no such page. sig://help lists its pages.";

/// Whether a `sig:` URL names the home page (`sig:home`, `SIG://Home/` and the like work too).
pub fn is_home(url: &str) -> bool {
    let rest = url.split_once(':').map_or("", |(_, rest)| rest);
    let name = rest.trim_start_matches('/').split(['/', '?', '#']).next();
    name.is_some_and(|name| name.eq_ignore_ascii_case("home"))
}

impl TabSnapshot {
    /// What the tab strip calls the tab: the core's title, contained, or for the pages the
    /// chrome draws, "Home" and "Can't open page", as in the old UI.
    pub fn display_title(&self) -> String {
        match &self.content {
            TabContent::Internal if is_home(&self.url) => "Home".into(),
            TabContent::Internal | TabContent::Error(_) => "Can't open page".into(),
            TabContent::Engine | TabContent::Blank => contained(self.title.trim(), MAX_TITLE),
        }
    }

    /// The engine a label on the tab names: [`TabSnapshot::engine`], contained, for an engine
    /// page only. The pages the chrome draws, blank tabs and an empty name have none.
    pub fn engine_label(&self) -> Option<String> {
        let engine = self.engine.as_deref();
        let engine = engine.filter(|_| self.content == TabContent::Engine)?;
        let engine = contained(engine, MAX_ENGINE_NAME);
        let engine = engine.trim();
        (!engine.is_empty()).then(|| engine.to_owned())
    }

    /// The engine a switch of an engine page is under way to, contained, if it differs from
    /// the one the tab shows ([`TabSnapshot::engine_label`]); a tab with no engine label has no
    /// switch to show.
    pub fn switching_label(&self) -> Option<String> {
        let current = self.engine_label()?;
        let target = contained(self.switching_to.as_deref()?, MAX_ENGINE_NAME);
        let target = target.trim();
        (!target.is_empty() && target != current).then(|| target.to_owned())
    }
}

/// The chrome's temporary interaction and editing state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ChromeState {
    /// The address and command line.
    pub address: TextEditor,
    /// The tab whose address the editor shows; switching tabs drops what was typed.
    pub address_tab: Option<TabId>,
    /// Whether the address line has keyboard focus.
    pub focused: bool,
    /// Whether the window has keyboard focus; the caret and focus ring show only then.
    pub window_active: bool,
    /// What the pointer is over.
    pub hover: Option<Target>,
    /// What the primary button went down on, until it comes up.
    pub pressed: Option<Target>,
    /// The primary button went down on the address field's engine label. A release still on
    /// that label runs `page.switch-engine`. Not a [`Target`]: see `label_hover`.
    pub label_pressed: bool,
    /// The pointer is over the engine label (the button at the address field's right end).
    pub label_hover: bool,
    /// The address of the home page's link under the pointer. Not a [`Target`]: links are where
    /// the page's text is laid out (`Model`'s page layout), not in the [`crate::Layout`].
    pub link_hover: Option<String>,
    /// The address of the link the primary button went down on; a release still on that link
    /// runs `page.open` with it.
    pub link_pressed: Option<String>,
    /// Whether dragging with the button down selects text in the address line.
    pub selecting: bool,
    /// Why the last `:command` typed into the address line failed, until the line changes.
    pub command_error: Option<String>,
    /// The line was opened with Enter and shows what is being opened; this is the committed
    /// address its tab had then. It lasts until the tab commits another address, stops
    /// loading or is switched away from, or the line is edited or discarded.
    pub opened: Option<String>,
    /// The clipboard read the line waits for: the latest Ctrl+V's, until it is answered or
    /// the line loses focus or its tab.
    pub paste: Option<RequestId>,
    /// The visited address the typed text completes to, shown after the caret, and the
    /// request for it; dropped as the line changes, loses focus or its tab.
    pub completion: InlineCompletion,
    /// `chrome.toggle-tabs` hid the tabs, or showed those `[chrome.standard]` hides.
    pub tabs_toggled: bool,
    /// The tab list's first row shown.
    pub first_row: usize,
    /// Scrolling of the tab list not yet a whole row, in logical pixels.
    pub scrolled: f64,
    /// The find field is open in the address line's place (`page.find` without text): the
    /// line's editor holds the search's text, and this the address line it replaced, until
    /// the field closes.
    pub find: Option<FindField>,
}

/// The find field while it is open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FindField {
    /// The address line as it was when the field opened; it comes back when the field
    /// closes.
    pub address: TextEditor,
    /// The text the field last searched for (`page.find <text>`), so only an edit that changes
    /// it searches again. Empty when it ended the search.
    pub searched: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(content: TabContent, title: &str, url: &str) -> TabSnapshot {
        TabSnapshot {
            id: TabId::next(),
            title: title.into(),
            url: url.into(),
            loading: false,
            active: false,
            content,
            engine: None,
            switching_to: None,
        }
    }

    #[test]
    fn home_urls() {
        for url in [
            "sig://home",
            "sig://home/",
            "SIG://Home",
            "sig:home",
            "sig://home?x#y",
        ] {
            assert!(is_home(url), "{url}");
        }
        for url in ["sig://settings", "sig://", "sig:", "sig://homepage"] {
            assert!(!is_home(url), "{url}");
        }
    }

    #[test]
    fn titles() {
        use TabContent::*;
        let title = |content, title, url| tab(content, title, url).display_title();
        // The pages the chrome draws have its own titles, whatever the core calls them.
        assert_eq!(title(Internal, "sig://home", "sig://home"), "Home");
        assert_eq!(
            title(Internal, "sig://nope", "sig://nope"),
            "Can't open page"
        );
        assert_eq!(
            title(Error("x".into()), "a.test", "https://a.test"),
            "Can't open page"
        );
        // Otherwise the core's title shows.
        assert_eq!(title(Blank, "New Tab", ""), "New Tab");
        assert_eq!(title(Engine, "  Blåbär  ", "https://a.test"), "Blåbär");
        assert_eq!(
            title(Engine, "app.wasm", "file:///tmp/app.wasm"),
            "app.wasm"
        );
    }

    #[test]
    fn every_string_of_a_snapshot_is_contained_to_its_bound() {
        let long = |c: char, n: usize| format!("\u{202e}a\nb{}", c.to_string().repeat(n));
        let mut tab = tab(
            TabContent::Error(long('m', 5000)),
            &long('t', 300),
            &long('u', 40000),
        );
        tab.active = true;
        tab.engine = Some(long('e', 200));
        tab.switching_to = Some(long('w', 200));
        let mut snapshot = Snapshot {
            tabs: vec![tab],
            status: Some(long('s', 3000)),
            keymap: Some(long('k', 100)),
            find: Some(FindSnapshot {
                text: long('f', 2000),
                ..FindSnapshot::default()
            }),
            home: HomeInfo {
                version: long('v', 100),
                config_path: long('p', 2000),
                pages: vec![
                    HomeLink {
                        url: "sig://help".into(),
                        title: long('t', 300),
                        detail: long('d', 600),
                    },
                    HomeLink {
                        url: "https://a.test/\u{202e}".into(),
                        ..HomeLink::default()
                    },
                ],
                keys: vec![
                    HomeKey {
                        keys: long('k', 100),
                        action: long('a', 300),
                    };
                    20
                ],
                recent: vec![
                    HomeLink {
                        url: format!("https://a.test/{}", long('u', 9000)),
                        ..HomeLink::default()
                    },
                    HomeLink {
                        url: "https://a.test/".into(),
                        ..HomeLink::default()
                    },
                ],
                engines: vec![EngineRow {
                    name: long('n', 200),
                    detail: long('d', 600),
                    available: true,
                }],
            },
            ..Snapshot::default()
        };
        snapshot.contain();
        let tab = &snapshot.tabs[0];
        let TabContent::Error(message) = &tab.content else {
            panic!("{:?}", tab.content)
        };
        let home = &snapshot.home;
        let engine = &home.engines[0];
        // The links whose addresses containing would change are gone; the others stay exact.
        let urls = |links: &[HomeLink]| links.iter().map(|l| l.url.clone()).collect::<Vec<_>>();
        assert_eq!(urls(&home.pages), ["sig://help"]);
        assert_eq!(urls(&home.recent), ["https://a.test/"]);
        assert_eq!(home.keys.len(), 12);
        let strings = [
            (&tab.title, 200),
            (&tab.url, 32 * 1024),
            (tab.engine.as_ref().unwrap(), 100),
            (tab.switching_to.as_ref().unwrap(), 100),
            (message, 4000),
            (snapshot.status.as_ref().unwrap(), 2000),
            (snapshot.keymap.as_ref().unwrap(), 64),
            (&snapshot.find.as_ref().unwrap().text, 1024),
            (&snapshot.home.config_path, 1000),
            (&engine.name, 100),
            (&engine.detail, 500),
            (&home.version, 64),
            (&home.pages[0].title, 200),
            (&home.pages[0].detail, 500),
            (&home.keys[0].keys, 64),
            (&home.keys[0].action, 200),
        ];
        for (text, bound) in strings {
            assert!(text.starts_with("a b"), "{text:.10}");
            assert_eq!(text.chars().count(), bound);
        }
    }

    /// Only an engine page names its engine, contained and trimmed; an empty name is none.
    #[test]
    fn engine_labels() {
        use TabContent::*;
        let label = |content, engine: Option<&str>| {
            let mut tab = tab(content, "t", "https://a.test");
            tab.engine = engine.map(Into::into);
            tab.engine_label()
        };
        assert_eq!(label(Engine, Some("servo")), Some("servo".into()));
        assert_eq!(label(Engine, Some(" \u{202e}wpe\n")), Some("wpe".into()));
        assert_eq!(label(Engine, Some(" ")), None);
        assert_eq!(label(Engine, None), None);
        for content in [Blank, Internal, Error("x".into())] {
            assert_eq!(label(content, Some("servo")), None);
        }
    }

    /// A switch is shown only for an engine page, and only toward a different engine.
    #[test]
    fn switching_labels() {
        let mut tab = tab(TabContent::Engine, "t", "https://a.test");
        tab.engine = Some("servo".into());
        tab.switching_to = Some("wpe".into());
        assert_eq!(tab.switching_label(), Some("wpe".into()));
        tab.switching_to = Some(" \u{202e}wpe\n".into());
        assert_eq!(tab.switching_label(), Some("wpe".into()));
        // The same engine, or none, is not a switch to show.
        tab.switching_to = Some("servo".into());
        assert_eq!(tab.switching_label(), None);
        tab.switching_to = None;
        assert_eq!(tab.switching_label(), None);
        tab.content = TabContent::Blank;
        tab.switching_to = Some("wpe".into());
        assert_eq!(tab.switching_label(), None);
    }

    #[test]
    fn find_indicators() {
        let find = |found, current, total, supported| FindSnapshot {
            text: "x".into(),
            found,
            current,
            total,
            supported,
        };
        let says = |f: FindSnapshot| f.indicator(Some("servo"));
        assert_eq!(says(find(Some(true), Some(3), Some(12), true)), "3 of 12");
        assert_eq!(says(find(Some(true), None, Some(12), true)), "12 matches");
        assert_eq!(says(find(Some(true), None, Some(1), true)), "1 match");
        assert_eq!(says(find(Some(false), None, Some(0), true)), "no matches");
        assert_eq!(says(find(Some(false), None, None, true)), "no matches");
        assert_eq!(says(find(Some(true), None, None, true)), "found");
        assert_eq!(says(find(None, None, None, true)), "");
        assert_eq!(
            says(find(None, None, None, false)),
            "not supported by servo"
        );
        assert_eq!(
            find(None, None, None, false).indicator(None),
            "not supported"
        );
    }

    #[test]
    fn page_titles_are_contained() {
        let evil = format!("a\nb\u{202e}gpj.exe{}", "x".repeat(1000));
        let shown = tab(TabContent::Engine, &evil, "").display_title();
        assert!(shown.starts_with("a bgpj.exe"), "{shown}");
        assert_eq!(shown.chars().count(), MAX_TITLE);
    }
}
