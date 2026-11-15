//! The default experience: Sighurt's keys, written with nothing but this crate's prelude (a
//! test holds this file to it), as a configuration would write them.
//!
//! - **normal**, the mode a session starts in: while the page has the keyboard, keys are
//!   commands and none reaches the page, whether or not a text field of it has the keyboard
//!   (decision 134). A bound key runs its command (the arrows, PageUp, PageDown, Home, End,
//!   Space, Tab, Enter and their shifted forms send that key to the page); a key no binding has
//!   is taken. Space, Shift+Space and Enter, which would type or submit, are bound only outside
//!   fields, so in a field they are taken too. No text reaches the page: no input method, no
//!   paste. Its indicator is empty.
//! - **insert**: every key and all text reach the page; Escape leaves it. Its indicator is
//!   `INSERT MODE`.
//! - **Escape** switches mode, except while the page shown holds an element fullscreen (a
//!   video): then it leaves that, and the mode stays.
//! - **The line** (the address line and the find field) has the keys that work wherever the
//!   keyboard is: Ctrl+O, Ctrl+R, F11, F1, Alt+1 to Alt+9, and in the terminal Ctrl+L. Every
//!   other key is the line's.
//! - **sig://terminal and sig://config**: Escape is the program's in insert (nvim needs it),
//!   and Ctrl+Escape or F12 leaves insert. Ctrl+Escape works in the window and in terminals
//!   that speak the kitty keyboard protocol; a terminal with legacy key encoding (and tmux by
//!   default) sends it as a plain Escape, so F12, which every terminal sends, is the key there.
//! - Three actions computed from the browser: `y y` copies the page's address, `y f` the link
//!   under the pointer, `c o` closes the other tabs.
//!
//! Must not own: anything but this experience's data and its three functions, which read only
//! the `Browser` they are given.

use std::num::NonZeroUsize;

use crate::prelude::*;

/// The default experience.
pub fn experience() -> Experience {
    let mut sig = Experience::empty();
    let keys = &mut sig.keys;
    keys.add_mode(
        Mode::new("normal")
            .page_text(false)
            .unmatched(Fallback::Take),
    )
    .add_mode(Mode::new("insert").indicator("INSERT MODE"))
    .initial("normal")
    .remove_mode(PLAIN_MODE);

    // The terminal's keys of 0.9.0 for both frontends, the page's own navigation keys, help.
    keys.normal()
        .bind("q", "browser.quit")
        .bind("o", "location.focus")
        // The command line: the line opens holding ":". "/" opens the chrome's find field.
        .bind(":", "location.focus :")
        .bind("/", "page.find")
        .bind("n", "page.find-next")
        .bind("N", "page.find-previous");
    keys.tables(["normal", "line"])
        .bind("ctrl-o", "location.focus");
    keys.normal()
        .bind("t", "tab.new")
        .bind("x", "tab.close")
        .bind("d", "tab.close")
        .bind("u", "tab.reopen")
        .bind("J", "tab.next")
        .bind("K", "tab.previous")
        .bind("H", "page.back")
        .bind("L", "page.forward")
        .bind("r", "page.reload");
    keys.tables(["normal", "line"])
        .bind("ctrl-r", "page.reload")
        .bind("f11", "browser.fullscreen");
    // `help` is the configuration's own command (`[commands]`): sig://help in a new tab.
    keys.normal().bind("?", "help");
    keys.tables(["normal", "line"]).bind("f1", "help");
    keys.normal()
        .bind("e", "page.switch-engine")
        .bind("j", "page.send-key down")
        .bind("k", "page.send-key up")
        .bind("ctrl-d", "page.send-key pagedown")
        .bind("ctrl-u", "page.send-key pageup")
        .bind("g", "page.send-key home")
        .bind("G", "page.send-key end")
        .bind("+", "page.zoom-in")
        .bind("-", "page.zoom-out")
        .bind("=", "page.zoom-reset")
        // The page's own keys for scrolling, moving between links and following one.
        .bind("down", "page.send-key down")
        .bind("up", "page.send-key up")
        .bind("left", "page.send-key left")
        .bind("right", "page.send-key right")
        .bind("pagedown", "page.send-key pagedown")
        .bind("pageup", "page.send-key pageup")
        .bind("home", "page.send-key home")
        .bind("end", "page.send-key end")
        .bind("tab", "page.send-key tab")
        .bind("shift-tab", "page.send-key shift-tab");
    // Space and Enter would type or submit in a field: there, normal takes them.
    keys.normal()
        .outside_fields()
        .bind("space", "page.send-key space")
        .bind("shift-space", "page.send-key shift-space")
        .bind("enter", "page.send-key enter");
    let mut both = keys.tables(["normal", "line"]);
    for n in 1..=9 {
        both.bind(&format!("alt-{n}"), format!("tab.select {n}"));
    }
    // Terminal only: its chrome's commands, and Ctrl+C, a terminal's usual way out.
    keys.normal()
        .in_terminal()
        .bind("ctrl-c", "browser.quit")
        .bind("m", "chrome.toggle-mode");
    keys.tables(["normal", "line"])
        .in_terminal()
        .bind("ctrl-l", "chrome.refresh");
    // The mode switch, unless a page's element is fullscreen: then Escape leaves that.
    keys.normal().bind(
        "escape",
        custom("INSERT mode (or leave a page's fullscreen)", |browser| {
            leave_fullscreen_or("insert", browser)
        }),
    );
    // Defined by this experience against the state API.
    keys.normal()
        .bind("y y", custom("copy the page's address", copy_address))
        .bind("y f", custom("copy the link under the pointer", copy_link))
        .bind("c o", custom("close the other tabs", close_other_tabs));

    keys.mode("insert").bind(
        "escape",
        custom("NORMAL mode (or leave a page's fullscreen)", |browser| {
            leave_fullscreen_or("normal", browser)
        }),
    );
    // The pages that run a program in a terminal: Escape is the program's.
    for page in ["sig://terminal", "sig://config"] {
        keys.mode("insert")
            .on(page)
            .unbind("escape")
            .bind("ctrl-escape", "input.mode normal")
            .bind("f12", "input.mode normal");
    }
    sig
}

