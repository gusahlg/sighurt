//! Terminal I/O for Sighurt's text frontend.
//!
//! The crate reads the terminal with its own parser ([`parser::Parser`]) on a
//! file descriptor the caller owns, writes a cell grid ([`grid::Grid`]) through
//! a bounded non-blocking buffer ([`output::Output`]), and restores the
//! terminal from [`session::Session`]. Comparison with `vte`, `termwiz` and
//! `crossterm` lives in the integration tests and in `RESULTS.md`.

#![allow(clippy::too_many_lines)]

pub mod grid;
pub mod keys;
pub mod modes;
pub mod output;
pub mod parser;
pub mod probe;
pub mod session;
pub mod source;

pub use grid::{Attrs, Cell, Color, Grid};
pub use keys::{ButtonState, Key, KeyCode, KeyEvent, Modifiers};
pub use modes::{ENTER, KITTY_KEYBOARD_FLAGS, LEAVE, QUERIES, SYNC_BEGIN, SYNC_END};
pub use output::Output;
pub use parser::{Input, MouseButton, MouseEvent, Parser, Reply, Wheel, ESC_TIMEOUT, SEQ_TIMEOUT};
pub use probe::{format_caps, Caps, XtGetTcap, PROBE_DEADLINE};
pub use session::{open_pty, prepare_forced_exit, termios_matches, Session};
pub use source::InputSource;
