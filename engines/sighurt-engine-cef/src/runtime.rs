//! [`CefRuntime`]: the `EngineRuntime` of the CEF engine.
//!
//! Owns: starting and stopping CEF (once per process), the views' owner-thread halves (their
//! targets, input state), and turning the host's commands and input into [`Action`]s. What CEF
//! reports comes back through [`crate::handlers`] and the shared queue.
//!
//! Must not own: any CEF callback, or browser policy.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, SiteDataKinds, SiteDataRequest, ViewOptions,
};
use sighurt_api::events::{EngineEvent, NavigationError};
use sighurt_api::geometry::{Metrics, SurfaceRect};
use sighurt_api::ids::{RequestId, ViewId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyEvent, PointerButton, PointerEvent,
    ScrollDelta, TextInputEvent,
};
use sighurt_api::present::CpuTarget;
use sighurt_api::services::{
    ClipboardData, Selection, Service, ServiceError, ServiceReply, ServiceRequest, ServiceResult,
};

use crate::actions::{Action, KEY_CHAR, KEY_RAW_DOWN, KEY_UP};
use crate::capi::{
    CefApp, CefBrowserSettings, CefCommandLine, CefMainArgs, CefSettings, CefWindowInfo,
};
use crate::edit::Binding;
use crate::ffi::{self, call, new_object, zeroed, CefStr, Ref};
use crate::handlers::{self, origin};
use crate::keys;
use crate::present;
use crate::shared::{lock, Geometry, Shared, ViewShared, MAX_SELECTION};

/// The engine's name in the configuration.
pub const NAME: &str = "cef";

/// What the engine supports.
pub const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    history_traversal: true,
    text_input: true,
    accessibility: false,
    screenshots: false,
    find_in_page: true,
    page_zoom: true,
    pointer_lock: false,
    native_popups: false,
    process_per_view: false,
    semantics: false,
    // Chromium keeps all of these in its profile; forgetting them is not implemented, and
    // `site_data` says so instead of pretending.
    site_data: SiteDataKinds {
        cookies: true,
        storage: true,
        caches: true,
        hsts: true,
    },
    fullscreen: false, // lane D: true once element fullscreen is wired
};

/// The most events one `pump` hands over.
const MAX_PER_PUMP: usize = 256;
/// The most clipboard text pasted into a page.
const MAX_PASTE: usize = MAX_SELECTION;
/// The most clipboard operations waiting for an answer.
const MAX_EDITS: usize = 16;
/// How long `shutdown` waits for the browsers to close.
const CLOSE_WAIT: Duration = Duration::from_secs(5);
/// What a double click is.
const CLICK_TIME: Duration = Duration::from_millis(500);

/// What the runtime starts with.
#[derive(Clone, Debug)]
pub struct CefConfig {
    /// The runtime directory: `libcef.so`, its resources and (by default) the helper.
    pub dist: PathBuf,
    /// The helper process; by default `sig-cef-helper` next to the executable, else in `dist`.
    pub helper: Option<PathBuf>,
    /// Chromium's profile directory (cookies, cache, local storage). Without one the session is
    /// ephemeral: kept in a temporary directory that CEF holds in memory.
    pub profile: Option<PathBuf>,
    /// A directory CEF may use for its own bookkeeping when there is no profile.
    pub scratch: Option<PathBuf>,
    /// The `User-Agent` header and `navigator.userAgent`, instead of Chromium's.
    pub user_agent: Option<String>,
    /// Turns Chromium's sandbox off. Never by default.
    pub no_sandbox: bool,
}

impl CefConfig {
    /// The configuration for the CEF this build found, if any.
    pub fn installed() -> Option<Self> {
        let dist = option_env!("SIGHURT_CEF_DIST").filter(|dist| !dist.is_empty())?;
        Some(Self {
            dist: PathBuf::from(dist),
            helper: None,
            profile: None,
            scratch: None,
            user_agent: None,
            no_sandbox: false,
        })
    }
}

/// What the engine counted, for the diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    /// Open views.
    pub views: usize,
    /// Frames CEF painted.
    pub frames: u64,
    /// Bytes copied from CEF's buffers into staging (dirty rectangles only after the first).
    pub staged_bytes: u64,
    /// Bytes copied into the host's frames.
    pub presented_bytes: u64,
}

