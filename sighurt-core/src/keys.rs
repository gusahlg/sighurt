//! Keys: the `bindings` keymap, which reads the user's `[keys]` configuration, and the core's
//! record of the presses taken, which holds whatever keymap is in use.
//!
//! Bindings come only from the configuration: there are none by default and no key handlers in
//! code. `[keys]` applies everywhere, `[keys.page]` while the page has focus and
//! `[keys.location]` while the chrome (the address bar) has; a binding of the focused part wins
//! over a global one. Keystrokes are `sighurt_api::keymap::Chord`s, matched as that type says:
//! by the layout's meaning unless they name a physical key, and AltGr never Ctrl+Alt. While the
//! user types, in the address bar or a page's text field, a binding that would type a character
//! (no modifier but Shift) lets the key type.
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
use sighurt_api::keymap::{Chord, KeyAction, KeyContext, Keymap, KeymapKind};
use sighurt_api::settings::{Table, Value};

/// The `bindings` keymap: the default, which every build has.
pub const KIND: KeymapKind = KeymapKind {
    name: "bindings",
    check,
    new,
};

fn check(table: &Table, commands: &CommandSet<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    Bindings::from_table(table, commands, &mut problems);
    problems
}

fn new() -> Box<dyn Keymap> {
    Box::new(Bindings::default())
}

/// The configured bindings, per input context.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bindings {
    global: Vec<Binding>,
    page: Vec<Binding>,
    location: Vec<Binding>,
}

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    /// As the user wrote it, for messages.
    text: String,
    chord: Chord,
    command: Command,
}

impl Bindings {
    /// Reads the `[keys]` table, with its commands parsed by `commands`, adding what is wrong
    /// with it to `problems`.
    pub(crate) fn from_table(
        keys: &Table,
        commands: &CommandSet<'_>,
        problems: &mut Vec<String>,
    ) -> Self {
        let mut bindings = Self::default();
        for (key, value) in keys {
            match value {
                Value::String(command) => add(
                    &mut bindings.global,
                    "[keys]",
                    key,
                    command,
                    commands,
                    problems,
                ),
                Value::Table(table) => {
                    let (list, section) = match key.as_str() {
                        "page" => (&mut bindings.page, "[keys.page]"),
                        "location" => (&mut bindings.location, "[keys.location]"),
                        other => {
                            problems.push(format!(
                                "[keys.{other}]: unknown context (use [keys.page] or [keys.location])"
                            ));
                            continue;
                        }
                    };
                    for (key, value) in table {
                        match value.as_str() {
                            Some(command) => add(list, section, key, command, commands, problems),
                            None => problems
                                .push(format!("{section} {key:?}: expected a command in quotes")),
                        }
                    }
                }
                _ => problems.push(format!(
                    "[keys] {key:?}: expected a command in quotes, or a [keys.page] or \
                     [keys.location] table"
                )),
            }
        }
        bindings
    }

    /// The command bound to `event` while `focus` has the keyboard. `typing` says the focused
    /// part is a text field, where keystrokes that type characters are left to it.
    pub fn find(&self, focus: KeyboardFocus, typing: bool, event: &KeyEvent) -> Option<&Command> {
        let context = match focus {
            KeyboardFocus::Page => &self.page,
            KeyboardFocus::Chrome => &self.location,
        };
        context
            .iter()
            .chain(&self.global)
            .find(|binding| !(typing && binding.chord.types()) && binding.chord.matches(event))
            .map(|binding| &binding.command)
    }
}

impl Keymap for Bindings {
    fn configure(&mut self, settings: &Table) {
        // `check` accepted the table, so reading it finds nothing wrong.
        *self = Bindings::from_table(settings, &CommandSet::ANY, &mut Vec::new());
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext) -> KeyAction {
        match self.find(context.focus, context.typing, key) {
            Some(command) => KeyAction::Run(command.clone()),
            None => KeyAction::Pass,
        }
    }
}

