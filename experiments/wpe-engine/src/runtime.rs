//! The WPE runtime: one per process, serving every WPE view behind `EngineRuntime`, with the
//! host's target leases and frame permits.
//!
//! WebKit runs its pages in its own web and network processes; that is WebKit's architecture,
//! not Sighurt IPC. Its UI-process side lives here, on the owner thread: GLib's default context
//! ([`GlibLoop`]), Sighurt's WPE platform ([`Display`], one [`View`] per page) and the presenter
//! ([`Wayland`]). The first view decides the transport: DMA-BUF when the compositor offers
//! `zwp_linux_dmabuf_v1` version 4 with a DRM device WebKit can render on, shared memory
//! otherwise (or with [`WpeConfig::force_shm`]); WebKit's web processes learn it from the
//! primary display when they start.
//!
//! WebKit's callbacks (its signals, the view's `render_buffer`) only queue [`Note`]s, which each
//! runtime entry turns into events after WebKit returns. A crashed web process is
//! `ViewFailed` (WebKit's `web-process-terminated`): the view stays and a reload starts a new
//! process.
//!
//! Must not own: tabs, routing or history policy (the core), surfaces (the host).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::rc::{Rc, Weak};
use std::time::Instant;

use glib::prelude::*;
use glib::translate::*;
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, ViewOptions,
};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError};
use sighurt_api::geometry::Metrics;
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, ViewId};
use sighurt_api::input::{FocusEvent, InputKind};
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};
use wayland_client::protocol::wl_surface::WlSurface;

use crate::ffi;
use crate::glib_loop::GlibLoop;
use crate::input::Input;
use crate::platform::{Buffer, Call, Display, View};
use crate::present::{self, BufferKey, PresentStats, ShmBuffers, Wayland};

/// How the runtime starts.
#[derive(Clone, Copy, Debug, Default)]
pub struct WpeConfig {
    /// Shared memory even where DMA-BUF works: the fallback path, for testing it.
    pub force_shm: bool,
    /// Lets JavaScriptCore in this process install its signal handlers, as WebKit does by
    /// default: only to reproduce the clash `keep_jsc_off_signals` avoids.
    pub jsc_signal_handlers: bool,
}

/// How WebKit's frames reach the compositor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// WebKit's GPU buffers, attached as they are: no CPU copy.
    Dmabuf,
    /// WebKit's shared-memory frames, copied once into ours: the diagnosed fallback.
    Shm,
}

/// What the runtime can tell about itself.
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub transport: Option<Transport>,
    /// The DRM render node WebKit renders on, with DMA-BUF.
    pub device: Option<String>,
    /// DMA-BUF format and modifier pairs the compositor imports on that device.
    pub formats: usize,
    pub present: PresentStats,
    /// DMA-BUF imports the compositor refused.
    pub failed_imports: u64,
    /// Times GLib's context dispatched sources.
    pub glib_dispatches: u64,
    /// Frames of an old geometry, dropped as stale.
    pub stale_frames: u64,
    /// Frames one pixel off the host's buffer size in a dimension, shown anyway: WebKit floors
    /// `logical × scale` where the host rounds half away from zero, and the host's viewport
    /// scales the buffer to the logical size whatever its size.
    pub rounded_frames: u64,
}

/// What WebKit's callbacks report, for the runtime to handle once WebKit has returned.
enum Note {
    Frame(ViewId, Buffer, Vec<ffi::WPERectangle>),
    Cursor(ViewId, String),
    Load(ViewId, u32),
    LoadFailed(ViewId, String),
    Title(ViewId),
    Terminated(ViewId, u32),
    Script(ViewId, Result<String, String>),
}

type Notes = Rc<RefCell<Vec<Note>>>;

/// The display and presenter, made with the first view.
struct Platform {
    wayland: Wayland,
    display: Display,
    transport: Transport,
    device: Option<String>,
    formats: usize,
}

