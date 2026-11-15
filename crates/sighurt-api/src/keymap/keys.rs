//! The data the key engine is configured with ([`Keys`]), the operations that change it
//! ([`Keys::bind`], [`Keys::unbind`]), its validation ([`Keys::check`]), the lookup the engine
//! and the help page share ([`Keys::resolve`]) and its description ([`Keys::describe`])
//! (docs/rewrite/experience-2026-11-15.md, §1).
//!
//! **Binding is replacing.** Binding or unbinding keys in a table with a condition `W` first
//! removes from that table every binding with `W`'s address condition whose keys are the same,
//! a start of the new keys or started by them, and whose `field` and `frontend` conditions are
//! `W`'s or narrower. So no binding is ever the start of another with the same condition, a
//! later definition wins a prefix conflict, and a site's bindings survive a general override.
//!
//! **Which binding wins.** Of the bindings that apply and that the keys typed so far could be,
//! the most specific decides: the more specific address (an exact host before a wildcard, then
//! the longer path; any address before none), then `field` stated before unstated, `frontend`
//! stated before unstated, then the binding defined later. An [`Action::Unbound`] tombstone
//! masks only less specific bindings of exactly its keys: it is no start of anything and hides
//! no longer sequence.
//!
//! **The typing rule.** A binding whose first keystroke would type a character
//! ([`Chord::types`]) does not apply where typed text is going: the chrome's line, and a page
//! text field in a mode with [`Mode::page_text`].
//!
//! Must not own: the current mode, an unfinished sequence or the presses taken (the core's
//! engine), or what a command does.

use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use super::{Browser, Chord, ChordKey, Frontend, KeyBinding, KeyContext, UrlPattern};
use crate::commands::{is_mode_name, Command, CommandSet};
use crate::input::{KeyEvent, KeyboardFocus};

/// The most keystrokes in one binding.
pub const MAX_KEYS: usize = 4;
/// The most modes.
pub const MAX_MODES: usize = 16;
/// The most bindings in one table (a mode's, or the line's).
pub const MAX_BINDINGS: usize = 1024;
/// The most commands one action lists.
pub const MAX_ACTION_COMMANDS: usize = 64;
/// The most characters of a mode's indicator.
pub const MAX_INDICATOR: usize = 64;
/// The name [`KeyTable::Line`] goes by where tables are named by text (`tables(["normal",
/// "line"])`): no mode may have it.
pub const LINE: &str = "line";

/// One to [`MAX_KEYS`] keystrokes typed one after the other, such as `y y` or `ctrl-x k`. A key
/// that is a modifier, a lock or Compose is never one of several.
///
/// Compared by its keystrokes (`ctrl-T` is `ctrl-shift-t`), and printed as it was written,
/// its words separated by one space, so the help page and the indicator spell keys as the
/// configuration does.
#[derive(Clone, Debug)]
pub struct KeySeq {
    chords: Vec<Chord>,
    /// The words as written.
    text: String,
}

impl PartialEq for KeySeq {
    fn eq(&self, other: &Self) -> bool {
        self.chords == other.chords
    }
}

impl Eq for KeySeq {}

impl std::hash::Hash for KeySeq {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.chords.hash(state);
    }
}

impl KeySeq {
    /// Its keystrokes, in order; never empty.
    pub fn chords(&self) -> &[Chord] {
        &self.chords
    }

    /// Its first keystroke.
    pub fn first(&self) -> &Chord {
        &self.chords[0]
    }

    /// Whether `other`'s keystrokes are the first of these (equal keys included).
    pub fn starts_with(&self, other: &KeySeq) -> bool {
        self.chords.starts_with(&other.chords)
    }

    /// The first `n` keystrokes as written ("y" of "y y").
    pub fn written(&self, n: usize) -> String {
        let words: Vec<&str> = self.text.split(' ').take(n).collect();
        words.join(" ")
    }

    fn len(&self) -> usize {
        self.chords.len()
    }

    /// Whether one of the two is the start of the other, or they are equal.
    fn overlaps(&self, other: &KeySeq) -> bool {
        self.starts_with(other) || other.starts_with(self)
    }

    /// Whether the keys typed so far could be these: as many or fewer, each matching.
    fn could_be(&self, typed: &[KeyEvent]) -> bool {
        typed.len() <= self.chords.len()
            && typed.iter().zip(&self.chords).all(|(key, c)| c.matches(key))
    }
}

impl From<Chord> for KeySeq {
    fn from(chord: Chord) -> Self {
        Self {
            text: chord.to_string(),
            chords: vec![chord],
        }
    }
}

/// Whether `chord` names a modifier, a lock or Compose: never a key of a sequence.
fn names_modifier(chord: &Chord) -> bool {
    match &chord.key {
        ChordKey::Named(name) => super::MODIFIER_NAMES.contains(&name.as_str()) || name == "Compose",
        ChordKey::Character(_) | ChordKey::Code(_) => false,
    }
}

impl FromStr for KeySeq {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.is_empty() {
            return Err("the keys name no key".into());
        }
        if words.len() > MAX_KEYS {
            return Err(format!(
                "at most {MAX_KEYS} keystrokes in one binding, not {}",
                words.len()
            ));
        }
        let mut chords = Vec::with_capacity(words.len());
        for word in &words {
            let chord: Chord = word.parse()?;
            if words.len() > 1 && names_modifier(&chord) {
                return Err(format!(
                    "{word:?} is a modifier: it can be a binding's only key, never one of a \
                     sequence"
                ));
            }
            chords.push(chord);
        }
        Ok(Self {
            chords,
            text: words.join(" "),
        })
    }
}

impl fmt::Display for KeySeq {
    /// The keystrokes as written, separated by one space.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// When a binding applies. Every condition stated must hold; [`Default`] is "always".
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct When {
    /// Only in the window, or only in the terminal.
    pub frontend: Option<Frontend>,
    /// Only on the addresses this matches: the shown tab's committed one. It selects
    /// shortcuts and grants nothing: it ignores the port, and the address is the page's.
    pub url: Option<UrlPattern>,
    /// Only while a text field of the page has the keyboard (`true`), or only while none does
    /// (`false`). The chrome's line is no field of the page.
    pub field: Option<bool>,
}

impl When {
    /// Whether every condition stated holds in `situation`.
    pub fn holds(&self, situation: &Situation<'_>) -> bool {
        let frontend = self.frontend.is_none_or(|f| f == situation.frontend);
        let field = self.field.is_none_or(|f| f == situation.field());
        let url = match &self.url {
            None => true,
            Some(pattern) => situation.address.is_some_and(|a| pattern.matches(a)),
        };
        frontend && field && url
    }

