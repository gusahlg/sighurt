//! The Wayland connection: connecting, binding globals, and dispatching events.
//!
//! Binds only the protocol versions the host knows how to use. `wl_compositor` (version 4 or
//! newer, for `damage_buffer`), `wl_subcompositor`, `wl_shm` and `xdg_wm_base` are required;
//! when any is missing, [`Host::new`] fails with [`HostError::MissingGlobals`] naming all of
//! them. Optional globals (viewporter, fractional scale, single-pixel buffers, text input, the
//! data devices, cursor shapes, presentation time, decorations) are reported as
//! [`Capabilities`].
//!
//! The event queue joins the application's calloop loop through calloop-wayland-source, with
//! the prepare-read, read, dispatch, flush cycle; there are no hand-written polling loops, and
//! the only blocking roundtrip is the registry's at startup. Key repeat, layout grace periods,
//! the client-size wait and clipboard transfers are calloop sources too. Uses the system
//! libwayland backend, which raw-window-handle interop requires.
//!
//! A protocol error or a lost connection invalidates the whole connection. The host reports it
//! once as [`HostEvent::ConnectionLost`], with the protocol error's interface, object and
//! message when there is one, and stops dispatching; it is a platform failure, not a page error.
//!
//! # Using the host
//!
//! The executable owns the loop and the host:
//!
//! 1. [`Host::new`] with the loop's handle, [`Host::create_window`], and targets with
//!    [`Host::create_target`].
//! 2. Each iteration, after dispatching: drain [`Host::next_event`]; answer
//!    [`HostEvent::WindowConfigured`] with [`Host::apply_placement`]; forward
//!    [`HostEvent::MetricsChanged`] to the target's presenter and input to its owner;
//!    [`Host::invalidate`] targets with something new; hand out [`Host::permits`] (or
//!    [`Host::fill_permits`] into a reused list).
//! 3. End the iteration with [`Host::flush`], which commits finished layout transactions,
//!    forgets retired targets and flushes the connection.
//!
//! Must not own: what the window shows.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::fs::File;
use std::rc::Rc;
use std::time::Instant;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{
    EventIterator, EventSource, Interest, LoopHandle, Mode, Poll, PostAction, Readiness,
    RegistrationToken, Token, TokenFactory,
};
use sighurt_api::geometry::{LogicalSize, Metrics, Scale};
use sighurt_api::ids::{RequestId, SeatId, TargetId, WindowId};
use sighurt_api::input::InputEvent;
use sighurt_api::services::{ClipboardData, ServiceError};
use smithay_client_toolkit::compositor::CompositorState;
use smithay_client_toolkit::data_device_manager::DataDeviceManagerState;
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::primary_selection::PrimarySelectionManagerState;
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::SeatState;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_pointer,
    delegate_primary_selection, delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell,
    delegate_xdg_window, registry_handlers,
};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{registry_queue_init, GlobalList};
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_region::WlRegion;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::{Connection, Dispatch, DispatchError, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use wayland_protocols::wp::single_pixel_buffer::v1::client::wp_single_pixel_buffer_manager_v1::WpSinglePixelBufferManagerV1;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;

use crate::clipboard::Clipboard;
use crate::input::Seat;
use crate::scheduler::PresentStats;
use crate::shm::BufferStats;
use crate::targets::{Env, PresentMode, Target};
use crate::window::Window;

/// The Wayland host: the one connection to the compositor, its windows and targets. Lives on the
/// thread that owns the event loop; it is not `Send`.
pub struct Host {
    pub(crate) conn: Connection,
    pub(crate) qh: QueueHandle<Host>,
    pub(crate) registry: RegistryState,
    pub(crate) outputs: OutputState,
    pub(crate) seat_state: SeatState,
    pub(crate) compositor: CompositorState,
    pub(crate) shm: Shm,
    pub(crate) xdg: XdgShell,
    pub(crate) subcompositor: WlSubcompositor,
    pub(crate) globals: Globals,
    pub(crate) env: Rc<Env>,
    pub(crate) loop_ops: Box<dyn LoopOps>,
    pub(crate) windows: HashMap<WindowId, Window>,
    pub(crate) targets: HashMap<TargetId, Rc<RefCell<Target>>>,
    /// Which window or target each of the host's surfaces is.
    pub(crate) surfaces: HashMap<ObjectId, SurfaceOwner>,
    pub(crate) seats: Vec<Seat>,
    /// The seat with the latest input, which clipboard operations use.
    pub(crate) active_seat: Option<SeatId>,
    pub(crate) clipboard: Clipboard,
    pub(crate) events: VecDeque<HostEvent>,
    pub(crate) presentation_clock: Option<u32>,
    // Ids walked by `fill_permits`, kept so that pass does not allocate the list.
    pub(crate) permit_ids: Vec<TargetId>,
    lost: bool,
}

/// Optional globals, bound when the compositor has them.
pub(crate) struct Globals {
    pub viewporter: Option<WpViewporter>,
    pub fractional: Option<WpFractionalScaleManagerV1>,
    pub single_pixel: Option<WpSinglePixelBufferManagerV1>,
    pub presentation: Option<WpPresentation>,
    pub text_input: Option<ZwpTextInputManagerV3>,
    pub data_device: Option<DataDeviceManagerState>,
    pub primary: Option<PrimarySelectionManagerState>,
}

/// The optional protocols the compositor offers, and so what the host can do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// `wp_viewporter`: fractional scaling and a one-pixel background.
    pub viewporter: bool,
    /// `wp_fractional_scale_v1`: fractional scales (with a viewporter), else integer ones.
    pub fractional_scale: bool,
    /// `wp_single_pixel_buffer_v1`: a background without shared memory.
    pub single_pixel_buffer: bool,
    /// `zwp_text_input_v3`: input methods. Without it only keyboard and compose input work.
    pub text_input: bool,
    /// `wl_data_device`: the clipboard.
    pub clipboard: bool,
    /// `zwp_primary_selection_v1`: the primary selection.
    pub primary_selection: bool,
    /// `wp_cursor_shape_v1`: cursors by name; otherwise from the cursor theme.
    pub cursor_shape: bool,
    /// `wp_presentation`: presentation feedback for diagnostics.
    pub presentation_time: bool,
    /// `zxdg_decoration_manager_v1`: server-side decorations; otherwise borderless.
    pub decorations: bool,
}

