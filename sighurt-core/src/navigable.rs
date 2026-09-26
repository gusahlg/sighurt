//! A navigable: one place pages are shown, with its own history, whose pages may come from a
//! different engine at every step.
//!
//! The core has no tabs or windows. A UI makes a [`Navigable`] for every place it shows pages (a
//! tab, a split, a window) and draws what it [shows](Navigable::content): the engine's [`Page`],
//! or whatever the UI likes for its own `sig:` URLs and for addresses that can't be opened. Every
//! engine page runs its own engine process; a navigable keeps its page while it navigates within
//! the same engine and starts a new one when a URL belongs to another engine.
//!
//! A navigable keeps a copy of what its page reports and notifies when that changes, so a UI
//! draws its chrome from the navigable alone: an engine painting a frame changes neither.

use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Subscription, Task};

use crate::config::Config;
use crate::download;
use crate::page::{LoadState, Page, PageEvent, PageInfo};
use crate::router::{self, Route};

/// What a navigable shows.
#[derive(Clone, Copy, Debug)]
pub enum Content<'a> {
    /// Nothing: it has not shown anything yet, or its only URL was downloaded.
    Blank,
    /// An engine's page, which may report an error of its own in [`PageInfo::load`].
    Page(&'a Entity<Page>),
    /// A `sig:` URL, the UI's own page for it to draw.
    Ui(&'a str),
    /// An address no engine can open, and why.
    Error { url: &'a str, message: &'a str },
}

/// Requests from a navigable to the UI.
#[derive(Clone, Debug)]
pub enum NavigableEvent {
    /// The page asks for `url` in a new navigable (a link with a target, `window.open`). It is
    /// only raised right after the user clicked the page or pressed a key in it, once for each.
    OpenNew(String),
    /// A download started. Its status lines ("Downloading a.zip: 1.2 MB of 3.4 MB", "Saved
    /// ...") arrive on the channel, which closes when the download is over.
    Download(async_channel::Receiver<String>),
}

/// Something shown instead of the engine page, which stays alive, hidden, so going back to it
/// needs no reload.
enum Cover {
    Ui(String),
    Error { url: String, message: String },
}

impl Cover {
    fn url(&self) -> &str {
        match self {
            Self::Ui(url) | Self::Error { url, .. } => url,
        }
    }
}

pub struct Navigable {
    config: Rc<Config>,
    page: Option<Entity<Page>>,
    /// What `page` reported last.
    info: PageInfo,
    /// Delivers `page`'s events.
    _events: Option<Subscription>,
    cover: Option<Cover>,
    history: History,
    /// A URL whose Content-Type is being looked up to pick its engine, the history entry it is
    /// (`None` for a new one), and the lookup.
    pending: Option<(String, Option<usize>, Task<()>)>,
    /// When the navigable last started a download, for [`throttle`].
    last_download: Option<Instant>,
    /// Page zoom as a multiplier.
    zoom: f32,
}

impl EventEmitter<NavigableEvent> for Navigable {}

impl Navigable {
    /// A navigable that opens `url`, routed by `config`.
    pub fn new(config: Rc<Config>, url: String, cx: &mut Context<Self>) -> Self {
        let mut navigable = Self {
            config,
            page: None,
            info: PageInfo::default(),
            _events: None,
            cover: None,
            history: History::default(),
            pending: None,
            last_download: None,
            zoom: 1.0,
        };
        navigable.load(url, None, cx);
        navigable
    }

    pub fn content(&self) -> Content<'_> {
        match (&self.cover, &self.page) {
            (Some(Cover::Ui(url)), _) => Content::Ui(url),
            (Some(Cover::Error { url, message }), _) => Content::Error { url, message },
            (None, Some(page)) => Content::Page(page),
            (None, None) => Content::Blank,
        }
    }

    /// The engine page, if it is what the navigable shows.
    fn page(&self) -> Option<&Entity<Page>> {
        self.page.as_ref().filter(|_| self.cover.is_none())
    }

    /// What the engine page reports, if it is what the navigable shows.
    pub fn info(&self) -> Option<&PageInfo> {
        self.page().map(|_| &self.info)
    }

    /// The URL the navigable shows, or is about to show while its engine is being picked. Empty
    /// while it is [`Content::Blank`] and not loading.
    pub fn url(&self) -> &str {
        match (&self.pending, &self.cover) {
            (Some((url, ..)), _) => url,
            (None, Some(cover)) => cover.url(),
            (None, None) => &self.info.url,
        }
    }

    /// Whether the navigable is picking an engine or its page is loading.
    pub fn loading(&self) -> bool {
        self.pending.is_some() || self.info().is_some_and(|i| i.load == LoadState::Loading)
    }

    pub fn can_go_back(&self) -> bool {
        let history = &self.history;
        history.index > 0
            || self
                .info()
                .is_some_and(|info| history.page_steps(info, false))
    }

    pub fn can_go_forward(&self) -> bool {
        let history = &self.history;
        history.index + 1 < history.entries.len()
            || self
                .info()
                .is_some_and(|info| history.page_steps(info, true))
    }

    pub fn zoom(&self) -> f32 {
        self.zoom
    }

    /// Opens `url` as a new history entry. What the user typed becomes a URL in the UI.
    pub fn open(&mut self, url: String, cx: &mut Context<Self>) {
        self.load(url, None, cx);
        cx.notify();
    }

    /// Opens `url` as history entry `entry`, or as a new entry for `None`.
    fn load(&mut self, url: String, entry: Option<usize>, cx: &mut Context<Self>) {
        self.pending = None;
        // Stepping to what the page still shows, such as the page a UI page covered, needs
        // neither a lookup nor a reload.
        let shown = self.page.is_some() && self.info.url == url;
        if let (Some(i), true) = (entry, shown) {
            self.history.enter(Some(i), &url, true);
            self.cover = None;
            self.history.unconfirmed = false;
            return;
        }
        match router::route(&url, &self.config) {
            Route::Ui => self.show_cover(Cover::Ui(url), entry),
            Route::Engine(engine) => self.show_engine(engine, url, entry, cx),
            Route::Probe => self.probe(url, entry, cx),
            Route::Download => self.download(url, cx),
            Route::Invalid(message) => self.show_cover(Cover::Error { url, message }, entry),
        }
    }

    fn show_cover(&mut self, cover: Cover, entry: Option<usize>) {
        self.history.enter(entry, cover.url(), false);
        self.history.unconfirmed = false;
        self.cover = Some(cover);
    }

    fn show_engine(
        &mut self,
        engine: String,
        url: String,
        entry: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        match &self.page {
            Some(page) if page.read(cx).engine() == engine => {
                if self.info.url == url {
                    // Opening the URL the page shows reloads it rather than adding it again.
                    page.update(cx, |page, cx| page.reload(cx));
                    self.history.enter(entry, &url, true);
                } else {
                    page.update(cx, |page, cx| page.navigate(&url, cx));
                    self.history.page_navigated(entry, &url);
                }
            }
            _ => {
                // Routing only picks configured engines.
                let command = self.config.engine(&engine).map(|e| e.command.clone());
                let zoom = self.zoom;
                let page =
                    cx.new(|cx| Page::new(engine, command.unwrap_or_default(), url.clone(), cx));
                if zoom != 1.0 {
                    page.update(cx, |page, _| page.set_zoom(zoom));
                }
                self._events = Some(cx.subscribe(&page, Self::page_event));
                // The subscription hears only what the page says from now on.
                self.info = page.read(cx).info().clone();
                self.page = Some(page);
                // A new page has no history of its own yet.
                for e in &mut self.history.entries {
                    e.in_page = false;
                }
                self.history.enter(entry, &url, true);
            }
        }
        self.cover = None;
        self.history.unconfirmed = true;
    }

    /// Looks up `url`'s Content-Type in the background, then routes it by that, unless the
    /// navigable has moved on by then (which drops this task).
    fn probe(&mut self, url: String, entry: Option<usize>, cx: &mut Context<Self>) {
        let probe_url = url.clone();
        let probe = cx.spawn(async move |this, cx| {
            let content_type = cx
                .background_spawn(async move { router::content_type(&probe_url) })
                .await;
            this.update(cx, |this, cx| this.probed(content_type.as_deref(), cx))
                .ok();
        });
        self.pending = Some((url, entry, probe));
    }

    /// Routes the pending URL by its Content-Type.
    fn probed(&mut self, content_type: Option<&str>, cx: &mut Context<Self>) {
        let Some((url, entry, _)) = self.pending.take() else {
            return;
        };
        match router::route_mime(content_type, &self.config) {
            Route::Engine(engine) => self.show_engine(engine, url, entry, cx),
            Route::Download => self.download(url, cx),
            Route::Invalid(message) => self.show_cover(Cover::Error { url, message }, entry),
            // `route_mime` only picks an engine or a download.
            Route::Ui | Route::Probe => {}
        }
        cx.notify();
    }

    /// Saves `url` to the download folder and hands the UI its progress. A navigable gets one
    /// download a second, so a page can't start them without end.
    fn download(&mut self, url: String, cx: &mut Context<Self>) {
        if !throttle(&mut self.last_download, Instant::now()) {
            return tracing::info!("blocked a download of {url}");
        }
        let status = download::start(url, download::folder());
        cx.emit(NavigableEvent::Download(status));
    }

    fn page_event(&mut self, page: Entity<Page>, event: &PageEvent, cx: &mut Context<Self>) {
        match event {
            // `sig:` pages are for the user, not for content to open.
            PageEvent::Open { url, .. } if router::route(url, &self.config) == Route::Ui => {
                tracing::info!("blocked a page from opening {url}");
            }
            PageEvent::Open {
                url,
                new_tab: false,
            } => self.load(url.clone(), None, cx),
            // A page calling window.open in a loop, or from the background, opens nothing.
            PageEvent::Open { url, new_tab: true } => {
                if self.cover.is_none() && page.update(cx, |page, _| page.take_activation()) {
                    cx.emit(NavigableEvent::OpenNew(url.clone()));
                } else {
                    tracing::info!("blocked a page from opening {url} without a click or key");
                }
            }
            PageEvent::Changed | PageEvent::Loaded => {
                self.info = page.read(cx).info().clone();
                // A page loading under a UI page doesn't move the history.
                if *event == PageEvent::Loaded && self.cover.is_none() {
                    self.history.sync(&self.info.url);
                }
            }
        }
        cx.notify();
    }

    /// Steps back or forward: through the page's own history where it has the neighbouring
    /// entry, otherwise by opening that entry, which may switch engines.
    pub fn traverse(&mut self, forward: bool, cx: &mut Context<Self>) {
        // While an engine is still being picked, this just cancels that.
        if self.pending.take().is_some() {
            return cx.notify();
        }
        if let Some(page) = self
            .page()
            .filter(|_| self.history.page_steps(&self.info, forward))
        {
            page.update(cx, |page, _| page.traverse(forward));
            // The load this starts is not a redirect of the current entry.
            self.history.unconfirmed = false;
            return;
        }
        let index = self.history.index;
        let target = if forward {
            Some(index + 1)
        } else {
            index.checked_sub(1)
        };
        if let Some(url) = target
            .and_then(|i| self.history.entries.get(i))
            .map(|e| e.url.clone())
        {
            self.load(url, target, cx);
            cx.notify();
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        if let Some((url, entry, _)) = &self.pending {
            let (url, entry) = (url.clone(), *entry);
            self.load(url, entry, cx);
        } else if let Some(Cover::Error { url, .. }) = &self.cover {
            let (url, entry) = (url.clone(), Some(self.history.index));
            self.load(url, entry, cx);
        } else if let Some(page) = self.page() {
            page.update(cx, |page, cx| page.reload(cx));
            // A reload that redirects replaces the entry.
            self.history.unconfirmed = true;
        }
        cx.notify();
    }

    pub fn stop(&mut self, cx: &mut Context<Self>) {
        // While an engine is still being picked, this just cancels that.
        if self.pending.take().is_none() {
            if let Some(page) = self.page() {
                page.update(cx, |page, _| page.stop());
            }
        }
        cx.notify();
    }

    /// Page zoom as a multiplier; `1.0` is 100%. Kept for the pages the navigable starts later.
    pub fn set_zoom(&mut self, zoom: f32, cx: &mut App) {
        self.zoom = zoom;
        if let Some(page) = &self.page {
            page.update(cx, |page, _| page.set_zoom(zoom));
        }
    }
}

/// One entry of a navigable's history.
struct Entry {
    url: String,
    /// The engine page has this entry in its own history, next to the entries beside it here,
    /// so stepping to it from them can go through the page instead of loading it.
    in_page: bool,
}

/// The URLs a navigable has shown, oldest first: UI pages, and engine pages including the ones
/// they navigated to themselves, so stepping back can switch engines.
#[derive(Default)]
struct History {
    entries: Vec<Entry>,
    index: usize,
    /// The current entry was opened by the navigable and its page hasn't finished loading, so
    /// the URL the engine settles on (after redirects, say) replaces it.
    unconfirmed: bool,
}

impl History {
    /// Whether stepping back or forward goes through the visible page's own history (keeping
    /// its state) rather than opening the neighbouring entry.
    fn page_steps(&self, info: &PageInfo, forward: bool) -> bool {
        if forward {
            return info.can_go_forward
                && self.entries.get(self.index + 1).is_none_or(|e| e.in_page);
        }
        let previous_in_page = self.index > 0 && self.entries[self.index - 1].in_page;
        // The page left the entry's URL without a load: it went to a same-document entry
        // (a fragment, pushed state) that only it knows about.
        let moved = info.load != LoadState::Loading
            && self
                .entries
                .get(self.index)
                .is_some_and(|e| e.url != info.url);
        info.can_go_back && (previous_in_page || moved)
    }

    /// Makes `url` current: entry `entry`, or a new entry after the current one if that is
    /// `None` or holds another URL by now.
    fn enter(&mut self, entry: Option<usize>, url: &str, in_page: bool) {
        match entry.filter(|&i| self.entries.get(i).is_some_and(|e| e.url == url)) {
            Some(i) => self.index = i,
            None if self.entries.get(self.index).is_some_and(|e| e.url == url) => {}
            None => {
                self.entries.truncate(self.index + 1);
                self.entries.push(Entry {
                    url: url.to_string(),
                    in_page,
                });
                self.index = self.entries.len() - 1;
            }
        }
        self.entries[self.index].in_page = in_page;
    }

    /// Records that the engine page was sent to `url`. Its own history now ends with what it
    /// showed before and then `url`, so going back through it is only right when it came
    /// from the entry before.
    fn page_navigated(&mut self, entry: Option<usize>, url: &str) {
        let from = self.index;
        self.enter(entry, url, true);
        if self.index != from + 1 && self.index > 0 {
            self.entries[self.index - 1].in_page = false;
        }
    }

    /// Brings the history up to date with a load the page finished. Its URL may come from a
    /// redirect, a link followed inside the page, or the engine's own back and forward.
    fn sync(&mut self, url: &str) {
        let unconfirmed = std::mem::take(&mut self.unconfirmed);
        let holds = |i: usize| self.entries.get(i).is_some_and(|e| e.url == url);
        let i = self.index;
        if holds(i) {
        } else if unconfirmed && i < self.entries.len() {
            self.entries[i].url = url.to_string();
        } else if i > 0 && holds(i - 1) {
            self.index = i - 1;
        } else if holds(i + 1) {
            self.index = i + 1;
        } else {
            return self.enter(None, url, true);
        }
        self.entries[self.index].in_page = true;
    }
}

/// Lets a page have what it asks for at most once a second; `last` is when it last got it.
fn throttle(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.duration_since(last) < Duration::from_secs(1)) {
        return false;
    }
    *last = Some(now);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A history of `entries` (URL, in the page's history) at `index`.
    fn history(entries: &[(&str, bool)], index: usize) -> History {
        History {
            entries: entries
                .iter()
                .map(|&(url, in_page)| Entry {
                    url: url.to_string(),
                    in_page,
                })
                .collect(),
            index,
            unconfirmed: false,
        }
    }

    fn urls(history: &History) -> Vec<&str> {
        history.entries.iter().map(|e| e.url.as_str()).collect()
    }

    fn info(url: &str, can_go_back: bool, can_go_forward: bool) -> PageInfo {
        PageInfo {
            url: url.into(),
            can_go_back,
            can_go_forward,
            ..PageInfo::default()
        }
    }

    #[test]
    fn entering_drops_forward_entries_and_skips_duplicates() {
        let mut h = history(&[("a", false), ("b", false), ("c", false)], 1);
        h.enter(None, "b", false);
        assert_eq!(urls(&h), ["a", "b", "c"]);
        h.enter(None, "d", false);
        assert_eq!(urls(&h), ["a", "b", "d"]);
        assert_eq!(h.index, 2);
        h.enter(Some(0), "a", false);
        assert_eq!(h.index, 0);

        let mut empty = History::default();
        empty.enter(None, "a", false);
        assert_eq!((urls(&empty), empty.index), (vec!["a"], 0));
    }

    #[test]
    fn a_stale_entry_becomes_a_new_one() {
        // The entry a lookup was for changed while it ran.
        let mut h = history(&[("a", false), ("x", false)], 0);
        h.enter(Some(1), "b", true);
        assert_eq!(urls(&h), ["a", "b"]);
        assert_eq!(h.index, 1);
    }

    #[test]
    fn a_finished_load_replaces_an_unconfirmed_entry() {
        let mut h = history(&[("a", false), ("https://x.test", true)], 1);
        h.unconfirmed = true;
        h.sync("https://x.test/");
        assert_eq!(urls(&h), ["a", "https://x.test/"]);
        assert!(!h.unconfirmed);
    }

    #[test]
    fn engine_navigations_move_through_or_extend_the_history() {
        let mut h = history(&[("a", true), ("b", true), ("c", true)], 1);
        // The engine went back or forward by itself.
        h.sync("a");
        assert_eq!(h.index, 0);
        h.sync("b");
        assert_eq!(h.index, 1);
        // A link followed inside the page.
        h.sync("e");
        assert_eq!(urls(&h), ["a", "b", "e"]);
        assert_eq!(h.index, 2);
        assert!(h.entries[2].in_page);
    }

    #[test]
    fn the_page_steps_only_to_entries_it_has() {
        // Links followed in the page: its back button is the navigable's.
        let h = history(&[("a", true), ("b", true)], 1);
        assert!(h.page_steps(&info("b", true, false), false));
        // A UI page came before. The page's own back would skip it.
        let h = history(&[("sig://home", false), ("b", true)], 1);
        assert!(!h.page_steps(&info("b", true, false), false));
        // The page moved to a same-document entry the history doesn't list, unless loading.
        assert!(h.page_steps(&info("b#top", true, false), false));
        let loading = PageInfo {
            load: LoadState::Loading,
            ..info("b2", true, false)
        };
        assert!(!h.page_steps(&loading, false));
        // Forward: into the page's own entries, or past the end of the history.
        assert!(h.page_steps(&info("b", false, true), true));
        let h = history(&[("a", true), ("sig://home", false)], 0);
        assert!(!h.page_steps(&info("a", false, true), true));
        assert!(!h.page_steps(&info("a", false, false), true));
    }

    #[test]
    fn a_page_sent_back_forgets_what_came_before() {
        let mut h = history(&[("x", true), ("a", true), ("b", true)], 2);
        // Opening "a" again appends it to the page's history after "b", not after "x".
        h.page_navigated(Some(1), "a");
        assert_eq!(h.index, 1);
        assert!(!h.entries[0].in_page && h.entries[1].in_page);
        // Stepping forward the same way keeps "a" reachable from "b".
        h.page_navigated(Some(2), "b");
        assert!(h.entries[1].in_page);
        assert!(h.page_steps(&info("b", true, false), false));
    }

    #[test]
    fn pages_get_one_download_a_second() {
        let start = Instant::now();
        let mut last = None;
        assert!(throttle(&mut last, start));
        assert!(!throttle(&mut last, start + Duration::from_millis(500)));
        assert!(throttle(&mut last, start + Duration::from_millis(1000)));
        assert!(!throttle(&mut last, start + Duration::from_millis(1999)));
    }

    #[test]
    fn sig_urls_are_the_uis() {
        let config = Rc::new(Config::parse(None).unwrap());
        let cx = &mut gpui::TestAppContext::single();
        for url in ["sig://home", "sig:nope"] {
            let navigable = cx.new(|cx| Navigable::new(config.clone(), url.into(), cx));
            navigable.read_with(cx, |navigable, _| {
                assert!(matches!(navigable.content(), Content::Ui(shown) if shown == url));
                assert!(!navigable.loading() && navigable.info().is_none());
            });
        }
        let navigable = cx.new(|cx| Navigable::new(config, "nope:x".into(), cx));
        navigable.read_with(cx, |navigable, _| {
            let Content::Error { url, message } = navigable.content() else {
                panic!("an unknown scheme is an error");
            };
            assert_eq!(url, "nope:x");
            assert!(message.contains("no engine"), "{message}");
        });
    }

    /// Runs a stand-in engine that says a few things about its page and then waits: its
    /// navigable shows what it said without anyone reading the page.
    #[cfg(unix)]
    #[test]
    fn navigables_keep_what_their_pages_report() {
        use crate::page::tests::{encode, hello};
        use sighurt_ipc::{FromEngine, VERSION};

        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("output");
        let said = [
            hello(VERSION),
            FromEngine::Title {
                title: "Fake".into(),
            },
            FromEngine::Status { text: "hi".into() },
        ];
        std::fs::write(&output, encode(&said)).unwrap();
        // The shell keeps the engine's stdout open until its stdin closes.
        let config = Config::parse(Some(&format!(
            "[engines.fake]\ncommand = ['sh', '-c', 'cat \"$0\"; cat > /dev/null; true', '{}']\n\
             extensions = ['fake']",
            output.display()
        )))
        .unwrap();

        let mut cx = gpui::TestAppContext::single();
        let navigable = cx.new(|cx| Navigable::new(Rc::new(config), "file:///a.fake".into(), cx));
        let status =
            |cx: &gpui::TestAppContext| navigable.read_with(cx, |n, _| n.info()?.status.clone());
        let deadline = Instant::now() + Duration::from_secs(10);
        while status(&cx).is_none() {
            assert!(
                Instant::now() < deadline,
                "the navigable never heard from its page"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        navigable.read_with(&cx, |navigable, _| {
            let info = navigable.info().unwrap();
            assert_eq!(info.title.as_deref(), Some("Fake"));
            assert_eq!(navigable.url(), "file:///a.fake");
            // It never said it finished loading.
            assert!(navigable.loading() && !navigable.can_go_back());
        });
    }
}
