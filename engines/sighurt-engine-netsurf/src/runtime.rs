//! [`NetSurfRuntime`]: NetSurf behind the engine contract.
//!
//! One runtime per process (NetSurf's state is global). Every call is on the owner thread, and
//! every callback NetSurf makes only queues an event in the C frontend; the runtime drains that
//! queue after each call into the core and hands the events over from `pump`.
//!
//! - **Time.** `pump` runs the core's due callbacks (`nsh_poll`), then reports the next deadline
//!   the core asked for, or none. The descriptors of its fetchers go to the [`Watcher`] thread,
//!   which wakes the owner when one is ready. Nothing ticks.
//! - **Navigation.** NetSurf has no navigation ids and no commit callback: the runtime
//!   synthesizes them. A browser navigation is announced when it is accepted and committed when
//!   the core replaces the content; one the page started (a link, a redirect, a refresh) is
//!   announced and committed together.
//! - **Links.** NetSurf cannot ask before it follows a link, so the runtime asks when the user
//!   releases the button on one (`NavigationRequested`) and sends the click on `allow`.
//! - **Services.** The clipboard, downloads, external URLs and file inputs are service requests;
//!   a paste key waits for the clipboard before the core sees it.
//! - **Painting.** On the CPU, a whole frame at a time into the lent buffer (the host's buffers
//!   rotate, so a dirty rectangle would not be enough); it is cheap (1 to 5 ms at 1024x768).
//!
//! Owns: the mapping between the contract and the C API. Must not own: tab policy, the clipboard
//! or any file access of its own, a timer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::mem::{discriminant, Discriminant};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, ViewOptions,
};
use sighurt_api::events::{EngineEvent, HistoryEffect, NavigationError, OpenDisposition};
use sighurt_api::geometry::{Metrics, SurfacePoint, SurfaceRect};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, RequestId, ViewId};
use sighurt_api::input::{InputKind, TextInputState, TextPurpose};
use sighurt_api::present::CpuTarget;
use sighurt_api::services::{
    ClipboardData, DownloadSource, PickMode, Selection, Service, ServiceReply, ServiceRequest,
    ServiceResult,
};
use sighurt_wayland::TargetLease;

use crate::ffi::{self, Handle};
use crate::input::{self, KeyAction, Pointer, PointerAction};
use crate::watcher::Watcher;
use crate::CAPABILITIES;

/// Events handed over by one `pump`; the rest wait for the next.
const MAX_EVENTS_PER_PUMP: usize = 256;
/// Navigation requests a view holds; a link click past this is performed without asking.
const MAX_HELD_NAVIGATIONS: usize = 16;
/// The most clipboard text a paste reads.
const MAX_PASTE: usize = 1 << 20;

/// What `[engines.netsurf]` and the profile give the runtime.
#[derive(Clone, Debug, Default)]
pub struct NetSurfConfig {
    /// `user-agent`: NetSurf's own when none.
    pub user_agent: Option<String>,
    /// The profile directory (reserved: the core keeps no cookie file yet).
    pub profile_dir: Option<PathBuf>,
}

/// A navigation the browser asked for that has not committed.
struct PendingNavigation {
    navigation: NavigationId,
    url: String,
    effect: HistoryEffect,
    /// The core reported the load starting. A stop before that is the previous load ending.
    started: bool,
}

/// A link click held for `AnswerNavigation`.
struct HeldClick {
    request: RequestId,
    x: i32,
    y: i32,
    state: u32,
    mods: u32,
}

/// What a service request this view made is for.
enum Asked {
    Paste,
    Files,
    Other,
}

struct View<T: CpuTarget> {
    // Closed explicitly (before the target is dropped); `None` only while closing.
    handle: Option<Handle>,
    target: T,
    metrics: Metrics,
    /// Buffer size the window was last given.
    size: (u32, u32),
    visible: bool,
    document: DocumentGeneration,
    /// The committed URL.
    url: Option<String>,
    /// The latest URL the core reported, committed or not.
    last_url: Option<String>,
    pending: Option<PendingNavigation>,
    loading: bool,
    dirty: bool,
    /// The layout epoch the core has caught up with (it reformats from `pump`).
    ready: Option<LayoutEpoch>,
    painted: Option<LayoutEpoch>,
    pointer: Pointer,
    caret: bool,
    held: Vec<HeldClick>,
    asked: HashMap<RequestId, Asked>,
}

