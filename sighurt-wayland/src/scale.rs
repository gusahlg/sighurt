//! Per-surface scale: the compositor's preferred scale for each surface, and the metrics
//! snapshots derived from it.
//!
//! Prefers `wp_fractional_scale_v1` with `wp_viewporter`, keeping the scale as its numerator
//! over 120 (`sighurt_api::geometry::Scale`); falls back to integer buffer scale. Buffer sizes
//! follow the one rounding policy in `sighurt_api::geometry`. A scale change starts a layout
//! transaction even when the logical size is unchanged.
//!
//! Must not own: page zoom, which is an engine setting.