/// One WPE view.
struct Page {
    web_view: glib::Object,
    view: View,
    lease: TargetLease,
    surface: WlSurface,
    shm: ShmBuffers,
    input: Input,
    metrics: Metrics,
    /// The latest frame WebKit rendered that is not yet shown.
    pending: Option<(Buffer, Vec<ffi::WPERectangle>)>,
    /// The epoch of the frame last submitted.
    presented: Option<LayoutEpoch>,
    /// The browser's navigation that WebKit's next `STARTED` belongs to.
    requested: Option<NavigationId>,
    /// The navigation loading now.
    current: Option<NavigationId>,
}

impl Page {
    fn wpe(&self) -> *mut ffi::WPEView {
        self.view.as_ptr()
    }

    fn web(&self) -> *mut ffi::WebKitWebView {
        self.web_view.as_ptr().cast()
    }
}

/// The WPE engine's runtime.
pub struct WpeRuntime {
    config: WpeConfig,
    waker: EngineWaker,
    glib: Option<GlibLoop>,
    platform: Option<Platform>,
    session: Option<glib::Object>,
    pages: HashMap<ViewId, Page>,
    notes: Notes,
    events: Vec<EngineEvent>,
    /// Our DMA-BUF `wl_buffer`s the compositor holds, with the WebKit buffer to release.
    in_flight: HashMap<BufferKey, (ViewId, Buffer)>,
    stats: PresentStats,
    stale_frames: u64,
    rounded_frames: u64,
    scripts: Vec<(ViewId, Result<String, String>)>,
    shut_down: bool,
}

impl WpeRuntime {
    /// Starts the runtime on the owner thread: takes GLib's default context, which WebKit's
    /// UI-process side will use. WebKit itself starts with the first view.
    pub fn new(config: WpeConfig, waker: EngineWaker) -> Result<Self, EngineError> {
        if !config.jsc_signal_handlers {
            keep_jsc_off_signals()?;
        }
        let glib = GlibLoop::new(waker.clone()).map_err(EngineError::Runtime)?;
        Ok(Self {
            config,
            waker,
            glib: Some(glib),
            platform: None,
            session: None,
            pages: HashMap::new(),
            notes: Rc::default(),
            events: Vec::new(),
            in_flight: HashMap::new(),
            stats: PresentStats::default(),
            stale_frames: 0,
            rounded_frames: 0,
            scripts: Vec::new(),
            shut_down: false,
        })
    }

    pub fn diagnostics(&self) -> Diagnostics {
        let platform = self.platform.as_ref();
        let (imports, failed_imports) = platform.map_or((0, 0), |p| p.wayland.imports());
        Diagnostics {
            transport: platform.map(|p| p.transport),
            device: platform.and_then(|p| p.device.clone()),
            formats: platform.map_or(0, |p| p.formats),
            present: PresentStats {
                dmabuf_imports: imports,
                ..self.stats
            },
            failed_imports,
            glib_dispatches: self.glib.as_ref().map_or(0, |g| g.dispatches),
            stale_frames: self.stale_frames,
            rounded_frames: self.rounded_frames,
        }
    }

    /// Runs `script` in `view`'s main frame; its result (as a string) arrives in
    /// [`take_script_results`](Self::take_script_results) after a later pump.
    pub fn evaluate(&mut self, view: ViewId, script: &str) -> Result<(), EngineError> {
        let page = self
            .pages
            .get(&view)
            .ok_or(EngineError::UnknownView(view))?;
        let script =
            CString::new(script).map_err(|_| EngineError::Unsupported("NUL in a script"))?;
        let data = Box::into_raw(Box::new((Rc::downgrade(&self.notes), view)));
        unsafe {
            ffi::webkit_web_view_evaluate_javascript(
                page.web(),
                script.as_ptr(),
                -1,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                Some(script_done),
                data.cast(),
            );
        }
        Ok(())
    }

    /// Script results that arrived, oldest first.
    pub fn take_script_results(&mut self) -> Vec<(ViewId, Result<String, String>)> {
        std::mem::take(&mut self.scripts)
    }

