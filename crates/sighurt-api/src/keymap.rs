//! Keymaps: what a key press does, as a small trait other crates can implement.
//!
//! The core holds one keymap, the one the configuration's `[input] keymap` names, and asks it
//! about each fresh key press: pass it on to whatever has the keyboard, take it, or run a
//! command. The default keymap, `bindings`, reads `[keys]`; another keymap reads its own table,
//! `[keymap.<name>]`, which its kind's check validates when the configuration loads. Keymaps are
//! compiled into `sig` and listed in its registry, as engines are; nothing is loaded at run
//! time.
//!
//! What the core guarantees around any keymap:
//!
//! 1. A taken press takes its release and its repeats, and a passed press passes them, so no
//!    keymap can leave the page or the chrome with half a keystroke. The keymap decides only
//!    about fresh presses and about repeats of presses it took.
//! 2. Only a fresh press starts anything: a key held from before goes where its press went,
//!    without asking the keymap.
//! 3. [`KeyAction::RunThenPass`] runs the command first, then routes the press by the focus as
//!    it is after the command; the press counts as passed, so its release follows it.
//! 4. The context is the core's: [`Keymap::context`] when the keyboard moves between page and
//!    chrome or a text field takes or loses it, [`Keymap::reset`] when the window loses the
//!    keyboard.
//! 5. The keymap runs inside the core's update: it does no I/O, reads no clock and starts no
//!    thread. It sees what the user types, passwords included, and keeps no more of it than an
//!    unfinished sequence and never logs it.
//! 6. A changed [`Keymap::indicator`] redraws the chrome.
//! 7. The [`Browser`] view a press comes with is read-only and lasts that one call: it is built
//!    from the state before the press is applied. It carries no clock, handle or engine type.
//! 8. A [`KeyAction::RunAll`] runs whole, in order, in the update of its press. Commands that
//!    change the tab list change the numbering the later ones see (`tab.close 3` renumbers tab
//!    4), as typing them one after another would, so a keymap closing several tabs by number
//!    closes the highest first.
//!
//! A keymap is the browser's configuration in Rust (docs/rewrite/config-extension.md): it reads
//! what the browser shows through [`Browser`], defines its own actions as commands computed from
//! it, and shows its state in both frontends' chromes through [`Keymap::indicator`]. Titles and
//! addresses are page data: a keymap may compute with them (an address to copy, a tab to close)
//! and must not put them in its indicator or log them.
//!
//! [`Chord`] is one keystroke of the configuration's syntax, for keymaps to match key events
//! with.
//!
//! Must not own: the presses taken (the core's), where the keyboard is, or what a command does.

use std::fmt;
use std::str::FromStr;

use crate::commands::{Command, CommandSet};
use crate::ids::TabId;
use crate::input::{Key, KeyCode, KeyEvent, KeyboardFocus, Modifiers};
use crate::settings::Table;

/// What a key press happens in.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyContext {
    /// Which part of the window has the keyboard.
    pub focus: KeyboardFocus,
    /// The focused part takes typed text: the chrome's line, or a text field of the page.
    pub typing: bool,
}

impl KeyContext {
    /// A context.
    pub const fn new(focus: KeyboardFocus, typing: bool) -> Self {
        Self { focus, typing }
    }
}

/// Which frontend runs the browser.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Frontend {
    /// The Wayland window. The default, so a caller that doesn't say keeps the window's checks.
    #[default]
    Gui,
    /// The terminal.
    Terminal,
}

/// What a keymap may read of the browser while it decides one key press. Borrowed from the core
/// for that call only (guarantee 7).
pub trait Browser {
    /// The frontend that runs the browser.
    fn frontend(&self) -> Frontend;
    /// The same context [`Keymap::press`] is given.
    fn context(&self) -> KeyContext;
    /// How many tabs there are.
    fn tab_count(&self) -> usize;
    /// The index of the selected tab, counting from 0 in tab-strip order (`tab.select` counts
    /// from 1).
    fn active_tab(&self) -> usize;
    /// Tab `index`, or `None` past the end.
    fn tab(&self, index: usize) -> Option<TabInfo<'_>>;
}

/// One tab, as a keymap sees it.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabInfo<'a> {
    /// The tab.
    pub id: TabId,
    /// The page's own title, if it gave one.
    pub title: Option<&'a str>,
    /// The committed address: what the tab shows, not what is loading.
    pub url: Option<&'a str>,
    /// Whether the tab is loading.
    pub loading: bool,
    /// The engine of the page it shows; `None` for `sig:` pages, blank tabs and errors.
    pub engine: Option<&'a str>,
    /// The page zoom, 1.0 being 100 %.
    pub zoom: f64,
    /// Whether `page.back` has somewhere to go.
    pub can_go_back: bool,
    /// Whether `page.forward` has somewhere to go.
    pub can_go_forward: bool,
}

impl<'a> TabInfo<'a> {
    /// A tab with these fields, for the core and for tests outside the crate.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        id: TabId,
        title: Option<&'a str>,
        url: Option<&'a str>,
        loading: bool,
        engine: Option<&'a str>,
        zoom: f64,
        can_go_back: bool,
        can_go_forward: bool,
    ) -> Self {
        Self {
            id,
            title,
            url,
            loading,
            engine,
            zoom,
            can_go_back,
            can_go_forward,
        }
    }
}