/// NetSurf as an engine runtime.
pub struct NetSurfRuntime<T: CpuTarget = TargetLease> {
    views: BTreeMap<ViewId, View<T>>,
    /// The C side's view numbers (`ViewId::serial`) back to ids.
    ids: HashMap<u64, ViewId>,
    queue: Vec<EngineEvent>,
    watcher: Watcher,
    /// The view that last had input: external URLs and clipboard writes have no window of their own.
    active: Option<ViewId>,
    shut_down: bool,
}

fn ca_bundle() -> Option<std::ffi::CString> {
    let candidates = [
        std::env::var("SSL_CERT_FILE").ok(),
        Some("/etc/ssl/certs/ca-certificates.crt".into()),
        Some("/etc/ssl/certs/ca-bundle.crt".into()),
    ];
    candidates
        .into_iter()
        .flatten()
        .find(|path| !path.is_empty() && std::path::Path::new(path).is_file())
        .and_then(|path| std::ffi::CString::new(path).ok())
}

impl<T: CpuTarget> NetSurfRuntime<T> {
    /// Starts NetSurf. Fails if this process already did (one runtime per process).
    pub fn new(config: NetSurfConfig, waker: EngineWaker) -> Result<Self, EngineError> {
        let mut init = ffi::Init::from_env();
        init.user_agent = config
            .user_agent
            .and_then(|ua| std::ffi::CString::new(ua).ok());
        init.ca_bundle = ca_bundle();
        if init.ca_bundle.is_none() {
            log::warn!("netsurf: no CA bundle found (SSL_CERT_FILE): https pages will fail");
        }
        init.profile_dir = None;
        let _ = config.profile_dir;
        if ffi::init(&init) != 0 {
            return Err(EngineError::Runtime(
                "NetSurf could not start (one runtime per process)".into(),
            ));
        }
        let watcher = match Watcher::start(waker) {
            Ok(watcher) => watcher,
            Err(message) => {
                ffi::shutdown();
                return Err(EngineError::Runtime(message));
            }
        };
        Ok(Self {
            views: BTreeMap::new(),
            ids: HashMap::new(),
            queue: Vec::new(),
            watcher,
            active: None,
            shut_down: false,
        })
    }

    fn running(&self) -> Result<(), EngineError> {
        if self.shut_down {
            Err(EngineError::Runtime("NetSurf has shut down".into()))
        } else {
            Ok(())
        }
    }

    /// Moves what the core queued into `self.queue`, in order. Replaceable metadata (title,
    /// status, cursor, caret, frames) is applied after the ordered events of the batch, so a
    /// title the core reported just before it replaced the content belongs to the new document.
    fn collect(&mut self) {
        let mut later: Vec<ffi::OwnedEvent> = Vec::new();
        while let Some(event) = ffi::next_event() {
            match event.kind {
                ffi::EV_LOADING
                | ffi::EV_TITLE
                | ffi::EV_STATUS
                | ffi::EV_POINTER
                | ffi::EV_CARET
                | ffi::EV_REMOVE_CARET
                | ffi::EV_FRAME => later.push(event),
                _ => self.apply(event),
            }
        }
        for event in later {
            self.apply(event);
        }
        // A URL change with no new content and no load in flight is a same-document navigation
        // (a fragment): commit it, keeping the document.
        let ids: Vec<ViewId> = self.views.keys().copied().collect();
        for id in ids {
            let Some(view) = self.views.get_mut(&id) else {
                continue;
            };
            if view.loading || view.pending.is_some() {
                continue;
            }
            let (Some(last), Some(url)) = (view.last_url.clone(), view.url.clone()) else {
                continue;
            };
            if last == url {
                continue;
            }
            let navigation = NavigationId::next();
            view.url = Some(last.clone());
            let document = view.document;
            self.queue.push(EngineEvent::NavigationStarted {
                view: id,
                navigation,
                url: last.clone(),
            });
            self.queue.push(EngineEvent::NavigationCommitted {
                view: id,
                navigation,
                url: last,
                document,
                history: Some(HistoryEffect::Push),
            });
        }
    }

