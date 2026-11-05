//! `example`: the example configuration. A NORMAL and an INSERT mode with the terminal's keys in
//! both frontends (`table`), and three actions computed from the browser's state (`actions`),
//! written in Rust against `sighurt_api::keymap` (docs/rewrite/config-extension.md, §3). Select it
//! with `[input] keymap = "example"`; copy the crate and edit the tables to make your own.
//!
//! - **NORMAL**, the mode it starts in: while the page has the keyboard, keys are commands and
//!   none reaches the page, whether or not a text field of it has the keyboard. A bound key runs
//!   its command (the arrows, PageUp, PageDown, Home, End, Space, Tab, Enter and their shifted
//!   forms send that key to the page, to scroll and to move between and follow links); an
//!   unbound key, a modifier and a dead key are taken and do nothing. Space and Enter, which
//!   would type or submit, are taken too while a text field has the keyboard
//!   ([`Scope::NoField`]). No text reaches the page either ([`Keymap::page_takes_text`]): no
//!   input method, no paste. The indicator is empty, or the keys of an unfinished sequence.
//! - **INSERT**: every key reaches the page, and no binding listens, the `Always` ones neither;
//!   the one key taken is an unmodified Escape. Text and pastes reach the page. The indicator is
//!   `INSERT MODE`. On `sig://terminal` (the tab's [`TabInfo::internal`] is `terminal`) Escape
//!   reaches the page too, for the program there, and `ctrl-\ ctrl-n` leaves INSERT
//!   ([`table::INSERT_TERMINAL`]); a `ctrl-\` that another key follows is dropped, as any
//!   unfinished sequence.
//!
//! [`TabInfo::internal`]: sighurt_api::keymap::TabInfo::internal
//! - **Escape** switches mode, except while the page shown holds an element fullscreen (a
//!   video): then it runs `page.exit-fullscreen` and the mode stays.
//! - **The address line and the find field.** The modes are the page's: while the chrome has
//!   the keyboard, in either mode, only NORMAL's `Always` bindings whose first key types nothing
//!   apply, and every other key, Escape included, is the line's. The mode stays.
//! - **The mouse** is not typing: clicks reach the page in both modes and change no mode.
//! - **Sequences** are at most 4 keys, no binding the start of another, and never wait for a
//!   timeout. Modifier and dead keys are not keys of a sequence; a key that doesn't continue
//!   one drops it and starts afresh; `context` and `reset` drop it.
//! - **Repeats** run a single-key `Run` again; anything else taken is held quietly.
//!
//! Must not own: the presses taken (the core's) or what a command does. It keeps no more of what
//! the user types than an unfinished sequence, and logs nothing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod actions;
pub mod table;

use std::str::FromStr;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::input::{KeyCode, KeyEvent, KeyboardFocus};
use sighurt_api::keymap::{
    is_modifier, Browser, Chord, Frontend, KeyAction, KeyContext, Keymap, KeymapKind,
};
use sighurt_api::settings::Table;

pub use table::{Action, Binding, Mode, Scope, Where};

/// The example configuration, for the registry: `[input] keymap = "example"`.
pub const KIND: KeymapKind = KeymapKind {
    name: "example",
    check,
    new: || Box::new(Example::new()),
};

/// The most keystrokes in one binding.
pub const MAX_KEYS: usize = 4;

fn check(table: &Table, _commands: &CommandSet<'_>) -> Vec<String> {
    table
        .keys()
        .map(|key| {
            format!(
                "[keymap.example] takes no settings; the example configuration's bindings are \
                 in its crate (not {key:?})"
            )
        })
        .collect()
}

/// The bindings that exist in `frontend`, in both modes, as `(keys, action)`.
pub fn bindings(frontend: Frontend) -> impl Iterator<Item = (&'static str, &'static Action)> {
    table::NORMAL
        .iter()
        .chain(table::INSERT)
        .chain(table::INSERT_TERMINAL)
        .filter(move |binding| exists_in(binding.frontend, frontend))
        .map(|binding| (binding.keys, &binding.action))
}

fn exists_in(wanted: Where, frontend: Frontend) -> bool {
    matches!(
        (wanted, frontend),
        (Where::Both, _) | (Where::Terminal, Frontend::Terminal) | (Where::Window, Frontend::Gui)
    )
}

/// A binding with its keys and command parsed.
#[derive(Debug)]
struct Parsed {
    keys: Vec<Chord>,
    action: Resolved,
    scope: Scope,
    frontend: Where,
}

#[derive(Clone, Debug)]
enum Resolved {
    Run(Command),
    RunThenPass(Command),
    Mode(Mode),
    ExitFullscreenOr(Mode),
    Custom(fn(&dyn Browser) -> Vec<Command>),
}

/// Parses `table`'s bindings. A bad built-in key or command is a bug of this crate; the tests
/// parse every entry.
fn parse(table: &[Binding]) -> Vec<Parsed> {
    let command = |text: &str| {
        Command::from_str(text).unwrap_or_else(|e| panic!("built-in command {text:?}: {e}"))
    };
    table
        .iter()
        .map(|binding| Parsed {
            keys: binding
                .keys
                .split_whitespace()
                .map(|key| {
                    key.parse()
                        .unwrap_or_else(|e| panic!("built-in keys {:?}: {e}", binding.keys))
                })
                .collect(),
            action: match binding.action {
                Action::Run(text) => Resolved::Run(command(text)),
                Action::RunThenPass(text) => Resolved::RunThenPass(command(text)),
                Action::Mode(mode) => Resolved::Mode(mode),
                Action::ExitFullscreenOr(mode) => Resolved::ExitFullscreenOr(mode),
                Action::Custom(f) => Resolved::Custom(f),
            },
            scope: binding.scope,
            frontend: binding.frontend,
        })
        .collect()
}