    /// Whether a binding with `self` is among those that binding or unbinding with `wanted`
    /// replaces: the same address condition, and `field` and `frontend` the same as `wanted`'s
    /// or narrower (`wanted` leaves them unstated).
    fn replaced_by(&self, wanted: &When) -> bool {
        fn narrower<T: PartialEq>(mine: Option<T>, theirs: Option<T>) -> bool {
            theirs.is_none() || mine == theirs
        }
        self.url == wanted.url
            && narrower(self.field, wanted.field)
            && narrower(self.frontend, wanted.frontend)
    }

    /// How specific it is, the more specific the greater: the address first (any before
    /// none), then `field`, then `frontend`.
    fn specificity(&self) -> (bool, (usize, usize), bool, bool) {
        let address = self.url.as_ref().map(UrlPattern::specificity);
        (
            address.is_some(),
            address.unwrap_or_default(),
            self.field.is_some(),
            self.frontend.is_some(),
        )
    }
}

/// What a key is looked up in: the frontend, where the keyboard is and the shown tab's
/// committed address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Situation<'a> {
    /// The frontend that runs the browser.
    pub frontend: Frontend,
    /// Where the keyboard is, and whether that part takes typed text.
    pub context: KeyContext,
    /// The shown tab's committed address.
    pub address: Option<&'a str>,
}

impl Situation<'_> {
    /// Whether a text field of the page has the keyboard.
    pub fn field(&self) -> bool {
        self.context.focus == KeyboardFocus::Page && self.context.typing
    }
}

/// What a key comes to: plain data, never another computation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Run in order, in the update of the press.
    pub commands: Vec<Command>,
    /// Then the key goes to whatever has the keyboard after them.
    pub pass: bool,
}

impl Outcome {
    /// Taken, and these run.
    pub fn run(commands: Vec<Command>) -> Self {
        Self {
            commands,
            pass: false,
        }
    }

    /// Taken, and nothing runs.
    pub fn take() -> Self {
        Self::run(Vec::new())
    }

    /// Nothing runs, and the key goes to whatever has the keyboard.
    pub fn pass() -> Self {
        Self {
            commands: Vec::new(),
            pass: true,
        }
    }
}

/// The function of a [`Custom`] action.
pub type CustomFn = dyn Fn(&dyn Browser) -> Outcome + Send + Sync;

/// The function of a [`Fallback::Custom`].
pub type FallbackFn = dyn Fn(&KeyEvent, &dyn Browser) -> Outcome + Send + Sync;

/// A named function of the browser's state, called once when its keys are pressed with the
/// view of the browser before the press. It does no I/O, reads no clock, starts no thread and
/// logs nothing; what it reads of a page (titles, addresses) it passes on only as a command's
/// operand (`Command::ClipboardCopy(url)`), never as command text.
#[derive(Clone)]
pub struct Custom {
    /// What it does, for the help page ("copy the page's address").
    pub describe: String,
    /// The function.
    pub run: Arc<CustomFn>,
}

impl Custom {
    /// A custom action described as `describe`.
    pub fn new(
        describe: impl Into<String>,
        run: impl Fn(&dyn Browser) -> Outcome + Send + Sync + 'static,
    ) -> Self {
        Self {
            describe: describe.into(),
            run: Arc::new(run),
        }
    }
}

impl fmt::Debug for Custom {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Custom({:?})", self.describe)
    }
}

/// Equal when they are the same function with the same description: a configuration read
/// again from the same value is equal to it.
impl PartialEq for Custom {
    fn eq(&self, other: &Self) -> bool {
        self.describe == other.describe && Arc::ptr_eq(&self.run, &other.run)
    }
}

impl Eq for Custom {}

/// What a binding does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// The key is taken and these run in order. Empty: taken, and nothing runs.
    Run(Vec<Command>),
    /// These run, then the key goes to whatever has the keyboard now. Empty: the key passes.
    RunThenPass(Vec<Command>),
    /// Computed when the key is pressed, from what the browser shows before it.
    Custom(Custom),
    /// A tombstone: nothing is bound to exactly these keys here, and a less specific binding
    /// of exactly these keys does not apply either. The key falls to the table's `unmatched`.
    Unbound,
}

impl Action {
    /// The commands it lists, if it lists any.
    fn commands(&self) -> &[Command] {
        match self {
            Self::Run(commands) | Self::RunThenPass(commands) => commands,
            Self::Custom(_) | Self::Unbound => &[],
        }
    }
}

/// What a mode does with a key no binding has, while the page has the keyboard.
#[derive(Clone)]
pub enum Fallback {
    /// The key goes to the page.
    Pass,
    /// The key is taken and does nothing.
    Take,
    /// Decided per key, as a [`Custom`] action is, seeing the key.
    Custom(Arc<FallbackFn>),
}

impl fmt::Debug for Fallback {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pass => "Pass",
            Self::Take => "Take",
            Self::Custom(_) => "Custom",
        })
    }
}

impl PartialEq for Fallback {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pass, Self::Pass) | (Self::Take, Self::Take) => true,
            (Self::Custom(a), Self::Custom(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for Fallback {}

/// One binding: keys, when they apply, and what they do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    /// Its keystrokes.
    pub keys: KeySeq,
    /// When it applies.
    pub when: When,
    /// What it does.
    pub action: Action,
}

/// A mode: a table of bindings for while the page has the keyboard, and what happens to the
/// rest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mode {
    /// Its name ("normal"): 1 to 32 lower-case letters, digits and dashes, not [`LINE`].
    pub name: String,
    /// What the chrome shows in this mode; may be empty. At most [`MAX_INDICATOR`]
    /// characters on one line.
    pub indicator: String,
    /// Whether text that is not a key press (an input method's, a paste) may reach the page,
    /// and the page's fields are offered to the input method.
    pub page_text: bool,
    /// What a key no binding has does while the page has the keyboard.
    pub unmatched: Fallback,
    /// Its bindings, in the order they were defined.
    pub bindings: Vec<Binding>,
}

