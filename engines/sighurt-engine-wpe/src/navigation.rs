//! One view's navigations, documents and session history, from what WebKit reports.
//!
//! WebKit's back-forward list is the truth about the view's history: [`Snapshot`]s of it, taken
//! inside WebKit's signals, give every commit its entries and its effect ([`effect`]): the same
//! current entry is a `Replace` (a reload, `replaceState`, `location.replace`, loading the URL
//! shown, which WebKit doesn't add again), an entry that was already in the list a `Traverse`,
//! and a new entry right after the old current one a `Push`. Entries are compared by identity
//! (WebKit's item objects), never by URL.
//!
//! WebKit's GLib API gives navigations no ids, so they are attributed as the api allows
//! (`sighurt_api::events`, "Navigations"): the browser's latest request that WebKit hasn't
//! started yet takes the next load WebKit starts, and any other load gets an id of its own. The
//! sequences come from WebKit 2.52 in probe runs (docs/rewrite/WPE.md, "What the engine adds to
//! the prototype"):
//!
//! - A document load is `load-changed` STARTED, then COMMITTED, or `load-failed`. The browser's
//!   request shows itself at once (`is-loading` and the `uri` of the pending request); the
//!   page's own load is first seen at STARTED. Back-forward entries change before the commit.
//! - A same-document navigation of the page's own (a fragment, `pushState`, `replaceState`,
//!   `history.back()` within the document) changes the list, then `uri`, with no load: a `uri`
//!   change while nothing loads is that commit.
//! - The browser's request that turns out to be same-document (a fragment of the page shown,
//!   back to an entry of the same document) starts no load and changes no `uri` (the pending
//!   request already had it): a new entry, or WebKit's `is-loading` going false after the list
//!   moved, ends it as a same-document commit.
//!
//! Limits: a load the page starts in the moment between the browser's request and WebKit
//! starting it takes the browser's id; the browser's same-document traversal while the page is
//! still loading resources commits when that loading ends; a navigation to the very address shown
//! with its fragment changes nothing in WebKit and stays pending until the next.
//!
//! Must not own: WebKit calls (the runtime makes them) or browser policy.

use sighurt_api::events::HistoryEffect;
use sighurt_api::ids::{DocumentGeneration, NavigationId};

/// WebKit's back-forward list at one moment: its entries' identities (`T`) and addresses, and
/// which is current.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot<T> {
    pub items: Vec<T>,
    pub urls: Vec<String>,
    pub current: Option<usize>,
}

impl<T> Default for Snapshot<T> {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            urls: Vec::new(),
            current: None,
        }
    }
}

impl<T> Snapshot<T> {
    fn current(&self) -> Option<(&T, &str)> {
        let index = self.current?;
        Some((self.items.get(index)?, self.urls.get(index)?))
    }
}

/// A navigation that committed.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub navigation: NavigationId,
    pub url: String,
    pub document: DocumentGeneration,
    /// Whether `document` is new: a document load, not a same-document navigation.
    pub new_document: bool,
    pub history: Option<HistoryEffect>,
    /// The session history after it, for `HistoryChanged`; `None` while WebKit's list is empty.
    pub entries: Option<(Vec<String>, usize)>,
}

/// The browser's request WebKit hasn't started a load for.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Sent {
    navigation: NavigationId,
    /// WebKit moved the list's current entry for it (a traversal), before any load.
    moved: bool,
}

/// One view's navigation state. See the module documentation.
#[derive(Debug)]
pub struct Navigator<T> {
    sent: Option<Sent>,
    /// The document load in progress: STARTED, not yet committed or failed.
    loading: Option<NavigationId>,
    document: Option<DocumentGeneration>,
    /// The list at the last commit.
    history: Snapshot<T>,
    url: Option<String>,
    /// The web process is gone: what WebKit says of the address means nothing until a load.
    gone: bool,
}

impl<T> Default for Navigator<T> {
    fn default() -> Self {
        Self {
            sent: None,
            loading: None,
            document: None,
            history: Snapshot::default(),
            url: None,
            gone: false,
        }
    }
}

impl<T: Clone + PartialEq> Navigator<T> {
    /// The document shown, once one has committed.
    pub fn document(&self) -> Option<DocumentGeneration> {
        self.document
    }

    /// The committed address.
    pub fn url(&self) -> Option<&str> {
        self.url.as_deref()
    }

