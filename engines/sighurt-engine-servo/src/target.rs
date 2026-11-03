//! Where a Servo view presents: a Wayland subsurface, or one offscreen framebuffer.
//!
//! [`ServoTarget`] is sealed. Only [`TargetLease`] and [`Offscreen`] implement it, so a view's
//! pixels leave through one of those two and nowhere else. The Wayland impl swaps GL into the
//! subsurface and reads nothing back. The offscreen impl reads one frame into the target's lent
//! buffer. The framebuffer type and the GL calls live on crate-private traits (`Present`,
//! `Deliver`); this module must not make them public. It must not own Servo, the views'
//! documents, or the buffers the target lends.

use std::rc::Rc;
use std::time::{Duration, Instant};

use sighurt_api::engine::PresentOutcome;
use sighurt_api::geometry::{BufferSize, Metrics};
use sighurt_api::ids::{LayoutEpoch, ViewId};
use sighurt_api::present::CpuTarget;
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};

use crate::offscreen::OffscreenContext;
use crate::presentation::{FrameDriver, PageContext, PageSurface, Readbacks};

mod seal {
    pub trait Sealed {}
}

/// GL surface operations of a [`ServoTarget`]. Crate-private: the surface types are not part of
/// the public API.
pub(crate) trait Present: seal::Sealed + 'static {
    /// The GL surface this target's views render into.
    type Surface: PageSurface + 'static;

    /// Makes the view's GL context at `size`. The Wayland context uses the lease's handles.
    /// The offscreen context is surfaceless.
    fn create_context(
        &self,
        size: BufferSize,
        driver: Rc<FrameDriver>,
        readbacks: &Readbacks,
    ) -> Result<Self::Surface, String>;

    /// The view was hidden. A Wayland target gives its window surface back; an offscreen
    /// framebuffer stays.
    fn hide(&self, surface: &Self::Surface);

    /// The view was shown again, at `size`.
    fn show(&self, surface: &Self::Surface, size: BufferSize) -> Result<(), String>;
}

/// Hands a painted frame to the target. Crate-private, and generic over the permit so the
/// public [`ServoTarget`] can name its [`ServoTarget::Permit`] without exposing the surface.
pub(crate) trait Deliver<P>: Present {
    /// Shows the frame that was just painted. A Wayland target swaps. An offscreen target reads
    /// the framebuffer into the lent frame and submits it. The duration is the swap, or the
    /// readback, and is what the timing line calls `swap_ms`.
    fn deliver(&mut self, surface: &Self::Surface, permit: P) -> Result<Duration, String>;
}

impl seal::Sealed for TargetLease {}

impl Present for TargetLease {
    type Surface = PageContext;

    fn create_context(
        &self,
        size: BufferSize,
        driver: Rc<FrameDriver>,
        readbacks: &Readbacks,
    ) -> Result<Self::Surface, String> {
        // SAFETY: everything made from these handles (the EGL window surface and
        // `wl_egl_window` inside the context) is destroyed before the lease is dropped:
        // the runtime drops the WebView and the context first, and keeps the lease while
        // anything still holds the context. The EGL display is terminated at shutdown, after
        // every context and Servo, while the host (and so the connection) still exists; it is
        // shared by every view's context and can't be terminated per lease (see `presentation`).
        let handles = unsafe { self.gl_handles() }.ok_or("the target isn't a GL target")?;
        PageContext::new(handles, size, driver, readbacks)
    }

    fn hide(&self, surface: &Self::Surface) {
        surface.release_surface();
    }

    fn show(&self, surface: &Self::Surface, size: BufferSize) -> Result<(), String> {
        // SAFETY: as in `create_context`; the new surface belongs to the same context, and the
        // lease still outlives it.
        let handles = unsafe { self.gl_handles() }.ok_or("the target isn't a GL target")?;
        surface.bind_surface(handles, size)
    }
}

impl Deliver<FramePermit> for TargetLease {
    fn deliver(
        &mut self,
        surface: &Self::Surface,
        permit: FramePermit,
    ) -> Result<Duration, String> {
        let start = Instant::now();
        surface.swap();
        let swap = start.elapsed();
        permit.finish(PresentOutcome::Submitted);
        Ok(swap)
    }
}

/// A CPU target Servo presents into by reading one offscreen frame back.
///
/// `C` lends the frame for the length of [`CpuTarget::submit`]. This wrapper does not keep the
/// buffer. The Wayland GUI does not use it: its runtime is `ServoRuntime<TargetLease>`, which
/// swaps GL and reads nothing back.
pub struct Offscreen<C>(pub C);

