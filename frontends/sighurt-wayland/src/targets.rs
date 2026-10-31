//! Presentation targets: the host-managed destinations chrome and engine views present into.
//!
//! A target ([`TargetId`]) is a `wl_surface` the host created and places as a subsurface of its
//! window's root. A [`TargetLease`] keeps its native backing alive and gives a trusted presenter
//! the target id, the current [`Metrics`], the presentation mode chosen when it was created
//! ([`PresentMode::Shm`], or [`PresentMode::Gl`] for a native GL path), and, for GL, the raw
//! display and window handles through one unsafe accessor ([`TargetLease::gl_handles`]) whose
//! lifetime rules are documented there and nowhere else. A [`FramePermit`] allows one
//! presentation for one target at one layout epoch: it is consumed by use, not `Clone`, and
//! [`FramePermit::is_current`] tells a presenter whose rendering finished late whether its
//! result is still wanted.
//!
//! # Commit ownership
//!
//! On an SHM target the host commits the leaf, in [`ShmFrame::submit`](crate::ShmFrame::submit)
//! and nowhere else. On a GL target the presenter's swap attaches and commits the leaf itself,
//! so the host never attaches or commits anything on it: what the host changes on a GL leaf
//! (viewport, buffer scale, input region, the frame callback it requests with each permit) is
//! pending state that the presenter's next swap commits. Positions and stacking are the root's
//! state, which only the host commits.
//!
//! # Destruction order
//!
//! 1. The presenter destroys everything it made from the lease (the EGL surface and
//!    `wl_egl_window`; for Servo, the `WindowRenderingContext`) and terminates its EGL display
//!    if it opened one.
//! 2. It drops the lease ([`TargetLease::retire`]). Outstanding permits end with it.
//! 3. The host, at its next [`Host::flush`], forgets the target, and once no permit refers to it
//!    destroys its native objects in this order: SHM buffers, viewport, subsurface role,
//!    surface.
//! 4. The Wayland connection stays open while any lease exists: each lease holds a reference to
//!    it, so a presenter's EGL display never outlives the `wl_display` it uses.
//!
//! The host never destroys a leased target's surface, not even when its window closes (the
//! surface is only unmapped then). Step 1 before 2 is the presenter's promise, made by calling
//! the unsafe [`TargetLease::gl_handles`].
//!
//! Must not own: what is drawn, or moving live views between windows.

use std::cell::{RefCell, RefMut};
use std::ffi::c_void;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::rc::Rc;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, WindowHandle,
};
use sighurt_api::engine::PresentOutcome;
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{Metrics, WindowRect};
use sighurt_api::ids::{LayoutEpoch, TargetId, ViewId, WindowId};
use wayland_client::protocol::wl_shm::WlShm;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::presentation_time::client::wp_presentation::WpPresentation;
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::connection::Host;
use crate::placement::InputRegion;
use crate::scheduler::{FeedbackData, FrameClock, FrameData, PresentStats};
use crate::shm::{
    AcquireError, BufferData, BufferKey, BufferOwner, ShmBuffer, ShmFrame, Swapchain,
};

/// How a target's frames reach the compositor, chosen when the target is created.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresentMode {
    /// The host's shared-memory buffers, painted on the CPU: [`FramePermit::shm_frame`].
    Shm,
    /// A native GL presenter (EGL on the target's `wl_surface`) that commits the leaf with its
    /// own swaps: [`TargetLease::gl_handles`] and [`FramePermit::finish`].
    Gl,
}

/// What the host's objects need to present on a target without the host at hand.
pub(crate) struct Env {
    pub qh: QueueHandle<Host>,
    pub shm: WlShm,
    pub presentation: Option<WpPresentation>,
}

/// A target's state, shared by the host, its lease and its permits.
pub(crate) struct Target {
    pub id: TargetId,
    pub window: WindowId,
    pub view: Option<ViewId>,
    pub mode: PresentMode,
    pub env: Rc<Env>,
    pub surface: WlSurface,
    pub viewport: Option<WpViewport>,
    /// The subsurface role, while the target is mapped or being mapped.
    pub role: Option<WlSubsurface>,
    pub synced: bool,
    pub metrics: Metrics,
    /// Whether a placement has positioned the target yet.
    pub placed: bool,
    /// Whether the latest placement shows it (visible, with room).
    pub want_shown: bool,
    pub input: InputRegion,
    /// The epoch of the last frame submitted.
    pub presented: Option<LayoutEpoch>,
    pub clock: FrameClock,
    pub stats: PresentStats,
    pub shm: Option<Swapchain<ShmBuffer>>,
    pub cursor: Cursor,
    /// False once the lease is gone; the host then forgets the target.
    pub leased: bool,
}