/// What the browser does with a key press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Not the browser's: the press, its repeats and its release go to whatever has focus.
    Pass,
    /// Taken, and nothing runs: part of a sequence, or a change of mode.
    Take,
    /// Taken, and the command runs.
    Run(Command),
    /// The command runs, then the press goes to whatever has focus now, as if passed, so `:`
    /// can focus the command line and type itself into it.
    RunThenPass(Command),
    /// Taken, and these commands run in order in this update (guarantee 8). Empty is `Take`.
    RunAll(Vec<Command>),
}

/// A keymap: turns key presses into [`KeyAction`]s.
pub trait Keymap {
    /// Applies its table (`[keys]` for `bindings`, `[keymap.<name>]` otherwise), which its
    /// kind's check accepted. Called before anything else, and after every reload that keeps
    /// this keymap.
    fn configure(&mut self, settings: &Table);

    /// A fresh key press, or an auto-repeat (`key.repeat`) of a press this keymap took.
    /// Releases never come here. For a repeat, `Pass` counts as `Take`, `RunThenPass` as `Run`
    /// and `RunAll` as itself. `browser` is what the browser shows, before this press.
    fn press(&mut self, key: &KeyEvent, context: KeyContext, browser: &dyn Browser) -> KeyAction;

    /// The context changed between presses: the keyboard moved between page and chrome, or a
    /// text field took or lost it.
    fn context(&mut self, _context: KeyContext) {}

    /// The window lost the keyboard: drop an unfinished sequence.
    fn reset(&mut self) {}

    /// What the chrome shows about the keymap ("NORMAL", "NORMAL g"). The executable passes on
    /// at most 64 characters of one line.
    fn indicator(&self) -> Option<&str> {
        None
    }
}

/// A keymap a build can offer. Each keymap crate exports one, as `KIND`. Compared and printed
/// by its name.
#[derive(Clone, Copy)]
pub struct KeymapKind {
    /// Its name in the configuration's `[input] keymap`.
    pub name: &'static str,
    /// Everything wrong with its table, one line each; empty if the table is valid. Commands
    /// are parsed with `commands`, so chrome actions are checked too.
    pub check: fn(&Table, &CommandSet<'_>) -> Vec<String>,
    /// A new keymap, which the core configures before using it.
    pub new: fn() -> Box<dyn Keymap>,
}

impl fmt::Debug for KeymapKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeymapKind({})", self.name)
    }
}

impl PartialEq for KeymapKind {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
    }
}

impl Eq for KeymapKind {}

/// Whether `key` is a modifier, a lock, or a key that only starts or advances a composition (a
/// dead key, Compose). Sequences leave these out.
pub fn is_modifier(key: &KeyEvent) -> bool {
    const NAMES: [&str; 15] = [
        "Shift",
        "Control",
        "Alt",
        "AltGraph",
        "Meta",
        "Super",
        "Hyper",
        "Fn",
        "FnLock",
        "CapsLock",
        "NumLock",
        "ScrollLock",
        "Symbol",
        "SymbolLock",
        "ModeChange",
    ];
    match &key.key {
        Key::Dead(_) => true,
        Key::Named(name) => name == "Compose" || NAMES.contains(&name.as_str()),
        Key::Character(_) | Key::Unidentified => false,
    }
}

/// One keystroke: modifiers and a key, such as `ctrl-shift-t`, `alt-left` or `ctrl-code:KeyQ`.
///
/// A keystroke matches the key's meaning in the current layout (`KeyEvent::key`) unless it
/// names a physical key (`code:KeyQ`). Modifiers the layout used to produce a character don't
/// count, so AltGr + 2 is "@" on a Swedish layout and never a Ctrl+Alt shortcut, and `ctrl-+`
/// is Ctrl and whatever key types "+". For letters, shift counts: `ctrl-shift-t` (or `ctrl-T`)
/// is Ctrl+Shift+T whether or not Caps Lock is on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Chord {
    pub(crate) mods: Mods,
    pub(crate) key: ChordKey,
}

/// The modifiers a binding cares about. Caps Lock and Num Lock never matter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Mods {
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

    /// The modifiers held, with no lock on.
    pub(crate) fn modifiers(self) -> Modifiers {
        Modifiers {
            shift: self.shift,
            control: self.ctrl,
            alt: self.alt,
            alt_graph: self.alt_graph,
            logo: self.logo,
            ..Modifiers::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum ChordKey {
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

    /// Whether the keystroke would type a character (no modifier but Shift), so it must not
    /// fire while the user types.
    pub fn types(&self) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::ButtonState;

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

    #[test]
    fn keystrokes_that_type() {
        for (text, types) in [
            ("j", true),
            ("J", true),
            ("space", true),
            ("ctrl-j", false),
            ("altgr-j", false),
            ("enter", false),
            ("code:KeyJ", false),
        ] {
            assert_eq!(chord(text).types(), types, "{text}");
        }
    }

    #[test]
    fn modifiers_locks_and_compositions_are_not_keys_of_a_sequence() {
        let is = |key: Key| is_modifier(&press(0, key, NONE, NONE));
        for name in [
            "Shift", "Control", "AltGraph", "Meta", "CapsLock", "Compose",
        ] {
            assert!(is(named(name)), "{name}");
        }
        assert!(is(Key::Dead(Some('¨'))));
        assert!(!is(character("g")));
        assert!(!is(named("Enter")));
        assert!(!is(Key::Unidentified));
    }
}
