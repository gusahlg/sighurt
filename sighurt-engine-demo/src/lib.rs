//! A demo engine that exists to test the host, the chrome and the engine contract without
//! building Servo (`sig` with `--no-default-features --features engine-demo`).
//!
//! Implements `sighurt_api::engine::EngineRuntime` with CPU drawing into shared-memory buffers.
//! Its views show, depending on the URL they are given: a static pattern (nothing to do while
//! idle), an animation (paced by frame callbacks, never by a fixed timer), an intentionally slow
//! frame (to exercise the resize fallback and a busy engine), content that follows every
//! resize and scale change (to check buffer sizes and input coordinates), and a recoverable
//! error (a failed navigation, or a failed view that can be reloaded).
//!
//! Must not own: anything a real engine would not; it goes through the same contract.
