//! Keys: the key engine ([`Engine`]), which is driven by a `sighurt_api::keymap::Keys` value
//! and nothing else; the configuration's key tables (`[keys]`, `[tui.keys]`) read as the
//! operations that change such a value ([`Tables`]); and the core's record of the presses
//! taken ([`KeyState`]).
//!
//! The engine keeps the tables, the current mode, an unfinished sequence with where it began
//! (the table, the shown tab by its `TabId` and its committed address), and which single-key
//! binding the key now held ran. For a fresh press it looks the keys up in the line's table while the
//! chrome has the keyboard, else in the current mode's (`Keys::resolve`: the typing rule, then
//! the most specific binding the keys could be); a complete binding acts, a start waits (no
//! timeout, ever), and nothing, or a tombstone, falls to the table's `unmatched` (in the line
//! always "pass"). A key that continues no sequence drops it and starts afresh; a sequence is
//! dropped, and the key starts afresh, when the table, the shown tab or its committed address
//! changed since its first key, and on `input.mode`, a context change, a reset or a reload. The
//! core also drops it at once when what is shown changes ([`Engine::shown_changed`]: another
//! tab, or another view or document in the tab), so a sequence belongs to one document of one
//! tab: closing a tab and leaving and coming back end it, as a reload does.
//! Modifier and dead keys are looked up alone and never join, continue or drop a sequence. A
//! repeat of a taken press runs again only if, looked up alone now, it is the single-key binding
//! its press ran, and that binding is a plain `Run` with no `input.mode` in it, the commands of
//! `[commands]` aliases included ([`KeyState`] looks through them). The indicator is
//! the mode's, then the unfinished sequence's keys as the bindings spell them, never event
//! text. A reload keeps the mode if the new keys have it, else their initial one.
//!
//! Each entry of a key table is one `bind` (a command in quotes) or `unbind` (`false`) on the
//! keys the configuration starts from, in this order: `[keys]` (every mode and the line),
//! `[keys.page]` (every mode), `[keys.location]` (the line), `[keys.mode.<name>]` (that mode),
//! `[keys.url."<pattern>"]` (every mode, on those addresses), `[keys.mode.<name>.url."…"]`
//! (that mode, on those addresses); `[tui.keys]` and its tables do the same in the terminal
//! only, after them. Keys may be sequences; within one table a key that is, or starts, another
//! is a problem.
//!
//! [`KeyState`] keeps the guarantees of `sighurt_api::keymap`: a press taken also takes its
//! release and its auto-repeats, so the page or the address bar never sees half a keystroke;
//! only a fresh press asks the engine, so a key held down before a modifier joined it keeps
//! repeating where its press went.
//!
//! Must not own: what a command does ([`crate::commands`]) or where the keyboard is
//! ([`crate::state`]).

use std::collections::{BTreeMap, HashSet};

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::ids::{DocumentGeneration, SeatId, TabId, ViewId};
use sighurt_api::input::{ButtonState, KeyCode, KeyEvent, KeyboardFocus};
use sighurt_api::keymap::{
    is_modifier, Action, Binding, Browser, Fallback, Found, Frontend, KeyBinding, KeyContext,
    KeySeq, KeyTable, Keys, Outcome, Situation, UrlPattern, When,
};
use sighurt_api::settings::{Table, Value};

/// The most `[keys.url."<pattern>"]` tables in one key table (and in each
/// `[keys.mode.<name>.url]`).
pub const MAX_SITES: usize = 64;

/// Where one entry of a key table binds.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Target {
    /// Every mode and the line: `[keys]`.
    Everywhere,
    /// Every mode: `[keys.page]`, `[keys.url."<pattern>"]`.
    Modes,
    /// The line: `[keys.location]`.
    Line,
    /// One mode: `[keys.mode.<name>]`, `[keys.mode.<name>.url."<pattern>"]`.
    Mode(String),
}

/// One entry: its keys and condition, and the action, or `None` to unbind (`false`).
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    keys: KeySeq,
    when: When,
    action: Option<Action>,
}

/// A key table of the configuration (`[keys]` or `[tui.keys]`, with the tables under it) read
/// as the operations it stands for, in the order they apply: `[keys]`, `[keys.page]`,
/// `[keys.location]`, each `[keys.mode.<name>]`, each `[keys.url."<pattern>"]`, then each
/// `[keys.mode.<name>.url."<pattern>"]`. A value is a command in quotes, or `false` to
/// unbind; keys may be sequences. Within one table, a key that is another, or the start of
/// another, is a problem (a TOML table has no order).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Tables {
    ops: Vec<(Target, Entry)>,
}

impl Tables {
    /// Reads a key table (`section` is `keys` or `tui.keys`), with its commands parsed by
    /// `commands`, its bindings applying only in `frontend` if it names one, adding what is
    /// wrong with it to `problems`.
    pub(crate) fn from_table(
        keys: &Table,
        section: &str,
        commands: &CommandSet<'_>,
        frontend: Option<Frontend>,
        problems: &mut Vec<String>,
    ) -> Self {
        let mut reader = Reader {
            commands,
            frontend,
            problems,
        };
        let when = |url: Option<UrlPattern>| When {
            frontend,
            url,
            field: None,
        };
        let mut global = Vec::new();
        let mut page = Vec::new();
        let mut location = Vec::new();
        let mut modes = Vec::new();
        let mut sites = Vec::new();
        let mut mode_sites = Vec::new();
        let head = format!("[{section}]");
        for (key, value) in keys {
            match (key.as_str(), value) {
                ("url", Value::Table(table)) => {
                    sites = reader.sites(table, &format!("{section}.url"));
                }
                ("mode", Value::Table(table)) => {
                    for (name, value) in table {
                        let nested = format!("[{section}.mode.{name}]");
                        let Some(table) = value.as_table() else {
                            reader.problems.push(format!(
                                "[{section}.mode] {name:?}: expected a table of keys, written \
                                 {nested}"
                            ));
                            continue;
                        };
                        let mut list = Vec::new();
                        for (key, value) in table {
                            match (key.as_str(), value) {
                                ("url", Value::Table(urls)) => {
                                    let found =
                                        reader.sites(urls, &format!("{section}.mode.{name}.url"));
                                    mode_sites.push((name.clone(), found));
                                }
                                _ => reader.entry(&mut list, &nested, key, value, when(None)),
                            }
                        }
                        modes.push((name.clone(), list));
                    }
                }
                ("page" | "location", Value::Table(table)) => {
                    let nested = format!("[{section}.{key}]");
                    let list = match key.as_str() {
                        "page" => &mut page,
                        _ => &mut location,
                    };
                    for (key, value) in table {
                        reader.entry(list, &nested, key, value, when(None));
                    }
                }
                (other, Value::Table(_)) => reader.problems.push(format!(
                    "[{section}.{other}]: unknown context (use [{section}.page], \
                     [{section}.location], [{section}.mode.<name>] or \
                     [{section}.url.\"<address>\"])"
                )),
                (key, value) => reader.entry(&mut global, &head, key, value, when(None)),
            }
        }
        let mut ops = Vec::new();
        ops.extend(global.into_iter().map(|e| (Target::Everywhere, e)));
        ops.extend(page.into_iter().map(|e| (Target::Modes, e)));
        ops.extend(location.into_iter().map(|e| (Target::Line, e)));
        for (name, list) in modes {
            ops.extend(list.into_iter().map(|e| (Target::Mode(name.clone()), e)));
        }
        ops.extend(sites.into_iter().map(|e| (Target::Modes, e)));
        for (name, list) in mode_sites {
            ops.extend(list.into_iter().map(|e| (Target::Mode(name.clone()), e)));
        }
        Self { ops }
    }