impl<C: CpuTarget + 'static> seal::Sealed for Offscreen<C> {}

impl<C: CpuTarget + 'static> Present for Offscreen<C> {
    type Surface = OffscreenContext;

    fn create_context(
        &self,
        size: BufferSize,
        driver: Rc<FrameDriver>,
        readbacks: &Readbacks,
    ) -> Result<Self::Surface, String> {
        OffscreenContext::open(size, driver, readbacks)
    }

    fn hide(&self, _surface: &Self::Surface) {}

    fn show(&self, _surface: &Self::Surface, _size: BufferSize) -> Result<(), String> {
        Ok(())
    }
}

impl<C: CpuTarget + 'static> Deliver<C::Permit> for Offscreen<C> {
    fn deliver(&mut self, surface: &Self::Surface, permit: C::Permit) -> Result<Duration, String> {
        let start = Instant::now();
        self.0
            .submit(permit, &mut |frame| surface.read_argb(frame))?;
        Ok(start.elapsed())
    }
}

/// Sealed presentation target of a [`crate::ServoRuntime`].
///
/// Implemented only for the Wayland host's [`TargetLease`] (GL straight into the subsurface,
/// no readback) and for [`Offscreen`] (one surfaceless framebuffer per view, one readback per
/// submitted frame into the [`CpuTarget`]). The GL surface stays inside this crate.
#[allow(private_bounds)]
pub trait ServoTarget: seal::Sealed + Present + Deliver<Self::Permit> {
    /// Permission to present one frame. A Wayland permit is the host's [`FramePermit`]; an
    /// offscreen permit is the [`CpuTarget`]'s.
    type Permit;

    /// Metrics when the view opens. Later ones arrive as `SetMetrics`.
    fn metrics(&self) -> Metrics;

    /// Why this target cannot be used, or `None` when it can.
    fn unsupported(&self) -> Option<&'static str>;

    /// The layout epoch `permit` allows a frame for.
    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch;

    /// Whether `permit` is for `view`. An offscreen permit is not tagged with a view.
    fn permit_is_for(permit: &Self::Permit, view: ViewId) -> bool;

    /// Whether `permit` is still the view's current layout. `view_epoch` is the view's metrics.
    fn permit_is_current(permit: &Self::Permit, view_epoch: LayoutEpoch) -> bool;

    /// The buffer size to paint. A Wayland permit carries the host's buffer; an offscreen view
    /// paints at its own metrics.
    fn permit_buffer(permit: &Self::Permit, view_buffer: BufferSize) -> BufferSize;

    /// The present did not submit a frame. A Wayland permit reports `outcome`; an offscreen
    /// permit is dropped, so nothing is submitted.
    fn dismiss(permit: Self::Permit, outcome: PresentOutcome);
}

impl ServoTarget for TargetLease {
    type Permit = FramePermit;

    fn metrics(&self) -> Metrics {
        TargetLease::metrics(self)
    }

    fn unsupported(&self) -> Option<&'static str> {
        match self.mode() {
            PresentMode::Native => None,
            PresentMode::Shm => Some("presenting into a shared-memory target"),
        }
    }

    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch {
        permit.metrics().epoch()
    }

    fn permit_is_for(permit: &Self::Permit, view: ViewId) -> bool {
        permit.view() == Some(view)
    }

    fn permit_is_current(permit: &Self::Permit, _view_epoch: LayoutEpoch) -> bool {
        permit.is_current()
    }

    fn permit_buffer(permit: &Self::Permit, _view_buffer: BufferSize) -> BufferSize {
        permit.metrics().buffer()
    }

    fn dismiss(permit: Self::Permit, outcome: PresentOutcome) {
        permit.finish(outcome);
    }
}

impl<C: CpuTarget + 'static> ServoTarget for Offscreen<C> {
    type Permit = C::Permit;

    fn metrics(&self) -> Metrics {
        self.0.metrics()
    }

    fn unsupported(&self) -> Option<&'static str> {
        None
    }

    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch {
        C::permit_epoch(permit)
    }

    fn permit_is_for(_permit: &Self::Permit, _view: ViewId) -> bool {
        true
    }

    fn permit_is_current(permit: &Self::Permit, view_epoch: LayoutEpoch) -> bool {
        C::permit_epoch(permit) == view_epoch
    }

    fn permit_buffer(_permit: &Self::Permit, view_buffer: BufferSize) -> BufferSize {
        view_buffer
    }

    fn dismiss(_permit: Self::Permit, _outcome: PresentOutcome) {}
}
