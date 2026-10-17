//! # sighurt-engine-wasm — the Wasm engine
//!
//! Runs Oxide-format `.wasm` apps: modules that export `start_app()` (and optionally
//! `on_frame`, `on_timer`, `on_event`) and import host capabilities from the `"oxide"` import
//! module. The import module keeps its original name so every app built for Oxide keeps working.
//!
//! The engine is a library compiled into `sig` and run in its process. [`WasmRuntime`]
//! implements `sighurt_api`'s [`EngineRuntime`](sighurt_api::engine::EngineRuntime): one runtime
//! with one shared Wasmtime engine serves every Wasm view, each document in a store of its own.
//! Guests run on worker threads under fuel and time limits ([`engine`]) and reach only the
//! capabilities registered in [`capabilities`], with no WASI. Their drawing is painted on the
//! CPU (`raster.rs`) into a [`Frame`] the host lends through a [`CpuTarget`]. Both live in
//! [`sighurt_api::present`](sighurt_api::present) and are re-exported here. `sig` gives each
//! view a `sighurt_wayland::TargetLease` of an SHM target, which implements [`CpuTarget`] in
//! `sighurt-wayland`.
//!
//! Being in-process makes this cooperative, not a sandbox: Wasmtime isolates a guest's memory
//! and the linker limits what it can call, but a bug in the engine or a host function is a bug
//! in the browser.

pub mod adapter;
pub mod audio;
pub mod audio_format;
pub mod capabilities;
pub mod compression;
pub mod download;
mod draw;
pub mod engine;
pub mod events;
pub mod fetch;
pub mod file_picker;
mod keys;
pub mod kv;
mod live;
pub mod manifest;
pub mod media_capture;
pub mod midi;
pub mod navigation;
pub mod permissions;
mod pool;
mod raster;
pub mod rtc;
pub mod runtime;
pub mod sse;
pub mod subtitle;
pub mod system;
pub mod text;
pub mod url;
pub mod video;
pub mod video_format;
mod view;
pub mod websocket;
pub mod worker;

pub use adapter::WasmRuntime;
pub use engine::{default_aot_cache, SandboxPolicy, WasmConfig};
pub use sighurt_api::present::{CpuTarget, Frame};
