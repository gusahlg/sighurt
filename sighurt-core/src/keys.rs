//! Key bindings: the keystrokes of the user's `[keys]` configuration, matched against the key
//! events the host normalized (`sighurt_api::input::KeyEvent`).
//!
//! Bindings come only from the configuration: there are none by default and no key handlers in
//! code. `[keys]` applies everywhere, `[keys.page]` while the page has focus and
//! `[keys.location]` while the address bar has; a binding of the focused part wins over a
//! global one.
//!
//! A binding matches the key's meaning in the current layout (`KeyEvent::key`) unless it names a
//! physical key (`code:KeyQ`). Modifiers the layout used to produce a character don't count, so
//! AltGr + 2 is "@" on a Swedish layout and never a Ctrl+Alt shortcut, and `ctrl-+` is Ctrl and
//! whatever key types "+". For letters, shift counts: `ctrl-shift-t` (or `ctrl-T`) is Ctrl+Shift+T
//! whether or not Caps Lock is on. While the user types, in the address bar or a page's text
//! field, a binding that would type a character (no modifier but Shift) lets the key type.
//!
//! A press the browser consumes also consumes its release and its auto-repeats, so the page or
//! the address bar never sees half a keystroke. Only a fresh press starts a binding: a key held
//! down before a modifier joined it keeps repeating where its press went.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use sighurt_api::commands::Command;
use sighurt_api::ids::SeatId;
use sighurt_api::input::{ButtonState, Key, KeyCode, KeyEvent, Modifiers};

use crate::state::Focus;

/// One keystroke: modifiers and a key, such as `ctrl-shift-t`, `alt-left` or `ctrl-code:KeyQ`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    mods: Mods,
    key: ChordKey,
}

/// The modifiers a binding cares about. Caps Lock and Num Lock never matter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
struct Mods {
    ctrl: bool,
    alt: bool,
    shift: bool,
    logo: bool,
    alt_graph: bool,
}

