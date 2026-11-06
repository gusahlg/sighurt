//! `sig://terminal`: a terminal emulator inside sig's Wasm sandbox, for a program the browser
//! starts (docs/rewrite/pages-2026-11-05.md, D, E and the amendments).
//!
//! The program is the user's shell (`sig://terminal`) or their editor on sig's configuration
//! (`sig://terminal/config`); the page only chooses between the two, and the browser builds the
//! rest and runs it with the user's full rights. What runs here is the emulator: it reads the
//! program's output, which is untrusted, draws the screen on the canvas, and turns keys and
//! pastes into the program's input. Nothing starts until the user presses Enter in the page.
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

/// What `url` runs: `sig://terminal/config` the editor on the configuration, anything else of
/// the page the shell.
pub fn program_for(url: &str) -> Program {
    let path = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest)
        .split(['?', '#'])
        .next()
        .unwrap_or("");
    let path = path.split_once('/').map_or("", |(_, path)| path);
    if path.trim_end_matches('/') == "config" {
        Program::EditConfig
    } else {
        Program::Shell
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_picks_the_program() {
        assert_eq!(program_for("sig://terminal"), Program::Shell);
        assert_eq!(program_for("sig://terminal/"), Program::Shell);
        assert_eq!(program_for("sig://terminal/config"), Program::EditConfig);
        assert_eq!(program_for("sig://terminal/config/"), Program::EditConfig);
        assert_eq!(
            program_for("sig://terminal/config?x#y"),
            Program::EditConfig
        );
        assert_eq!(program_for("sig://terminal/configure"), Program::Shell);
        assert_eq!(program_for("sig://terminal/x/config"), Program::Shell);
    }
}
