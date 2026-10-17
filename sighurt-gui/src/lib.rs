//! The Wayland frontend: the owner loop, the chrome, and binding engines to the host's targets.
//!
//! Owns the calloop loop over [`sighurt_wayland::Host`], the chrome's surfaces and the snapshot
//! it is shown, presentation, the clipboard through the host, and the [`Frontend`] the runtime
//! drives. Engines are constructed here, bound to the host's target lease and frame permit;
//! the registry that holds them is the runtime's.
//!
//! Must not own: browser policy (the core) or which effect runs (the runtime's session).
//!
//! [`Frontend`]: sighurt_runtime::frontend::Frontend

mod app;
mod chrome;
mod engines;

pub use app::{run, Options};
