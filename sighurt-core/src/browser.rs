//! The session: tabs, what each one shows, its history, and how a URL finds the engine that
//! opens it.
//!
//! [`Browser`] is plain state plus [`Browser::run_command`]. A UI crate draws it: the tabs, the
//! active tab's URL and state, and its [`Page`] or [`Shell`] page. Every engine page runs its
//! own engine process; a tab keeps its page while it navigates within the same engine and
//! starts a new one when a URL belongs to another engine.
//!
//! Each tab keeps a copy of what its page reports, updated when that changes, so the UI draws
//! the tabs from the browser alone: an engine painting a frame changes neither.

use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Entity, Subscription, Task};

use crate::config::Config;
use crate::page::{LoadState, Page, PageEvent, PageInfo};
use crate::router::{self, Route};
use crate::{commands, download};

/// Zoom levels in percent, as in common browsers.
const ZOOM_STEPS: [u32; 13] = [50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// A page that is not an engine's: the UI draws it.
#[derive(Clone, Debug, PartialEq)]
pub enum Shell {
    /// `sig://home`.
    Home,
    /// An address that couldn't be opened, and why.
    Error { url: String, message: String },
}

impl Shell {
    /// The page a `sig://` URL names (`sig:home`, `SIG://Home/` and the like work too), or an
    /// error page for the others.
    fn parse(url: &str) -> Self {
        let rest = url.split_once(':').map_or("", |(_, rest)| rest);
        let name = rest.trim_start_matches('/').split(['/', '?', '#']).next();
        match name.unwrap_or_default().to_ascii_lowercase().as_str() {
            "home" => Self::Home,
            _ => Self::Error {
                url: url.to_string(),
                message: "Sighurt has no such page. Its only page is sig://home.".into(),
            },
        }
    }

    fn url(&self) -> &str {
        match self {
            Self::Home => "sig://home",
            Self::Error { url, .. } => url,
        }
    }
}

/// One entry of a tab's history.
struct Entry {
    url: String,
    /// The tab's engine page has this entry in its own history, next to the entries beside
    /// it here, so stepping to it from them can go through the page instead of loading it.
    in_page: bool,
}

#[derive(Default)]
pub struct Tab {
    id: u64,
    /// The engine page. It stays alive, hidden, while a shell page covers it, so going back to
    /// it needs no reload.
    page: Option<Entity<Page>>,
    /// What `page` reported last.
    info: PageInfo,
    /// Delivers `page`'s events to the browser.
    _events: Option<Subscription>,
    /// A shell page shown instead of `page`.
    shell: Option<Shell>,
    /// URLs this tab has shown, oldest first: shell pages, and engine pages including the ones
    /// they navigated to themselves, so stepping back can switch engines.
    history: Vec<Entry>,
    index: usize,
    /// The current history entry was opened by the browser and its page hasn't finished
    /// loading, so the URL the engine settles on (after redirects, say) replaces it.
    unconfirmed: bool,
    /// A URL whose Content-Type is being looked up to pick its engine, the history entry it is
    /// (`None` for a new one), and the lookup.
    pending: Option<(String, Option<usize>, Task<()>)>,
    /// When the page last opened a tab and the tab last started a download, for [`throttle`].
    last_popup: Option<Instant>,
    last_download: Option<Instant>,
    /// Page zoom as a multiplier.
    zoom: f32,
}

impl Tab {
    fn new(id: u64) -> Self {
        Self {
            id,
            zoom: 1.0,
            ..Self::default()
        }
    }

    /// Stays the same for the tab's life.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// The engine page, unless a shell page covers it.
    pub fn page(&self) -> Option<&Entity<Page>> {
        match self.shell {
            Some(_) => None,
            None => self.page.as_ref(),
        }
    }

    pub fn shell(&self) -> Option<&Shell> {
        self.shell.as_ref()
    }

    /// What the engine page reports, unless a shell page covers it.
    pub fn info(&self) -> Option<&PageInfo> {
        self.page().map(|_| &self.info)
    }

    /// The URL the tab shows, or is about to show while its engine is being picked.
    pub fn url(&self) -> &str {
        match (&self.pending, &self.shell) {
            (Some((url, ..)), _) => url,
            (None, Some(shell)) => shell.url(),
            (None, None) => &self.info.url,
        }
    }

    pub fn title(&self) -> String {
        match (&self.shell, &self.page, &self.pending) {
            (Some(Shell::Home), ..) => "Home".into(),
            (Some(Shell::Error { .. }), ..) => "Can't open page".into(),
            (None, Some(_), _) => page_title(&self.info),
            (None, None, Some((url, ..))) => url_to_title(url),
            (None, None, None) => "New Tab".into(),
        }
    }

    /// Whether the tab is picking an engine or its page is loading.
    pub fn loading(&self) -> bool {
        self.pending.is_some() || self.info().is_some_and(|i| i.load == LoadState::Loading)
    }

    pub fn can_go_back(&self) -> bool {
        self.index > 0 || self.info().is_some_and(|info| self.page_steps(info, false))
    }

    pub fn can_go_forward(&self) -> bool {
        self.index + 1 < self.history.len()
            || self.info().is_some_and(|info| self.page_steps(info, true))
    }

    /// Whether stepping back or forward goes through the visible page's own history (keeping
    /// its state) rather than opening the neighbouring entry.
    fn page_steps(&self, info: &PageInfo, forward: bool) -> bool {
        if forward {
            return info.can_go_forward
                && self.history.get(self.index + 1).is_none_or(|e| e.in_page);
        }
        let previous_in_page = self.index > 0 && self.history[self.index - 1].in_page;
        // The page left the entry's URL without a load: it went to a same-document entry
        // (a fragment, pushed state) that only it knows about.
        let moved = info.load != LoadState::Loading
            && self
                .history
                .get(self.index)
                .is_some_and(|e| e.url != info.url);
        info.can_go_back && (previous_in_page || moved)
    }

    /// Makes `url` current: entry `entry`, or a new entry after the current one if that is
    /// `None` or holds another URL by now.
    fn enter(&mut self, entry: Option<usize>, url: &str, in_page: bool) {
        match entry.filter(|&i| self.history.get(i).is_some_and(|e| e.url == url)) {
            Some(i) => self.index = i,
            None if self.history.get(self.index).is_some_and(|e| e.url == url) => {}
            None => {
                self.history.truncate(self.index + 1);
                self.history.push(Entry {
                    url: url.to_string(),
                    in_page,
                });
                self.index = self.history.len() - 1;
            }
        }
        self.history[self.index].in_page = in_page;
    }

    /// Records that the engine page was sent to `url`. Its own history now ends with what it
    /// showed before and then `url`, so going back through it is only right when it came
    /// from the entry before.
    fn page_navigated(&mut self, entry: Option<usize>, url: &str) {
        let from = self.index;
        self.enter(entry, url, true);
        if self.index != from + 1 && self.index > 0 {
            self.history[self.index - 1].in_page = false;
        }
    }

    /// Brings the history up to date with a load the page finished. Its URL may come from a
    /// redirect, a link followed inside the page, or the engine's own back and forward.
    fn sync_history(&mut self, url: &str) {
        let unconfirmed = std::mem::take(&mut self.unconfirmed);
        let holds = |i: usize| self.history.get(i).is_some_and(|e| e.url == url);
        let i = self.index;
        if holds(i) {
        } else if unconfirmed && i < self.history.len() {
            self.history[i].url = url.to_string();
        } else if i > 0 && holds(i - 1) {
            self.index = i - 1;
        } else if holds(i + 1) {
            self.index = i + 1;
        } else {
            return self.enter(None, url, true);
        }
        self.history[self.index].in_page = true;
    }
}

/// The browser session: its tabs and which one is active.
pub struct Browser {
    config: Config,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    /// The latest word from a download.
    status: Option<String>,
}

impl Browser {
    /// Opens a tab for each of `urls`, or for the home page if there are none.
    pub fn new(config: Config, urls: Vec<String>, cx: &mut Context<Self>) -> Self {
        let mut browser = Self {
            config,
            tabs: Vec::new(),
            active: 0,
            next_tab_id: 0,
            status: None,
        };
        let urls = if urls.is_empty() {
            vec![browser.config.home.clone()]
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

    /// Never empty: the last tab can't be closed.
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Index of the active tab in [`Self::tabs`].
    pub fn active(&self) -> usize {
        self.active
    }

    pub fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    /// A short line about the latest download, for the status bar.
    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Runs a command such as `tab.new` or `page.open https://example.com` (see
    /// [`commands::COMMANDS`]). `location.focus` is the UI's to handle.
    pub fn run_command(&mut self, command: &str, cx: &mut Context<Self>) {
        let (name, arg) = commands::split(command);
        let active = self.active;
        let count = self.tabs.len();
        match name {
            "tab.new" => self.active = self.open_tab(self.config.home.clone(), cx),
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
            "page.open" => self.navigate(active, arg, cx),
            "page.reload" => self.reload(cx),
            "page.stop" => self.stop(cx),
            "page.back" => self.traverse(false, cx),
            "page.forward" => self.traverse(true, cx),
            "page.zoom-in" => self.set_zoom(zoom_step(self.tabs[active].zoom, true), cx),
            "page.zoom-out" => self.set_zoom(zoom_step(self.tabs[active].zoom, false), cx),
            "page.zoom-reset" => self.set_zoom(1.0, cx),
            "browser.home" => {
                let home = self.config.home.clone();
                self.navigate(active, &home, cx);
            }
            "browser.quit" => cx.quit(),
            // The UI's.
            "location.focus" => {}
            _ => tracing::warn!("unknown command {command:?}"),
        }
        cx.notify();
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id == id)
    }

    /// Index of the tab numbered `arg` (counting from 1), if there is one.
    fn tab_number(&self, arg: &str) -> Option<usize> {
        let n: usize = arg.parse().ok()?;
        (1..=self.tabs.len()).contains(&n).then(|| n - 1)
    }

    /// Adds a tab that opens `url` and returns its index. The active tab doesn't change.
    fn open_tab(&mut self, url: String, cx: &mut Context<Self>) -> usize {
        self.tabs.push(Tab::new(self.next_tab_id));
        self.next_tab_id += 1;
        let idx = self.tabs.len() - 1;
        self.load(idx, url, None, cx);
        idx
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

    /// Opens what the user typed in tab `idx`.
    fn navigate(&mut self, idx: usize, input: &str, cx: &mut Context<Self>) {
        let url = router::fixup(input, &self.config.search);
        if !url.is_empty() {
            self.load(idx, url, None, cx);
        }
    }

    /// Opens `url` in tab `idx` as history entry `entry`, or as a new entry for `None`.
    fn load(&mut self, idx: usize, url: String, entry: Option<usize>, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[idx];
        tab.pending = None;
        // Stepping to what the page still shows, such as the page a shell page covered, needs
        // neither a lookup nor a reload.
        let shown = tab.page.is_some() && tab.info.url == url;
        if let (Some(i), true) = (entry, shown) {
            tab.enter(Some(i), &url, true);
            tab.shell = None;
            tab.unconfirmed = false;
            return;
        }
        match router::route(&url, &self.config) {
            Route::Shell => self.show_shell(idx, Shell::parse(&url), entry),
            Route::Engine(engine) => self.show_engine(idx, engine, url, entry, cx),
            Route::Probe => self.probe(idx, url, entry, cx),
            Route::Download => self.download(idx, url, cx),
            Route::Invalid(message) => self.show_shell(idx, Shell::Error { url, message }, entry),
        }
    }

    fn show_shell(&mut self, idx: usize, page: Shell, entry: Option<usize>) {
        let tab = &mut self.tabs[idx];
        tab.enter(entry, page.url(), false);
        tab.unconfirmed = false;
        tab.shell = Some(page);
    }

    fn show_engine(
        &mut self,
        idx: usize,
        engine: String,
        url: String,
        entry: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        let tab = &mut self.tabs[idx];
        match &tab.page {
            Some(page) if page.read(cx).engine() == engine => {
                if tab.info.url == url {
                    // Opening the URL the page shows reloads it rather than adding it again.
                    page.update(cx, |page, cx| page.reload(cx));
                    tab.enter(entry, &url, true);
                } else {
                    page.update(cx, |page, cx| page.navigate(&url, cx));
                    tab.page_navigated(entry, &url);
                }
            }
            _ => {
                // Routing only picks configured engines.
                let command = self.config.engine(&engine).map(|e| e.command.clone());
                let (tab_id, zoom) = (tab.id, tab.zoom);
                let page =
                    cx.new(|cx| Page::new(engine, command.unwrap_or_default(), url.clone(), cx));
                if zoom != 1.0 {
                    page.update(cx, |page, _| page.set_zoom(zoom));
                }
                tab._events = Some(cx.subscribe(&page, move |this, page, event, cx| {
                    this.page_event(tab_id, &page, event, cx)
                }));
                // The subscription hears only what the page says from now on.
                tab.info = page.read(cx).info().clone();
                tab.page = Some(page);
                // A new page has no history of its own yet.
                for e in &mut tab.history {
                    e.in_page = false;
                }
                tab.enter(entry, &url, true);
            }
        }
        tab.shell = None;
        tab.unconfirmed = true;
    }

    /// Looks up `url`'s Content-Type in the background, then routes it by that, unless the tab
    /// has moved on by then (which drops this task).
    fn probe(&mut self, idx: usize, url: String, entry: Option<usize>, cx: &mut Context<Self>) {
        let (tab_id, probe_url) = (self.tabs[idx].id, url.clone());
        let probe = cx.spawn(async move |this, cx| {
            let content_type = cx
                .background_spawn(async move { router::content_type(&probe_url) })
                .await;
            this.update(cx, |this, cx| {
                this.probed(tab_id, content_type.as_deref(), cx)
            })
            .ok();
        });
        self.tabs[idx].pending = Some((url, entry, probe));
    }

    /// Routes tab `tab_id`'s pending URL by its Content-Type.
    fn probed(&mut self, tab_id: u64, content_type: Option<&str>, cx: &mut Context<Self>) {
        let Some(idx) = self.tab_index(tab_id) else {
            return;
        };
        let Some((url, entry, _)) = self.tabs[idx].pending.take() else {
            return;
        };
        match router::route_mime(content_type, &self.config) {
            Route::Engine(engine) => self.show_engine(idx, engine, url, entry, cx),
            Route::Download => {
                self.download(idx, url, cx);
                // A tab opened just for this has nothing to show.
                if self.tabs[idx].history.is_empty() {
                    if self.tabs.len() > 1 {
                        self.close_tab(idx);
                    } else {
                        self.show_shell(idx, Shell::Home, None);
                    }
                }
            }
            Route::Invalid(message) => self.show_shell(idx, Shell::Error { url, message }, entry),
            // `route_mime` only picks an engine or a download.
            Route::Shell | Route::Probe => {}
        }
        cx.notify();
    }

    /// Saves `url` to the download folder and shows how that goes in [`Self::status`]. A tab
    /// gets one download a second, so a page can't start them without end.
    fn download(&mut self, idx: usize, url: String, cx: &mut Context<Self>) {
        if !throttle(&mut self.tabs[idx].last_download, Instant::now()) {
            return tracing::info!("blocked a download of {url}");
        }
        let status = download::start(url, download::folder());
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
    }

    fn page_event(
        &mut self,
        tab_id: u64,
        page: &Entity<Page>,
        event: &PageEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(idx) = self.tab_index(tab_id) else {
            return;
        };
        let tab = &mut self.tabs[idx];
        match event {
            // Shell pages are for the user, not for content to open.
            PageEvent::Open { url, .. } if router::route(url, &self.config) == Route::Shell => {
                tracing::info!("blocked a page from opening {url}");
            }
            PageEvent::Open {
                url,
                new_tab: false,
            } => self.load(idx, url.clone(), None, cx),
            // Only the page in front may open tabs, one a second, so a page calling
            // window.open in a loop can't start engines without end.
            PageEvent::Open { url, new_tab: true } => {
                if idx == self.active
                    && tab.shell.is_none()
                    && throttle(&mut tab.last_popup, Instant::now())
                {
                    self.open_tab(url.clone(), cx);
                } else {
                    tracing::info!("blocked a page from opening a tab for {url}");
                }
            }
            PageEvent::Changed | PageEvent::Loaded => {
                tab.info = page.read(cx).info().clone();
                // A page loading under a shell page doesn't move the tab's history.
                if *event == PageEvent::Loaded && tab.shell.is_none() {
                    let url = tab.info.url.clone();
                    tab.sync_history(&url);
                }
            }
        }
        cx.notify();
    }

    /// Steps the active tab back or forward: through the page's own history where it has the
    /// neighbouring entry, otherwise by opening that entry, which may switch engines.
    fn traverse(&mut self, forward: bool, cx: &mut Context<Self>) {
        let idx = self.active;
        let tab = &mut self.tabs[idx];
        // While an engine is still being picked, this just cancels that.
        if tab.pending.take().is_some() {
            return;
        }
        if let Some(page) = tab.page().filter(|_| tab.page_steps(&tab.info, forward)) {
            page.update(cx, |page, _| page.traverse(forward));
            // The load this starts is not a redirect of the current entry.
            tab.unconfirmed = false;
            return;
        }
        let target = if forward {
            Some(tab.index + 1)
        } else {
            tab.index.checked_sub(1)
        };
        if let Some(url) = target
            .and_then(|i| tab.history.get(i))
            .map(|e| e.url.clone())
        {
            self.load(idx, url, target, cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let idx = self.active;
        let tab = &mut self.tabs[idx];
        if let Some((url, entry, _)) = &tab.pending {
            let (url, entry) = (url.clone(), *entry);
            return self.load(idx, url, entry, cx);
        }
        match &tab.shell {
            Some(Shell::Error { url, .. }) => {
                let (url, entry) = (url.clone(), Some(tab.index));
                self.load(idx, url, entry, cx);
            }
            Some(Shell::Home) => {}
            None => {
                if let Some(page) = &tab.page {
                    page.update(cx, |page, cx| page.reload(cx));
                    // A reload that redirects replaces the entry.
                    tab.unconfirmed = true;
                }
            }
        }
    }

    fn stop(&mut self, cx: &mut App) {
        let tab = &mut self.tabs[self.active];
        // While an engine is still being picked, this just cancels that.
        if tab.pending.take().is_none() {
            if let Some(page) = tab.page() {
                page.update(cx, |page, _| page.stop());
            }
        }
    }

    fn set_zoom(&mut self, zoom: f32, cx: &mut App) {
        let tab = &mut self.tabs[self.active];
        tab.zoom = zoom;
        if let Some(page) = &tab.page {
            page.update(cx, |page, _| page.set_zoom(zoom));
        }
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

/// Lets a page have what it asks for (a new tab, a download) at most once a second; `last`
/// is when it last got it.
fn throttle(last: &mut Option<Instant>, now: Instant) -> bool {
    if last.is_some_and(|last| now.duration_since(last) < Duration::from_secs(1)) {
        return false;
    }
    *last = Some(now);
    true
}

/// The page's own title, or one made from its URL.
fn page_title(info: &PageInfo) -> String {
    match info.title.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() => title.to_string(),
        _ => url_to_title(&info.url),
    }
}

/// A short title for a page that doesn't name itself.
fn url_to_title(url: &str) -> String {
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        return rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or(rest)
            .to_string();
    }
    if let Some(path) = url.strip_prefix("file://") {
        return path
            .rsplit('/')
            .find(|segment| !segment.is_empty())
            .unwrap_or("Local file")
            .to_string();
    }
    match url.char_indices().nth(20) {
        Some((end, _)) => format!("{}\u{2026}", &url[..end]),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tab whose history is `entries` (URL, in the page's history) at `index`.
    fn tab_with(entries: &[(&str, bool)], index: usize) -> Tab {
        let mut tab = Tab::new(0);
        tab.history = entries
            .iter()
            .map(|&(url, in_page)| Entry {
                url: url.to_string(),
                in_page,
            })
            .collect();
        tab.index = index;
        tab
    }

    fn urls(tab: &Tab) -> Vec<&str> {
        tab.history.iter().map(|e| e.url.as_str()).collect()
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
        let mut tab = tab_with(&[("a", false), ("b", false), ("c", false)], 1);
        tab.enter(None, "b", false);
        assert_eq!(urls(&tab), ["a", "b", "c"]);
        tab.enter(None, "d", false);
        assert_eq!(urls(&tab), ["a", "b", "d"]);
        assert_eq!(tab.index, 2);
        tab.enter(Some(0), "a", false);
        assert_eq!(tab.index, 0);

        let mut empty = Tab::new(0);
        empty.enter(None, "a", false);
        assert_eq!((urls(&empty), empty.index), (vec!["a"], 0));
    }

    #[test]
    fn a_stale_entry_becomes_a_new_one() {
        // The entry a lookup was for changed while it ran.
        let mut tab = tab_with(&[("a", false), ("x", false)], 0);
        tab.enter(Some(1), "b", true);
        assert_eq!(urls(&tab), ["a", "b"]);
        assert_eq!(tab.index, 1);
    }

    #[test]
    fn a_finished_load_replaces_an_unconfirmed_entry() {
        let mut tab = tab_with(&[("a", false), ("https://x.test", true)], 1);
        tab.unconfirmed = true;
        tab.sync_history("https://x.test/");
        assert_eq!(urls(&tab), ["a", "https://x.test/"]);
        assert!(!tab.unconfirmed);
    }

    #[test]
    fn engine_navigations_move_through_or_extend_the_history() {
        let mut tab = tab_with(&[("a", true), ("b", true), ("c", true)], 1);
        // The engine went back or forward by itself.
        tab.sync_history("a");
        assert_eq!(tab.index, 0);
        tab.sync_history("b");
        assert_eq!(tab.index, 1);
        // A link followed inside the page.
        tab.sync_history("e");
        assert_eq!(urls(&tab), ["a", "b", "e"]);
        assert_eq!(tab.index, 2);
        assert!(tab.history[2].in_page);
    }

    #[test]
    fn the_page_steps_only_to_entries_it_has() {
        // Links followed in the page: its back button is the tab's.
        let tab = tab_with(&[("a", true), ("b", true)], 1);
        assert!(tab.page_steps(&info("b", true, false), false));
        // A shell page came before. The page's own back would skip it.
        let tab = tab_with(&[("sig://home", false), ("b", true)], 1);
        assert!(!tab.page_steps(&info("b", true, false), false));
        // The page moved to a same-document entry the tab doesn't list, unless still loading.
        assert!(tab.page_steps(&info("b#top", true, false), false));
        let loading = PageInfo {
            load: LoadState::Loading,
            ..info("b2", true, false)
        };
        assert!(!tab.page_steps(&loading, false));
        // Forward: into the page's own entries, or past the end of the tab's.
        assert!(tab.page_steps(&info("b", false, true), true));
        let tab = tab_with(&[("a", true), ("sig://home", false)], 0);
        assert!(!tab.page_steps(&info("a", false, true), true));
        assert!(!tab.page_steps(&info("a", false, false), true));
    }

    #[test]
    fn a_page_sent_back_forgets_what_came_before() {
        let mut tab = tab_with(&[("x", true), ("a", true), ("b", true)], 2);
        // Opening "a" again appends it to the page's history after "b", not after "x".
        tab.page_navigated(Some(1), "a");
        assert_eq!(tab.index, 1);
        assert!(!tab.history[0].in_page && tab.history[1].in_page);
        // Stepping forward the same way keeps "a" reachable from "b".
        tab.page_navigated(Some(2), "b");
        assert!(tab.history[1].in_page);
        assert!(tab.page_steps(&info("b", true, false), false));
    }

    #[test]
    fn pages_get_one_popup_a_second() {
        let start = Instant::now();
        let mut last = None;
        assert!(throttle(&mut last, start));
        assert!(!throttle(&mut last, start + Duration::from_millis(500)));
        assert!(throttle(&mut last, start + Duration::from_millis(1000)));
        assert!(!throttle(&mut last, start + Duration::from_millis(1999)));
    }

    #[test]
    fn closing_keeps_a_sensible_tab_active() {
        let mut browser = Browser {
            config: Config::parse(None).unwrap(),
            tabs: (0..3).map(Tab::new).collect(),
            active: 2,
            next_tab_id: 3,
            status: None,
        };
        browser.close_tab(2);
        assert_eq!(browser.active, 1);
        browser.close_tab(0);
        assert_eq!((browser.active, browser.tabs[0].id), (0, 1));
        // The last tab stays.
        browser.close_tab(0);
        assert_eq!(browser.tabs.len(), 1);
        assert_eq!(browser.tab_number("1"), Some(0));
        assert_eq!(browser.tab_number("2"), None);
        assert_eq!(browser.tab_number("0"), None);
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
    fn shell_urls() {
        for url in [
            "sig://home",
            "sig://home/",
            "SIG://Home",
            "sig:home",
            "sig://home?x#y",
        ] {
            assert_eq!(Shell::parse(url), Shell::Home, "{url}");
        }
        for url in ["sig://settings", "sig://", "sig:"] {
            let Shell::Error {
                url: shown,
                message,
            } = Shell::parse(url)
            else {
                panic!("{url} is not a page");
            };
            assert_eq!(shown, url);
            assert!(message.contains("sig://home"), "{message}");
        }
    }

    #[test]
    fn titles_from_urls() {
        assert_eq!(url_to_title("https://example.com/a/b?c"), "example.com");
        assert_eq!(url_to_title("file:///tmp/app.wasm"), "app.wasm");
        assert_eq!(
            url_to_title("data:text/html,hello world!!"),
            "data:text/html,hello\u{2026}"
        );
        assert_eq!(url_to_title("about:blank"), "about:blank");
    }

    /// Runs a stand-in engine that says a few things about its page and then waits: its tab
    /// shows what it said without anyone reading the page.
    #[cfg(unix)]
    #[test]
    fn tabs_keep_what_their_pages_report() {
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
        let urls = vec!["file:///a.fake".into()];
        let browser = cx.new(|cx| Browser::new(config, urls, cx));
        let status = |cx: &gpui::TestAppContext| {
            browser.read_with(cx, |b, _| b.active_tab().info()?.status.clone())
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while status(&cx).is_none() {
            assert!(
                Instant::now() < deadline,
                "the tab never heard from its page"
            );
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(10));
        }
        browser.read_with(&cx, |browser, _| {
            let tab = browser.active_tab();
            assert_eq!((tab.title(), tab.url()), ("Fake".into(), "file:///a.fake"));
            assert_eq!(tab.info().unwrap().status.as_deref(), Some("hi"));
            // It never said it finished loading.
            assert!(tab.loading() && !tab.can_go_back());
        });
    }
}
