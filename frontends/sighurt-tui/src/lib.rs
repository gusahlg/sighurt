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
//! The terminal's I/O ([`term`]), the owner loop and `--dump` are Unix's (termios, signals,
//! descriptors polled with calloop). Elsewhere (Windows) the crate has the rest, and [`run`] and
//! [`dump`] say the terminal is not supported there (docs/rewrite/PORTING.md).

#[cfg(unix)]
mod app;
pub mod chrome;
#[cfg(unix)]
mod dump;
// Bound to the terminal's target by the owner loop and `--dump`, which are Unix's.
#[cfg_attr(not(unix), allow(dead_code))]
mod engines;
pub mod grid;
pub mod home;
pub mod input;
pub mod kitty;
pub mod sanitize;
pub mod sixel;
pub mod target;
#[cfg(unix)]
pub mod term;
/// Without a Unix terminal: only the escape sequences the grid writes.
#[cfg(not(unix))]
mod term {
    #[allow(dead_code)]
    mod modes;
    pub(crate) use modes::{SYNC_BEGIN, SYNC_END};
}
pub mod text;
pub mod textmode;
#[cfg(not(unix))]
mod unsupported;

#[cfg(unix)]
pub use app::{run, Options};
#[cfg(unix)]
pub use dump::{run as dump, DumpOptions};
#[cfg(not(unix))]
pub use unsupported::{dump, run, DumpOptions, Options};