impl Target {
    pub fn new(
        window: WindowId,
        view: Option<ViewId>,
        mode: PresentMode,
        env: Rc<Env>,
        surface: WlSurface,
        viewport: Option<WpViewport>,
        metrics: Metrics,
    ) -> Self {
        Self {
            id: TargetId::next(),
            window,
            view,
            mode,
            env,
            surface,
            viewport,
            role: None,
            synced: false,
            metrics,
            placed: false,
            want_shown: false,
            input: InputRegion::Whole,
            presented: None,
            clock: FrameClock::new(),
            stats: PresentStats::default(),
            shm: (mode == PresentMode::Shm).then(Swapchain::new),
            cursor: Cursor::Default,
            leased: true,
        }
    }

    /// Whether the last frame submitted is for the current metrics.
    pub fn has_current_frame(&self) -> bool {
        self.presented == Some(self.metrics.epoch())
    }

    /// Whether an SHM buffer of the current size could be lent now.
    fn buffer_available(&self) -> bool {
        match &self.shm {
            Some(buffers) => buffers.can_acquire(self.metrics.buffer()),
            None => true,
        }
    }

    /// Whether a permit may be issued now.
    pub fn can_present(&self) -> bool {
        self.leased && self.clock.can_present(self.want_shown) && self.buffer_available()
    }

    /// Unmaps the target at once by destroying its subsurface role (`wl_subsurface.destroy`
    /// unmaps immediately). Neither the buffer nor any commit of the leaf is touched, so this
    /// is safe on a GL leaf too; mapping again creates a new role.
    pub fn unmap(&mut self) {
        if let Some(role) = self.role.take() {
            role.destroy();
            self.synced = false;
            self.clock.unmapped();
            if let Some(buffers) = self.shm.as_mut() {
                if !self.want_shown {
                    // A hidden surface shows nothing: attaching no buffer lets the compositor
                    // release the last one, whose release then frees it (`Swapchain::trim`).
                    self.surface.attach(None, 0, 0);
                    self.surface.commit();
                }
                buffers.trim();
            }
        }
    }

    /// Sets the surface's input region; `compositor` makes the region object.
    pub fn set_input_region(
        &mut self,
        host_compositor: &wayland_client::protocol::wl_compositor::WlCompositor,
        input: &InputRegion,
    ) {
        let region = match input {
            InputRegion::Whole => None,
            InputRegion::Rects(rects) => {
                let region = host_compositor.create_region(&self.env.qh, ());
                for r in rects {
                    region.add(r.x, r.y, r.width as i32, r.height as i32);
                }
                Some(region)
            }
        };
        self.surface.set_input_region(region.as_ref());
        if let Some(region) = region {
            region.destroy();
        }
        self.input = input.clone();
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        // Everything a presenter made from the lease is gone by now (see the module docs).
        self.shm = None;
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
        if let Some(role) = self.role.take() {
            role.destroy();
        }
        self.surface.destroy();
    }
}

/// A host-managed destination a trusted presenter draws into: one engine view's page surface,
/// or the chrome's. Dropping it (or [`retire`](Self::retire)) gives the target back.
///
/// Not `Send`: a lease stays on the thread that owns the host.
pub struct TargetLease {
    target: Rc<RefCell<Target>>,
    id: TargetId,
    // Keeps libwayland's wl_display alive for as long as a presenter may use the handles.
    connection: Connection,
}

impl TargetLease {
    pub(crate) fn new(target: Rc<RefCell<Target>>, connection: Connection) -> Self {
        let id = target.borrow().id;
        Self {
            target,
            id,
            connection,
        }
    }

    /// The target's stable identity.
    pub fn id(&self) -> TargetId {
        self.id
    }

    /// The view the target was created for, if any.
    pub fn view(&self) -> Option<ViewId> {
        self.target.borrow().view
    }

    /// How frames reach the compositor.
    pub fn mode(&self) -> PresentMode {
        self.target.borrow().mode
    }

    /// The current metrics. Empty until the first placement that gives the target room.
    pub fn metrics(&self) -> Metrics {
        self.target.borrow().metrics
    }

