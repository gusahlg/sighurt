//! `LadybirdRuntime`: the `EngineRuntime` over the shim.
//!
//! Owns: the runtime's views, the translation of Ladybird's callbacks (as the shim reports them) into
//! the contract's events (throttled per view without reordering them), the keys and buttons each
//! view's content holds, and presenting frames into the host's CPU target (one counted copy per
//! frame). Must not own: how Ladybird's loop works (the shim) or when the host pumps (it gets a
//! deadline and a waker).

use std::collections::HashMap;
use std::ffi::{c_char, c_int, CStr, CString};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, ViewOptions,
};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError, OpenDisposition};
use sighurt_api::geometry::{Metrics, SurfacePoint, SurfaceRect};
use sighurt_api::ids::{DocumentGeneration, NavigationId, RequestId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputKind, KeyEvent, Modifiers, PointerButton, PointerEvent,
    ScrollDelta, TextInputState, TextPurpose,
};
use sighurt_api::present::CpuTarget;
use sighurt_api::services::{
    ClipboardData, DownloadSource, PickMode, Selection, Service, ServiceError, ServiceReply,
    ServiceRequest, ServiceResult,
};

use crate::ffi;
use crate::waiter::Waiter;

/// The engine's name in the configuration and the registry.
pub const NAME: &str = "ladybird";

/// What the engine supports: back and forward within Ladybird's own session history, and page
/// zoom. Each view's pages run in a WebContent process of Ladybird's. The rest is not wired yet
/// (docs/rewrite/LADYBIRD.md, "What is missing").
pub const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    history_traversal: true,
    text_input: true,
    accessibility: false,
    screenshots: false,
    find_in_page: false,
    page_zoom: true,
    pointer_lock: false,
    native_popups: false,
    process_per_view: true,
    semantics: false,
    site_data: sighurt_api::engine::SiteDataKinds::NONE,
};

/// Most events one pump hands over.
const MAX_EVENTS_PER_PUMP: usize = 256;
/// Clicks closer together in time and space than these are a double click.
const MULTI_CLICK: Duration = Duration::from_millis(400);
const MULTI_CLICK_SLOP: i32 = 4;

/// What the runtime starts with.
#[derive(Clone, Debug, Default)]
pub struct LadybirdConfig {
    /// Ladybird's profile directory (its cookies, storage, caches). `None`: a temporary one,
    /// removed at shutdown.
    pub profile: Option<PathBuf>,
    /// Another Ladybird prefix (whose `libexec/` has the helpers) than the shim's own.
    pub prefix: Option<PathBuf>,
    /// The shim library, when not where the build script found it.
    pub shim: Option<PathBuf>,
    /// More Ladybird command-line options.
    pub arguments: Vec<String>,
}

struct View<T: CpuTarget> {
    target: T,
    lb: u64,
    ready: bool,
    visible: bool,
    document: DocumentGeneration,
    /// The browser's navigation that Ladybird has not started yet.
    pending: Option<(NavigationId, Pend)>,
    /// An address change Ladybird reported whose kind is not known yet: a new document says so with
    /// its load start or commit right after; a page's own change (a fragment, pushState) says nothing.
    deferred: Option<(String, Instant)>,
    /// A navigation Ladybird started that has not committed.
    active: Option<NavigationId>,
    /// Whether the browser stopped the active navigation.
    stopped: bool,
    /// A frame arrived since the last submission.
    frame_dirty: bool,
    last_click: Option<(Instant, i32, i32, PointerButton, i32)>,
    /// The buttons held, as the shim's mask: every pointer event says them.
    pointer_buttons: i32,
    /// The keys the content got pressed and not released, released for it when the view loses
    /// focus.
    keys: Vec<KeyEvent>,
    history: Option<(Vec<String>, usize)>,
    /// The address of the document shown.
    url: String,
    /// A page's copy, representation by representation, until the last one has come.
    clipboard: Vec<ClipboardData>,
}

/// What the browser asked of a view that has not started yet.
enum Pend {
    Url(String),
    Reload,
    Traverse,
}

/// How long a changed address waits to learn whether it is a new document (the next callbacks come
/// in the same burst of messages, so this is generous).
const COMMIT_SETTLE: Duration = Duration::from_millis(50);

/// What the shim is waiting to be told about a service request.
enum Pending {
    /// A paste: the text to paste goes to the view.
    ClipboardRead(u64),
    /// Nothing comes back to Ladybird (a copy, a download): the answer is only recorded.
    Told,
    /// A file picker: the shim's token.
    Pick(i64),
    /// An external URL: the shim's token.
    Open(i64),
}

/// Navigations held for the browser per view, at most; one more is denied at once.
const MAX_HELD: usize = 16;

/// Most bytes of text a paste takes.
const MAX_PASTE: usize = 1024 * 1024;

/// Most bytes of a download handed to the host (the shim stops larger ones while they run).
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;

/// What the runtime counted, for diagnostics.
#[derive(Clone, Copy, Debug, Default)]
pub struct LadybirdDiagnostics {
    /// Frames copied from Ladybird's backing stores into the host's buffers: every frame
    /// presented is one whole-frame copy.
    pub frames_copied: u64,
}

/// The Ladybird engine's runtime. One per process, on the owner thread.
pub struct LadybirdRuntime<T: CpuTarget> {
    views: HashMap<ViewId, View<T>>,
    by_lb: HashMap<u64, ViewId>,
    waiter: Option<Waiter>,
    queue: Vec<EngineEvent>,
    services: HashMap<RequestId, (ViewId, Pending)>,
    /// Navigations Ladybird holds for the browser: its request id, the view, the shim's token.
    held: HashMap<RequestId, (ViewId, i64)>,
    throttle: Throttle,
    diagnostics: LadybirdDiagnostics,
    profile: Option<PathBuf>,
    temporary_profile: bool,
    shut_down: bool,
    _not_send: std::marker::PhantomData<*const ()>,
}

/// A new directory under `base` that only this user can enter, with a name nobody can guess
/// (`mkdtemp`: it fails rather than use one that exists).
fn private_temporary_directory(base: &std::path::Path) -> Result<PathBuf, String> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let template = base.join("sighurt-ladybird-XXXXXX");
    let mut bytes = CString::new(template.as_os_str().as_bytes())
        .map_err(|_| "the temporary directory's path has a NUL".to_string())?
        .into_bytes_with_nul();
    // SAFETY: a NUL-terminated, writable template, as mkdtemp wants.
    let made = unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) };
    if made.is_null() {
        return Err(format!(
            "cannot make a private directory in {}: {}",
            base.display(),
            std::io::Error::last_os_error()
        ));
    }
    bytes.pop();
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).expect("NULs removed")
}

