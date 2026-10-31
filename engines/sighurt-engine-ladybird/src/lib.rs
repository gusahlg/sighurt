//! The Ladybird (LibWeb) engine: Ladybird's UI process in `sig`'s process, its pages in
//! Ladybird's own helper processes (WebContent, RequestServer, ImageDecoder, Compositor).
//!
//! Implements `sighurt_api::engine::EngineRuntime` ([`LadybirdRuntime`]) over a C ABI shim
//! (`shim/`, built by `nix/build-ladybird-shim.sh` against nixpkgs' Ladybird) that is opened with
//! `dlopen` when the first page opens ([`ffi`]). The shim runs Ladybird's event loop on the owner
//! thread, rebuilt over one epoll descriptor; a small thread of this crate polls that descriptor
//! and wakes the owner loop through the [`EngineWaker`](sighurt_api::engine::EngineWaker), so an
//! idle page costs no wakeups of its own. Frames are Ladybird's backing stores, copied once per
//! frame into the host's CPU target ([`CpuTarget`](sighurt_api::present::CpuTarget)).
//!
//! Design, decisions and limits: `docs/rewrite/LADYBIRD.md`.
//!
//! Must not own: tab or routing policy, the chrome, anything of the host's surfaces.

#![warn(missing_docs)]

#[allow(missing_docs)] // The C declarations, documented by the header.
pub mod ffi;
mod runtime;
mod waiter;

pub use runtime::{LadybirdConfig, LadybirdRuntime, CAPABILITIES, NAME};
