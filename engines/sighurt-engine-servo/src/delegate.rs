//! Servo's `ServoDelegate`, `WebViewDelegate` and `ClipboardDelegate` callbacks, queued as
//! notes for the runtime.
//!
//! Callbacks only queue: one [`Delegate`] serves every WebView, records what Servo said and
//! the request objects it handed over, keyed by the WebView, and the runtime turns them into
//! `EngineEvent`s after the Servo call returns (`runtime::ServoRuntime::absorb`), so nothing
//! mutates browser state re-entrantly. Replaceable notes (a new frame, title, status text,
//! cursor) keep only the latest per WebView between two drains; everything else keeps its
//! order. [`Coalescing`] is that queue, and the runtime's queue of events too: a page that
//! floods Servo with them costs constant time per item.
//!
//! A navigation the page asks for is held as a [`PageNavigation`], which denies it when dropped
//! unanswered (Servo's own `NavigationRequest` allows it), so it goes ahead only if allowed.
//!
//! Requests the API has no answer for are dropped on arrival, which resolves them closed, as
//! decision 16 deferred them: `<select>` menus keep their selection, colour pickers their
//! colour, `alert` is acknowledged, `confirm` and `prompt` are cancelled, context menus are
//! dismissed; Servo's own defaults send no HTTP credentials and refuse protocol handlers,
//! Bluetooth and devtools.
//! Console messages are page data and go nowhere.
//!
//! Must not own: policy decisions, or process lifetime: backend loss is recorded for the
//! runtime to report, never an exit.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::mem::Discriminant;
use std::time::Instant;

use servo::{
    ClipboardDelegate, CreateNewWebViewRequest, Cursor, EmbedderControl, EmbedderControlId,
    LoadStatus, NavigationRequest, PermissionRequest, ServoDelegate, ServoError, StringRequest,
    TraversalId, WebView, WebViewDelegate, WebViewId,
};
use url::Url;

/// Something Servo said about one WebView.
pub(crate) enum Note {
    FrameReady,
    Title(Option<String>),
    Status(Option<String>),
    Cursor(Cursor),
    Load(LoadStatus),
    History(Vec<Url>, usize),
    /// A traversal the browser asked for (`WebView::go_back`/`go_forward`) is done. Servo's
    /// constellation sends it right after the history change the traversal made, or alone when
    /// the traversal did nothing.
    TraversalComplete(TraversalId),
    Crashed(String),
    /// The page called `window.close()`.
    Closed,
    /// The page asks to navigate (a link, a form, a script) in its frame or a subframe.
    Navigation(PageNavigation),
    /// The page opens a new browsing context (`target=_blank`, `window.open`).
    CreateNew(CreateNewWebViewRequest),
    Permission(PermissionRequest),
    /// An input method or file picker the page wants; other controls are resolved on arrival.
    Control(EmbedderControl),
    HideControl(EmbedderControlId),
    ClipboardRead(StringRequest),
    /// New clipboard text; `None` empties the clipboard.
    ClipboardWrite(Option<String>),
    /// `window.stop()` ran in the page.
    Stopped,
}

impl Note {
    /// Whether a later note of the same kind replaces this one.
    fn replaceable(&self) -> bool {
        matches!(
            self,
            Note::FrameReady | Note::Title(_) | Note::Status(_) | Note::Cursor(_)
        )
    }
}

/// The notes of every WebView, keeping the latest replaceable note per WebView and kind.
type Notes = Coalescing<(WebViewId, Discriminant<Note>), (WebViewId, Note)>;

/// Every WebView's delegate: queues [`Note`]s until the runtime drains them.
#[derive(Default)]
pub(crate) struct Delegate {
    notes: RefCell<Notes>,
    backend_lost: Cell<bool>,
}

impl Delegate {
    fn note(&self, webview: &WebView, note: Note) {
        self.note_for(webview.id(), note);
    }

    fn note_for(&self, id: WebViewId, note: Note) {
        let key = note
            .replaceable()
            .then(|| (id, std::mem::discriminant(&note)));
        self.notes.borrow_mut().push(key, (id, note));
    }