/// CEF may be initialized once per process, ever.
static STARTED: AtomicBool = AtomicBool::new(false);

struct Click {
    at: Instant,
    button: i32,
    x: i32,
    y: i32,
    count: i32,
}

/// The owner-thread half of a view.
struct View<T> {
    shared: Arc<ViewShared>,
    target: T,
    visible: bool,
    pointer: (i32, i32),
    buttons: u32,
    click: Option<Click>,
    /// Keys held down: evdev code to (virtual key, modifiers), to release them on focus loss.
    held: BTreeMap<u32, (i32, u32)>,
    /// Presses the engine took (a clipboard chord): their releases are not sent either.
    consumed: HashSet<u32>,
    /// Pastes and cuts waiting for the browser's answer, with the document and frame that asked.
    edits: HashMap<RequestId, (ClipboardOp, Binding)>,
    /// An input method's composition is showing.
    composing: bool,
    /// What was last written into each buffer the lease has lent: its address, stride and size.
    buffers: HashMap<(usize, usize, usize, usize), present::BufferState>,
    last_frames: u64,
    last_epoch: Option<sighurt_api::ids::LayoutEpoch>,
}

/// The engine: one per process.
pub struct CefRuntime<T: CpuTarget> {
    shared: Arc<Shared>,
    views: HashMap<ViewId, View<T>>,
    shut_down: bool,
    leaked: bool,
    presented_bytes: u64,
}

impl<T: CpuTarget> CefRuntime<T> {
    /// Starts Chromium: opens `libcef.so`, initializes CEF (on this thread, which must also be
    /// the one that shuts it down) and returns the runtime. Takes tens to hundreds of
    /// milliseconds. An error says why the engine cannot run here, in words for the user.
    pub fn new(config: CefConfig, waker: EngineWaker) -> Result<Self, String> {
        if !config.dist.join("libcef.so").exists() {
            return Err(format!(
                "CEF is not installed ({}/libcef.so is missing): run nix/build-cef.sh",
                config.dist.display()
            ));
        }
        let helper = helper_path(&config)?;
        if STARTED.swap(true, Ordering::SeqCst) {
            return Err("CEF can be started only once per process".into());
        }
        let api =
            ffi::load(&config.dist).inspect_err(|_| STARTED.store(false, Ordering::SeqCst))?;

        // Strings CEF copies during `cef_initialize`.
        let helper = CefStr::new(&helper.to_string_lossy());
        let dist = CefStr::new(&config.dist.to_string_lossy());
        let locales = CefStr::new(&config.dist.join("locales").to_string_lossy());
        let cache = config.profile.clone().or_else(|| config.scratch.clone());
        let root = CefStr::new(
            &cache
                .as_ref()
                .map_or(String::new(), |p| p.to_string_lossy().into()),
        );
        let agent = CefStr::new(config.user_agent.as_deref().unwrap_or(""));
        // SAFETY: an all-zero `cef_settings_t` is valid (null strings, zero flags).
        let mut settings: CefSettings = unsafe { std::mem::zeroed() };
        settings.size = std::mem::size_of::<CefSettings>();
        settings.no_sandbox = i32::from(config.no_sandbox);
        settings.browser_subprocess_path = helper.raw();
        settings.multi_threaded_message_loop = 1;
        settings.windowless_rendering_enabled = 1;
        settings.disable_signal_handlers = 1;
        settings.resources_dir_path = dist.raw();
        settings.locales_dir_path = locales.raw();
        settings.root_cache_path = root.raw();
        if config.profile.is_some() {
            // An ephemeral session has an empty `cache_path`: kept in memory.
            settings.cache_path = root.raw();
            settings.persist_session_cookies = 1;
        }
        if config.user_agent.is_some() {
            settings.user_agent = agent.raw();
        }
        settings.log_severity = 3; // LOGSEVERITY_WARNING
        settings.background_color = 0xFFFF_FFFF;

        // Chromium rewrites the argv area it is given to set the process title: give it a
        // buffer of its own, never `sig`'s arguments.
        let program = CString::new("sig").expect("no NUL");
        let argv: &'static mut [*mut libc::c_char; 2] =
            Box::leak(Box::new([program.into_raw(), std::ptr::null_mut()]));
        let args = CefMainArgs {
            argc: 1,
            _pad: 0,
            argv: argv.as_mut_ptr(),
        };
        let mut app: CefApp = zeroed();
        app.on_before_command_line_processing = Some(on_before_command_line_processing);
        let app = new_object(app, ());
        // SAFETY: valid structs; CEF adopts the reference to `app`.
        let ok = unsafe { (api.initialize)(&args, &settings, app, std::ptr::null_mut()) };
        if ok == 0 {
            STARTED.store(false, Ordering::SeqCst);
            return Err(
                "Chromium did not start: another sig may hold the profile, or the display is \
                 unusable (see its log lines above)"
                    .into(),
            );
        }
        Ok(Self {
            shared: Shared::new(waker),
            views: HashMap::new(),
            shut_down: false,
            leaked: false,
            presented_bytes: 0,
        })
    }

