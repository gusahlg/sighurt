//! # sighurt-core — Sighurt without any UI
//!
//! Sighurt is a minimal browser that shows what engines draw. Every engine is a separate
//! process speaking [`sighurt_ipc`]; this crate runs them and paints what they send ([`page`]),
//! routes URLs to engines by the config ([`router`], [`config`]), keeps the history of each
//! place pages are shown in, across engines ([`navigable`]), and saves what no engine shows
//! ([`download`]).
//!
//! It has no tabs, no URL bar, no key bindings and no pages of its own. A UI crate (the default
//! is `sighurt-ui`, which also builds `sig`) decides how many navigables there are and where
//! they go, draws the chrome around their pages, and turns what the user does into calls on
//! them.
//!
//! ```text
//!   UI crate (chrome, tabs, keys) ── open, traverse, reload ──> Navigable (history, routing)
//!        │ places the page's view                                  │ one Page per engine
//!        └──────────────────────────── Page ◀──────────────────────┘
//!                                        ▲ sighurt-ipc on stdin/stdout: frames or draw lists
//!                            engine processes (sig-wasm, sig-servo, ...)
//! ```

pub mod config;
pub mod download;
pub mod navigable;
pub mod page;
pub mod router;