/// Why the host could not start.
#[derive(Debug)]
pub enum HostError {
    /// No Wayland compositor to connect to.
    Connect(String),
    /// The compositor lacks globals the host needs; each entry names one, with the version.
    MissingGlobals(Vec<String>),
    /// The event loop refused the Wayland source.
    Loop(String),
    /// No such window.
    UnknownWindow(WindowId),
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Connect(error) => write!(f, "cannot connect to a Wayland compositor: {error}"),
            Self::MissingGlobals(missing) => write!(
                f,
                "the Wayland compositor lacks what Sighurt needs: {}",
                missing.join(", ")
            ),
            Self::Loop(error) => write!(
                f,
                "cannot add the Wayland connection to the event loop: {error}"
            ),
            Self::UnknownWindow(window) => write!(f, "no such window: {window:?}"),
        }
    }
}

impl std::error::Error for HostError {}

/// Something that happened, for the application to act on.
#[derive(Clone, Debug, PartialEq)]
pub enum HostEvent {
    /// The window was configured for the first time, or its size or scale changed. Answer with
    /// a placement for exactly this size ([`Host::apply_placement`]). Only the latest one per
    /// window waits in the queue.
    WindowConfigured {
        /// The window.
        window: WindowId,
        /// Its logical size.
        size: LogicalSize,
        /// The scale it renders at.
        scale: Scale,
    },
    /// The user asked to close the window. The browser decides whether it closes.
    CloseRequested {
        /// The window.
        window: WindowId,
    },
    /// A target has new metrics. Forward them to its presenter (an engine view gets
    /// `EngineCommand::SetMetrics`); a new epoch needs a new frame.
    MetricsChanged {
        /// The target.
        target: TargetId,
        /// Its metrics now.
        metrics: Metrics,
    },
    /// Input. Pointer input goes to the target under the pointer, or to the one holding a
    /// drag, in its surface-local coordinates; keyboard and focus input go to the window's
    /// logical focus target ([`Host::set_keyboard_focus`]); input-method updates go to the
    /// target whose editor enabled them. `target` is `None` over the window's background or
    /// with no focus target set.
    Input {
        /// The window.
        window: WindowId,
        /// The target it is for.
        target: Option<TargetId>,
        /// What happened.
        event: InputEvent,
    },
    /// A clipboard read finished.
    ClipboardRead {
        /// The request that started it.
        request: RequestId,
        /// The content, or why there is none.
        result: Result<ClipboardData, ServiceError>,
    },
    /// The connection to the compositor is gone. Nothing more will be shown; shut down.
    ConnectionLost {
        /// What happened, with the protocol error when there was one.
        error: String,
    },
}

