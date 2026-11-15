//! Keys: the key engine ([`Engine`]), which is driven by a `sighurt_api::keymap::Keys` value
//! and nothing else; the configuration's key tables (`[keys]`, `[tui.keys]`) read as the
//! operations that change such a value ([`Bindings`]); and the core's record of the presses
//! taken ([`KeyState`]).
//!
//! The engine keeps the tables, the current mode, an unfinished sequence with where it began
//! (the table, the shown tab and its committed address), and which single-key binding the key
//! now held ran. For a fresh press it looks the keys up in the line's table while the chrome
//! has the keyboard, else in the current mode's (`Keys::resolve`: the typing rule, then the
//! most specific binding the keys could be); a complete binding acts, a start waits (no
//! timeout, ever), and nothing, or a tombstone, falls to the table's `unmatched` (in the line
//! always "pass"). A key that continues no sequence drops it and starts afresh; a sequence is
//! dropped, and the key starts afresh, when the table, the shown tab or its committed address
//! changed since its first key. Modifier and dead keys are looked up alone and never join,
//! continue or drop a sequence. A repeat of a taken press runs again only if, looked up alone
//! now, it is the single-key binding its press ran, and that binding is a plain `Run` with no
//! `input.mode` in it. The indicator is the mode's, then the unfinished sequence's keys as the
//! bindings spell them, never event text.
//!
//! `[keys]` binds in every mode and the line, `[keys.page]` in every mode, `[keys.location]` in
//! the line, `[keys.url."<pattern>"]` (up to [`MAX_SITES`]) in every mode on those addresses;
//! `[tui.keys]` does the same in the terminal only. Each entry is one `bind` on the value the
//! configuration starts from, applied after it, in that order.
//!
//! [`KeyState`] keeps the guarantees of `sighurt_api::keymap` for any keymap: a press taken
//! also takes its release and its auto-repeats, so the page or the address bar never sees half a
//! keystroke; only a fresh press asks the keymap, so a key held down before a modifier joined it
//! keeps repeating where its press went.
//!
//! Must not own: what a command does ([`crate::commands`]) or where the keyboard is
//! ([`crate::state`]).

use std::collections::HashSet;
use std::fmt;

use sighurt_api::commands::{Command, CommandSet};
use sighurt_api::ids::SeatId;
use sighurt_api::input::{ButtonState, KeyCode, KeyEvent, KeyboardFocus};
use sighurt_api::keymap::{
    is_modifier, Action, Binding, Browser, Chord, Fallback, Found, Frontend, KeyAction,
    KeyBinding, KeyContext, KeySeq, KeyTable, Keymap, KeymapKind, Keys, Outcome, Situation,
    UrlPattern, When,
};
use sighurt_api::settings::{Table, Value};

/// The `bindings` keymap: no experience at all (`Keys::plain`), and the key tables over it.
pub const KIND: KeymapKind = KeymapKind {
    name: "bindings",
    check,
    new,
};

fn check(table: &Table, commands: &CommandSet<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    Tables::from_table(table, "keys", commands, &mut problems);
    problems
}

/// The two layers of a terminal `bindings` table, highest priority first. A checked `[keys]`
/// table cannot have these names: they are unknown contexts there.
pub(crate) const LAYER_TUI: &str = "tui";
/// The user's `[keys]`, under [`LAYER_TUI`].
pub(crate) const LAYER_USER: &str = "user";

/// One key table, or the two layers when both have bindings. An empty layer is left out, so
/// a single table stays a single table.
pub(crate) fn combine(tui: Table, user: Table) -> Table {
    if tui.is_empty() || user.is_empty() {
        return [tui, user]
            .into_iter()
            .find(|table| !table.is_empty())
            .unwrap_or_default();
    }
    Table::from([
        (LAYER_TUI.to_owned(), Value::Table(tui)),
        (LAYER_USER.to_owned(), Value::Table(user)),
    ])
}

/// The most `[keys.url."<pattern>"]` tables in one key table.
pub const MAX_SITES: usize = 64;

fn new() -> Box<dyn Keymap> {
    Box::new(Bindings::default())
}

/// The `bindings` keymap: `Keys::plain` and the key tables it is configured with.
#[derive(Clone, Debug)]
pub struct Bindings {
    keymap: Configured,
    /// Whether its table has a `[tui.keys]` layer: it runs in the terminal.
    terminal: bool,
}