    /// What the engine counted.
    pub fn diagnostics(&self) -> Diagnostics {
        let mut d = Diagnostics {
            views: self.views.len(),
            presented_bytes: self.presented_bytes,
            ..Diagnostics::default()
        };
        for view in self.views.values() {
            let staged = lock(&view.shared.staged);
            d.frames += staged.frames;
            d.staged_bytes += staged.copied;
        }
        d
    }

    /// Whether `shutdown` had to leave Chromium running (a browser never closed).
    pub fn leaked(&self) -> bool {
        self.leaked
    }

    fn running(&self) -> Result<(), EngineError> {
        if self.shut_down {
            Err(EngineError::Runtime("the cef engine was shut down".into()))
        } else {
            Ok(())
        }
    }
}

/// The helper process: configured, else next to the executable, else beside `libcef.so`.
fn helper_path(config: &CefConfig) -> Result<PathBuf, String> {
    let candidates = config.helper.iter().cloned().chain(
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join("sig-cef-helper"))),
    );
    candidates
        .chain([config.dist.join("sig-cef-helper")])
        .find(|path| path.is_file())
        .ok_or_else(|| "sig-cef-helper is missing: run nix/build-cef.sh".into())
}

/// Chromium's command line, for the browser process only: nothing here reaches `sig`'s own.
unsafe extern "C" fn on_before_command_line_processing(
    _this: *mut CefApp,
    process_type: *const crate::capi::CefStringUtf16,
    command_line: *mut CefCommandLine,
) {
    // SAFETY: CEF's arguments; the command line is an owned reference.
    unsafe {
        let Some(command_line) = Ref::adopt(command_line) else {
            return;
        };
        if !ffi::read(process_type).is_empty() {
            return;
        }
        for switch in [
            // No display connection at all: nothing of the desktop (its clipboard included) is
            // reachable from Chromium, and a terminal session without a display can use it.
            // Frames are painted in software (SwiftShader draws WebGL).
            "ozone-platform=headless",
            "use-angle=swiftshader",
            "enable-unsafe-swiftshader",
            "enable-logging=stderr",
            "disable-component-update",
            "disable-background-networking",
            "disable-sync",
            "no-default-browser-check",
            "no-first-run",
            // One switch: Chromium keeps a single disable-features list.
            // Translate is the browser's own UI.
            // OptimizationGuideOnDeviceModel is compiled on. CanLaunchOnDeviceModelService is
            // that feature or LogOnDeviceMetricsOnStartup (off unless a trial enables it).
            // 180s after the optimization guide starts, the performance classifier calls
            // ServiceClient::Get. LaunchService then starts
            // `--utility-sub-type=on_device_model.mojom.OnDeviceModelService`
            // (`--service-sandbox-type=on_device_model_execution`) and a `--type=broker`.
            // Neither is a zygote child, so neither has a death signal. cef_shutdown does
            // not finish them, and a quit while they are up reparents them to init.
            "disable-features=Translate,OptimizationGuideOnDeviceModel,LogOnDeviceMetricsOnStartup",
        ] {
            let (name, value) = switch.split_once('=').unwrap_or((switch, ""));
            let name = CefStr::new(name);
            if value.is_empty() {
                call!(command_line.as_ptr(), append_switch, &name.raw());
            } else {
                let value = CefStr::new(value);
                call!(
                    command_line.as_ptr(),
                    append_switch_with_value,
                    &name.raw(),
                    &value.raw()
                );
            }
        }
    }
}

