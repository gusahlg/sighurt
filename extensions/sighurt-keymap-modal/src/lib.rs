//! `modal`: a keymap with modes and key sequences, for vim-style keys. It has no bindings of its
//! own: modes, sequences and switching between them are its code, and what each key does comes
//! from the user's table, `[keymap.modal]`:
//!
//! ```toml
//! [input]
//! keymap = "modal"
//!
//! [keymap.modal]
//! start = "normal"     # the mode it starts in
//! typing = "insert"    # the mode it enters when a text field of the page takes the keyboard;
//!                      # when the field loses it, the keymap returns to `start`
//! pass = ["insert"]    # modes in which unbound keys reach the page; in the others, unbound
//!                      # keys that would type are swallowed (arrows and ctrl-c still pass)
//!
//! [keymap.modal.always]   # every mode, and the only table while the chrome has the keyboard
//! "ctrl-q" = "browser.quit"
//!
//! [keymap.modal.normal]
//! "j" = "page.send-key down"
//! "g g" = "page.send-key home"
//! "g t" = "tab.next"
//! ":" = { run = "location.focus", pass = true }   # opens the command line with ":"
//! "i" = "mode insert"
//!
//! [keymap.modal.insert]
//! "escape" = "mode normal"
//! ```
//!
//! A mode is a sub-table; `always`, `start`, `typing` and `pass` are reserved. A binding maps 1
//! to [`MAX_KEYS`] keystrokes, separated by spaces and written as `[keys]` writes them
//! (`sighurt_api::keymap::Chord`), to a command, to `"mode <name>"`, or to `{ run = "<command>",
//! pass = true }`, which runs the command and then lets the key through to whatever has the
//! keyboard.
//!
//! - **Sequences.** An unfinished sequence takes its keys and shows in the indicator ("NORMAL
//!   g"). A key that doesn't continue it drops it and starts afresh. No binding may be the start
//!   of another in a mode (with `always`), so a sequence never waits for a timeout. Modifier and
//!   dead keys are not keys of a sequence: `g`, Shift, `t` is `g T`.
//! - **Passing.** In a `pass` mode an unbound key reaches the page, so no sequence there may
//!   start with a key that types: it would be held back from the page, and lost if the sequence
//!   didn't finish. A single key that types may be bound; it then never reaches the page in that
//!   mode. `always` follows the same rule, since it applies while the user types.
//! - **The chrome.** While the chrome has the keyboard, only `always` applies, and only its keys
//!   that don't type: the address line takes the rest. The mode stays as it was.
//! - **Repeats.** A held key repeats a single-key command; anything else it took is held
//!   quietly.
//!
//! Must not own: the presses it took, which the core keeps for any keymap, or what a command
//! does. It keeps no more of what the user types than an unfinished sequence, and logs nothing.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::input::{KeyCode, KeyEvent, KeyboardFocus};
use sighurt_api::keymap::{is_modifier, Chord, KeyAction, KeyContext, Keymap, KeymapKind};
use sighurt_api::settings::{Table, Value};

/// The modal keymap, for the registry: `[input] keymap = "modal"`.
pub const KIND: KeymapKind = KeymapKind {
    name: "modal",
    check,
    new: || Box::<Modal>::default(),
};

/// The most keystrokes in one binding.
pub const MAX_KEYS: usize = 4;

/// The table every mode adds to.
const ALWAYS: &str = "always";

/// What a binding does.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    Run(Command),
    RunThenPass(Command),
    Mode(String),
}

#[derive(Clone, Debug)]
struct Binding {
    /// As the user wrote it, for messages.
    text: String,
    keys: Vec<Chord>,
    action: Action,
}

/// `[keymap.modal]`, read.
#[derive(Debug, Default)]
struct Settings {
    start: String,
    typing: Option<String>,
    pass: Vec<String>,
    always: Vec<Binding>,
    modes: BTreeMap<String, Vec<Binding>>,
}

fn check(table: &Table, commands: &CommandSet<'_>) -> Vec<String> {
    read(table, commands).1
}