    /// Makes the display and presenter on the host's connection, from the first lease.
    fn platform(&mut self, lease: &TargetLease) -> Result<&mut Platform, EngineError> {
        if self.platform.is_none() {
            let runtime = |e: String| EngineError::Runtime(e);
            // SAFETY: the presenter's objects (our queue, buffers and surface proxies) are
            // destroyed before the leases go (`close_view`, `shutdown`).
            let handles = unsafe { lease.gl_handles() }.ok_or(EngineError::Unsupported(
                "WPE views present on native targets",
            ))?;
            let display = match handles.display_handle().map(|h| h.as_raw()) {
                Ok(RawDisplayHandle::Wayland(h)) => h.display,
                _ => return Err(runtime("no Wayland display".into())),
            };
            let wayland = unsafe { Wayland::new(display) }.map_err(runtime)?;
            let dmabuf = wayland.dmabuf().filter(|_| !self.config.force_shm);
            let transport = match dmabuf {
                Some(_) => Transport::Dmabuf,
                None => Transport::Shm,
            };
            let device = dmabuf.as_ref().map(|d| {
                let node = d.render.as_ref().unwrap_or(&d.primary);
                node.to_string_lossy().into_owned()
            });
            let formats = dmabuf.as_ref().map_or(0, |d| d.formats.len());
            let display = Display::new(dmabuf).map_err(|e| runtime(e.to_string()))?;
            let session = unsafe {
                glib::Object::from_glib_full(ffi::webkit_network_session_new_ephemeral())
            };
            self.session = Some(session);
            self.platform = Some(Platform {
                wayland,
                display,
                transport,
                device,
                formats,
            });
        }
        Ok(self.platform.as_mut().expect("made above"))
    }

    fn push(&mut self, event: EngineEvent) {
        push(&mut self.events, event);
    }

    /// Turns what WebKit's callbacks queued into events and page state.
    fn handle_notes(&mut self) {
        loop {
            let notes = std::mem::take(&mut *self.notes.borrow_mut());
            if notes.is_empty() {
                return;
            }
            for note in notes {
                self.handle(note);
            }
        }
    }

    fn handle(&mut self, note: Note) {
        let view = match &note {
            Note::Frame(v, ..)
            | Note::Cursor(v, _)
            | Note::Load(v, _)
            | Note::LoadFailed(v, _)
            | Note::Title(v)
            | Note::Terminated(v, _)
            | Note::Script(v, _) => *v,
        };
        if let Note::Script(_, result) = note {
            self.scripts.push((view, result));
            return;
        }
        let Some(page) = self.pages.get_mut(&view) else {
            return;
        };
        let event = match note {
            Note::Frame(_, buffer, damage) => {
                if let Some((old, _)) = page.pending.replace((buffer, damage)) {
                    self.stats.dropped += 1;
                    finish_unshown(page.wpe(), &old);
                }
                EngineEvent::FrameReady { view }
            }
            Note::Cursor(_, name) => EngineEvent::CursorChanged {
                view,
                cursor: cursor(&name),
            },
            Note::Load(_, ffi::WEBKIT_LOAD_STARTED) => {
                let navigation = page.requested.take().unwrap_or_else(NavigationId::next);
                page.current = Some(navigation);
                let url = uri(page.web());
                self.push(EngineEvent::LoadingChanged {
                    view,
                    loading: true,
                });
                EngineEvent::NavigationStarted {
                    view,
                    navigation,
                    url,
                }
            }
            Note::Load(_, ffi::WEBKIT_LOAD_COMMITTED) => EngineEvent::NavigationCommitted {
                view,
                navigation: *page.current.get_or_insert_with(NavigationId::next),
                url: uri(page.web()),
                document: DocumentGeneration::next(),
                history: None,
            },
            Note::Load(_, ffi::WEBKIT_LOAD_FINISHED) => EngineEvent::LoadingChanged {
                view,
                loading: false,
            },
            Note::Load(..) => return,
            Note::LoadFailed(_, message) => EngineEvent::NavigationFailed {
                view,
                navigation: page.current.unwrap_or_else(NavigationId::next),
                error: NavigationError::Load(message),
            },
            Note::Title(_) => {
                let title = unsafe { ffi::webkit_web_view_get_title(page.web()) };
                EngineEvent::TitleChanged {
                    view,
                    title: (!title.is_null()).then(|| {
                        unsafe { CStr::from_ptr(title) }
                            .to_string_lossy()
                            .into_owned()
                    }),
                }
            }
            Note::Terminated(_, reason) => {
                if let Some((frame, _)) = page.pending.take() {
                    finish_unshown(page.wpe(), &frame);
                }
                let why = match reason {
                    ffi::WEBKIT_WEB_PROCESS_CRASHED => "crashed",
                    ffi::WEBKIT_WEB_PROCESS_EXCEEDED_MEMORY_LIMIT => "exceeded its memory limit",
                    _ => "was terminated",
                };
                EngineEvent::ViewFailed {
                    view,
                    message: format!("WebKit's web process {why}; reload to start a new one"),
                }
            }
            Note::Script(..) => unreachable!("handled above"),
        };
        self.push(event);
    }

