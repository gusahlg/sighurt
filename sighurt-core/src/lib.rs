//! # sighurt-core — Sighurt without its chrome
//!
//! Sighurt is a minimal browser that shows what engines draw. Every engine is a separate
//! process speaking [`sighurt_ipc`]; this crate runs them and paints what they send
//! ([`page`]), and keeps the session around them: tabs and their history, routing URLs to
//! engines by the config, and downloads ([`browser`]). It draws no UI of its own. A UI crate
//! (the default is `sighurt-ui`) lays out the chrome around the pages, reads the session's
//! state and hands the user's commands to [`browser::Browser::run_command`].
//!
//! ```text
//!   UI crate (chrome) ── run_command ──> Browser (tabs, history, routing, downloads)
//!        │ places the active page's view        │ one Page per engine process
//!        └──────────────── Page ◀───────────────┘
//!                           ▲ sighurt-ipc on stdin/stdout: frames or draw lists
//!               engine processes (sig-wasm, sig-servo, ...)
//! ```

pub mod browser;
pub mod commands;
pub mod config;
pub mod download;
pub mod page;
pub mod router;