    /// Binds and unbinds each entry in `keys`, in order. A mode `keys` has none of is a
    /// problem.
    pub(crate) fn apply(&self, keys: &mut Keys, problems: &mut Vec<String>) {
        for (target, entry) in &self.ops {
            let apply = |bindings: &mut Vec<Binding>| match &entry.action {
                Some(action) => sighurt_api::keymap::bind(
                    bindings,
                    Binding {
                        keys: entry.keys.clone(),
                        when: entry.when.clone(),
                        action: action.clone(),
                    },
                ),
                None => {
                    sighurt_api::keymap::unbind(bindings, entry.keys.clone(), entry.when.clone())
                }
            };
            match target {
                Target::Line => apply(&mut keys.line),
                Target::Everywhere => {
                    apply(&mut keys.line);
                    keys.modes
                        .iter_mut()
                        .for_each(|mode| apply(&mut mode.bindings));
                }
                Target::Modes => keys
                    .modes
                    .iter_mut()
                    .for_each(|mode| apply(&mut mode.bindings)),
                Target::Mode(name) => match keys.modes.iter_mut().find(|m| m.name == *name) {
                    Some(mode) => apply(&mut mode.bindings),
                    None => {
                        let names: Vec<&str> = keys.modes.iter().map(|m| m.name.as_str()).collect();
                        let problem = format!(
                            "[keys.mode.{name}]: there is no mode {name:?} (the modes are {})",
                            names.join(", ")
                        );
                        if !problems.contains(&problem) {
                            problems.push(problem);
                        }
                    }
                },
            }
        }
    }
}

/// What reading a key table needs at hand.
struct Reader<'a, 'p> {
    commands: &'a CommandSet<'a>,
    frontend: Option<Frontend>,
    problems: &'p mut Vec<String>,
}

impl Reader<'_, '_> {
    /// `[<section>]`: a table of key tables per address pattern, at most [`MAX_SITES`], no two
    /// with the same pattern.
    fn sites(&mut self, table: &Table, section: &str) -> Vec<Entry> {
        if table.len() > MAX_SITES {
            self.problems.push(format!(
                "[{section}] has {} addresses; at most {MAX_SITES}",
                table.len()
            ));
        }
        let mut patterns: Vec<UrlPattern> = Vec::new();
        let mut found = Vec::new();
        for (text, value) in table {
            let head = format!("[{section}.{text:?}]");
            let pattern = match text.parse::<UrlPattern>() {
                Ok(pattern) => pattern,
                Err(error) => {
                    self.problems.push(format!("{head}: {error}"));
                    continue;
                }
            };
            if patterns.contains(&pattern) {
                self.problems.push(format!(
                    "{head}: another [{section}] table has the same address pattern ({pattern})"
                ));
                continue;
            }
            patterns.push(pattern.clone());
            let Some(keys) = value.as_table() else {
                self.problems.push(format!(
                    "[{section}] {text:?}: expected a table of keys, written {head}"
                ));
                continue;
            };
            let when = When {
                frontend: self.frontend,
                url: Some(pattern),
                field: None,
            };
            let mut list = Vec::new();
            for (key, value) in keys {
                self.entry(&mut list, &head, key, value, when.clone());
            }
            found.extend(list);
        }
        found
    }

    /// Reads one entry into `list`, or says what is wrong with it.
    fn entry(
        &mut self,
        list: &mut Vec<Entry>,
        section: &str,
        text: &str,
        value: &Value,
        when: When,
    ) {
        let keys = match text.parse::<KeySeq>() {
            Ok(keys) => keys,
            Err(error) => return self.problems.push(format!("{section} {text:?}: {error}")),
        };
        let action = match value {
            Value::String(command) => match self.commands.parse(command) {
                Ok(command) => Some(Action::Run(vec![command])),
                Err(error) => {
                    return self
                        .problems
                        .push(format!("{section} {text:?} = {command:?}: {error}"))
                }
            },
            Value::Boolean(false) => None,
            _ => {
                return self.problems.push(format!(
                    "{section} {text:?}: expected a command in quotes, or false to unbind the key"
                ))
            }
        };
        if let Some(same) = list.iter().find(|entry| entry.keys == keys) {
            let chords: Vec<String> = keys.chords().iter().map(ToString::to_string).collect();
            let what = match chords.len() {
                1 => "keystroke",
                _ => "keys",
            };
            return self.problems.push(format!(
                "{section} {text:?} and {:?} are the same {what} ({})",
                same.keys.to_string(),
                chords.join(" ")
            ));
        }
        let start = list
            .iter()
            .find(|entry| entry.keys.starts_with(&keys) || keys.starts_with(&entry.keys));
        if let Some(other) = start {
            let (short, long) = match keys.chords().len() < other.keys.chords().len() {
                true => (text.to_owned(), other.keys.to_string()),
                false => (other.keys.to_string(), text.to_owned()),
            };
            return self.problems.push(format!(
                "{section} {short:?} is the start of {long:?}: one table can't bind both"
            ));
        }
        list.push(Entry { keys, when, action });
    }
}

/// A table of the engine's [`Keys`]: the line's, or a mode's by its index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    Line,
    Mode(usize),
}

/// An unfinished sequence, and where it began.
#[derive(Clone, Debug)]
struct Pending {
    /// The keys typed so far, at most `MAX_KEYS - 1`.
    keys: Vec<KeyEvent>,
    /// The keys as the binding they start spells them, for the indicator.
    shown: String,
    place: Place,
    /// The shown tab, and its committed address.
    tab: Option<TabId>,
    address: Option<String>,
}

/// The single-key binding a held key's press ran.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Held {
    code: KeyCode,
    place: Place,
    index: usize,
}

/// The key engine: one per browser, configured by a [`Keys`] value.
#[derive(Clone, Debug)]
pub struct Engine {
    keys: Keys,
    /// The index of the current mode.
    mode: usize,
    pending: Option<Pending>,
    held: Option<Held>,
    indicator: String,
}

