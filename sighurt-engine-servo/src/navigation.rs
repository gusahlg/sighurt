//! Navigation for Servo views: which navigation each of Servo's history changes commits, which
//! document it shows and what it did to the history.
//!
//! Servo owns its session history, same-document entries included, and reports it whole with
//! every change (`notify_history_changed`: the entries and the current index). Each change is a
//! commit of something: a new document, a same-document entry (a fragment, `pushState`,
//! `replaceState`), a traversal, or an iframe's navigation, which Servo's joint session history
//! also records (with the top-level URL). This module turns those reports into
//! `NavigationCommitted` and `HistoryChanged`:
//!
//! - **Which navigation.** A traversal the browser asked for is known exactly: Servo's
//!   constellation reports the history change a `go_back`/`go_forward` made right before that
//!   traversal's `notify_traversal_complete`, and the runtime passes its id in. Servo 0.7's
//!   `WebView::load` still returns no id, and `notify_history_changed` still carries no
//!   navigation id, so any other commit is attributed to the latest pending
//!   navigation the browser asked for that fits it (the API allows this): `Navigate` a new
//!   document in a new entry, `Reload` a new document in the same entry. Anything else is the
//!   page's own navigation (its `history.back()` included), with an id allocated here. An
//!   attributed commit supersedes the requests before it.
//! - **Which document.** A new document is known from Servo's `LoadStatus::Started`, which the
//!   new top-level document's script thread reports before it asks to be activated (so before
//!   the history change), and from the browser's own `Navigate` and `Reload`, which always load
//!   a new document. A traversal moves to another document when the entry it lands on belongs
//!   to another one: each entry carries the key of the document that made it, kept alongside
//!   Servo's list as its shape changes (an entry pushed without a new document shares its
//!   predecessor's key). No URL is compared to decide any of this. A document shown again gets
//!   a new [`DocumentGeneration`], so nothing granted to it before carries over.
//! - **What it did to the history** ([`HistoryEffect`]): from the shape of the change, a new
//!   entry after the current one, the current one replaced, or a move to another; unknown for a
//!   shape this doesn't recognise.
//! - **What is reported.** A commit that changes neither the document nor the address and that
//!   the browser didn't ask for (an iframe's navigation) updates the history only.
//! - **The entries' addresses.** Servo 0.5's report named an entry that `pushState` or
//!   `replaceState` made after its neighbour nearer the current one (its projection has no
//!   address for such entries: `Constellation::notify_history_changed`). That workaround stays
//!   (not re-checked against 0.7's projection; see `SERVO-0.7.md`), while the current
//!   entry's address is always right. So each entry keeps the address it had when it was last
//!   current, carried along as the list changes shape, like the document keys; the history the
//!   browser gets (and saves) names every entry rightly.
//!
//! Limits carried from Servo 0.5, all erring towards "new document" except the first: a page
//! navigation
//! from a document that never got a `<body>` (still `Started`) arrives without its own
//! `Started`, which Servo suppresses as unchanged, and is taken for a same-document commit; a
//! same-document commit that lands while a browser `Navigate`/`Reload` is loading and the
//! current document is still `Started` is taken for that navigation; a history reshaped in a
//! way this doesn't recognise (Servo trimming it) starts every entry afresh. A page's own step
//! forward to the last entry (`history.forward()`) looks like a `pushState` that replaced the
//! one entry after it; without a new document it is taken for a traversal and its effect is
//! unknown.
//!
//! Must not own: cross-engine history (the core).

use sighurt_api::events::HistoryEffect;
use sighurt_api::ids::{DocumentGeneration, NavigationId};

/// The most browser requests remembered as pending. Older ones are forgotten: their commits
/// then count as the page's own.
const MAX_PENDING: usize = 8;

/// What the browser asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    Navigate,
    Reload,
    Traverse(i32),
}

/// A commit to report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Commit {
    pub navigation: NavigationId,
    pub url: String,
    pub document: DocumentGeneration,
    pub history: Option<HistoryEffect>,
}