fn from_c(pointer: *const c_char) -> Option<String> {
    (!pointer.is_null()).then(|| {
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    })
}

/// The throttle of each view's title, status, cursor, history and text input ([`THROTTLE`]).
#[derive(Default)]
struct Throttle {
    /// When each throttled kind of event last went to the browser.
    last_sent: HashMap<(u8, ViewId), Instant>,
    /// The latest one of each kind waiting to be sent, in the order they came.
    waiting: Vec<((u8, ViewId), EngineEvent)>,
}

impl Throttle {
    /// Passes the batch through: a throttled event sent less than [`THROTTLE`] after the last of its
    /// kind waits (the latest one) and goes out when the time is up. An event that is not replaceable
    /// (a commit, a failure, a request, the view's closing) goes out after what waits for its view, so
    /// a view's events keep their order: a field's text input never arrives after the commit of the
    /// next document. Returns what to send now, and when the next waiting one is due.
    fn pass(
        &mut self,
        batch: Vec<EngineEvent>,
        now: Instant,
    ) -> (Vec<EngineEvent>, Option<Instant>) {
        let mut out = Vec::new();
        for event in batch {
            let Some(key) = throttled(&event) else {
                let barrier = replaceable(&event)
                    .is_none()
                    .then(|| event.view())
                    .flatten();
                if let Some(view) = barrier {
                    self.release_view(view, now, &mut out);
                }
                let closed = match &event {
                    EngineEvent::ViewClosed { view } => Some(*view),
                    _ => None,
                };
                out.push(event);
                if let Some(view) = closed {
                    self.last_sent.retain(|(_, v), _| *v != view);
                }
                continue;
            };
            let held = self.waiting.iter().position(|(k, _)| *k == key);
            let due = held.is_none()
                && self
                    .last_sent
                    .get(&key)
                    .is_none_or(|at| now.duration_since(*at) >= THROTTLE);
            if due {
                self.last_sent.insert(key, now);
                out.push(event);
            } else {
                if let Some(i) = held {
                    self.waiting.remove(i);
                }
                self.waiting.push((key, event));
            }
        }
        let mut next: Option<Instant> = None;
        let mut i = 0;
        while i < self.waiting.len() {
            let key = self.waiting[i].0;
            let at = self.last_sent.get(&key).map_or(now, |at| *at + THROTTLE);
            if now >= at {
                let (_, event) = self.waiting.remove(i);
                self.last_sent.insert(key, now);
                out.push(event);
            } else {
                next = Some(next.map_or(at, |n| n.min(at)));
                i += 1;
            }
        }
        (out, next)
    }

    /// Sends what waits for `view` now, in the order it came.
    fn release_view(&mut self, view: ViewId, now: Instant, out: &mut Vec<EngineEvent>) {
        let mut i = 0;
        while i < self.waiting.len() {
            if self.waiting[i].0 .1 == view {
                let (key, event) = self.waiting.remove(i);
                self.last_sent.insert(key, now);
                out.push(event);
            } else {
                i += 1;
            }
        }
    }

    /// Drops everything of `view`'s: the browser closed it.
    fn forget(&mut self, view: ViewId) {
        self.waiting.retain(|((_, v), _)| *v != view);
        self.last_sent.retain(|(_, v), _| *v != view);
    }
}

