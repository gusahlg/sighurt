//! The CEF engine: Chromium, through the Chromium Embedded Framework's C API, rendering
//! windowless into CPU frames. Compiled into `sig` with the feature `engine-cef` (opt-in:
//! the prefix is 2.8 GB; docs/rewrite/CEF.md).
//!
//! [`CefRuntime`] implements `sighurt_api::engine::EngineRuntime` for any
//! [`CpuTarget`](sighurt_api::present::CpuTarget): the Wayland host's shared-memory leases and the
//! terminal's frames alike. One runtime serves the process (`cef_initialize` runs once), with one
//! windowless browser per view.
//!
//! `libcef.so` is opened with `dlopen` when the runtime first starts ([`ffi::load`]), so a
//! session without CEF pages maps none of it. CEF's own UI thread runs its browser-process code
//! (`multi_threaded_message_loop`: the external message pump left the browser unfinished, see
//! CEF.md); every callback ([`handlers`]) only updates shared state, queues events and wakes the
//! owner loop, which drains them in `pump`. The engine never ticks. Chromium's renderer, GPU and
//! network processes are CEF's helper processes (`sig-cef-helper`, built by `nix/build-cef.sh`
//! beside `libcef.so`), which also carry Chromium's own sandbox; `sig` needs no CEF code of its
//! own. The browser-process half of Chromium runs inside `sig`: a native engine is cooperative,
//! not a sandbox for `sig` (CLAUDE.md, "Security").
//!
//! Frames come from `OnPaint` as BGRA with dirty rectangles: those are copied into a staging
//! buffer on CEF's thread (only what changed) and, at `present`, into the host's lease (only the
//! damage that buffer lacks). Chromium runs with headless ozone and SwiftShader, so it opens no
//! display connection and the runtime starts without one. The browser owns the clipboard: copy
//! and paste go through the services, never through Chromium. Dialogs, permission prompts and
//! menus are refused.
//!
//! Must not own: browser policy, surfaces or the event loop; like every engine it reports facts
//! as events, asks for privileged services and presents only with the host's permits.

#![warn(missing_docs)]
// The C API is raw pointers: each unsafe block says why it is sound.
#![allow(clippy::missing_safety_doc)]

pub mod actions;
pub mod capi;
pub mod edit;
pub mod ffi;
pub mod find;
pub mod handlers;
pub mod keys;
pub mod nav;
pub mod present;
mod runtime;
pub mod shared;

pub use runtime::CAPABILITIES;
pub use runtime::{CefConfig, CefRuntime, Diagnostics, NAME};