/// How Servo's history changed from one report to the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// The first report.
    First,
    /// Same length, same index: the current entry was replaced (reload, `replaceState`,
    /// `location.replace`).
    Replace,
    /// One entry after the current one, and nothing after it: a new entry.
    Push,
    /// Same length, index moved: a traversal.
    Traverse,
    /// One entry forward to the last entry of a list of the same length: a traversal by one,
    /// or a new entry that replaced the only forward entry.
    ForwardOrPush,
    /// Anything else.
    Other,
}

fn shape(old: Option<(usize, usize)>, new: (usize, usize)) -> Shape {
    let Some((len, index)) = old else {
        return Shape::First;
    };
    let (new_len, new_index) = new;
    let one_forward = new_index == index + 1 && new_len == new_index + 1;
    match () {
        _ if new_len == len && new_index == index => Shape::Replace,
        _ if one_forward && new_len == len => Shape::ForwardOrPush,
        _ if one_forward => Shape::Push,
        _ if new_len == len => Shape::Traverse,
        _ => Shape::Other,
    }
}

/// One view's navigations and Servo's history as last reported, with a document key per entry.
#[derive(Debug, Default)]
pub(crate) struct Navigator {
    /// The browser's requests not yet committed, oldest first.
    pending: Vec<(NavigationId, Request)>,
    entries: Vec<String>,
    keys: Vec<u64>,
    current: usize,
    reported: bool,
    next_key: u64,
    document: Option<DocumentGeneration>,
    /// Servo reported `LoadStatus::Started` since the last new document.
    started: bool,
    /// The load status is `Started` as far as Servo knows (it suppresses a repeated `Started`).
    loading_started: bool,
}

impl Navigator {
    /// The browser asked for a navigation.
    pub fn request(&mut self, navigation: NavigationId, request: Request) {
        if request == Request::Reload {
            // `WebView::reload` sets Servo's load status to `Started` without reporting it, so
            // the new document's own `Started` is suppressed.
            self.loading_started = true;
        }
        self.pending.push((navigation, request));
        let excess = self.pending.len().saturating_sub(MAX_PENDING);
        self.pending.drain(..excess);
    }

    /// Servo's load status for the view changed; `started` is `LoadStatus::Started`, which
    /// means a new document unless it was `counted` with its history change already.
    pub fn load_status(&mut self, started: bool, counted: bool) {
        self.loading_started = started;
        self.started |= started && !counted;
    }

    /// The current document, once one has committed.
    pub fn document(&self) -> Option<DocumentGeneration> {
        self.document
    }

    /// The committed address.
    pub fn url(&self) -> Option<&str> {
        self.reported.then(|| self.entries[self.current].as_str())
    }

    /// Whether a traversal by `delta` lands on an entry.
    pub fn can_traverse(&self, delta: i32) -> bool {
        let target = self.current as i64 + i64::from(delta);
        delta != 0 && self.reported && (0..self.entries.len() as i64).contains(&target)
    }

    /// Forgets the browser's request `navigation`: its commit, if Servo makes one after all,
    /// counts as the page's own.
    pub fn forget(&mut self, navigation: NavigationId) {
        self.pending.retain(|&(id, _)| id != navigation);
    }