impl<T: CpuTarget> LadybirdRuntime<T> {
    /// Loads the shim, starts Ladybird's UI process half on this thread, and the wait thread that
    /// wakes the owner loop through `waker`.
    pub fn new(config: LadybirdConfig, waker: EngineWaker) -> Result<Self, String> {
        let shim = config
            .shim
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        ffi::load(shim.as_deref())?;
        let (profile, temporary_profile) = match config.profile {
            Some(path) => {
                std::fs::create_dir_all(&path)
                    .map_err(|e| format!("cannot make {}: {e}", path.display()))?;
                (path, false)
            }
            None => {
                let base = std::env::var_os("XDG_RUNTIME_DIR")
                    .map(PathBuf::from)
                    .unwrap_or_else(std::env::temp_dir);
                (private_temporary_directory(&base)?, true)
            }
        };
        let arguments: Vec<CString> = config.arguments.iter().map(|a| c_string(a)).collect();
        let pointers: Vec<*const c_char> = arguments.iter().map(|a| a.as_ptr()).collect();
        let c_profile = c_string(&profile.to_string_lossy());
        let c_prefix = config
            .prefix
            .as_ref()
            .map(|p| c_string(&p.to_string_lossy()));
        let mut error = [0 as c_char; 512];
        let status = unsafe {
            ffi::lb_init(
                pointers.as_ptr(),
                pointers.len() as c_int,
                c_profile.as_ptr(),
                c_prefix.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if status != 0 {
            if temporary_profile {
                let _ = std::fs::remove_dir_all(&profile);
            }
            return Err(format!(
                "Ladybird did not start: {}",
                from_c(error.as_ptr()).unwrap_or_default()
            ));
        }
        let fd = unsafe { ffi::lb_loop_fd() };
        let waiter = match Waiter::start(fd, waker) {
            Ok(waiter) => waiter,
            Err(error) => {
                // Not a started Ladybird nobody pumps: its helpers and the temporary profile go.
                unsafe { ffi::lb_shutdown() };
                if temporary_profile {
                    let _ = std::fs::remove_dir_all(&profile);
                }
                return Err(error);
            }
        };
        Ok(Self {
            views: HashMap::new(),
            by_lb: HashMap::new(),
            waiter: Some(waiter),
            queue: Vec::new(),
            services: HashMap::new(),
            held: HashMap::new(),
            throttle: Throttle::default(),
            diagnostics: LadybirdDiagnostics::default(),
            profile: Some(profile),
            temporary_profile,
            shut_down: false,
            _not_send: std::marker::PhantomData,
        })
    }

    /// How many times the owner loop's wake descriptor fired (for the idle test).
    pub fn wake_count(&self) -> u64 {
        unsafe { ffi::lb_wake_count() }
    }

    /// What the runtime counted.
    pub fn diagnostics(&self) -> LadybirdDiagnostics {
        self.diagnostics
    }

    /// Reads and removes the file a download was staged in (the event's `data` is its path in the
    /// profile's staging directory): `None` when it is not there or is over [`MAX_DOWNLOAD`].
    fn take_staged(&self, event: &ffi::Event) -> Option<Vec<u8>> {
        use std::io::Read;
        use std::os::unix::ffi::OsStrExt;
        if event.data.is_null() {
            return None;
        }
        let bytes = unsafe { std::slice::from_raw_parts(event.data.cast::<u8>(), event.len) };
        let path = std::path::Path::new(std::ffi::OsStr::from_bytes(bytes));
        let staging = self.profile.as_ref()?.join("downloads-staging");
        if path.parent() != Some(staging.as_path()) {
            return None;
        }
        let read = std::fs::File::open(path).and_then(|file| {
            let mut data = Vec::new();
            file.take(MAX_DOWNLOAD + 1).read_to_end(&mut data)?;
            Ok(data)
        });
        let _ = std::fs::remove_file(path);
        read.ok().filter(|data| data.len() as u64 <= MAX_DOWNLOAD)
    }

    fn running(&self) -> Result<(), EngineError> {
        match self.shut_down {
            true => Err(EngineError::Runtime(
                "the Ladybird runtime is shut down".into(),
            )),
            false => Ok(()),
        }
    }

    fn translate(&mut self, event: &ffi::Event) {
        let known = self.by_lb.get(&event.view).copied();
        let Some((id, view)) = known.and_then(|id| self.views.get_mut(&id).map(|view| (id, view)))
        else {
            // A view that is gone (or a popup the browser never sees) asked for something that waits
            // for an answer: refused, so nothing is left holding a token.
            match event.kind {
                ffi::EV_NAVIGATION_REQUESTED => unsafe { ffi::lb_navigation_answer(event.i1, 0) },
                ffi::EV_OPEN_EXTERNAL => unsafe { ffi::lb_open_external_reply(event.i1, 0) },
                ffi::EV_PICK_FILES => unsafe { ffi::lb_pick_reply(event.i1, std::ptr::null()) },
                ffi::EV_CLIPBOARD_WANT => unsafe { ffi::lb_clipboard_deny(event.view) },
                ffi::EV_DOWNLOAD => drop(self.take_staged(event)),
                _ => {}
            }
            return;
        };
        let s0 = from_c(event.s0);
        match event.kind {
            ffi::EV_VIEW_READY => {
                if !view.ready {
                    view.ready = true;
                    self.queue.push(EngineEvent::ViewReady { view: id });
                }
            }
            // Ladybird reports a load's start after the document committed: the start of a
            // navigation is the loading state turning on.
            // A document's load starts, and Ladybird's top-level commit: the address change that
            // came just before was a new document.
            ffi::EV_LOAD_START | ffi::EV_COMMIT => {
                settle_commit(
                    id,
                    view,
                    &mut self.queue,
                    &mut self.held,
                    Settle::NewDocument,
                );
            }
            ffi::EV_LOAD_FINISH => {
                settle_commit(id, view, &mut self.queue, &mut self.held, Settle::Ended)
            }
            ffi::EV_LOADING => {
                let loading = event.i0 != 0;
                // Whatever address change came before is settled first (a fragment navigation never
                // gets a load start).
                settle_commit(id, view, &mut self.queue, &mut self.held, Settle::Ended);
                if loading {
                    if view.active.is_none() {
                        let navigation = view
                            .pending
                            .take()
                            .map_or_else(NavigationId::next, |(navigation, _)| navigation);
                        view.active = Some(navigation);
                        view.stopped = false;
                        // The URL is not known until it commits; the browser has it for its own.
                        self.queue.push(EngineEvent::NavigationStarted {
                            view: id,
                            navigation,
                            url: String::new(),
                        });
                    }
                } else if let Some(navigation) = view.active.take() {
                    // Loading stopped with nothing committed: stopped, or failed.
                    let error = if view.stopped {
                        NavigationError::Cancelled
                    } else {
                        NavigationError::Load("the page did not load".into())
                    };
                    self.queue.push(EngineEvent::NavigationFailed {
                        view: id,
                        navigation,
                        error,
                    });
                }
                self.queue
                    .push(EngineEvent::LoadingChanged { view: id, loading });
            }
            ffi::EV_URL_CHANGED => {
                settle_commit(id, view, &mut self.queue, &mut self.held, Settle::Ended);
                view.deferred = Some((s0.unwrap_or_default(), Instant::now()));
            }
            ffi::EV_TITLE => self.queue.push(EngineEvent::TitleChanged {
                view: id,
                title: s0.filter(|t| !t.is_empty()),
            }),
            ffi::EV_CURSOR => self.queue.push(EngineEvent::CursorChanged {
                view: id,
                cursor: cursor_for(event.i0),
            }),
            ffi::EV_HOVER => self
                .queue
                .push(EngineEvent::StatusTextChanged { view: id, text: s0 }),
            ffi::EV_OPEN_REQUESTED => {
                if let Some(url) = s0 {
                    self.queue.push(EngineEvent::OpenRequested {
                        view: id,
                        document: view.document,
                        url,
                        disposition: OpenDisposition::NewView,
                    });
                }
            }
            ffi::EV_FRAME => {
                view.frame_dirty = true;
                self.queue.push(EngineEvent::FrameReady { view: id });
            }
            ffi::EV_CRASHED => self.queue.push(EngineEvent::ViewFailed {
                view: id,
                message: s0.unwrap_or_else(|| "the page crashed".into()),
            }),
            // The page closed its own view (window.close() of a script-closable one): Ladybird has
            // already dropped the page, so the view cannot continue. The browser hears the request
            // (and closes a tab the page opened), then that the view failed and is closed.
            ffi::EV_CLOSED => {
                let document = view.document;
                self.queue
                    .push(EngineEvent::CloseRequested { view: id, document });
                self.queue.push(EngineEvent::ViewFailed {
                    view: id,
                    message: "the page closed its window".into(),
                });
                self.remove_view(id);
                self.queue.push(EngineEvent::ViewClosed { view: id });
            }
            ffi::EV_HISTORY => {
                let entries: Vec<String> = s0
                    .unwrap_or_default()
                    .lines()
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
                    .collect();
                let current = usize::try_from(event.i0).unwrap_or(0);
                if current < entries.len() {
                    let changed = view
                        .history
                        .as_ref()
                        .is_none_or(|(old, at)| *old != entries || *at != current);
                    if changed {
                        view.history = Some((entries.clone(), current));
                        self.queue.push(EngineEvent::HistoryChanged {
                            view: id,
                            entries,
                            current,
                        });
                    }
                }
            }
            ffi::EV_INPUT_METHOD => {
                let state = (event.i0 != 0).then(|| {
                    let caret = s0.as_deref().map(parse_caret).unwrap_or_default();
                    let ratio = view.target.metrics().scale().factor();
                    // Device pixels to the target's logical ones. No text around the caret is
                    // reported: Ladybird does not say whether the field is a password.
                    TextInputState {
                        caret: SurfaceRect {
                            x: (f64::from(caret[0]) / ratio).floor() as i32,
                            y: (f64::from(caret[1]) / ratio).floor() as i32,
                            width: (f64::from(caret[2]) / ratio).ceil() as u32,
                            height: (f64::from(caret[3]) / ratio).ceil() as u32,
                        },
                        surrounding: None,
                        // Ladybird does not say whether a field is a password, so every field is
                        // treated as one: the input method gets the sensitive and hidden hints.
                        purpose: TextPurpose::Password,
                        multiline: false,
                    }
                });
                self.queue
                    .push(EngineEvent::TextInputChanged { view: id, state });
            }
            ffi::EV_CLIPBOARD_WRITE => {
                // Binary payloads are in event.data.
                let bytes = if event.data.is_null() {
                    Vec::new()
                } else {
                    unsafe { std::slice::from_raw_parts(event.data.cast::<u8>(), event.len) }
                        .to_vec()
                };
                view.clipboard.push(ClipboardData {
                    mime_type: s0.unwrap_or_default(),
                    data: bytes,
                });
                if event.i1 == 0 {
                    let items = std::mem::take(&mut view.clipboard);
                    let request = request(
                        id,
                        view,
                        Service::ClipboardWrite {
                            selection: Selection::Clipboard,
                            items,
                        },
                    );
                    self.services.insert(request.id, (id, Pending::Told));
                    self.queue.push(EngineEvent::ServiceRequested(request));
                }
            }
            ffi::EV_CLIPBOARD_WANT => {
                let request = request(
                    id,
                    view,
                    Service::ClipboardRead {
                        selection: Selection::Clipboard,
                        mime_types: vec!["text/plain;charset=utf-8".into(), "text/plain".into()],
                        limit: MAX_PASTE,
                    },
                );
                self.services
                    .insert(request.id, (id, Pending::ClipboardRead(view.lb)));
                self.queue.push(EngineEvent::ServiceRequested(request));
            }
            ffi::EV_DOWNLOAD => {
                let Some(bytes) = self.take_staged(event) else {
                    return;
                };
                let view = &self.views[&id];
                let request = request(
                    id,
                    view,
                    Service::Download {
                        source: DownloadSource::Data(bytes),
                        suggested_name: from_c(event.s1),
                    },
                );
                self.services.insert(request.id, (id, Pending::Told));
                self.queue.push(EngineEvent::ServiceRequested(request));
            }
            ffi::EV_PICK_FILES => {
                let request = request(
                    id,
                    view,
                    Service::PickFiles {
                        mode: if event.i0 != 0 {
                            PickMode::OpenFiles
                        } else {
                            PickMode::OpenFile
                        },
                        filters: Vec::new(),
                    },
                );
                self.services
                    .insert(request.id, (id, Pending::Pick(event.i1)));
                self.queue.push(EngineEvent::ServiceRequested(request));
            }
            ffi::EV_NAVIGATION_REQUESTED => {
                let held = self.held.values().filter(|(v, _)| *v == id).count();
                if held >= MAX_HELD {
                    // Too many held at once: the contract lets the page's navigations beyond the
                    // limit go ahead without asking.
                    unsafe { ffi::lb_navigation_answer(event.i1, 1) };
                } else if let Some(url) = s0 {
                    let request = RequestId::next();
                    self.held.insert(request, (id, event.i1));
                    self.queue.push(EngineEvent::NavigationRequested {
                        view: id,
                        document: view.document,
                        request,
                        url,
                    });
                } else {
                    unsafe { ffi::lb_navigation_answer(event.i1, 0) };
                }
            }
            ffi::EV_OPEN_EXTERNAL => {
                let request = request(
                    id,
                    view,
                    Service::OpenExternal {
                        url: s0.unwrap_or_default(),
                    },
                );
                self.services
                    .insert(request.id, (id, Pending::Open(event.i1)));
                self.queue.push(EngineEvent::ServiceRequested(request));
            }
            _ => {}
        }
    }

    fn resolve(&mut self, request: RequestId, result: ServiceResult) {
        let Some((_, pending)) = self.services.remove(&request) else {
            return;
        };
        match pending {
            Pending::Told => {}
            Pending::ClipboardRead(lb) => match result {
                Ok(ServiceReply::Clipboard(data)) if data.data.len() <= MAX_PASTE => unsafe {
                    ffi::lb_clipboard_grant(lb, data.data.as_ptr(), data.data.len());
                },
                _ => unsafe { ffi::lb_clipboard_deny(lb) },
            },
            Pending::Pick(token) => match result {
                Ok(ServiceReply::Files(files)) => {
                    let joined = files
                        .iter()
                        .map(|p| p.to_string_lossy().replace('\n', " "))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let joined = c_string(&joined);
                    unsafe { ffi::lb_pick_reply(token, joined.as_ptr()) };
                }
                _ => unsafe { ffi::lb_pick_reply(token, std::ptr::null()) },
            },
            Pending::Open(token) => unsafe {
                ffi::lb_open_external_reply(
                    token,
                    c_int::from(matches!(result, Ok(ServiceReply::Done))),
                );
            },
        }
    }

    fn input_pointer(&mut self, id: ViewId, event: &PointerEvent) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let metrics = view.target.metrics();
        let device = |position: SurfacePoint| {
            let point = metrics.to_buffer(position);
            (point.x.round() as c_int, point.y.round() as c_int)
        };
        let lb = view.lb;
        match event {
            PointerEvent::Enter { position } => {
                let (x, y) = device(*position);
                unsafe { ffi::lb_input_pointer_move(lb, x, y, view.pointer_buttons, 0) };
            }
            PointerEvent::Leave => unsafe { ffi::lb_input_pointer_leave(lb) },
            PointerEvent::Motion {
                position,
                modifiers,
            } => {
                let (x, y) = device(*position);
                unsafe {
                    ffi::lb_input_pointer_move(
                        lb,
                        x,
                        y,
                        view.pointer_buttons,
                        modifier_mask(modifiers),
                    )
                };
            }
            PointerEvent::Button {
                button,
                state,
                position,
                modifiers,
            } => {
                let (x, y) = device(*position);
                let code = match button {
                    PointerButton::Left => 1,
                    PointerButton::Right => 2,
                    PointerButton::Middle => 4,
                    PointerButton::Back => 8,
                    PointerButton::Forward => 16,
                    PointerButton::Other(_) => return,
                };
                let pressed = *state == ButtonState::Pressed;
                let mut clicks = 1;
                if pressed {
                    let now = Instant::now();
                    if let Some((at, lx, ly, lbutton, count)) = view.last_click {
                        if lbutton == *button
                            && now.duration_since(at) < MULTI_CLICK
                            && (lx - x).abs() <= MULTI_CLICK_SLOP
                            && (ly - y).abs() <= MULTI_CLICK_SLOP
                        {
                            clicks = count + 1;
                        }
                    }
                    view.last_click = Some((now, x, y, *button, clicks));
                    view.pointer_buttons |= code;
                } else {
                    clicks = view.last_click.map_or(1, |c| c.4);
                    view.pointer_buttons &= !code;
                }
                unsafe {
                    ffi::lb_input_pointer_button(
                        lb,
                        x,
                        y,
                        code,
                        view.pointer_buttons,
                        c_int::from(pressed),
                        clicks,
                        modifier_mask(modifiers),
                    )
                };
            }
            PointerEvent::Scroll {
                delta,
                position,
                modifiers,
            } => {
                let (x, y) = device(*position);
                let (dx, dy, precise) = match delta {
                    ScrollDelta::Pixels { x, y } => (
                        x * metrics.scale().factor(),
                        y * metrics.scale().factor(),
                        1,
                    ),
                    ScrollDelta::Wheel { x, y } => (f64::from(*x), f64::from(*y), 0),
                };
                unsafe {
                    ffi::lb_input_scroll(lb, x, y, dx, dy, precise, modifier_mask(modifiers))
                };
            }
        }
    }

    fn input_key(&mut self, id: ViewId, event: &KeyEvent) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        match event.state {
            ButtonState::Pressed if !view.keys.iter().any(|k| k.code == event.code) => {
                view.keys.push(event.clone())
            }
            ButtonState::Released => view.keys.retain(|k| k.code != event.code),
            ButtonState::Pressed => {}
        }
        let code = c_string(event.code.w3c_code().unwrap_or(""));
        let key = c_string(match &event.key {
            sighurt_api::input::Key::Character(text) | sighurt_api::input::Key::Named(text) => text,
            _ => "",
        });
        let code_point = event
            .text
            .as_deref()
            .and_then(|text| text.chars().next())
            .map_or(0, u32::from);
        unsafe {
            ffi::lb_input_key(
                view.lb,
                code.as_ptr(),
                key.as_ptr(),
                c_int::from(event.state == ButtonState::Pressed),
                c_int::from(event.repeat),
                modifier_mask(&event.modifiers),
                code_point,
            )
        };
    }
}

