//! The Wayland host's shared-memory target as a [`CpuTarget`]: what `sig` gives each Wasm view.
//!
//! A view presents into a `sighurt_wayland` [`TargetLease`] created with `PresentMode::Shm`.
//! Presenting turns the host's [`FramePermit`] into the [`ShmFrame`] it lends, paints the frame
//! at the permit's metrics straight into that shared memory, and submits it; the host commits
//! the leaf there and nowhere else. A frame that fails to paint is not submitted: dropping the
//! [`ShmFrame`] gives its buffer back unused.
//!
//! Owns: the step from permit to painted, submitted buffer. Must not own: when a permit is
//! issued (the host's scheduler), buffer reuse (the host's swapchain) or the surface.

use sighurt_api::geometry::Metrics;
use sighurt_api::ids::LayoutEpoch;
use sighurt_wayland::{FramePermit, ShmFrame, TargetLease};

use crate::adapter::CpuTarget;
use crate::raster::Frame;

impl CpuTarget for TargetLease {
    type Permit = FramePermit;

    fn metrics(&self) -> Metrics {
        TargetLease::metrics(self)
    }

    fn permit_epoch(permit: &FramePermit) -> LayoutEpoch {
        permit.metrics().epoch()
    }

    fn submit(
        &mut self,
        permit: FramePermit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        if permit.target() != self.id() {
            return Err(format!(
                "a permit for {:?} presented on {:?}",
                permit.target(),
                self.id()
            ));
        }
        let mut frame: ShmFrame = permit.shm_frame().map_err(|e| e.to_string())?;
        let (size, stride) = (frame.size(), frame.stride());
        paint(Frame {
            pixels: frame.pixels(),
            size,
            stride,
        })?;
        frame.submit();
        Ok(())
    }
}