fn geometry(metrics: &Metrics, previous: Option<Geometry>) -> Geometry {
    let size = metrics.rect().size();
    if size.is_empty() {
        // Empty is "no room", not "hidden": keep the last size (CEF cannot paint 0 by 0).
        return previous.unwrap_or(Geometry {
            width: 800,
            height: 600,
            scale: 1.0,
        });
    }
    Geometry {
        width: size.width,
        height: size.height,
        scale: metrics.scale().factor() as f32,
    }
}

impl<T: CpuTarget> EngineRuntime for CefRuntime<T> {
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
        let metrics = target.metrics();
        let shared = ViewShared::new(id, Arc::clone(&self.shared), geometry(&metrics, None));
        let view = View {
            shared: Arc::clone(&shared),
            target,
            visible: options.visible,
            pointer: (0, 0),
            buttons: 0,
            click: None,
            held: BTreeMap::new(),
            consumed: HashSet::new(),
            edits: HashMap::new(),
            composing: false,
            buffers: HashMap::new(),
            last_frames: 0,
            last_epoch: None,
        };
        if !options.visible {
            shared.run(Action::Hidden(true));
        }
        if (options.zoom - 1.0).abs() > f64::EPSILON {
            shared.run(Action::Zoom(options.zoom));
        }
        let client = handlers::make_client(&shared);
        let geometry = *lock(&shared.geometry);
        // SAFETY: all-zero window and browser settings are valid; sizes set as CEF requires.
        let mut info: CefWindowInfo = unsafe { std::mem::zeroed() };
        info.size = std::mem::size_of::<CefWindowInfo>();
        info.windowless_rendering_enabled = 1;
        info.bounds.width = i32::try_from(geometry.width).unwrap_or(1);
        info.bounds.height = i32::try_from(geometry.height).unwrap_or(1);
        let mut settings: CefBrowserSettings = unsafe { std::mem::zeroed() };
        settings.size = std::mem::size_of::<CefBrowserSettings>();
        settings.windowless_frame_rate = 60;
        // `background_color` sits at offset 248 of `cef_browser_settings_t` (the 150 headers;
        // the rest of the struct is the zeroed tail). Alpha 0 turns on transparent painting
        // for a windowless browser, so a cleared page is a hole. Opaque white, as
        // `CefSettings.background_color` above.
        const BACKGROUND_COLOR: usize = 248;
        let at = BACKGROUND_COLOR - std::mem::offset_of!(CefBrowserSettings, _rest);
        settings._rest[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_ne_bytes());
        // No initial address: a first navigation would race the browser's own.
        let url = CefStr::new("");
        self.shared
            .live
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        // SAFETY: CEF copies the strings and settings and adopts the client's reference.
        let created = unsafe {
            (ffi::api().create_browser)(
                &info,
                client,
                &url.raw(),
                &settings,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if created == 0 {
            self.shared
                .live
                .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
            return Err(EngineError::Runtime(
                "CEF could not create a browser".into(),
            ));
        }
        self.views.insert(id, view);
        Ok(())
    }

    fn command(&mut self, id: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        self.running()?;
        let view = self
            .views
            .get_mut(&id)
            .ok_or(EngineError::UnknownView(id))?;
        let shared = Arc::clone(&view.shared);
        match command {
            EngineCommand::Navigate { navigation, url } => {
                if url
                    .trim_start()
                    .to_ascii_lowercase()
                    .starts_with("javascript:")
                {
                    self.shared.push(EngineEvent::NavigationFailed {
                        view: id,
                        navigation,
                        error: NavigationError::Load("javascript: addresses are not loaded".into()),
                    });
                    return Ok(());
                }
                let superseded = lock(&shared.nav).request_navigate(navigation);
                self.shared.push_all(superseded);
                shared.run(Action::Load(url));
            }
            EngineCommand::Reload { navigation } => {
                let superseded = lock(&shared.nav).request_reload(navigation);
                self.shared.push_all(superseded);
                shared.run(Action::Reload);
            }
            EngineCommand::Stop => shared.run(Action::Stop),
            EngineCommand::Traverse { navigation, delta } => {
                let steps = lock(&shared.nav).request_traverse(navigation, delta);
                match steps {
                    Ok((steps, superseded)) => {
                        self.shared.push_all(superseded);
                        shared.run(Action::History(steps));
                    }
                    Err(event) => self.shared.push(event),
                }
            }
            EngineCommand::SetMetrics(metrics) => {
                let mut geo = lock(&shared.geometry);
                let next = geometry(&metrics, Some(*geo));
                let changed =
                    next.width != geo.width || next.height != geo.height || next.scale != geo.scale;
                *geo = next;
                drop(geo);
                if changed && !metrics.is_empty() {
                    shared.run(Action::Resize);
                }
            }
            EngineCommand::SetVisibility(visible) => {
                view.visible = visible;
                shared.run(Action::Hidden(!visible));
            }
            EngineCommand::SetFocus(focus) => view.set_focus(focus),
            EngineCommand::SetZoom(zoom) => shared.run(Action::Zoom(zoom)),
            EngineCommand::Input(event) => view.input(event, &self.shared),
            EngineCommand::ResolveServiceRequest { request, result } => {
                view.resolve(request, result);
            }
            EngineCommand::AnswerNavigation { request, allow } => {
                let url = lock(&shared.nav).answer(request, allow);
                if let Some(url) = url {
                    shared.run(Action::Load(url));
                }
            }
            EngineCommand::SetSemantics(_) | EngineCommand::Semantic { .. } => {
                return Err(EngineError::Unsupported("semantics"));
            }
            EngineCommand::Find(find) => shared.find(find),
            EngineCommand::ExitFullscreen => return Err(EngineError::Unsupported("fullscreen")),
        }
        Ok(())
    }

    fn site_data(&mut self, _request: SiteDataRequest) -> Result<(), EngineError> {
        // Declared (Chromium stores them), not implemented: refused, so the user is told.
        Err(EngineError::Unsupported(
            "listing or forgetting Chromium's site data",
        ))
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        if self.shut_down {
            return PumpStatus::Idle;
        }
        // Nothing here waits for CEF: its thread wakes the owner loop through the waker.
        let from = events.len();
        let more = self.shared.drain(MAX_PER_PUMP, events);
        // A page that closed itself (`window.close()`): its target goes with the view.
        for event in &events[from..] {
            if let EngineEvent::ViewClosed { view } = event {
                self.views.remove(view);
            }
        }
        if more {
            PumpStatus::Pending { deadline: None }
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
        let epoch = T::permit_epoch(&permit);
        let staged = lock(&view.shared.staged);
        if !staged.valid {
            return Ok(PresentOutcome::NotReady);
        }
        if staged.frames == view.last_frames && view.last_epoch == Some(epoch) {
            return Ok(PresentOutcome::Unchanged);
        }
        let mut bytes = 0usize;
        let buffers = &mut view.buffers;
        view.target
            .submit(permit, &mut |frame| {
                let size = (frame.size.width as usize, frame.size.height as usize);
                let key = (frame.pixels.as_ptr() as usize, frame.stride, size.0, size.1);
                let (copied, state) = present::copy_frame(
                    &staged,
                    frame.pixels,
                    frame.stride,
                    size,
                    epoch,
                    buffers.get(&key),
                );
                bytes += copied;
                if buffers.len() >= 8 && !buffers.contains_key(&key) {
                    buffers.clear();
                }
                buffers.insert(key, state);
                Ok(())
            })
            .map_err(|message| EngineError::View { view: id, message })?;
        view.last_frames = staged.frames;
        view.last_epoch = Some(epoch);
        drop(staged);
        self.presented_bytes += bytes as u64;
        Ok(PresentOutcome::Submitted)
    }

    fn close_view(&mut self, id: ViewId) {
        // The target goes now (on the owner thread); CEF closes the browser and reports
        // `ViewClosed` when it is gone.
        if let Some(view) = self.views.remove(&id) {
            view.shared
                .closing
                .store(true, std::sync::atomic::Ordering::Release);
            view.shared.run(Action::Close);
            self.shared.forget(id);
        }
    }

    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        for (_, view) in self.views.drain() {
            view.shared
                .closing
                .store(true, std::sync::atomic::Ordering::Release);
            view.shared.run(Action::Close);
        }
        // Each `OnBeforeClose` brings the count down; a browser that never answers does not
        // hold the exit forever.
        let deadline = Instant::now() + CLOSE_WAIT;
        while self.shared.live.load(std::sync::atomic::Ordering::Acquire) > 0
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        if self.shared.live.load(std::sync::atomic::Ordering::Acquire) > 0 {
            log::warn!("cef: browsers still open at shutdown; leaving Chromium running");
            self.leaked = true;
        } else {
            // SAFETY: after every browser closed, on the thread that initialized CEF.
            unsafe { (ffi::api().shutdown)() };
        }
        let mut discarded = Vec::new();
        while self.shared.drain(usize::MAX, &mut discarded) {}
    }
}

impl<T: CpuTarget> Drop for CefRuntime<T> {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl<T> View<T> {
    fn set_focus(&mut self, focus: bool) {
        self.shared.run(Action::Focus(focus));
        if !focus {
            // Release every key the page holds as pressed.
            for (virtual_key, modifiers) in std::mem::take(&mut self.held).into_values() {
                self.shared.run(Action::Key {
                    kind: KEY_UP,
                    modifiers,
                    virtual_key,
                    native_key: 0,
                    character: 0,
                });
            }
            self.consumed.clear();
        }
    }