    /// The Wayland display and the target's `wl_surface`, as raw-window-handle handles, for a
    /// GL presenter to create its EGL window surface on (Servo:
    /// `WindowRenderingContext::new(display, window, size)`). `None` on an SHM target.
    ///
    /// The presenter owns buffer submission on this surface from then on: its swaps attach and
    /// commit it, and the host never does. It must present only with a [`FramePermit`], at the
    /// permit's [`Metrics::buffer`] size, and should use swap interval 0: the host paces frames
    /// with its own frame callbacks, and a driver waiting for the callback of a surface the host
    /// unmapped would wait forever.
    ///
    /// # Safety
    ///
    /// The handles are valid for the borrow of this lease, but what a presenter creates from
    /// them (an `EGLSurface`, a `wl_egl_window`, a surfman widget) keeps using the surface and
    /// display after the borrow ends. The caller promises to destroy all of it, and terminate
    /// any EGL display it opened on this connection, before this lease is dropped. The lease
    /// keeps the surface and the connection alive until then.
    pub unsafe fn gl_handles(&self) -> Option<GlHandles<'_>> {
        let target = self.target.borrow();
        if target.mode != PresentMode::Gl {
            return None;
        }
        let display = NonNull::new(self.connection.backend().display_ptr().cast::<c_void>())?;
        let surface = NonNull::new(target.surface.id().as_ptr().cast::<c_void>())?;
        Some(GlHandles {
            display,
            surface,
            _lease: PhantomData,
        })
    }

    /// Gives the target back. The same as dropping the lease; see the module documentation for
    /// what must happen first on a GL target.
    pub fn retire(self) {}
}

impl Drop for TargetLease {
    fn drop(&mut self) {
        self.target.borrow_mut().leased = false;
    }
}

impl fmt::Debug for TargetLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let target = self.target.borrow();
        f.debug_struct("TargetLease")
            .field("id", &self.id)
            .field("mode", &target.mode)
            .field("metrics", &target.metrics)
            .finish()
    }
}

/// Raw Wayland handles of a GL target, borrowed from its lease. See
/// [`TargetLease::gl_handles`].
#[derive(Clone, Copy, Debug)]
pub struct GlHandles<'a> {
    display: NonNull<c_void>,
    surface: NonNull<c_void>,
    _lease: PhantomData<&'a TargetLease>,
}

impl HasDisplayHandle for GlHandles<'_> {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        let raw = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(self.display));
        // SAFETY: the lease this borrows keeps the connection, and so the wl_display, alive.
        Ok(unsafe { DisplayHandle::borrow_raw(raw) })
    }
}

impl HasWindowHandle for GlHandles<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let raw = RawWindowHandle::Wayland(WaylandWindowHandle::new(self.surface));
        // SAFETY: the lease this borrows keeps the wl_surface alive.
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

/// Why a permit could not be used.
#[derive(Debug)]
pub enum PresentError {
    /// The target presents the other way ([`PresentMode`]).
    WrongMode,
    /// Shared memory for a buffer could not be allocated.
    Buffer(std::io::Error),
}

impl fmt::Display for PresentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongMode => f.write_str("the target presents in another mode"),
            Self::Buffer(error) => write!(f, "no buffer: {error}"),
        }
    }
}

impl std::error::Error for PresentError {}

/// Permission to present one frame on one target, at one layout epoch. From
/// [`Host::permits`].
///
/// Consumed by one presentation attempt: an SHM presenter turns it into a buffer with
/// [`shm_frame`](Self::shm_frame); a GL presenter renders at [`metrics`](Self::metrics), swaps,
/// and reports with [`finish`](Self::finish). Dropping it unused counts as
/// [`PresentOutcome::NotReady`]. Not `Clone`.
pub struct FramePermit {
    target: Rc<RefCell<Target>>,
    id: TargetId,
    view: Option<ViewId>,
    metrics: Metrics,
    outcome: Option<PresentOutcome>,
}

impl FramePermit {
    /// The target to present on.
    pub fn target(&self) -> TargetId {
        self.id
    }

    /// The view the target belongs to. An engine checks it is the view it is presenting.
    pub fn view(&self) -> Option<ViewId> {
        self.view
    }