/// Whether going from `from` to `to` is HTML's "navigate to a fragment": the same address but for a
/// fragment, which `to` has.
fn is_fragment_navigation(from: &str, to: &str) -> bool {
    let without = |url: &str| {
        url.split_once('#')
            .map_or(url.to_owned(), |(head, _)| head.to_owned())
    };
    !from.is_empty() && to.contains('#') && without(from) == without(to)
}

/// A service request for `view`'s current document.
fn request<T: CpuTarget>(id: ViewId, view: &View<T>, service: Service) -> ServiceRequest {
    let origin = url::Url::parse(&view.url)
        .ok()
        .map(|url| url.origin().ascii_serialization())
        .filter(|origin| origin != "null");
    ServiceRequest {
        id: RequestId::next(),
        view: id,
        document: view.document,
        origin,
        service,
    }
}

/// `x,y,width,height` from the shim.
fn parse_caret(text: &str) -> [i32; 4] {
    let mut out = [0; 4];
    for (slot, part) in out.iter_mut().zip(text.split(',')) {
        *slot = part.parse().unwrap_or(0);
    }
    out
}

/// What settles an address change that is waiting.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Settle {
    /// A load start or Ladybird's commit: a new document.
    NewDocument,
    /// Time passed with nothing following.
    Timeout,
    /// Something else happened (loading ended, another change): no new document is coming for it.
    Ended,
}