    /// Servo reported its history: `entries`, the current one at `current`. `started_later` says
    /// whether Servo's `Started` for this view arrived in the same batch of callbacks after this
    /// report (the two come on different channels, so their order within one batch is
    /// arbitrary). `traversal` is the browser's traversal this change carried out, if Servo said
    /// so (`notify_traversal_complete`). Returns the commit to report, if any.
    pub fn history_changed(
        &mut self,
        entries: Vec<String>,
        current: usize,
        started_later: bool,
        traversal: Option<NavigationId>,
    ) -> Option<Commit> {
        if current >= entries.len() {
            return None;
        }
        let old = self.reported.then_some((self.entries.len(), self.current));
        let shape = shape(old, (entries.len(), current));
        let started = self.started || started_later;

        // The browser request this commit carries out, if any: the traversal Servo named, or
        // the latest navigation or reload that fits.
        let fits = |request: Request| match request {
            Request::Navigate => {
                matches!(
                    shape,
                    Shape::First | Shape::Push | Shape::ForwardOrPush | Shape::Other
                )
            }
            Request::Reload => shape == Shape::Replace,
            Request::Traverse(_) => false,
        };
        let evidence_of_a_new_document = started || self.loading_started;
        let attributed = match traversal {
            Some(id) => self.pending.iter().position(|&(other, _)| other == id),
            None => self
                .pending
                .iter()
                .rposition(|&(_, request)| fits(request) && evidence_of_a_new_document),
        };
        let request = match traversal {
            Some(id) => Some((id, Request::Traverse(current as i32 - self.current as i32))),
            None => attributed.map(|index| self.pending[index]),
        };
        if let Some(index) = attributed {
            self.pending.drain(..=index);
        }
        // The request, when there is one, says which of the two a step forward was.
        let shape = match (shape, request) {
            (Shape::ForwardOrPush, Some((_, Request::Traverse(_)))) => Shape::Traverse,
            (Shape::ForwardOrPush, Some((_, Request::Navigate))) => Shape::Push,
            (shape, _) => shape,
        };

        let other_document = |at: usize| self.keys.get(at) != self.keys.get(self.current);
        let new_document = started
            || matches!(request, Some((_, Request::Navigate | Request::Reload)))
            || match shape {
                Shape::First | Shape::Other => true,
                Shape::Replace | Shape::Push => false,
                Shape::Traverse => other_document(current),
                Shape::ForwardOrPush => other_document(self.current + 1),
            };

        let keys = self.reshape_keys(shape, entries.len(), current, new_document, started);
        let url_changed = self.url() != Some(entries[current].as_str());
        self.keys = keys;
        self.entries = self.reshape_entries(shape, entries, current);
        self.current = current;
        self.reported = true;
        if new_document || self.document.is_none() {
            self.started = false;
            self.document = Some(DocumentGeneration::next());
        }
        let history = match shape {
            Shape::First | Shape::Push => Some(HistoryEffect::Push),
            Shape::Replace => Some(HistoryEffect::Replace),
            Shape::Traverse => Some(HistoryEffect::Traverse),
            // A document loaded into the next entry is a new entry; otherwise it may have been
            // the page's own step forward.
            Shape::ForwardOrPush => started.then_some(HistoryEffect::Push),
            Shape::Other => None,
        };
        (request.is_some() || new_document || url_changed).then(|| Commit {
            navigation: request.map_or_else(NavigationId::next, |(id, _)| id),
            url: self.entries[current].clone(),
            document: self.document.expect("set above"),
            history,
        })
    }

    /// The entries as last reported, each named by the address it had when it was last current
    /// (see the module documentation).
    pub fn entries(&self) -> &[String] {
        &self.entries
    }

    /// The address of each entry of the new list: the one it had when it was last current, as
    /// far as this view saw it, else Servo's.
    fn reshape_entries(&self, shape: Shape, reported: Vec<String>, current: usize) -> Vec<String> {
        let mut entries = match shape {
            Shape::First | Shape::Other => return reported,
            Shape::Push => self.entries[..=self.current].to_vec(),
            Shape::Replace | Shape::Traverse | Shape::ForwardOrPush => self.entries.clone(),
        };
        entries.truncate(reported.len());
        let known = entries.len();
        entries.extend_from_slice(&reported[known..]);
        entries[current].clone_from(&reported[current]);
        entries
    }

    /// The document key of each entry of the new list.
    fn reshape_keys(
        &mut self,
        shape: Shape,
        len: usize,
        current: usize,
        new_document: bool,
        started: bool,
    ) -> Vec<u64> {
        let mut keys = match shape {
            // Nothing to go by: every entry is a document of its own.
            Shape::First | Shape::Other => Vec::new(),
            // A new entry of the current document, unless a document was loaded for it below.
            Shape::Push => {
                let mut kept = self.keys[..=self.current].to_vec();
                kept.push(self.keys[self.current]);
                kept
            }
            Shape::Replace | Shape::Traverse | Shape::ForwardOrPush => self.keys.clone(),
        };
        keys.truncate(len);
        while keys.len() < len {
            keys.push(self.fresh_key());
        }
        // A document loaded for the entry is a document of its own; a traversal to a document
        // Servo kept alive keeps that document's key.
        let loaded = started || (new_document && matches!(shape, Shape::Replace | Shape::Push));
        if loaded {
            keys[current] = self.fresh_key();
        }
        keys
    }

