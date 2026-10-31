//! NetSurf 3.11 as a Sighurt content engine.
//!
//! One runtime per process. It paints on the CPU into a [`CpuTarget`](sighurt_api::present::CpuTarget).
//! The C frontend under `csrc/` is the only code that calls NetSurf, and Rust calls that frontend
//! only through [`ffi`]. The engine reaches the browser through `EngineEvent`s and service
//! requests, the same way the other engines do.
//!
//! Building `sig` with `engine-netsurf` is for personal use. The crate is GPL-2.0-only and must
//! not be distributed combined with Sighurt. See the crate's README.
//!
//! `netsurf_init` ignores `SIGPIPE`, because OpenSSL can raise it and the default action would
//! kill `sig`. It does not call `setlocale`: this process is shared with the other engines.

#![warn(missing_docs)]

#[doc(hidden)]
pub mod ffi;

/// The Wayland host's shared-memory target. The runtime paints through it.
pub use sighurt_wayland::TargetLease;

/// The engine's name in the configuration and the registry.
pub const NAME: &str = "netsurf";