    fn input(&mut self, event: InputEvent, queue: &Arc<Shared>) {
        match event.kind {
            InputKind::Pointer(pointer) => self.pointer(pointer),
            InputKind::Key(key) => self.key(&key, queue),
            InputKind::TextInput(text) => self.text(text),
            InputKind::Focus(FocusEvent::Activated) => self.set_focus(true),
            InputKind::Focus(FocusEvent::Deactivated) => self.set_focus(false),
        }
    }

    fn pointer(&mut self, event: PointerEvent) {
        let at = |p: sighurt_api::geometry::SurfacePoint| (p.x.round() as i32, p.y.round() as i32);
        match event {
            PointerEvent::Enter { position } => {
                self.pointer = at(position);
                self.shared.run(Action::MouseMove {
                    x: self.pointer.0,
                    y: self.pointer.1,
                    modifiers: self.buttons,
                    leave: false,
                });
            }
            PointerEvent::Leave => self.shared.run(Action::MouseMove {
                x: self.pointer.0,
                y: self.pointer.1,
                modifiers: self.buttons,
                leave: true,
            }),
            PointerEvent::Motion {
                position,
                modifiers,
            } => {
                self.pointer = at(position);
                self.shared.run(Action::MouseMove {
                    x: self.pointer.0,
                    y: self.pointer.1,
                    modifiers: keys::modifiers(&modifiers) | self.buttons,
                    leave: false,
                });
            }
            PointerEvent::Button {
                button,
                state,
                position,
                modifiers,
            } => {
                let (cef_button, flag) = match button {
                    PointerButton::Left => (0, keys::flag::LEFT_BUTTON),
                    PointerButton::Middle => (1, keys::flag::MIDDLE_BUTTON),
                    PointerButton::Right => (2, keys::flag::RIGHT_BUTTON),
                    // Back, forward and the rest are the browser's (its keymap binds them).
                    _ => return,
                };
                let (x, y) = at(position);
                self.pointer = (x, y);
                let up = state == ButtonState::Released;
                let count = if up {
                    self.click.as_ref().map_or(1, |click| click.count)
                } else {
                    let now = Instant::now();
                    let count = match &self.click {
                        Some(c)
                            if c.button == cef_button
                                && now.duration_since(c.at) < CLICK_TIME
                                && (c.x - x).abs() <= 4
                                && (c.y - y).abs() <= 4 =>
                        {
                            c.count + 1
                        }
                        _ => 1,
                    };
                    self.click = Some(Click {
                        at: now,
                        button: cef_button,
                        x,
                        y,
                        count,
                    });
                    // The input method's caret defaults to where the user last clicked.
                    *lock(&self.shared.caret) = SurfaceRect {
                        x,
                        y,
                        width: 1,
                        height: 16,
                    };
                    count
                };
                if up {
                    self.buttons &= !flag;
                } else {
                    self.buttons |= flag;
                }
                self.shared.run(Action::MouseClick {
                    x,
                    y,
                    modifiers: keys::modifiers(&modifiers) | self.buttons,
                    button: cef_button,
                    up,
                    count,
                });
            }
            PointerEvent::Scroll {
                delta,
                position,
                modifiers,
            } => {
                let (x, y) = at(position);
                // CEF's positive deltas scroll up and left; Wayland's scroll down and right.
                let (dx, dy) = match delta {
                    ScrollDelta::Pixels { x, y } => (-(x.round() as i32), -(y.round() as i32)),
                    ScrollDelta::Wheel { x, y } => (-x * 40, -y * 40),
                };
                self.shared.run(Action::Wheel {
                    x,
                    y,
                    modifiers: keys::modifiers(&modifiers) | self.buttons,
                    dx,
                    dy,
                });
            }
        }
    }