impl Mode {
    /// A mode called `name` with no indicator and no binding, that gives the page its keys and
    /// text.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            indicator: String::new(),
            page_text: true,
            unmatched: Fallback::Pass,
            bindings: Vec::new(),
        }
    }

    /// This mode, showing `indicator`.
    pub fn indicator(mut self, indicator: impl Into<String>) -> Self {
        self.indicator = indicator.into();
        self
    }

    /// This mode, giving the page text or not.
    pub fn page_text(mut self, page_text: bool) -> Self {
        self.page_text = page_text;
        self
    }

    /// This mode, doing `unmatched` with a key no binding has.
    pub fn unmatched(mut self, unmatched: Fallback) -> Self {
        self.unmatched = unmatched;
        self
    }
}

/// A table of [`Keys`]: the line's, or a mode's by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyTable<'a> {
    /// [`Keys::line`].
    Line,
    /// The bindings of the mode named.
    Mode(&'a str),
}

impl fmt::Display for KeyTable<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Line => f.write_str("the line"),
            Self::Mode(name) => write!(f, "mode {name}"),
        }
    }
}

/// Everything the key engine is configured with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keys {
    /// At least one, at most [`MAX_MODES`], names unique.
    pub modes: Vec<Mode>,
    /// The mode a session starts in.
    pub initial: String,
    /// The table while the chrome (the address line, the find field) has the keyboard:
    /// bindings only. A key no binding has is the line's own.
    pub line: Vec<Binding>,
}

impl Default for Keys {
    /// [`Keys::plain`].
    fn default() -> Self {
        Self::plain()
    }
}

