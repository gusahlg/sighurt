//! Sighurt's terminal frontend (docs/rewrite/TUI.md): the terminal `sig` runs in shows the
//! browser's chrome as lines of text and pages as kitty graphics, or as text from their content.
//!
//! Owns: the terminal's owner loop and the [`sighurt_runtime::frontend::Frontend`] it gives the
//! session ([`app`]), terminal I/O, capability detection, the cell grid and its writer, the
//! terminal chrome, `sig --dump` (`dump`), the page's CPU target and its graphics encoding, key and
//! mouse mapping, and
//! binding the compiled-in engines to that target ([`engines`]). Must not own: browser policy
//! (the core), which effect runs (the runtime), engine internals, or anything of Wayland or
//! sighurt-ui.

mod app;
pub mod chrome;
mod dump;
mod engines;
pub mod grid;
pub mod input;
pub mod kitty;
pub mod sanitize;
pub mod target;
pub mod term;
pub mod text;
pub mod textmode;

pub use app::{run, Options};
pub use dump::{run as dump, DumpOptions};