/// Which window, and which target of it, a surface of the host is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SurfaceOwner {
    Root(WindowId),
    Target(WindowId, TargetId),
}

impl SurfaceOwner {
    pub fn window(self) -> WindowId {
        match self {
            Self::Root(window) | Self::Target(window, _) => window,
        }
    }

    pub fn target(self) -> Option<TargetId> {
        match self {
            Self::Root(_) => None,
            Self::Target(_, target) => Some(target),
        }
    }
}

/// Timers the host sets in the event loop.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TimerEvent {
    KeyRepeat(SeatId),
    LayoutGrace(WindowId),
    /// No configure has named this window's size yet. Place it at the client default.
    ClientSize(WindowId),
    Transfer(u64),
}

/// The event loop, seen from the host, whatever the application's loop data is.
pub(crate) trait LoopOps {
    /// Calls the host at `at` with `event`.
    fn timer(&self, at: Instant, event: TimerEvent) -> Option<RegistrationToken>;
    /// Calls the host whenever `file` is ready for `interest`, for transfer `id`.
    fn pipe(&self, file: File, interest: Interest, id: u64) -> Option<RegistrationToken>;
    fn remove(&self, token: RegistrationToken);
}

struct CalloopOps<T: 'static> {
    handle: LoopHandle<'static, T>,
    host: fn(&mut T) -> &mut Host,
}

impl<T: 'static> LoopOps for CalloopOps<T> {
    fn timer(&self, at: Instant, event: TimerEvent) -> Option<RegistrationToken> {
        let host = self.host;
        let source = Timer::from_deadline(at);
        self.handle
            .insert_source(source, move |now, _, data| {
                match host(data).on_timer(event, now) {
                    Some(next) => TimeoutAction::ToInstant(next),
                    None => TimeoutAction::Drop,
                }
            })
            .ok()
    }

    fn pipe(&self, file: File, interest: Interest, id: u64) -> Option<RegistrationToken> {
        let host = self.host;
        let source = Generic::new(file, interest, Mode::Level);
        self.handle
            .insert_source(source, move |_, file, data| {
                Ok(host(data).transfer_ready(id, file))
            })
            .ok()
    }

    fn remove(&self, token: RegistrationToken) {
        self.handle.remove(token);
    }
}

/// calloop-wayland-source's `WaylandSource`, turning a failed connection into one callback with
/// the error instead of an error from the whole event loop.
struct HostSource {
    inner: WaylandSource<Host>,
    failed: Option<calloop::Error>,
    token: Option<Token>,
}

impl EventSource for HostSource {
    type Event = Option<calloop::Error>;
    type Metadata = EventQueue<Host>;
    type Ret = Result<usize, DispatchError>;
    type Error = calloop::Error;

    const NEEDS_EXTRA_LIFECYCLE_EVENTS: bool = true;

    fn process_events<F>(
        &mut self,
        readiness: Readiness,
        token: Token,
        mut callback: F,
    ) -> Result<PostAction, Self::Error>
    where
        F: FnMut(Self::Event, &mut Self::Metadata) -> Self::Ret,
    {
        let result = match self.failed.take() {
            Some(error) => Err(error),
            None => self
                .inner
                .process_events(readiness, token, |(), queue| callback(None, queue)),
        };
        match result {
            Ok(action) => Ok(action),
            Err(error) => {
                let _ = callback(Some(error), self.inner.queue());
                Ok(PostAction::Remove)
            }
        }
    }