    /// The view an event without a window (a clipboard write, an external URL) goes to.
    fn service_view(&self) -> Option<ViewId> {
        self.active
            .filter(|id| self.views.contains_key(id))
            .or_else(|| self.views.keys().next().copied())
    }

    /// Queues a service request for `id`'s current document.
    fn request(&mut self, id: ViewId, service: Service, asked: Asked) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let request = RequestId::next();
        view.asked.insert(request, asked);
        let origin = view.url.as_deref().and_then(origin_of);
        self.queue
            .push(EngineEvent::ServiceRequested(ServiceRequest {
                id: request,
                view: id,
                document: view.document,
                origin,
                service,
            }));
    }

    fn apply(&mut self, event: ffi::OwnedEvent) {
        match event.kind {
            ffi::EV_LAUNCH => {
                if let (Some(url), Some(view)) = (event.text, self.service_view()) {
                    self.request(view, Service::OpenExternal { url }, Asked::Other);
                }
            }
            ffi::EV_CLIPBOARD => {
                if let (Some(text), Some(view)) = (event.text, self.service_view()) {
                    let items = vec![ClipboardData {
                        mime_type: "text/plain;charset=utf-8".into(),
                        data: text.into_bytes(),
                    }];
                    self.request(
                        view,
                        Service::ClipboardWrite {
                            selection: Selection::Clipboard,
                            items,
                        },
                        Asked::Other,
                    );
                }
            }
            ffi::EV_DOWNLOAD => {
                // 0: created, 1: finished, 2: failed. Only a finished one is handed over.
                if event.i0 != 1 {
                    return;
                }
                let Some(&id) = self.ids.get(&event.view_id) else {
                    // Its view is gone: release the buffered body.
                    let _ = ffi::take_download(event.u0);
                    return;
                };
                if let Some((_url, name, data)) = ffi::take_download(event.u0) {
                    self.request(
                        id,
                        Service::Download {
                            source: DownloadSource::Data(data),
                            suggested_name: name,
                        },
                        Asked::Other,
                    );
                }
            }
            ffi::EV_OPEN => {
                // `view_id` is the opener's.
                let Some(&id) = self.ids.get(&event.view_id) else {
                    return;
                };
                if let (Some(url), Some(view)) = (event.text, self.views.get(&id)) {
                    self.queue.push(EngineEvent::OpenRequested {
                        view: id,
                        document: view.document,
                        url,
                        disposition: OpenDisposition::NewView,
                    });
                }
            }
            ffi::EV_FILE => {
                if let Some(&id) = self.ids.get(&event.view_id) {
                    self.request(
                        id,
                        Service::PickFiles {
                            mode: PickMode::OpenFile,
                            filters: Vec::new(),
                        },
                        Asked::Files,
                    );
                }
            }
            _ => {
                let Some(&id) = self.ids.get(&event.view_id) else {
                    return;
                };
                self.apply_to_view(id, event);
            }
        }
    }

    fn apply_to_view(&mut self, id: ViewId, event: ffi::OwnedEvent) {
        let Some(mut view) = self.views.remove(&id) else {
            return;
        };
        match event.kind {
            ffi::EV_TITLE => self.queue.push(EngineEvent::TitleChanged {
                view: id,
                title: event.text.filter(|t| !t.is_empty()),
            }),
            ffi::EV_STATUS => self.queue.push(EngineEvent::StatusTextChanged {
                view: id,
                text: event.text.filter(|t| !t.is_empty()),
            }),
            ffi::EV_POINTER => self.queue.push(EngineEvent::CursorChanged {
                view: id,
                cursor: input::cursor(event.i0),
            }),
            ffi::EV_URL => view.last_url = event.text,
            ffi::EV_LOADING => {
                view.loading = event.i0 == 1;
                self.queue.push(EngineEvent::LoadingChanged {
                    view: id,
                    loading: view.loading,
                });
                if view.loading {
                    if let Some(pending) = &mut view.pending {
                        pending.started = true;
                    }
                } else if view.pending.as_ref().is_some_and(|p| p.started) {
                    // Stopped with the navigation never committed: it was cancelled (or the
                    // core had nothing to show for it).
                    if let Some(pending) = view.pending.take() {
                        self.queue.push(EngineEvent::NavigationFailed {
                            view: id,
                            navigation: pending.navigation,
                            error: NavigationError::Cancelled,
                        });
                    }
                }
            }
            ffi::EV_NEW_CONTENT => self.commit(id, &mut view),
            ffi::EV_FRAME => view.dirty = true,
            ffi::EV_CARET => {
                let ratio = scale_ratio(&view.metrics);
                let at = |n: i32| (f64::from(n) / ratio).round() as i32;
                view.caret = true;
                self.queue.push(EngineEvent::TextInputChanged {
                    view: id,
                    state: Some(TextInputState {
                        caret: SurfaceRect {
                            x: at(event.i0),
                            y: at(event.i1),
                            width: 1,
                            height: at(event.i2).max(1) as u32,
                        },
                        surrounding: None,
                        purpose: TextPurpose::Normal,
                        multiline: false,
                    }),
                });
            }
            ffi::EV_REMOVE_CARET if std::mem::take(&mut view.caret) => {
                self.queue.push(EngineEvent::TextInputChanged {
                    view: id,
                    state: None,
                });
            }
            _ => {}
        }
        self.views.insert(id, view);
    }

    /// The core replaced the view's content: a new document commits.
    fn commit(&mut self, id: ViewId, view: &mut View<T>) {
        let pending = view.pending.take();
        let url = view
            .last_url
            .clone()
            .or_else(|| pending.as_ref().map(|p| p.url.clone()))
            .or_else(|| view.url.clone())
            .unwrap_or_default();
        let (navigation, effect) = match pending {
            Some(p) => (p.navigation, p.effect),
            None => {
                // The page started it (a link, a redirect, a refresh).
                let navigation = NavigationId::next();
                self.queue.push(EngineEvent::NavigationStarted {
                    view: id,
                    navigation,
                    url: url.clone(),
                });
                (navigation, HistoryEffect::Push)
            }
        };
        view.document = DocumentGeneration::next();
        view.url = Some(url.clone());
        // What the old document held is dead with it.
        view.held.clear();
        view.asked.retain(|_, asked| matches!(asked, Asked::Other));
        if std::mem::take(&mut view.caret) {
            self.queue.push(EngineEvent::TextInputChanged {
                view: id,
                state: None,
            });
        }
        self.queue.push(EngineEvent::NavigationCommitted {
            view: id,
            navigation,
            url,
            document: view.document,
            history: Some(effect),
        });
    }

    fn send_pointer(view: &View<T>, action: &PointerAction) {
        let Some(handle) = &view.handle else { return };
        match *action {
            PointerAction::Mouse { x, y, state, mods }
            | PointerAction::LeftClick { x, y, state, mods } => handle.mouse(x, y, state, mods),
            PointerAction::Scroll { x, y, dx, dy } => handle.scroll(x, y, dx, dy),
        }
    }

    fn input(&mut self, id: ViewId, kind: InputKind) {
        self.active = Some(id);
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        match kind {
            InputKind::Pointer(event) => {
                let metrics = view.metrics;
                let at = move |p: SurfacePoint| {
                    let b = metrics.to_buffer(p);
                    (b.x.round() as i32, b.y.round() as i32)
                };
                let actions = view.pointer.event(&event, at, Instant::now());
                for action in actions {
                    if let PointerAction::LeftClick { x, y, state, mods } = action {
                        // A plain click on a link is held until the browser says the engine may
                        // follow it.
                        let link = view.handle.as_ref().and_then(|h| h.link_at(x, y));
                        if let Some(url) = link.filter(|_| view.held.len() < MAX_HELD_NAVIGATIONS) {
                            let request = RequestId::next();
                            view.held.push(HeldClick {
                                request,
                                x,
                                y,
                                state,
                                mods,
                            });
                            self.queue.push(EngineEvent::NavigationRequested {
                                view: id,
                                document: view.document,
                                request,
                                url,
                            });
                            continue;
                        }
                    }
                    Self::send_pointer(view, &action);
                }
            }
            InputKind::Key(event) => match input::key_action(&event) {
                KeyAction::Ignore => {}
                KeyAction::Keys(keys) => {
                    if let Some(handle) = &view.handle {
                        for key in keys {
                            handle.key(key);
                        }
                    }
                }
                KeyAction::Paste => {
                    if !view.asked.values().any(|a| matches!(a, Asked::Paste)) {
                        self.request(
                            id,
                            Service::ClipboardRead {
                                selection: Selection::Clipboard,
                                mime_types: vec![
                                    "text/plain;charset=utf-8".into(),
                                    "text/plain".into(),
                                ],
                                limit: MAX_PASTE,
                            },
                            Asked::Paste,
                        );
                    }
                }
            },
            InputKind::TextInput(event) => {
                if let (Some(text), Some(handle)) = (event.commit, &view.handle) {
                    if let KeyAction::Keys(keys) = input::typed(&text) {
                        for key in keys {
                            handle.key(key);
                        }
                    }
                }
            }
            InputKind::Focus(_) => {}
        }
    }

    fn resolve(&mut self, id: ViewId, request: RequestId, result: ServiceResult) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        let Some(asked) = view.asked.remove(&request) else {
            return;
        };
        match (asked, result) {
            (Asked::Paste, Ok(ServiceReply::Clipboard(data))) => {
                let text = String::from_utf8_lossy(&data.data[..data.data.len().min(MAX_PASTE)])
                    .into_owned();
                ffi::set_clipboard_shadow(&text);
                if let Some(handle) = &view.handle {
                    handle.key(ffi::KEY_PASTE);
                }
            }
            (Asked::Files, Ok(ServiceReply::Files(files))) => {
                let path = files.first().and_then(|p| p.to_str());
                ffi::answer_file(id.serial(), path);
            }
            (Asked::Files, Err(_)) => ffi::answer_file(id.serial(), None),
            _ => {}
        }
    }

    fn navigate(
        &mut self,
        id: ViewId,
        navigation: NavigationId,
        url: String,
        effect: HistoryEffect,
    ) {
        let Some(view) = self.views.get_mut(&id) else {
            return;
        };
        if let Some(old) = view.pending.take() {
            self.queue.push(EngineEvent::NavigationFailed {
                view: id,
                navigation: old.navigation,
                error: NavigationError::Cancelled,
            });
        }
        self.queue.push(EngineEvent::NavigationStarted {
            view: id,
            navigation,
            url: url.clone(),
        });
        let started = match (&view.handle, effect) {
            (Some(handle), HistoryEffect::Replace) => {
                handle.reload();
                true
            }
            (Some(handle), _) => handle.navigate(&url),
            (None, _) => false,
        };
        if started {
            view.pending = Some(PendingNavigation {
                navigation,
                url,
                effect,
                started: false,
            });
        } else {
            self.queue.push(EngineEvent::NavigationFailed {
                view: id,
                navigation,
                error: NavigationError::Load("the address cannot be loaded".into()),
            });
        }
    }
}

