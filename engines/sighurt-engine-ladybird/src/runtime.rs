//! `LadybirdRuntime`: the `EngineRuntime` over the shim.
//!
//! Owns: the runtime's views, the translation of Ladybird's callbacks (as the shim reports them) into
//! the contract's events, and presenting frames into the host's CPU target. Must not own: how
//! Ladybird's loop works (the shim) or when the host pumps (it gets a deadline and a waker).

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
    pending: Option<NavigationId>,
    /// A navigation Ladybird started that has not committed.
    active: Option<NavigationId>,
    /// Whether the browser stopped the active navigation.
    stopped: bool,
    /// A frame is ready and `FrameReady` was reported for it.
    frame_reported: bool,
    /// A frame arrived since the last submission.
    frame_dirty: bool,
    last_click: Option<(Instant, i32, i32, PointerButton, i32)>,
    pointer_buttons: i32,
    history: Option<(Vec<String>, usize)>,
    /// The address of the document shown.
    url: String,
    /// A page's copy, representation by representation, until the last one has come.
    clipboard: Vec<ClipboardData>,
}

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

/// Most bytes of text a paste takes.
const MAX_PASTE: usize = 1024 * 1024;

/// The Ladybird engine's runtime. One per process, on the owner thread.
pub struct LadybirdRuntime<T: CpuTarget> {
    views: HashMap<ViewId, View<T>>,
    by_lb: HashMap<u64, ViewId>,
    waiter: Option<Waiter>,
    queue: Vec<EngineEvent>,
    services: HashMap<RequestId, (ViewId, Pending)>,
    profile: Option<PathBuf>,
    temporary_profile: bool,
    shut_down: bool,
    _not_send: std::marker::PhantomData<*const ()>,
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
                let path = base.join(format!("sighurt-ladybird-{}", std::process::id()));
                std::fs::create_dir_all(&path)
                    .map_err(|e| format!("cannot make {}: {e}", path.display()))?;
                (path, true)
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
        let waiter = Waiter::start(fd, waker)?;
        Ok(Self {
            views: HashMap::new(),
            by_lb: HashMap::new(),
            waiter: Some(waiter),
            queue: Vec::new(),
            services: HashMap::new(),
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

    fn running(&self) -> Result<(), EngineError> {
        match self.shut_down {
            true => Err(EngineError::Runtime(
                "the Ladybird runtime is shut down".into(),
            )),
            false => Ok(()),
        }
    }

    fn translate(&mut self, event: &ffi::Event) {
        let Some(&id) = self.by_lb.get(&event.view) else {
            return;
        };
        let Some(view) = self.views.get_mut(&id) else {
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
            ffi::EV_LOAD_START | ffi::EV_LOAD_FINISH => {}
            ffi::EV_LOADING => {
                let loading = event.i0 != 0;
                if loading {
                    if view.active.is_none() {
                        let navigation = view.pending.take().unwrap_or_else(NavigationId::next);
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
                let url = s0.unwrap_or_default();
                // A commit while a navigation is active is a new document; one with nothing
                // loading is the page changing its own address (a fragment, pushState).
                // The exception is a navigation to a fragment of the document shown (HTML's
                // "navigate to a fragment"): Ladybird reports it as loading, but it replaces
                // nothing (the shim ends the loading state it leaves on).
                let (navigation, new_document) = match view.active.take() {
                    Some(navigation) if is_fragment_navigation(&view.url, &url) => {
                        (navigation, false)
                    }
                    Some(navigation) => (navigation, true),
                    None => (
                        view.pending.take().unwrap_or_else(NavigationId::next),
                        false,
                    ),
                };
                if new_document {
                    view.document = DocumentGeneration::next();
                }
                view.url.clone_from(&url);
                self.queue.push(EngineEvent::NavigationCommitted {
                    view: id,
                    navigation,
                    url,
                    document: view.document,
                    history: None,
                });
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
                if !view.frame_reported {
                    view.frame_reported = true;
                    self.queue.push(EngineEvent::FrameReady { view: id });
                }
            }
            ffi::EV_CRASHED => self.queue.push(EngineEvent::ViewFailed {
                view: id,
                message: s0.unwrap_or_else(|| "the page crashed".into()),
            }),
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
                        purpose: TextPurpose::Normal,
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
                let bytes = if event.data.is_null() {
                    Vec::new()
                } else {
                    unsafe { std::slice::from_raw_parts(event.data.cast::<u8>(), event.len) }
                        .to_vec()
                };
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
                unsafe { ffi::lb_input_pointer_move(lb, x, y, 0) };
            }
            PointerEvent::Leave => unsafe { ffi::lb_input_pointer_leave(lb) },
            PointerEvent::Motion {
                position,
                modifiers,
            } => {
                let (x, y) = device(*position);
                unsafe { ffi::lb_input_pointer_move(lb, x, y, modifier_mask(modifiers)) };
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
        let Some(view) = self.views.get(&id) else {
            return;
        };
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
                active: None,
                stopped: false,
                frame_reported: false,
                frame_dirty: false,
                last_click: None,
                pointer_buttons: 0,
                history: None,
                url: String::new(),
                clipboard: Vec::new(),
            },
        );
        Ok(())
    }

    fn command(&mut self, id: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.running()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        let lb = view.lb;
        match command {
            EngineCommand::Navigate { navigation, url } => {
                view.pending = Some(navigation);
                view.stopped = false;
                let url = c_string(&url);
                unsafe { ffi::lb_navigate(lb, url.as_ptr()) };
            }
            EngineCommand::Reload { navigation } => {
                view.pending = Some(navigation);
                view.stopped = false;
                unsafe { ffi::lb_reload(lb) };
            }
            EngineCommand::Stop => {
                view.stopped = true;
                unsafe { ffi::lb_stop(lb) };
            }
            EngineCommand::Traverse { navigation, delta } => {
                view.pending = Some(navigation);
                view.stopped = false;
                unsafe { ffi::lb_traverse(lb, delta) };
            }
            EngineCommand::SetMetrics(metrics) => self.set_metrics(id, &metrics),
            EngineCommand::SetVisibility(visible) => {
                view.visible = visible;
                unsafe { ffi::lb_set_visible(lb, c_int::from(visible)) };
            }
            EngineCommand::SetFocus(focused) => unsafe {
                ffi::lb_set_focus(lb, c_int::from(focused))
            },
            EngineCommand::SetZoom(zoom) => unsafe { ffi::lb_set_zoom(lb, zoom) },
            EngineCommand::Input(event) => match &event.kind {
                InputKind::Pointer(pointer) => self.input_pointer(id, pointer),
                InputKind::Key(key) => self.input_key(id, key),
                InputKind::TextInput(text) => {
                    if let Some(preedit) = &text.preedit {
                        let preedit = c_string(&preedit.text);
                        unsafe { ffi::lb_input_preedit(lb, preedit.as_ptr()) };
                    }
                    if let Some(commit) = &text.commit {
                        let commit = c_string(commit);
                        unsafe { ffi::lb_input_commit_text(lb, commit.as_ptr()) };
                    }
                }
                InputKind::Focus(focus) => unsafe {
                    ffi::lb_set_focus(lb, c_int::from(matches!(focus, FocusEvent::Activated)))
                },
            },
            EngineCommand::ResolveServiceRequest { request, result } => {
                self.resolve(request, result);
            }
            // The engine never asks about navigations (docs/rewrite/LADYBIRD.md).
            EngineCommand::AnswerNavigation { .. } => {}
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
        events.append(&mut self.queue);
        if more != 0 {
            PumpStatus::Pending { deadline: None }
        } else if next_ms >= 0 {
            PumpStatus::Pending {
                deadline: Some(Instant::now() + Duration::from_millis(next_ms as u64)),
            }
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
        view.frame_reported = false;
        Ok(PresentOutcome::Submitted)
    }

    fn close_view(&mut self, id: ViewId) {
        if let Some(view) = self.views.remove(&id) {
            self.by_lb.remove(&view.lb);
            unsafe { ffi::lb_view_close(view.lb) };
            self.queue.retain(|event| event.view() != Some(id));
            // What the view was waiting for is refused, so nothing is left holding a token.
            let waiting: Vec<RequestId> = self
                .services
                .iter()
                .filter(|(_, (v, _))| *v == id)
                .map(|(request, _)| *request)
                .collect();
            for request in waiting {
                self.resolve(request, Err(ServiceError::Cancelled));
            }
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
        // Views first (their targets), then Ladybird itself.
        self.views.clear();
        self.by_lb.clear();
        self.queue.clear();
        unsafe { ffi::lb_shutdown() };
        if self.temporary_profile {
            if let Some(profile) = self.profile.take() {
                let _ = std::fs::remove_dir_all(profile);
            }
        }
    }
}

impl<T: CpuTarget> LadybirdRuntime<T> {
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
