//! `sighurt-config-example`: the configuration compiled into `sig`
//! (docs/rewrite/experience-2026-11-15.md, §2). [`configure`] returns the experience `sig`
//! runs: the default experience (`sighurt_config::defaults`), changed as this crate says. As it
//! ships it changes nothing; the commented examples below show each kind of change. Edit them
//! (or point the build at a copy of this crate) to use a configuration in Rust.
//!
//! `sig` takes the experience apart: its keys go to the core, which applies the configuration
//! file's `[keys]` over them when `[input] keymap` is `example` (the default). An experience
//! with any problem (a bad key or command, a mode that doesn't exist, two bindings where one
//! starts the other) is not used at all: `sig` says what is wrong and runs the default.
//!
//! Must not own: anything at run time. It is data and the pure functions it names.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[allow(unused_imports)]
use sighurt_config::prelude::*;

/// The experience `sig` runs.
pub fn configure() -> Experience {
    #[allow(unused_mut)]
    let mut sig = Experience::default();

    // Change one key, keep the rest: a binding replaces only what it binds.
    //
    //     sig.keys.normal().bind("e", "page.switch-engine").unbind("x");
    //
    // A sequence, and a command written as a value rather than text:
    //
    //     sig.keys.normal()
    //         .bind("g g", "page.send-key home")
    //         .bind("ctrl-g", Command::PageOpen("https://github.com".into()));
    //
    // A key in the normal mode and the address line at once, and one in the terminal only:
    //
    //     sig.keys.tables(["normal", "line"]).bind("ctrl-t", "tab.new");
    //     sig.keys.normal().in_terminal().bind("ctrl-q", "browser.quit");
    //
    // Bindings for some addresses, layered over the mode's (an address chooses shortcuts and
    // grants nothing): here Escape goes to the page in insert, and F12 leaves insert.
    //
    //     sig.keys.mode("insert").on("https://mail.example.com")
    //         .unbind("escape")
    //         .bind("f12", "input.mode normal");
    //
    // A mode of your own, entered with a key and left with Escape; it takes every key it
    // doesn't bind:
    //
    //     sig.keys.add_mode(Mode::new("zoom").indicator("ZOOM").unmatched(Fallback::Take));
    //     sig.keys.normal().bind("z", "input.mode zoom");
    //     sig.keys.mode("zoom")
    //         .bind("i", "page.zoom-in")
    //         .bind("o", "page.zoom-out")
    //         .bind("escape", "input.mode normal");
    //
    // An action computed from what the browser shows (it reads only the view it is given and
    // passes page data on only as a command's operand):
    //
    //     fn duplicate_left(browser: &dyn Browser) -> Outcome {
    //         let commands = match browser.active_tab() {
    //             0 => vec![Command::TabDuplicate],
    //             _ => vec![Command::TabDuplicate, "tab.move left".parse().unwrap()],
    //         };
    //         Outcome::run(commands)
    //     }
    //     sig.keys.normal().bind("D", custom("duplicate the tab", duplicate_left));
    //
    // A gentler normal mode, which gives the page the keys it doesn't bind (its fields are
    // public data):
    //
    //     for mode in &mut sig.keys.value_mut().modes {
    //         if mode.name == "normal" {
    //             mode.unmatched = Fallback::Pass;
    //         }
    //     }
    //
    // Or start from nothing at all, as `keymap = "bindings"` does: `Experience::empty()`.

    sig
}
