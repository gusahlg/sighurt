//! `sig://terminal` and `sig://config`: a terminal emulator inside sig's Wasm sandbox, for a
//! program the browser starts (docs/rewrite/pages-2026-11-05.md, D, E and the amendments;
//! STATUS decisions 138 and 140).
//!
//! The program is the user's shell on `sig://terminal` and the user's editor on sig's
//! configuration on `sig://config`: one module, two pages, and the page's grant decides which
//! program the browser lets it start. The browser builds the rest and runs it with the user's
//! full rights. What runs here is the emulator: it reads the program's output, which is
//! untrusted, draws the screen on the canvas, and turns keys and pastes into the program's
//! input. Nothing starts until the user presses Enter in the page.
//!
//! The logic is plain Rust, tested on the host: keys ([`keys`]), pastes ([`paste`]), the
//! emulator and its query answers ([`term`]), drawing ([`draw`]) and the page's states
//! ([`app`]). `page`, compiled only for wasm32, is the thin layer of `sig` and canvas calls.
//!
//! Must not own: what runs (the runtime's), who may have a terminal (the core's), or the bytes on
//! the way (the Wasm engine's `pty`).

pub mod app;
pub mod draw;
pub mod keys;
pub mod paste;
pub mod term;

#[cfg(target_arch = "wasm32")]
mod page;

use sig_page_kit::sig::Program;

/// What the page at `url` runs: `sig://config` the editor on the configuration, any other (the
/// terminal) the shell. The browser lets each page start only its own.
pub fn program_for(url: &str) -> Program {
    let host = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if host.eq_ignore_ascii_case("config") {
        Program::EditConfig
    } else {
        Program::Shell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_page_picks_the_program() {
        assert_eq!(program_for("sig://terminal"), Program::Shell);
        assert_eq!(program_for("sig://terminal/"), Program::Shell);
        assert_eq!(program_for("sig://terminal/config"), Program::Shell);
        assert_eq!(program_for("sig://config"), Program::EditConfig);
        assert_eq!(program_for("sig://config/"), Program::EditConfig);
        assert_eq!(program_for("sig://config?x#y"), Program::EditConfig);
        assert_eq!(program_for("sig://configure"), Program::Shell);
    }
}
