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
mod input;
mod runtime;
mod watcher;

use sighurt_api::engine::{EngineCapabilities, SiteDataKinds};

pub use runtime::{NetSurfConfig, NetSurfRuntime};

/// The Wayland host's shared-memory target. The runtime paints through it.
pub use sighurt_wayland::TargetLease;

/// The engine's name in the configuration and the registry.
pub const NAME: &str = "netsurf";

/// What the engine supports: page zoom. `text_input` is IME composition, which it does not do (a
/// focused field is reported through `TextInputChanged`). No history of its own (the core gives
/// each page a view), no semantics, no find, no site data yet.
pub const CAPABILITIES: EngineCapabilities = EngineCapabilities {
    history_traversal: false,
    text_input: false,
    accessibility: false,
    screenshots: false,
    find_in_page: false,
    page_zoom: true,
    pointer_lock: false,
    native_popups: false,
    process_per_view: false,
    semantics: false,
    site_data: SiteDataKinds::NONE,
};