    fn register(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.token = Some(factory.token());
        self.inner.register(poll, factory)
    }

    fn reregister(&mut self, poll: &mut Poll, factory: &mut TokenFactory) -> calloop::Result<()> {
        self.token = Some(factory.token());
        self.inner.reregister(poll, factory)
    }

    fn unregister(&mut self, poll: &mut Poll) -> calloop::Result<()> {
        self.inner.unregister(poll)
    }

    fn before_sleep(&mut self) -> calloop::Result<Option<(Readiness, Token)>> {
        match self.inner.before_sleep() {
            Ok(wake) => Ok(wake),
            Err(error) => {
                // Flushing failed: the compositor is gone. Wake up to report it.
                self.failed = Some(error);
                Ok(self.token.map(|token| (Readiness::EMPTY, token)))
            }
        }
    }

    fn before_handle_events(&mut self, events: EventIterator<'_>) {
        self.inner.before_handle_events(events);
    }
}

/// The required globals and their minimum versions.
const REQUIRED: [(&str, u32); 4] = [
    ("wl_compositor", 4),
    ("wl_subcompositor", 1),
    ("wl_shm", 1),
    ("xdg_wm_base", 1),
];

/// The required globals `globals` lacks, as "name (version n or newer)".
fn missing_globals(globals: &GlobalList) -> Vec<String> {
    globals.contents().with_list(|list| {
        REQUIRED
            .iter()
            .filter(|(name, min)| {
                !list
                    .iter()
                    .any(|g| g.interface == *name && g.version >= *min)
            })
            .map(|(name, min)| match min {
                1 => (*name).to_string(),
                _ => format!("{name} (version {min} or newer)"),
            })
            .collect()
    })
}