impl Engine {
    /// An engine for `keys`, in their initial mode. The keys are those `Keys::check` accepted.
    pub fn new(keys: Keys) -> Self {
        let mut engine = Self {
            mode: index_of(&keys, &keys.initial),
            keys,
            pending: None,
            held: None,
            indicator: String::new(),
        };
        engine.show();
        engine
    }

    /// The keys it uses.
    pub fn keys(&self) -> &Keys {
        &self.keys
    }

    /// Takes `keys` into use, keeping the mode if they have one of its name, else starting in
    /// their initial mode. An unfinished sequence and a held key's binding are dropped.
    pub fn configure(&mut self, keys: Keys) {
        let name = self.mode_name().to_owned();
        self.mode = match keys.modes.iter().position(|mode| mode.name == name) {
            Some(index) => index,
            None => index_of(&keys, &keys.initial),
        };
        self.keys = keys;
        self.pending = None;
        self.held = None;
        self.show();
    }

    /// The current mode's name.
    pub fn mode_name(&self) -> &str {
        &self.keys.modes[self.mode].name
    }

    /// Switches to the mode `name`, dropping an unfinished sequence even when it is the mode
    /// already set. `false`, changing nothing, when there is no such mode.
    pub fn set_mode(&mut self, name: &str) -> bool {
        let Some(index) = self.keys.modes.iter().position(|mode| mode.name == name) else {
            return false;
        };
        self.mode = index;
        self.pending = None;
        self.show();
        true
    }

    /// Whether the current mode lets text that is not a key press reach the page.
    pub fn page_text(&self) -> bool {
        self.keys.modes[self.mode].page_text
    }

    /// What the chrome shows: the mode's indicator, then an unfinished sequence's keys.
    pub fn indicator(&self) -> Option<&str> {
        Some(self.indicator.as_str()).filter(|text| !text.is_empty())
    }

    /// The context changed between presses: an unfinished sequence is dropped.
    pub fn context_changed(&mut self) {
        if self.pending.take().is_some() {
            self.show();
        }
    }

    /// What is shown changed between presses (another tab, or another view or document in the
    /// shown tab): an unfinished sequence is dropped.
    pub fn shown_changed(&mut self) {
        self.context_changed();
    }

    /// The window lost the keyboard: an unfinished sequence and a held key's binding are
    /// dropped.
    pub fn reset(&mut self) {
        self.pending = None;
        self.held = None;
        self.show();
    }

