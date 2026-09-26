//! The Servo runtime: owns the `Servo` instance and its views, and implements `EngineRuntime`.
//!
//! Initializes Servo on the owner thread (with the preferences the old `sig-servo` chose, and
//! their reasons, from its README), hooks Servo's event-loop waker to the
//! `sighurt_api::engine::EngineWaker`, pumps Servo when woken, and hands the events its delegates
//! queued to the host after each call. A failed view is reported as `ViewFailed`; losing Servo's
//! backend is `RuntimeFailed`, and may need a browser restart: dropping and rebuilding the Rust
//! wrapper does not reset Servo's global state. Keeps one profile directory, written by this
//! runtime only. Shutdown closes every view, then Servo, before the host releases its surfaces.
//!
//! Must not own: process lifetime, logging or TLS setup (the executable).
