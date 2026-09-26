//! The Wayland host: the one connection to the compositor, and every surface Sighurt shows.
//!
//! The host creates the window and all native surfaces for chrome and page views, and owns
//! their roles, parent-child relationships, placement, stacking, scaling and destruction. It
//! normalizes input into `sighurt_api::input` events addressed to targets, presents CPU buffers
//! through shared memory, and schedules frames. Trusted engine adapters and the chrome get
//! lifetime-bound target leases, never a bare `wl_surface`, and never create unrelated
//! top-level windows. The desktop compositor combines the surfaces; Sighurt has no compositor
//! of its own.
//!
//! One window's surfaces: the root (`xdg_toplevel`, an opaque fallback background), a chrome
//! subsurface, one page subsurface per live view (mapped only while selected), and bounded
//! overlay subsurfaces while the chrome needs one.
//!
//! Must not own: tabs, navigation, commands, DOM layout, guest execution or any engine's
//! rendering. Native presenters cooperate with these rules; a raw Wayland pointer in the same
//! process is not a security boundary.

pub mod clipboard;
pub mod connection;
pub mod input;
pub mod placement;
pub mod scale;
pub mod scheduler;
pub mod shm;
pub mod targets;
pub mod text_input;
pub mod window;