impl Host {
    /// Connects to the compositor named by `WAYLAND_DISPLAY`, binds its globals and joins the
    /// event loop of `handle`. `host` finds the host in the loop's data; the host must be there
    /// before the loop next dispatches.
    pub fn new<T: 'static>(
        handle: &LoopHandle<'static, T>,
        host: fn(&mut T) -> &mut Host,
    ) -> Result<Self, HostError> {
        let conn = Connection::connect_to_env().map_err(|e| HostError::Connect(e.to_string()))?;
        let (globals, queue) =
            registry_queue_init::<Host>(&conn).map_err(|e| HostError::Connect(e.to_string()))?;
        let missing = missing_globals(&globals);
        if !missing.is_empty() {
            return Err(HostError::MissingGlobals(missing));
        }
        let qh = queue.handle();
        let bind_error =
            |e: wayland_client::globals::BindError| HostError::MissingGlobals(vec![e.to_string()]);
        let compositor = CompositorState::bind(&globals, &qh).map_err(bind_error)?;
        let shm = Shm::bind(&globals, &qh).map_err(bind_error)?;
        let xdg = XdgShell::bind(&globals, &qh).map_err(bind_error)?;
        let subcompositor = globals.bind(&qh, 1..=1, ()).map_err(bind_error)?;
        let optional = Globals {
            viewporter: globals.bind(&qh, 1..=1, ()).ok(),
            fractional: globals.bind(&qh, 1..=1, ()).ok(),
            single_pixel: globals.bind(&qh, 1..=1, ()).ok(),
            presentation: globals.bind(&qh, 1..=1, ()).ok(),
            text_input: globals.bind(&qh, 1..=1, ()).ok(),
            data_device: DataDeviceManagerState::bind(&globals, &qh).ok(),
            primary: PrimarySelectionManagerState::bind(&globals, &qh).ok(),
        };
        let env = Rc::new(Env {
            qh: qh.clone(),
            shm: shm.wl_shm().clone(),
            presentation: optional.presentation.clone(),
        });
        let source = HostSource {
            inner: WaylandSource::new(conn.clone(), queue),
            failed: None,
            token: None,
        };
        handle
            .insert_source(source, move |error, queue, data| {
                let h = host(data);
                match error {
                    None => queue.dispatch_pending(h),
                    Some(error) => {
                        h.connection_lost(error);
                        Ok(0)
                    }
                }
            })
            .map_err(|e| HostError::Loop(e.error.to_string()))?;
        let host = Host {
            registry: RegistryState::new(&globals),
            outputs: OutputState::new(&globals, &qh),
            seat_state: SeatState::new(&globals, &qh),
            conn,
            qh,
            compositor,
            shm,
            xdg,
            subcompositor,
            globals: optional,
            env,
            loop_ops: Box::new(CalloopOps {
                handle: handle.clone(),
                host,
            }),
            windows: HashMap::new(),
            targets: HashMap::new(),
            surfaces: HashMap::new(),
            seats: Vec::new(),
            active_seat: None,
            clipboard: Clipboard::default(),
            events: VecDeque::new(),
            presentation_clock: None,
            permit_ids: Vec::new(),
            lost: false,
        };
        // Seats that existed before the registry was read get no `new_seat` call.
        let mut host = host;
        let qh = host.qh.clone();
        for wl in host.seat_state.seats() {
            let mut seat = Seat::new(wl);
            host.attach_seat_services(&mut seat, &qh);
            host.seats.push(seat);
        }
        Ok(host)
    }

    /// What the compositor offers beyond the required protocols.
    pub fn capabilities(&self) -> Capabilities {
        let has = |name: &str| self.registry.globals_by_interface(name).next().is_some();
        let g = &self.globals;
        Capabilities {
            viewporter: g.viewporter.is_some(),
            fractional_scale: g.fractional.is_some(),
            single_pixel_buffer: g.single_pixel.is_some(),
            text_input: g.text_input.is_some(),
            clipboard: g.data_device.is_some(),
            primary_selection: g.primary.is_some(),
            cursor_shape: has("wp_cursor_shape_manager_v1"),
            presentation_time: g.presentation.is_some(),
            decorations: has("zxdg_decoration_manager_v1"),
        }
    }

    /// The next event, oldest first.
    pub fn next_event(&mut self) -> Option<HostEvent> {
        self.events.pop_front()
    }

    /// Ends a loop iteration: forgets retired targets, maps targets whose late frame arrived,
    /// commits layout transactions whose frames are all in, and flushes requests to the
    /// compositor. Call it after presenting.
    pub fn flush(&mut self) {
        self.release_retired_targets();
        self.map_late_frames();
        self.complete_transactions();
        // A failure shows up at the next dispatch, which reports ConnectionLost.
        let _ = self.conn.flush();
    }

    /// Whether the connection is gone.
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Diagnostics: what each target is doing and holds.
    pub fn diagnostics(&self) -> Diagnostics {
        let mut targets: Vec<TargetDiagnostics> = self
            .targets
            .values()
            .map(|t| {
                let t = t.borrow();
                TargetDiagnostics {
                    target: t.id,
                    window: t.window,
                    mode: t.mode,
                    metrics: t.metrics,
                    mapped: t.role.is_some(),
                    shown: t.want_shown,
                    invalidated: t.clock.is_invalidated(),
                    awaiting_callback: t.clock.awaits_callback(),
                    stats: t.stats,
                    buffers: t.shm.as_ref().map(|b| b.stats()).unwrap_or_default(),
                }
            })
            .collect();
        targets.sort_by_key(|t| t.target);
        Diagnostics {
            capabilities: self.capabilities(),
            targets,
            pending_events: self.events.len(),
        }
    }

    /// Calls `f` with each target's id and the buffers it holds, without building a report.
    pub fn for_each_buffer(&self, mut f: impl FnMut(TargetId, BufferStats)) {
        for target in self.targets.values() {
            let target = target.borrow();
            let stats = target
                .shm
                .as_ref()
                .map(|buffers| buffers.stats())
                .unwrap_or_default();
            f(target.id, stats);
        }
    }

    fn connection_lost(&mut self, error: calloop::Error) {
        if self.lost {
            return;
        }
        self.lost = true;
        let error = match self.conn.protocol_error() {
            Some(e) => format!(
                "protocol error {} on {}@{}: {}",
                e.code, e.object_interface, e.object_id, e.message
            ),
            None => error.to_string(),
        };
        for index in 0..self.seats.len() {
            if let Some(token) = self.seats[index]
                .keyboard
                .as_mut()
                .and_then(|k| k.take_repeat_timer())
            {
                self.loop_ops.remove(token);
            }
        }
        self.events.push_back(HostEvent::ConnectionLost { error });
    }

    /// A timer the host set fired at `now`. Returns when to fire again, if at all.
    fn on_timer(&mut self, event: TimerEvent, now: Instant) -> Option<Instant> {
        match event {
            TimerEvent::KeyRepeat(seat) => self.on_key_repeat(seat, now),
            TimerEvent::LayoutGrace(window) => {
                self.layout_grace_over(window);
                None
            }
            TimerEvent::ClientSize(window) => {
                self.accept_client_size(window);
                None
            }
            TimerEvent::Transfer(id) => {
                self.transfer_timed_out(id);
                None
            }
        }
    }

    /// Creates the seat's text input and data devices, where the compositor has them.
    pub(crate) fn attach_seat_services(&self, seat: &mut Seat, qh: &QueueHandle<Host>) {
        if let Some(manager) = &self.globals.text_input {
            seat.text_input = Some(crate::text_input::TextInput::new(
                manager, &seat.wl, seat.id, qh,
            ));
        }
        if let Some(manager) = &self.globals.data_device {
            seat.data_device = Some(manager.get_data_device(qh, &seat.wl));
        }
        if let Some(manager) = &self.globals.primary {
            seat.primary = Some(manager.get_selection_device(qh, &seat.wl));
        }
    }

    /// `window` is gone: no seat's input goes to it any more.
    pub(crate) fn window_closed(&mut self, window: WindowId) {
        for index in 0..self.seats.len() {
            if let Some(timer) = self.seats[index].forget_window(window) {
                self.loop_ops.remove(timer);
            }
            self.sync_text_input(index);
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        // Children before their parents: targets first (a leased one only loses its role here;
        // its surface lives until the lease is dropped), then the windows, then input devices.
        for target in self.targets.values() {
            target.borrow_mut().unmap();
        }
        self.targets.clear();
        self.windows.clear();
        self.seats.clear();
        let _ = self.conn.flush();
    }
}

