//! Sighurt's terminal frontend (docs/rewrite/TUI.md): the terminal `sig` runs in shows the
//! browser's chrome as lines of text and pages as kitty or sixel graphics, or as text from their
//! content.
//!
//! Owns: the terminal's owner loop and the [`sighurt_runtime::frontend::Frontend`] it gives the
//! session ([`app`]), terminal I/O, capability detection, the cell grid and its writer, the
//! terminal chrome, `sig://home` in the page rows ([`home`]), `sig --dump` (`dump`), the page's
//! CPU target and its graphics encoding, key and
//! mouse mapping, and
//! binding the compiled-in engines to that target ([`engines`]). Must not own: browser policy
//! (the core), which effect runs (the runtime), engine internals, or anything of Wayland or
//! sighurt-ui.
//!
//! The terminal's I/O ([`term`]) is a Unix terminal (termios, signals, descriptors polled with
//! calloop) or a Windows console in VT mode (its own input and output threads, which wake
//! calloop with pings); the owner loop and `--dump` are the same on both, apart from what
//! [`sys`] does for each. Elsewhere the crate has the rest, and [`run`] and [`dump`] say the
//! terminal is not supported there (docs/rewrite/PORTING.md).

#[cfg(any(unix, windows))]
mod app;
pub mod chrome;
#[cfg(any(unix, windows))]
mod dump;
// Bound to the terminal's target by the owner loop and `--dump`.
#[cfg_attr(not(any(unix, windows)), allow(dead_code))]
mod engines;
pub mod grid;
pub mod home;
pub mod input;
pub mod kitty;
pub mod sanitize;
pub mod sixel;
#[cfg(any(unix, windows))]
mod sys;
pub mod target;
#[cfg(any(unix, windows))]
pub mod term;
/// Without a terminal: only the escape sequences the grid writes.
#[cfg(not(any(unix, windows)))]
mod term {
    #[allow(dead_code)]
    mod modes;
    pub(crate) use modes::{SYNC_BEGIN, SYNC_END};
}
pub mod text;
pub mod textmode;
#[cfg(not(any(unix, windows)))]
mod unsupported;

#[cfg(any(unix, windows))]
pub use app::{run, Options};
#[cfg(any(unix, windows))]
pub use dump::{run as dump, DumpOptions};
#[cfg(not(any(unix, windows)))]
pub use unsupported::{dump, run, DumpOptions, Options};
