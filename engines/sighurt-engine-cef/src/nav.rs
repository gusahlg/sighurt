//! A view's navigation state: what CEF's load callbacks mean as the contract's navigation
//! events (ids, documents, history effects, held navigations). Pure: no CEF object is touched,
//! so it is unit-tested without the library.
//!
//! CEF reports a main-frame navigation in this order: `OnBeforeBrowse` (before the request,
//! for the browser's loads and the page's own), `OnLoadingStateChange`, `OnLoadStart` (the
//! commit), `OnAddressChange`, `OnLoadEnd`; a failure is `OnLoadError`; a same-document
//! navigation (fragment, `pushState`) is only `OnAddressChange`. The history list comes from the
//! navigation entries, which the caller reads when a commit happens and passes in.
//!
//! Must not own: any CEF object, any lock, the event queue.

use std::collections::HashMap;

use sighurt_api::events::{EngineEvent, HistoryEffect, NavigationError};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, ViewId};

/// `TT_SOURCE_MASK`, `TT_RELOAD`, `TT_LINK`, `TT_FORM_SUBMIT` and `TT_FORWARD_BACK_FLAG` of
/// `cef_transition_type_t`.
const TT_SOURCE_MASK: i32 = 0xFF;
const TT_LINK: i32 = 0;
const TT_FORM_SUBMIT: i32 = 7;
const TT_RELOAD: i32 = 8;
const TT_FORWARD_BACK_FLAG: i32 = 0x0100_0000;

/// `ERR_ABORTED`: a stopped or superseded load.
pub const ERR_ABORTED: i32 = -3;

/// The most navigations held per view (the engine contract).
pub const MAX_HELD: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Navigate,
    Reload,
    Traverse,
}

#[derive(Clone, Debug)]
struct Pending {
    id: NavigationId,
    kind: Kind,
}

/// What `OnBeforeBrowse` answers.
#[derive(Debug, PartialEq)]
pub enum Browse {
    /// Let CEF load it.
    Go,
    /// Cancel it: the browser was asked (`NavigationRequested` is in the events).
    Hold,
}

/// One view's navigation state.
#[derive(Debug)]
pub struct Nav {
    view: ViewId,
    pending: Option<Pending>,
    started: Option<NavigationId>,
    started_kind: Kind,
    document: Option<DocumentGeneration>,
    url: String,
    loading: bool,
    entries: Vec<String>,
    current: usize,
    held: HashMap<RequestId, String>,
    allowed: Vec<String>,
}

impl Nav {
    /// A view with no document yet.
    pub fn new(view: ViewId) -> Self {
        Self {
            view,
            pending: None,
            started: None,
            started_kind: Kind::Navigate,
            document: None,
            url: String::new(),
            loading: false,
            entries: Vec::new(),
            current: 0,
            held: HashMap::new(),
            allowed: Vec::new(),
        }
    }

    /// The document being shown, for requests made on its behalf.
    pub fn document(&self) -> Option<DocumentGeneration> {
        self.document
    }

    /// The committed address.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The browser asks to load `url`.
    pub fn request_navigate(&mut self, id: NavigationId) {
        self.pending = Some(Pending {
            id,
            kind: Kind::Navigate,
        });
    }

    /// The browser asks to reload.
    pub fn request_reload(&mut self, id: NavigationId) {
        self.pending = Some(Pending {
            id,
            kind: Kind::Reload,
        });
    }

    /// The browser asks to move `delta` entries. `Ok(steps)` is how many single back (negative)
    /// or forward steps to take; `Err` is the event for a move beyond the list.
    pub fn request_traverse(&mut self, id: NavigationId, delta: i32) -> Result<i32, EngineEvent> {
        let target = i64::try_from(self.current).unwrap_or(0) + i64::from(delta);
        if delta == 0 || target < 0 || target >= i64::try_from(self.entries.len()).unwrap_or(0) {
            return Err(EngineEvent::NavigationFailed {
                view: self.view,
                navigation: id,
                error: NavigationError::Cancelled,
            });
        }
        self.pending = Some(Pending {
            id,
            kind: Kind::Traverse,
        });
        Ok(delta)
    }

