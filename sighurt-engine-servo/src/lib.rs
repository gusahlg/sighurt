//! The Servo engine: HTML, CSS and JavaScript, compiled into `sig` and run in its process.
//!
//! Implements `sighurt_api::engine::EngineRuntime` with the Wayland host's target lease and
//! frame permit. One Servo runtime serves the whole process, with one WebView per live view
//! (Servo can be initialized only once per process). Its API calls and GL presentation stay on
//! the owner thread (Servo and its rendering context
//! are not `Send`); that does not mean Servo runs script, layout and networking on one thread.
//!
//! Pinned at Servo 0.5 with surfman 0.13: read the 0.5 sources, not the current upstream
//! documentation. The normal display path presents GL straight into the view's subsurface; no
//! page pixels are read back to the CPU.
//!
//! A library, not a process: no `process::exit` or `_exit`, no shutdown watchdog, no panic
//! hook, logging setup or TLS provider installation here. The executable owns process-wide
//! setup. Files adapted from Servo keep their MPL-2.0 notices.
//!
//! The previous out-of-process engine (`sig-servo`) is at git tag `gpui-baseline`.

pub mod delegate;
pub mod input;
pub mod navigation;
pub mod presentation;
pub mod runtime;
pub mod view;