impl Default for Bindings {
    fn default() -> Self {
        Self {
            keymap: Configured::new(Keys::plain()),
            terminal: false,
        }
    }
}

impl PartialEq for Bindings {
    fn eq(&self, other: &Self) -> bool {
        self.keymap.engine.keys() == other.keymap.engine.keys()
    }
}

impl Keymap for Bindings {
    fn configure(&mut self, settings: &Table) {
        self.terminal = layered(settings).is_some();
        self.keymap.configure(settings);
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> KeyAction {
        self.keymap.press(key, context, browser)
    }

    fn context(&mut self, context: KeyContext) {
        self.keymap.context(context);
    }

    fn reset(&mut self) {
        self.keymap.reset();
    }

    fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        self.keymap.describe(frontend)
    }
}

/// Where one entry of a key table binds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// Every mode and the line: `[keys]`.
    Everywhere,
    /// Every mode: `[keys.page]`, `[keys.url."<pattern>"]`.
    Modes,
    /// The line: `[keys.location]`.
    Line,
}

/// A key table read as the operations it stands for, in the order they apply.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Tables {
    ops: Vec<(Target, Binding)>,
}

impl Tables {
    /// Reads a key table (`section` is `keys` or `tui.keys`), with its commands parsed by
    /// `commands`, adding what is wrong with it to `problems`.
    pub(crate) fn from_table(
        keys: &Table,
        section: &str,
        commands: &CommandSet<'_>,
        problems: &mut Vec<String>,
    ) -> Self {
        let mut global = Vec::new();
        let mut page = Vec::new();
        let mut location = Vec::new();
        let mut sites = Vec::new();
        let head = format!("[{section}]");
        for (key, value) in keys {
            match value {
                Value::String(command) => add(
                    &mut global,
                    &head,
                    key,
                    command,
                    None,
                    commands,
                    problems,
                ),
                Value::Table(table) if key == "url" => {
                    sites = read_sites(table, section, commands, problems);
                }
                Value::Table(table) => {
                    let nested = format!("[{section}.{key}]");
                    let list = match key.as_str() {
                        "page" => &mut page,
                        "location" => &mut location,
                        other => {
                            problems.push(format!(
                                "[{section}.{other}]: unknown context (use [{section}.page], \
                                 [{section}.location] or [{section}.url.\"<address>\"])"
                            ));
                            continue;
                        }
                    };
                    for (key, value) in table {
                        match value.as_str() {
                            Some(command) => {
                                add(list, &nested, key, command, None, commands, problems)
                            }
                            None => problems
                                .push(format!("{nested} {key:?}: expected a command in quotes")),
                        }
                    }
                }
                _ => problems.push(format!(
                    "[{section}] {key:?}: expected a command in quotes, or a [{section}.page] or \
                     [{section}.location] table"
                )),
            }
        }
        let ops = (global.into_iter().map(|b| (Target::Everywhere, b)))
            .chain(page.into_iter().map(|b| (Target::Modes, b)))
            .chain(location.into_iter().map(|b| (Target::Line, b)))
            .chain(sites.into_iter().map(|b| (Target::Modes, b)));
        Self { ops: ops.collect() }
    }

    /// The bindings `settings` describes: one key table, or `[tui.keys]` over `[keys]`, the
    /// terminal's applying in the terminal only.
    fn from_settings(settings: &Table) -> Self {
        let mut problems = Vec::new();
        let bindings = match layered(settings) {
            Some([tui, user]) => {
                let mut all = Self::from_table(user, "keys", &CommandSet::ANY, &mut problems);
                let tui = Self::from_table(tui, "tui.keys", &CommandSet::ANY, &mut problems);
                all.ops.extend(tui.ops.into_iter().map(|(target, mut binding)| {
                    binding.when.frontend = Some(Frontend::Terminal);
                    (target, binding)
                }));
                all
            }
            None => Self::from_table(settings, "keys", &CommandSet::ANY, &mut problems),
        };
        // Each layer was checked when the configuration loaded.
        debug_assert!(problems.is_empty(), "{problems:?}");
        bindings
    }