/// What some keys typed so far are in a table, and which binding says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Found<'a> {
    /// The keys of this binding.
    Complete(&'a Binding),
    /// The start of this binding: the engine waits for the next key.
    Start(&'a Binding),
    /// Nothing, because this tombstone unbinds exactly these keys here.
    Unbound(&'a Binding),
    /// Nothing.
    None,
}

/// The name of the one mode of [`Keys::plain`].
pub const PLAIN_MODE: &str = "default";

impl Keys {
    /// No experience at all: one mode, [`PLAIN_MODE`], that passes every key and gives the
    /// page text, and binds nothing.
    pub fn plain() -> Self {
        Self {
            modes: vec![Mode::new(PLAIN_MODE)],
            initial: PLAIN_MODE.into(),
            line: Vec::new(),
        }
    }

    /// The mode named `name`.
    pub fn mode(&self, name: &str) -> Option<&Mode> {
        self.modes.iter().find(|mode| mode.name == name)
    }

    /// The bindings of `table`, or `None` for a mode there is none of.
    pub fn table(&self, table: KeyTable<'_>) -> Option<&[Binding]> {
        match table {
            KeyTable::Line => Some(&self.line),
            KeyTable::Mode(name) => self.mode(name).map(|mode| &mode.bindings[..]),
        }
    }

    /// The bindings of `table`, to change, or `None` for a mode there is none of.
    pub fn table_mut(&mut self, table: KeyTable<'_>) -> Option<&mut Vec<Binding>> {
        match table {
            KeyTable::Line => Some(&mut self.line),
            KeyTable::Mode(name) => self
                .modes
                .iter_mut()
                .find(|mode| mode.name == name)
                .map(|mode| &mut mode.bindings),
        }
    }

    /// Binds `binding` in `table`, replacing what it replaces (the module's rule). An error
    /// when there is no such mode.
    pub fn bind(&mut self, table: KeyTable<'_>, binding: Binding) -> Result<(), String> {
        let bindings = self
            .table_mut(table)
            .ok_or_else(|| format!("there is no {table}"))?;
        bind(bindings, binding);
        Ok(())
    }

    /// Unbinds `keys` in `table` for `when`, as [`unbind`] does. An error when there is no
    /// such mode.
    pub fn unbind(&mut self, table: KeyTable<'_>, keys: KeySeq, when: When) -> Result<(), String> {
        let bindings = self
            .table_mut(table)
            .ok_or_else(|| format!("there is no {table}"))?;
        unbind(bindings, keys, when);
        Ok(())
    }

    /// Everything wrong with these keys however they were built, one line each; empty if they
    /// can be used. Commands are parsed again with `commands` (so a `[commands]` alias the
    /// configuration lacks, or a chrome command its chrome lacks, is a problem) for the
    /// bindings that can apply in `frontend`; every `input.mode` must name a mode; no binding
    /// may be the start of another with the same condition.
    pub fn check(&self, commands: &CommandSet<'_>, frontend: Frontend) -> Vec<String> {
        let mut problems = Vec::new();
        if self.modes.is_empty() || self.modes.len() > MAX_MODES {
            problems.push(format!(
                "there are {} modes; there must be 1 to {MAX_MODES}",
                self.modes.len()
            ));
        }
        for (i, mode) in self.modes.iter().enumerate() {
            if !is_mode_name(&mode.name) || mode.name == LINE {
                problems.push(format!(
                    "{:?} is not a mode's name (1 to 32 lower-case letters, digits and dashes, \
                     and not {LINE:?})",
                    mode.name
                ));
            }
            if self.modes[..i].iter().any(|other| other.name == mode.name) {
                problems.push(format!("there are two modes called {:?}", mode.name));
            }
            let fits = mode.indicator.chars().count() <= MAX_INDICATOR;
            if !fits || mode.indicator.chars().any(char::is_control) {
                problems.push(format!(
                    "mode {}: its indicator is at most {MAX_INDICATOR} characters on one line",
                    mode.name
                ));
            }
        }
        if self.mode(&self.initial).is_none() {
            problems.push(format!(
                "the initial mode {:?} is not one of the modes",
                self.initial
            ));
        }
        let tables = std::iter::once((KeyTable::Line, &self.line)).chain(
            self.modes
                .iter()
                .map(|mode| (KeyTable::Mode(&mode.name), &mode.bindings)),
        );
        for (table, bindings) in tables {
            if bindings.len() > MAX_BINDINGS {
                problems.push(format!(
                    "{table} has {} bindings; at most {MAX_BINDINGS}",
                    bindings.len()
                ));
            }
            for (i, binding) in bindings.iter().enumerate() {
                self.check_binding(table, binding, commands, frontend, &mut problems);
                let conflict = bindings[..i].iter().find(|earlier| {
                    let tombstone = matches!(earlier.action, Action::Unbound)
                        || matches!(binding.action, Action::Unbound);
                    earlier.when == binding.when
                        && match tombstone {
                            true => earlier.keys == binding.keys,
                            false => earlier.keys.overlaps(&binding.keys),
                        }
                });
                if let Some(earlier) = conflict {
                    problems.push(format!(
                        "{table}: {:?} and {:?} have the same condition, and one is or starts \
                         the other",
                        earlier.keys.to_string(),
                        binding.keys.to_string()
                    ));
                }
            }
        }
        problems
    }

    fn check_binding(
        &self,
        table: KeyTable<'_>,
        binding: &Binding,
        commands: &CommandSet<'_>,
        frontend: Frontend,
        problems: &mut Vec<String>,
    ) {
        let keys = binding.keys.to_string();
        let listed = binding.action.commands();
        if listed.len() > MAX_ACTION_COMMANDS {
            problems.push(format!(
                "{table}: {keys:?} runs {} commands; at most {MAX_ACTION_COMMANDS}",
                listed.len()
            ));
        }
        let here = binding.when.frontend.is_none_or(|f| f == frontend);
        for command in listed {
            match command {
                Command::InputMode(name) if self.mode(name).is_none() => problems.push(format!(
                    "{table}: {keys:?} runs {command}, but there is no mode {name:?}"
                )),
                Command::InputMode(_) => {}
                // Written again and parsed as the configuration's own text would be.
                command if here => {
                    if let Err(error) = commands.parse(&command.to_string()) {
                        problems.push(format!("{table}: {keys:?} runs {command}: {error}"));
                    }
                }
                _ => {}
            }
        }
    }

    /// What the keys `typed` so far are in `table` in `situation`, by the module's rules, and
    /// which binding says so: for the engine, for tests and for the help page.
    pub fn resolve<'k>(
        &'k self,
        table: KeyTable<'_>,
        typed: &[KeyEvent],
        situation: &Situation<'_>,
    ) -> Found<'k> {
        let (bindings, typing) = match table {
            KeyTable::Line => (&self.line[..], true),
            KeyTable::Mode(name) => match self.mode(name) {
                Some(mode) => (&mode.bindings[..], situation.field() && mode.page_text),
                None => return Found::None,
            },
        };
        if typed.is_empty() {
            return Found::None;
        }
        let applies =
            |b: &Binding| b.when.holds(situation) && !(typing && b.keys.first().types());
        let rank = |i: usize| (bindings[i].when.specificity(), i);
        let tombstone = |b: &Binding| matches!(b.action, Action::Unbound);
        let masked = |i: usize| {
            let binding = &bindings[i];
            bindings.iter().enumerate().any(|(j, t)| {
                tombstone(t) && t.keys == binding.keys && applies(t) && rank(j) > rank(i)
            })
        };
        let live = (0..bindings.len()).filter(|&i| {
            let b = &bindings[i];
            !tombstone(b) && applies(b) && b.keys.could_be(typed) && !masked(i)
        });
        if let Some(best) = live.max_by_key(|&i| rank(i)) {
            let binding = &bindings[best];
            return match binding.keys.len() == typed.len() {
                true => Found::Complete(binding),
                false => Found::Start(binding),
            };
        }
        let unbinding = (0..bindings.len()).filter(|&i| {
            let b = &bindings[i];
            tombstone(b) && applies(b) && b.keys.len() == typed.len() && b.keys.could_be(typed)
        });
        match unbinding.max_by_key(|&i| rank(i)) {
            Some(i) => Found::Unbound(&bindings[i]),
            None => Found::None,
        }
    }

    /// The bindings that exist in `frontend`, for the help page: each mode's in order (the
    /// mode as its name in capitals, or empty when there is only one mode), then the line's
    /// that no mode has as well. The action is the commands as written (one `input.mode
    /// <name>` alone reads `<NAME> mode`), a custom action's description, "pass to the page"
    /// or "nothing"; where it applies is said from its condition. Tombstones are not listed:
    /// a binding they mask on some addresses says "except on" them.
    pub fn describe(&self, frontend: Frontend) -> Vec<KeyBinding> {
        let exists = |b: &&Binding| {
            b.when.frontend.is_none_or(|f| f == frontend) && !matches!(b.action, Action::Unbound)
        };
        let label = |mode: &Mode| match self.modes.len() {
            1 => String::new(),
            _ => mode.name.to_uppercase(),
        };
        // A key that types never fires in the line (the typing rule).
        let in_line = |b: &Binding| {
            b.when.url.is_none()
                && b.when.field.is_none()
                && !b.keys.first().types()
                && self.line.iter().any(|l| {
                    l.keys == b.keys && l.action == b.action && l.when.frontend == b.when.frontend
                })
        };
        let mut found = Vec::new();
        let mut shown_in_mode: Vec<&Binding> = Vec::new();
        for mode in &self.modes {
            for binding in mode.bindings.iter().filter(exists) {
                let mut applies = match (&binding.when.url, binding.when.field) {
                    (Some(url), None) => format!("on {url}"),
                    (Some(url), Some(false)) => format!("on {url}, not in a text field"),
                    (Some(url), Some(true)) => format!("on {url}, in a text field"),
                    (None, None) => "in the page".to_owned(),
                    (None, Some(false)) => "in the page, not in a text field".to_owned(),
                    (None, Some(true)) => "in a text field of the page".to_owned(),
                };
                if binding.when.url.is_none() {
                    let except: Vec<String> = mode
                        .bindings
                        .iter()
                        .filter(|other| {
                            other.keys == binding.keys
                                && other.when.url.is_some()
                                && other.when.frontend.is_none_or(|f| f == frontend)
                        })
                        .filter_map(|other| other.when.url.as_ref().map(ToString::to_string))
                        .collect();
                    if !except.is_empty() {
                        applies = format!("{applies}, except on {}", except.join(", "));
                    }
                }
                if in_line(binding) {
                    let line = self
                        .line
                        .iter()
                        .find(|l| l.keys == binding.keys && l.action == binding.action);
                    shown_in_mode.extend(line);
                    applies = match applies.as_str() {
                        "in the page" => "also in the address line".to_owned(),
                        _ => format!("also in the address line; {applies}"),
                    };
                }
                found.push(KeyBinding::new(
                    label(mode),
                    binding.keys.to_string(),
                    describe_action(&binding.action),
                    applies,
                ));
            }
        }
        let line_only = self
            .line
            .iter()
            .filter(exists)
            .filter(|l| !l.keys.first().types())
            .filter(|l| !shown_in_mode.iter().any(|shown| std::ptr::eq(*shown, *l)));
        for binding in line_only {
            found.push(KeyBinding::new(
                "",
                binding.keys.to_string(),
                describe_action(&binding.action),
                "in the address line",
            ));
        }
        found
    }
}