    fn fresh_key(&mut self) -> u64 {
        self.next_key += 1;
        self.next_key
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(list: &[&str]) -> Vec<String> {
        list.iter().map(|u| u.to_string()).collect()
    }

    /// The browser navigates `nav` to `url`: Servo reports `Started`, then the new history.
    fn load(n: &mut Navigator, entries: &[&str], current: usize) -> Commit {
        n.load_status(true, false);
        let commit = n
            .history_changed(urls(entries), current, false, None)
            .expect("a commit");
        n.load_status(false, false);
        commit
    }

    /// The page's own change of the history, or an iframe's.
    fn page(n: &mut Navigator, entries: &[&str], current: usize) -> Option<Commit> {
        n.history_changed(urls(entries), current, false, None)
    }

    /// The browser's traversal `id`, as Servo reports it: the history change, then
    /// `notify_traversal_complete`.
    fn traversed(n: &mut Navigator, entries: &[&str], current: usize, id: NavigationId) -> Commit {
        n.history_changed(urls(entries), current, false, Some(id))
            .expect("a traversal commits")
    }

    /// Servo 0.5 names an entry `pushState` made after its neighbour nearer the current one;
    /// each entry keeps the address it had when it was current.
    #[test]
    fn entries_keep_the_address_they_had_when_current() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/"], 0);
        page(&mut n, &["https://a/", "https://a/#one"], 1);
        // `pushState("#two")`: Servo names the entry before it after it.
        page(
            &mut n,
            &["https://a/", "https://a/#two", "https://a/#two"],
            2,
        );
        let right = urls(&["https://a/", "https://a/#one", "https://a/#two"]);
        assert_eq!(n.entries(), right);
        // Back to #one: now the entry after it is named after it.
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let commit = traversed(
            &mut n,
            &["https://a/", "https://a/#one", "https://a/#one"],
            1,
            back,
        );
        assert_eq!(commit.url, "https://a/#one");
        assert_eq!(n.entries(), right);
        // A new entry from #one drops #two; `replaceState` renames the current entry.
        page(
            &mut n,
            &["https://a/", "https://a/#one", "https://a/#three"],
            2,
        );
        page(
            &mut n,
            &["https://a/", "https://a/#four", "https://a/#four"],
            2,
        );
        assert_eq!(
            n.entries(),
            urls(&["https://a/", "https://a/#one", "https://a/#four"])
        );
    }

    #[test]
    fn the_browsers_navigation_commits_with_its_id_and_a_new_document() {
        let mut n = Navigator::default();
        let first = NavigationId::next();
        n.request(first, Request::Navigate);
        let commit = load(&mut n, &["https://a/"], 0);
        assert_eq!(commit.navigation, first);
        assert_eq!(commit.url, "https://a/");
        assert_eq!(commit.history, Some(HistoryEffect::Push));
        let a = commit.document;

        let second = NavigationId::next();
        n.request(second, Request::Navigate);
        let commit = load(&mut n, &["https://a/", "https://b/"], 1);
        assert_eq!(commit.navigation, second);
        assert_ne!(commit.document, a);
        assert_eq!(commit.history, Some(HistoryEffect::Push));
    }

    #[test]
    fn the_first_document_commits_for_the_browser_without_its_own_started() {
        let mut n = Navigator::default();
        let first = NavigationId::next();
        n.request(first, Request::Navigate);
        // Servo's new WebView starts `Started` and suppresses the first document's report.
        n.load_status(true, true);
        let commit = n
            .history_changed(urls(&["https://a/"]), 0, false, None)
            .unwrap();
        assert_eq!(commit.navigation, first);
    }

    #[test]
    fn same_document_entries_keep_the_document_and_back_stays_in_it() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let doc = load(&mut n, &["https://a/"], 0).document;

        // A fragment and pushState: new entries, same document, the page's own ids.
        let hash = page(&mut n, &["https://a/", "https://a/#x"], 1).unwrap();
        assert_eq!(hash.document, doc);
        assert_eq!(hash.history, Some(HistoryEffect::Push));
        let push = page(&mut n, &["https://a/", "https://a/#x", "https://a/p"], 2).unwrap();
        assert_eq!(push.document, doc);
        assert_ne!(hash.navigation, push.navigation);

