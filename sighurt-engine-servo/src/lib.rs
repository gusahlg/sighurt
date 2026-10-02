//! The Servo engine: HTML, CSS and JavaScript, compiled into `sig` and run in its process.
//!
//! [`ServoRuntime`] implements `sighurt_api::engine::EngineRuntime` with the Wayland host's
//! target lease and frame permit. One Servo runtime serves the whole process, with one WebView
//! per live view, each on its own GL context made from its lease (Servo can be initialized only
//! once per process). Its API calls and GL presentation stay on the owner thread (Servo and its
//! rendering contexts are not `Send`); that does not mean Servo runs script, layout and
//! networking on one thread.
//!
//! Pinned at Servo 0.5 with surfman 0.13: read the 0.5 sources, not the current upstream
//! documentation. The normal display path presents GL straight into the view's subsurface; no
//! page pixels are read back to the CPU.
//!
//! A library, not a process: no `process::exit` or `_exit`, no shutdown watchdog, no panic
//! hook, logging setup or TLS provider installation here. The executable owns process-wide
//! setup: it installs its panic hook (which asks [`should_panic_hook_suppress_termination`])
//! and rustls's aws-lc-rs provider, builds the runtime at startup and calls
//! [`ServoRuntime::setup_logging`] (decision 26). Files adapted from Servo keep their MPL-2.0
//! notices; none are here now.
//!
//! The previous out-of-process engine (`sig-servo`) is at git tag `gpui-baseline`.

pub mod delegate;
pub mod input;
pub mod navigation;
pub mod presentation;
pub mod runtime;
pub mod view;

pub use presentation::GlInfo;
pub use runtime::{ServoConfig, ServoDiagnostics, ServoRuntime};
/// Whether a panic is one Servo catches itself (image decoding), which the executable's panic
/// hook passes over.
pub use servo::should_panic_hook_suppress_termination;