/// What `action` does, in a few words, for the help page.
fn describe_action(action: &Action) -> String {
    match action {
        Action::Run(commands) if commands.is_empty() => "nothing".into(),
        Action::RunThenPass(commands) if commands.is_empty() => "pass to the page".into(),
        Action::Run(commands) => match &commands[..] {
            [Command::InputMode(name)] => format!("{} mode", name.to_uppercase()),
            _ => join(commands),
        },
        Action::RunThenPass(commands) => format!("{}, then pass to the page", join(commands)),
        Action::Custom(custom) => custom.describe.clone(),
        Action::Unbound => "nothing".into(),
    }
}

fn join(commands: &[Command]) -> String {
    let texts: Vec<String> = commands.iter().map(ToString::to_string).collect();
    texts.join("; ")
}

/// Binds `binding` in `bindings`: first removes every binding it replaces (the same address
/// condition, keys that are the same, a start of its keys or started by them, and `field` and
/// `frontend` conditions the same as its own or narrower), then adds it last.
pub fn bind(bindings: &mut Vec<Binding>, binding: Binding) {
    remove_replaced(bindings, &binding.keys, &binding.when);
    bindings.push(binding);
}

/// Unbinds `keys` in `bindings` for `when`: removes what binding them would replace, and then,
/// unless `when` is "always" (when nothing broader can be left), adds a tombstone, so a
/// broader binding of exactly these keys does not apply where `when` holds.
pub fn unbind(bindings: &mut Vec<Binding>, keys: KeySeq, when: When) {
    remove_replaced(bindings, &keys, &when);
    if when != When::default() {
        bindings.push(Binding {
            keys,
            when,
            action: Action::Unbound,
        });
    }
}

