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
//! Console messages are page data: bodies are logged at `debug` under `sighurt::console`, cut
//! and escaped, never trusted. A warn line is only a per-view count, with no message text.
//!
//! Must not own: policy decisions, or process lifetime: backend loss is recorded for the
//! runtime to report, never an exit.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::mem::Discriminant;
use std::time::{Duration, Instant};

use servo::{
    ClipboardDelegate, ConsoleLogLevel, CreateNewWebViewRequest, Cursor, EmbedderControl,
    EmbedderControlId, LoadStatus, NavigationRequest, PermissionRequest, ServoDelegate, ServoError,
    StringRequest, TraversalId, WebView, WebViewDelegate, WebViewId,
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

/// Debug bodies kept per view per second. The rest of the second is one "suppressed N" line.
const CONSOLE_DEBUG_PER_SEC: u32 = 20;
const CONSOLE_DEBUG_WINDOW: Duration = Duration::from_secs(1);
/// How often a view's script-error count is logged, with no message text.
const CONSOLE_ERROR_WINDOW: Duration = Duration::from_secs(10);

/// One view's console rate limit. The strings it builds name a count, never page text.
struct ViewConsole {
    debug_start: Instant,
    logged: u32,
    suppressed: u32,
    error_start: Instant,
    errors: u32,
}

struct ConsoleLine {
    level: log::Level,
    text: String,
}

impl ViewConsole {
    fn new(now: Instant) -> Self {
        Self {
            debug_start: now,
            logged: 0,
            suppressed: 0,
            error_start: now,
            errors: 0,
        }
    }

    /// Closes windows that have elapsed. `label` is a view id, not page text.
    fn roll(
        &mut self,
        now: Instant,
        label: &str,
        debug_on: bool,
        warn_on: bool,
    ) -> Vec<ConsoleLine> {
        let mut out = Vec::new();
        if now.duration_since(self.debug_start) >= CONSOLE_DEBUG_WINDOW {
            if debug_on && self.suppressed > 0 {
                out.push(ConsoleLine {
                    level: log::Level::Debug,
                    text: format!(
                        "view {label}: suppressed {} console messages",
                        self.suppressed
                    ),
                });
            }
            self.debug_start = now;
            self.logged = 0;
            self.suppressed = 0;
        }
        if now.duration_since(self.error_start) >= CONSOLE_ERROR_WINDOW {
            if warn_on && self.errors > 0 {
                out.push(ConsoleLine {
                    level: log::Level::Warn,
                    text: format!(
                        "view {label}: {} page script errors in the last 10 s",
                        self.errors
                    ),
                });
            }
            self.error_start = now;
            self.errors = 0;
        }
        out
    }

    /// `log_body` is whether this message's text may be formatted and logged at debug.
    fn observe(
        &mut self,
        now: Instant,
        label: &str,
        is_error: bool,
        debug_on: bool,
        warn_on: bool,
    ) -> (Vec<ConsoleLine>, bool) {
        let lines = self.roll(now, label, debug_on, warn_on);
        if is_error && warn_on {
            self.errors += 1;
        }
        let log_body = if debug_on {
            if self.logged < CONSOLE_DEBUG_PER_SEC {
                self.logged += 1;
                true
            } else {
                self.suppressed += 1;
                false
            }
        } else {
            false
        };
        (lines, log_body)
    }

    fn deadline(&self, now: Instant, debug_on: bool, warn_on: bool) -> Option<Instant> {
        let mut due = None;
        if debug_on && self.suppressed > 0 {
            let at = self.debug_start + CONSOLE_DEBUG_WINDOW;
            if at > now {
                due = Some(at);
            }
        }
        if warn_on && self.errors > 0 {
            let at = self.error_start + CONSOLE_ERROR_WINDOW;
            if at > now {
                due = Some(due.map_or(at, |earlier| earlier.min(at)));
            }
        }
        due
    }
}

/// Every WebView's delegate: queues [`Note`]s until the runtime drains them.
#[derive(Default)]
pub(crate) struct Delegate {
    notes: RefCell<Notes>,
    backend_lost: Cell<bool>,
    /// Console rate limits. `None` is a message Servo did not attach to a WebView.
    console: RefCell<HashMap<Option<WebViewId>, ViewConsole>>,
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

    /// Emits console summaries whose windows have elapsed. No page text.
    pub fn flush_console_logs(&self, now: Instant) {
        let (debug_on, warn_on) = console_levels();
        if !debug_on && !warn_on {
            return;
        }
        let mut lines = Vec::new();
        for (view, state) in self.console.borrow_mut().iter_mut() {
            let label = console_view_label(*view);
            lines.extend(state.roll(now, &label, debug_on, warn_on));
        }
        emit_console_lines(&lines);
    }

    /// When the owner loop should pump so a console summary is not stuck behind idle.
    pub fn console_log_deadline(&self, now: Instant) -> Option<Instant> {
        let (debug_on, warn_on) = console_levels();
        if !debug_on && !warn_on {
            return None;
        }
        self.console.borrow().values().fold(None, |due, state| {
            match (due, state.deadline(now, debug_on, warn_on)) {
                (None, next) => next,
                (Some(due), None) => Some(due),
                (Some(due), Some(next)) => Some(due.min(next)),
            }
        })
    }

    fn log_console(&self, view: Option<WebViewId>, level: ConsoleLogLevel, message: &str) {
        let (debug_on, warn_on) = console_levels();
        let is_error = matches!(level, ConsoleLogLevel::Error);
        if !debug_on && !(warn_on && is_error) {
            return;
        }
        let now = Instant::now();
        let label = console_view_label(view);
        let (lines, log_body) = {
            let mut limits = self.console.borrow_mut();
            let state = limits.entry(view).or_insert_with(|| ViewConsole::new(now));
            state.observe(now, &label, is_error, debug_on, warn_on)
        };
        emit_console_lines(&lines);
        if log_body {
            let (level, text) = console_line(level, message);
            log::log!(target: "sighurt::console", level, "{text}");
        }
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

    fn show_console_message(&self, level: ConsoleLogLevel, message: String) {
        self.log_console(None, level, &message);
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

    fn show_console_message(&self, webview: WebView, level: ConsoleLogLevel, message: String) {
        self.log_console(Some(webview.id()), level, &message);
    }
}

fn console_levels() -> (bool, bool) {
    (
        log::log_enabled!(target: "sighurt::console", log::Level::Debug),
        log::log_enabled!(target: "sighurt::console", log::Level::Warn),
    )
}

fn console_view_label(view: Option<WebViewId>) -> String {
    match view {
        Some(id) => format!("{id:?}"),
        None => "servo".to_string(),
    }
}

fn emit_console_lines(lines: &[ConsoleLine]) {
    for line in lines {
        log::log!(target: "sighurt::console", line.level, "{}", line.text);
    }
}

/// Page console text is untrusted and is always `debug`. The message is cut to 1 KiB on a
/// char boundary, then Cc, Cf, Zl and Zp are escaped, so the log line can be longer than the cut.
pub(crate) fn console_line(_level: ConsoleLogLevel, message: &str) -> (log::Level, String) {
    (log::Level::Debug, sanitize_console(message))
}

fn sanitize_console(message: &str) -> String {
    let cut = truncate_utf8(message, 1024);
    let mut out = String::new();
    for ch in cut.chars() {
        if console_char_escaped(ch) {
            out.extend(ch.escape_default());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Cc (`is_control`), plus Cf, Zl (U+2028) and Zp (U+2029): bidi overrides, isolates and
/// zero-width characters otherwise pass through into the log.
fn console_char_escaped(ch: char) -> bool {
    ch.is_control()
        || matches!(
            ch,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

fn truncate_utf8(message: &str, max: usize) -> &str {
    if message.len() <= max {
        return message;
    }
    let mut end = max;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    &message[..end]
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

    #[test]
    fn console_messages_are_cut_and_controls_escaped() {
        let (level, text) = console_line(ConsoleLogLevel::Error, "boom\nline\t");
        assert_eq!(level, log::Level::Debug);
        assert_eq!(text, "boom\\nline\\t");
        assert!(!text.chars().any(char::is_control));

        let (level, text) = console_line(ConsoleLogLevel::Warn, "ok\u{0001}");
        assert_eq!(level, log::Level::Debug);
        assert_eq!(text, "ok\\u{1}");
        for level in [
            ConsoleLogLevel::Error,
            ConsoleLogLevel::Log,
            ConsoleLogLevel::Debug,
            ConsoleLogLevel::Info,
            ConsoleLogLevel::Trace,
            ConsoleLogLevel::Dir,
        ] {
            assert_eq!(console_line(level, "x").0, log::Level::Debug);
        }

        let bidi = console_line(
            ConsoleLogLevel::Log,
            "a\u{202A}b\u{202E}c\u{2066}d\u{2069}e\u{061C}f\u{200B}g\u{2028}h\u{2029}",
        )
        .1;
        for raw in [
            '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{061C}', '\u{200B}', '\u{2028}',
            '\u{2029}',
        ] {
            assert!(!bidi.contains(raw), "{bidi}");
        }
        assert!(bidi.contains("\\u{202a}"), "{bidi}");
        assert!(bidi.contains("\\u{2028}"), "{bidi}");
        assert!(bidi.contains("\\u{200b}"), "{bidi}");
        assert_eq!(console_line(ConsoleLogLevel::Log, "ok é").1, "ok é");

        let long = "é".repeat(600);
        let (_, text) = console_line(ConsoleLogLevel::Log, &long);
        assert_eq!(text.len(), 1024);
        assert_eq!(text.chars().count(), 512);

        let mut mixed = "a".repeat(1023);
        mixed.push('é');
        let (_, text) = console_line(ConsoleLogLevel::Info, &mixed);
        assert_eq!(text, "a".repeat(1023));
    }

    #[test]
    fn console_bodies_and_error_counts_are_rate_limited_per_view() {
        let start = Instant::now();
        let mut view = ViewConsole::new(start);
        let mut bodies = 0;
        for i in 0..25 {
            let (lines, log_body) = view.observe(start, "1", i == 0, true, true);
            assert!(lines.is_empty(), "the window has not elapsed");
            if log_body {
                bodies += 1;
            }
        }
        assert_eq!(bodies, 20);
        assert_eq!(view.suppressed, 5);
        assert_eq!(view.errors, 1);
        let due = view.deadline(start, true, true).expect("a wake");
        assert_eq!(due, start + CONSOLE_DEBUG_WINDOW);

        let rolled = view.roll(start + CONSOLE_DEBUG_WINDOW, "1", true, true);
        assert!(
            rolled.iter().any(|line| {
                line.level == log::Level::Debug && line.text.contains("suppressed 5")
            }),
            "{:?}",
            rolled.iter().map(|line| &line.text).collect::<Vec<_>>()
        );
        assert!(rolled.iter().all(|line| line.level != log::Level::Warn));
        assert_eq!(view.suppressed, 0);

        let (lines, log_body) = view.observe(start + CONSOLE_ERROR_WINDOW, "1", true, true, true);
        assert!(log_body);
        assert!(
            lines.iter().any(|line| {
                line.level == log::Level::Warn
                    && line.text == "view 1: 1 page script errors in the last 10 s"
            }),
            "{:?}",
            lines.iter().map(|line| &line.text).collect::<Vec<_>>()
        );
        assert!(lines.iter().all(|line| !line.text.contains("boom")));
        assert_eq!(view.errors, 1, "the new error starts the next window");
    }
}
