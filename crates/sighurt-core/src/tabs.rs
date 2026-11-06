//! Tabs and the engine views attached to them.
//!
//! A tab is identified by its [`TabId`], never by its position, and keeps it across navigations
//! and engine switches. Its history is a list of segments ([`crate::navigation`]); the views of
//! its live segments, and the view it is switching to, are its views. Each [`View`] has a
//! lifecycle ([`ViewState`], §20 of the plan) kept apart from its navigations. Only the active
//! tab's current view is visible.
//!
//! Page-supplied data is bounded before it is kept: text (titles, status, messages) is cut to
//! [`MAX_TEXT`] bytes, and a URL longer than [`MAX_URL`] is dropped, never shortened, since a
//! shortened URL points somewhere else.
//!
//! Must not own: how many surfaces realize a view, or anything about its rendering.

use std::borrow::Cow;
use std::time::Instant;

use sighurt_api::events::Cursor;
use sighurt_api::ids::{DocumentGeneration, RequestId, TabId, ViewId};
use sighurt_api::input::TextInputState;
use sighurt_api::services::Permission;

use crate::navigation::{EngineHistory, Navigations, Record, Segment, Switch};

/// The longest text kept from a page (titles, status text, error messages), in bytes.
pub const MAX_TEXT: usize = 16 * 1024;

/// The longest URL accepted from a page, in bytes (Chrome's limit). Longer ones are dropped.
pub const MAX_URL: usize = 2 * 1024 * 1024;

/// The most closed tabs `tab.reopen` can bring back. The oldest goes first.
pub const MAX_CLOSED: usize = 10;

/// The longest hovered link a keymap is shown, in bytes. Longer ones are not shown, never
/// shortened.
pub const MAX_LINK: usize = 8 * 1024;

/// What `tab.reopen` keeps of a closed tab: its address, not its history.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedTab {
    /// The address it showed, or was loading.
    pub url: String,
    /// The page's own title, if it gave one.
    pub title: Option<String>,
    /// The engine the user chose for its page, if they chose one.
    pub engine: Option<String>,
    /// Its page zoom.
    pub zoom: f64,
}

/// A view's search (`page.find`): what it looks for and what the last result for it said. One
/// per view; a new `page.find` replaces it, and it ends with `page.find-stop`, a new document,
/// or the document failing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Find {
    /// Identifies the search in its results; a result for another is stale.
    pub(crate) search: RequestId,
    /// What it looks for, as the user gave it (control characters made spaces, trimmed, at most
    /// [`MAX_FIND`](sighurt_api::commands::MAX_FIND) bytes).
    pub text: String,
    /// Whether case matters: the text has an upper-case letter in it (smart case).
    pub case_sensitive: bool,
    /// Whether there is a match; `None` until a result comes.
    pub found: Option<bool>,
    /// Which match is selected, from 1, when the finder knows.
    pub current: Option<u32>,
    /// How many matches there are, when the finder knows.
    pub total: Option<u32>,
    /// `false` once neither the frontend nor the page's engine could search.
    pub supported: bool,
}

/// `text`, if it is an http, https or file URL no longer than [`MAX_LINK`]: as the URL parser
/// writes it, which is what opening it would load.
pub(crate) fn link(text: &str) -> Option<String> {
    let url = url::Url::parse(text).ok()?;
    let link = matches!(url.scheme(), "http" | "https" | "file").then(|| String::from(url))?;
    (link.len() <= MAX_LINK).then_some(link)
}

/// `text` cut to at most [`MAX_TEXT`] bytes at a character boundary.
pub(crate) fn clamp(mut text: String) -> String {
    if text.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.shrink_to_fit();
    }
    text
}

/// `url` if it is not longer than [`MAX_URL`].
pub(crate) fn bounded(url: String) -> Option<String> {
    (url.len() <= MAX_URL).then_some(url)
}

