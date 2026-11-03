//! Navigation coordination and cross-engine history.
//!
//! # Navigations
//!
//! A navigation is requested (the core sent `Navigate`, `Reload` or `Traverse` with a
//! [`NavigationId`]), in progress (the engine reported `NavigationStarted`), then committed,
//! failed or cancelled; after its commit, loading completes when the view stops loading. The
//! view's current document stays until a navigation commits, and the requested URL is kept
//! apart from the committed one, so an unfinished navigation never shows as the address. A
//! commit, or a newer navigation starting, supersedes the view's earlier pending navigations,
//! and the core's own superseded requests are remembered for when they commit after all. A
//! failed load is not a failed engine.
//!
//! # History
//!
//! A tab's history is a list of [`Segment`]s. The engine owns the history inside its segment,
//! same-document entries included, and reports it with `HistoryChanged`; the core keeps its
//! entries only to save and display them, never matches URLs against them, and moves through
//! them with `Traverse` deltas. Back and forward traverse the current segment's engine history
//! first and cross into the neighbouring segment at its end. Internal `sig:` pages are
//! segments of their own; nothing is inserted into an engine's history.
//!
//! Crossing into another segment keeps the one left behind alive, up to [`RETAINED_VIEWS`]
//! per tab, where a view of an engine with a process per view (WebKit's) counts as all of them:
//! it is kept alone. A segment evicted beyond that, or restored from a saved session, becomes a
//! [`Record`]: its URLs and nothing else. Opening a record loads its URL in a new view, and
//! the page's document, scripts and form state are gone. That is reported as a reload, never
//! as exact restoration.
//!
//! A navigation that commits in a segment discards the segments after it, and the entries after
//! it in that segment's engine history, which a view can't forget, are cut off by a limit.
//! Only a new navigation does this. The core knows its own navigations, traversals and reloads
//! by their ids, including those a newer request superseded, since an engine carries out every
//! traversal it is sent. A commit the page started itself is a new navigation when the engine
//! says it added an entry ([`HistoryEffect::Push`]: a link, a form, a script navigation, a
//! fragment, `pushState`), and not when it replaced the entry or moved to another (a reload,
//! `replaceState`, the page's own `history.back()`). An engine that doesn't say leaves only
//! whether the document changed: a new document counts as a new navigation, a same-document
//! commit doesn't, so then `pushState` leaves later segments reachable and a page's own
//! traversal to another document, or its own reload, discards them.
//!
//! Must not own: the engines' session history.

use sighurt_api::events::HistoryEffect;
use sighurt_api::ids::{NavigationId, ViewId};

use crate::tabs::MAX_URL;

/// Engine views a tab keeps alive besides the one it shows, or one alone of an engine that
/// runs each view in a process of its own (`EngineCapabilities::process_per_view`). Older ones
/// become [`Record`]s.
pub const RETAINED_VIEWS: usize = 2;

/// The most segments a tab's history keeps; the oldest go first.
pub const MAX_SEGMENTS: usize = 50;

/// The most history entries kept of one segment, around its current entry.
pub const MAX_HISTORY_ENTRIES: usize = 50;

/// The most bytes of URLs kept of one segment's history. The current entry is always kept.
pub const MAX_HISTORY_BYTES: usize = 4 * 1024 * 1024;

/// The most of its own superseded requests the core remembers per view, newest kept.
const MAX_SUPERSEDED: usize = 8;

/// What the core asked for, or [`NavigationKind::Page`] for a navigation the page started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationKind {
    /// A new URL.
    Navigate,
    /// The same document again.
    Reload,
    /// Back or forward in the engine's history.
    Traverse,
    /// Started by the page itself: a link, a script, a redirect, the history API.
    Page,
}

impl NavigationKind {
    /// Whether committing it creates a new history entry, discarding what came after. For a
    /// page's own navigation, `history` is what the engine says the commit did, and
    /// `new_document` whether it replaced the document, all that is known without it (see the
    /// module documentation).
    pub fn is_new(self, new_document: bool, history: Option<HistoryEffect>) -> bool {
        match self {
            Self::Navigate => true,
            Self::Reload | Self::Traverse => false,
            Self::Page => history.map_or(new_document, |h| h == HistoryEffect::Push),
        }
    }
}

