//! The core's entry point: `BrowserState`, the `BrowserEvent`s it accepts and the `Effect`s it
//! returns from `update`.
//!
//! Events are user commands, chrome actions, engine events, service results and configuration
//! changes; effects are what the executable should do next (open or close a view, send an
//! engine command, show or hide a target, request a service, persist the session). `update`
//! applies one event and returns its effects without doing I/O, so every state change can be
//! replayed and tested.
//!
//! Must not own: I/O, timers, threads, surface metrics or anything the chrome draws. It is not a
//! framework: no event bus, no generic UI tree, no plugin registry.