    /// The browser asks WebKit for `navigation`. A request WebKit hasn't started yet is
    /// superseded, as the api's ordering rules let the core see.
    pub fn request(&mut self, navigation: NavigationId) {
        self.sent = Some(Sent {
            navigation,
            moved: false,
        });
    }

    /// The browser's request will never start: refused before it reached WebKit's loader.
    pub fn refused(&mut self) -> Option<NavigationId> {
        self.sent.take().map(|s| s.navigation)
    }

    /// WebKit started a document load (STARTED): the browser's request, if one waits, else the
    /// page's own. Returns its id and whether it is the browser's.
    pub fn started(&mut self) -> (NavigationId, bool) {
        let (navigation, browsers) = match self.sent.take() {
            Some(sent) => (sent.navigation, true),
            None => (NavigationId::next(), false),
        };
        self.loading = Some(navigation);
        (navigation, browsers)
    }

    /// The document load committed a new document at `url`, with WebKit's list as `history`.
    pub fn committed(&mut self, url: String, history: Snapshot<T>) -> Commit {
        let navigation = match (self.loading.take(), self.sent.take()) {
            (Some(navigation), _) => navigation,
            (None, Some(sent)) => sent.navigation,
            (None, None) => NavigationId::next(),
        };
        self.document = Some(DocumentGeneration::next());
        self.gone = false;
        Commit {
            new_document: true,
            ..self.commit(navigation, url, history)
        }
    }

    /// The document load failed before it committed: its id, if one was loading.
    pub fn failed(&mut self) -> Option<NavigationId> {
        self.loading.take()
    }

    /// WebKit's list changed. A new entry while the browser's request has started no load is
    /// that request committing within the document; an old entry becoming current is a
    /// traversal that may still start a load.
    pub fn list_changed(&mut self, added: bool, history: Snapshot<T>) -> Option<Commit> {
        if self.loading.is_some() || self.gone {
            return None;
        }
        let sent = self.sent.as_mut()?;
        if !added {
            sent.moved = true;
            return None;
        }
        let navigation = sent.navigation;
        self.sent = None;
        let url = history.current()?.1.to_owned();
        Some(self.commit(navigation, url, history))
    }

    /// WebKit's `uri` changed to `url`. While nothing loads and the browser waits for nothing,
    /// that is the page's own same-document navigation.
    pub fn url_changed(&mut self, url: String, history: Snapshot<T>) -> Option<Commit> {
        if self.loading.is_some() || self.sent.is_some() || self.gone {
            return None;
        }
        if url.is_empty() || self.url.as_deref() == Some(&url) {
            return None;
        }
        self.document?;
        Some(self.commit(NavigationId::next(), url, history))
    }

    /// WebKit stopped loading, with `url` and `history` now. If WebKit moved its list for the
    /// browser's request and started no load, the request was a traversal within the document,
    /// and this is its commit. Otherwise the request waits on: WebKit drops its marker of a
    /// pending request when an earlier one is abandoned (two requests in a row) before the later
    /// one shows itself.
    pub fn loading_stopped(&mut self, url: String, history: Snapshot<T>) -> Option<Commit> {
        if self.loading.is_some()
            || !self.sent.is_some_and(|s| s.moved)
            || self.document.is_none()
            || self.gone
        {
            return None;
        }
        let sent = self.sent.take()?;
        Some(self.commit(sent.navigation, url, history))
    }

    /// The web process is gone: the navigations in flight end with it, and until a document
    /// loads, WebKit's address and list say nothing about the view.
    pub fn crashed(&mut self) -> Vec<NavigationId> {
        self.gone = true;
        let sent = self.sent.take().map(|s| s.navigation);
        self.loading.take().into_iter().chain(sent).collect()
    }

    fn commit(&mut self, navigation: NavigationId, url: String, history: Snapshot<T>) -> Commit {
        let effect = effect(&self.history, &history);
        let entries = history.current.map(|current| {
            let mut urls = history.urls.clone();
            // The commit's own address is WebKit's word for the current entry.
            if let Some(entry) = urls.get_mut(current) {
                entry.clone_from(&url);
            }
            (urls, current)
        });
        self.url = Some(url.clone());
        self.history = history;
        Commit {
            navigation,
            url,
            document: self.document.expect("a commit follows a document"),
            new_document: false,
            history: effect,
            entries,
        }
    }
}