/// `page.exit-fullscreen` while the page shown holds an element fullscreen, so Escape leaves
/// it as on the web; otherwise `input.mode <mode>`.
fn leave_fullscreen_or(mode: &str, browser: &dyn Browser) -> Outcome {
    let tab = browser.tab(browser.active_tab());
    Outcome::run(vec![match tab.is_some_and(|tab| tab.fullscreen) {
        true => Command::PageExitFullscreen,
        false => Command::InputMode(mode.into()),
    }])
}

/// `y y`: copy the selected tab's address. Nothing to copy: the key is taken and nothing runs.
pub fn copy_address(browser: &dyn Browser) -> Outcome {
    let url = browser.tab(browser.active_tab()).and_then(|tab| tab.url);
    Outcome::run(
        url.map(|url| Command::ClipboardCopy(url.to_owned()))
            .into_iter()
            .collect(),
    )
}

/// `y f`: copy the link the pointer is over in the selected tab. No link: the key is taken and
/// nothing runs.
pub fn copy_link(browser: &dyn Browser) -> Outcome {
    let link = browser.tab(browser.active_tab()).and_then(|tab| tab.link);
    Outcome::run(
        link.map(|link| Command::ClipboardCopy(link.to_owned()))
            .into_iter()
            .collect(),
    )
}

/// `c o`: close every tab but the selected one, highest number first so the numbers of the ones
/// still to close don't move.
pub fn close_other_tabs(browser: &dyn Browser) -> Outcome {
    let active = browser.active_tab();
    Outcome::run(
        (0..browser.tab_count())
            .rev()
            .filter(|&index| index != active)
            .filter_map(|index| NonZeroUsize::new(index + 1))
            .map(|number| Command::TabClose(Some(number)))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_defaults_use_only_the_prelude() {
        let source = include_str!("defaults.rs");
        let code = &source[..source.find("#[cfg(test)]").unwrap()];
        let uses: Vec<&str> = code
            .lines()
            .filter(|line| line.trim_start().starts_with("use "))
            .collect();
        assert_eq!(
            uses,
            ["use std::num::NonZeroUsize;", "use crate::prelude::*;"]
        );
        for word in ["sighurt_api", "crate::", "super::", "value_mut"] {
            let outside_use = code
                .lines()
                .filter(|line| !line.trim_start().starts_with("use "))
                .any(|line| line.contains(word));
            assert!(!outside_use, "the defaults name {word}");
        }
    }

    #[test]
    fn the_defaults_have_no_problem() {
        let (keys, problems) = super::experience().finish();
        assert_eq!(problems, Vec::<String>::new());
        let help = ["help".to_owned()];
        let commands = sighurt_api::commands::CommandSet::ANY.with_custom(&help);
        for frontend in [
            sighurt_api::keymap::Frontend::Gui,
            sighurt_api::keymap::Frontend::Terminal,
        ] {
            let problems = keys.check(&commands, frontend);
            assert_eq!(problems, Vec::<String>::new(), "{frontend:?}");
        }
        assert_eq!(keys.initial, "normal");
        let names: Vec<&str> = keys.modes.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["normal", "insert"]);
    }
}
