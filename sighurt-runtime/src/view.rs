//! Per-view bookkeeping for every live engine view.
//!
//! Owns the engine's name, the target's id, whether the core currently shows the view, and
//! when the view was last asked to navigate while its tab was the active one. The session
//! keeps one entry per view.
//!
//! Must not own: the target itself (the frontend), the engine runtime (the registry), or
//! browser policy (the core).

use std::time::Instant;

use sighurt_api::ids::TargetId;

/// A live engine view, as the runtime knows it.
pub struct ViewEntry {
    /// The engine's name in the configuration.
    pub engine: String,
    /// The frontend's target for the view.
    pub target: TargetId,
    /// Whether the core shows it (`SetVisibility`).
    pub visible: bool,
    /// When it was last asked to navigate while its tab was the active one, until its next
    /// frame or until another tab becomes active.
    pub navigated: Option<Instant>,
}