/// Reads `table` with its commands parsed by `commands`, and says everything wrong with it.
fn read(table: &Table, commands: &CommandSet<'_>) -> (Settings, Vec<String>) {
    let mut problems = Vec::new();
    let mut settings = Settings::default();
    let mut start = None;
    for (key, value) in table {
        match (key.as_str(), value) {
            ("start", Value::String(mode)) => start = Some(mode.clone()),
            ("typing", Value::String(mode)) => settings.typing = Some(mode.clone()),
            ("pass", Value::Array(modes)) => {
                for mode in modes {
                    match mode.as_str() {
                        Some(mode) => settings.pass.push(mode.to_owned()),
                        None => problems.push(format!("pass: expected modes, not {}", mode.kind())),
                    }
                }
            }
            ("start" | "typing", other) => {
                problems.push(format!("{key}: expected a mode, not {}", other.kind()))
            }
            ("pass", other) => problems.push(format!(
                "pass: expected a list of modes, not {}",
                other.kind()
            )),
            (_, Value::Table(table)) => {
                let bindings = bindings(key, table, commands, &mut problems);
                if key == ALWAYS {
                    settings.always = bindings;
                } else if is_mode_name(key) {
                    settings.modes.insert(key.clone(), bindings);
                } else {
                    problems.push(format!(
                        "[keymap.modal.{key}]: a mode's name is 1 to 32 lower-case letters, \
                         digits and dashes"
                    ));
                }
            }
            (_, other) => problems.push(format!(
                "{key}: expected start, typing, pass or a mode's table [keymap.modal.{key}], \
                 not {}",
                other.kind()
            )),
        }
    }
    match start {
        Some(start) => settings.start = start,
        None => problems.push("start: missing: the mode the keymap starts in".into()),
    }
    settings.check(&mut problems);
    (settings, problems)
}

fn is_mode_name(name: &str) -> bool {
    let valid = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
    (1..=32).contains(&name.len()) && name.chars().all(valid)
}

/// The bindings of the mode table `mode`.
fn bindings(
    mode: &str,
    table: &Table,
    commands: &CommandSet<'_>,
    problems: &mut Vec<String>,
) -> Vec<Binding> {
    let mut bindings = Vec::new();
    for (text, value) in table {
        let mut problem = |why: String| problems.push(format!("{mode} {text:?}: {why}"));
        let words: Vec<&str> = text.split_whitespace().collect();
        if !(1..=MAX_KEYS).contains(&words.len()) {
            problem(format!("a binding is 1 to {MAX_KEYS} keystrokes"));
            continue;
        }
        let keys: Result<Vec<Chord>, String> = words.iter().map(|word| word.parse()).collect();
        let action = match value {
            Value::String(text) => match text.strip_prefix("mode ") {
                Some(mode) => Ok(Action::Mode(mode.trim().to_owned())),
                None => commands.parse(text).map(Action::Run),
            },
            Value::Table(table) => run_then_pass(table, commands),
            other => Err(format!(
                "expected a command, \"mode <name>\" or {{ run = \"<command>\", pass = true }}, \
                 not {}",
                other.kind()
            )),
        };
        match (keys, action) {
            (Ok(keys), Ok(action)) => bindings.push(Binding {
                text: text.clone(),
                keys,
                action,
            }),
            (keys, action) => {
                keys.err()
                    .into_iter()
                    .chain(action.err())
                    .for_each(&mut problem);
            }
        }
    }
    bindings
}

/// `{ run = "<command>", pass = true }`.
fn run_then_pass(table: &Table, commands: &CommandSet<'_>) -> Result<Action, String> {
    let mut run = None;
    let mut pass = false;
    for (key, value) in table {
        match (key.as_str(), value) {
            ("run", Value::String(text)) => run = Some(commands.parse(text)?),
            ("pass", Value::Boolean(value)) => pass = *value,
            ("run" | "pass", other) => {
                return Err(format!(
                    "{key}: expected a command or true, not {}",
                    other.kind()
                ))
            }
            (other, _) => return Err(format!("{other}: unknown key (it takes run and pass)")),
        }
    }
    let run = run.ok_or("run: missing: the command to run")?;
    Ok(match pass {
        true => Action::RunThenPass(run),
        false => Action::Run(run),
    })
}