/// Settles the address change `view` reported last: as a new document (its load start or commit
/// came) or as the page's own change. A new document takes the navigation in flight, supersedes
/// what was held, and gets a new generation; a page's own change keeps the document, and is only
/// credited to a navigation of the browser's when it is that navigation (HTML's fragment navigation, or the
/// browser's own address), never to one it merely overlaps.
fn settle_commit<T: CpuTarget>(
    id: ViewId,
    view: &mut View<T>,
    queue: &mut Vec<EngineEvent>,
    held: &mut HashMap<RequestId, (ViewId, i64)>,
    how: Settle,
) {
    let Some((url, _)) = view.deferred.as_ref() else {
        return;
    };
    // Ladybird shows the address of a navigation the browser asked for as soon as it asked (before
    // anything is loaded, and also when the load is then refused or fails), so an address change
    // while a navigation to another document is in flight says nothing until that document
    // commits. It is the page's own change only when nothing is loading, or when it is a navigation
    // to a fragment of the document shown.
    // (Ladybird does not always report the loading state turning on: it can be on already. The
    // address the browser itself asked for, to another document, is that same optimistic address.)
    let asked_by_browser = view.pending.as_ref().is_some_and(
        |(_, pend)| matches!(pend, Pend::Url(asked) if asked == url && !is_fragment_navigation(&view.url, asked)),
    );
    let own_change =
        (view.active.is_none() && !asked_by_browser) || is_fragment_navigation(&view.url, url);
    match how {
        Settle::Timeout if !own_change => return,
        Settle::Ended if !own_change => {
            view.deferred = None;
            if asked_by_browser {
                // The browser's navigation never committed (refused or failed): it fails.
                if let Some((navigation, _)) = view.pending.take() {
                    queue.push(EngineEvent::NavigationFailed {
                        view: id,
                        navigation,
                        error: if view.stopped {
                            NavigationError::Cancelled
                        } else {
                            NavigationError::Load("the page did not load".into())
                        },
                    });
                }
            }
            return;
        }
        _ => {}
    }
    let (url, _) = view.deferred.take().expect("checked");
    let new_document = how == Settle::NewDocument;
    let navigation = if new_document {
        view.document = DocumentGeneration::next();
        deny_held(held, id);
        view.active
            .take()
            .or_else(|| view.pending.take().map(|(navigation, _)| navigation))
            .unwrap_or_else(NavigationId::next)
    } else if view.active.is_some() && is_fragment_navigation(&view.url, &url) {
        view.active.take().expect("checked")
    } else if view.pending.as_ref().is_some_and(|(_, pend)| match pend {
        Pend::Url(asked) => {
            *asked == url
                || (is_fragment_navigation(&view.url, &url)
                    && is_fragment_navigation(&view.url, asked))
        }
        Pend::Traverse => true,
        Pend::Reload => false,
    }) {
        view.pending.take().expect("checked").0
    } else {
        NavigationId::next()
    };
    view.url.clone_from(&url);
    queue.push(EngineEvent::NavigationCommitted {
        view: id,
        navigation,
        url,
        document: view.document,
        history: None,
    });
}

