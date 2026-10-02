//! WPE WebKit as an in-process Sighurt engine: the prototype of `sighurt-engine-wpe`
//! (docs/rewrite/WPE.md).
//!
//! [`WpeRuntime`] implements `sighurt_api::engine::EngineRuntime` with `sighurt_wayland`'s target
//! leases and frame permits, as `sig` binds every engine. It embeds WebKit through the WPE
//! Platform API: Sighurt's own `WPEDisplay` and `WPEView` ([`platform`]) receive WebKit's frames
//! as DMA-BUF or shared-memory buffers, which [`present`] attaches to the host's page subsurface;
//! WebKit's GLib main context runs inside the owner loop ([`glib_loop`]); input is translated in
//! [`input`].
//!
//! Must not own: browser policy, surfaces or the event loop; like every engine, it reports facts
//! as events and presents only with the host's permits.

pub mod ffi;
pub mod glib_loop;
pub mod input;
pub mod platform;
pub mod present;
pub mod runtime;

pub use runtime::{Diagnostics, Transport, WpeConfig, WpeRuntime};