impl Settings {
    /// The checks across bindings and modes: every mode named exists, no binding is the start
    /// of another (or the same keys) where both apply, and a sequence where keys pass doesn't
    /// start with a key that types.
    fn check(&self, problems: &mut Vec<String>) {
        // A missing start is reported already.
        let start = Some(self.start.as_str()).filter(|start| !start.is_empty());
        let mut named: Vec<(String, &str)> = Vec::new();
        named.extend(start.map(|mode| ("start".into(), mode)));
        named.extend(
            self.typing
                .iter()
                .map(|mode| ("typing".into(), mode.as_str())),
        );
        named.extend(self.pass.iter().map(|mode| ("pass".into(), mode.as_str())));
        let tables = self
            .modes
            .iter()
            .map(|(mode, bindings)| (mode.as_str(), bindings));
        for (mode, bindings) in tables.chain([(ALWAYS, &self.always)]) {
            for binding in bindings {
                if let Action::Mode(target) = &binding.action {
                    named.push((format!("{mode} {:?}", binding.text), target));
                }
            }
        }
        for (what, mode) in named {
            if !self.modes.contains_key(mode) {
                let modes: Vec<&str> = self.modes.keys().map(String::as_str).collect();
                problems.push(format!(
                    "{what}: there is no mode {mode:?} (the modes are the tables: {})",
                    match modes.is_empty() {
                        true => "none".to_owned(),
                        false => modes.join(", "),
                    }
                ));
            }
        }
        conflicts(ALWAYS, &self.always, &[], problems);
        typing_starts(ALWAYS, &self.always, problems);
        for (mode, bindings) in &self.modes {
            conflicts(mode, bindings, &self.always, problems);
            if self.pass.contains(mode) {
                typing_starts(mode, bindings, problems);
            }
        }
    }

    /// The bindings that apply in `mode`, or only `always`'s while the chrome has the keyboard.
    fn applying<'a>(&'a self, mode: &str, chrome: bool) -> impl Iterator<Item = &'a Binding> {
        let mode = self
            .modes
            .get(mode)
            .filter(|_| !chrome)
            .into_iter()
            .flatten();
        let always = self.always.iter();
        always
            .filter(move |b| !(chrome && b.keys[0].types()))
            .chain(mode)
    }
}

/// Reports bindings of `mode` that are the same keys as, or the start of, another of `mode` or
/// of `always`.
fn conflicts(mode: &str, bindings: &[Binding], always: &[Binding], problems: &mut Vec<String>) {
    for (i, binding) in bindings.iter().enumerate() {
        let others = bindings[i + 1..].iter().map(|other| (mode, other));
        for (other_mode, other) in others.chain(always.iter().map(|other| (ALWAYS, other))) {
            let (short, long) = match binding.keys.len() <= other.keys.len() {
                true => ((mode, binding), (other_mode, other)),
                false => ((other_mode, other), (mode, binding)),
            };
            if long.1.keys.starts_with(&short.1.keys) {
                let what = match short.1.keys.len() == long.1.keys.len() {
                    true => "are the same keys as",
                    false => "is the start of",
                };
                problems.push(format!(
                    "{} {:?} {what} {} {:?}: a key must never wait for the next to know what \
                     it does",
                    short.0, short.1.text, long.0, long.1.text
                ));
            }
        }
    }
}

/// Reports sequences of `mode`, where unbound keys pass, that start with a key that types.
fn typing_starts(mode: &str, bindings: &[Binding], problems: &mut Vec<String>) {
    for binding in bindings
        .iter()
        .filter(|b| b.keys.len() > 1 && b.keys[0].types())
    {
        problems.push(format!(
            "{mode} {:?}: keys pass to the page here, so a sequence can't start with a key that \
             types; it would hold that key back",
            binding.text
        ));
    }
}

