//! The neutral contract between Sighurt's browser core, Wayland host, chrome and engines.
//!
//! Every crate of the browser depends on this one, and this one depends on nothing: no GPUI,
//! Wayland, Servo, Wasmtime or graphics-API types, and no `Any` escape hatch. What crosses a
//! boundary is plain data with typed identities:
//!
//! - [`ids`]: opaque identities for windows, tabs, views, navigations, documents, targets,
//!   layouts, requests and seats, from process-wide counters, so a late event can never match a
//!   newer object.
//! - [`geometry`]: window-logical, surface-local and buffer-pixel units, the rational scale and
//!   the metrics snapshot a host issues for each target.
//! - [`input`]: seat-aware pointer, keyboard, text-input and focus events.
//! - [`engine`]: the [`EngineRuntime`](engine::EngineRuntime) trait engines implement, what the
//!   host tells an engine, and the waker engines use to ask for attention.
//! - [`events`]: what an engine reports back.
//! - [`services`]: privileged requests (clipboard, downloads, file pickers, external URLs,
//!   permissions) and their answers.
//! - [`commands`]: the browser commands users bind keys to, such as `tab.new`.
//!
//! This is an internal contract rebuilt with the workspace, not a stable plugin ABI. The guest
//! SDK for Wasm apps is a separate compatibility promise. See `sighurt-rewrite-plan.md` §5–§7,
//! §11, §12 and §17 for the design this follows.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod commands;
pub mod engine;
pub mod events;
pub mod geometry;
pub mod ids;
pub mod input;
pub mod services;