    /// Binds each entry in `keys`, in order.
    pub(crate) fn apply(&self, keys: &mut Keys) {
        for (target, binding) in &self.ops {
            if matches!(target, Target::Everywhere | Target::Line) {
                sighurt_api::keymap::bind(&mut keys.line, binding.clone());
            }
            if matches!(target, Target::Everywhere | Target::Modes) {
                for mode in &mut keys.modes {
                    sighurt_api::keymap::bind(&mut mode.bindings, binding.clone());
                }
            }
        }
    }
}

/// The two layers, when `settings` is the table [`combine`] builds for them.
fn layered(settings: &Table) -> Option<[&Table; 2]> {
    if settings.len() != 2 {
        return None;
    }
    Some([
        settings.get(LAYER_TUI)?.as_table()?,
        settings.get(LAYER_USER)?.as_table()?,
    ])
}

/// `[<section>.url]`: a table of key bindings per address pattern, at most [`MAX_SITES`], no
/// two with the same pattern.
fn read_sites(
    table: &Table,
    section: &str,
    commands: &CommandSet<'_>,
    problems: &mut Vec<String>,
) -> Vec<Binding> {
    if table.len() > MAX_SITES {
        problems.push(format!(
            "[{section}.url] has {} addresses; at most {MAX_SITES}",
            table.len()
        ));
    }
    let mut patterns: Vec<UrlPattern> = Vec::new();
    let mut found = Vec::new();
    for (text, value) in table {
        let head = format!("[{section}.url.{text:?}]");
        let pattern = match text.parse::<UrlPattern>() {
            Ok(pattern) => pattern,
            Err(error) => {
                problems.push(format!("{head}: {error}"));
                continue;
            }
        };
        if patterns.contains(&pattern) {
            problems.push(format!(
                "{head}: another [{section}.url] table has the same address pattern ({pattern})"
            ));
            continue;
        }
        patterns.push(pattern.clone());
        let Some(keys) = value.as_table() else {
            problems.push(format!(
                "[{section}.url] {text:?}: expected a table of keys, written {head}"
            ));
            continue;
        };
        let mut list = Vec::new();
        for (key, value) in keys {
            match value.as_str() {
                Some(command) => add(
                    &mut list,
                    &head,
                    key,
                    command,
                    Some(&pattern),
                    commands,
                    problems,
                ),
                None => problems.push(format!("{head} {key:?}: expected a command in quotes")),
            }
        }
        found.extend(list);
    }
    found
}