        // replaceState: same entry, same document.
        let replaced = page(&mut n, &["https://a/", "https://a/#x", "https://a/q"], 2).unwrap();
        assert_eq!(replaced.document, doc);
        assert_eq!(replaced.history, Some(HistoryEffect::Replace));

        // The browser goes back twice: traversals within the document.
        let entries = ["https://a/", "https://a/#x", "https://a/q"];
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let commit = traversed(&mut n, &entries, 1, back);
        assert_eq!((commit.navigation, commit.document), (back, doc));
        assert_eq!(commit.history, Some(HistoryEffect::Traverse));
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let commit = traversed(&mut n, &entries, 0, back);
        assert_eq!((commit.navigation, commit.document), (back, doc));
        // And forward again, to the last entry: a traversal, not a new entry.
        let forward = NavigationId::next();
        n.request(forward, Request::Traverse(2));
        let commit = traversed(&mut n, &entries, 2, forward);
        assert_eq!((commit.navigation, commit.document), (forward, doc));
        assert_eq!(commit.history, Some(HistoryEffect::Traverse));
    }

    #[test]
    fn traversing_to_another_document_is_a_new_generation_even_when_kept_alive() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let a = load(&mut n, &["https://a/"], 0).document;
        let a_hash = page(&mut n, &["https://a/", "https://a/#x"], 1).unwrap();
        assert_eq!(a_hash.document, a);
        n.request(NavigationId::next(), Request::Navigate);
        let b = load(&mut n, &["https://a/", "https://a/#x", "https://b/"], 2).document;

        // Back to a#x: Servo reactivates a's document without loading it (no `Started`).
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let entries = ["https://a/", "https://a/#x", "https://b/"];
        let commit = traversed(&mut n, &entries, 1, back);
        assert_eq!(commit.navigation, back);
        assert_ne!(
            commit.document, a,
            "a document shown again gets a new generation"
        );
        assert_ne!(commit.document, b);
        let a_again = commit.document;
        // Within a again: same document.
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        assert_eq!(traversed(&mut n, &entries, 0, back).document, a_again);
        // Forward by two to b, reloaded because Servo had evicted it: `Started` arrives in the
        // same batch, after the history report.
        let forward = NavigationId::next();
        n.request(forward, Request::Traverse(2));
        let commit = n
            .history_changed(urls(&entries), 2, true, Some(forward))
            .unwrap();
        assert_eq!(commit.navigation, forward);
        assert_ne!(commit.document, a_again);
        assert_ne!(commit.document, b);
    }

    #[test]
    fn two_traversals_asked_for_before_either_commits_keep_their_ids() {
        // The reviewer's case: back, back, with both requests pending before Servo reports.
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/"], 0);
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/", "https://b/"], 1);
        n.request(NavigationId::next(), Request::Navigate);
        let c = load(&mut n, &["https://a/", "https://b/", "https://c/"], 2).document;
        let (first, second) = (NavigationId::next(), NavigationId::next());
        n.request(first, Request::Traverse(-1));
        n.request(second, Request::Traverse(-1));
        let entries = ["https://a/", "https://b/", "https://c/"];
        let to_b = traversed(&mut n, &entries, 1, first);
        let to_a = traversed(&mut n, &entries, 0, second);
        assert_eq!((to_b.navigation, to_a.navigation), (first, second));
        assert_eq!(to_a.history, Some(HistoryEffect::Traverse));
        let documents = [c, to_b.document, to_a.document];
        assert!(documents[0] != documents[1] && documents[1] != documents[2]);
    }

    #[test]
    fn a_pages_own_traversal_is_its_own_while_the_browser_traverses() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/"], 0);
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/", "https://b/"], 1);
        let entries = ["https://a/", "https://b/"];
        // The browser's back is on its way when the page's `history.back()` lands: Servo says
        // nothing completed, so it is the page's, though its shape fits the browser's request.
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let own = page(&mut n, &entries, 0).expect("another document");
        assert_ne!(own.navigation, back);
        assert_eq!(own.history, Some(HistoryEffect::Traverse));
        // A browser traversal Servo names is the browser's.
        let forward = NavigationId::next();
        n.request(forward, Request::Traverse(1));
        assert_eq!(traversed(&mut n, &entries, 1, forward).navigation, forward);
    }

    #[test]
    fn a_reload_is_a_new_document_in_the_same_entry() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let doc = load(&mut n, &["https://a/"], 0).document;
        let reload = NavigationId::next();
        n.request(reload, Request::Reload);
        // Servo suppresses the new document's `Started`: only the history change comes.
        let commit = page(&mut n, &["https://a/"], 0).unwrap();
        assert_eq!(commit.navigation, reload);
        assert_ne!(commit.document, doc);
        assert_eq!(commit.history, Some(HistoryEffect::Replace));
    }

    #[test]
    fn a_page_navigation_gets_its_own_id_and_supersedes_nothing_it_isnt() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let a = load(&mut n, &["https://a/"], 0).document;
        // The browser's navigation to c is pending while the page follows a link to b.
        let to_c = NavigationId::next();
        n.request(to_c, Request::Navigate);
        n.load_status(true, false);
        let link = page(&mut n, &["https://a/", "https://b/"], 1).unwrap();
        // Servo can't say which: the pending request takes it, as the API allows.
        assert_eq!(link.navigation, to_c);
        assert_ne!(link.document, a);
        n.load_status(false, false);
        // A later link, with nothing pending, is the page's own.
        n.load_status(true, false);
        let own = page(&mut n, &["https://a/", "https://b/", "https://d/"], 2).unwrap();
        assert_ne!(own.navigation, to_c);
        assert_ne!(own.document, link.document);
        assert_eq!(own.history, Some(HistoryEffect::Push));
    }

    #[test]
    fn a_forgotten_request_is_not_credited_with_a_commit() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/"], 0);
        let given_up = NavigationId::next();
        n.request(given_up, Request::Navigate);
        n.forget(given_up);
        let late = load(&mut n, &["https://a/", "https://b/"], 1);
        assert_ne!(late.navigation, given_up);
    }

    #[test]
    fn a_same_document_commit_while_the_browser_navigates_isnt_taken_for_it() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let a = load(&mut n, &["https://a/"], 0).document;
        let to_b = NavigationId::next();
        n.request(to_b, Request::Navigate);
        // The current page pushes a fragment before b's response arrives.
        let hash = page(&mut n, &["https://a/", "https://a/#x"], 1).unwrap();
        assert_ne!(hash.navigation, to_b);
        assert_eq!(hash.document, a);
        // Then b arrives and still carries the browser's id.
        let b = load(&mut n, &["https://a/", "https://a/#x", "https://b/"], 2);
        assert_eq!(b.navigation, to_b);
    }

    #[test]
    fn an_iframe_navigation_changes_only_the_history() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/"], 0);
        // Servo's joint history gains an entry with the top-level URL.
        assert_eq!(page(&mut n, &["https://a/", "https://a/"], 1), None);
        assert!(n.can_traverse(-1));
        // The browser's back, within the iframe, still commits for its request.
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let commit = traversed(&mut n, &["https://a/", "https://a/"], 0, back);
        assert_eq!(commit.navigation, back);
    }

    #[test]
    fn traversals_outside_the_history_are_refused() {
        let mut n = Navigator::default();
        assert!(!n.can_traverse(-1), "nothing committed yet");
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/"], 0);
        n.request(NavigationId::next(), Request::Navigate);
        load(&mut n, &["https://a/", "https://b/"], 1);
        assert!(n.can_traverse(-1));
        assert!(!n.can_traverse(-2));
        assert!(!n.can_traverse(1));
        assert!(!n.can_traverse(0));
    }

    #[test]
    fn an_unrecognised_reshape_starts_afresh() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let a = load(&mut n, &["https://a/"], 0).document;
        let commit = page(&mut n, &["https://x/", "https://y/", "https://z/"], 0).unwrap();
        assert_ne!(commit.document, a);
        assert_eq!(commit.history, None, "what it did is unknown");
        assert_eq!(n.url(), Some("https://x/"));
    }
}