/// Buffer pixels per logical pixel of a view's metrics.
fn scale_ratio(metrics: &Metrics) -> f64 {
    let logical = metrics.rect().size().width;
    if logical == 0 {
        1.0
    } else {
        f64::from(metrics.buffer().width) / f64::from(logical)
    }
}

/// `scheme://host[:port]` of an http(s) or file URL: who is asking, for a prompt.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?;
    Some(format!("{scheme}://{host}"))
}

impl<T: CpuTarget> EngineRuntime for NetSurfRuntime<T> {
    type Target = T;
    type FramePermit = T::Permit;

    fn capabilities(&self) -> EngineCapabilities {
        CAPABILITIES
    }

    fn open_view(
        &mut self,
        id: ViewId,
        options: ViewOptions,
        target: T,
    ) -> Result<(), EngineError> {
        self.running()?;
        if self.views.contains_key(&id) {
            return Err(EngineError::View {
                view: id,
                message: "this view is already open".into(),
            });
        }
        let metrics = target.metrics();
        let size = (metrics.buffer().width, metrics.buffer().height);
        let scale = metrics.scale().numerator();
        let handle =
            Handle::open(id.serial(), size.0, size.1, scale).ok_or_else(|| EngineError::View {
                view: id,
                message: "NetSurf could not open a window".into(),
            })?;
        if (options.zoom - 1.0).abs() > f64::EPSILON {
            handle.set_zoom(options.zoom);
        }
        handle.set_hidden(!options.visible);
        self.ids.insert(id.serial(), id);
        self.views.insert(
            id,
            View {
                handle: Some(handle),
                target,
                metrics,
                size,
                visible: options.visible,
                document: DocumentGeneration::next(),
                url: None,
                last_url: None,
                pending: None,
                loading: false,
                dirty: false,
                ready: None,
                painted: None,
                pointer: Pointer::default(),
                caret: false,
                held: Vec::new(),
                asked: HashMap::new(),
            },
        );
        self.queue.push(EngineEvent::ViewReady { view: id });
        Ok(())
    }