fn remove_replaced(bindings: &mut Vec<Binding>, keys: &KeySeq, when: &When) {
    bindings.retain(|old| !(old.keys.overlaps(keys) && old.when.replaced_by(when)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ButtonState, Key, KeyCode, Modifiers};

    fn keys(text: &str) -> KeySeq {
        text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    fn command(text: &str) -> Command {
        CommandSet::ANY
            .parse(text)
            .unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

    fn run(text: &str) -> Action {
        Action::Run(vec![command(text)])
    }

    fn binding(text: &str, when: When, action: Action) -> Binding {
        Binding {
            keys: keys(text),
            when,
            action,
        }
    }

    fn on(url: &str) -> When {
        When {
            url: Some(url.parse().unwrap()),
            ..When::default()
        }
    }

    fn field(field: bool) -> When {
        When {
            field: Some(field),
            ..When::default()
        }
    }

    fn frontend(frontend: Frontend) -> When {
        When {
            frontend: Some(frontend),
            ..When::default()
        }
    }

    /// A press of a key that means `key`: a character, or a W3C key name.
    fn event(key: &str, modifiers: Modifiers) -> KeyEvent {
        let key = match key.chars().count() {
            1 => Key::Character(key.into()),
            _ => Key::Named(key.into()),
        };
        KeyEvent {
            code: KeyCode(30),
            key,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers,
            consumed: Modifiers::default(),
            text: None,
        }
    }

    fn plain(key: &str) -> KeyEvent {
        event(key, Modifiers::default())
    }

    fn ctrl(key: &str) -> KeyEvent {
        event(
            key,
            Modifiers {
                control: true,
                ..Modifiers::default()
            },
        )
    }

    const PAGE: KeyContext = KeyContext::new(KeyboardFocus::Page, false);
    const FIELD: KeyContext = KeyContext::new(KeyboardFocus::Page, true);
    const CHROME: KeyContext = KeyContext::new(KeyboardFocus::Chrome, true);

    fn at(context: KeyContext, address: Option<&str>) -> Situation<'_> {
        Situation {
            frontend: Frontend::Gui,
            context,
            address,
        }
    }

    /// Two modes as the default experience has them: `normal` (no page text, takes what it
    /// doesn't bind) and `insert`.
    fn two_modes() -> Keys {
        Keys {
            modes: vec![
                Mode::new("normal")
                    .page_text(false)
                    .unmatched(Fallback::Take),
                Mode::new("insert").indicator("INSERT MODE"),
            ],
            initial: "normal".into(),
            line: Vec::new(),
        }
    }

    const NORMAL: KeyTable<'static> = KeyTable::Mode("normal");
    const INSERT: KeyTable<'static> = KeyTable::Mode("insert");

    /// What `typed` does in `table`: the action of the binding that decides, `Start` for one
    /// that waits, `None` for no binding (an `Unbound` tombstone included).
    fn does(keys: &Keys, table: KeyTable<'_>, typed: &[KeyEvent], s: &Situation<'_>) -> String {
        match keys.resolve(table, typed, s) {
            Found::Complete(b) => describe_action(&b.action),
            Found::Start(b) => format!("waits for {}", b.keys),
            Found::Unbound(_) | Found::None => "none".into(),
        }
    }

    #[test]
    fn key_sequences_parse_and_print() {
        assert_eq!(keys("y y").to_string(), "y y");
        assert_eq!(keys("  ctrl-x   k ").to_string(), "ctrl-x k");
        assert_eq!(keys("T").to_string(), "T");
        assert_eq!(keys("T"), keys("shift-t"));
        assert_eq!(keys("y  f").written(1), "y");
        assert_eq!(keys("g").chords().len(), 1);
        assert!(keys("g g").starts_with(&keys("g")));
        assert!(!keys("g").starts_with(&keys("g g")));
        // A modifier may be a binding's only key (finding 15), never one of a sequence.
        assert_eq!(keys("shift-Shift").to_string(), "shift-Shift");
        for (text, error) in [
            ("", "no key"),
            ("a b c d e", "at most 4"),
            ("g Shift", "modifier"),
            ("Compose x", "modifier"),
            ("g hyper-x", "unknown modifier"),
        ] {
            let parsed = text.parse::<KeySeq>();
            assert!(
                parsed.as_ref().is_err_and(|e| e.contains(error)),
                "{text:?}: {parsed:?}"
            );
        }
    }

    #[test]
    fn binding_replaces_the_same_keys_whatever_conditions_the_old_one_had() {
        // Finding 8: the default Space is stated for "outside fields"; a plain bind replaces
        // it, and a plain unbind removes it, without the user knowing that condition.
        let mut table = vec![binding("space", field(false), run("page.send-key space"))];
        bind(&mut table, binding("space", When::default(), run("tab.next")));
        assert_eq!(table, [binding("space", When::default(), run("tab.next"))]);
        let mut table = vec![binding("space", field(false), run("page.send-key space"))];
        unbind(&mut table, keys("space"), When::default());
        assert!(table.is_empty(), "nothing broader is left, so no tombstone");
        // A generic ctrl-l replaces the default's terminal-only one.
        let terminal = frontend(Frontend::Terminal);
        let mut table = vec![binding("ctrl-l", terminal.clone(), run("chrome.refresh"))];
        bind(&mut table, binding("ctrl-l", When::default(), run("page.reload")));
        assert_eq!(table.len(), 1);
        assert_eq!(table[0].when, When::default());
        // A narrower bind leaves the broader one: `in_window().bind("q", ...)` keeps the
        // general q for the terminal.
        let mut table = vec![binding("q", When::default(), run("browser.quit"))];
        bind(&mut table, binding("q", frontend(Frontend::Gui), run("tab.close")));
        assert_eq!(table.len(), 2);
        let s = at(PAGE, None);
        let terminal = Situation {
            frontend: Frontend::Terminal,
            ..s
        };
        let keys_of = |modes: Vec<Binding>| Keys {
            modes: vec![Mode {
                bindings: modes,
                ..Mode::new("m")
            }],
            initial: "m".into(),
            line: Vec::new(),
        };
        let both = keys_of(table);
        let m = KeyTable::Mode("m");
        assert_eq!(does(&both, m, &[plain("q")], &s), "tab.close");
        assert_eq!(does(&both, m, &[plain("q")], &terminal), "browser.quit");
        // A narrower field condition is replaced; a different one is not.
        let mut table = vec![
            binding("x", field(false), run("tab.new")),
            binding("x", field(true), run("tab.close")),
        ];
        bind(&mut table, binding("x", field(true), run("page.reload")));
        assert_eq!(
            table,
            [
                binding("x", field(false), run("tab.new")),
                binding("x", field(true), run("page.reload"))
            ]
        );
    }

    #[test]
    fn a_later_definition_wins_a_prefix_conflict() {
        // `g g` removes `g`, and `g` later removes `g g`: no binding is ever the start of
        // another with the same condition.
        let mut table = vec![binding("g", When::default(), run("page.send-key home"))];
        bind(&mut table, binding("g g", When::default(), run("tab.new")));
        assert_eq!(table, [binding("g g", When::default(), run("tab.new"))]);
        bind(&mut table, binding("g", When::default(), run("tab.close")));
        assert_eq!(table, [binding("g", When::default(), run("tab.close"))]);
        // And `y y`, `y f` are no starts of one another.
        let mut table = vec![binding("y y", When::default(), run("tab.new"))];
        bind(&mut table, binding("y f", When::default(), run("tab.close")));
        assert_eq!(table.len(), 2);
    }

    #[test]
    fn a_sites_bindings_survive_a_general_override_and_the_reverse() {
        let mut k = two_modes();
        let escape = || binding("escape", When::default(), run("input.mode normal"));
        k.bind(INSERT, escape()).unwrap();
        k.unbind(INSERT, keys("escape"), on("sig://terminal")).unwrap();
        k.bind(
            INSERT,
            binding("f12", on("sig://terminal"), run("input.mode normal")),
        )
        .unwrap();
        // `insert.bind("escape", ...)` leaves the terminal pages' Escape alone.
        k.bind(
            INSERT,
            binding("escape", When::default(), run("page.reload")),
        )
        .unwrap();
        let bindings = k.table(INSERT).unwrap();
        assert_eq!(bindings.len(), 3);
        assert!(bindings
            .iter()
            .any(|b| b.action == Action::Unbound && b.when == on("sig://terminal")));
        // And a site's binding leaves the general one alone.
        k.bind(
            INSERT,
            binding("escape", on("https://a.test"), run("tab.new")),
        )
        .unwrap();
        let s = |url| at(PAGE, Some(url));
        let terminal = s("sig://terminal");
        assert_eq!(does(&k, INSERT, &[plain("Escape")], &terminal), "none");
        assert_eq!(
            does(&k, INSERT, &[plain("Escape")], &s("https://b.test/")),
            "page.reload"
        );
        assert_eq!(
            does(&k, INSERT, &[plain("Escape")], &s("https://a.test/x")),
            "tab.new"
        );
        assert_eq!(
            does(&k, INSERT, &[plain("F12")], &terminal),
            "NORMAL mode"
        );
    }

    #[test]
    fn the_most_specific_binding_decides() {
        let rule = |text: &str, when: When, does: &str| binding(text, when, run(does));
        let mut m = Mode::new("m");
        m.bindings = vec![
            rule("ctrl-k", When::default(), "tab.new"),
            rule("ctrl-k", frontend(Frontend::Gui), "tab.close"),
            rule("ctrl-k", field(false), "tab.next"),
            rule("ctrl-k", on("https://*.a.test"), "page.back"),
            rule("ctrl-k", on("https://w.a.test"), "page.forward"),
            rule("ctrl-k", on("https://w.a.test/docs"), "page.stop"),
        ];
        let k = Keys {
            modes: vec![m],
            initial: "m".into(),
            line: Vec::new(),
        };
        let m = KeyTable::Mode("m");
        let q = |address| does(&k, m, &[ctrl("k")], &at(PAGE, address));
        // An exact host's longer path, then the exact host, then the wildcard.
        assert_eq!(q(Some("https://w.a.test/docs/x")), "page.stop");
        assert_eq!(q(Some("https://w.a.test/")), "page.forward");
        assert_eq!(q(Some("https://v.a.test/")), "page.back");
        // No address: `field` stated before `frontend` stated before neither.
        assert_eq!(q(Some("https://other.test/")), "tab.next");
        let field_now = does(&k, m, &[ctrl("k")], &at(FIELD, None));
        assert_eq!(field_now, "tab.close", "`field = false` does not hold");
        let terminal = Situation {
            frontend: Frontend::Terminal,
            ..at(FIELD, None)
        };
        assert_eq!(does(&k, m, &[ctrl("k")], &terminal), "tab.new");
        // Of equally specific bindings, the one defined later; the operations never leave two,
        // but a hand-built value can.
        let mut m = Mode::new("m");
        m.bindings = vec![
            rule("j", When::default(), "tab.new"),
            rule("j", When::default(), "tab.close"),
        ];
        let k = Keys {
            modes: vec![m],
            initial: "m".into(),
            line: Vec::new(),
        };
        assert_eq!(
            does(&k, KeyTable::Mode("m"), &[plain("j")], &at(PAGE, None)),
            "tab.close"
        );
    }

    #[test]
    fn an_address_is_more_specific_than_a_frontend() {
        // Finding 2, as decided: `[keys.url."https://a.test"] ctrl-r = page.reload` and
        // `[tui.keys] ctrl-r = chrome.refresh`, in the terminal on a.test: the address wins.
        let mut k = Keys::plain();
        let table = KeyTable::Mode(PLAIN_MODE);
        k.bind(table, binding("ctrl-r", on("https://a.test"), run("page.reload")))
            .unwrap();
        k.bind(
            table,
            binding("ctrl-r", frontend(Frontend::Terminal), run("chrome.refresh")),
        )
        .unwrap();
        let terminal = |address| Situation {
            frontend: Frontend::Terminal,
            context: PAGE,
            address,
        };
        assert_eq!(
            does(&k, table, &[ctrl("r")], &terminal(Some("https://a.test/"))),
            "page.reload"
        );
        assert_eq!(
            does(&k, table, &[ctrl("r")], &terminal(Some("https://b.test/"))),
            "chrome.refresh"
        );
    }

    #[test]
    fn a_tombstone_masks_only_its_own_keys() {
        // Finding 4: only `unbind("g g")` in a permissive mode. With no condition nothing is
        // added; on a site the tombstone is no start of anything, so `g` is not swallowed.
        let mut k = Keys::plain();
        let table = KeyTable::Mode(PLAIN_MODE);
        k.unbind(table, keys("g g"), When::default()).unwrap();
        assert!(k.table(table).unwrap().is_empty());
        k.unbind(table, keys("g g"), on("https://a.test")).unwrap();
        let s = at(PAGE, Some("https://a.test/"));
        assert_eq!(k.resolve(table, &[plain("g")], &s), Found::None);
        assert!(matches!(
            k.resolve(table, &[plain("g"), plain("g")], &s),
            Found::Unbound(_)
        ));
        // Define `g g` generally, then unbind `g` on a site: `g g` is still there, there.
        let mut k = Keys::plain();
        k.bind(table, binding("g g", When::default(), run("tab.new")))
            .unwrap();
        k.unbind(table, keys("g"), on("https://a.test")).unwrap();
        assert!(matches!(
            k.resolve(table, &[plain("g")], &s),
            Found::Start(_)
        ));
        assert!(matches!(
            k.resolve(table, &[plain("g"), plain("g")], &s),
            Found::Complete(_)
        ));
        // A tombstone masks a less specific binding of its keys, not a more specific one.
        let mut k = two_modes();
        k.bind(INSERT, binding("escape", When::default(), run("input.mode normal")))
            .unwrap();
        k.unbind(INSERT, keys("escape"), on("https://*.a.test"))
            .unwrap();
        k.bind(INSERT, binding("escape", on("https://w.a.test"), run("tab.new")))
            .unwrap();
        let escape = |url| does(&k, INSERT, &[plain("Escape")], &at(PAGE, Some(url)));
        assert_eq!(escape("https://v.a.test/"), "none");
        assert_eq!(escape("https://w.a.test/"), "tab.new");
        assert_eq!(escape("https://b.test/"), "NORMAL mode");
    }

    #[test]
    fn keys_that_type_do_not_apply_where_typed_text_goes() {
        let mut k = two_modes();
        for table in [NORMAL, INSERT, KeyTable::Line] {
            k.bind(table, binding("j", When::default(), run("tab.next")))
                .unwrap();
            k.bind(table, binding("ctrl-j", When::default(), run("tab.new")))
                .unwrap();
        }
        // Strict NORMAL gives the page no text: `j` is a command even in a field.
        assert_eq!(does(&k, NORMAL, &[plain("j")], &at(FIELD, None)), "tab.next");
        // INSERT gives it text: in a field, `j` types; outside one it runs.
        assert_eq!(does(&k, INSERT, &[plain("j")], &at(FIELD, None)), "none");
        assert_eq!(does(&k, INSERT, &[plain("j")], &at(PAGE, None)), "tab.next");
        // The line is typing, always. Keys that type nothing apply everywhere.
        let line = KeyTable::Line;
        assert_eq!(does(&k, line, &[plain("j")], &at(CHROME, None)), "none");
        for table in [NORMAL, INSERT, line] {
            for context in [PAGE, FIELD, CHROME] {
                assert_eq!(does(&k, table, &[ctrl("j")], &at(context, None)), "tab.new");
            }
        }
        // The rule looks at the first keystroke: `ctrl-k a` is no typing at its start.
        k.bind(INSERT, binding("ctrl-k a", When::default(), run("tab.close")))
            .unwrap();
        assert_eq!(
            does(&k, INSERT, &[ctrl("k")], &at(FIELD, None)),
            "waits for ctrl-k a"
        );
    }

    #[test]
    fn sequences_wait_and_complete() {
        let mut k = two_modes();
        k.bind(NORMAL, binding("y y", When::default(), run("tab.duplicate")))
            .unwrap();
        k.bind(NORMAL, binding("y f", When::default(), run("tab.close")))
            .unwrap();
        let s = at(PAGE, None);
        // Of two starts, the one defined later says so; either waits.
        assert_eq!(does(&k, NORMAL, &[plain("y")], &s), "waits for y f");
        assert_eq!(
            does(&k, NORMAL, &[plain("y"), plain("f")], &s),
            "tab.close"
        );
        assert_eq!(does(&k, NORMAL, &[plain("y"), plain("z")], &s), "none");
        assert_eq!(does(&k, NORMAL, &[], &s), "none");
        assert_eq!(does(&k, KeyTable::Mode("nope"), &[plain("y")], &s), "none");
    }

    #[test]
    fn check_finds_what_cannot_be_used() {
        let commands = CommandSet::new("standard", &["toggle-tabs"]);
        let ok = |k: &Keys| k.check(&commands.with_custom(&[]), Frontend::Gui);
        assert_eq!(ok(&two_modes()), Vec::<String>::new());
        assert_eq!(ok(&Keys::plain()), Vec::<String>::new());
        let mut k = two_modes();
        k.initial = "visual".into();
        k.modes.push(Mode::new("insert"));
        k.modes.push(Mode::new(LINE));
        k.modes.push(Mode::new("Bad Name").indicator("two\nlines"));
        let normal = &mut k.modes[0].bindings;
        // A hand-built table with a prefix conflict, and a tombstone that is a duplicate.
        normal.push(binding("g", When::default(), run("tab.new")));
        normal.push(binding("g g", When::default(), run("tab.close")));
        normal.push(binding("x", on("https://a.test"), Action::Unbound));
        normal.push(binding("x", on("https://a.test"), Action::Unbound));
        // A tombstone starting a binding with the same condition is no conflict.
        normal.push(binding("z", on("https://a.test"), Action::Unbound));
        normal.push(binding("z z", on("https://a.test"), run("tab.new")));
        normal.push(binding("r", When::default(), run("input.mode resize")));
        normal.push(binding("h", When::default(), run("help")));
        normal.push(binding("t", When::default(), run("chrome.toggle-tab")));
        // The terminal's chrome commands are checked only where they can run.
        normal.push(binding("m", frontend(Frontend::Terminal), run("chrome.refresh")));
        let problems = ok(&k);
        let expected = [
            "the initial mode \"visual\" is not one of the modes",
            "there are two modes called \"insert\"",
            "\"line\" is not a mode's name",
            "\"Bad Name\" is not a mode's name",
            "mode Bad Name: its indicator is at most 64 characters on one line",
            "mode normal: \"g\" and \"g g\" have the same condition",
            "mode normal: \"x\" and \"x\" have the same condition",
            "mode normal: \"r\" runs input.mode resize, but there is no mode \"resize\"",
            "mode normal: \"h\" runs help: unknown command \"help\"",
            "mode normal: \"t\" runs chrome.toggle-tab: the standard chrome has no command",
        ];
        for part in expected {
            assert!(
                problems.iter().any(|p| p.contains(part)),
                "{part:?} not in {problems:#?}"
            );
        }
        assert_eq!(problems.len(), expected.len(), "{problems:#?}");
        // The configuration's aliases are its own.
        let mut k = two_modes();
        k.bind(NORMAL, binding("h", When::default(), run("help")))
            .unwrap();
        let help = ["help".to_owned()];
        assert!(k.check(&commands.with_custom(&help), Frontend::Gui).is_empty());
        // Bounds.
        let mut k = Keys::plain();
        k.modes.extend((0..MAX_MODES).map(|i| Mode::new(format!("m{i}"))));
        assert!(ok(&k)[0].contains("there must be 1 to 16"));
        let none = Keys {
            modes: Vec::new(),
            initial: String::new(),
            line: Vec::new(),
        };
        assert!(ok(&none)[0].contains("there are 0 modes"));
        let mut k = Keys::plain();
        let many = vec![command("tab.new"); MAX_ACTION_COMMANDS + 1];
        k.line.push(binding("ctrl-a", When::default(), Action::Run(many)));
        k.line.extend((0..MAX_BINDINGS).map(|i| {
            binding(&format!("ctrl-{}", i % 9 + 1), on(&format!("https://s{i}.test")), run("tab.new"))
        }));
        let problems = ok(&k);
        assert!(problems.iter().any(|p| p.contains("runs 65 commands")));
        assert!(problems.iter().any(|p| p.contains("1025 bindings")));
        // A hand-built command that can't be written back is caught too.
        let mut k = Keys::plain();
        k.line.push(binding("ctrl-a", When::default(), Action::Run(vec![Command::PageOpen(String::new())])));
        assert!(ok(&k)[0].contains("page.open needs an argument"), "{:?}", ok(&k));
    }

    #[test]
    fn describe_says_what_each_binding_does_and_where() {
        let mut k = two_modes();
        let both = [NORMAL, KeyTable::Line];
        for table in both {
            k.bind(table, binding("f1", When::default(), run("help")))
                .unwrap();
        }
        k.bind(NORMAL, binding("j", When::default(), run("page.send-key down")))
            .unwrap();
        k.bind(NORMAL, binding("space", field(false), run("page.send-key space")))
            .unwrap();
        k.bind(
            NORMAL,
            binding("y y", When::default(), Action::Custom(Custom::new("copy the page's address", |_| Outcome::take()))),
        )
        .unwrap();
        k.bind(NORMAL, binding("ctrl-c", frontend(Frontend::Terminal), run("browser.quit")))
            .unwrap();
        k.bind(NORMAL, binding(":", When::default(), Action::RunThenPass(vec![command("location.focus")])))
            .unwrap();
        k.bind(NORMAL, binding("z", When::default(), Action::Run(Vec::new())))
            .unwrap();
        k.bind(INSERT, binding("escape", When::default(), run("input.mode normal")))
            .unwrap();
        for site in ["sig://terminal", "sig://config"] {
            k.unbind(INSERT, keys("escape"), on(site)).unwrap();
            k.bind(INSERT, binding("f12", on(site), run("input.mode normal")))
                .unwrap();
        }
        k.bind(KeyTable::Line, binding("ctrl-r", When::default(), run("page.reload")))
            .unwrap();
        let rows: Vec<(String, String, String, String)> = k
            .describe(Frontend::Gui)
            .into_iter()
            .map(|b| (b.mode, b.keys, b.action, b.applies))
            .collect();
        let row = |m: &str, keys: &str, action: &str, applies: &str| {
            (m.to_owned(), keys.to_owned(), action.to_owned(), applies.to_owned())
        };
        assert_eq!(
            rows,
            [
                row("NORMAL", "f1", "help", "also in the address line"),
                row("NORMAL", "j", "page.send-key ArrowDown", "in the page"),
                row("NORMAL", "space", "page.send-key space", "in the page, not in a text field"),
                row("NORMAL", "y y", "copy the page's address", "in the page"),
                row("NORMAL", ":", "location.focus, then pass to the page", "in the page"),
                row("NORMAL", "z", "nothing", "in the page"),
                row(
                    "INSERT",
                    "escape",
                    "NORMAL mode",
                    "in the page, except on sig://terminal, sig://config"
                ),
                row("INSERT", "f12", "NORMAL mode", "on sig://terminal"),
                row("INSERT", "f12", "NORMAL mode", "on sig://config"),
                row("", "ctrl-r", "page.reload", "in the address line"),
            ]
        );
        // The terminal's own binding is the terminal's, and one mode has no name to show.
        assert!(k
            .describe(Frontend::Terminal)
            .iter()
            .any(|b| b.keys == "ctrl-c"));
        let mut plain = Keys::plain();
        plain
            .bind(KeyTable::Mode(PLAIN_MODE), binding("ctrl-t", When::default(), run("tab.new")))
            .unwrap();
        assert_eq!(plain.describe(Frontend::Gui)[0].mode, "");
    }
}