    /// The geometry to render for: size, scale and layout epoch.
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }

    /// Whether the permit is still for the target's current layout: false once the target was
    /// resized, rescaled, hidden or retired. A frame rendered for a stale permit must not be
    /// shown as current. A frame for a current permit is shown however late it comes, even after
    /// its layout's grace period ([`crate::placement`]).
    pub fn is_current(&self) -> bool {
        let target = self.target.borrow();
        target.leased && target.want_shown && target.metrics.epoch() == self.metrics.epoch()
    }

    /// A buffer of the permit's size to paint into, on an SHM target.
    pub fn shm_frame(self) -> Result<ShmFrame, PresentError> {
        let size = self.metrics.buffer();
        let mut guard = self.target.borrow_mut();
        let target = &mut *guard;
        let (id, env) = (target.id, Rc::clone(&target.env));
        let Some(buffers) = target.shm.as_mut() else {
            drop(guard);
            return Err(PresentError::WrongMode);
        };
        let bytes = crate::shm::layout(size).map_or(0, |(.., bytes)| bytes);
        let acquired = buffers.acquire(size, bytes, |key| {
            let data = BufferData {
                owner: BufferOwner::Target(id),
                key,
            };
            ShmBuffer::new(&env.shm, &env.qh, size, data)
        });
        drop(guard);
        match acquired {
            Ok((key, buffer, age)) => Ok(ShmFrame::new(self, key, buffer, size, age)),
            // `permits` checked availability, so only an allocation can fail here; keep the
            // invalidation for other outcomes anyway.
            Err(AcquireError::Alloc(error)) => Err(PresentError::Buffer(error)),
            Err(AcquireError::InUse | AcquireError::Retiring) => {
                self.target.borrow_mut().clock.invalidate();
                Err(PresentError::Buffer(std::io::Error::other(
                    "every buffer is in use",
                )))
            }
        }
    }

    /// Reports what a GL presenter did with the permit. `Submitted` means it swapped a frame
    /// rendered at [`metrics`](Self::metrics); an SHM presenter submits with
    /// [`ShmFrame::submit`] instead.
    pub fn finish(mut self, outcome: PresentOutcome) {
        debug_assert!(
            outcome != PresentOutcome::Submitted || self.target.borrow().mode == PresentMode::Gl,
            "an SHM frame is submitted through ShmFrame::submit"
        );
        self.outcome = Some(outcome);
    }

    pub(crate) fn target_mut(&self) -> RefMut<'_, Target> {
        self.target.borrow_mut()
    }

    pub(crate) fn set_outcome(&mut self, outcome: PresentOutcome) {
        self.outcome = Some(outcome);
    }

    /// Attaches, damages and commits SHM buffer `key` at the permit's metrics, and gives it back
    /// to the target's buffers as submitted. `None` or an empty `damage` damages the whole buffer.
    pub(crate) fn submit_shm(
        &mut self,
        key: BufferKey,
        buffer: ShmBuffer,
        damage: Option<Vec<crate::damage::DamageRect>>,
    ) {
        let metrics = self.metrics;
        let mut target = self.target.borrow_mut();
        let target = &mut *target;
        let size = metrics.buffer();
        crate::scale::apply(
            &target.surface,
            target.viewport.as_ref(),
            metrics.scale(),
            metrics.rect().size(),
        );
        target.surface.attach(Some(buffer.wl_buffer()), 0, 0);
        let rects = clamp_damage(damage, size);
        if rects.is_empty() {
            target
                .surface
                .damage_buffer(0, 0, size.width as i32, size.height as i32);
        } else {
            for rect in rects {
                target
                    .surface
                    .damage_buffer(rect.x, rect.y, rect.width, rect.height);
            }
        }
        if let Some(seq) = target.clock.request_callback() {
            target.surface.frame(
                &target.env.qh,
                FrameData {
                    target: target.id,
                    seq,
                },
            );
        }
        if let Some(presentation) = &target.env.presentation {
            presentation.feedback(
                &target.surface,
                &target.env.qh,
                FeedbackData {
                    target: target.id,
                    submitted_ns: crate::scheduler::monotonic_ns(),
                },
            );
        }
        target.surface.commit();
        if let Some(buffers) = target.shm.as_mut() {
            buffers.submit(key, buffer);
        }
        self.outcome = Some(PresentOutcome::Submitted);
    }
}

/// `damage` clipped to `size`. Empty means the caller should damage the whole buffer.
fn clamp_damage(
    damage: Option<Vec<crate::damage::DamageRect>>,
    size: sighurt_api::geometry::BufferSize,
) -> Vec<crate::damage::DamageRect> {
    let Some(rects) = damage else {
        return Vec::new();
    };
    let (width, height) = (size.width as i32, size.height as i32);
    let mut kept = Vec::new();
    for rect in rects {
        if rect.width <= 0 || rect.height <= 0 {
            continue;
        }
        let x0 = rect.x.max(0);
        let y0 = rect.y.max(0);
        let x1 = rect.x.saturating_add(rect.width).min(width);
        let y1 = rect.y.saturating_add(rect.height).min(height);
        if x1 > x0 && y1 > y0 {
            kept.push(crate::damage::DamageRect {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            });
        }
    }
    kept
}

