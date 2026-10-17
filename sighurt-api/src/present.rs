//! Presenting on the CPU: memory a frontend lends an engine for one frame.
//!
//! A view's target is whatever the frontend binds
//! [`EngineRuntime::Target`](crate::engine::EngineRuntime::Target) to. The Wayland frontend gives native targets to the
//! engines that present on the GPU (Servo's GL, WebKit's DMA-BUFs) and shared-memory targets to
//! those that paint on the CPU; the terminal frontend gives every engine a [`CpuTarget`], whose
//! frames it encodes for the terminal (TUI.md §2). A [`CpuTarget`] is the one CPU shape both use:
//! the Wayland host's shared-memory target lease implements it, as does the terminal's.
//!
//! The format is fixed so that no target converts: wl_shm's ARGB8888, premultiplied alpha,
//! little-endian (B, G, R, A in memory), rows top to bottom. An engine that renders another way
//! (a GL readback is RGBA, bottom row first) converts while it copies into the frame.
//!
//! Owns: the CPU frame's shape. Must not own: buffers, their lifetimes or how a frontend shows
//! them.

use crate::geometry::{BufferSize, Metrics};
use crate::ids::LayoutEpoch;

/// Memory to paint one frame into, lent by the target for the length of one
/// [`CpuTarget::submit`].
pub struct Frame<'a> {
    /// `size.height` rows of `stride` bytes. Each pixel is ARGB8888: premultiplied alpha,
    /// little-endian (B, G, R, A in memory). The first row is the top one.
    pub pixels: &'a mut [u8],
    /// The buffer's size in pixels: the size of the permit's metrics.
    pub size: BufferSize,
    /// Bytes per row, at least `4 * size.width`.
    pub stride: usize,
}

/// Where a view that paints on the CPU presents.
///
/// The runtime drops a view's target on the owner thread when the view closes or the runtime
/// shuts down.
pub trait CpuTarget {
    /// Permission to present one frame at one layout epoch.
    type Permit;

    /// The target's metrics when the view opens; later ones arrive as
    /// [`EngineCommand::SetMetrics`](crate::engine::EngineCommand::SetMetrics).
    fn metrics(&self) -> Metrics;

    /// The layout epoch `permit` allows a frame for.
    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch;

    /// Lends a free buffer the size of the permit's metrics to `paint`, then submits it,
    /// consuming the permit. If `paint` fails, nothing is submitted.
    fn submit(
        &mut self,
        permit: Self::Permit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String>;
}