/// What the host knows about each target, for diagnostics.
#[derive(Clone, Debug)]
pub struct Diagnostics {
    /// The compositor's optional protocols.
    pub capabilities: Capabilities,
    /// Every live target.
    pub targets: Vec<TargetDiagnostics>,
    /// Events not yet taken with [`Host::next_event`].
    pub pending_events: usize,
}

/// One target's state, for diagnostics.
#[derive(Clone, Debug)]
pub struct TargetDiagnostics {
    /// The target.
    pub target: TargetId,
    /// Its window.
    pub window: WindowId,
    /// How it presents.
    pub mode: PresentMode,
    /// Its metrics: logical and buffer size, scale, layout epoch.
    pub metrics: Metrics,
    /// Whether it has a subsurface role (mapped, or being mapped).
    pub mapped: bool,
    /// Whether the latest placement shows it.
    pub shown: bool,
    /// Whether an invalidation waits for a permit.
    pub invalidated: bool,
    /// Whether the next frame waits for a frame callback.
    pub awaiting_callback: bool,
    /// Permits, submissions, callbacks, releases and presentation feedback.
    pub stats: PresentStats,
    /// SHM buffers held.
    pub buffers: BufferStats,
}

impl ShmHandler for Host {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl OutputHandler for Host {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: WlOutput) {}
}

impl ProvidesRegistryState for Host {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }

    registry_handlers![OutputState, SeatState];
}

/// Globals and objects without events.
macro_rules! no_events {
    ($($proxy:ty),* $(,)?) => {$(
        impl Dispatch<$proxy, ()> for Host {
            fn event(_: &mut Host, _: &$proxy, _: <$proxy as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Host>) {}
        }
    )*};
}

no_events![
    WlSubcompositor,
    WlSubsurface,
    WlRegion,
    WpSinglePixelBufferManagerV1,
    WlBuffer
];

delegate_compositor!(Host);
delegate_output!(Host);
delegate_shm!(Host);
delegate_seat!(Host);
delegate_pointer!(Host);
delegate_xdg_shell!(Host);
delegate_xdg_window!(Host);
delegate_registry!(Host);
delegate_data_device!(Host);
delegate_primary_selection!(Host);