impl Drop for FramePermit {
    fn drop(&mut self) {
        let outcome = self.outcome.unwrap_or(PresentOutcome::NotReady);
        let mut target = self.target.borrow_mut();
        target.clock.finished(outcome);
        if outcome == PresentOutcome::Submitted {
            target.stats.submissions += 1;
            target.presented = Some(self.metrics.epoch());
        }
    }
}

impl fmt::Debug for FramePermit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FramePermit")
            .field("target", &self.id)
            .field("view", &self.view)
            .field("metrics", &self.metrics)
            .finish()
    }
}

impl Host {
    /// Creates a target in `window`, presenting in `mode`, for `view` if it is an engine's.
    ///
    /// The target is unmapped and has empty metrics until a placement includes it; every later
    /// placement of the window must list it until the lease is dropped.
    pub fn create_target(
        &mut self,
        window: WindowId,
        mode: PresentMode,
        view: Option<ViewId>,
    ) -> Result<TargetLease, crate::HostError> {
        let w = self
            .windows
            .get_mut(&window)
            .ok_or(crate::HostError::UnknownWindow(window))?;
        let surface = self.compositor.create_surface(&self.qh);
        let viewport = self
            .globals
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, &self.qh, ()));
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            WindowRect::default(),
            w.scale.effective(viewport.is_some()),
        );
        let target = Target::new(
            window,
            view,
            mode,
            Rc::clone(&self.env),
            surface.clone(),
            viewport,
            metrics,
        );
        let id = target.id;
        w.targets.push(id);
        self.surfaces.insert(
            surface.id(),
            crate::connection::SurfaceOwner::Target(window, id),
        );
        let target = Rc::new(RefCell::new(target));
        self.targets.insert(id, Rc::clone(&target));
        Ok(TargetLease::new(target, self.conn.clone()))
    }

    /// Marks `target` as having something new to present. Invalidations coalesce; a permit
    /// follows from [`Host::permits`] once the target is shown and its presenter can take a
    /// frame.
    pub fn invalidate(&mut self, target: TargetId) {
        if let Some(t) = self.targets.get(&target) {
            t.borrow_mut().clock.invalidate();
        }
    }

    /// Permits for every target that may present now: shown, invalidated, not waiting for a
    /// frame callback, and (SHM) with a buffer to lend. Call it once per loop iteration, after
    /// handling events, and hand each permit to the target's presenter.
    pub fn permits(&mut self) -> Vec<FramePermit> {
        let mut permits = Vec::new();
        let ids: Vec<TargetId> = self.targets.keys().copied().collect();
        for id in ids {
            let target = Rc::clone(&self.targets[&id]);
            if !target.borrow().can_present() {
                continue;
            }
            if target.borrow().role.is_none() && !self.begin_map(&target) {
                continue;
            }
            let mut t = target.borrow_mut();
            t.clock.issue();
            t.stats.permits += 1;
            if t.mode == PresentMode::Gl {
                // Pending state the presenter's swap commits.
                if let Some(seq) = t.clock.request_callback() {
                    t.surface.frame(&self.qh, FrameData { target: id, seq });
                    if let Some(presentation) = &self.globals.presentation {
                        let feedback = FeedbackData {
                            target: id,
                            submitted_ns: crate::scheduler::monotonic_ns(),
                        };
                        presentation.feedback(&t.surface, &self.qh, feedback);
                    }
                }
            }
            let (view, metrics) = (t.view, t.metrics);
            drop(t);
            permits.push(FramePermit {
                target,
                id,
                view,
                metrics,
                outcome: None,
            });
        }
        permits
    }

    /// Forgets targets whose leases are gone. Their native objects are destroyed when the last
    /// permit referring to them is dropped too.
    pub(crate) fn release_retired_targets(&mut self) {
        let retired: Vec<TargetId> = self
            .targets
            .iter()
            .filter(|(_, t)| !t.borrow().leased)
            .map(|(&id, _)| id)
            .collect();
        for id in retired {
            let Some(target) = self.targets.remove(&id) else {
                continue;
            };
            let (window, surface) = {
                let mut t = target.borrow_mut();
                // Unmapped at once, independently of any layout transaction.
                t.unmap();
                (t.window, t.surface.id())
            };
            self.surfaces.remove(&surface);
            self.forget_surface(crate::connection::SurfaceOwner::Target(window, id));
            if let Some(w) = self.windows.get_mut(&window) {
                w.targets.retain(|&t| t != id);
                if w.focus == Some(id) {
                    w.focus = None;
                }
                if w.text_input.as_ref().is_some_and(|(t, _)| *t == id) {
                    w.text_input = None;
                }
            }
        }
    }
}