    /// `OnBeforeBrowse` of the main frame. A user's click on a link or form the browser did
    /// not ask for is held and asked about; everything else goes ahead.
    pub fn before_browse(
        &mut self,
        url: &str,
        user_gesture: bool,
        redirect: bool,
        transition: i32,
        events: &mut Vec<EngineEvent>,
    ) -> Browse {
        if redirect {
            return Browse::Go;
        }
        let allowed = self.allowed.iter().position(|allowed| allowed == url);
        if let Some(index) = allowed {
            self.allowed.swap_remove(index);
        } else if self.pending.is_none() {
            let source = transition & TT_SOURCE_MASK;
            let clicked = user_gesture && matches!(source, TT_LINK | TT_FORM_SUBMIT);
            if let (true, Some(document)) = (clicked, self.document) {
                if self.held.len() < MAX_HELD {
                    let request = RequestId::next();
                    self.held.insert(request, url.to_owned());
                    events.push(EngineEvent::NavigationRequested {
                        view: self.view,
                        document,
                        request,
                        url: url.to_owned(),
                    });
                    return Browse::Hold;
                }
            }
        }
        let (id, kind) = self.pending.take().map_or_else(
            || (NavigationId::next(), Kind::Navigate),
            |p| (p.id, p.kind),
        );
        // Kept for the commit, which needs the kind.
        self.started = Some(id);
        self.started_kind = kind;
        events.push(EngineEvent::NavigationStarted {
            view: self.view,
            navigation: id,
            url: url.to_owned(),
        });
        Browse::Go
    }

    /// The browser's answer to a held navigation: the address to load, if allowed.
    pub fn answer(&mut self, request: RequestId, allow: bool) -> Option<String> {
        let url = self.held.remove(&request)?;
        if allow {
            self.allowed.push(url.clone());
            Some(url)
        } else {
            None
        }
    }

    /// `OnLoadStart` of the main frame: the new document committed.
    pub fn commit(
        &mut self,
        url: &str,
        transition: i32,
        entries: Vec<String>,
        current: usize,
        events: &mut Vec<EngineEvent>,
    ) {
        // CEF shows its own page for a failed load: not a navigation of ours, the error was
        // already reported.
        if url.starts_with("chrome-error:") {
            return;
        }
        // The blank page a browser starts with is not a navigation anyone asked for.
        if url == "about:blank"
            && self.document.is_none()
            && self.started.is_none()
            && self.pending.is_none()
        {
            return;
        }
        let navigation = self
            .started
            .take()
            .or_else(|| self.pending.take().map(|pending| pending.id))
            .unwrap_or_else(NavigationId::next);
        let effect = if transition & TT_FORWARD_BACK_FLAG != 0 {
            HistoryEffect::Traverse
        } else if transition & TT_SOURCE_MASK == TT_RELOAD || self.started_kind == Kind::Reload {
            HistoryEffect::Replace
        } else if self.started_kind == Kind::Traverse {
            HistoryEffect::Traverse
        } else if !self.entries.is_empty() && current == self.current {
            HistoryEffect::Replace
        } else {
            HistoryEffect::Push
        };
        self.started_kind = Kind::Navigate;
        let document = DocumentGeneration::next();
        self.document = Some(document);
        self.url = url.to_owned();
        self.held.clear();
        events.push(EngineEvent::NavigationCommitted {
            view: self.view,
            navigation,
            url: url.to_owned(),
            document,
            history: Some(effect),
        });
        self.set_history(entries, current, events);
    }

    /// `OnAddressChange` of the main frame. While nothing loads it is the page changing its own
    /// address inside the document (a fragment, `pushState`, `history.back()`).
    pub fn address_changed(
        &mut self,
        url: &str,
        entries: Vec<String>,
        current: usize,
        events: &mut Vec<EngineEvent>,
    ) {
        if self.loading || url == self.url || url.starts_with("chrome-error:") {
            return;
        }
        let Some(document) = self.document else {
            return;
        };
        let effect = match current.cmp(&self.current) {
            std::cmp::Ordering::Less => HistoryEffect::Traverse,
            std::cmp::Ordering::Equal => HistoryEffect::Replace,
            std::cmp::Ordering::Greater if entries.len() > self.entries.len() => {
                HistoryEffect::Push
            }
            std::cmp::Ordering::Greater => HistoryEffect::Traverse,
        };
        let navigation = NavigationId::next();
        self.url = url.to_owned();
        events.push(EngineEvent::NavigationStarted {
            view: self.view,
            navigation,
            url: url.to_owned(),
        });
        events.push(EngineEvent::NavigationCommitted {
            view: self.view,
            navigation,
            url: url.to_owned(),
            document,
            history: Some(effect),
        });
        self.set_history(entries, current, events);
    }

