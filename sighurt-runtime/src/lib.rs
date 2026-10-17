//! The frontend-neutral machinery that connects a frontend to the core and the engines.
//!
//! Owns the engine registry and runtime lifecycle, carrying out the core's effects (except
//! what a frontend does), per-view bookkeeping, services, configuration and session files,
//! diagnostics, and the [`session::Session`] that runs events in, the core, effects out,
//! engine pumps, and events back. A frontend — the Wayland GUI, and later the terminal —
//! supplies targets, permits, the clipboard and the chrome through [`frontend::Frontend`].
//!
//! Must not own: windows, terminals, pixels, chrome layout, or a Wayland or chrome type. Those
//! reach this crate only as the `Target` and `FramePermit` type parameters of
//! [`registry::Registry`].

pub mod diagnostics;
pub mod frontend;
pub mod persistence;
pub mod registry;
pub mod services;
pub mod session;
pub mod view;

pub use session::exit_now;
