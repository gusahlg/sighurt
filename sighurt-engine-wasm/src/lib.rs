//! # sighurt-engine-wasm — the WASM/binary content engine
//!
//! Runs Oxide-format `.wasm` apps: modules that export `start_app()` (and optionally
//! `on_frame`, `on_timer`, `on_event`) and import host capabilities from the `"oxide"` import
//! module. The import module keeps its original name so every app built for Oxide keeps working.
//!
//! Guests run under Wasmtime with fuel metering, bounded memory and no WASI: they can only reach
//! the capabilities registered in [`capabilities`].
//!
//! The engine is its own process, `sig-wasm` (`src/main.rs`), started once per page. It speaks
//! the [`sighurt_ipc`] protocol: the browser sends it navigation and input, and it answers with
//! draw lists for the browser to paint. This library holds the sandbox and host API that process
//! is built from; the browser never links it.

pub mod audio;
pub mod audio_format;
pub mod capabilities;
pub mod compression;
pub mod download;
pub mod engine;
pub mod events;
pub mod fetch;
pub mod file_picker;
pub mod kv;
pub mod manifest;
pub mod media_capture;
pub mod midi;
pub mod navigation;
pub mod permissions;
pub mod rtc;
pub mod runtime;
pub mod sse;
pub mod subtitle;
pub mod system;
pub mod text;
pub mod url;
pub mod video;
pub mod video_format;
pub mod websocket;
pub mod worker;