    fn set_history(&mut self, entries: Vec<String>, current: usize, events: &mut Vec<EngineEvent>) {
        self.entries = entries;
        self.current = current;
        events.push(EngineEvent::HistoryChanged {
            view: self.view,
            entries: self.entries.clone(),
            current,
        });
    }

    /// `OnLoadError` of the main frame.
    pub fn failed(&mut self, code: i32, text: &str, events: &mut Vec<EngineEvent>) {
        let navigation = self
            .started
            .take()
            .or_else(|| self.pending.take().map(|pending| pending.id))
            .unwrap_or_else(NavigationId::next);
        let error = if code == ERR_ABORTED {
            NavigationError::Cancelled
        } else if text.is_empty() {
            NavigationError::Load(format!("the page failed to load (error {code})"))
        } else {
            NavigationError::Load(text.to_owned())
        };
        events.push(EngineEvent::NavigationFailed {
            view: self.view,
            navigation,
            error,
        });
    }

    /// `OnLoadingStateChange`.
    pub fn loading_changed(&mut self, loading: bool, events: &mut Vec<EngineEvent>) {
        if loading != self.loading {
            self.loading = loading;
            events.push(EngineEvent::LoadingChanged {
                view: self.view,
                loading,
            });
        }
        // A navigation that started and neither committed nor failed when loading stopped was
        // stopped.
        if !loading {
            if let Some(navigation) = self.started.take() {
                events.push(EngineEvent::NavigationFailed {
                    view: self.view,
                    navigation,
                    error: NavigationError::Cancelled,
                });
            }
        }
    }

    /// The document's content process died: nothing of it is left to ask on behalf of.
    pub fn crashed(&mut self) {
        self.started = None;
        self.held.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nav() -> Nav {
        Nav::new(ViewId::next())
    }

    fn load(nav: &mut Nav, url: &str, entries: &[&str], current: usize) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        nav.loading_changed(true, &mut events);
        nav.before_browse(url, false, false, 1, &mut events);
        nav.commit(
            url,
            1,
            entries.iter().map(|e| (*e).to_owned()).collect(),
            current,
            &mut events,
        );
        nav.loading_changed(false, &mut events);
        events
    }

    #[test]
    fn the_browsers_navigation_keeps_its_id_from_start_to_commit() {
        let mut nav = nav();
        let id = NavigationId::next();
        nav.request_navigate(id);
        let events = load(&mut nav, "https://a/", &["https://a/"], 0);
        assert!(matches!(
            &events[1],
            EngineEvent::NavigationStarted { navigation, .. } if *navigation == id
        ));
        assert!(matches!(
            &events[2],
            EngineEvent::NavigationCommitted { navigation, history: Some(HistoryEffect::Push), .. }
                if *navigation == id
        ));
        assert!(matches!(
            &events[3],
            EngineEvent::HistoryChanged { current: 0, .. }
        ));
        assert!(nav.document().is_some());
    }

    #[test]
    fn a_users_click_is_held_and_answered() {
        let mut nav = nav();
        load(&mut nav, "https://a/", &["https://a/"], 0);
        let mut events = Vec::new();
        assert_eq!(
            nav.before_browse("https://b/", true, false, TT_LINK, &mut events),
            Browse::Hold
        );
        let EngineEvent::NavigationRequested { request, url, .. } = events.remove(0) else {
            panic!("asked");
        };
        assert_eq!(url, "https://b/");
        assert_eq!(nav.answer(request, true).as_deref(), Some("https://b/"));
        // The reissued load goes ahead without asking again, as a page-started navigation.
        assert_eq!(
            nav.before_browse("https://b/", true, false, TT_LINK, &mut events),
            Browse::Go
        );
        assert!(matches!(events[0], EngineEvent::NavigationStarted { .. }));
        // A denial forgets it, and a late answer does nothing.
        let mut events = Vec::new();
        nav.before_browse("https://c/", true, false, TT_FORM_SUBMIT, &mut events);
        let EngineEvent::NavigationRequested { request, .. } = events.remove(0) else {
            panic!("asked");
        };
        assert_eq!(nav.answer(request, false), None);
        assert_eq!(nav.answer(request, true), None);
    }

    #[test]
    fn what_the_browser_asked_for_is_never_held() {
        let mut nav = nav();
        load(&mut nav, "https://a/", &["https://a/"], 0);
        nav.request_navigate(NavigationId::next());
        let mut events = Vec::new();
        assert_eq!(
            nav.before_browse("https://b/", true, false, TT_LINK, &mut events),
            Browse::Go
        );
    }