    /// What Servo said since the last call, oldest first.
    pub fn take(&self) -> Vec<(WebViewId, Note)> {
        let mut notes = self.notes.borrow_mut();
        std::iter::from_fn(|| notes.pop()).collect()
    }

    /// Whether Servo lost its backend (the constellation), which it can't recover from.
    pub fn backend_lost(&self) -> bool {
        self.backend_lost.get()
    }

    /// Queues [`Note::Stopped`] for `webview`, from the callback of `window.stop()`, which
    /// holds no WebView handle (it would keep a closed view alive).
    pub fn stopped(&self, webview: WebViewId) {
        self.note_for(webview, Note::Stopped);
    }
}

impl ServoDelegate for Delegate {
    fn notify_error(&self, error: ServoError) {
        match error {
            ServoError::LostConnectionWithBackend => self.backend_lost.set(true),
            other => log::warn!("servo: {other:?}"),
        }
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, webview: WebView) {
        self.note(&webview, Note::FrameReady);
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        self.note(&webview, Note::Title(title));
    }

    fn notify_status_text_changed(&self, webview: WebView, status: Option<String>) {
        self.note(&webview, Note::Status(status));
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        self.note(&webview, Note::Cursor(cursor));
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        self.note(&webview, Note::Load(status));
    }

    fn notify_history_changed(&self, webview: WebView, entries: Vec<Url>, current: usize) {
        self.note(&webview, Note::History(entries, current));
    }

    fn notify_traversal_complete(&self, webview: WebView, traversal: TraversalId) {
        self.note(&webview, Note::TraversalComplete(traversal));
    }

    fn notify_closed(&self, webview: WebView) {
        self.note(&webview, Note::Closed);
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        self.note(&webview, Note::Crashed(reason));
    }

    fn request_navigation(&self, webview: WebView, request: NavigationRequest) {
        let navigation = PageNavigation {
            request: Some(request),
            since: Instant::now(),
        };
        self.note(&webview, Note::Navigation(navigation));
    }

    fn request_create_new(&self, parent: WebView, request: CreateNewWebViewRequest) {
        self.note(&parent, Note::CreateNew(request));
    }

    fn request_permission(&self, webview: WebView, request: PermissionRequest) {
        self.note(&webview, Note::Permission(request));
    }

    fn show_embedder_control(&self, webview: WebView, control: EmbedderControl) {
        match control {
            EmbedderControl::InputMethod(_) | EmbedderControl::FilePicker(_) => {
                self.note(&webview, Note::Control(control))
            }
            // Resolved as dismissed when dropped (decision 16).
            EmbedderControl::SelectElement(_)
            | EmbedderControl::ColorPicker(_)
            | EmbedderControl::SimpleDialog(_)
            | EmbedderControl::ContextMenu(_) => {}
        }
    }

    fn hide_embedder_control(&self, webview: WebView, control: EmbedderControlId) {
        self.note(&webview, Note::HideControl(control));
    }
}

impl ClipboardDelegate for Delegate {
    fn clear(&self, webview: WebView) {
        self.note(&webview, Note::ClipboardWrite(None));
    }

    fn get_text(&self, webview: WebView, request: StringRequest) {
        self.note(&webview, Note::ClipboardRead(request));
    }

    fn set_text(&self, webview: WebView, text: String) {
        self.note(&webview, Note::ClipboardWrite(Some(text)));
    }
}

/// A navigation a page asks for, until it is answered. Dropped unanswered (with its note, its
/// view, its document or the runtime), it is denied: Servo would allow it.
pub(crate) struct PageNavigation {
    /// `None` once answered.
    request: Option<NavigationRequest>,
    /// When Servo asked.
    pub since: Instant,
}

impl PageNavigation {
    /// Where it goes.
    pub fn url(&self) -> &Url {
        &self.request.as_ref().expect("unanswered").url
    }

