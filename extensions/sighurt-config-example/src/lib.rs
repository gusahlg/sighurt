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
//!   `INSERT MODE`.
//! - **Sites** ([`table::SITES`]): on an address a site's pattern matches, a table the site
//!   gives for a mode replaces that mode's general table while the page has the keyboard, so
//!   a site can take Escape away from INSERT and leave it on other keys. The address is the
//!   shown tab's committed one ([`TabInfo::url`]), never one loading or a link; the most
//!   specific matching pattern wins ([`UrlPattern`]). No address is named here: the sites are
//!   data in `table`.
//! - **Escape** switches mode (in the general tables), except while the page shown holds an element fullscreen (a
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
//! [`TabInfo::url`]: sighurt_api::keymap::TabInfo::url
//!
//! Must not own: the presses taken (the core's) or what a command does. It keeps no more of what
//! the user types than an unfinished sequence, and logs nothing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod actions;
pub mod table;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::input::{KeyCode, KeyEvent, KeyboardFocus};
use sighurt_api::keymap::{
    is_modifier, Browser, Chord, Frontend, KeyAction, KeyBinding, KeyContext, Keymap, KeymapKind,
    UrlPattern,
};
use sighurt_api::settings::Table;

pub use table::{Action, Binding, Mode, Scope, Site, Where};

/// The example configuration, for the registry: `[input] keymap = "example"`.
pub const KIND: KeymapKind = KeymapKind {
    name: "example",
    check,
    new: || Box::new(Example::new()),
};

/// The most keystrokes in one binding.
pub const MAX_KEYS: usize = 4;

fn check(table: &Table, commands: &CommandSet<'_>) -> Vec<String> {
    let settings = table.keys().map(|key| {
        format!(
            "[keymap.example] takes no settings; the example configuration's bindings are in \
             its crate (not {key:?})"
        )
    });
    // Its commands of `[commands]` (help) must be the configuration's.
    let actions = tables().flat_map(|(_, _, table)| table);
    let commands = actions.filter_map(|binding| match binding.action {
        Action::Run(text) | Action::RunThenPass(text)
            if matches!(CommandSet::ANY.parse(text), Ok(Command::Custom(_))) =>
        {
            commands.parse(text).err().map(|e| {
                format!(
                    "the example configuration binds {} to {text}: {e}",
                    binding.keys
                )
            })
        }
        _ => None,
    });
    settings.chain(commands).collect()
}

/// Every table, as `(mode, site, bindings)`: the general NORMAL and INSERT (no site), then each
/// site's tables in [`table::SITES`] order.
pub fn tables() -> impl Iterator<Item = (Mode, Option<&'static str>, &'static [Binding])> {
    let general = [
        (Mode::Normal, None, table::NORMAL),
        (Mode::Insert, None, table::INSERT),
    ];
    let sites = table::SITES.iter().flat_map(|site| {
        [(Mode::Normal, site.normal), (Mode::Insert, site.insert)]
            .into_iter()
            .filter_map(move |(mode, table)| Some((mode, Some(site.url), table?)))
    });
    general.into_iter().chain(sites)
}