    /// Gives WebKit back the buffers the compositor released.
    fn release_buffers(&mut self) {
        let Some(platform) = self.platform.as_mut() else {
            return;
        };
        for key in platform.wayland.dispatch() {
            self.stats.releases += 1;
            if let Some((view, buffer)) = self.in_flight.remove(&key) {
                if let Some(page) = self.pages.get(&view) {
                    unsafe { ffi::wpe_view_buffer_released(page.wpe(), buffer.as_ptr()) };
                }
            } else if let Some((&view, page)) = self
                .pages
                .iter_mut()
                .find_map(|(view, page)| page.shm.release(key).then_some((view, page)))
            {
                // A frame that waited for a free SHM buffer can be shown now.
                if page.pending.is_some() {
                    push(&mut self.events, EngineEvent::FrameReady { view });
                }
            }
        }
    }

    fn apply_metrics(&mut self, view: ViewId, metrics: Metrics) {
        let Some(page) = self.pages.get_mut(&view) else {
            return;
        };
        page.metrics = metrics;
        if metrics.is_empty() {
            return; // No room: keep the last size (plan §11).
        }
        let size = metrics.rect().size();
        let (width, height) = (size.width as i32, size.height as i32);
        unsafe {
            let toplevel = toplevel(page.wpe());
            ffi::wpe_toplevel_scale_changed(toplevel, metrics.scale().factor());
            ffi::wpe_toplevel_resized(toplevel, width, height);
            ffi::wpe_view_resized(page.wpe(), width, height);
        }
        // A frame of the old geometry would never be shown: WebKit may render the new one now.
        let buffer = metrics.buffer();
        if page
            .pending
            .as_ref()
            .is_some_and(|(frame, _)| frame.size() != (buffer.width, buffer.height))
        {
            let (frame, _) = page.pending.take().expect("checked");
            self.stats.dropped += 1;
            finish_unshown(page.wpe(), &frame);
        }
    }

    fn input(&mut self, view: ViewId, kind: InputKind) {
        let Some(page) = self.pages.get_mut(&view) else {
            return;
        };
        let wpe = page.wpe();
        let events = match kind {
            InputKind::Pointer(event) => {
                page.input.pointer(&page.view, &event).into_iter().collect()
            }
            InputKind::Key(event) => page.input.key(&page.view, &event).into_iter().collect(),
            InputKind::Focus(FocusEvent::Activated) => {
                unsafe {
                    ffi::wpe_toplevel_state_changed(toplevel(wpe), ffi::WPE_TOPLEVEL_STATE_ACTIVE)
                };
                Vec::new()
            }
            InputKind::Focus(FocusEvent::Deactivated) => {
                unsafe { ffi::wpe_toplevel_state_changed(toplevel(wpe), 0) };
                page.input.release_keys(&page.view)
            }
            // The view never reports text-input state, so no input method talks to it.
            InputKind::TextInput(_) => Vec::new(),
        };
        for event in events {
            unsafe {
                ffi::wpe_view_event(wpe, event);
                ffi::wpe_event_unref(event);
            }
        }
    }

