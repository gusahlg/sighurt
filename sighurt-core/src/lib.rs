//! The browser's state and policy, as ordinary Rust.
//!
//! The core knows which tabs exist, which one is active, which engine views belong to each tab
//! and its history, which navigations are under way, and what each document may do. It changes
//! only in response to typed events and answers with effects, through one function:
//! [`update`]`(&mut BrowserState, BrowserEvent) -> Vec<Effect>` ([`state`]). Effects are data
//! the executable carries out: open or close a view, send an engine command, perform a service,
//! focus the address bar, save the session. The core performs no I/O and reads no clock.
//!
//! It does not own windows, surfaces, glyphs, buffers, GPU contexts or engine internals, and
//! depends only on `sighurt-api` and plain parsing crates: it builds and tests without GPUI,
//! Wayland, Servo, Wasmtime or a graphics driver. One source of truth per field: engines report
//! facts about their documents and history, the chrome owns its editing state, the host owns
//! surface metrics; the core owns tabs, their order, navigation coordination and policy.
//!
//! - [`state`]: [`BrowserState`], [`BrowserEvent`], [`Effect`] and [`update`].
//! - [`tabs`]: tabs, their views and what the chrome shows of them; limits on page data.
//! - [`navigation`]: navigations and cross-engine history segments.
//! - [`router`]: which engine opens a URL; turning typed text into a URL.
//! - [`commands`]: carrying out browser commands.
//! - [`keys`]: key bindings.
//! - [`permissions`]: what pages may ask for.
//! - [`config`]: the configuration format.
//! - [`session`]: the saved session.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod commands;
pub mod config;
pub mod keys;
pub mod navigation;
pub mod permissions;
pub mod router;
pub mod session;
pub mod state;
pub mod tabs;

pub use state::{update, BrowserEvent, BrowserState, Effect, Environment, Focus, OpenRequest};