    fn key(&mut self, key: &KeyEvent, queue: &Arc<Shared>) {
        let code = key.code.0;
        let virtual_key = keys::virtual_key_of(key.code, &key.key);
        let mut modifiers = keys::modifiers(&key.modifiers);
        if key.repeat {
            modifiers |= keys::flag::IS_REPEAT;
        }
        match key.code.location() {
            sighurt_api::input::KeyLocation::Left => modifiers |= keys::flag::IS_LEFT,
            sighurt_api::input::KeyLocation::Right => modifiers |= keys::flag::IS_RIGHT,
            sighurt_api::input::KeyLocation::Numpad => modifiers |= keys::flag::IS_KEY_PAD,
            sighurt_api::input::KeyLocation::Standard => {}
        }
        if key.state == ButtonState::Released {
            self.held.remove(&code);
            if self.consumed.remove(&code) {
                return;
            }
            self.shared.run(Action::Key {
                kind: KEY_UP,
                modifiers,
                virtual_key,
                native_key: i32::try_from(code + 8).unwrap_or(0),
                character: 0,
            });
            return;
        }
        // Copy, cut and paste are the browser's, whichever way they are typed (the chords, the
        // Insert and Delete forms, auto-repeat): a press the engine took is not repeated to the
        // page either.
        if self.consumed.contains(&code) {
            return;
        }
        if let Some(op) = clipboard_op(key) {
            if self.clipboard(op, queue) {
                self.consumed.insert(code);
                return;
            }
        }
        self.held.insert(code, (virtual_key, modifiers));
        let native_key = i32::try_from(code + 8).unwrap_or(0);
        self.shared.run(Action::Key {
            kind: KEY_RAW_DOWN,
            modifiers,
            virtual_key,
            native_key,
            character: 0,
        });
        let mut typed: Vec<u16> = key
            .text
            .as_deref()
            .filter(|_| !key.modifiers.control || key.modifiers.alt_graph)
            .map(|text| {
                text.chars()
                    .filter(|c| !c.is_control() || *c == '\r' || *c == '\t')
                    .flat_map(|c| {
                        let mut buffer = [0u16; 2];
                        c.encode_utf16(&mut buffer).to_vec()
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Enter has no text but the page needs its character to submit a form.
        if typed.is_empty() && virtual_key == 0x0D && !key.modifiers.control {
            typed.push(0x0D);
        }
        for character in typed {
            self.shared.run(Action::Key {
                kind: KEY_CHAR,
                modifiers,
                virtual_key: i32::from(character),
                native_key,
                character,
            });
        }
    }

    /// Copy, cut or paste: asks the browser's clipboard. Whether the key was taken.
    fn clipboard(&mut self, op: ClipboardOp, queue: &Arc<Shared>) -> bool {
        let Some(binding) = self.shared.binding() else {
            return false;
        };
        let Some(document) = lock(&self.shared.nav).document() else {
            return false;
        };
        let request = RequestId::next();
        let service = match op {
            ClipboardOp::Paste => {
                if self.edits.len() >= MAX_EDITS {
                    return true;
                }
                self.edits.insert(request, (op, binding));
                Service::ClipboardRead {
                    selection: Selection::Clipboard,
                    mime_types: vec!["text/plain;charset=utf-8".into(), "text/plain".into()],
                    limit: MAX_PASTE,
                }
            }
            ClipboardOp::Copy | ClipboardOp::Cut => {
                let text = lock(&self.shared.selection).clone();
                if text.is_empty() {
                    return true; // nothing selected: nothing to copy, and the page is not asked
                }
                if op == ClipboardOp::Cut {
                    if self.edits.len() >= MAX_EDITS {
                        return true;
                    }
                    // The selection is deleted only once the write is approved.
                    self.edits.insert(request, (op, binding));
                }
                Service::ClipboardWrite {
                    selection: Selection::Clipboard,
                    items: vec![ClipboardData {
                        mime_type: "text/plain;charset=utf-8".into(),
                        data: text.into_bytes(),
                    }],
                }
            }
        };
        queue.push(EngineEvent::ServiceRequested(ServiceRequest {
            id: request,
            view: self.shared.id,
            document,
            origin: origin(&self.shared),
            service,
        }));
        true
    }

    fn resolve(&mut self, request: RequestId, result: ServiceResult) {
        let Some((op, binding)) = self.edits.remove(&request) else {
            return; // a copy or a download: nothing more to do
        };
        match (op, result) {
            (ClipboardOp::Paste, Ok(ServiceReply::Clipboard(data))) => {
                let mut text = String::from_utf8_lossy(&data.data).into_owned();
                if text.len() > MAX_PASTE {
                    let mut end = MAX_PASTE;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                }
                // Typed into the document and frame that asked, if they still are the page's.
                if !text.is_empty() {
                    self.shared.insert_text(&text, binding);
                }
            }
            (ClipboardOp::Cut, Ok(ServiceReply::Done)) => self.shared.delete_selection(binding),
            (_, Err(ServiceError::Failed(why))) => {
                log::debug!("cef: clipboard request failed: {why}");
            }
            _ => {}
        }
    }

    fn text(&mut self, event: TextInputEvent) {
        if let Some(preedit) = event.preedit {
            self.composing = true;
            self.shared.run(Action::ImeSet(preedit.text));
        } else if event.commit.is_none() && self.composing {
            self.composing = false;
            self.shared.run(Action::ImeCancel);
        }
        if let Some(text) = event.commit {
            // Chromium takes a commit only inside a composition it knows of; text with none
            // is inserted as one of its own into what is the page now.
            if std::mem::take(&mut self.composing) {
                self.shared.run(Action::ImeCommit(text));
            } else if let Some(binding) = self.shared.binding() {
                self.shared.insert_text(&text, binding);
            }
        }
    }
}

/// What a key asks of the clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClipboardOp {
    Copy,
    Cut,
    Paste,
}

/// The clipboard operation a key press is, in any of its forms: Ctrl+C, Ctrl+X, Ctrl+V,
/// Ctrl+Insert, Shift+Delete, Shift+Insert.
fn clipboard_op(key: &KeyEvent) -> Option<ClipboardOp> {
    let m = &key.modifiers;
    if m.alt || m.logo || m.alt_graph {
        return None;
    }
    match &key.key {
        Key::Character(text) if m.control && !m.shift => match text.to_lowercase().as_str() {
            "c" => Some(ClipboardOp::Copy),
            "x" => Some(ClipboardOp::Cut),
            "v" => Some(ClipboardOp::Paste),
            _ => None,
        },
        Key::Named(name) => match (name.as_str(), m.control, m.shift) {
            ("Insert", true, false) => Some(ClipboardOp::Copy),
            ("Insert", false, true) => Some(ClipboardOp::Paste),
            ("Delete", false, true) => Some(ClipboardOp::Cut),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::input::{ButtonState, KeyCode, Modifiers};

    use super::*;

    fn press(key: Key, shift: bool, control: bool) -> KeyEvent {
        KeyEvent {
            code: KeyCode(1),
            key,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers {
                shift,
                control,
                ..Modifiers::default()
            },
            consumed: Modifiers::default(),
            text: None,
        }
    }

    #[test]
    fn every_way_to_use_the_clipboard_is_the_browsers() {
        let c = |s: &str| Key::Character(s.into());
        let n = |s: &str| Key::Named(s.into());
        assert_eq!(
            clipboard_op(&press(c("c"), false, true)),
            Some(ClipboardOp::Copy)
        );
        assert_eq!(
            clipboard_op(&press(c("X"), false, true)),
            Some(ClipboardOp::Cut)
        );
        assert_eq!(
            clipboard_op(&press(c("v"), false, true)),
            Some(ClipboardOp::Paste)
        );
        assert_eq!(
            clipboard_op(&press(n("Insert"), true, false)),
            Some(ClipboardOp::Paste)
        );
        assert_eq!(
            clipboard_op(&press(n("Insert"), false, true)),
            Some(ClipboardOp::Copy)
        );
        assert_eq!(
            clipboard_op(&press(n("Delete"), true, false)),
            Some(ClipboardOp::Cut)
        );
        // Other chords belong to the page.
        assert_eq!(clipboard_op(&press(c("a"), false, true)), None);
        assert_eq!(clipboard_op(&press(c("v"), false, false)), None);
        assert_eq!(clipboard_op(&press(n("Delete"), false, false)), None);
    }
}