/// What the move from `before` to `after` did to the session history; `None` when `after` has
/// no current entry.
pub fn effect<T: PartialEq>(before: &Snapshot<T>, after: &Snapshot<T>) -> Option<HistoryEffect> {
    let index = after.current?;
    let now = after.items.get(index)?;
    let Some((old, _)) = before.current() else {
        return Some(HistoryEffect::Push);
    };
    Some(if old == now {
        HistoryEffect::Replace
    } else if before.items.contains(now) {
        HistoryEffect::Traverse
    } else if index.checked_sub(1).and_then(|i| after.items.get(i)) == Some(old) {
        HistoryEffect::Push
    } else {
        // A new entry in the old one's place.
        HistoryEffect::Replace
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list of entries named by number, `current` marked.
    fn list(items: &[u32], current: usize) -> Snapshot<u32> {
        Snapshot {
            items: items.to_vec(),
            urls: items
                .iter()
                .map(|i| format!("https://a.test/{i}"))
                .collect(),
            current: Some(current),
        }
    }

    fn url(item: u32) -> String {
        format!("https://a.test/{item}")
    }

    fn loaded() -> Navigator<u32> {
        let mut nav = Navigator::default();
        nav.started();
        nav.committed(url(1), list(&[1], 0));
        nav
    }

    #[test]
    fn effects_follow_entry_identity_not_addresses() {
        let one = list(&[1, 2, 3], 1);
        assert_eq!(effect(&list(&[], 0), &one), Some(HistoryEffect::Push));
        // A new entry after the old current one, the forward entries gone.
        assert_eq!(
            effect(&one, &list(&[1, 2, 4], 2)),
            Some(HistoryEffect::Push)
        );
        // The same entry: a reload, replaceState, the URL shown loaded again.
        assert_eq!(effect(&one, &one), Some(HistoryEffect::Replace));
        // Another entry that was there: back or forward, however far.
        assert_eq!(
            effect(&one, &list(&[1, 2, 3], 0)),
            Some(HistoryEffect::Traverse)
        );
        assert_eq!(
            effect(&one, &list(&[1, 2, 3], 2)),
            Some(HistoryEffect::Traverse)
        );
        // A new entry in place of the current one.
        assert_eq!(
            effect(&one, &list(&[1, 5, 3], 1)),
            Some(HistoryEffect::Replace)
        );
        // The oldest entry dropped at WebKit's limit while pushing.
        assert_eq!(effect(&one, &list(&[2, 6], 1)), Some(HistoryEffect::Push));
        // Two entries with the same address are still two entries.
        let same = Snapshot {
            items: vec![1, 2],
            urls: vec![url(1), url(1)],
            current: Some(1),
        };
        assert_eq!(effect(&list(&[1], 0), &same), Some(HistoryEffect::Push));
        assert_eq!(
            effect(&same, &list(&[1, 2], 0)),
            Some(HistoryEffect::Traverse)
        );
    }

    #[test]
    fn the_browsers_request_takes_the_next_load_and_the_page_gets_its_own_ids() {
        let mut nav = loaded();
        let first = nav.document().unwrap();
        let asked = NavigationId::next();
        nav.request(asked);
        assert_eq!(nav.started(), (asked, true));
        let commit = nav.committed(url(2), list(&[1, 2], 1));
        assert_eq!(commit.navigation, asked);
        assert_eq!(commit.history, Some(HistoryEffect::Push));
        assert_ne!(commit.document, first);
        assert_eq!(commit.entries, Some((vec![url(1), url(2)], 1)));

        // A link the page follows.
        let (own, browsers) = nav.started();
        assert!(!browsers && own != asked);
        assert_eq!(nav.committed(url(3), list(&[1, 2, 3], 2)).navigation, own);

        // A newer request supersedes one WebKit hasn't started.
        let (old, new) = (NavigationId::next(), NavigationId::next());
        nav.request(old);
        nav.request(new);
        assert_eq!(nav.started().0, new);
        assert_eq!(nav.failed(), Some(new));
        assert_eq!(nav.failed(), None);
    }

    #[test]
    fn same_document_navigations_of_the_page_keep_the_document() {
        let mut nav = loaded();
        let doc = nav.document();
        // pushState: the list grows, then the address changes.
        let push = nav
            .url_changed(format!("{}#a", url(1)), list(&[1, 2], 1))
            .unwrap();
        assert_eq!(
            (push.document, push.history),
            (doc.unwrap(), Some(HistoryEffect::Push))
        );
        // replaceState: the same entry, a new address.
        let mut replaced = list(&[1, 2], 1);
        replaced.urls[1] = format!("{}#b", url(1));
        let replace = nav.url_changed(replaced.urls[1].clone(), replaced).unwrap();
        assert_eq!(replace.history, Some(HistoryEffect::Replace));
        // history.back() within the document.
        let back = nav.url_changed(url(1), list(&[1, 2], 0)).unwrap();
        assert_eq!(back.history, Some(HistoryEffect::Traverse));
        assert_ne!(back.navigation, push.navigation);
        // The same address again is no navigation.
        assert_eq!(nav.url_changed(url(1), list(&[1, 2], 0)), None);
        // While a document loads, `uri` is the load's, not a commit.
        nav.started();
        assert_eq!(nav.url_changed(url(9), list(&[1, 2], 0)), None);
    }

    #[test]
    fn the_browsers_same_document_requests_commit_without_a_load() {
        // A fragment of the page shown: WebKit adds the entry and starts no load.
        let mut nav = loaded();
        let fragment = NavigationId::next();
        nav.request(fragment);
        // The pending request's `uri` is not a commit.
        assert_eq!(
            nav.url_changed(format!("{}#x", url(1)), list(&[1], 0)),
            None
        );
        let mut added = list(&[1, 2], 1);
        added.urls[1] = format!("{}#x", url(1));
        let commit = nav.list_changed(true, added).unwrap();
        assert_eq!(commit.navigation, fragment);
        assert_eq!(commit.history, Some(HistoryEffect::Push));
        assert_eq!(commit.url, format!("{}#x", url(1)));
        assert_eq!(
            nav.loading_stopped(commit.url.clone(), list(&[1, 2], 1)),
            None
        );

        // Back to an entry of the same document: the list moves, then loading stops.
        let back = NavigationId::next();
        nav.request(back);
        assert_eq!(nav.list_changed(false, list(&[1, 2], 0)), None);
        let commit = nav
            .loading_stopped(url(1), list(&[1, 2], 0))
            .expect("the traversal commits");
        assert_eq!(
            (commit.navigation, commit.history),
            (back, Some(HistoryEffect::Traverse))
        );

        // Back to another document: the list moves, then the load starts and commits.
        let mut nav = loaded();
        nav.started();
        nav.committed(url(2), list(&[1, 2], 1));
        let back = NavigationId::next();
        nav.request(back);
        assert_eq!(nav.list_changed(false, list(&[1, 2], 0)), None);
        assert_eq!(nav.started(), (back, true));
        let commit = nav.committed(url(1), list(&[1, 2], 0));
        assert_eq!(commit.history, Some(HistoryEffect::Traverse));
    }

    /// Two requests in a row, the second a fragment of the page shown (WebKit 2.52): WebKit
    /// abandons the first, drops its marker of a pending request (`is-loading` false, `uri` back
    /// to the page's) and then carries out the second within the document.
    #[test]
    fn the_later_of_two_requests_keeps_its_id_through_webkits_dropped_marker() {
        let mut nav = loaded();
        let (first, second) = (NavigationId::next(), NavigationId::next());
        nav.request(first);
        assert_eq!(nav.url_changed(url(9), list(&[1], 0)), None);
        nav.request(second);
        assert_eq!(
            nav.url_changed(format!("{}#x", url(1)), list(&[1], 0)),
            None
        );
        assert_eq!(nav.loading_stopped(url(1), list(&[1], 0)), None);
        assert_eq!(nav.url_changed(url(1), list(&[1], 0)), None);
        let mut added = list(&[1, 2], 1);
        added.urls[1] = format!("{}#x", url(1));
        let commit = nav
            .list_changed(true, added.clone())
            .expect("the fragment commits");
        assert_eq!(commit.navigation, second);
        assert_eq!(nav.url_changed(format!("{}#x", url(1)), added), None);
    }

    #[test]
    fn a_crash_ends_what_was_in_flight() {
        let mut nav = loaded();
        nav.started();
        let asked = NavigationId::next();
        nav.request(asked);
        assert_eq!(nav.crashed().len(), 2);
        assert!(nav.crashed().is_empty());
        // The document stays known: a reload loads it again. WebKit's address empties meanwhile,
        // which is no navigation.
        assert!(nav.document().is_some());
        assert_eq!(nav.url_changed(String::new(), list(&[1], 0)), None);
        assert_eq!(nav.url_changed(url(5), list(&[5], 0)), None);
        let reload = NavigationId::next();
        nav.request(reload);
        nav.started();
        assert_eq!(nav.committed(url(1), list(&[1], 0)).navigation, reload);
        assert!(nav
            .url_changed(format!("{}#a", url(1)), list(&[1, 2], 1))
            .is_some());
    }
}