    fn close(&mut self, view: ViewId) -> bool {
        let Some(page) = self.pages.remove(&view) else {
            return false;
        };
        page.view.set_sink(None);
        if let Some((frame, _)) = &page.pending {
            finish_unshown(page.wpe(), frame);
        }
        self.in_flight.retain(|_, (v, _)| *v != view);
        // WebKit's view and its buffers (and our wl_buffers in them) first, then our surface
        // proxy and SHM buffers, then the lease (targets.rs, "Destruction order").
        let Page {
            web_view,
            view: wpe_view,
            lease,
            surface,
            shm,
            ..
        } = page;
        drop(wpe_view);
        drop(web_view);
        drop(shm);
        drop(surface);
        lease.retire();
        true
    }
}

/// Keeps JavaScriptCore in this process away from signals. WebKit runs JavaScriptCore in the
/// UI process too (it holds the values scripts return), and once a VM exists there it installs
/// SIGSEGV and SIGBUS handlers that block every signal while they chain to the next handler.
/// Wasmtime leaves its handler with `longjmp`, which keeps that mask: after one Wasmtime trap
/// under JavaScriptCore's handler, the thread's next trap is a blocked SIGILL that kills the
/// process (docs/rewrite/WPE.md, "Signals"). So this process's JavaScriptCore polls for its VM
/// traps and has no Wasm fault handler; the web processes, where pages run, keep theirs. Must
/// run before WebKit is first used; options are frozen once JavaScriptCore initializes.
fn keep_jsc_off_signals() -> Result<(), EngineError> {
    for (option, value) in [
        (c"usePollingTraps", true),
        (c"useWasmFaultSignalHandler", false),
    ] {
        let mut now = 0;
        let ok = unsafe {
            ffi::jsc_options_set_boolean(option.as_ptr(), i32::from(value)) != 0
                && ffi::jsc_options_get_boolean(option.as_ptr(), &mut now) != 0
        };
        if !ok || (now != 0) != value {
            let message = format!("JavaScriptCore's option {option:?} is not {value}");
            return Err(EngineError::Runtime(message));
        }
    }
    Ok(())
}

/// `push`, keeping only the latest replaceable event of a kind for a view.
fn push(events: &mut Vec<EngineEvent>, event: EngineEvent) {
    let replaceable = matches!(
        event,
        EngineEvent::FrameReady { .. }
            | EngineEvent::TitleChanged { .. }
            | EngineEvent::LoadingChanged { .. }
            | EngineEvent::CursorChanged { .. }
    );
    if replaceable {
        let kind = std::mem::discriminant(&event);
        events.retain(|e| std::mem::discriminant(e) != kind || e.view() != event.view());
    }
    events.push(event);
}

/// Tells WebKit a frame it will never see shown is done and free.
fn finish_unshown(view: *mut ffi::WPEView, frame: &Buffer) {
    unsafe {
        ffi::wpe_view_buffer_rendered(view, frame.as_ptr());
        ffi::wpe_view_buffer_released(view, frame.as_ptr());
    }
}

fn toplevel(view: *mut ffi::WPEView) -> *mut ffi::WPEToplevel {
    let view: glib::Object = unsafe { from_glib_none(view.cast::<glib::gobject_ffi::GObject>()) };
    let toplevel: glib::Object = view.property("toplevel");
    toplevel.as_ptr().cast()
}

fn uri(web_view: *mut ffi::WebKitWebView) -> String {
    let uri = unsafe { ffi::webkit_web_view_get_uri(web_view) };
    match uri.is_null() {
        true => String::new(),
        false => unsafe { CStr::from_ptr(uri) }
            .to_string_lossy()
            .into_owned(),
    }
}

fn cursor(name: &str) -> Cursor {
    match name {
        "pointer" => Cursor::Pointer,
        "text" => Cursor::Text,
        "wait" => Cursor::Wait,
        "progress" => Cursor::Progress,
        "crosshair" => Cursor::Crosshair,
        "move" => Cursor::Move,
        "not-allowed" => Cursor::NotAllowed,
        "grab" => Cursor::Grab,
        "grabbing" => Cursor::Grabbing,
        "help" => Cursor::Help,
        "none" => Cursor::None,
        _ => Cursor::Default,
    }
}