/// The replaceable events of the contract: only the latest of each kind per view matters.
fn replaceable(event: &EngineEvent) -> Option<(u8, ViewId)> {
    Some(match event {
        EngineEvent::TitleChanged { view, .. } => (0, *view),
        EngineEvent::StatusTextChanged { view, .. } => (1, *view),
        EngineEvent::CursorChanged { view, .. } => (2, *view),
        EngineEvent::LoadingChanged { view, .. } => (3, *view),
        EngineEvent::HistoryChanged { view, .. } => (4, *view),
        EngineEvent::TextInputChanged { view, .. } => (5, *view),
        EngineEvent::FrameReady { view } => (6, *view),
        _ => return None,
    })
}

/// The most often a view's title, status, cursor, history or text input reaches the browser: a page
/// changing one in a loop costs it a bounded number of events a second.
const THROTTLE: Duration = Duration::from_millis(50);

/// Whether `event` is throttled, and its key.
fn throttled(event: &EngineEvent) -> Option<(u8, ViewId)> {
    replaceable(event).filter(|(kind, _)| !matches!(kind, 3 | 6))
}

/// Keeps the last of each replaceable event per view, in its own place; the others stay in order.
fn coalesce(queue: Vec<EngineEvent>) -> Vec<EngineEvent> {
    let mut seen = std::collections::HashSet::new();
    let mut kept: Vec<EngineEvent> = queue
        .into_iter()
        .rev()
        .filter(|event| replaceable(event).is_none_or(|key| seen.insert(key)))
        .collect();
    kept.reverse();
    kept
}

/// Denies every navigation held for `view`: its document is replaced or it is closing.
fn deny_held(held: &mut HashMap<RequestId, (ViewId, i64)>, view: ViewId) {
    held.retain(|_, (owner, token)| {
        if *owner == view {
            unsafe { ffi::lb_navigation_answer(*token, 0) };
            false
        } else {
            true
        }
    });
}

fn modifier_mask(modifiers: &Modifiers) -> c_int {
    c_int::from(modifiers.alt)
        | c_int::from(modifiers.control) << 1
        | c_int::from(modifiers.shift) << 2
        | c_int::from(modifiers.logo) << 3
}

/// Ladybird's `Gfx::StandardCursor`, by its enumerator's number.
fn cursor_for(standard: i64) -> Cursor {
    match standard {
        0 | 1 => Cursor::None,
        3 => Cursor::Crosshair,
        4 => Cursor::Text,
        5 => Cursor::EwResize,
        6 => Cursor::NsResize,
        7 => Cursor::NwseResize,
        8 => Cursor::NeswResize,
        9 => Cursor::ColResize,
        10 => Cursor::RowResize,
        11 => Cursor::Pointer,
        12 => Cursor::Help,
        13 => Cursor::Grab,
        14 | 15 => Cursor::Grabbing,
        16 => Cursor::Move,
        17 => Cursor::Wait,
        18 => Cursor::NotAllowed,
        20 => Cursor::ZoomIn,
        _ => Cursor::Default,
    }
}