/// Parses one binding into `list`, for the addresses `url` matches, or says what is wrong with
/// it.
fn add(
    list: &mut Vec<Binding>,
    section: &str,
    text: &str,
    command: &str,
    url: Option<&UrlPattern>,
    commands: &CommandSet<'_>,
    problems: &mut Vec<String>,
) {
    let chord = match text.parse::<Chord>() {
        Ok(chord) => chord,
        Err(error) => return problems.push(format!("{section} {text:?}: {error}")),
    };
    let command = match commands.parse(command) {
        Ok(command) => command,
        Err(error) => return problems.push(format!("{section} {text:?} = {command:?}: {error}")),
    };
    let keys = KeySeq::from(chord.clone());
    if let Some(same) = list.iter().find(|binding| binding.keys == keys) {
        return problems.push(format!(
            "{section} {text:?} and {:?} are the same keystroke ({chord})",
            same.keys.to_string()
        ));
    }
    list.push(Binding {
        keys: text.parse().expect("one keystroke is a sequence"),
        when: When {
            url: url.cloned(),
            ..When::default()
        },
        action: Action::Run(vec![command]),
    });
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
    /// The shown tab's index in the tab strip, and its committed address.
    tab: usize,
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
    /// to, and whether that was a custom action's.
    fn decide(
        &mut self,
        key: &KeyEvent,
        context: KeyContext,
        browser: &dyn Browser,
    ) -> (Outcome, bool) {
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
                _ => (self.unmatched(place, key, browser), false),
            };
        }
        self.held = None;
        let tab_id = browser.active_tab();
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
                (Outcome::take(), false)
            }
            Found::Unbound(_) | Found::None => (self.unmatched(place, key, browser), false),
        };
        self.show();
        outcome
    }

    /// A fresh press in `context`; `browser` is what the browser shows before it.
    pub fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> Outcome {
        self.decide(key, context, browser).0
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

/// Whether a held key repeats `binding`: a plain `Run` of commands with no `input.mode`.
fn repeats(binding: &Binding) -> bool {
    match &binding.action {
        Action::Run(commands) => {
            !commands.is_empty()
                && !commands
                    .iter()
                    .any(|command| matches!(command, Command::InputMode(_)))
        }
        _ => false,
    }
}

/// What `binding` does now, and whether it was a custom action.
fn act(binding: &Binding, browser: &dyn Browser) -> (Outcome, bool) {
    match &binding.action {
        Action::Run(commands) => (Outcome::run(commands.clone()), false),
        Action::RunThenPass(commands) => (
            Outcome {
                commands: commands.clone(),
                pass: true,
            },
            false,
        ),
        Action::Custom(custom) => ((custom.run)(browser), true),
        Action::Unbound => (Outcome::take(), false),
    }
}

/// The engine behind the `Keymap` trait, for a configuration that starts from `base` and
/// applies the key tables it is configured with over it.
#[derive(Clone, Debug)]
pub struct Configured {
    base: Keys,
    engine: Engine,
}

impl Configured {
    /// The engine for `base` and no key table.
    pub fn new(base: Keys) -> Self {
        Self {
            engine: Engine::new(base.clone()),
            base,
        }
    }

    /// The engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

impl Keymap for Configured {
    fn configure(&mut self, settings: &Table) {
        let mut keys = self.base.clone();
        Tables::from_settings(settings).apply(&mut keys);
        self.engine.configure(keys);
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> KeyAction {
        if key.repeat {
            return match self.engine.repeat(key, context, browser) {
                Some(mut commands) if commands.len() == 1 => {
                    KeyAction::Run(commands.remove(0))
                }
                Some(commands) => KeyAction::RunAll(commands),
                None => KeyAction::Take,
            };
        }
        let (outcome, custom) = self.engine.decide(key, context, browser);
        let switches = !outcome.commands.is_empty()
            && outcome
                .commands
                .iter()
                .all(|command| matches!(command, Command::InputMode(_)));
        let mut rest = Vec::new();
        for command in outcome.commands {
            match command {
                Command::InputMode(name) => _ = self.engine.set_mode(&name),
                command => rest.push(command),
            }
        }
        match (outcome.pass, rest.len()) {
            (true, 0) => KeyAction::Pass,
            (false, 0) if switches || !custom => KeyAction::Take,
            (true, 1) => KeyAction::RunThenPass(rest.remove(0)),
            (false, 1) if !custom => KeyAction::Run(rest.remove(0)),
            (_, _) => KeyAction::RunAll(rest),
        }
    }

    fn context(&mut self, _context: KeyContext) {
        self.engine.context_changed();
    }

    fn reset(&mut self) {
        self.engine.reset();
    }

    fn indicator(&self) -> Option<&str> {
        self.engine.indicator()
    }

    fn page_takes_text(&self) -> bool {
        self.engine.page_text()
    }

    fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        self.engine.describe(frontend)
    }
}

/// What to do with one key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// Run the commands, in order. The key goes nowhere else.
    Run(Vec<Command>),
    /// Run the command, then deliver the key to whatever has focus after it.
    RunThenDeliver(Command),
    /// A press the keymap took, or the release or auto-repeat of one: drop it.
    Consumed,
    /// Not the browser's: deliver it to whatever has focus.
    Deliver,
}

/// The keymap in use, and the presses taken whose releases the core owes nobody, per seat.
pub(crate) struct KeyState {
    kind: KeymapKind,
    keymap: Box<dyn Keymap>,
    /// The context the keymap was last told of.
    context: Option<KeyContext>,
    taken: HashSet<(SeatId, KeyCode)>,
}

impl fmt::Debug for KeyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyState")
            .field("keymap", &self.kind)
            .field("context", &self.context)
            .field("taken", &self.taken)
            .finish()
    }
}

impl KeyState {
    /// A new keymap of `kind`, configured with `settings`.
    pub(crate) fn new(kind: KeymapKind, settings: &Table) -> Self {
        let mut keymap = (kind.new)();
        keymap.configure(settings);
        Self {
            kind,
            keymap,
            context: None,
            taken: HashSet::new(),
        }
    }