    fn command(&mut self, id: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.running()?;
        if !self.views.contains_key(&id) {
            return Err(EngineError::UnknownView(id));
        }
        match command {
            EngineCommand::Navigate { navigation, url } => {
                self.navigate(id, navigation, url, HistoryEffect::Push)
            }
            EngineCommand::Reload { navigation } => {
                let url = self.views[&id].url.clone();
                match url {
                    Some(url) => self.navigate(id, navigation, url, HistoryEffect::Replace),
                    None => self.queue.push(EngineEvent::NavigationFailed {
                        view: id,
                        navigation,
                        error: NavigationError::Load("there is nothing to reload".into()),
                    }),
                }
            }
            EngineCommand::Stop => {
                if let Some(handle) = &self.views[&id].handle {
                    handle.stop();
                }
            }
            EngineCommand::Traverse { .. } => {
                return Err(EngineError::Unsupported("history traversal"))
            }
            EngineCommand::SetMetrics(metrics) => {
                let view = self.views.get_mut(&id).expect("checked above");
                let buffer = metrics.buffer();
                // An empty rectangle is "no room": the window keeps its last size.
                if !metrics.is_empty() && buffer.width > 0 && buffer.height > 0 {
                    view.size = (buffer.width, buffer.height);
                    if let Some(handle) = &view.handle {
                        handle.resize(buffer.width, buffer.height, metrics.scale().numerator());
                    }
                    view.dirty = true;
                }
                view.metrics = metrics;
                // The core reformats from `pump`; frames for this epoch wait for it.
                view.ready = None;
            }
            EngineCommand::SetVisibility(visible) => {
                let view = self.views.get_mut(&id).expect("checked above");
                view.visible = visible;
                if let Some(handle) = &view.handle {
                    handle.set_hidden(!visible);
                }
                view.dirty |= visible;
            }
            EngineCommand::SetFocus(focused) => {
                if focused {
                    self.active = Some(id);
                }
            }
            EngineCommand::SetZoom(zoom) => {
                let view = self.views.get_mut(&id).expect("checked above");
                if let Some(handle) = &view.handle {
                    handle.set_zoom(zoom);
                }
                view.dirty = true;
                view.ready = None;
            }
            EngineCommand::Input(event) => self.input(id, event.kind),
            EngineCommand::ResolveServiceRequest { request, result } => {
                self.resolve(id, request, result)
            }
            EngineCommand::AnswerNavigation { request, allow } => {
                let view = self.views.get_mut(&id).expect("checked above");
                if let Some(at) = view.held.iter().position(|h| h.request == request) {
                    let held = view.held.remove(at);
                    if let (true, Some(handle)) = (allow, &view.handle) {
                        handle.mouse(held.x, held.y, held.state, held.mods);
                    }
                }
            }
            EngineCommand::SetSemantics(_) | EngineCommand::Semantic { .. } => {
                return Err(EngineError::Unsupported("semantics"))
            }
        }
        self.collect();
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.shut_down {
            return PumpStatus::Idle;
        }
        let poll = ffi::poll();
        self.collect();
        // The core has reformatted what `SetMetrics` asked for.
        for (&id, view) in &mut self.views {
            view.ready = Some(view.metrics.epoch());
            if std::mem::take(&mut view.dirty) && view.visible && !view.metrics.is_empty() {
                self.queue.push(EngineEvent::FrameReady { view: id });
                view.painted = None;
                view.dirty = true;
            }
        }
        self.watcher.set(&poll.fds);
        coalesce(&mut self.queue);
        let count = self.queue.len().min(MAX_EVENTS_PER_PUMP);
        events.extend(self.queue.drain(..count));
        if !self.queue.is_empty() {
            return PumpStatus::Pending { deadline: None };
        }
        match poll.timeout_ms {
            Some(0) => PumpStatus::Pending { deadline: None },
            Some(ms) => PumpStatus::Pending {
                deadline: Some(Instant::now() + Duration::from_millis(u64::from(ms))),
            },
            // Descriptors wake the owner through the watcher; nothing else is waiting.
            None => PumpStatus::Idle,
        }
    }

