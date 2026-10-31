//! The binding tables: what each key does in NORMAL and INSERT, and where it applies. Owns the
//! data and nothing else; matching keys and the modes' behaviour are `lib.rs`'s.

use sighurt_api::commands::Command;
use sighurt_api::keymap::Browser;

use crate::actions::{close_other_tabs, copy_address};

/// Where a binding applies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Where {
    /// Both frontends.
    Both,
    /// The terminal only.
    Terminal,
    /// The window only.
    Window,
}

/// A mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Keys are commands; unbound keys reach the page.
    Normal,
    /// Every key reaches whatever has the keyboard, except Escape.
    Insert,
}

/// Where in the window a binding applies: `Page` while the page has the keyboard; `Always` in
/// the page and, if its first key types nothing, in the address line too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Only while the page has the keyboard.
    Page,
    /// Also while the address line has it.
    Always,
}

/// What a binding does.
#[derive(Clone, Copy, Debug)]
pub enum Action {
    /// A command, parsed once at construction.
    Run(&'static str),
    /// The command, then the key goes on to whatever has the keyboard.
    RunThenPass(&'static str),
    /// Switch mode.
    Mode(Mode),
    /// What the configuration computes from the browser: the commands to run, or none to take
    /// the key and do nothing.
    Custom(fn(&dyn Browser) -> Vec<Command>),
}

/// One binding.
#[derive(Clone, Copy, Debug)]
pub struct Binding {
    /// One to four keystrokes, written as `[keys]` writes them, separated by spaces.
    pub keys: &'static str,
    /// What it does.
    pub action: Action,
    /// Where in the window it applies.
    pub scope: Scope,
    /// In which frontend.
    pub frontend: Where,
}

const fn bind(keys: &'static str, action: Action, scope: Scope, frontend: Where) -> Binding {
    Binding {
        keys,
        action,
        scope,
        frontend,
    }
}

use Action::{Custom, Mode as To, Run, RunThenPass};
use Scope::{Always, Page};
use Where::{Both, Terminal};

/// NORMAL: the terminal's keys of 0.9.0 for both frontends, plus two actions of its own.
pub const NORMAL: &[Binding] = &[
    bind("q", Run("browser.quit"), Page, Both),
    bind("o", Run("location.focus"), Page, Both),
    // The line opens with ":" typed into it.
    bind(":", RunThenPass("location.focus"), Page, Both),
    bind("ctrl-o", Run("location.focus"), Always, Both),
    bind("t", Run("tab.new"), Page, Both),
    bind("x", Run("tab.close"), Page, Both),
    bind("J", Run("tab.next"), Page, Both),
    bind("K", Run("tab.previous"), Page, Both),
    bind("H", Run("page.back"), Page, Both),
    bind("L", Run("page.forward"), Page, Both),
    bind("r", Run("page.reload"), Page, Both),
    bind("ctrl-r", Run("page.reload"), Always, Both),
    bind("e", Run("page.switch-engine"), Page, Both),
    bind("j", Run("page.send-key down"), Page, Both),
    bind("k", Run("page.send-key up"), Page, Both),
    bind("ctrl-d", Run("page.send-key pagedown"), Page, Both),
    bind("ctrl-u", Run("page.send-key pageup"), Page, Both),
    bind("g", Run("page.send-key home"), Page, Both),
    bind("G", Run("page.send-key end"), Page, Both),
    bind("+", Run("page.zoom-in"), Page, Both),
    bind("-", Run("page.zoom-out"), Page, Both),
    bind("=", Run("page.zoom-reset"), Page, Both),
    bind("alt-1", Run("tab.select 1"), Always, Both),
    bind("alt-2", Run("tab.select 2"), Always, Both),
    bind("alt-3", Run("tab.select 3"), Always, Both),
    bind("alt-4", Run("tab.select 4"), Always, Both),
    bind("alt-5", Run("tab.select 5"), Always, Both),
    bind("alt-6", Run("tab.select 6"), Always, Both),
    bind("alt-7", Run("tab.select 7"), Always, Both),
    bind("alt-8", Run("tab.select 8"), Always, Both),
    bind("alt-9", Run("tab.select 9"), Always, Both),
    // Terminal only: its chrome's commands, and Ctrl+C as the way out of a page field.
    bind("ctrl-c", Run("browser.quit"), Page, Terminal),
    bind("m", Run("chrome.toggle-mode"), Page, Terminal),
    bind("ctrl-l", Run("chrome.refresh"), Always, Terminal),
    // The mode switch.
    bind("escape", To(Mode::Insert), Page, Both),
    // Defined by this configuration against the state API.
    bind("y y", Custom(copy_address), Page, Both),
    bind("c o", Custom(close_other_tabs), Page, Both),
];

/// INSERT: only Escape; every other key passes.
pub const INSERT: &[Binding] = &[bind("escape", To(Mode::Normal), Page, Both)];
