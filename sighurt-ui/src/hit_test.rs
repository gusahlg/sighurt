//! Which chrome element is under a point: maps positions from the layout's regions to hit
//! targets (tabs, buttons, the address field).
//!
//! Works in window-logical coordinates, converting the host's surface-local position once with
//! the chrome target's placement, so hit targets stay under what is drawn at every scale.
//!
//! Must not own: deciding what a hit does (see `chrome`).