/// Parses one binding into `list`, or says what is wrong with it.
fn add(
    list: &mut Vec<Binding>,
    section: &str,
    text: &str,
    command: &str,
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
    if let Some(same) = list.iter().find(|binding| binding.chord == chord) {
        return problems.push(format!(
            "{section} {text:?} and {:?} are the same keystroke ({chord})",
            same.text
        ));
    }
    list.push(Binding {
        text: text.to_owned(),
        chord,
        command,
    });
}

/// What to do with one key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyOutcome {
    /// Run the command. The key goes nowhere else.
    Run(Command),
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

    /// Decides what happens to `event`, a key event from `seat` in `context`.
    pub(crate) fn handle(
        &mut self,
        context: KeyContext,
        seat: SeatId,
        event: &KeyEvent,
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
            // A repeat of a press the keymap took may run again, but never passes.
            ButtonState::Pressed if event.repeat => match self.keymap.press(event, context) {
                KeyAction::Run(command) | KeyAction::RunThenPass(command) => {
                    KeyOutcome::Run(command)
                }
                KeyAction::Pass | KeyAction::Take => KeyOutcome::Consumed,
            },
            ButtonState::Pressed => match self.keymap.press(event, context) {
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
                    KeyOutcome::Run(command)
                }
                KeyAction::RunThenPass(command) => {
                    self.taken.remove(&key);
                    KeyOutcome::RunThenDeliver(command)
                }
            },
        }
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
        let bindings = Bindings::from_table(&table(text), &commands, &mut problems);
        problems.is_empty().then_some(bindings).ok_or(problems)
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
            bindings.find(KeyboardFocus::Page, false, &ctrl_l),
            Some(&Command::PageReload)
        );
        assert_eq!(
            bindings.find(KeyboardFocus::Chrome, true, &ctrl_l),
            Some(&Command::LocationFocus)
        );
        let j = press(36, character("j"), NONE, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, &j),
            Some(&Command::TabNext)
        );
        assert_eq!(bindings.find(KeyboardFocus::Page, true, &j), None);
        assert_eq!(bindings.find(KeyboardFocus::Chrome, true, &j), None);
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

    #[test]
    fn the_keymap_reads_its_table_as_the_check_did() {
        let text = "\"ctrl-q\" = \"browser.quit\"\n[page]\n\"f5\" = \"page.reload\"";
        let mut keymap = Bindings::default();
        keymap.configure(&table(text));
        assert_eq!(keymap, bindings(text).unwrap());
        let ctrl_q = press(KEY_Q, character("q"), CTRL, NONE);
        assert_eq!(
            keymap.press(&ctrl_q, PAGE),
            KeyAction::Run(Command::BrowserQuit)
        );
        assert_eq!(
            keymap.press(&press(KEY_A, character("a"), NONE, NONE), PAGE),
            KeyAction::Pass
        );
    }

    #[test]
    fn consumed_presses_consume_their_releases() {
        let mut keys = keys(r#""ctrl-a" = "tab.new""#);
        let seat = SeatId::next();
        let mut handle = |event: &KeyEvent| keys.handle(PAGE, seat, event);
        let down = press(KEY_A, character("a"), CTRL, NONE);
        let repeat = KeyEvent {
            repeat: true,
            ..down.clone()
        };
        let up = KeyEvent {
            state: ButtonState::Released,
            ..down.clone()
        };
        assert_eq!(handle(&down), KeyOutcome::Run(Command::TabNew));
        assert_eq!(handle(&repeat), KeyOutcome::Run(Command::TabNew));
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
        let mut handle = |event: &KeyEvent| keys.handle(PAGE, seat, event);
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
        assert_eq!(handle(&ctrl_r), KeyOutcome::Run(Command::PageReload));
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
        keys.handle(PAGE, seat, &down);
        keys.forget(seat);
        assert_eq!(keys.handle(PAGE, seat, &up), KeyOutcome::Deliver);
    }
}
