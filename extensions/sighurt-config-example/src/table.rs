//! The binding tables: what each key does in NORMAL and INSERT, and where it applies. Owns the
//! data and nothing else; matching keys and the modes' behaviour are `lib.rs`'s.

use sighurt_api::commands::Command;
use sighurt_api::keymap::Browser;

use crate::actions::{close_other_tabs, copy_address, copy_link};

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
    /// Keys are commands; no key and no text reaches the page.
    Normal,
    /// Every key and all text reach the page; only Escape is the keymap's.
    Insert,
}

/// Where in the window a binding applies: `Page` while the page has the keyboard; `NoField`
/// while the page has it and none of its text fields does; `Always` in the page and, if its
/// first key types nothing, in the address line and the find field too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Only while the page has the keyboard.
    Page,
    /// Only while the page has the keyboard and no text field of it does: a key the binding
    /// sends would type or submit there (Space, Enter), so in a field NORMAL takes it instead.
    NoField,
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
    /// `page.exit-fullscreen` while the page shown holds an element fullscreen (a video), so
    /// Escape leaves it as on the web; otherwise switch mode.
    ExitFullscreenOr(Mode),
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

use Action::{Custom, ExitFullscreenOr, Run};
use Scope::{Always, NoField, Page};
use Where::{Both, Terminal};

/// NORMAL: the terminal's keys of 0.9.0 for both frontends, the page's own navigation keys,
/// help, plus three actions of its own. No other key reaches the page.
pub const NORMAL: &[Binding] = &[
    bind("q", Run("browser.quit"), Page, Both),
    bind("o", Run("location.focus"), Page, Both),
    // The command line: the line opens holding ":". "/" opens the chrome's find field.
    bind(":", Run("location.focus :"), Page, Both),
    bind("/", Run("page.find"), Page, Both),
    bind("n", Run("page.find-next"), Page, Both),
    bind("N", Run("page.find-previous"), Page, Both),
    bind("ctrl-o", Run("location.focus"), Always, Both),
    bind("t", Run("tab.new"), Page, Both),
    bind("x", Run("tab.close"), Page, Both),
    bind("d", Run("tab.close"), Page, Both),
    bind("u", Run("tab.reopen"), Page, Both),
    bind("J", Run("tab.next"), Page, Both),
    bind("K", Run("tab.previous"), Page, Both),
    bind("H", Run("page.back"), Page, Both),
    bind("L", Run("page.forward"), Page, Both),
    bind("r", Run("page.reload"), Page, Both),
    bind("ctrl-r", Run("page.reload"), Always, Both),
    bind("f11", Run("browser.fullscreen"), Always, Both),
    // `help` is the configuration's own command (`[commands]`): sig://help in a new tab.
    bind("?", Run("help"), Page, Both),
    bind("f1", Run("help"), Always, Both),
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
    // The page's own keys for scrolling, moving between links and following one, sent to it.
    bind("down", Run("page.send-key down"), Page, Both),
    bind("up", Run("page.send-key up"), Page, Both),
    bind("left", Run("page.send-key left"), Page, Both),
    bind("right", Run("page.send-key right"), Page, Both),
    bind("pagedown", Run("page.send-key pagedown"), Page, Both),
    bind("pageup", Run("page.send-key pageup"), Page, Both),
    bind("home", Run("page.send-key home"), Page, Both),
    bind("end", Run("page.send-key end"), Page, Both),
    bind("tab", Run("page.send-key tab"), Page, Both),
    bind("shift-tab", Run("page.send-key shift-tab"), Page, Both),
    bind("space", Run("page.send-key space"), NoField, Both),
    bind(
        "shift-space",
        Run("page.send-key shift-space"),
        NoField,
        Both,
    ),
    bind("enter", Run("page.send-key enter"), NoField, Both),
    bind("alt-1", Run("tab.select 1"), Always, Both),
    bind("alt-2", Run("tab.select 2"), Always, Both),
    bind("alt-3", Run("tab.select 3"), Always, Both),
    bind("alt-4", Run("tab.select 4"), Always, Both),
    bind("alt-5", Run("tab.select 5"), Always, Both),
    bind("alt-6", Run("tab.select 6"), Always, Both),
    bind("alt-7", Run("tab.select 7"), Always, Both),
    bind("alt-8", Run("tab.select 8"), Always, Both),
    bind("alt-9", Run("tab.select 9"), Always, Both),
    // Terminal only: its chrome's commands, and Ctrl+C, a terminal's usual way out.
    bind("ctrl-c", Run("browser.quit"), Page, Terminal),
    bind("m", Run("chrome.toggle-mode"), Page, Terminal),
    bind("ctrl-l", Run("chrome.refresh"), Always, Terminal),
    // The mode switch, unless a page's element is fullscreen: then Escape leaves that.
    bind("escape", ExitFullscreenOr(Mode::Insert), Page, Both),
    // Defined by this configuration against the state API.
    bind("y y", Custom(copy_address), Page, Both),
    bind("y f", Custom(copy_link), Page, Both),
    bind("c o", Custom(close_other_tabs), Page, Both),
];

/// INSERT: only Escape; every other key reaches the page.
pub const INSERT: &[Binding] = &[bind("escape", ExitFullscreenOr(Mode::Normal), Page, Both)];