    /// Applies a reloaded configuration: a keymap of the same kind is configured again and
    /// keeps its state; another kind replaces it. Taken presses stay taken either way.
    pub(crate) fn configure(&mut self, kind: KeymapKind, settings: &Table) {
        if kind != self.kind {
            self.kind = kind;
            self.keymap = (kind.new)();
            self.context = None;
        }
        self.keymap.configure(settings);
    }

    /// Decides what happens to `event`, a key event from `seat` in `context`. `browser` is what
    /// the browser shows before this press.
    pub(crate) fn handle(
        &mut self,
        context: KeyContext,
        seat: SeatId,
        event: &KeyEvent,
        browser: &dyn Browser,
    ) -> KeyOutcome {
        let key = (seat, event.code);
        match event.state {
            ButtonState::Released if self.taken.remove(&key) => KeyOutcome::Consumed,
            ButtonState::Released => KeyOutcome::Deliver,
            // Passed, or held since before (R held, then Ctrl): only a fresh press asks the
            // keymap, so its repeats and release go where its press went.
            ButtonState::Pressed if event.repeat && !self.taken.contains(&key) => {
                KeyOutcome::Deliver
            }
            // A repeat of a press the keymap took may run again, but never passes. `Pass`
            // counts as `Take`, `RunThenPass` as `Run`, and an empty `RunAll` as `Take`.
            ButtonState::Pressed if event.repeat => {
                match self.keymap.press(event, context, browser) {
                    KeyAction::Run(command) | KeyAction::RunThenPass(command) => {
                        KeyOutcome::Run(vec![command])
                    }
                    KeyAction::RunAll(commands) if !commands.is_empty() => {
                        KeyOutcome::Run(commands)
                    }
                    KeyAction::Pass | KeyAction::Take | KeyAction::RunAll(_) => {
                        KeyOutcome::Consumed
                    }
                }
            }
            ButtonState::Pressed => match self.keymap.press(event, context, browser) {
                KeyAction::Pass => {
                    self.taken.remove(&key);
                    KeyOutcome::Deliver
                }
                KeyAction::Take => {
                    self.taken.insert(key);
                    KeyOutcome::Consumed
                }
                KeyAction::Run(command) => {
                    self.taken.insert(key);
                    KeyOutcome::Run(vec![command])
                }
                KeyAction::RunThenPass(command) => {
                    self.taken.remove(&key);
                    KeyOutcome::RunThenDeliver(command)
                }
                // Empty is `Take`: the key is consumed and nothing runs.
                KeyAction::RunAll(commands) if commands.is_empty() => {
                    self.taken.insert(key);
                    KeyOutcome::Consumed
                }
                KeyAction::RunAll(commands) => {
                    self.taken.insert(key);
                    KeyOutcome::Run(commands)
                }
            },
        }
    }

    /// Takes `code`'s press on `seat` for the core, as if the keymap had: its repeats and
    /// release go nowhere.
    pub(crate) fn take(&mut self, seat: SeatId, code: KeyCode) {
        self.taken.insert((seat, code));
    }

    /// Tells the keymap the context, if it changed since it was last told.
    pub(crate) fn set_context(&mut self, context: KeyContext) {
        if self.context != Some(context) {
            self.context = Some(context);
            self.keymap.context(context);
        }
    }

    /// The window lost the keyboard: no releases will come for what is held, and the keymap
    /// drops what it waits for.
    pub(crate) fn forget(&mut self, seat: SeatId) {
        self.taken.retain(|(held, _)| *held != seat);
        self.keymap.reset();
    }

    /// What the keymap says about itself, for the chrome.
    pub(crate) fn indicator(&self) -> Option<&str> {
        self.keymap.indicator()
    }

    /// Whether the keymap lets text that is not a key press reach the page now.
    pub(crate) fn page_takes_text(&self) -> bool {
        self.keymap.page_takes_text()
    }

    /// The keymap's bindings in `frontend` ([`Keymap::describe`]).
    pub(crate) fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        self.keymap.describe(frontend)
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

    fn bindings(text: &str) -> Result<Bindings, Vec<String>> {
        let mut problems = Vec::new();
        let commands = CommandSet::new("standard", &[]);
        let read = Tables::from_table(&table(text), "keys", &commands, &mut problems);
        let mut keys = Keys::plain();
        read.apply(&mut keys);
        let mut bindings = Bindings::default();
        bindings.keymap.engine.configure(keys);
        problems.is_empty().then_some(bindings).ok_or(problems)
    }