impl<T: CpuTarget> EngineRuntime for LadybirdRuntime<T> {
    type Target = T;
    type FramePermit = T::Permit;

    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: T,
    ) -> Result<(), EngineError> {
        self.running()?;
        if self.views.contains_key(&view) {
            return Err(EngineError::View {
                view,
                message: "this view is already open".into(),
            });
        }
        let metrics = target.metrics();
        log::debug!("ladybird: open {view:?} {options:?} {:?}", metrics.buffer());
        let size = metrics.buffer();
        let lb = unsafe {
            ffi::lb_view_open(
                size.width.max(1) as c_int,
                size.height.max(1) as c_int,
                metrics.scale().factor(),
                c_int::from(options.visible),
                options.zoom,
            )
        };
        self.by_lb.insert(lb, view);
        self.views.insert(
            view,
            View {
                target,
                lb,
                ready: false,
                visible: options.visible,
                document: DocumentGeneration::next(),
                pending: None,
                deferred: None,
                active: None,
                stopped: false,
                frame_dirty: false,
                last_click: None,
                pointer_buttons: 0,
                keys: Vec::new(),
                history: None,
                url: String::new(),
                clipboard: Vec::new(),
            },
        );
        Ok(())
    }

    fn command(&mut self, id: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.running()?;
        log::debug!("ladybird: {id:?} <- {command:?}");
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        let lb = view.lb;
        match command {
            EngineCommand::Navigate { navigation, url } => {
                view.pending = Some((navigation, Pend::Url(url.clone())));
                view.stopped = false;
                let url = c_string(&url);
                unsafe { ffi::lb_navigate(lb, url.as_ptr()) };
            }
            EngineCommand::Reload { navigation } => {
                view.pending = Some((navigation, Pend::Reload));
                view.stopped = false;
                unsafe { ffi::lb_reload(lb) };
            }
            EngineCommand::Stop => {
                view.stopped = true;
                unsafe { ffi::lb_stop(lb) };
            }
            EngineCommand::Traverse { navigation, delta } => {
                view.pending = Some((navigation, Pend::Traverse));
                view.stopped = false;
                unsafe { ffi::lb_traverse(lb, delta) };
            }
            EngineCommand::SetMetrics(metrics) => self.set_metrics(id, &metrics),
            EngineCommand::SetVisibility(visible) => {
                view.visible = visible;
                unsafe { ffi::lb_set_visible(lb, c_int::from(visible)) };
            }
            EngineCommand::SetFocus(focused) => self.set_focus(id, focused),
            EngineCommand::SetZoom(zoom) => unsafe { ffi::lb_set_zoom(lb, zoom) },
            EngineCommand::Input(event) => match &event.kind {
                InputKind::Pointer(pointer) => self.input_pointer(id, pointer),
                InputKind::Key(key) => self.input_key(id, key),
                InputKind::TextInput(text) => {
                    // The commit replaces the composition and ends it; then the new composition,
                    // where none (or an empty one) clears what is shown: Ladybird ends a composition
                    // on empty marked text, and ignores it when none is going on.
                    if let Some(commit) = &text.commit {
                        let commit = c_string(commit);
                        unsafe { ffi::lb_input_commit_text(lb, commit.as_ptr()) };
                    }
                    let preedit = c_string(text.preedit.as_ref().map_or("", |p| &p.text));
                    unsafe { ffi::lb_input_preedit(lb, preedit.as_ptr()) };
                }
                InputKind::Focus(focus) => {
                    self.set_focus(id, matches!(focus, FocusEvent::Activated))
                }
            },
            EngineCommand::ResolveServiceRequest { request, result } => {
                self.resolve(request, result);
            }
            EngineCommand::AnswerNavigation { request, allow } => {
                // An answer for a request that is no longer held does nothing.
                if let Some((_, token)) = self.held.remove(&request) {
                    unsafe { ffi::lb_navigation_answer(token, c_int::from(allow)) };
                }
            }
            EngineCommand::SetSemantics(_) | EngineCommand::Semantic { .. } => {
                return Err(EngineError::Unsupported("semantics"))
            }
        }
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.shut_down {
            return PumpStatus::Idle;
        }
        if let Some(waiter) = &self.waiter {
            waiter.pumping();
        }
        let mut more: c_int = 0;
        let next_ms = unsafe { ffi::lb_pump(&mut more) };
        let mut drained = 0;
        loop {
            let mut raw = ffi::Event {
                kind: 0,
                view: 0,
                i0: 0,
                i1: 0,
                s0: std::ptr::null(),
                s1: std::ptr::null(),
                data: std::ptr::null(),
                len: 0,
            };
            if unsafe { ffi::lb_next_event(&mut raw) } == 0 {
                break;
            }
            self.translate(&raw);
            drained += 1;
            if drained >= MAX_EVENTS_PER_PUMP {
                more = 1;
                break;
            }
        }
        // An address change that was not followed by a load start within the settle time is the page's own.
        let now = Instant::now();
        let mut settle_at: Option<Instant> = None;
        for (id, view) in &mut self.views {
            if let Some((_, since)) = &view.deferred {
                if now.duration_since(*since) >= COMMIT_SETTLE {
                    settle_commit(*id, view, &mut self.queue, &mut self.held, Settle::Timeout);
                } else {
                    let due = *since + COMMIT_SETTLE;
                    settle_at = Some(settle_at.map_or(due, |at| at.min(due)));
                }
            }
        }
        let batch = coalesce(std::mem::take(&mut self.queue));
        let (sent, release_at) = self.throttle.pass(batch, now);
        events.extend(sent);
        let settle_at = match (settle_at, release_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let timer = (next_ms >= 0).then(|| Instant::now() + Duration::from_millis(next_ms as u64));
        let deadline = match (timer, settle_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if more != 0 {
            PumpStatus::Pending { deadline: None }
        } else if deadline.is_some() {
            PumpStatus::Pending { deadline }
        } else {
            PumpStatus::Idle
        }
    }

    fn present(&mut self, id: ViewId, permit: T::Permit) -> Result<PresentOutcome, EngineError> {
        self.running()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        if !view.ready || !view.frame_dirty {
            return Ok(PresentOutcome::Unchanged);
        }
        let mut frame = ffi::Frame {
            pixels: std::ptr::null(),
            width: 0,
            height: 0,
            stride: 0,
            dmabuf: 0,
        };
        if unsafe { ffi::lb_frame_get(view.lb, &mut frame) } == 0 {
            return Ok(PresentOutcome::NotReady);
        }
        let mut not_ready = false;
        let lb = &frame;
        view.target
            .submit(permit, &mut |out| {
                // Ladybird renders at the size it was last told: until it has painted the
                // permit's, there is nothing to show for it.
                if lb.width as u32 != out.size.width || lb.height as u32 != out.size.height {
                    not_ready = true;
                    return Err("the frame is for another size".into());
                }
                let row = 4 * lb.width as usize;
                for y in 0..lb.height as usize {
                    // The frame is BGRA premultiplied, as the target wants it.
                    let source =
                        unsafe { std::slice::from_raw_parts(lb.pixels.add(y * lb.stride), row) };
                    out.pixels[y * out.stride..y * out.stride + row].copy_from_slice(source);
                }
                Ok(())
            })
            .or_else(|message| match not_ready {
                true => Ok(()),
                false => Err(EngineError::View { view: id, message }),
            })?;
        if not_ready {
            return Ok(PresentOutcome::NotReady);
        }
        view.frame_dirty = false;
        self.diagnostics.frames_copied += 1;
        Ok(PresentOutcome::Submitted)
    }

    fn close_view(&mut self, id: ViewId) {
        if self.remove_view(id) {
            // The browser closed it: what it had not been told is dropped.
            self.queue.retain(|event| event.view() != Some(id));
            self.throttle.forget(id);
            self.queue.push(EngineEvent::ViewClosed { view: id });
        }
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        if let Some(mut waiter) = self.waiter.take() {
            waiter.stop();
        }
        // What is held is denied, then views (their targets), then Ladybird itself.
        for (_, (_, token)) in self.held.drain() {
            unsafe { ffi::lb_navigation_answer(token, 0) };
        }
        self.views.clear();
        self.by_lb.clear();
        self.queue.clear();
        self.throttle = Throttle::default();
        unsafe { ffi::lb_shutdown() };
        if self.temporary_profile {
            if let Some(profile) = self.profile.take() {
                let _ = std::fs::remove_dir_all(profile);
            }
        }
    }
}

impl<T: CpuTarget> LadybirdRuntime<T> {
    /// Closes the view in Ladybird and forgets it; what it was waiting for is refused, so nothing
    /// is left holding a token. Whether it was open.
    fn remove_view(&mut self, id: ViewId) -> bool {
        let Some(view) = self.views.remove(&id) else {
            return false;
        };
        self.by_lb.remove(&view.lb);
        unsafe { ffi::lb_view_close(view.lb) };
        let waiting: Vec<RequestId> = self
            .services
            .iter()
            .filter(|(_, (v, _))| *v == id)
            .map(|(request, _)| *request)
            .collect();
        for request in waiting {
            self.resolve(request, Err(ServiceError::Cancelled));
        }
        deny_held(&mut self.held, id);
        true
    }

    /// Gives or takes the view's focus. Losing it first releases every key its content holds as
    /// pressed, last pressed first, while the content still has the focus to receive the key-ups
    /// (the contract's `SetFocus(false)` and `FocusEvent::Deactivated`). Buttons stay held.
    fn set_focus(&mut self, id: ViewId, focused: bool) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let lb = view.lb;
        if !focused {
            for key in std::mem::take(&mut view.keys).into_iter().rev() {
                let release = KeyEvent {
                    state: ButtonState::Released,
                    repeat: false,
                    modifiers: Modifiers::default(),
                    text: None,
                    ..key
                };
                self.input_key(id, &release);
            }
        }
        unsafe { ffi::lb_set_focus(lb, c_int::from(focused)) };
    }

    fn set_metrics(&mut self, id: ViewId, metrics: &Metrics) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let size = metrics.buffer();
        if metrics.is_empty() {
            // "No room": the view keeps its last size and presents nothing.
            return;
        }
        unsafe {
            ffi::lb_set_viewport(
                view.lb,
                size.width as c_int,
                size.height as c_int,
                metrics.scale().factor(),
            )
        };
        // The next frame, at the new size, is what the host's permit will want.
        view.frame_dirty = false;
    }
}