/// A browser tab. The chrome sees it as a [`TabSummary`].
#[derive(Clone, Debug)]
pub(crate) struct Tab {
    pub(crate) id: TabId,
    /// Its history. May be empty while its first page loads.
    pub(crate) segments: Vec<Segment>,
    /// The index of the segment it shows, when there is one.
    pub(crate) current: usize,
    /// A new view it is waiting for.
    pub(crate) switch: Option<Switch>,
    /// Page zoom, kept across engine switches.
    pub(crate) zoom: f64,
    /// What went wrong with the latest navigation, until the next one.
    pub(crate) error: Option<LoadError>,
    /// When a page in the tab last started a download.
    pub(crate) last_download: Option<Instant>,
    /// A page opened it, so a page may close it again.
    pub(crate) opened_by_page: bool,
    /// Permissions the user refused, by origin, for the tab's lifetime, so a page can't keep
    /// asking.
    pub(crate) refused: Vec<(String, Permission)>,
}

impl Tab {
    /// An empty tab at page zoom `zoom`.
    pub(crate) fn new(opened_by_page: bool, zoom: f64) -> Self {
        Self {
            id: TabId::next(),
            segments: Vec::new(),
            current: 0,
            switch: None,
            zoom,
            error: None,
            last_download: None,
            opened_by_page,
            refused: Vec::new(),
        }
    }

    /// The segment it shows.
    pub(crate) fn current(&self) -> Option<&Segment> {
        self.segments.get(self.current)
    }

    /// The view of the segment it shows.
    pub(crate) fn current_view(&self) -> Option<ViewId> {
        match self.current()? {
            Segment::View(view) => Some(*view),
            _ => None,
        }
    }

    /// Every view it holds: its live segments' and the one it is switching to.
    pub(crate) fn views(&self) -> impl Iterator<Item = ViewId> + '_ {
        let segments = self.segments.iter().filter_map(|segment| match segment {
            Segment::View(view) => Some(*view),
            _ => None,
        });
        segments.chain(self.switch.map(|switch| switch.view))
    }
}

/// Why a tab couldn't show what was asked of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError {
    /// The address that failed to open, which reloading tries again. `None` when the page the
    /// tab shows failed, which reloading reloads.
    pub url: Option<String>,
    /// What happened, for the user.
    pub message: String,
}

/// A view's lifecycle (§20), apart from its navigations.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ViewState {
    /// Asked to open; not ready to present yet.
    Opening,
    /// Ready and shown.
    LiveVisible,
    /// Ready and hidden: a background tab, a retained history segment, or a page still
    /// loading before it replaces the shown one.
    LiveHidden,
    /// Its document is gone (it crashed, or a guest trapped). Reloading or navigating may
    /// recover it.
    Failed,
    /// Asked to close; waiting for its engine to say it has.
    Closing,
}

/// One engine view, as the core knows it: everything its engine reported, bounded.
#[derive(Clone, Debug)]
pub struct View {
    pub(crate) id: ViewId,
    pub(crate) engine: String,
    /// The internal page it shows (`help` for `sig://help`), fixed when it opened: a view of a
    /// page loads only that page, and no other view loads one.
    pub(crate) page: Option<String>,
    pub(crate) tab: TabId,
    /// The engine said `ViewReady`.
    pub(crate) ready: bool,
    /// Why its document failed, until one commits.
    pub(crate) failure: Option<String>,
    pub(crate) closing: bool,
    /// What the engine was last told.
    pub(crate) visible: bool,
    pub(crate) focused: bool,
    pub(crate) zoom: f64,
    /// The committed document, once there is one.
    pub(crate) document: Option<DocumentGeneration>,
    /// The committed URL; `None` before a commit, or if it was too long to keep.
    pub(crate) url: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) status: Option<String>,
    pub(crate) loading: bool,
    pub(crate) cursor: Cursor,
    pub(crate) text_input: Option<TextInputState>,
    pub(crate) history: EngineHistory,
    pub(crate) navigations: Navigations,
    /// When the user last clicked or pressed a key in it, unless used up.
    pub(crate) activation: Option<Instant>,
    /// When the user last pressed a key afresh in it or clicked it while its document ran no
    /// program, unless used up: what lets an internal page start one (`PtyOpen`). A repeat
    /// never sets it, nor a key that a command of its own took there from another page.
    pub(crate) launch: Option<Instant>,
    /// The link the pointer was over (the page's status text) when that was a click; `None`
    /// after a key press.
    pub(crate) link: Option<String>,
    /// The link the pointer is over now, when the status text is one ([`link`]); `None` for a
    /// new document.
    pub(crate) hovered: Option<String>,
    /// Opened, or last navigated by the user, in an engine the user chose.
    pub(crate) chosen: bool,
    /// The user's answers to `document`'s permission prompts. Dropped with the document.
    pub(crate) answers: Vec<(Permission, bool)>,
    /// Its search, if one is under way. Dropped with the document.
    pub(crate) find: Option<Find>,
    /// The visit `document`'s commit recorded, and that document: what a title it reports
    /// next names (`state`'s "Visited pages").
    pub(crate) visit: Option<(String, DocumentGeneration)>,
    /// Its engine said an element is fullscreen and has not been told to leave it since
    /// (`ExitFullscreen`), nor said it left.
    pub(crate) fullscreen: bool,
}

