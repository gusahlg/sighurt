//! Find in page: the view's search as CEF's UI thread sees it ([`FindState`]), the commands that
//! drive Chromium's (`find`, `stop_finding`), and what its `OnFindResult` reports become.
//!
//! CEF numbers each `find` call with an identifier that increases with every call in the
//! process, but doesn't return it; a result names it. So every operation runs as one task on
//! the UI thread, in the order the owner made them, and updates [`FindState`] there; a result,
//! also on the UI thread, is stamped with the search current when it arrives. A new search
//! first stops the last one (`stop_finding`), after which Chromium reports nothing more of it,
//! and a result whose identifier is no higher than the highest seen when the search started is
//! an earlier search's and dropped. A result for a document that is no longer the view's is
//! dropped, and a new document ends the search.
//!
//! Must not own: what a search is for (the core's), or a find bar.

use std::sync::Arc;

use sighurt_api::engine::FindCommand;
use sighurt_api::events::EngineEvent;
use sighurt_api::ids::{DocumentGeneration, RequestId, ViewId};

use crate::capi::CefBrowserHost;
use crate::ffi::{call, post_ui, CefStr, Ref};
use crate::shared::{lock, ViewShared};

/// The search the view's results belong to.
struct Current {
    search: RequestId,
    document: DocumentGeneration,
    text: String,
    case_sensitive: bool,
    /// The highest identifier seen when the search started: results up to it are older.
    floor: Option<i32>,
}

/// The view's search, as of CEF's UI thread.
#[derive(Default)]
pub struct FindState {
    current: Option<Current>,
    /// The highest identifier CEF reported for the view.
    seen: Option<i32>,
}

impl FindState {
    /// `search` replaces the view's search.
    pub fn start(
        &mut self,
        search: RequestId,
        document: DocumentGeneration,
        text: String,
        case_sensitive: bool,
    ) {
        self.current = Some(Current {
            search,
            document,
            text,
            case_sensitive,
            floor: self.seen,
        });
    }

    /// The text and case of `search` if it is the current search: what a move searches again.
    pub fn moving(&self, search: RequestId) -> Option<(String, bool)> {
        let current = self.current.as_ref().filter(|c| c.search == search)?;
        Some((current.text.clone(), current.case_sensitive))
    }

    /// Ends `search` if it is the current one; whether it was.
    pub fn stop(&mut self, search: RequestId) -> bool {
        let current = self.current.as_ref().is_some_and(|c| c.search == search);
        if current {
            self.current = None;
        }
        current
    }

    /// Ends the search: its document is gone.
    pub fn end(&mut self) {
        self.current = None;
    }

    /// What CEF's result `identifier` (`count` matches, the `active` one selected, the last of
    /// its request if `last`) is for `view` while `document` is its document: a
    /// [`FindResult`](EngineEvent::FindResult) of the current search, or nothing when it is an
    /// earlier search's or document's. An update that has found nothing yet is not reported:
    /// only the last says there is no match.
    pub fn result(
        &mut self,
        view: ViewId,
        identifier: i32,
        count: i32,
        active: i32,
        last: bool,
        document: Option<DocumentGeneration>,
    ) -> Option<EngineEvent> {
        self.seen = Some(self.seen.map_or(identifier, |seen| seen.max(identifier)));
        let current = self.current.as_ref()?;
        if current.floor.is_some_and(|floor| identifier <= floor)
            || document != Some(current.document)
            || (!last && count <= 0)
        {
            return None;
        }
        let found = count > 0;
        Some(EngineEvent::FindResult {
            view,
            document: current.document,
            search: current.search,
            found,
            current: u32::try_from(active).ok().filter(|&n| found && n > 0),
            total: u32::try_from(count).ok(),
        })
    }
}

impl ViewShared {
    /// Carries out `command` on CEF's UI thread, after what the owner asked before it.
    pub fn find(self: &Arc<Self>, command: FindCommand) {
        let view = Arc::clone(self);
        post_ui(move || view.find_now(command));
    }

