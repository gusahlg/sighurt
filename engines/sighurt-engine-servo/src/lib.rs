//! The Servo engine: HTML, CSS and JavaScript, compiled into `sig` and run in its process.
//!
//! [`ServoRuntime`] implements `sighurt_api::engine::EngineRuntime`. The default target is the
//! Wayland host's lease: one WebView per live view, each on its own GL context, presenting
//! straight into the view's subsurface with no page pixels read back. [`Offscreen`] is the
//! other target, any [`CpuTarget`](sighurt_api::present::CpuTarget), for a frontend with no
//! display connection: each view has its own surfaceless framebuffer and one readback per
//! submitted frame. One Servo runtime serves the whole process (Servo can be initialized only
//! once). Its API calls and GL presentation stay on the owner thread (Servo and its rendering
//! contexts are not `Send`); that does not mean Servo runs script, layout and networking on
//! one thread.
//!
//! Pinned at Servo 0.7 with surfman 0.14: read the 0.7 sources, not a newer upstream.
//! `SERVO-0.7.md` records what that release changed for this adapter.
//!
//! A library, not a process: no `process::exit` or `_exit`, no shutdown watchdog, no panic
//! hook, logging setup or TLS provider installation here. The executable owns process-wide
//! setup: it installs its panic hook (which asks [`should_panic_hook_suppress_termination`])
//! and rustls's aws-lc-rs provider, builds the runtime at startup and calls
//! [`ServoRuntime::setup_logging`] (decision 26). `src/offscreen.rs` is adapted from the
//! offscreen experiment and keeps its MPL-2.0 notice.
//!
//! The previous out-of-process engine (`sig-servo`) is at git tag `gpui-baseline`.

pub mod delegate;
pub mod input;
pub mod navigation;
pub(crate) mod offscreen;
pub mod presentation;
pub mod runtime;
mod semantic;
pub(crate) mod target;
pub mod view;

pub use presentation::{GlInfo, OffscreenAdapter};
pub use runtime::{
    ServoConfig, ServoDiagnostics, ServoPreference, ServoRuntime, ServoTiming, CAPABILITIES,
};
/// Whether a panic is one Servo catches itself (image decoding), which the executable's panic
/// hook passes over.
pub use servo::should_panic_hook_suppress_termination;
pub use target::{Offscreen, ServoTarget};