    #[test]
    fn at_most_sixteen_are_held() {
        let mut nav = nav();
        load(&mut nav, "https://a/", &["https://a/"], 0);
        let mut events = Vec::new();
        for n in 0..MAX_HELD {
            let url = format!("https://b/{n}");
            assert_eq!(
                nav.before_browse(&url, true, false, TT_LINK, &mut events),
                Browse::Hold
            );
        }
        assert_eq!(
            nav.before_browse("https://b/x", true, false, TT_LINK, &mut events),
            Browse::Go
        );
    }

    #[test]
    fn history_effects_follow_the_flags_and_the_entry_index() {
        let mut nav = nav();
        load(&mut nav, "https://a/", &["https://a/"], 0);
        load(&mut nav, "https://b/", &["https://a/", "https://b/"], 1);
        let mut events = Vec::new();
        nav.before_browse(
            "https://a/",
            false,
            false,
            TT_FORWARD_BACK_FLAG | 1,
            &mut events,
        );
        nav.commit(
            "https://a/",
            TT_FORWARD_BACK_FLAG | 1,
            vec!["https://a/".into(), "https://b/".into()],
            0,
            &mut events,
        );
        assert!(matches!(
            events[1],
            EngineEvent::NavigationCommitted {
                history: Some(HistoryEffect::Traverse),
                ..
            }
        ));
        // Same entry again: a reload or a load of the shown address replaces.
        let mut events = Vec::new();
        nav.commit(
            "https://a/",
            1,
            vec!["https://a/".into(), "https://b/".into()],
            0,
            &mut events,
        );
        assert!(matches!(
            events[0],
            EngineEvent::NavigationCommitted {
                history: Some(HistoryEffect::Replace),
                ..
            }
        ));
    }

    #[test]
    fn traversal_beyond_the_list_is_cancelled() {
        let mut nav = nav();
        load(&mut nav, "https://a/", &["https://a/"], 0);
        let id = NavigationId::next();
        assert!(matches!(
            nav.request_traverse(id, -1),
            Err(EngineEvent::NavigationFailed {
                error: NavigationError::Cancelled,
                ..
            })
        ));
        assert!(nav.request_traverse(id, 0).is_err());
    }

    #[test]
    fn a_same_document_change_is_a_navigation_of_the_same_document() {
        let mut nav = nav();
        load(&mut nav, "https://a/", &["https://a/"], 0);
        let document = nav.document();
        let mut events = Vec::new();
        nav.address_changed(
            "https://a/#x",
            vec!["https://a/".into(), "https://a/#x".into()],
            1,
            &mut events,
        );
        assert!(matches!(events[0], EngineEvent::NavigationStarted { .. }));
        match &events[1] {
            EngineEvent::NavigationCommitted {
                document: d,
                history,
                ..
            } => {
                assert_eq!(Some(*d), document);
                assert_eq!(*history, Some(HistoryEffect::Push));
            }
            other => panic!("{other:?}"),
        }
        // While a load is in progress an address change belongs to that load.
        let mut events = Vec::new();
        nav.loading_changed(true, &mut events);
        events.clear();
        nav.address_changed("https://b/", vec![], 0, &mut events);
        assert!(events.is_empty());
    }

    #[test]
    fn failures_and_stops() {
        let mut nav = nav();
        let id = NavigationId::next();
        nav.request_navigate(id);
        let mut events = Vec::new();
        nav.before_browse("https://a/", false, false, 1, &mut events);
        nav.failed(-105, "net::ERR_NAME_NOT_RESOLVED", &mut events);
        assert!(matches!(
            &events[1],
            EngineEvent::NavigationFailed { navigation, error: NavigationError::Load(_), .. }
                if *navigation == id
        ));
        let mut events = Vec::new();
        nav.before_browse("https://a/", false, false, 1, &mut events);
        nav.loading_changed(true, &mut events);
        nav.loading_changed(false, &mut events);
        assert!(matches!(
            events.last(),
            Some(EngineEvent::NavigationFailed {
                error: NavigationError::Cancelled,
                ..
            })
        ));
    }

    #[test]
    fn cefs_error_page_is_not_a_commit() {
        let mut nav = nav();
        let mut events = Vec::new();
        nav.commit("chrome-error://chromewebdata/", 1, vec![], 0, &mut events);
        assert!(events.is_empty());
        assert!(nav.document().is_none());
    }
}
