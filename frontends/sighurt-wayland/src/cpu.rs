//! The host's shared-memory [`TargetLease`] as a [`CpuTarget`].
//!
//! [`CpuTarget`] lives in [`sighurt_api::present`](sighurt_api::present): the one CPU frame
//! shape every frontend can show. A view presents into a [`TargetLease`] created with
//! [`PresentMode::Shm`](crate::PresentMode::Shm). Presenting turns the host's [`FramePermit`]
//! into the [`ShmFrame`] it lends, paints the frame at the permit's metrics straight into that
//! shared memory, and submits it; the host commits the leaf there and nowhere else. The paint
//! is wrapped in [`crate::damage::begin_paint`] with the buffer's age, and whatever the painter
//! reported with [`crate::damage::report_damage`] is what the commit damages. No report damages
//! the whole buffer. A frame that fails to paint is not submitted: dropping the [`ShmFrame`]
//! gives its buffer back unused, and that buffer's next age is 0.
//!
//! Owns: the step from permit to painted, submitted buffer. Must not own: when a permit is
//! issued (the host's scheduler), buffer reuse (the host's swapchain) or the surface.

use sighurt_api::geometry::Metrics;
use sighurt_api::ids::LayoutEpoch;
use sighurt_api::present::{CpuTarget, Frame};

use crate::shm::ShmFrame;
use crate::targets::{FramePermit, TargetLease};

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
        crate::damage::begin_paint(frame.age());
        let painted = paint(Frame {
            pixels: frame.pixels(),
            size,
            stride,
        });
        let damage = crate::damage::end_paint();
        painted?;
        frame.submit_with_damage(damage);
        Ok(())
    }
}