    fn present(&mut self, id: ViewId, permit: T::Permit) -> Result<PresentOutcome, EngineError> {
        self.running()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        let epoch = T::permit_epoch(&permit);
        if view.ready != Some(epoch) || !view.visible || view.handle.is_none() {
            return Ok(PresentOutcome::NotReady);
        }
        if !view.dirty && view.painted == Some(epoch) {
            return Ok(PresentOutcome::Unchanged);
        }
        let size = view.size;
        let handle = view.handle.as_ref().expect("checked above");
        let mut mismatch = false;
        let result = view.target.submit(permit, &mut |frame| {
            if (frame.size.width, frame.size.height) != size {
                mismatch = true;
                return Err("the frame is not the window's size".into());
            }
            match handle.paint(frame.pixels, size.0, size.1, frame.stride)? {
                true => Ok(()),
                false => Err("the core drew nothing".into()),
            }
        });
        match result {
            Ok(()) => {
                view.dirty = false;
                view.painted = Some(epoch);
                Ok(PresentOutcome::Submitted)
            }
            Err(_) if mismatch => Ok(PresentOutcome::NotReady),
            Err(message) => Err(EngineError::View { view: id, message }),
        }
    }

    fn close_view(&mut self, id: ViewId) {
        let Some(mut view) = self.views.remove(&id) else {
            return;
        };
        self.ids.remove(&id.serial());
        if let Some(handle) = view.handle.take() {
            handle.close();
        }
        // The handle is closed before the target is dropped.
        drop(view);
        self.queue.retain(|event| event.view() != Some(id));
        // What the core queued for the view went with its window.
        self.collect();
        self.queue.retain(|event| event.view() != Some(id));
        if self.active == Some(id) {
            self.active = None;
        }
        self.queue.push(EngineEvent::ViewClosed { view: id });
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        let ids: Vec<ViewId> = self.views.keys().copied().collect();
        for id in ids {
            if let Some(mut view) = self.views.remove(&id) {
                if let Some(handle) = view.handle.take() {
                    handle.close();
                }
            }
        }
        self.ids.clear();
        self.queue.clear();
        self.watcher.stop();
        ffi::shutdown();
        self.shut_down = true;
    }
}

impl<T: CpuTarget> Drop for NetSurfRuntime<T> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn coalesce(events: &mut Vec<EngineEvent>) {
    fn replaceable(event: &EngineEvent) -> bool {
        matches!(
            event,
            EngineEvent::TitleChanged { .. }
                | EngineEvent::StatusTextChanged { .. }
                | EngineEvent::CursorChanged { .. }
                | EngineEvent::LoadingChanged { .. }
                | EngineEvent::HistoryChanged { .. }
                | EngineEvent::TextInputChanged { .. }
                | EngineEvent::FrameReady { .. }
                | EngineEvent::SemanticsChanged { .. }
        )
    }
    let mut seen: HashSet<(Discriminant<EngineEvent>, Option<ViewId>)> = HashSet::new();
    let mut keep: Vec<bool> = events
        .iter()
        .rev()
        .map(|event| !replaceable(event) || seen.insert((discriminant(event), event.view())))
        .collect();
    keep.reverse();
    let mut keep = keep.into_iter();
    events.retain(|_| keep.next().unwrap_or(true));
}
