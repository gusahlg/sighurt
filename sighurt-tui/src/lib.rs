//! Sighurt's terminal frontend (docs/rewrite/TUI.md): the terminal `sig` runs in shows the
//! browser's chrome as lines of text and pages as kitty graphics, or as text from their content.
//!
//! Owns: terminal I/O, capability detection, the cell grid and its writer, the terminal chrome,
//! graphics encoding, key and mouse mapping. Must not own: browser policy (the core), engines,
//! or anything of Wayland.

pub mod chrome;
pub mod grid;
pub mod input;
pub mod kitty;
pub mod sanitize;
pub mod term;