/// How far a pending navigation has got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationPhase {
    /// The core asked for it.
    Requested,
    /// The engine started it.
    InProgress,
}

/// A navigation of a view that has not committed, failed or been cancelled yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Navigation {
    /// Its id.
    pub id: NavigationId,
    /// What it is.
    pub kind: NavigationKind,
    /// Where it goes, if known and short enough to keep.
    pub url: Option<String>,
    /// How far it has got.
    pub phase: NavigationPhase,
}

/// A view's pending navigations, oldest first, and what the core's superseded requests were.
#[derive(Clone, Debug, Default)]
pub struct Navigations {
    pending: Vec<Navigation>,
    /// The core's requests that a newer navigation superseded, oldest first. They are no longer
    /// pending (their failure means nothing), but the engine may still carry them out, as it
    /// does every traversal it is sent, so their commits are known for what they are.
    superseded: Vec<(NavigationId, NavigationKind)>,
}

impl Navigations {
    /// The core asked for a navigation. It supersedes the core's earlier requests.
    pub(crate) fn request(&mut self, id: NavigationId, kind: NavigationKind, url: Option<String>) {
        self.supersede(|n| n.kind != NavigationKind::Page);
        self.pending.push(Navigation {
            id,
            kind,
            url,
            phase: NavigationPhase::Requested,
        });
    }

    /// The engine started `id`. It supersedes every navigation that was pending before it,
    /// unless it is one of the core's superseded requests, which stays superseded.
    pub(crate) fn started(&mut self, id: NavigationId, url: Option<String>) {
        if self.superseded.iter().any(|&(old, _)| old == id) {
            return;
        }
        self.supersede(|n| n.id < id);
        match self.pending.iter_mut().find(|n| n.id == id) {
            Some(navigation) => {
                navigation.phase = NavigationPhase::InProgress;
                navigation.url = url.or(navigation.url.take());
            }
            None => self.pending.insert(
                0,
                Navigation {
                    id,
                    kind: NavigationKind::Page,
                    url,
                    phase: NavigationPhase::InProgress,
                },
            ),
        }
    }

    /// `id` committed: returns what it was and drops it. It supersedes every navigation that
    /// was pending before it; the core's own are remembered, as [`Navigations::request`] does,
    /// since the engine may still carry them out.
    pub(crate) fn committed(&mut self, id: NavigationId) -> NavigationKind {
        let pending = self.pending.iter().find(|n| n.id == id).map(|n| n.kind);
        let superseded = || {
            let old = self.superseded.iter().find(|&&(old, _)| old == id);
            old.map(|&(_, kind)| kind)
        };
        let kind = pending.or_else(superseded);
        self.pending.retain(|n| n.id != id);
        self.superseded.retain(|&(old, _)| old != id);
        self.supersede(|n| n.id < id);
        kind.unwrap_or(NavigationKind::Page)
    }

    /// `id` ended without committing: returns it if it was pending.
    pub(crate) fn ended(&mut self, id: NavigationId) -> Option<Navigation> {
        self.superseded.retain(|&(old, _)| old != id);
        let index = self.pending.iter().position(|n| n.id == id)?;
        Some(self.pending.remove(index))
    }

    /// The newest pending navigation.
    pub fn latest(&self) -> Option<&Navigation> {
        self.pending.last()
    }

    pub(crate) fn clear(&mut self) {
        self.pending.clear();
        self.superseded.clear();
    }

    /// Stops treating the pending navigations `pick` selects as pending, remembering the
    /// core's own.
    fn supersede(&mut self, pick: impl Fn(&Navigation) -> bool) {
        let ours = self
            .pending
            .extract_if(.., |n| pick(n))
            .filter(|n| n.kind != NavigationKind::Page)
            .map(|n| (n.id, n.kind));
        self.superseded.extend(ours);
        let excess = self.superseded.len().saturating_sub(MAX_SUPERSEDED);
        self.superseded.drain(..excess);
    }
}

