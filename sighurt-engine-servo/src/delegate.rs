//! Servo's `ServoDelegate`, `WebViewDelegate` and `ClipboardDelegate` callbacks, queued as
//! notes for the runtime.
//!
//! Callbacks only queue: one [`Delegate`] serves every WebView, records what Servo said and
//! the request objects it handed over, keyed by the WebView, and the runtime turns them into
//! `EngineEvent`s after the Servo call returns (`runtime::ServoRuntime::absorb`), so nothing
//! mutates browser state re-entrantly. Replaceable notes (a new frame, title, status text,
//! cursor) keep only the latest per WebView between two drains; everything else keeps its
//! order.
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

use servo::{
    ClipboardDelegate, CreateNewWebViewRequest, Cursor, EmbedderControl, EmbedderControlId,
    LoadStatus, NavigationRequest, PermissionRequest, ServoDelegate, ServoError, StringRequest,
    WebView, WebViewDelegate, WebViewId,
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
    Crashed(String),
    /// The page called `window.close()`.
    Closed,
    /// The page asks to navigate (a link, a form, a script) in its frame or a subframe.
    Navigation(NavigationRequest),
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
    fn replaces(&self, other: &Note) -> bool {
        matches!(
            (self, other),
            (Note::FrameReady, Note::FrameReady)
                | (Note::Title(_), Note::Title(_))
                | (Note::Status(_), Note::Status(_))
                | (Note::Cursor(_), Note::Cursor(_))
        )
    }
}

/// Every WebView's delegate: queues [`Note`]s until the runtime drains them.
#[derive(Default)]
pub(crate) struct Delegate {
    notes: RefCell<Vec<(WebViewId, Note)>>,
    backend_lost: Cell<bool>,
}

impl Delegate {
    fn note(&self, webview: &WebView, note: Note) {
        self.note_for(webview.id(), note);
    }

    fn note_for(&self, id: WebViewId, note: Note) {
        let mut notes = self.notes.borrow_mut();
        notes.retain(|(other, old)| *other != id || !note.replaces(old));
        notes.push((id, note));
    }

    /// What Servo said since the last call, oldest first.
    pub fn take(&self) -> Vec<(WebViewId, Note)> {
        self.notes.take()
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

    fn notify_closed(&self, webview: WebView) {
        self.note(&webview, Note::Closed);
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        self.note(&webview, Note::Crashed(reason));
    }

    fn request_navigation(&self, webview: WebView, request: NavigationRequest) {
        self.note(&webview, Note::Navigation(request));
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
