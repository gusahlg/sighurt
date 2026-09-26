//! The engines compiled into this build, and their runtimes.
//!
//! Cargo features decide which engines exist (`engine-servo`, `engine-wasm`, `engine-demo`);
//! runtime configuration decides which of them may open new views. A runtime is created on
//! first use, one per engine kind, with the `EngineWaker` it needs, and its
//! `Target` and `FramePermit` bound to `sighurt_wayland`'s types.
//!
//! Must not own: routing policy (the core decides which engine opens a URL).
