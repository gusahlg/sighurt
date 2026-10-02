//! Navigation for Servo views: which navigation each of Servo's history changes commits, and
//! which document it shows.
//!
//! Servo owns its session history, same-document entries included, and reports it whole with
//! every change (`notify_history_changed`: the entries and the current index). Each change is a
//! commit of something: a new document, a same-document entry (a fragment, `pushState`,
//! `replaceState`), a traversal, or an iframe's navigation, which Servo's joint session history
//! also records (with the top-level URL). This module turns those reports into
//! `NavigationCommitted` and `HistoryChanged`:
//!
//! - **Which navigation.** Servo 0.5's `WebView::load` returns no id, so a commit is attributed
//!   to the latest pending navigation the browser asked for that fits it (the API allows this):
//!   `Navigate` a new document in a new entry, `Reload` a new document in the same entry,
//!   `Traverse` a move by exactly its delta. Anything else is the page's own navigation, with an
//!   id allocated here. An attributed commit supersedes the requests before it.
//! - **Which document.** A new document is known from Servo's `LoadStatus::Started`, which the
//!   new top-level document's script thread reports before it asks to be activated (so before
//!   the history change), and from the browser's own `Navigate` and `Reload`, which always load
//!   a new document. A traversal moves to another document when the entry it lands on belongs
//!   to another one: each entry carries the key of the document that made it, kept alongside
//!   Servo's list as its shape changes (an entry pushed without a new document shares its
//!   predecessor's key). No URL is compared to decide any of this. A document shown again gets
//!   a new [`DocumentGeneration`], so nothing granted to it before carries over.
//! - **What is reported.** A commit that changes neither the document nor the address and that
//!   the browser didn't ask for (an iframe's navigation) updates the history only.
//!
//! Limits of Servo 0.5, all erring towards "new document" except the first: a page navigation
//! from a document that never got a `<body>` (still `Started`) arrives without its own
//! `Started`, which Servo suppresses as unchanged, and is taken for a same-document commit; a
//! same-document commit that lands while a browser `Navigate`/`Reload` is loading and the
//! current document is still `Started` is taken for that navigation; a history reshaped in a
//! way this doesn't recognise (Servo trimming it) starts every entry afresh.
//!
//! Must not own: cross-engine history (the core).

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

    /// Servo reported its history: `entries`, the current one at `current`. `started_later` says
    /// whether Servo's `Started` for this view arrived in the same batch of callbacks after this
    /// report (the two come on different channels, so their order within one batch is
    /// arbitrary). Returns the commit to report, if any.
    pub fn history_changed(
        &mut self,
        entries: Vec<String>,
        current: usize,
        started_later: bool,
    ) -> Option<Commit> {
        if current >= entries.len() {
            return None;
        }
        let old = self.reported.then_some((self.entries.len(), self.current));
        let shape = shape(old, (entries.len(), current));
        let started = self.started || started_later;
        let moved = current as i64 - self.current as i64;

        // The browser request this commit carries out, if any.
        let fits = |request: Request| match request {
            Request::Navigate => {
                matches!(
                    shape,
                    Shape::First | Shape::Push | Shape::ForwardOrPush | Shape::Other
                )
            }
            Request::Reload => shape == Shape::Replace,
            Request::Traverse(delta) => {
                matches!(shape, Shape::Traverse | Shape::ForwardOrPush) && i64::from(delta) == moved
            }
        };
        let evidence_of_a_new_document = started || self.loading_started;
        let attributed = self.pending.iter().rposition(|&(_, request)| {
            fits(request) && (matches!(request, Request::Traverse(_)) || evidence_of_a_new_document)
        });
        let request = attributed.map(|index| self.pending[index]);
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
        self.entries = entries;
        self.current = current;
        self.reported = true;
        if new_document || self.document.is_none() {
            self.started = false;
            self.document = Some(DocumentGeneration::next());
        }
        (request.is_some() || new_document || url_changed).then(|| Commit {
            navigation: request.map_or_else(NavigationId::next, |(id, _)| id),
            url: self.entries[current].clone(),
            document: self.document.expect("set above"),
        })
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
            .history_changed(urls(entries), current, false)
            .expect("a commit");
        n.load_status(false, false);
        commit
    }

    #[test]
    fn the_browsers_navigation_commits_with_its_id_and_a_new_document() {
        let mut n = Navigator::default();
        let first = NavigationId::next();
        n.request(first, Request::Navigate);
        let commit = load(&mut n, &["https://a/"], 0);
        assert_eq!(commit.navigation, first);
        assert_eq!(commit.url, "https://a/");
        let a = commit.document;

        let second = NavigationId::next();
        n.request(second, Request::Navigate);
        let commit = load(&mut n, &["https://a/", "https://b/"], 1);
        assert_eq!(commit.navigation, second);
        assert_ne!(commit.document, a);
    }

    #[test]
    fn the_first_document_commits_for_the_browser_without_its_own_started() {
        let mut n = Navigator::default();
        let first = NavigationId::next();
        n.request(first, Request::Navigate);
        // Servo's new WebView starts `Started` and suppresses the first document's report.
        n.load_status(true, true);
        let commit = n.history_changed(urls(&["https://a/"]), 0, false).unwrap();
        assert_eq!(commit.navigation, first);
    }

    #[test]
    fn same_document_entries_keep_the_document_and_back_stays_in_it() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let doc = load(&mut n, &["https://a/"], 0).document;

        // A fragment and pushState: new entries, same document, the page's own ids.
        let hash = n
            .history_changed(urls(&["https://a/", "https://a/#x"]), 1, false)
            .unwrap();
        assert_eq!(hash.document, doc);
        let push = n
            .history_changed(
                urls(&["https://a/", "https://a/#x", "https://a/p"]),
                2,
                false,
            )
            .unwrap();
        assert_eq!(push.document, doc);
        assert_ne!(hash.navigation, push.navigation);

        // replaceState: same entry, same document.
        let replaced = n
            .history_changed(
                urls(&["https://a/", "https://a/#x", "https://a/q"]),
                2,
                false,
            )
            .unwrap();
        assert_eq!(replaced.document, doc);

        // The browser goes back twice: traversals within the document.
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let commit = n
            .history_changed(
                urls(&["https://a/", "https://a/#x", "https://a/q"]),
                1,
                false,
            )
            .unwrap();
        assert_eq!((commit.navigation, commit.document), (back, doc));
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let commit = n
            .history_changed(
                urls(&["https://a/", "https://a/#x", "https://a/q"]),
                0,
                false,
            )
            .unwrap();
        assert_eq!((commit.navigation, commit.document), (back, doc));
        // And forward again, to the last entry: a traversal, not a new entry.
        let forward = NavigationId::next();
        n.request(forward, Request::Traverse(2));
        let commit = n
            .history_changed(
                urls(&["https://a/", "https://a/#x", "https://a/q"]),
                2,
                false,
            )
            .unwrap();
        assert_eq!((commit.navigation, commit.document), (forward, doc));
    }

    #[test]
    fn traversing_to_another_document_is_a_new_generation_even_when_kept_alive() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let a = load(&mut n, &["https://a/"], 0).document;
        let a_hash = n
            .history_changed(urls(&["https://a/", "https://a/#x"]), 1, false)
            .unwrap();
        assert_eq!(a_hash.document, a);
        n.request(NavigationId::next(), Request::Navigate);
        let b = load(&mut n, &["https://a/", "https://a/#x", "https://b/"], 2).document;

        // Back to a#x: Servo reactivates a's document without loading it (no `Started`).
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let entries = ["https://a/", "https://a/#x", "https://b/"];
        let commit = n.history_changed(urls(&entries), 1, false).unwrap();
        assert_eq!(commit.navigation, back);
        assert_ne!(
            commit.document, a,
            "a document shown again gets a new generation"
        );
        assert_ne!(commit.document, b);
        let a_again = commit.document;
        // Within a again: same document.
        n.request(NavigationId::next(), Request::Traverse(-1));
        let commit = n.history_changed(urls(&entries), 0, false).unwrap();
        assert_eq!(commit.document, a_again);
        // Forward by two to b, reloaded because Servo had evicted it: `Started` arrives in the
        // same batch, after the history report.
        let forward = NavigationId::next();
        n.request(forward, Request::Traverse(2));
        let commit = n.history_changed(urls(&entries), 2, true).unwrap();
        assert_eq!(commit.navigation, forward);
        assert_ne!(commit.document, a_again);
        assert_ne!(commit.document, b);
    }

    #[test]
    fn a_reload_is_a_new_document_in_the_same_entry() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let doc = load(&mut n, &["https://a/"], 0).document;
        let reload = NavigationId::next();
        n.request(reload, Request::Reload);
        // Servo suppresses the new document's `Started`: only the history change comes.
        let commit = n.history_changed(urls(&["https://a/"]), 0, false).unwrap();
        assert_eq!(commit.navigation, reload);
        assert_ne!(commit.document, doc);
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
        let link = n
            .history_changed(urls(&["https://a/", "https://b/"]), 1, false)
            .unwrap();
        // Servo can't say which: the pending request takes it, as the API allows.
        assert_eq!(link.navigation, to_c);
        assert_ne!(link.document, a);
        n.load_status(false, false);
        // A later link, with nothing pending, is the page's own.
        n.load_status(true, false);
        let own = n
            .history_changed(urls(&["https://a/", "https://b/", "https://d/"]), 2, false)
            .unwrap();
        assert_ne!(own.navigation, to_c);
        assert_ne!(own.document, link.document);
    }

    #[test]
    fn a_same_document_commit_while_the_browser_navigates_isnt_taken_for_it() {
        let mut n = Navigator::default();
        n.request(NavigationId::next(), Request::Navigate);
        let a = load(&mut n, &["https://a/"], 0).document;
        let to_b = NavigationId::next();
        n.request(to_b, Request::Navigate);
        // The current page pushes a fragment before b's response arrives.
        let hash = n
            .history_changed(urls(&["https://a/", "https://a/#x"]), 1, false)
            .unwrap();
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
        assert_eq!(
            n.history_changed(urls(&["https://a/", "https://a/"]), 1, false),
            None
        );
        assert!(n.can_traverse(-1));
        // The browser's back, within the iframe, still commits for its request.
        let back = NavigationId::next();
        n.request(back, Request::Traverse(-1));
        let commit = n.history_changed(urls(&["https://a/", "https://a/"]), 0, false);
        assert_eq!(commit.map(|c| c.navigation), Some(back));
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
        let commit = n
            .history_changed(urls(&["https://x/", "https://y/", "https://z/"]), 0, false)
            .unwrap();
        assert_ne!(commit.document, a);
        assert_eq!(n.url(), Some("https://x/"));
    }
}