impl View {
    pub(crate) fn new(engine: String, tab: TabId, zoom: f64) -> Self {
        Self {
            id: ViewId::next(),
            engine,
            page: None,
            tab,
            ready: false,
            failure: None,
            closing: false,
            visible: false,
            focused: false,
            zoom,
            document: None,
            url: None,
            title: None,
            status: None,
            loading: false,
            cursor: Cursor::Default,
            text_input: None,
            history: EngineHistory::default(),
            navigations: Navigations::default(),
            activation: None,
            launch: None,
            link: None,
            hovered: None,
            chosen: false,
            answers: Vec::new(),
            find: None,
            visit: None,
            fullscreen: false,
        }
    }

    /// The user clicked (over `link`, if the pointer was over one) or pressed a key in it at
    /// `at`.
    pub(crate) fn activated(&mut self, at: Instant, link: Option<String>) {
        self.activation = Some(at);
        self.link = link;
    }

    /// When the user last clicked or pressed a key in it, if they did.
    pub fn activation(&self) -> Option<Instant> {
        self.activation
    }

    /// Its id.
    pub fn id(&self) -> ViewId {
        self.id
    }

    /// The document it shows, once one committed.
    pub fn document(&self) -> Option<DocumentGeneration> {
        self.document
    }

    /// The engine it runs in.
    pub fn engine(&self) -> &str {
        &self.engine
    }

    /// The internal page it shows (`help` for `sig://help`), as the core recorded it when the
    /// view opened; `None` for every other view. A chrome names no engine for it.
    pub fn page(&self) -> Option<&str> {
        self.page.as_deref()
    }

    /// The tab it belongs to.
    pub fn tab(&self) -> TabId {
        self.tab
    }

    /// Where it is in its lifecycle.
    pub fn state(&self) -> ViewState {
        match self {
            Self { closing: true, .. } => ViewState::Closing,
            Self {
                failure: Some(_), ..
            } => ViewState::Failed,
            Self { ready: false, .. } => ViewState::Opening,
            Self { visible: true, .. } => ViewState::LiveVisible,
            Self { .. } => ViewState::LiveHidden,
        }
    }

    /// Its search, if one is under way.
    pub fn find(&self) -> Option<&Find> {
        self.find.as_ref()
    }

    /// Its pending navigations.
    pub fn navigations(&self) -> &Navigations {
        &self.navigations
    }

    /// Whether it is chosen: opened, or last navigated by the user, in an engine the user chose
    /// (`page.open-in`, `page.switch-engine`, or an `OpenRequest` naming an engine). Typing an
    /// address, `page.open` and `browser.home` make it unchosen; the page's own navigations
    /// leave it as it is. The links of a chosen view's pages stay in its engine, wherever
    /// routing would send them.
    pub fn chosen(&self) -> bool {
        self.chosen
    }

    /// What is left of it once it is closed.
    pub(crate) fn record(&self) -> Record {
        let (entries, current) = self.history.saved(self.url.as_deref());
        Record {
            engine: self.engine.clone(),
            entries,
            current,
            title: self.title.clone(),
        }
    }
}