    /// The bindings that exist in `frontend` (`Keys::describe`).
    pub fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        self.keys.describe(frontend)
    }

    fn show(&mut self) {
        let mode = &self.keys.modes[self.mode].indicator;
        let keys = self.pending.as_ref().map_or("", |p| p.shown.as_str());
        self.indicator = match (mode.is_empty(), keys.is_empty()) {
            (_, true) => mode.clone(),
            (true, false) => keys.to_owned(),
            (false, false) => format!("{mode} {keys}"),
        };
    }

    fn place(&self, context: KeyContext) -> Place {
        match context.focus {
            KeyboardFocus::Chrome => Place::Line,
            KeyboardFocus::Page => Place::Mode(self.mode),
        }
    }

    fn table(&self, place: Place) -> (KeyTable<'_>, &[Binding]) {
        match place {
            Place::Line => (KeyTable::Line, &self.keys.line),
            Place::Mode(index) => {
                let mode = &self.keys.modes[index];
                (KeyTable::Mode(&mode.name), &mode.bindings)
            }
        }
    }

    /// What `typed` is in `place`, and the binding's index in its table.
    fn find<'a>(
        &'a self,
        place: Place,
        typed: &[KeyEvent],
        situation: &Situation<'_>,
    ) -> (Found<'a>, Option<usize>) {
        let (table, bindings) = self.table(place);
        let found = self.keys.resolve(table, typed, situation);
        let index = match found {
            Found::Complete(b) | Found::Start(b) | Found::Unbound(b) => {
                bindings.iter().position(|other| std::ptr::eq(other, b))
            }
            Found::None => None,
        };
        (found, index)
    }

    /// What a key no binding has does in `place`.
    fn unmatched(&self, place: Place, key: &KeyEvent, browser: &dyn Browser) -> Outcome {
        match place {
            Place::Line => Outcome::pass(),
            Place::Mode(index) => match &self.keys.modes[index].unmatched {
                Fallback::Pass => Outcome::pass(),
                Fallback::Take => Outcome::take(),
                Fallback::Custom(decide) => decide(key, browser),
            },
        }
    }

    /// A fresh press in `context`; `browser` is what the browser shows before it. What it comes
    /// to: commands to run, and whether the key then passes.
    pub fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> Outcome {
        let tab = browser.tab(browser.active_tab());
        let address = tab.and_then(|tab| tab.url);
        let situation = Situation {
            frontend: browser.frontend(),
            context,
            address,
        };
        let place = self.place(context);
        // Looked up alone; they never join, continue or drop a sequence.
        if is_modifier(key) {
            let (found, index) = self.find(place, std::slice::from_ref(key), &situation);
            return match (found, index) {
                (Found::Complete(binding), Some(index)) => {
                    let binding = binding.clone();
                    self.held = repeats(&binding).then_some(Held {
                        code: key.code,
                        place,
                        index,
                    });
                    act(&binding, browser)
                }
                _ => self.unmatched(place, key, browser),
            };
        }
        self.held = None;
        let tab_id = tab.map(|tab| tab.id);
        // A sequence belongs to where it began.
        let mut typed = match self.pending.take() {
            Some(p) if p.place == place && p.tab == tab_id && p.address.as_deref() == address => {
                p.keys
            }
            _ => Vec::new(),
        };
        typed.push(key.clone());
        let (mut found, mut index) = self.find(place, &typed, &situation);
        // Keys that continue no sequence drop it, and the last starts afresh.
        if matches!(found, Found::None | Found::Unbound(_)) && typed.len() > 1 {
            typed.drain(..typed.len() - 1);
            (found, index) = self.find(place, &typed, &situation);
        }
        let outcome = match found {
            Found::Complete(binding) => {
                let binding = binding.clone();
                if let (1, Some(index)) = (typed.len(), index) {
                    self.held = repeats(&binding).then_some(Held {
                        code: key.code,
                        place,
                        index,
                    });
                }
                act(&binding, browser)
            }
            Found::Start(binding) => {
                let shown = binding.keys.written(typed.len());
                self.pending = Some(Pending {
                    keys: typed,
                    shown,
                    place,
                    tab: tab_id,
                    address: address.map(str::to_owned),
                });
                Outcome::take()
            }
            Found::Unbound(_) | Found::None => self.unmatched(place, key, browser),
        };
        self.show();
        outcome
    }

    /// An auto-repeat of a press that was taken: the commands to run again, if, looked up alone
    /// now, it is the single-key binding its press ran and that binding is a plain `Run` with no
    /// `input.mode`. Otherwise it is held quietly.
    pub fn repeat(
        &self,
        key: &KeyEvent,
        context: KeyContext,
        browser: &dyn Browser,
    ) -> Option<Vec<Command>> {
        let held = self.held.filter(|held| held.code == key.code)?;
        let place = self.place(context);
        if place != held.place {
            return None;
        }
        let tab = browser.tab(browser.active_tab());
        let situation = Situation {
            frontend: browser.frontend(),
            context,
            address: tab.and_then(|tab| tab.url),
        };
        match self.find(place, std::slice::from_ref(key), &situation) {
            (Found::Complete(binding), Some(index)) if index == held.index && repeats(binding) => {
                match &binding.action {
                    Action::Run(commands) => Some(commands.clone()),
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// The index of the mode `name` in `keys`, or the first.
fn index_of(keys: &Keys, name: &str) -> usize {
    keys.modes
        .iter()
        .position(|mode| mode.name == name)
        .unwrap_or(0)
}

/// Whether a held key repeats `binding`: a plain `Run` of commands with no `input.mode` (the
/// core looks through `[commands]` aliases too, [`KeyState::handle`]).
fn repeats(binding: &Binding) -> bool {
    match &binding.action {
        Action::Run(commands) => !commands.is_empty() && !commands.iter().any(is_mode_switch),
        _ => false,
    }
}

fn is_mode_switch(command: &Command) -> bool {
    matches!(command, Command::InputMode(_))
}

/// What `binding` does now.
fn act(binding: &Binding, browser: &dyn Browser) -> Outcome {
    match &binding.action {
        Action::Run(commands) => Outcome::run(commands.clone()),
        Action::RunThenPass(commands) => Outcome {
            commands: commands.clone(),
            pass: true,
        },
        Action::Custom(custom) => (custom.run)(browser),
        // `Keys::resolve` never completes on a tombstone.
        Action::Unbound => Outcome::take(),
    }
}

/// What to do with one key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// Run the commands, in order (there is at least one); then, with `pass`, deliver the key
    /// to whatever has the keyboard after them, as a key no binding took is delivered.
    Run {
        /// The commands.
        commands: Vec<Command>,
        /// Whether the key is delivered after them.
        pass: bool,
    },
    /// A press that was taken, or the release or auto-repeat of one: drop it.
    Consumed,
    /// Not the browser's: deliver it to whatever has focus.
    Deliver,
}

/// What is shown: the shown tab, and the view and document it shows.
pub(crate) type Shown = (Option<TabId>, Option<ViewId>, Option<DocumentGeneration>);

/// The key engine, and the presses taken whose releases the core owes nobody, per seat.
#[derive(Debug)]
pub(crate) struct KeyState {
    engine: Engine,
    /// The context the engine was last told of.
    context: Option<KeyContext>,
    /// What the engine was last told is shown.
    shown: Option<Shown>,
    taken: HashSet<(SeatId, KeyCode)>,
}

impl KeyState {
    /// The engine for `keys`, in their initial mode.
    pub(crate) fn new(keys: Keys) -> Self {
        Self {
            engine: Engine::new(keys),
            context: None,
            shown: None,
            taken: HashSet::new(),
        }
    }

    /// Takes reloaded keys into use: the mode stays if they have it (`Engine::configure`).
    /// Taken presses stay taken.
    pub(crate) fn configure(&mut self, keys: Keys) {
        self.engine.configure(keys);
    }

    /// Decides what happens to `event`, a key event from `seat` in `context`. `browser` is what
    /// the browser shows before this press; `aliases` are the configuration's `[commands]`,
    /// which a repeat looks through: a repeat whose binding runs `input.mode`, itself or through
    /// an alias, runs nothing.
    pub(crate) fn handle(
        &mut self,
        context: KeyContext,
        seat: SeatId,
        event: &KeyEvent,
        browser: &dyn Browser,
        aliases: &BTreeMap<String, Vec<Command>>,
    ) -> KeyOutcome {
        let key = (seat, event.code);
        match event.state {
            ButtonState::Released if self.taken.remove(&key) => return KeyOutcome::Consumed,
            ButtonState::Released => return KeyOutcome::Deliver,
            // Passed, or held since before (R held, then Ctrl): only a fresh press asks the
            // keymap, so its repeats and release go where its press went.
            ButtonState::Pressed if event.repeat && !self.taken.contains(&key) => {
                return KeyOutcome::Deliver
            }
            // A repeat of a press that was taken may run again, but never passes.
            ButtonState::Pressed if event.repeat => {
                let commands = self.engine.repeat(event, context, browser);
                let switches = |commands: &Vec<Command>| {
                    commands.iter().any(|command| match command {
                        Command::Custom(name) => aliases
                            .get(name)
                            .is_some_and(|run| run.iter().any(is_mode_switch)),
                        command => is_mode_switch(command),
                    })
                };
                return match commands.filter(|commands| !commands.is_empty() && !switches(commands))
                {
                    Some(commands) => KeyOutcome::Run {
                        commands,
                        pass: false,
                    },
                    None => KeyOutcome::Consumed,
                };
            }
            ButtonState::Pressed => {}
        }
        let outcome = self.engine.press(event, context, browser);
        match outcome.pass {
            true => _ = self.taken.remove(&key),
            false => _ = self.taken.insert(key),
        }
        match (outcome.commands.is_empty(), outcome.pass) {
            (true, true) => KeyOutcome::Deliver,
            (true, false) => KeyOutcome::Consumed,
            (false, pass) => KeyOutcome::Run {
                commands: outcome.commands,
                pass,
            },
        }
    }

    /// Takes `code`'s press on `seat` for the core, as if the keymap had: its repeats and
    /// release go nowhere.
    pub(crate) fn take(&mut self, seat: SeatId, code: KeyCode) {
        self.taken.insert((seat, code));
    }

    /// `input.mode <name>`: switches the engine's mode. `false`, changing nothing, when there
    /// is no such mode.
    pub(crate) fn set_mode(&mut self, name: &str) -> bool {
        self.engine.set_mode(name)
    }

    /// Tells the keymap the context, if it changed since it was last told.
    pub(crate) fn set_context(&mut self, context: KeyContext) {
        if self.context != Some(context) {
            self.context = Some(context);
            self.engine.context_changed();
        }
    }

    /// Tells the engine what is shown, if that changed since it was last told: an unfinished
    /// sequence ends with the tab, view or document it began in.
    pub(crate) fn set_shown(&mut self, shown: Shown) {
        if self.shown != Some(shown) {
            self.shown = Some(shown);
            self.engine.shown_changed();
        }
    }

    /// The window lost the keyboard: no releases will come for what is held, and the keymap
    /// drops what it waits for.
    pub(crate) fn forget(&mut self, seat: SeatId) {
        self.taken.retain(|(held, _)| *held != seat);
        self.engine.reset();
    }

    /// What the keymap says about itself, for the chrome.
    pub(crate) fn indicator(&self) -> Option<&str> {
        self.engine.indicator()
    }

    /// Whether the keymap lets text that is not a key press reach the page now.
    pub(crate) fn page_takes_text(&self) -> bool {
        self.engine.page_text()
    }

    /// The engine's bindings in `frontend` (`Keys::describe`).
    pub(crate) fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        self.engine.describe(frontend)
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::input::{Key, Modifiers};

    use super::*;

    const NONE: Modifiers = Modifiers {
        shift: false,
        control: false,
        alt: false,
        alt_graph: false,
        logo: false,
        caps_lock: false,
        num_lock: false,
    };
    const CTRL: Modifiers = Modifiers {
        control: true,
        ..NONE
    };

    /// A press of the key with evdev `code` that means `key`, as xkb would report it.
    fn press(code: u32, key: Key, modifiers: Modifiers, consumed: Modifiers) -> KeyEvent {
        KeyEvent {
            code: KeyCode(code),
            key,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers,
            consumed,
            text: None,
        }
    }

    fn character(text: &str) -> Key {
        Key::Character(text.into())
    }

    const KEY_Q: u32 = 16;
    const KEY_R: u32 = 19;
    const KEY_A: u32 = 30;

    /// `[keys]` written as `text`, as settings.
    fn table(text: &str) -> Table {
        let mut problems = Vec::new();
        let table = crate::config::settings(text.parse().unwrap(), "[keys]", &mut problems);
        assert_eq!(problems, Vec::<String>::new());
        table
    }

    /// `[keys]` written as `text`, read over no experience: the keys, or every problem.
    fn bindings(text: &str) -> Result<Bindings, Vec<String>> {
        let mut problems = Vec::new();
        let commands = CommandSet::new("standard", &[]);
        let read = Tables::from_table(&table(text), "keys", &commands, None, &mut problems);
        let mut bindings = Bindings::default();
        read.apply(&mut bindings.keys, &mut problems);
        problems.is_empty().then_some(bindings).ok_or(problems)
    }

    /// `[tui.keys]` and `[keys]` together, as one value for [`Bindings::configure`].
    fn combine(tui: Table, user: Table) -> (Table, Table) {
        (tui, user)
    }

    /// What these tests drive: `Keys::plain` with key tables applied over it, as
    /// `[input] keymap = "bindings"` makes it, looked up as the engine looks keys up.
    #[derive(Debug, PartialEq)]
    struct Bindings {
        keys: Keys,
        /// Whether a `[tui.keys]` layer was read: lookups are in the terminal.
        terminal: bool,
    }

    impl Default for Bindings {
        fn default() -> Self {
            Self {
                keys: Keys::plain(),
                terminal: false,
            }
        }
    }

    /// A key table, or `[tui.keys]` with `[keys]`.
    trait Layers {
        fn layers(&self) -> (Option<&Table>, &Table);
    }

    impl Layers for Table {
        fn layers(&self) -> (Option<&Table>, &Table) {
            (None, self)
        }
    }

    impl Layers for (Table, Table) {
        fn layers(&self) -> (Option<&Table>, &Table) {
            (Some(&self.0), &self.1)
        }
    }

    impl Bindings {
        fn configure(&mut self, settings: &impl Layers) {
            let (tui, user) = settings.layers();
            let mut problems = Vec::new();
            let mut keys = Keys::plain();
            Tables::from_table(user, "keys", &CommandSet::ANY, None, &mut problems)
                .apply(&mut keys, &mut problems);
            if let Some(tui) = tui {
                let terminal = Some(Frontend::Terminal);
                Tables::from_table(tui, "tui.keys", &CommandSet::ANY, terminal, &mut problems)
                    .apply(&mut keys, &mut problems);
            }
            assert_eq!(problems, Vec::<String>::new());
            self.keys = keys;
            self.terminal = tui.is_some();
        }

        /// What `event` comes to, pressed afresh with the page's keyboard.
        fn press(&self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> Outcome {
            Engine::new(self.keys.clone()).press(key, context, browser)
        }

        fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
            self.keys.describe(frontend)
        }

        /// What `event` runs while `focus` has the keyboard, typing or not, on `address`, as
        /// the engine looks it up (`Keys::resolve`): in the terminal when a `[tui.keys]` layer
        /// was read. The `bindings` keymap's own matcher had this interface.
        fn find(
            &self,
            focus: KeyboardFocus,
            typing: bool,
            address: Option<&str>,
            event: &KeyEvent,
        ) -> Option<&Command> {
            let table = match focus {
                KeyboardFocus::Chrome => KeyTable::Line,
                KeyboardFocus::Page => KeyTable::Mode(sighurt_api::keymap::PLAIN_MODE),
            };
            let situation = Situation {
                frontend: match self.terminal {
                    true => Frontend::Terminal,
                    false => Frontend::Gui,
                },
                context: KeyContext::new(focus, typing),
                address,
            };
            match self
                .keys
                .resolve(table, std::slice::from_ref(event), &situation)
            {
                Found::Complete(Binding {
                    action: Action::Run(commands),
                    ..
                }) => commands.first(),
                _ => None,
            }
        }
    }

    /// The key state with `[keys]` written as `text`.
    fn keys(text: &str) -> KeyState {
        KeyState::new(bindings(text).unwrap().keys)
    }

    const PAGE: KeyContext = KeyContext::new(KeyboardFocus::Page, false);

    #[test]
    fn context_bindings_win_and_typing_keeps_characters() {
        let bindings = bindings(
            r#"
            "ctrl-l" = "location.focus"
            "j" = "tab.next"
            [page]
            "ctrl-l" = "page.reload"
            [location]
            "alt-enter" = "page.open https://example.com"
            "#,
        )
        .unwrap();
        let ctrl_l = press(38, character("l"), CTRL, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, None, &ctrl_l),
            Some(&Command::PageReload)
        );
        assert_eq!(
            bindings.find(KeyboardFocus::Chrome, true, None, &ctrl_l),
            Some(&Command::LocationFocus(None))
        );
        let j = press(36, character("j"), NONE, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, None, &j),
            Some(&Command::TabNext)
        );
        assert_eq!(bindings.find(KeyboardFocus::Page, true, None, &j), None);
        assert_eq!(bindings.find(KeyboardFocus::Chrome, true, None, &j), None);
    }

    #[test]
    fn a_higher_layer_wins_and_typing_does_not_hide_other_keys() {
        // [tui.keys] over [keys].
        let settings = combine(
            table(
                r#"
                "j" = "tab.close"
                "ctrl-t" = "browser.quit"
                "#,
            ),
            table(
                r#"
                "j" = "tab.next"
                "q" = "browser.quit"
                "ctrl-d" = "page.reload"
                [page]
                "ctrl-t" = "tab.new"
                "#,
            ),
        );
        let mut bindings = Bindings::default();
        bindings.configure(&settings);
        let j = press(36, character("j"), NONE, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, None, &j),
            Some(&Command::TabClose(None))
        );
        // j would type, so it is left alone, in every layer.
        assert_eq!(bindings.find(KeyboardFocus::Page, true, None, &j), None);
        assert_eq!(bindings.find(KeyboardFocus::Chrome, true, None, &j), None);
        // A modifier in a lower layer still fires while a character binding is suppressed.
        let ctrl_d = press(32, character("d"), CTRL, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, true, None, &ctrl_d),
            Some(&Command::PageReload)
        );
        // A global binding in the higher layer beats a page binding under it.
        let ctrl_t = press(20, character("t"), CTRL, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, None, &ctrl_t),
            Some(&Command::BrowserQuit)
        );
        let q = press(KEY_Q, character("q"), NONE, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, None, &q),
            Some(&Command::BrowserQuit)
        );
    }

    #[test]
    fn bad_bindings_are_reported() {
        let problems = bindings(
            r#"
            "ctrl-t" = "tab.nwe"
            "ctrl-T" = "tab.new"
            "ctrl-shift-t" = "tab.close"
            "g g" = "location.focus"
            "g" = "tab.next"
            "ctrl-q" = "chrome.toggle-tabs"
            "x" = 1
            [sidebar]
            "a" = "tab.new"
            "#,
        )
        .unwrap_err();
        let expected = [
            "unknown command \"tab.nwe\"",
            "are the same keystroke",
            // Sequences are keys since the key engine (experience-2026-11-15.md, §1.5); in one
            // table a key can't be the start of another.
            "\"g\" is the start of \"g g\"",
            "the standard chrome has no command chrome.toggle-tabs",
            "expected a command",
            "unknown context",
        ];
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
        for part in expected {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} not in {problems:#?}"
            );
        }
    }

    const ESCAPE: u32 = 1;

    fn escape(modifiers: Modifiers) -> KeyEvent {
        press(ESCAPE, Key::Named("Escape".into()), modifiers, NONE)
    }

    #[test]
    fn site_tables_win_in_the_page_on_their_addresses() {
        let bindings = bindings(
            r#"
            "ctrl-escape" = "tab.new"
            "f12" = "browser.quit"
            [page]
            "ctrl-escape" = "page.reload"
            [url."sig://terminal"]
            "ctrl-escape" = "page.back"
            "j" = "tab.next"
            [url."https://*.example.com"]
            "ctrl-escape" = "page.forward"
            [url."https://docs.example.com/guide"]
            "ctrl-escape" = "page.stop"
            "#,
        )
        .unwrap();
        let ctrl_escape = escape(CTRL);
        let find = |focus, address| bindings.find(focus, false, address, &ctrl_escape);
        let page = KeyboardFocus::Page;
        // The site's binding beats [keys.page] and [keys] on its addresses only.
        for address in ["sig://terminal", "sig://terminal/config", "SIG://TERMINAL"] {
            assert_eq!(
                find(page, Some(address)),
                Some(&Command::PageBack),
                "{address}"
            );
        }
        for address in [
            Some("sig://terminal-x"),
            Some("https://sig/terminal"),
            Some("sig://config"),
            None,
        ] {
            assert_eq!(
                find(page, address),
                Some(&Command::PageReload),
                "{address:?}"
            );
        }
        // The most specific table that matches, and only it.
        assert_eq!(
            find(page, Some("https://docs.example.com/guide/x")),
            Some(&Command::PageStop)
        );
        assert_eq!(
            find(page, Some("https://docs.example.com/guidance")),
            Some(&Command::PageForward)
        );
        assert_eq!(
            find(page, Some("https://a.b.example.com/")),
            Some(&Command::PageForward)
        );
        assert_eq!(
            find(page, Some("https://example.com/")),
            Some(&Command::PageReload)
        );
        // A key the site's table doesn't bind falls to [keys.page] and [keys].
        let f12 = press(88, Key::Named("F12".into()), NONE, NONE);
        assert_eq!(
            bindings.find(page, false, Some("sig://terminal"), &f12),
            Some(&Command::BrowserQuit)
        );
        // Only while the page has the keyboard.
        assert_eq!(
            find(KeyboardFocus::Chrome, Some("sig://terminal")),
            Some(&Command::TabNew(None))
        );
        // A keystroke that types is left to a text field there too.
        let j = press(36, character("j"), NONE, NONE);
        assert_eq!(
            bindings.find(page, false, Some("sig://terminal"), &j),
            Some(&Command::TabNext)
        );
        assert_eq!(bindings.find(page, true, Some("sig://terminal"), &j), None);
    }

    #[test]
    fn the_keymap_takes_the_address_from_the_shown_tabs_committed_one() {
        let text = "[url.\"sig://terminal\"]\n\"ctrl-escape\" = \"page.back\"";
        let mut keymap = Bindings::default();
        keymap.configure(&table(text));
        let ctrl_escape = escape(CTRL);
        let at = |url| UrlBrowser { url, link: None };
        assert_eq!(
            keymap.press(&ctrl_escape, PAGE, &at(Some("sig://terminal"))),
            Outcome::run(vec![Command::PageBack])
        );
        for browser in [
            at(Some("https://example.com/")),
            at(None),
            UrlBrowser {
                url: Some("https://example.com/"),
                link: Some("sig://terminal"),
            },
        ] {
            assert_eq!(keymap.press(&ctrl_escape, PAGE, &browser), Outcome::pass());
        }
        // Listed with its address.
        assert_eq!(
            keymap.describe(Frontend::Gui),
            [KeyBinding::new(
                "",
                "ctrl-escape",
                "page.back",
                "on sig://terminal"
            )]
        );
    }

    #[test]
    fn a_terminal_layer_has_site_tables_of_its_own() {
        let settings = combine(
            table("[url.\"sig://terminal\"]\n\"f12\" = \"tab.close\""),
            table(
                "\"f12\" = \"browser.quit\"\n[url.\"sig://terminal\"]\n\"f12\" = \
                 \"page.back\"\n\"f11\" = \"page.forward\"",
            ),
        );
        let mut bindings = Bindings::default();
        bindings.configure(&settings);
        let key = |name: &str, code| press(code, Key::Named(name.into()), NONE, NONE);
        let find =
            |event: &KeyEvent, address| bindings.find(KeyboardFocus::Page, false, address, event);
        assert_eq!(
            find(&key("F12", 88), Some("sig://terminal")),
            Some(&Command::TabClose(None))
        );
        assert_eq!(find(&key("F12", 88), None), Some(&Command::BrowserQuit));
        assert_eq!(
            find(&key("F11", 87), Some("sig://terminal")),
            Some(&Command::PageForward)
        );
        let described = bindings.describe(Frontend::Terminal);
        assert_eq!(described.len(), 4);
        assert!(described.iter().all(|b| b.keys == "f12" || b.keys == "f11"));
    }

    #[test]
    fn bad_site_tables_are_reported() {
        let problems = bindings(
            r#"
            [url."terminal"]
            "f12" = "tab.new"
            [url."https://a.org:8080"]
            "f12" = "tab.new"
            [url."sig://Terminal"]
            "f12" = "tab.new"
            [url."SIG://terminal/"]
            "f12" = "tab.new"
            [url."sig://config"]
            "f12" = "tab.nwe"
            "f11" = 1
            "ctrl-escape" = "page.back"
            "ctrl-Escape" = "page.back"
            "#,
        )
        .unwrap_err();
        let expected = [
            "[keys.url.\"terminal\"]: an address pattern is scheme://host",
            "[keys.url.\"https://a.org:8080\"]: ':' in the host",
            "another [keys.url] table has the same address pattern (sig://terminal)",
            "[keys.url.\"sig://config\"] \"f12\" = \"tab.nwe\": unknown command",
            "[keys.url.\"sig://config\"] \"f11\": expected a command in quotes",
            "are the same keystroke",
        ];
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
        for part in expected {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} not in {problems:#?}"
            );
        }
        // A string where the tables are, and a table of tables.
        let wrong = bindings("url = \"sig://terminal\"").unwrap_err();
        assert!(wrong[0].contains("[keys] \"url\""), "{wrong:?}");
        let wrong = bindings("[url]\n\"sig://terminal\" = \"tab.new\"").unwrap_err();
        assert!(wrong[0].contains("expected a table of keys"), "{wrong:?}");
    }

    #[test]
    fn site_tables_are_bounded() {
        let mut text = String::new();
        for i in 0..=MAX_SITES {
            text.push_str(&format!(
                "[url.\"https://site{i}.example\"]\n\"f12\" = \"tab.new\"\n"
            ));
        }
        let problems = bindings(&text).unwrap_err();
        assert_eq!(problems, ["[keys.url] has 65 addresses; at most 64"]);
        let long = format!("https://a.org/{}", "x".repeat(2048));
        let problems = bindings(&format!("[url.{long:?}]\n\"f12\" = \"tab.new\"")).unwrap_err();
        assert!(problems[0].contains("at most 2048 bytes"), "{problems:?}");
    }

    #[test]
    fn the_keymap_reads_its_table_as_the_check_did() {
        let text = "\"ctrl-q\" = \"browser.quit\"\n[page]\n\"f5\" = \"page.reload\"";
        let mut keymap = Bindings::default();
        keymap.configure(&table(text));
        assert_eq!(keymap, bindings(text).unwrap());
        let ctrl_q = press(KEY_Q, character("q"), CTRL, NONE);
        assert_eq!(
            keymap.press(&ctrl_q, PAGE, &NoneBrowser),
            Outcome::run(vec![Command::BrowserQuit])
        );
        assert_eq!(
            keymap.press(
                &press(KEY_A, character("a"), NONE, NONE),
                PAGE,
                &NoneBrowser
            ),
            Outcome::pass()
        );
    }

    #[test]
    fn consumed_presses_consume_their_releases() {
        let mut keys = keys(r#""ctrl-a" = "tab.new""#);
        let seat = SeatId::next();
        let mut handle =
            |event: &KeyEvent| keys.handle(PAGE, seat, event, &NoneBrowser, &BTreeMap::new());
        let down = press(KEY_A, character("a"), CTRL, NONE);
        let repeat = KeyEvent {
            repeat: true,
            ..down.clone()
        };
        let up = KeyEvent {
            state: ButtonState::Released,
            ..down.clone()
        };
        assert_eq!(
            handle(&down),
            KeyOutcome::Run {
                commands: vec![Command::TabNew(None)],
                pass: false
            }
        );
        assert_eq!(
            handle(&repeat),
            KeyOutcome::Run {
                commands: vec![Command::TabNew(None)],
                pass: false
            }
        );
        // Ctrl was let go while A repeats: still the browser's.
        let bare_repeat = KeyEvent {
            modifiers: NONE,
            ..repeat.clone()
        };
        assert_eq!(handle(&bare_repeat), KeyOutcome::Consumed);
        assert_eq!(handle(&up), KeyOutcome::Consumed);
        // The next plain A is the page's, press and release.
        let plain = KeyEvent {
            modifiers: NONE,
            ..down.clone()
        };
        assert_eq!(handle(&plain), KeyOutcome::Deliver);
        assert_eq!(handle(&up), KeyOutcome::Deliver);
    }

    #[test]
    fn a_held_key_never_turns_into_a_shortcut() {
        let mut keys = keys(r#""ctrl-r" = "page.reload""#);
        let seat = SeatId::next();
        let mut handle =
            |event: &KeyEvent| keys.handle(PAGE, seat, event, &NoneBrowser, &BTreeMap::new());
        // R goes down and repeats, and Ctrl goes down while it does.
        let r = press(KEY_R, character("r"), NONE, NONE);
        assert_eq!(handle(&r), KeyOutcome::Deliver);
        let repeat = KeyEvent {
            repeat: true,
            modifiers: CTRL,
            ..r.clone()
        };
        assert_eq!(handle(&repeat), KeyOutcome::Deliver);
        let up = KeyEvent {
            state: ButtonState::Released,
            ..repeat.clone()
        };
        assert_eq!(handle(&up), KeyOutcome::Deliver);
        // Pressed afresh with Ctrl held, it is the shortcut.
        let ctrl_r = KeyEvent {
            modifiers: CTRL,
            ..r
        };
        assert_eq!(
            handle(&ctrl_r),
            KeyOutcome::Run {
                commands: vec![Command::PageReload],
                pass: false
            }
        );
    }

    #[test]
    fn losing_the_keyboard_forgets_held_keys() {
        let mut keys = keys(r#""ctrl-a" = "tab.new""#);
        let seat = SeatId::next();
        let down = press(KEY_A, character("a"), CTRL, NONE);
        let up = KeyEvent {
            state: ButtonState::Released,
            modifiers: NONE,
            ..down.clone()
        };
        keys.handle(PAGE, seat, &down, &NoneBrowser, &BTreeMap::new());
        keys.forget(seat);
        assert_eq!(
            keys.handle(PAGE, seat, &up, &NoneBrowser, &BTreeMap::new()),
            KeyOutcome::Deliver
        );
    }

    #[test]
    fn run_all_runs_again_on_repeat_and_an_empty_one_is_taken() {
        // `a` runs two commands, `e` runs none.
        let mut fixed = Keys::plain();
        let run = |keys: &str, commands: Vec<Command>| Binding {
            keys: keys.parse().unwrap(),
            when: When::default(),
            action: Action::Run(commands),
        };
        fixed.modes[0].bindings = vec![
            run("a", vec![Command::TabNew(None), Command::PageReload]),
            run("e", Vec::new()),
        ];
        let mut keys = KeyState::new(fixed);
        let seat = SeatId::next();
        let mut handle =
            |event: &KeyEvent| keys.handle(PAGE, seat, event, &NoneBrowser, &BTreeMap::new());
        let commands = vec![Command::TabNew(None), Command::PageReload];
        let down = press(KEY_A, character("a"), NONE, NONE);
        let repeat = KeyEvent {
            repeat: true,
            ..down.clone()
        };
        let up = KeyEvent {
            state: ButtonState::Released,
            ..down.clone()
        };
        assert_eq!(
            handle(&down),
            KeyOutcome::Run {
                commands: commands.clone(),
                pass: false
            }
        );
        assert_eq!(
            handle(&repeat),
            KeyOutcome::Run {
                commands,
                pass: false
            }
        );
        assert_eq!(handle(&up), KeyOutcome::Consumed);
        let empty = press(18, character("e"), NONE, NONE);
        let empty_repeat = KeyEvent {
            repeat: true,
            ..empty.clone()
        };
        let empty_up = KeyEvent {
            state: ButtonState::Released,
            ..empty.clone()
        };
        assert_eq!(handle(&empty), KeyOutcome::Consumed);
        assert_eq!(handle(&empty_repeat), KeyOutcome::Consumed);
        assert_eq!(handle(&empty_up), KeyOutcome::Consumed);
    }

    #[test]
    fn a_sequence_belongs_to_one_tab_and_what_it_shows() {
        let mut keys = Keys::plain();
        keys.modes[0].bindings = vec![Binding {
            keys: "g g".parse().unwrap(),
            when: When::default(),
            action: Action::Run(vec![Command::PageZoomIn]),
        }];
        let mut engine = Engine::new(keys);
        let g = press(34, character("g"), NONE, NONE);
        let (first, second) = (TabBrowser(TabId::next()), TabBrowser(TabId::next()));
        assert_eq!(engine.press(&g, PAGE, &first), Outcome::take());
        // Another tab in its place, at the same index and address: the sequence starts afresh.
        assert_eq!(engine.press(&g, PAGE, &second), Outcome::take());
        assert_eq!(
            engine.press(&g, PAGE, &second),
            Outcome::run(vec![Command::PageZoomIn])
        );
        // The core says what is shown changed (another document, or away and back).
        assert_eq!(engine.press(&g, PAGE, &first), Outcome::take());
        engine.shown_changed();
        assert_eq!(engine.indicator(), None);
        assert_eq!(engine.press(&g, PAGE, &first), Outcome::take());
    }

    #[test]
    fn a_held_key_never_switches_modes_again_through_an_alias() {
        let mut fixed = Keys::plain();
        fixed
            .modes
            .push(sighurt_api::keymap::Mode::new("other").indicator("OTHER"));
        fixed.modes[0].bindings = vec![
            Binding {
                keys: "a".parse().unwrap(),
                when: When::default(),
                action: Action::Run(vec![Command::Custom("other".into())]),
            },
            Binding {
                keys: "e".parse().unwrap(),
                when: When::default(),
                action: Action::Run(vec![Command::Custom("reload".into())]),
            },
        ];
        let aliases = BTreeMap::from([
            ("other".to_owned(), vec![Command::InputMode("other".into())]),
            ("reload".to_owned(), vec![Command::PageReload]),
        ]);
        let mut keys = KeyState::new(fixed);
        let seat = SeatId::next();
        let mut handle = |event: &KeyEvent| keys.handle(PAGE, seat, event, &NoneBrowser, &aliases);
        let repeat = |event: &KeyEvent| KeyEvent {
            repeat: true,
            ..event.clone()
        };
        let a = press(KEY_A, character("a"), NONE, NONE);
        let alias = vec![Command::Custom("other".into())];
        assert_eq!(
            handle(&a),
            KeyOutcome::Run {
                commands: alias,
                pass: false
            }
        );
        assert_eq!(handle(&repeat(&a)), KeyOutcome::Consumed);
        // An alias that switches no mode repeats as its commands would.
        let e = press(18, character("e"), NONE, NONE);
        let reload = KeyOutcome::Run {
            commands: vec![Command::Custom("reload".into())],
            pass: false,
        };
        assert_eq!(handle(&e), reload);
        assert_eq!(handle(&repeat(&e)), reload);
    }

    /// A browser with one tab, `0`, showing https://example.com/.
    struct TabBrowser(TabId);

    impl Browser for TabBrowser {
        fn frontend(&self) -> Frontend {
            Frontend::Gui
        }

        fn context(&self) -> KeyContext {
            PAGE
        }

        fn tab_count(&self) -> usize {
            1
        }

        fn active_tab(&self) -> usize {
            0
        }

        fn tab(&self, index: usize) -> Option<sighurt_api::keymap::TabInfo<'_>> {
            (index == 0).then(|| {
                sighurt_api::keymap::TabInfo::new(
                    self.0,
                    None,
                    Some("https://example.com/"),
                    false,
                    None,
                    1.0,
                    false,
                    false,
                    None,
                    false,
                )
            })
        }
    }

    /// A browser with one tab, showing `url` with the pointer over `link`.
    struct UrlBrowser {
        url: Option<&'static str>,
        link: Option<&'static str>,
    }

    impl Browser for UrlBrowser {
        fn frontend(&self) -> Frontend {
            Frontend::Gui
        }

        fn context(&self) -> KeyContext {
            PAGE
        }

        fn tab_count(&self) -> usize {
            1
        }

        fn active_tab(&self) -> usize {
            0
        }

        fn tab(&self, index: usize) -> Option<sighurt_api::keymap::TabInfo<'_>> {
            (index == 0).then(|| {
                sighurt_api::keymap::TabInfo::new(
                    sighurt_api::ids::TabId::next(),
                    None,
                    self.url,
                    true,
                    None,
                    1.0,
                    false,
                    false,
                    self.link,
                    false,
                )
            })
        }
    }

    /// A browser with no tab.
    struct NoneBrowser;

    impl Browser for NoneBrowser {
        fn frontend(&self) -> sighurt_api::keymap::Frontend {
            sighurt_api::keymap::Frontend::Gui
        }

        fn context(&self) -> KeyContext {
            KeyContext::new(KeyboardFocus::Page, false)
        }

        fn tab_count(&self) -> usize {
            0
        }

        fn active_tab(&self) -> usize {
            0
        }

        fn tab(&self, _: usize) -> Option<sighurt_api::keymap::TabInfo<'_>> {
            None
        }
    }
}