/// A view's engine history as the engine last reported it, kept within [`MAX_HISTORY_ENTRIES`]
/// and [`MAX_HISTORY_BYTES`] around the current entry. Indices are the engine's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineHistory {
    /// The kept entries.
    entries: Vec<String>,
    /// The engine's index of `entries[0]`.
    first: usize,
    /// The engine's index of the current entry.
    current: usize,
    /// How many entries the engine has; 0 until it reports.
    len: usize,
    /// The last entry that is still this segment's: the ones after it were left by a new
    /// navigation into another segment.
    limit: Option<usize>,
}

impl EngineHistory {
    /// Takes a `HistoryChanged` report. Returns `false`, keeping what it had, for one that is
    /// inconsistent or has a URL over [`MAX_URL`]: a shortened URL would point elsewhere.
    pub(crate) fn report(&mut self, mut entries: Vec<String>, current: usize) -> bool {
        if current >= entries.len() || entries.iter().any(|url| url.len() > MAX_URL) {
            return false;
        }
        let len = entries.len();
        // Grow the kept window one entry back and one forward at a time while it fits.
        let (mut start, mut end) = (current, current + 1);
        let mut bytes = entries[current].len();
        let fits = |start: usize, end: usize, bytes: usize, url: &String| {
            end - start < MAX_HISTORY_ENTRIES && bytes + url.len() <= MAX_HISTORY_BYTES
        };
        loop {
            let mut grew = false;
            if start > 0 && fits(start, end, bytes, &entries[start - 1]) {
                start -= 1;
                bytes += entries[start].len();
                grew = true;
            }
            if end < len && fits(start, end, bytes, &entries[end]) {
                bytes += entries[end].len();
                end += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }
        entries.truncate(end);
        entries.drain(..start);
        *self = Self {
            entries,
            first: start,
            current,
            len,
            limit: self.limit.map(|limit| limit.max(current)),
        };
        true
    }

    /// Whether the engine can go back within this segment.
    pub fn can_go_back(&self) -> bool {
        self.current > 0
    }

    /// Whether the engine can go forward within this segment.
    pub fn can_go_forward(&self) -> bool {
        self.current + 1 < self.len && self.limit.is_none_or(|limit| self.current < limit)
    }

    /// A new navigation into another segment left this one at its current entry.
    pub(crate) fn end_here(&mut self) {
        self.limit = Some(self.current);
    }

    /// A new navigation committed here: the engine dropped its forward entries itself.
    pub(crate) fn unlimit(&mut self) {
        self.limit = None;
    }

    /// The entries to keep in a [`Record`] and the index of the current one, with `url`, the
    /// committed URL, standing in for an engine that never reported its history.
    pub(crate) fn saved(&self, url: Option<&str>) -> (Vec<String>, usize) {
        if self.len == 0 {
            return (url.map(str::to_owned).into_iter().collect(), 0);
        }
        let end = self.limit.map_or(self.entries.len(), |limit| {
            (limit + 1 - self.first).min(self.entries.len())
        });
        (self.entries[..end].to_vec(), self.current - self.first)
    }
}

/// One stretch of a tab's history.
#[derive(Clone, Debug, PartialEq)]
pub enum Segment {
    /// An internal `sig:` page, which the chrome draws. It has one entry, its URL.
    Internal(String),
    /// A live engine view, whose own history is the segment's.
    View(ViewId),
    /// What is left of an engine view: its URLs, without the document.
    Record(Record),
}

/// A segment without a view: evicted to bound memory, closed by its engine, or restored from a
/// saved session. Opening it again reloads its current URL in a new view; document, script and
/// form state are lost, and the page loads as it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    /// The engine it was open in.
    pub engine: String,
    /// Its history, oldest first. Empty when there was no URL worth keeping (one over
    /// [`MAX_URL`]).
    pub entries: Vec<String>,
    /// The index of the current entry in `entries`.
    pub current: usize,
    /// The title the page had.
    pub title: Option<String>,
}

impl Record {
    /// The URL that opening it loads.
    pub fn url(&self) -> Option<&str> {
        self.entries.get(self.current).map(String::as_str)
    }