    /// Lets it go ahead.
    pub fn allow(mut self) {
        if let Some(request) = self.request.take() {
            request.allow();
        }
    }
}

impl Drop for PageNavigation {
    fn drop(&mut self) {
        if let Some(request) = self.request.take() {
            request.deny();
        }
    }
}

/// A first-in, first-out queue in which an item pushed with a key replaces the queued item with
/// the same key: the earlier is dropped, the later keeps its own place. Pushing and popping
/// cost constant time (amortized): a replaced item leaves a gap, and the gaps are swept out
/// once they outnumber the items.
pub(crate) struct Coalescing<K, T> {
    slots: VecDeque<Option<(Option<K>, T)>>,
    /// The sequence number of `slots[0]`.
    first: u64,
    /// The sequence number of the queued item with each key.
    keyed: HashMap<K, u64>,
    len: usize,
}

impl<K, T> Default for Coalescing<K, T> {
    fn default() -> Self {
        Self {
            slots: VecDeque::new(),
            first: 0,
            keyed: HashMap::new(),
            len: 0,
        }
    }
}

impl<K: Copy + Eq + Hash, T> Coalescing<K, T> {
    pub fn push(&mut self, key: Option<K>, item: T) {
        if let Some(old) = key.and_then(|key| self.keyed.remove(&key)) {
            self.slots[(old - self.first) as usize] = None;
            self.len -= 1;
            if self.slots.len() > 2 * self.len + 64 {
                self.sweep();
            }
        }
        if let Some(key) = key {
            self.keyed.insert(key, self.first + self.slots.len() as u64);
        }
        self.slots.push_back(Some((key, item)));
        self.len += 1;
    }

    pub fn pop(&mut self) -> Option<T> {
        while let Some(slot) = self.slots.pop_front() {
            self.first += 1;
            if let Some((key, item)) = slot {
                if let Some(key) = key {
                    self.keyed.remove(&key);
                }
                self.len -= 1;
                return Some(item);
            }
        }
        None
    }

    /// How many items are queued.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        *self = Self {
            first: self.first + self.slots.len() as u64,
            ..Self::default()
        };
    }

    /// Drops the gaps, renumbering what is left.
    fn sweep(&mut self) {
        let items = std::mem::take(&mut self.slots);
        self.keyed.clear();
        for (key, item) in items.into_iter().flatten() {
            if let Some(key) = key {
                self.keyed.insert(key, self.first + self.slots.len() as u64);
            }
            self.slots.push_back(Some((key, item)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyed_item_replaces_the_queued_one_and_keeps_its_own_place() {
        let mut queue = Coalescing::default();
        queue.push(Some('t'), "title 1");
        queue.push(None, "commit");
        queue.push(Some('t'), "title 2");
        queue.push(Some('s'), "status");
        queue.push(None, "commit");
        assert_eq!(queue.len(), 4);
        assert_eq!(queue.pop(), Some("commit"));
        // A key popped is free again.
        assert_eq!(queue.pop(), Some("title 2"));
        queue.push(Some('t'), "title 3");
        let rest: Vec<_> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(rest, ["status", "commit", "title 3"]);
        assert_eq!(queue.len(), 0);
    }

    #[test]
    fn a_flood_of_replacements_stays_small() {
        let mut queue = Coalescing::default();
        for i in 0..100_000 {
            queue.push(None, i);
            queue.push(Some(()), -i);
        }
        assert_eq!(queue.len(), 100_001);
        assert!(
            queue.slots.len() <= 2 * queue.len() + 65,
            "{}",
            queue.slots.len()
        );
        let mut queue = Coalescing::default();
        for i in 0..100_000 {
            queue.push(Some(()), i);
        }
        assert_eq!(queue.len(), 1);
        assert!(queue.slots.len() <= 65);
        assert_eq!(queue.pop(), Some(99_999));
        queue.push(Some(()), 1);
        queue.clear();
        assert_eq!((queue.len(), queue.pop()), (0, None));
        queue.push(Some(()), 2);
        assert_eq!(queue.pop(), Some(2));
    }
}