impl<T: CpuTarget> Drop for LadybirdRuntime<T> {
    fn drop(&mut self) {
        if !self.shut_down {
            self.shutdown();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(view: ViewId, text: &str) -> EngineEvent {
        EngineEvent::TitleChanged {
            view,
            title: Some(text.into()),
        }
    }

    fn text_input(view: ViewId) -> EngineEvent {
        EngineEvent::TextInputChanged { view, state: None }
    }

    fn commit(view: ViewId) -> EngineEvent {
        EngineEvent::NavigationCommitted {
            view,
            navigation: NavigationId::next(),
            url: "https://example.org/".into(),
            document: DocumentGeneration::next(),
            history: None,
        }
    }

    fn kinds(events: &[EngineEvent]) -> Vec<&'static str> {
        events
            .iter()
            .map(|e| match e {
                EngineEvent::TitleChanged { .. } => "title",
                EngineEvent::TextInputChanged { .. } => "text-input",
                EngineEvent::NavigationCommitted { .. } => "commit",
                EngineEvent::FrameReady { .. } => "frame",
                EngineEvent::ViewClosed { .. } => "closed",
                _ => "other",
            })
            .collect()
    }

    #[test]
    fn a_burst_is_throttled_to_its_latest() {
        let mut throttle = Throttle::default();
        let view = ViewId::next();
        let now = Instant::now();
        let (sent, next) = throttle.pass(vec![title(view, "a")], now);
        assert_eq!(kinds(&sent), ["title"]);
        assert_eq!(next, None);
        let (sent, next) = throttle.pass(vec![title(view, "b"), title(view, "c")], now);
        assert!(sent.is_empty());
        assert_eq!(next, Some(now + THROTTLE));
        let (sent, next) = throttle.pass(Vec::new(), now + THROTTLE);
        assert!(
            matches!(&sent[..], [EngineEvent::TitleChanged { title: Some(t), .. }] if t == "c")
        );
        assert_eq!(next, None);
    }

    #[test]
    fn what_waits_goes_before_the_next_commit() {
        let mut throttle = Throttle::default();
        let view = ViewId::next();
        let other = ViewId::next();
        let now = Instant::now();
        throttle.pass(vec![text_input(view), title(other, "a")], now);
        // Held: the second text input and the other view's title.
        let (sent, _) = throttle.pass(vec![text_input(view), title(other, "b")], now);
        assert!(sent.is_empty());
        // A frame is not a barrier; the commit is, for its own view only.
        let (sent, next) = throttle.pass(
            vec![EngineEvent::FrameReady { view }, commit(view)],
            now + THROTTLE / 2,
        );
        assert_eq!(kinds(&sent), ["frame", "text-input", "commit"]);
        assert_eq!(next, Some(now + THROTTLE));
        // The text input after the commit is throttled again, from the flush.
        let (sent, _) = throttle.pass(vec![text_input(view)], now + THROTTLE / 2);
        assert!(sent.is_empty());
    }

    #[test]
    fn nothing_of_a_closed_view_comes_after_its_closing() {
        let view = ViewId::next();
        let now = Instant::now();
        // Closed by its page: what waits goes first.
        let mut throttle = Throttle::default();
        throttle.pass(vec![title(view, "a")], now);
        throttle.pass(vec![title(view, "b")], now);
        let (sent, next) = throttle.pass(vec![EngineEvent::ViewClosed { view }], now);
        assert_eq!(kinds(&sent), ["title", "closed"]);
        assert_eq!(next, None);
        assert!(throttle.last_sent.is_empty() && throttle.waiting.is_empty());
        // Closed by the browser: what waits is dropped.
        let mut throttle = Throttle::default();
        throttle.pass(vec![title(view, "a")], now);
        throttle.pass(vec![title(view, "b")], now);
        throttle.forget(view);
        let (sent, next) = throttle.pass(Vec::new(), now + THROTTLE);
        assert!(sent.is_empty());
        assert_eq!(next, None);
        assert!(throttle.last_sent.is_empty());
    }
}
