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
//!
//! The executable's side is small: [`Host`] (with [`Host::new`], windows, targets, placements,
//! permits and [`Host::next_event`]), [`TargetLease`] and [`FramePermit`] for presenters,
//! [`ShmFrame`] for CPU painters, and [`Placement`] for the chrome's layout. A shared-memory
//! [`TargetLease`] implements [`CpuTarget`](sighurt_api::present::CpuTarget) ([`cpu`]). The
//! module documentation of each part states its rules.

#![warn(missing_docs)]

pub mod clipboard;
pub mod connection;
pub mod cpu;
pub mod damage;
pub mod input;
pub mod placement;
pub mod scale;
pub mod scheduler;
pub mod shm;
pub mod targets;
pub mod text_input;
pub mod window;

pub use connection::{Capabilities, Diagnostics, Host, HostError, HostEvent, TargetDiagnostics};
pub use damage::{begin_paint, buffer_age, end_paint, report_damage, DamageRect};
pub use placement::{InputRegion, Placement, PlacementError, TargetPlacement, RESIZE_GRACE};
pub use scheduler::PresentStats;
pub use shm::{BufferStats, ShmFrame, BYTES_PER_PIXEL};
pub use targets::{FramePermit, GlHandles, PresentError, PresentMode, TargetLease};
pub use window::WindowOptions;
