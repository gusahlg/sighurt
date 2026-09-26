//! The application owner loop, built on calloop, and the wiring of core, host, chrome and
//! engines.
//!
//! Its sources are Wayland events, engine wakers, worker and service completions, and timers
//! with real work. One iteration: collect ready input and completions (bounded, so a busy guest
//! cannot starve the keyboard); apply them to the core; deliver engine commands and pump the
//! runtimes that asked or were called this iteration; queue the resulting engine events and
//! apply them after the engine calls return; recompute only invalidated chrome and layout;
//! present and apply layout transactions where allowed; flush Wayland and sleep until an event
//! or a real deadline. Replaceable events (pointer motion, invalidations) coalesce; ordered
//! ones (keys, navigation commits) don't. Nothing ticks at a fixed rate, and blocking disk,
//! compilation and service work stays off this thread. Shutdown stops new work, saves the
//! session, calls `EngineRuntime::shutdown` on every runtime, and only then releases platform
//! objects.
//!
//! Must not own: browser policy (the core) or drawing (the chrome and engines).
