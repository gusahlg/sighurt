//! The WPE WebKit engine: web pages with WebKit, for compatibility while Servo matures (Servo stays
//! the long-term engine). Compiled into `sig` with the feature `engine-wpe`.
//!
//! [`WpeRuntime`] implements `sighurt_api::engine::EngineRuntime` with `sighurt_wayland`'s target
//! leases and frame permits. One runtime serves the process (WebKit's UI-process state is
//! global), with one `WebKitWebView` per view. WebKit's UI-process half runs here, on the owner
//! thread; pages run in WebKit's own web and network processes (WebKit's architecture, with its
//! bubblewrap sandbox for the web process), so a page crash is a view failure, not a browser
//! crash. That is still no sandbox for Sighurt's process: WebKit's UI-process code, GLib and the
//! presenter run in it (CLAUDE.md, "Security").
//!
//! It embeds WebKit through the WPE Platform API: Sighurt's own `WPEDisplay` and `WPEView`
//! ([`platform`]) receive WebKit's frames as DMA-BUF or shared-memory buffers. On a Wayland
//! target [`present`] attaches them to the host's page subsurface with no CPU copy (DMA-BUF) or
//! one counted copy (the shared-memory fallback). On [`Offscreen`] there is no Wayland
//! connection: WebKit's shared-memory frame is copied into the lent CPU frame, and acknowledged
//! only when that present holds a permit. WebKit's GLib main context runs inside the owner loop
//! ([`glib_loop`]); input is translated in [`input`] and [`ime`]; the clipboard is the browser's
//! ([`clipboard`]); navigations and history come from WebKit's back-forward list
//! ([`navigation`]); each view's WebKit objects and signals are in [`view`]. The executable needs
//! WPE WebKit 2.52 from `nix/build-wpewebkit.sh` (docs/rewrite/WPE.md).
//!
//! Must not own: browser policy, surfaces or the event loop; like every engine, it reports facts
//! as events, asks for privileged services, and presents only with the host's permits.

pub mod clipboard;
pub mod ffi;
pub mod glib_loop;
pub mod ime;
pub mod input;
pub mod navigation;
pub mod platform;
pub mod present;
pub mod runtime;
pub mod target;
pub mod view;

pub use runtime::{Diagnostics, Transport, WpeConfig, WpeRuntime, CAPABILITIES};
pub use target::{Offscreen, WpeTarget};