    impl Bindings {
        /// What `event` runs while `focus` has the keyboard, typing or not, on `address`, as
        /// the engine looks it up (`Keys::resolve`): in the terminal when the table has a
        /// `[tui.keys]` layer. The `bindings` keymap's own matcher had this interface.
        fn find(
            &self,
            focus: KeyboardFocus,
            typing: bool,
            address: Option<&str>,
            event: &KeyEvent,
        ) -> Option<&Command> {
            let keys = self.keymap.engine.keys();
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
            match keys.resolve(table, std::slice::from_ref(event), &situation) {
                Found::Complete(Binding {
                    action: Action::Run(commands),
                    ..
                }) => commands.first(),
                _ => None,
            }
        }
    }

    /// The `bindings` keymap with `[keys]` written as `text`.
    fn keys(text: &str) -> KeyState {
        assert!(bindings(text).is_ok());
        KeyState::new(KIND, &table(text))
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
            "not supported",
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
            KeyAction::Run(Command::PageBack)
        );
        for browser in [
            at(Some("https://example.com/")),
            at(None),
            UrlBrowser {
                url: Some("https://example.com/"),
                link: Some("sig://terminal"),
            },
        ] {
            assert_eq!(keymap.press(&ctrl_escape, PAGE, &browser), KeyAction::Pass);
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
            KeyAction::Run(Command::BrowserQuit)
        );
        assert_eq!(
            keymap.press(
                &press(KEY_A, character("a"), NONE, NONE),
                PAGE,
                &NoneBrowser
            ),
            KeyAction::Pass
        );
    }

    #[test]
    fn consumed_presses_consume_their_releases() {
        let mut keys = keys(r#""ctrl-a" = "tab.new""#);
        let seat = SeatId::next();
        let mut handle = |event: &KeyEvent| keys.handle(PAGE, seat, event, &NoneBrowser);
        let down = press(KEY_A, character("a"), CTRL, NONE);
        let repeat = KeyEvent {
            repeat: true,
            ..down.clone()
        };
        let up = KeyEvent {
            state: ButtonState::Released,
            ..down.clone()
        };
        assert_eq!(handle(&down), KeyOutcome::Run(vec![Command::TabNew(None)]));
        assert_eq!(
            handle(&repeat),
            KeyOutcome::Run(vec![Command::TabNew(None)])
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
        let mut handle = |event: &KeyEvent| keys.handle(PAGE, seat, event, &NoneBrowser);
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
        assert_eq!(handle(&ctrl_r), KeyOutcome::Run(vec![Command::PageReload]));
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
        keys.handle(PAGE, seat, &down, &NoneBrowser);
        keys.forget(seat);
        assert_eq!(
            keys.handle(PAGE, seat, &up, &NoneBrowser),
            KeyOutcome::Deliver
        );
    }

    /// A keymap with no table: `a` runs two commands, `e` runs none.
    struct Fixed;

    impl Keymap for Fixed {
        fn configure(&mut self, _: &Table) {}

        fn press(&mut self, key: &KeyEvent, _: KeyContext, _: &dyn Browser) -> KeyAction {
            match &key.key {
                Key::Character(text) if text == "a" => {
                    KeyAction::RunAll(vec![Command::TabNew(None), Command::PageReload])
                }
                Key::Character(text) if text == "e" => KeyAction::RunAll(Vec::new()),
                _ => KeyAction::Pass,
            }
        }
    }

    #[test]
    fn run_all_runs_again_on_repeat_and_an_empty_one_is_taken() {
        let kind = KeymapKind {
            name: "fixed",
            check: |_, _| Vec::new(),
            new: || Box::new(Fixed),
        };
        let mut keys = KeyState::new(kind, &Table::new());
        let seat = SeatId::next();
        let mut handle = |event: &KeyEvent| keys.handle(PAGE, seat, event, &NoneBrowser);
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
        assert_eq!(handle(&down), KeyOutcome::Run(commands.clone()));
        assert_eq!(handle(&repeat), KeyOutcome::Run(commands));
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