/// What a tab's content area shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Content<'a> {
    /// Nothing yet: its first page is loading, or a record waits to be loaded again. The
    /// chrome shows the tab's error here if it has one.
    Blank,
    /// An engine view.
    View(ViewId),
    /// An internal page, which the chrome draws.
    Internal(&'a str),
}

/// What the chrome shows of a tab.
#[derive(Clone, Debug, PartialEq)]
pub struct TabSummary<'a> {
    /// The tab.
    pub id: TabId,
    /// Its title: the page's, or one made from its address.
    pub title: Cow<'a, str>,
    /// The committed address of what it shows. A navigation in progress doesn't change it.
    pub url: Option<&'a str>,
    /// Where a navigation in progress goes, if it is known.
    pub requested: Option<&'a str>,
    /// Whether a navigation is under way or the page is loading.
    pub loading: bool,
    /// Whether `page.back` has somewhere to go.
    pub can_go_back: bool,
    /// Whether `page.forward` has somewhere to go.
    pub can_go_forward: bool,
    /// Page zoom, 1.0 being 100 %. The tab keeps it across engines.
    pub zoom: f64,
    /// Whether the zoom applies to what it shows: an engine page whose engine supports page
    /// zoom. Zoom commands still change the tab's zoom otherwise.
    pub can_zoom: bool,
    /// The page's status text, such as a hovered link.
    pub status: Option<&'a str>,
    /// What the content area shows.
    pub content: Content<'a>,
    /// What went wrong with the latest navigation.
    pub error: Option<&'a LoadError>,
    /// It shows a record: the page will load again, without its previous state.
    pub reloads: bool,
    /// The engine of the view the tab is switching to, until that view has committed and can
    /// present, or the switch fails or is cancelled. It can be the engine the tab shows now.
    pub switching_to: Option<&'a str>,
    /// The search in the page it shows, if one is under way: what the chrome's find field
    /// shows beside it ("3 of 12").
    pub find: Option<&'a Find>,
}

/// A short title for a page that doesn't name itself.
pub fn title_from_url(url: &str) -> String {
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        return rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(rest)
            .to_owned();
    }
    if let Some(path) = url.strip_prefix("file://") {
        return path
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or("Local file")
            .to_owned();
    }
    match url.char_indices().nth(20) {
        Some((end, _)) => format!("{}\u{2026}", &url[..end]),
        None => url.to_owned(),
    }
}

/// The title to show for a page with `title` at `url`.
pub(crate) fn display_title<'a>(title: Option<&'a str>, url: Option<&str>) -> Cow<'a, str> {
    match (title.map(str::trim), url) {
        (Some(title), _) if !title.is_empty() => Cow::Borrowed(title),
        (_, Some(url)) => Cow::Owned(title_from_url(url)),
        (_, None) => Cow::Borrowed("New Tab"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_from_urls() {
        assert_eq!(title_from_url("https://example.com/a/b?c"), "example.com");
        assert_eq!(title_from_url("file:///tmp/app.wasm"), "app.wasm");
        assert_eq!(
            title_from_url("data:text/html,hello world!!"),
            "data:text/html,hello\u{2026}"
        );
        assert_eq!(title_from_url("about:blank"), "about:blank");
        assert_eq!(display_title(Some("  "), Some("https://a.test/")), "a.test");
        assert_eq!(display_title(Some(" Hi "), None), "Hi");
        assert_eq!(display_title(None, None), "New Tab");
    }

    #[test]
    fn page_text_is_bounded() {
        let title = clamp(format!("a{}", "é".repeat(MAX_TEXT)));
        assert_eq!(title.len(), MAX_TEXT - 1);
        assert!(title.capacity() < 2 * MAX_TEXT);
        assert_eq!(clamp("short".into()), "short");
        assert_eq!(bounded("x".repeat(MAX_URL)).map(|u| u.len()), Some(MAX_URL));
        assert_eq!(bounded("x".repeat(MAX_URL + 1)), None);
    }
}
