//! `ChromeState`: the chrome's own interaction state, and the browser [`Snapshot`] it reads.
//!
//! The snapshot is read-only input: the executable fills it from the core's state after every
//! change. `ChromeState` holds what only the chrome needs: the address editor's text,
//! selection, composition and scroll, whether it has focus, and what the pointer hovers over
//! and presses.
//!
//! Must not own: the committed URL, the tab list, tab titles, which commands move the keyboard
//! focus, or anything else the core owns. Editing the address never changes the committed
//! address; submitting it issues a command.

use sighurt_api::ids::{RequestId, TabId};

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
    /// The status line: the active page's hovered link, or the latest word from a download.
    /// Untrusted text; drawn contained.
    pub status: Option<String>,
    /// What `sig://home` shows.
    pub home: HomeInfo,
}

impl Snapshot {
    /// The active tab.
    pub fn active(&self) -> Option<&TabSnapshot> {
        self.tabs.iter().find(|tab| tab.active)
    }
}

/// One tab.
#[derive(Clone, Debug, PartialEq)]
pub struct TabSnapshot {
    /// The tab's identity.
    pub id: TabId,
    /// The core's title for it (`TabSummary::title`): the page's own, or one made from its
    /// address. Untrusted; drawn contained. Pages the chrome draws have titles of its own.
    pub title: String,
    /// The committed URL: what the tab shows now, not what is being typed or requested. Empty
    /// while nothing has committed.
    pub url: String,
    /// Whether the page is loading.
    pub loading: bool,
    /// Whether this is the selected tab.
    pub active: bool,
    /// What the page area shows for this tab.
    pub content: TabContent,
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
    /// The address couldn't be shown; the chrome draws the error. The message is for the user.
    Error(String),
}

/// What the home page says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeInfo {
    /// Where the configuration file is.
    pub config_path: String,
    /// The engines this build has, in the configuration's order.
    pub engines: Vec<EngineRow>,
}

/// One engine on the home page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineRow {
    /// Its name in the configuration.
    pub name: String,
    /// One line about it, such as "compiled in · default" or "disabled in the configuration".
    pub detail: String,
    /// Whether it can open pages; the detail is shown as an error otherwise.
    pub available: bool,
}

/// What the page area says about `sig:` URLs other than the home page.
pub const NO_SUCH_PAGE: &str = "Sighurt has no such page. Its only page is sig://home.";

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
            TabContent::Engine | TabContent::Blank => untrusted_line(self.title.trim(), MAX_TITLE),
        }
    }
}

/// Characters of a title kept for drawing: more than a tab can show.
const MAX_TITLE: usize = 200;

/// Untrusted text made safe to draw on one line: control characters become spaces, bidi
/// embedding, override and isolate controls are dropped (so a title or hovered link can't
/// reorder what follows it), and at most `max_chars` characters are kept.
pub(crate) fn untrusted_line(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(max_chars)
        .collect()
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
    fn page_titles_are_contained() {
        let evil = format!("a\nb\u{202e}gpj.exe{}", "x".repeat(1000));
        let shown = tab(TabContent::Engine, &evil, "").display_title();
        assert!(shown.starts_with("a bgpj.exe"), "{shown}");
        assert_eq!(shown.chars().count(), MAX_TITLE);
    }
}