/// What some keys are, in the bindings that apply.
enum Found<'a> {
    /// The keys of this binding.
    Complete(&'a Parsed),
    /// The start of this binding.
    Start(&'a Parsed),
    /// Nothing.
    None,
}

/// The example configuration.
#[derive(Debug)]
pub struct Example {
    normal: Vec<Parsed>,
    insert: Vec<Parsed>,
    /// INSERT on `sig://terminal`.
    insert_terminal: Vec<Parsed>,
    mode: Mode,
    /// The keys of an unfinished sequence, at most `MAX_KEYS - 1`.
    pending: Vec<KeyEvent>,
    /// The key of the single-key command last run, which its repeats run again.
    held: Option<(KeyCode, Command)>,
    indicator: String,
}

impl Default for Example {
    fn default() -> Self {
        Self::new()
    }
}

impl Example {
    /// The configuration, in NORMAL.
    pub fn new() -> Self {
        Self {
            normal: parse(table::NORMAL),
            insert: parse(table::INSERT),
            insert_terminal: parse(table::INSERT_TERMINAL),
            mode: Mode::Normal,
            pending: Vec::new(),
            held: None,
            indicator: String::new(),
        }
    }

    /// What `keys` are in the bindings that apply now; `terminal` is whether the tab shown is
    /// `sig://terminal`.
    fn find(
        &self,
        keys: &[KeyEvent],
        context: KeyContext,
        frontend: Frontend,
        terminal: bool,
    ) -> Found<'_> {
        let chrome = context.focus == KeyboardFocus::Chrome;
        // The modes are the page's: the line keeps NORMAL's few bindings in both.
        let table = match (chrome, self.mode) {
            (true, _) | (false, Mode::Normal) => &self.normal,
            (false, Mode::Insert) if terminal => &self.insert_terminal,
            (false, Mode::Insert) => &self.insert,
        };
        let applies = |binding: &&Parsed| {
            exists_in(binding.frontend, frontend)
                && match binding.scope {
                    Scope::Always => !(chrome && binding.keys[0].types()),
                    Scope::Page => !chrome,
                    Scope::NoField => !chrome && !context.typing,
                }
        };
        let fits = |binding: &&Parsed| {
            binding.keys.len() >= keys.len()
                && keys
                    .iter()
                    .zip(&binding.keys)
                    .all(|(key, chord)| chord.matches(key))
        };
        let mut start = None;
        for binding in table.iter().filter(applies).filter(fits) {
            if binding.keys.len() == keys.len() {
                return Found::Complete(binding);
            }
            start.get_or_insert(binding);
        }
        start.map_or(Found::None, Found::Start)
    }

    fn show(&mut self, sequence: &str) {
        self.indicator = match self.mode {
            Mode::Normal => sequence.trim_start().to_owned(),
            Mode::Insert => "INSERT MODE".to_owned(),
        };
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.pending.clear();
        self.show("");
    }
}

impl Keymap for Example {
    fn configure(&mut self, _settings: &Table) {
        // It takes no settings; a reload keeps the mode.
        self.held = None;
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> KeyAction {
        if key.repeat {
            return match &self.held {
                Some((code, command)) if *code == key.code => KeyAction::Run(command.clone()),
                _ => KeyAction::Take,
            };
        }
        // What becomes of a key no binding has: in NORMAL the page gets no key at all.
        let page = context.focus == KeyboardFocus::Page;
        let unbound = match (page, self.mode) {
            (true, Mode::Normal) => KeyAction::Take,
            _ => KeyAction::Pass,
        };
        if is_modifier(key) {
            return unbound;
        }
        self.held = None;
        let frontend = browser.frontend();
        // From the core's record of the view, never from an address.
        let terminal = browser
            .tab(browser.active_tab())
            .is_some_and(|tab| tab.internal == Some("terminal"));
        let mut keys = std::mem::take(&mut self.pending);
        keys.push(key.clone());
        // Keys that continue no sequence drop it, and the last starts afresh.
        let mut found = self.find(&keys, context, frontend, terminal);
        if matches!(found, Found::None) && keys.len() > 1 {
            keys.drain(..keys.len() - 1);
            found = self.find(&keys, context, frontend, terminal);
        }
        let action = match found {
            Found::Complete(binding) => Some(binding.action.clone()),
            Found::Start(binding) => {
                let typed = &binding.keys[..keys.len()];
                let sequence: String = typed.iter().map(|key| format!(" {key}")).collect();
                self.pending = keys;
                self.show(&sequence);
                return KeyAction::Take;
            }
            Found::None => None,
        };
        self.show("");
        match action {
            Some(Resolved::Run(command)) => {
                if keys.len() == 1 {
                    self.held = Some((key.code, command.clone()));
                }
                KeyAction::Run(command)
            }
            Some(Resolved::RunThenPass(command)) => KeyAction::RunThenPass(command),
            Some(Resolved::ExitFullscreenOr(_))
                if browser
                    .tab(browser.active_tab())
                    .is_some_and(|tab| tab.fullscreen) =>
            {
                KeyAction::Run(Command::PageExitFullscreen)
            }
            Some(Resolved::Mode(mode) | Resolved::ExitFullscreenOr(mode)) => {
                self.set_mode(mode);
                KeyAction::Take
            }
            Some(Resolved::Custom(f)) => KeyAction::RunAll(f(browser)),
            None => unbound,
        }
    }

    fn context(&mut self, _context: KeyContext) {
        self.pending.clear();
        self.show("");
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.held = None;
        self.show("");
    }

    fn indicator(&self) -> Option<&str> {
        Some(self.indicator.as_str()).filter(|text| !text.is_empty())
    }

    fn page_takes_text(&self) -> bool {
        self.mode == Mode::Insert
    }
}
