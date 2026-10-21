//! The page's CPU target: the pixels an engine submits and the layout epoch they belong to.
//!
//! Owns: one view's buffer, reused from frame to frame, the latest frame painted into it, and the
//! metrics of the page area. Must not own: when a frame may be shown, how it is encoded, or which
//! view is visible. The engine holds the [`TermTarget`]; the owner loop keeps a
//! [`TargetHandle`] to the same slot. Both live on the owner thread.

use std::cell::RefCell;
use std::rc::Rc;

use sighurt_api::geometry::{BufferSize, Metrics};
use sighurt_api::ids::LayoutEpoch;
use sighurt_api::present::{CpuTarget, Frame};

/// Permission to present one frame at one layout epoch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TermPermit {
    /// The layout the frame is for.
    pub epoch: LayoutEpoch,
    /// The buffer the frame must fill.
    pub size: BufferSize,
}

struct Slot {
    metrics: Metrics,
    /// ARGB8888, premultiplied, top row first, `4 * width` bytes a row. Kept between frames.
    pixels: Vec<u8>,
    /// The layout and size of the frame in `pixels`, once a submit finished painting it.
    frame: Option<(LayoutEpoch, BufferSize)>,
}

/// The owner loop's side of a view's [`TermTarget`].
pub struct TargetHandle {
    slot: Rc<RefCell<Slot>>,
}

impl TargetHandle {
    /// Replace the metrics. A new epoch makes the stored frame stale.
    pub fn set_metrics(&self, metrics: Metrics) {
        let mut slot = self.slot.borrow_mut();
        if slot.metrics.epoch() != metrics.epoch() {
            slot.frame = None;
        }
        slot.metrics = metrics;
    }

    /// Runs `f` on the stored frame if it was painted for `epoch`.
    pub fn with_frame<R>(&self, epoch: LayoutEpoch, f: impl FnOnce(&Frame<'_>) -> R) -> Option<R> {
        let mut slot = self.slot.borrow_mut();
        let (painted, size) = slot.frame?;
        if painted != epoch {
            return None;
        }
        let stride = 4 * usize::try_from(size.width).ok()?;
        let frame = Frame {
            pixels: &mut slot.pixels,
            size,
            stride,
        };
        Some(f(&frame))
    }

    /// Bytes the buffer holds.
    pub fn buffer_bytes(&self) -> usize {
        self.slot.borrow().pixels.len()
    }
}

/// The engine's CPU target for one view.
pub struct TermTarget {
    slot: Rc<RefCell<Slot>>,
}

impl TermTarget {
    /// A target whose [`CpuTarget::metrics`] are `metrics`, and the loop's handle to it.
    pub fn new(metrics: Metrics) -> (Self, TargetHandle) {
        let slot = Rc::new(RefCell::new(Slot {
            metrics,
            pixels: Vec::new(),
            frame: None,
        }));
        let handle = TargetHandle {
            slot: Rc::clone(&slot),
        };
        (Self { slot }, handle)
    }
}

impl CpuTarget for TermTarget {
    type Permit = TermPermit;

    fn metrics(&self) -> Metrics {
        self.slot.borrow().metrics
    }

    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch {
        permit.epoch
    }

    fn submit(
        &mut self,
        permit: Self::Permit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let mut slot = self.slot.borrow_mut();
        let size = slot.metrics.buffer();
        if permit.epoch != slot.metrics.epoch() {
            return Err("the permit is for a layout the page no longer has".into());
        }
        if permit.size != size {
            return Err("the permit's buffer is not the page's".into());
        }
        if size.width == 0 || size.height == 0 {
            return Err("the page has no pixels".into());
        }
        let stride = usize::try_from(size.width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or("the page is too wide to paint")?;
        let len = usize::try_from(size.height)
            .ok()
            .and_then(|height| stride.checked_mul(height))
            .ok_or("the page's buffer overflows")?;
        // Whatever was there is overwritten: no frame until this paint finishes.
        slot.frame = None;
        slot.pixels.resize(len, 0);
        paint(Frame {
            pixels: &mut slot.pixels,
            size,
            stride,
        })?;
        slot.frame = Some((permit.epoch, size));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_api::geometry::{Scale, WindowRect};

    fn metrics(width: u32, height: u32) -> Metrics {
        Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width,
                height,
            },
            Scale::ONE,
        )
    }

    fn permit(metrics: Metrics) -> TermPermit {
        TermPermit {
            epoch: metrics.epoch(),
            size: metrics.buffer(),
        }
    }

    #[test]
    fn a_submit_keeps_the_frame_until_the_layout_changes() {
        let first = metrics(2, 1);
        let (mut target, handle) = TermTarget::new(first);
        target
            .submit(permit(first), &mut |frame| {
                frame.pixels.fill(7);
                Ok(())
            })
            .expect("submit");
        let pixels =
            handle.with_frame(first.epoch(), |frame| (frame.pixels.to_vec(), frame.stride));
        assert_eq!(pixels, Some((vec![7; 8], 8)));

        let stale = TermPermit {
            epoch: LayoutEpoch::next(),
            size: first.buffer(),
        };
        let error = target.submit(stale, &mut |_| Ok(())).expect_err("stale");
        assert!(error.contains("layout"), "{error}");
        assert!(handle.with_frame(first.epoch(), |_| ()).is_some());

        let second = metrics(3, 1);
        handle.set_metrics(second);
        assert!(handle.with_frame(first.epoch(), |_| ()).is_none());
        assert!(handle.with_frame(second.epoch(), |_| ()).is_none());
    }

    #[test]
    fn a_failed_paint_leaves_no_frame() {
        let metrics = metrics(1, 1);
        let (mut target, handle) = TermTarget::new(metrics);
        target
            .submit(permit(metrics), &mut |_| Ok(()))
            .expect("submit");
        let error = target.submit(permit(metrics), &mut |_| Err("no".into()));
        assert!(error.is_err());
        assert!(handle.with_frame(metrics.epoch(), |_| ()).is_none());
    }
}