    /// On the UI thread. No lock is held while CEF is called.
    fn find_now(&self, command: FindCommand) {
        if self.is_closing() {
            return;
        }
        let Some(host) = lock(&self.slot).host.clone() else {
            return;
        };
        match command {
            FindCommand::Search {
                search,
                document,
                text,
                case_sensitive,
            } => {
                // A search of a document that is gone is dropped.
                if lock(&self.nav).document() != Some(document) {
                    return;
                }
                stop_finding(&host);
                lock(&self.find).start(search, document, text.clone(), case_sensitive);
                find(&host, &text, true, case_sensitive);
            }
            FindCommand::Next { search } | FindCommand::Previous { search } => {
                let forward = matches!(command, FindCommand::Next { .. });
                // A move of a search the view no longer has does nothing.
                let moving = lock(&self.find).moving(search);
                if let Some((text, case_sensitive)) = moving {
                    find(&host, &text, forward, case_sensitive);
                }
            }
            FindCommand::Stop { search } => {
                if lock(&self.find).stop(search) {
                    stop_finding(&host);
                }
            }
        }
    }
}

/// Searches for `text` from the selected match. `findNext` is always set: Chromium starts a
/// new search by itself when the text or case changed or the last was stopped, and without it
/// CEF 150 selects no match for a new search (no `activeMatchOrdinal`; seen in
/// `tests/offscreen.rs`).
fn find(host: &Ref<CefBrowserHost>, text: &str, forward: bool, case_sensitive: bool) {
    let text = CefStr::new(text);
    let (forward, case_sensitive) = (i32::from(forward), i32::from(case_sensitive));
    // SAFETY: a live host reference, on the UI thread.
    unsafe { call!(host.as_ptr(), find, &text.raw(), forward, case_sensitive, 1) };
}

/// Ends the search and clears its selection and highlights.
fn stop_finding(host: &Ref<CefBrowserHost>) {
    // SAFETY: a live host reference, on the UI thread.
    unsafe { call!(host.as_ptr(), stop_finding, 1) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_are_the_current_searchs_and_never_an_earlier_ones() {
        let view = ViewId::next();
        let (doc, other) = (DocumentGeneration::next(), DocumentGeneration::next());
        let (one, two) = (RequestId::next(), RequestId::next());
        let mut find = FindState::default();
        let result = |search, found, current, total| EngineEvent::FindResult {
            view,
            document: doc,
            search,
            found,
            current,
            total,
        };
        // Nothing searched: nothing reported.
        assert_eq!(find.result(view, 1, 3, 1, true, Some(doc)), None);
        find.start(one, doc, "apple".into(), false);
        // An update that found nothing yet waits; the last says so.
        assert_eq!(find.result(view, 2, 0, 0, false, Some(doc)), None);
        assert_eq!(
            find.result(view, 2, 4, 1, false, Some(doc)),
            Some(result(one, true, Some(1), Some(4)))
        );
        assert_eq!(
            find.result(view, 3, 4, 2, true, Some(doc)),
            Some(result(one, true, Some(2), Some(4)))
        );
        assert_eq!(find.moving(one), Some(("apple".into(), false)));
        // A new search: a late result of the earlier one is not the new one's.
        find.start(two, doc, "pear".into(), true);
        assert_eq!(find.moving(one), None);
        assert_eq!(find.result(view, 3, 4, 3, true, Some(doc)), None);
        assert_eq!(
            find.result(view, 4, 0, 0, true, Some(doc)),
            Some(result(two, false, None, Some(0)))
        );
        // Another document's, or a stopped search's: nothing.
        assert_eq!(find.result(view, 5, 1, 1, true, Some(other)), None);
        assert!(!find.stop(one));
        assert!(find.stop(two));
        assert_eq!(find.result(view, 6, 1, 1, true, Some(doc)), None);
        assert_eq!(find.moving(two), None);
        find.start(one, doc, "apple".into(), false);
        find.end();
        assert_eq!(find.moving(one), None);
    }
}
