//! Where a WPE view presents: the host's Wayland subsurface, or one CPU frame.
//!
//! [`WpeTarget`] is sealed. Only [`TargetLease`] and [`Offscreen`] implement it, so a view's
//! pixels leave through one of those two and nowhere else. The Wayland impl attaches WebKit's
//! DMA-BUF or shared-memory buffer to the subsurface. The offscreen impl copies one
//! shared-memory frame into the [`CpuTarget`](sighurt_api::present::CpuTarget) the frontend
//! lent, and only when that present holds a permit. This module must not own WebKit, the
//! views' documents, or the buffers the target lends.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use sighurt_api::engine::{EngineError, PresentOutcome};
use sighurt_api::geometry::{BufferSize, Metrics};
use sighurt_api::ids::LayoutEpoch;
use sighurt_api::present::CpuTarget;
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};
use wayland_client::protocol::wl_surface::WlSurface;

use crate::platform::Buffer;
use crate::present::{self, ShmBuffers, Wayland};

mod seal {
    pub trait Sealed {}
}

/// The Wayland objects one view presents through. Dropped before the lease.
pub(crate) struct WaylandSlot {
    pub(crate) shm: ShmBuffers,
    pub(crate) surface: WlSurface,
}

/// How a target reaches a display. Crate-private: the Wayland types are not part of the public
/// API.
pub(crate) trait Host<P>: seal::Sealed {
    /// The host `wl_display`, or `None` when the target has no Wayland connection.
    fn wayland_display(&self) -> Result<Option<NonNull<c_void>>, EngineError>;

    /// The view's `wl_surface` proxy and its shared-memory buffers.
    ///
    /// # Safety
    ///
    /// The proxy is dropped before the lease (`close`). `wayland` is the presenter's queue on
    /// the same connection.
    fn open_slot(&self, wayland: &Wayland) -> Result<WaylandSlot, String>;

    /// Copies one shared-memory frame into the lent CPU buffer. A Wayland target does not.
    fn copy_shm(&mut self, permit: P, buffer: &Buffer) -> Result<Duration, String>;
}

/// Sealed presentation target of a [`crate::WpeRuntime`].
///
/// Implemented only for the Wayland host's [`TargetLease`] (DMA-BUF straight to the
/// subsurface, or one shared-memory copy into a `wl_shm` buffer) and for [`Offscreen`] (no
/// display connection: WebKit's shared-memory frame copied into the [`CpuTarget`]).
#[allow(private_bounds)]
pub trait WpeTarget: seal::Sealed + Host<Self::Permit> + 'static {
    /// Permission to present one frame. A Wayland permit is the host's [`FramePermit`]; an
    /// offscreen permit is the [`CpuTarget`]'s.
    type Permit;

    /// Metrics when the view opens. Later ones arrive as `SetMetrics`.
    fn metrics(&self) -> Metrics;

    /// Why this target cannot be used, or `None` when it can.
    fn unsupported(&self) -> Option<&'static str>;

    /// The layout epoch `permit` allows a frame for.
    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch;

    /// Whether `permit` is still the view's current layout. `view_epoch` is the view's metrics.
    /// A Wayland permit answers from the host and ignores `view_epoch`.
    fn permit_is_current(permit: &Self::Permit, view_epoch: LayoutEpoch) -> bool;

    /// The buffer size to present. A Wayland permit carries the host's buffer; an offscreen
    /// view presents at its own metrics.
    fn permit_buffer(permit: &Self::Permit, view_buffer: BufferSize) -> BufferSize;

    /// The present did not submit a frame. A Wayland permit reports `outcome`; an offscreen
    /// permit is dropped, so nothing is submitted.
    fn dismiss(permit: Self::Permit, outcome: PresentOutcome);

    /// Drops the target after the view's WebKit objects. A Wayland lease is retired.
    fn retire(self);
}

impl seal::Sealed for TargetLease {}

impl Host<FramePermit> for TargetLease {
    fn wayland_display(&self) -> Result<Option<NonNull<c_void>>, EngineError> {
        // SAFETY: the presenter's objects are destroyed before the lease (`close`, `shutdown`).
        let handles = unsafe { self.gl_handles() }.ok_or(EngineError::Unsupported(
            "WPE views present on native targets",
        ))?;
        match handles.display_handle().map(|handle| handle.as_raw()) {
            Ok(RawDisplayHandle::Wayland(handle)) => Ok(Some(handle.display)),
            _ => Err(EngineError::Runtime("no Wayland display".into())),
        }
    }

    fn open_slot(&self, wayland: &Wayland) -> Result<WaylandSlot, String> {
        // SAFETY: the proxy is dropped before the lease (`close`).
        let surface = match unsafe { self.gl_handles() }
            .map(|handles| handles.window_handle().map(|window| window.as_raw()))
        {
            Some(Ok(RawWindowHandle::Wayland(handle))) => unsafe {
                wayland.surface(handle.surface)
            },
            _ => Err("no wl_surface".into()),
        }?;
        Ok(WaylandSlot {
            shm: wayland.shm_buffers(),
            surface,
        })
    }

    fn copy_shm(&mut self, permit: FramePermit, _buffer: &Buffer) -> Result<Duration, String> {
        TargetLease::dismiss(permit, PresentOutcome::NotReady);
        Err("internal: a Wayland target does not copy into a CPU frame".into())
    }
}

impl WpeTarget for TargetLease {
    type Permit = FramePermit;

    fn metrics(&self) -> Metrics {
        TargetLease::metrics(self)
    }

    fn unsupported(&self) -> Option<&'static str> {
        match self.mode() {
            PresentMode::Gl => None,
            PresentMode::Shm => Some("WPE views present on native targets"),
        }
    }

    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch {
        permit.metrics().epoch()
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

    fn retire(self) {
        TargetLease::retire(self);
    }
}

/// A CPU target WebKit presents into by copying one shared-memory frame.
///
/// `C` lends the frame for the length of [`CpuTarget::submit`]. This wrapper does not keep the
/// buffer, and it opens no Wayland connection: the display is created with no DMA-BUF device,
/// so WebKit's frames are `WPEBufferSHM`. The Wayland GUI does not use it.
pub struct Offscreen<C>(pub C);

impl<C: CpuTarget + 'static> seal::Sealed for Offscreen<C> {}

impl<C: CpuTarget + 'static> Host<C::Permit> for Offscreen<C> {
    fn wayland_display(&self) -> Result<Option<NonNull<c_void>>, EngineError> {
        Ok(None)
    }

    fn open_slot(&self, _wayland: &Wayland) -> Result<WaylandSlot, String> {
        Err("a CPU target has no Wayland surface".into())
    }

    fn copy_shm(&mut self, permit: C::Permit, buffer: &Buffer) -> Result<Duration, String> {
        let start = Instant::now();
        self.0
            .submit(permit, &mut |frame| present::copy_argb(buffer, frame))?;
        Ok(start.elapsed())
    }
}

impl<C: CpuTarget + 'static> WpeTarget for Offscreen<C> {
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

    fn permit_is_current(permit: &Self::Permit, view_epoch: LayoutEpoch) -> bool {
        C::permit_epoch(permit) == view_epoch
    }

    fn permit_buffer(_permit: &Self::Permit, view_buffer: BufferSize) -> BufferSize {
        view_buffer
    }

    fn dismiss(_permit: Self::Permit, _outcome: PresentOutcome) {}

    fn retire(self) {}
}