unsafe extern "C" fn script_done(
    source: *mut glib::gobject_ffi::GObject,
    result: *mut ffi::GAsyncResult,
    data: glib::ffi::gpointer,
) {
    let data = unsafe { Box::from_raw(data.cast::<(Weak<RefCell<Vec<Note>>>, ViewId)>()) };
    let (notes, view) = *data;
    let mut error = std::ptr::null_mut();
    let value = unsafe {
        ffi::webkit_web_view_evaluate_javascript_finish(source.cast(), result, &mut error)
    };
    let outcome = if value.is_null() {
        let error: glib::Error = unsafe { from_glib_full(error) };
        Err(error.message().to_owned())
    } else {
        let text: glib::GString = unsafe { from_glib_full(ffi::jsc_value_to_string(value)) };
        unsafe { glib::gobject_ffi::g_object_unref(value.cast()) };
        Ok(text.to_string())
    };
    if let Some(notes) = notes.upgrade() {
        notes.borrow_mut().push(Note::Script(view, outcome));
    }
}

/// The value of an enum-typed signal argument.
fn enum_arg(value: &glib::Value) -> u32 {
    unsafe { glib::gobject_ffi::g_value_get_enum(value.to_glib_none().0) as u32 }
}

impl EngineRuntime for WpeRuntime {
    type Target = TargetLease;
    type FramePermit = FramePermit;

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            history_traversal: true,
            page_zoom: true,
            ..EngineCapabilities::default()
        }
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        lease: TargetLease,
    ) -> Result<(), EngineError> {
        if self.shut_down {
            return Err(EngineError::Runtime("the WPE runtime was shut down".into()));
        }
        if lease.mode() != PresentMode::Gl {
            return Err(EngineError::Unsupported(
                "WPE views present on native targets",
            ));
        }
        let notes = Rc::clone(&self.notes);
        let platform = self.platform(&lease)?;
        // SAFETY: the proxy is dropped before the lease (`close`).
        let surface = match unsafe { lease.gl_handles() }
            .map(|h| h.window_handle().map(|w| w.as_raw()))
        {
            Some(Ok(RawWindowHandle::Wayland(h))) => unsafe { platform.wayland.surface(h.surface) },
            _ => Err("no wl_surface".to_string()),
        }
        .map_err(EngineError::Runtime)?;
        let shm = platform.wayland.shm_buffers();
        let web_view =
            glib::Object::builder_with_type(unsafe { from_glib(ffi::webkit_web_view_get_type()) })
                .property("display", platform.display.clone())
                .property(
                    "network-session",
                    self.session.as_ref().expect("made with the platform"),
                )
                .build();
        let wpe: *mut ffi::WPEView =
            unsafe { ffi::webkit_web_view_get_wpe_view(web_view.as_ptr().cast()) };
        let wpe_view: View =
            unsafe { glib::Object::from_glib_none(wpe.cast::<glib::gobject_ffi::GObject>()) }
                .downcast()
                .map_err(|_| {
                    EngineError::Runtime("WebKit made a view of another platform".into())
                })?;
        let sink_notes = Rc::downgrade(&notes);
        wpe_view.set_sink(Some(Rc::new(move |call| {
            let Some(notes) = sink_notes.upgrade() else {
                return;
            };
            notes.borrow_mut().push(match call {
                Call::Frame(buffer, damage) => Note::Frame(view, buffer, damage),
                Call::Cursor(name) => Note::Cursor(view, name),
            });
        })));
        connect(&web_view, &notes, view);
        unsafe { ffi::webkit_web_view_set_zoom_level(web_view.as_ptr().cast(), options.zoom) };
        let metrics = lease.metrics();
        self.pages.insert(
            view,
            Page {
                web_view,
                view: wpe_view,
                lease,
                surface,
                shm,
                input: Input::default(),
                metrics,
                pending: None,
                presented: None,
                requested: None,
                current: None,
            },
        );
        self.apply_metrics(view, metrics);
        let wpe = self.pages[&view].wpe();
        unsafe {
            match options.visible {
                true => ffi::wpe_view_map(wpe),
                false => ffi::wpe_view_unmap(wpe),
            }
        }
        self.push(EngineEvent::ViewReady { view });
        self.handle_notes();
        Ok(())
    }

    fn command(&mut self, view: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        if self.shut_down {
            return Err(EngineError::Runtime("the WPE runtime was shut down".into()));
        }
        let page = self
            .pages
            .get_mut(&view)
            .ok_or(EngineError::UnknownView(view))?;
        let web = page.web();
        match command {
            EngineCommand::Navigate { navigation, url } => {
                page.requested = Some(navigation);
                let url =
                    CString::new(url).map_err(|_| EngineError::Unsupported("NUL in a URL"))?;
                unsafe { ffi::webkit_web_view_load_uri(web, url.as_ptr()) };
            }
            EngineCommand::Reload { navigation } => {
                page.requested = Some(navigation);
                unsafe { ffi::webkit_web_view_reload(web) };
            }
            EngineCommand::Stop => unsafe { ffi::webkit_web_view_stop_loading(web) },
            EngineCommand::Traverse { navigation, delta } => {
                let possible = unsafe {
                    match delta.signum() {
                        -1 => ffi::webkit_web_view_can_go_back(web) != 0,
                        1 => ffi::webkit_web_view_can_go_forward(web) != 0,
                        _ => false,
                    }
                };
                if !possible || delta.abs() != 1 {
                    self.push(EngineEvent::NavigationFailed {
                        view,
                        navigation,
                        error: NavigationError::Cancelled,
                    });
                    return Ok(());
                }
                page.requested = Some(navigation);
                unsafe {
                    match delta {
                        -1 => ffi::webkit_web_view_go_back(web),
                        _ => ffi::webkit_web_view_go_forward(web),
                    }
                }
            }
            EngineCommand::SetMetrics(metrics) => self.apply_metrics(view, metrics),
            EngineCommand::SetVisibility(visible) => unsafe {
                match visible {
                    true => ffi::wpe_view_map(page.wpe()),
                    false => ffi::wpe_view_unmap(page.wpe()),
                }
            },
            EngineCommand::SetFocus(focused) => {
                let wpe = page.wpe();
                if focused {
                    unsafe { ffi::wpe_view_focus_in(wpe) };
                } else {
                    for event in page.input.release_keys(&page.view) {
                        unsafe {
                            ffi::wpe_view_event(wpe, event);
                            ffi::wpe_event_unref(event);
                        }
                    }
                    unsafe { ffi::wpe_view_focus_out(wpe) };
                }
            }
            EngineCommand::SetZoom(zoom) => unsafe {
                ffi::webkit_web_view_set_zoom_level(web, zoom)
            },
            EngineCommand::Input(event) => self.input(view, event.kind),
            EngineCommand::ResolveServiceRequest { .. } => {
                return Err(EngineError::Unsupported(
                    "WPE views make no service requests yet",
                ));
            }
        }
        self.handle_notes();
        Ok(())
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.shut_down {
            return PumpStatus::Idle;
        }
        self.release_buffers();
        let next = self.glib.as_mut().and_then(GlibLoop::iterate);
        self.handle_notes();
        events.append(&mut self.events);
        match next {
            None => PumpStatus::Idle,
            Some(at) if at <= Instant::now() => PumpStatus::Pending { deadline: None },
            Some(at) => PumpStatus::Pending { deadline: Some(at) },
        }
    }

    fn present(
        &mut self,
        view: ViewId,
        permit: FramePermit,
    ) -> Result<PresentOutcome, EngineError> {
        self.release_buffers();
        if !self.events.is_empty() {
            self.waker.wake();
        }
        let page = self
            .pages
            .get_mut(&view)
            .ok_or(EngineError::UnknownView(view))?;
        let epoch = permit.metrics().epoch();
        let unchanged = match page.presented == Some(epoch) {
            true => PresentOutcome::Unchanged,
            false => PresentOutcome::NotReady,
        };
        if !permit.is_current() || page.pending.is_none() {
            permit.finish(unchanged);
            return Ok(unchanged);
        }
        let (frame, damage) = page.pending.take().expect("checked");
        let buffer = permit.metrics().buffer();
        let size = (buffer.width, buffer.height);
        let (width, height) = frame.size();
        if width.abs_diff(size.0) > 1 || height.abs_diff(size.1) > 1 {
            self.stale_frames += 1;
            self.stats.dropped += 1;
            finish_unshown(page.wpe(), &frame);
            permit.finish(unchanged);
            return Ok(unchanged);
        }
        if (width, height) != size {
            self.rounded_frames += 1;
        }
        let size = (width, height);
        let platform = self.platform.as_mut().expect("a page has a platform");
        let imported = if frame.is_dma_buf() {
            platform
                .wayland
                .dmabuf_buffer(&frame)
                .map(|b| Some((b, false)))
        } else {
            page.shm
                .copy(&mut platform.wayland, &frame)
                .map(|b| b.map(|b| (b, true)))
        }
        .map_err(|message| EngineError::View { view, message })?;
        let Some(((wl_buffer, key), copied)) = imported else {
            // Every SHM buffer of ours is still with the compositor: keep the frame.
            page.pending = Some((frame, damage));
            permit.finish(unchanged);
            return Ok(unchanged);
        };
        page.surface.attach(Some(&wl_buffer), 0, 0);
        present::damage(&page.surface, if copied { &[] } else { &damage }, size);
        page.surface.commit();
        page.presented = Some(epoch);
        unsafe { ffi::wpe_view_buffer_rendered(page.wpe(), frame.as_ptr()) };
        if copied {
            self.stats.shm_copies += 1;
            self.stats.shm_bytes += u64::from(size.0) * u64::from(size.1) * 4;
            unsafe { ffi::wpe_view_buffer_released(page.wpe(), frame.as_ptr()) };
        } else {
            self.stats.dmabuf_frames += 1;
            self.in_flight.insert(key, (view, frame));
        }
        permit.finish(PresentOutcome::Submitted);
        self.handle_notes();
        // Events reach the owner only through `pump`: ask for one.
        if !self.events.is_empty() {
            self.waker.wake();
        }
        Ok(PresentOutcome::Submitted)
    }

    fn close_view(&mut self, view: ViewId) {
        if self.close(view) {
            self.push(EngineEvent::ViewClosed { view });
        }
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        let views: Vec<ViewId> = self.pages.keys().copied().collect();
        for view in views {
            self.close(view);
        }
        self.in_flight.clear();
        // Let WebKit finish tearing its views down (their buffers hold our wl_buffers).
        if let Some(glib) = self.glib.as_mut() {
            for _ in 0..8 {
                glib.iterate();
            }
        }
        self.notes.borrow_mut().clear();
        self.events.clear();
        self.session = None;
        self.platform = None;
        self.glib = None;
    }
}

/// Connects the web view's signals to the notes queue.
fn connect(web_view: &glib::Object, notes: &Notes, view: ViewId) {
    let weak = Rc::downgrade(notes);
    let note = move |note: Note| {
        if let Some(notes) = weak.upgrade() {
            notes.borrow_mut().push(note);
        }
    };
    let n = note.clone();
    web_view.connect_local("load-changed", false, move |args| {
        n(Note::Load(view, enum_arg(&args[1])));
        None
    });
    let n = note.clone();
    web_view.connect_local("load-failed", false, move |args| {
        let error = args[3].get::<glib::Error>().map(|e| e.message().to_owned());
        n(Note::LoadFailed(
            view,
            error.unwrap_or_else(|_| "load failed".into()),
        ));
        Some(false.to_value())
    });
    let n = note.clone();
    web_view.connect_notify_local(Some("title"), move |_, _| n(Note::Title(view)));
    web_view.connect_local("web-process-terminated", false, move |args| {
        note(Note::Terminated(view, enum_arg(&args[1])));
        None
    });
}
