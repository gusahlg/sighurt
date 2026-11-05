//! Keys: the `bindings` keymap, which reads the user's `[keys]` configuration, and the core's
//! record of the presses taken, which holds whatever keymap is in use.
//!
//! Bindings come only from the configuration: there are no key handlers in code, and neither
//! frontend has any until the user binds them. In the terminal, a `[tui.keys]` the user writes
//! sits over `[keys]` and applies only there. `[keys]` applies everywhere, `[keys.page]` while
//! the page has focus and `[keys.location]` while the chrome (the address bar) has; a binding
//! of the focused part wins over a global one in the same layer, and a higher layer wins over
//! a lower one. Keystrokes are `sighurt_api::keymap::Chord`s, matched as that type says: by
//! the layout's meaning unless they name a physical key, and AltGr never Ctrl+Alt. While the
//! user types, in the address bar or a page's text field, a binding that would type a character
//! (no modifier but Shift) lets the key type, and the search continues with the other bindings.
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
    Browser, Chord, Frontend, KeyAction, KeyBinding, KeyContext, Keymap, KeymapKind,
};
use sighurt_api::settings::{Table, Value};

/// The `bindings` keymap: the default, which every build has.
pub const KIND: KeymapKind = KeymapKind {
    name: "bindings",
    check,
    new,
};

fn check(table: &Table, commands: &CommandSet<'_>) -> Vec<String> {
    let mut problems = Vec::new();
    Bindings::from_table(table, "keys", commands, &mut problems);
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

fn new() -> Box<dyn Keymap> {
    Box::new(Bindings::default())
}

/// The configured bindings, per input context.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bindings {
    global: Vec<Binding>,
    page: Vec<Binding>,
    location: Vec<Binding>,
    /// Layers under this one, in the order they are searched. Empty when the table is a
    /// single layer.
    below: Vec<Bindings>,
}

#[derive(Clone, Debug, PartialEq)]
struct Binding {
    /// As the user wrote it, for messages.
    text: String,
    chord: Chord,
    command: Command,
}

impl Bindings {
    /// Reads a key table (`section` is `keys` or `tui.keys`), with its commands parsed by
    /// `commands`, adding what is wrong with it to `problems`.
    pub(crate) fn from_table(
        keys: &Table,
        section: &str,
        commands: &CommandSet<'_>,
        problems: &mut Vec<String>,
    ) -> Self {
        let mut bindings = Self::default();
        let head = format!("[{section}]");
        for (key, value) in keys {
            match value {
                Value::String(command) => add(
                    &mut bindings.global,
                    &head,
                    key,
                    command,
                    commands,
                    problems,
                ),
                Value::Table(table) => {
                    let nested = format!("[{section}.{key}]");
                    let list = match key.as_str() {
                        "page" => &mut bindings.page,
                        "location" => &mut bindings.location,
                        other => {
                            problems.push(format!(
                                "[{section}.{other}]: unknown context (use [{section}.page] or \
                                 [{section}.location])"
                            ));
                            continue;
                        }
                    };
                    for (key, value) in table {
                        match value.as_str() {
                            Some(command) => add(list, &nested, key, command, commands, problems),
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
        bindings
    }

    /// The bindings `settings` describes: one key table, or `[tui.keys]` over `[keys]`.
    fn from_settings(settings: &Table) -> Self {
        let mut problems = Vec::new();
        let bindings = match layered(settings) {
            Some([tui, user]) => {
                let mut top = Self::from_table(tui, "tui.keys", &CommandSet::ANY, &mut problems);
                let layer = Self::from_table(user, "keys", &CommandSet::ANY, &mut problems);
                if !layer.is_empty() {
                    top.below.push(layer);
                }
                top
            }
            None => Self::from_table(settings, "keys", &CommandSet::ANY, &mut problems),
        };
        // Each layer was checked when the configuration loaded.
        debug_assert!(problems.is_empty(), "{problems:?}");
        bindings
    }

    fn is_empty(&self) -> bool {
        self.global.is_empty() && self.page.is_empty() && self.location.is_empty()
    }

    /// The command bound to `event` while `focus` has the keyboard. `typing` says the focused
    /// part is a text field, where keystrokes that type characters are left to it. A binding
    /// that does not fire is skipped, including one in a layer under this one.
    pub fn find(&self, focus: KeyboardFocus, typing: bool, event: &KeyEvent) -> Option<&Command> {
        self.find_here(focus, typing, event).or_else(|| {
            self.below
                .iter()
                .find_map(|layer| layer.find(focus, typing, event))
        })
    }

    fn find_here(&self, focus: KeyboardFocus, typing: bool, event: &KeyEvent) -> Option<&Command> {
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

impl Keymap for Bindings {
    fn configure(&mut self, settings: &Table) {
        *self = Bindings::from_settings(settings);
    }

    fn press(&mut self, key: &KeyEvent, context: KeyContext, _browser: &dyn Browser) -> KeyAction {
        match self.find(context.focus, context.typing, key) {
            Some(command) => KeyAction::Run(command.clone()),
            None => KeyAction::Pass,
        }
    }

    /// Every binding, the layer searched first first: `[keys]` everywhere, then `[keys.page]`
    /// and `[keys.location]`. A binding a higher layer hides is still listed.
    fn describe(&self, _frontend: Frontend) -> Vec<KeyBinding> {
        let mut found = Vec::new();
        for layer in std::iter::once(self).chain(&self.below) {
            let lists = [
                (&layer.global, ""),
                (&layer.page, "in the page"),
                (&layer.location, "in the address line"),
            ];
            for (list, applies) in lists {
                found.extend(list.iter().map(|binding| {
                    KeyBinding::new(
                        "",
                        binding.text.clone(),
                        binding.command.to_string(),
                        applies,
                    )
                }));
            }
        }
        found
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
        let bindings = Bindings::from_table(&table(text), "keys", &commands, &mut problems);
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
            Some(&Command::LocationFocus(None))
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
            bindings.find(KeyboardFocus::Page, false, &j),
            Some(&Command::TabClose(None))
        );
        // j would type, so it is left alone, in every layer.
        assert_eq!(bindings.find(KeyboardFocus::Page, true, &j), None);
        assert_eq!(bindings.find(KeyboardFocus::Chrome, true, &j), None);
        // A modifier in a lower layer still fires while a character binding is suppressed.
        let ctrl_d = press(32, character("d"), CTRL, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, true, &ctrl_d),
            Some(&Command::PageReload)
        );
        // A global binding in the higher layer beats a page binding under it.
        let ctrl_t = press(20, character("t"), CTRL, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, &ctrl_t),
            Some(&Command::BrowserQuit)
        );
        let q = press(KEY_Q, character("q"), NONE, NONE);
        assert_eq!(
            bindings.find(KeyboardFocus::Page, false, &q),
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

    /// A browser with nothing to read. `bindings` never looks.
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