impl Mods {
    fn held(held: Modifiers, consumed: Modifiers) -> Self {
        Self {
            ctrl: held.control && !consumed.control,
            alt: held.alt && !consumed.alt,
            shift: held.shift && !consumed.shift,
            logo: held.logo && !consumed.logo,
            alt_graph: held.alt_graph && !consumed.alt_graph,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum ChordKey {
    /// What the key types. A letter is kept in lower case; whether it was upper case is Shift.
    Character(String),
    /// A key that types nothing, by its W3C key name ("Enter", "ArrowLeft", "F5").
    Named(String),
    /// A physical key, wherever the layout puts its meaning.
    Code(KeyCode),
}

/// Key names accepted in lower case, and the W3C key names they stand for.
const KEY_NAMES: [(&str, &str); 16] = [
    ("enter", "Enter"),
    ("escape", "Escape"),
    ("esc", "Escape"),
    ("tab", "Tab"),
    ("backspace", "Backspace"),
    ("delete", "Delete"),
    ("insert", "Insert"),
    ("home", "Home"),
    ("end", "End"),
    ("pageup", "PageUp"),
    ("pagedown", "PageDown"),
    ("left", "ArrowLeft"),
    ("right", "ArrowRight"),
    ("up", "ArrowUp"),
    ("down", "ArrowDown"),
    ("menu", "ContextMenu"),
];

/// The lower-case form of `text` if it is one letter that has upper and lower case.
fn letter(text: &str) -> Option<String> {
    let mut chars = text.chars();
    let c = chars.next()?;
    let cased = chars.next().is_none() && c.to_lowercase().ne(c.to_uppercase());
    cased.then(|| c.to_lowercase().collect())
}

impl Chord {
    /// Whether `event` is this keystroke.
    pub fn matches(&self, event: &KeyEvent) -> bool {
        let all = Mods::held(event.modifiers, Modifiers::default());
        match &self.key {
            ChordKey::Code(code) => event.code == *code && self.mods == all,
            ChordKey::Named(name) => {
                matches!(&event.key, Key::Named(key) if key == name) && self.mods == all
            }
            ChordKey::Character(character) => {
                let Key::Character(typed) = &event.key else {
                    return false;
                };
                let mut mods = Mods::held(event.modifiers, event.consumed);
                match letter(typed) {
                    Some(lower) => {
                        mods.shift = event.modifiers.shift;
                        lower == *character && self.mods == mods
                    }
                    None => typed == character && self.mods == mods,
                }
            }
        }
    }

    /// Whether the keystroke would type a character, so it must not fire while the user types.
    fn types(&self) -> bool {
        let Mods {
            ctrl,
            alt,
            logo,
            alt_graph,
            ..
        } = self.mods;
        matches!(self.key, ChordKey::Character(_)) && !(ctrl || alt || logo || alt_graph)
    }
}

impl FromStr for Chord {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        if text.trim().contains(char::is_whitespace) {
            return Err("key sequences such as \"g g\" are not supported".into());
        }
        // The key comes after the last "-", and "-" is a key too: "ctrl--", "-".
        let (modifiers, key) = match text.strip_suffix("--") {
            Some(modifiers) => (modifiers, "-"),
            None if text == "-" => ("", "-"),
            None => text.rsplit_once('-').unwrap_or(("", text)),
        };
        let mut mods = Mods::default();
        for modifier in modifiers.split('-').filter(|_| !modifiers.is_empty()) {
            let held = match modifier {
                // `secondary` is how configurations for the GPUI-based sig said ctrl.
                "ctrl" | "secondary" => &mut mods.ctrl,
                "alt" => &mut mods.alt,
                "shift" => &mut mods.shift,
                "super" => &mut mods.logo,
                "altgr" => &mut mods.alt_graph,
                other => {
                    return Err(format!(
                        "unknown modifier {other:?} (use ctrl, alt, shift, super or altgr)"
                    ))
                }
            };
            *held = true;
        }
        let key = if let Some(code) = key.strip_prefix("code:") {
            let code = (0..=255)
                .map(KeyCode)
                .find(|candidate| candidate.w3c_code() == Some(code))
                .ok_or_else(|| format!("unknown key code {code:?}"))?;
            ChordKey::Code(code)
        } else if let Some(lower) = letter(key) {
            mods.shift |= key != lower;
            ChordKey::Character(lower)
        } else if key.chars().count() == 1 && !key.starts_with(char::is_control) {
            ChordKey::Character(key.to_owned())
        } else if key == "space" {
            ChordKey::Character(" ".into())
        } else if let Some((_, name)) = KEY_NAMES.iter().find(|(alias, _)| *alias == key) {
            ChordKey::Named((*name).into())
        } else if let Some(n) = key.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
            match n {
                1..=24 => ChordKey::Named(format!("F{n}")),
                _ => return Err(format!("there is no key {key:?}")),
            }
        } else if key.starts_with(|c: char| c.is_ascii_uppercase())
            && key.len() > 1
            && key.chars().all(|c| c.is_ascii_alphanumeric())
        {
            // A W3C key name as it is, such as "BrowserBack" or "AudioVolumeUp".
            ChordKey::Named(key.into())
        } else if key.is_empty() {
            return Err("the keystroke names no key".into());
        } else {
            return Err(format!("unknown key {key:?}"));
        };
        Ok(Self { mods, key })
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Mods {
            ctrl,
            alt,
            shift,
            logo,
            alt_graph,
        } = self.mods;
        let names = [
            (ctrl, "ctrl-"),
            (alt, "alt-"),
            (shift, "shift-"),
            (logo, "super-"),
            (alt_graph, "altgr-"),
        ];
        for (_, name) in names.iter().filter(|(held, _)| *held) {
            f.write_str(name)?;
        }
        match &self.key {
            ChordKey::Character(c) if c == " " => f.write_str("space"),
            ChordKey::Character(c) | ChordKey::Named(c) => f.write_str(c),
            ChordKey::Code(code) => write!(f, "code:{}", code.w3c_code().unwrap_or_default()),
        }
    }
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
    /// Reads the `[keys]` table, adding what is wrong with it to `problems`.
    pub(crate) fn from_table(keys: &toml::Table, problems: &mut Vec<String>) -> Self {
        let mut bindings = Self::default();
        for (key, value) in keys {
            match value {
                toml::Value::String(command) => {
                    add(&mut bindings.global, "[keys]", key, command, problems)
                }
                toml::Value::Table(table) => {
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
                            Some(command) => add(list, section, key, command, problems),
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
    pub fn find(&self, focus: Focus, typing: bool, event: &KeyEvent) -> Option<&Command> {
        let context = match focus {
            Focus::Page => &self.page,
            Focus::Location => &self.location,
        };
        context
            .iter()
            .chain(&self.global)
            .find(|binding| !(typing && binding.chord.types()) && binding.chord.matches(event))
            .map(|binding| &binding.command)
    }
}

/// Parses one binding into `list`, or says what is wrong with it.
fn add(
    list: &mut Vec<Binding>,
    section: &str,
    text: &str,
    command: &str,
    problems: &mut Vec<String>,
) {
    let chord = match text.parse::<Chord>() {
        Ok(chord) => chord,
        Err(error) => return problems.push(format!("{section} {text:?}: {error}")),
    };
    let command = match command.parse::<Command>() {
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
    /// It is bound: run the command. The key goes nowhere else.
    Run(Command),
    /// The release or auto-repeat of a press the browser consumed: drop it.
    Consumed,
    /// Not the browser's: deliver it to whatever has focus.
    Deliver,
}

/// The presses the browser consumed and whose releases it still owes nobody, per seat.
#[derive(Debug, Default)]
pub(crate) struct KeyState {
    consumed: HashSet<(SeatId, KeyCode)>,
}

impl KeyState {
    /// Decides what happens to `event`, a key event from `seat`.
    pub(crate) fn handle(
        &mut self,
        bindings: &Bindings,
        focus: Focus,
        typing: bool,
        seat: SeatId,
        event: &KeyEvent,
    ) -> KeyOutcome {
        let key = (seat, event.code);
        match event.state {
            ButtonState::Released if self.consumed.remove(&key) => KeyOutcome::Consumed,
            ButtonState::Released => KeyOutcome::Deliver,
            // Held since before it matched a binding (R held, then Ctrl): only a fresh press
            // starts one, so its repeats and release go where its press went.
            ButtonState::Pressed if event.repeat && !self.consumed.contains(&key) => {
                KeyOutcome::Deliver
            }
            ButtonState::Pressed => match bindings.find(focus, typing, event) {
                Some(command) => {
                    self.consumed.insert(key);
                    KeyOutcome::Run(command.clone())
                }
                // Held down after its binding stopped matching (a modifier was let go).
                None if event.repeat && self.consumed.contains(&key) => KeyOutcome::Consumed,
                None => {
                    self.consumed.remove(&key);
                    KeyOutcome::Deliver
                }
            },
        }
    }

    /// The window lost the keyboard: no releases will come for what is held.
    pub(crate) fn forget(&mut self, seat: SeatId) {
        self.consumed.retain(|(held, _)| *held != seat);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        text.parse().unwrap_or_else(|e| panic!("{text:?}: {e}"))
    }

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
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..NONE
    };
    const CTRL_SHIFT: Modifiers = Modifiers {
        control: true,
        shift: true,
        ..NONE
    };
    const ALT_GR: Modifiers = Modifiers {
        alt_graph: true,
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

    fn named(name: &str) -> Key {
        Key::Named(name.into())
    }

    const KEY_T: u32 = 20;
    const KEY_2: u32 = 3;
    const KEY_TAB: u32 = 15;
    const KEY_EQUAL: u32 = 13;
    const KEY_MINUS: u32 = 12;
    const KEY_Q: u32 = 16;
    const KEY_R: u32 = 19;
    const KEY_A: u32 = 30;
    const KEY_LEFT: u32 = 105;

    #[test]
    fn parses_keystrokes() {
        assert_eq!(chord("ctrl-t").to_string(), "ctrl-t");
        assert_eq!(chord("ctrl-T"), chord("ctrl-shift-t"));
        assert_eq!(chord("secondary-t"), chord("ctrl-t"));
        assert_eq!(chord("ctrl--").to_string(), "ctrl--");
        assert_eq!(chord("-").to_string(), "-");
        assert_eq!(chord("alt-left").to_string(), "alt-ArrowLeft");
        assert_eq!(chord("f5"), chord("F5"));
        assert_eq!(chord("space").to_string(), "space");
        assert_eq!(chord("ctrl-code:KeyQ").to_string(), "ctrl-code:KeyQ");
        assert_eq!(chord("BrowserBack").to_string(), "BrowserBack");
        assert_eq!(chord("altgr-Å").to_string(), "shift-altgr-å");
    }

    #[test]
    fn rejects_bad_keystrokes() {
        for (text, error) in [
            ("g g", "sequences"),
            ("ctrl-", "no key"),
            ("hyper-t", "unknown modifier"),
            ("ctrl--t", "unknown modifier"),
            ("ctrl-code:KeyZZ", "unknown key code"),
            ("f25", "no key"),
            ("ctrl-pgup", "unknown key"),
        ] {
            let result = text.parse::<Chord>();
            assert!(
                result.as_ref().is_err_and(|e| e.contains(error)),
                "{text:?}: {result:?}"
            );
        }
    }

    #[test]
    fn letters_match_with_their_shift_state() {
        let ctrl_t = press(KEY_T, character("t"), CTRL, NONE);
        let ctrl_shift_t = press(KEY_T, character("T"), CTRL_SHIFT, SHIFT);
        // Caps Lock makes the key "T" without Shift.
        let caps = Modifiers {
            caps_lock: true,
            ..CTRL
        };
        let ctrl_t_caps = press(KEY_T, character("T"), caps, NONE);
        assert!(chord("ctrl-t").matches(&ctrl_t));
        assert!(chord("ctrl-t").matches(&ctrl_t_caps));
        assert!(!chord("ctrl-t").matches(&ctrl_shift_t));
        assert!(chord("ctrl-shift-t").matches(&ctrl_shift_t));
        assert!(chord("ctrl-T").matches(&ctrl_shift_t));
        assert!(!chord("ctrl-shift-t").matches(&ctrl_t));
    }

    #[test]
    fn characters_match_what_the_layout_types() {
        // US: Shift + = types "+", with Shift consumed.
        let us_plus = press(KEY_EQUAL, character("+"), CTRL_SHIFT, SHIFT);
        // Swedish: + has a key of its own, left of the backspace key.
        let se_plus = press(KEY_MINUS, character("+"), CTRL, NONE);
        for event in [&us_plus, &se_plus] {
            assert!(chord("ctrl-+").matches(event));
            assert!(!chord("ctrl-=").matches(event));
        }
        // The physical key is the = key on US layouts whatever it types.
        assert!(chord("ctrl-shift-code:Equal").matches(&us_plus));
        assert!(!chord("ctrl-code:Equal").matches(&us_plus));
        assert!(!chord("ctrl-shift-code:Equal").matches(&se_plus));
    }

    #[test]
    fn altgr_is_never_ctrl_alt() {
        // Swedish AltGr + 2 types "@".
        let at = press(KEY_2, character("@"), ALT_GR, ALT_GR);
        assert!(!chord("ctrl-alt-2").matches(&at));
        assert!(!chord("ctrl-alt-@").matches(&at));
        assert!(!chord("altgr-2").matches(&at));
        assert!(chord("@").matches(&at));
        // Ctrl + Alt + 2, where the layout types nothing else, is Ctrl + Alt.
        let ctrl_alt = Modifiers { alt: true, ..CTRL };
        assert!(chord("ctrl-alt-2").matches(&press(KEY_2, character("2"), ctrl_alt, NONE)));
        // AltGr on a key that types nothing must be asked for.
        let left = press(KEY_LEFT, named("ArrowLeft"), ALT_GR, NONE);
        assert!(!chord("left").matches(&left));
        assert!(chord("altgr-left").matches(&left));
    }

    #[test]
    fn named_keys_keep_their_modifiers() {
        // xkb consumes Shift on Tab (it becomes ISO_Left_Tab), but Shift+Tab is still shifted.
        let shift_tab = press(KEY_TAB, named("Tab"), CTRL_SHIFT, SHIFT);
        assert!(chord("ctrl-shift-tab").matches(&shift_tab));
        assert!(!chord("ctrl-tab").matches(&shift_tab));
        assert!(chord("ctrl-tab").matches(&press(KEY_TAB, named("Tab"), CTRL, NONE)));
    }

    #[test]
    fn physical_keys_ignore_the_layout() {
        // On a Dvorak layout the key where Q is on QWERTY types "'".
        let event = press(KEY_Q, character("'"), CTRL, NONE);
        assert!(chord("ctrl-code:KeyQ").matches(&event));
        assert!(!chord("ctrl-q").matches(&event));
    }

    fn bindings(text: &str) -> Result<Bindings, Vec<String>> {
        let mut problems = Vec::new();
        let bindings = Bindings::from_table(&text.parse().unwrap(), &mut problems);
        problems.is_empty().then_some(bindings).ok_or(problems)
    }

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
            bindings.find(Focus::Page, false, &ctrl_l),
            Some(&Command::PageReload)
        );
        assert_eq!(
            bindings.find(Focus::Location, true, &ctrl_l),
            Some(&Command::LocationFocus)
        );
        let j = press(36, character("j"), NONE, NONE);
        assert_eq!(
            bindings.find(Focus::Page, false, &j),
            Some(&Command::TabNext)
        );
        assert_eq!(bindings.find(Focus::Page, true, &j), None);
        assert_eq!(bindings.find(Focus::Location, true, &j), None);
    }

    #[test]
    fn bad_bindings_are_reported() {
        let problems = bindings(
            r#"
            "ctrl-t" = "tab.nwe"
            "ctrl-T" = "tab.new"
            "ctrl-shift-t" = "tab.close"
            "g g" = "location.focus"
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
    fn consumed_presses_consume_their_releases() {
        let bindings = bindings(r#""ctrl-a" = "tab.new""#).unwrap();
        let seat = SeatId::next();
        let mut keys = KeyState::default();
        let mut handle = |event: &KeyEvent| keys.handle(&bindings, Focus::Page, false, seat, event);
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
        let bindings = bindings(r#""ctrl-r" = "page.reload""#).unwrap();
        let seat = SeatId::next();
        let mut keys = KeyState::default();
        let mut handle = |event: &KeyEvent| keys.handle(&bindings, Focus::Page, false, seat, event);
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
        let bindings = bindings(r#""ctrl-a" = "tab.new""#).unwrap();
        let seat = SeatId::next();
        let mut keys = KeyState::default();
        let down = press(KEY_A, character("a"), CTRL, NONE);
        let up = KeyEvent {
            state: ButtonState::Released,
            modifiers: NONE,
            ..down.clone()
        };
        keys.handle(&bindings, Focus::Page, false, seat, &down);
        keys.forget(seat);
        assert_eq!(
            keys.handle(&bindings, Focus::Page, false, seat, &up),
            KeyOutcome::Deliver
        );
    }
}