    /// Splits it around its current entry: the entries before it and after it stay records
    /// (the neighbours of the view that replaces the current one), `None` where there are none.
    pub(crate) fn split(self) -> (Option<Record>, Option<Record>) {
        let Record {
            engine,
            mut entries,
            current,
            ..
        } = self;
        let after: Vec<_> = entries.drain((current + 1).min(entries.len())..).collect();
        entries.truncate(current);
        // The title was the current entry's.
        let record = |entries: Vec<String>, current| {
            (!entries.is_empty()).then(|| Record {
                engine: engine.clone(),
                entries,
                current,
                title: None,
            })
        };
        let last = entries.len().saturating_sub(1);
        (record(entries, last), record(after, 0))
    }
}

/// A new view a tab is waiting for before it shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Switch {
    /// The view, opened hidden.
    pub view: ViewId,
    /// Where its segment goes once it has committed and is ready.
    pub into: SwitchInto,
    /// The page's own navigation opened it, not the user: another of the page's may replace
    /// it, while one the user started can't be cancelled or replaced by the page.
    pub by_page: bool,
}

/// Where a switched-to view's segment goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SwitchInto {
    /// After the current segment, discarding the segments after it: a new navigation.
    Append,
    /// In place of the record at this index, which it restores.
    Restore(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("https://a.test/{i}")).collect()
    }

    #[test]
    fn a_commit_supersedes_earlier_navigations() {
        let mut navigations = Navigations::default();
        let page = NavigationId::next();
        navigations.started(page, Some("https://a.test/".into()));
        let ours = NavigationId::next();
        navigations.request(ours, NavigationKind::Navigate, None);
        let traverse = NavigationId::next();
        // A newer request replaces the older one of ours, not the page's.
        navigations.request(traverse, NavigationKind::Traverse, None);
        assert!(navigations.ended(ours).is_none());
        assert_eq!(navigations.committed(traverse), NavigationKind::Traverse);
        assert_eq!(navigations.latest(), None);
        // A commit nobody asked for is the page's.
        assert_eq!(
            navigations.committed(NavigationId::next()),
            NavigationKind::Page
        );
    }

    #[test]
    fn a_superseded_request_is_known_when_it_commits() {
        let mut navigations = Navigations::default();
        let first = NavigationId::next();
        navigations.request(first, NavigationKind::Traverse, None);
        let second = NavigationId::next();
        navigations.request(second, NavigationKind::Traverse, None);
        // The engine carries out both. The first isn't pending again when it starts.
        navigations.started(first, None);
        assert_eq!(navigations.latest().unwrap().id, second);
        assert_eq!(navigations.committed(first), NavigationKind::Traverse);
        assert_eq!(navigations.latest().unwrap().id, second);
        assert_eq!(navigations.committed(second), NavigationKind::Traverse);
        // A bounded number of them is remembered, the newest.
        let ids: Vec<_> = (0..=MAX_SUPERSEDED + 1)
            .map(|_| NavigationId::next())
            .collect();
        for &id in &ids {
            navigations.request(id, NavigationKind::Reload, None);
        }
        assert_eq!(navigations.committed(ids[0]), NavigationKind::Page);
        assert_eq!(navigations.committed(ids[1]), NavigationKind::Reload);
    }

    #[test]
    fn a_request_a_page_commit_supersedes_is_known_when_it_commits() {
        let mut navigations = Navigations::default();
        let reload = NavigationId::next();
        navigations.request(reload, NavigationKind::Reload, None);
        let page = NavigationId::next();
        // The page's own navigation commits without having started: the reload is superseded,
        // not forgotten.
        assert_eq!(navigations.committed(page), NavigationKind::Page);
        assert_eq!(navigations.latest(), None);
        assert_eq!(navigations.committed(reload), NavigationKind::Reload);
        // Committed once: a second commit with its id is not the core's.
        assert_eq!(navigations.committed(reload), NavigationKind::Page);
        let traverse = NavigationId::next();
        navigations.request(traverse, NavigationKind::Traverse, None);
        navigations.started(traverse, None);
        let page = NavigationId::next();
        assert_eq!(navigations.committed(page), NavigationKind::Page);
        assert_eq!(navigations.committed(traverse), NavigationKind::Traverse);
    }

    #[test]
    fn only_new_entries_discard_what_came_after() {
        assert!(NavigationKind::Navigate.is_new(false, None));
        assert!(!NavigationKind::Traverse.is_new(true, None));
        assert!(!NavigationKind::Reload.is_new(true, None));
        assert!(NavigationKind::Page.is_new(true, None));
        assert!(!NavigationKind::Page.is_new(false, None));
        // What the engine says decides for the page's own commits.
        assert!(NavigationKind::Page.is_new(false, Some(HistoryEffect::Push)));
        assert!(!NavigationKind::Page.is_new(true, Some(HistoryEffect::Traverse)));
        assert!(!NavigationKind::Page.is_new(true, Some(HistoryEffect::Replace)));
    }

    #[test]
    fn a_newer_start_supersedes() {
        let mut navigations = Navigations::default();
        let ours = NavigationId::next();
        navigations.request(
            ours,
            NavigationKind::Navigate,
            Some("https://a.test/".into()),
        );
        navigations.started(ours, None);
        let latest = navigations.latest().unwrap();
        assert_eq!(latest.phase, NavigationPhase::InProgress);
        assert_eq!(latest.url.as_deref(), Some("https://a.test/"));
        let page = NavigationId::next();
        navigations.started(page, Some("https://b.test/".into()));
        assert!(navigations.ended(ours).is_none());
        assert_eq!(navigations.latest().unwrap().id, page);
    }

    #[test]
    fn history_is_kept_around_the_current_entry() {
        let mut history = EngineHistory::default();
        assert!(history.report(urls(200), 150));
        assert!(history.can_go_back() && history.can_go_forward());
        let (entries, current) = history.saved(None);
        assert_eq!(entries.len(), MAX_HISTORY_ENTRIES);
        assert_eq!(entries[current], "https://a.test/150");
        // Bytes are bounded too, but the current entry always stays.
        let long = |i| format!("https://a.test/{i}{}", "x".repeat(MAX_URL - 20));
        let entries: Vec<_> = (0..5).map(long).collect();
        assert!(history.report(entries, 2));
        let (entries, current) = history.saved(None);
        assert_eq!(entries.len(), 2);
        assert!(entries[current].starts_with("https://a.test/2"));
    }

    #[test]
    fn inconsistent_or_oversized_reports_are_ignored() {
        let mut history = EngineHistory::default();
        assert!(history.report(urls(3), 1));
        assert!(!history.report(urls(3), 3));
        let mut entries = urls(3);
        entries[0] = "x".repeat(MAX_URL + 1);
        assert!(!history.report(entries, 1));
        assert_eq!(history.saved(None).0, urls(3));
    }

    #[test]
    fn leaving_a_segment_limits_its_forward_entries() {
        let mut history = EngineHistory::default();
        assert!(history.report(urls(4), 1));
        history.end_here();
        assert!(!history.can_go_forward());
        let (entries, current) = history.saved(None);
        assert_eq!((entries, current), (urls(2), 1));
        // Back inside it, forward leads to where it was left, then stops.
        assert!(history.report(urls(4), 0));
        assert!(history.can_go_forward());
        history.unlimit();
        assert!(history.report(urls(4), 1));
        assert!(history.can_go_forward());
    }

    #[test]
    fn records_split_around_their_current_entry() {
        let record = Record {
            engine: "servo".into(),
            entries: urls(4),
            current: 2,
            title: Some("t".into()),
        };
        assert_eq!(record.url(), Some("https://a.test/2"));
        let (before, after) = record.split();
        let before = before.unwrap();
        assert_eq!((before.entries, before.current), (urls(2), 1));
        assert_eq!(after.unwrap().entries, ["https://a.test/3"]);
        let single = Record {
            engine: "servo".into(),
            entries: urls(1),
            current: 0,
            title: None,
        };
        assert_eq!(single.split(), (None, None));
    }
}
