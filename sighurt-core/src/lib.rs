//! The browser's state and policy, as ordinary Rust.
//!
//! The core knows which windows and tabs exist, which tab is active, which engine view belongs
//! to each tab, which navigations are under way, and what each document may do. It changes only
//! in response to typed events and answers with effects, roughly
//! `fn update(state: &mut BrowserState, event: BrowserEvent) -> Vec<Effect>`: open a view, send
//! an engine command, ask for a service, write a session snapshot. It performs no I/O itself;
//! the executable carries the effects out.
//!
//! It does not own windows, surfaces, glyphs, buffers, GPU contexts or engine internals, and
//! depends only on `sighurt-api`: it must compile and test without GPUI, Wayland, Servo,
//! Wasmtime or a graphics driver. One source of truth per field: engines report facts about
//! their documents and history, the chrome owns its editing state, the host owns surface
//! metrics; the core owns tabs, their order, navigation coordination and policy.

pub mod commands;
pub mod config;
pub mod navigation;
pub mod permissions;
pub mod router;
pub mod session;
pub mod state;
pub mod tabs;