/// The bindings that exist in `frontend`, in every table, as `(keys, action)`; a site's
/// bindings once per site.
pub fn bindings(frontend: Frontend) -> impl Iterator<Item = (&'static str, &'static Action)> {
    tables()
        .flat_map(|(_, _, table)| table)
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

/// A site's tables, parsed.
#[derive(Debug)]
struct ParsedSite {
    pattern: UrlPattern,
    normal: Option<Vec<Parsed>>,
    insert: Option<Vec<Parsed>>,
}

/// Parses `table`'s bindings. A bad built-in key or command is a bug of this crate; the tests
/// parse every entry.
fn parse(table: &[Binding]) -> Vec<Parsed> {
    // Its commands of `[commands]` (help) are checked when the configuration loads (`check`).
    let command = |text: &str| {
        CommandSet::ANY
            .parse(text)
            .unwrap_or_else(|e| panic!("built-in command {text:?}: {e}"))
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
    sites: Vec<ParsedSite>,
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
            sites: table::SITES
                .iter()
                .map(|site| ParsedSite {
                    pattern: site
                        .url
                        .parse()
                        .unwrap_or_else(|e| panic!("built-in site {:?}: {e}", site.url)),
                    normal: site.normal.map(parse),
                    insert: site.insert.map(parse),
                })
                .collect(),
            mode: Mode::Normal,
            pending: Vec::new(),
            held: None,
            indicator: String::new(),
        }
    }

    /// The table of the mode while the page has the keyboard on `address`: the most specific
    /// matching site's for the mode, or the mode's general one.
    fn table(&self, address: Option<&str>) -> &[Parsed] {
        let general = match self.mode {
            Mode::Normal => &self.normal,
            Mode::Insert => &self.insert,
        };
        let sites = self.sites.iter().filter_map(|site| {
            let table = match self.mode {
                Mode::Normal => site.normal.as_ref(),
                Mode::Insert => site.insert.as_ref(),
            };
            Some((&site.pattern, table?))
        });
        address
            .and_then(|address| UrlPattern::most_specific(sites, address))
            .unwrap_or(general)
    }

    /// What `keys` are in the bindings that apply now; `address` is the shown tab's committed
    /// address.
    fn find(
        &self,
        keys: &[KeyEvent],
        context: KeyContext,
        frontend: Frontend,
        address: Option<&str>,
    ) -> Found<'_> {
        let chrome = context.focus == KeyboardFocus::Chrome;
        // The modes are the page's: the line keeps NORMAL's few bindings in both.
        let table = match chrome {
            true => &self.normal[..],
            false => self.table(address),
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
        let tab = browser.tab(browser.active_tab());
        let address = tab.and_then(|tab| tab.url);
        let mut keys = std::mem::take(&mut self.pending);
        keys.push(key.clone());
        // Keys that continue no sequence drop it, and the last starts afresh.
        let mut found = self.find(&keys, context, frontend, address);
        if matches!(found, Found::None) && keys.len() > 1 {
            keys.drain(..keys.len() - 1);
            found = self.find(&keys, context, frontend, address);
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
            Some(Resolved::ExitFullscreenOr(_)) if tab.is_some_and(|tab| tab.fullscreen) => {
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

    fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        let mut found = Vec::new();
        for (mode, site, table) in tables() {
            // The sites that replace this general table, where its page bindings don't apply.
            let replaced: Vec<&str> = table::SITES
                .iter()
                .filter(|other| match mode {
                    Mode::Normal => other.normal.is_some(),
                    Mode::Insert => other.insert.is_some(),
                })
                .map(|other| other.url)
                .collect();
            let except = match (site, replaced.is_empty()) {
                (None, false) => format!(", except on {}", replaced.join(", ")),
                _ => String::new(),
            };
            let name = match mode {
                Mode::Normal => "NORMAL",
                Mode::Insert => "INSERT",
            };
            for binding in table.iter().filter(|b| exists_in(b.frontend, frontend)) {
                let action = match binding.action {
                    Action::Run(text) | Action::RunThenPass(text) => text.to_owned(),
                    Action::Mode(Mode::Insert) => "INSERT mode".to_owned(),
                    Action::Mode(Mode::Normal) => "NORMAL mode".to_owned(),
                    Action::ExitFullscreenOr(Mode::Insert) => {
                        "INSERT mode (or leave a page's fullscreen)".to_owned()
                    }
                    Action::ExitFullscreenOr(Mode::Normal) => {
                        "NORMAL mode (or leave a page's fullscreen)".to_owned()
                    }
                    Action::Custom(_) => describe_custom(binding.keys).to_owned(),
                };
                let applies = match (site, binding.scope) {
                    (Some(url), Scope::Page | Scope::Always) => format!("on {url}"),
                    (Some(url), Scope::NoField) => format!("on {url}, not in a text field"),
                    (None, Scope::Page) => format!("in the page{except}"),
                    (None, Scope::NoField) => format!("in the page, not in a text field{except}"),
                    (None, Scope::Always) if except.is_empty() => {
                        "also in the address line".to_owned()
                    }
                    (None, Scope::Always) => {
                        format!("also in the address line; in the page{except}")
                    }
                };
                found.push(KeyBinding::new(name, binding.keys, action, applies));
            }
        }
        found
    }
}

/// What a binding computed from the browser does, for the help page.
fn describe_custom(keys: &str) -> &'static str {
    match keys {
        "y y" => "copy the page's address",
        "y f" => "copy the link under the pointer",
        "c o" => "close the other tabs",
        _ => "an action of the example configuration",
    }
}