/// The modal keymap.
#[derive(Debug, Default)]
pub struct Modal {
    settings: Settings,
    mode: String,
    /// The keys of an unfinished sequence, at most `MAX_KEYS - 1`.
    pending: Vec<KeyEvent>,
    /// The key of the single-key command last run, which its repeats run again.
    held: Option<(KeyCode, Command)>,
    /// Whether a text field of the page had the keyboard when the page last had it.
    field: bool,
    indicator: String,
}

/// What some keys are, in the bindings that apply.
enum Found<'a> {
    /// The keys of this binding.
    Complete(&'a Binding),
    /// The start of this binding.
    Start(&'a Binding),
    /// Nothing.
    None,
}

impl Modal {
    /// What `keys` are in the bindings that apply now.
    fn find(&self, keys: &[KeyEvent], chrome: bool) -> Found<'_> {
        let fits = |binding: &&Binding| {
            binding.keys.len() >= keys.len()
                && keys
                    .iter()
                    .zip(&binding.keys)
                    .all(|(key, chord)| chord.matches(key))
        };
        let mut start = None;
        for binding in self.settings.applying(&self.mode, chrome).filter(fits) {
            if binding.keys.len() == keys.len() {
                return Found::Complete(binding);
            }
            start.get_or_insert(binding);
        }
        start.map_or(Found::None, Found::Start)
    }

    /// Shows the mode, and after it `sequence`: the keys of an unfinished one.
    fn show(&mut self, sequence: &str) {
        self.indicator = format!("{}{sequence}", self.mode.to_uppercase());
    }

    fn set_mode(&mut self, mode: &str) {
        mode.clone_into(&mut self.mode);
        self.pending.clear();
        self.show("");
    }
}

impl Keymap for Modal {
    fn configure(&mut self, settings: &Table) {
        // The kind's check accepted the table.
        let (settings, _) = read(settings, &CommandSet::ANY);
        let mode = match settings.modes.contains_key(&self.mode) {
            true => self.mode.clone(),
            false => settings.start.clone(),
        };
        self.settings = settings;
        self.held = None;
        self.set_mode(&mode);
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext) -> KeyAction {
        if key.repeat {
            return match &self.held {
                Some((code, command)) if *code == key.code => KeyAction::Run(command.clone()),
                _ => KeyAction::Take,
            };
        }
        if is_modifier(key) {
            return KeyAction::Pass;
        }
        self.held = None;
        let chrome = context.focus == KeyboardFocus::Chrome;
        let mut keys = std::mem::take(&mut self.pending);
        keys.push(key.clone());
        // Keys that continue no sequence drop it, and the last starts afresh.
        let mut found = self.find(&keys, chrome);
        if matches!(found, Found::None) && keys.len() > 1 {
            keys.drain(..keys.len() - 1);
            found = self.find(&keys, chrome);
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
            Some(Action::Run(command)) => {
                if keys.len() == 1 {
                    self.held = Some((key.code, command.clone()));
                }
                KeyAction::Run(command)
            }
            Some(Action::RunThenPass(command)) => KeyAction::RunThenPass(command),
            Some(Action::Mode(mode)) => {
                self.set_mode(&mode);
                KeyAction::Take
            }
            None if chrome || self.settings.pass.contains(&self.mode) => KeyAction::Pass,
            // Unbound keys that would type are swallowed here; the others pass.
            None if key.text.is_some() => KeyAction::Take,
            None => KeyAction::Pass,
        }
    }

    fn context(&mut self, context: KeyContext) {
        self.pending.clear();
        if context.focus == KeyboardFocus::Page {
            let mode = match (&self.settings.typing, context.typing, self.field) {
                (Some(typing), true, false) => Some(typing.clone()),
                (Some(_), false, true) => Some(self.settings.start.clone()),
                _ => None,
            };
            self.field = context.typing;
            if let Some(mode) = mode {
                self.mode = mode;
            }
        }
        self.show("");
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.held = None;
        self.show("");
    }

    fn indicator(&self) -> Option<&str> {
        Some(&self.indicator)
    }
}
