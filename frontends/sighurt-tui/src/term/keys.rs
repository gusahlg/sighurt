//! Key events shaped like `sighurt_api::input::KeyEvent`.
//!
//! This crate does not depend on `sighurt-api`. `KeyCode` is the Linux evdev
//! code. `key` is the W3C `KeyboardEvent.key` value. `text` is what the press
//! types, and it is never a control character.
//!
//! Kitty's base-layout alternate selects the evdev code through a US QWERTY
//! table. Without that alternate (Rio never sends one) a character key is
//! `KeyCode(0)`. Functional keys that kitty numbers (C0 9/13/27/127 and the
//! private-use block) still get an evdev code. Legacy terminals never report a
//! physical key, so their code stays 0, and each press is followed by a
//! synthesized release.
//!
//! An upper-case letter that arrives without Shift (a legacy byte, or Rio's
//! `CSI 74 u` for a `J` keysym) is reported with Shift held and consumed, unless
//! Caps Lock is on: that is how a layout makes it. Associated text that only
//! repeats the key while Alt is held (Rio's `CSI 120;3;120 u` for Alt+x) is an
//! Alt chord, which types nothing; text that differs from the key (an Option
//! character) consumes Alt. The keypad's character keys are characters.
//!
//! Legacy `0x1e` is reported as Ctrl+`^` and `0x1f` as Ctrl+`_`, both with
//! [`KeyEvent::ambiguous`] set. Those bytes are also Ctrl+6 and Ctrl+`-`.

use std::fmt;

/// Linux evdev code (`KEY_*`), or 0 when the physical key is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyCode(pub u32);

/// Down or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ButtonState {
    /// The key went down, or a legacy press (legacy releases are synthesized).
    Pressed,
    /// The key went up.
    Released,
}

/// W3C `KeyboardEvent.key`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character the layout produced: "a", "A", "å".
    Character(String),
    /// A named key: "Enter", "ArrowLeft", "Tab".
    Named(String),
    /// A dead key. The parser does not emit these; the variant is here so the
    /// shape matches `sighurt-api`.
    #[allow(dead_code)]
    Dead(Option<char>),
    /// No meaning in the layout.
    Unidentified,
}

/// Modifiers held when the key changed. AltGr is its own flag.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub control: bool,
    /// Alt.
    pub alt: bool,
    /// AltGr (ISO level 3). Kitty does not report this separately from Alt.
    pub alt_graph: bool,
    /// Super / logo.
    pub logo: bool,
    /// Caps Lock is on.
    pub caps_lock: bool,
    /// Num Lock is on.
    pub num_lock: bool,
}

/// One press, repeat or release.
///
/// `ambiguous` is not on `sighurt_api::input::KeyEvent`. It marks the legacy
/// bytes that share a meaning with a Ctrl chord: Tab/`0x09`/Ctrl-I,
/// Enter/`0x0d`/Ctrl-M, Backspace/`0x08`/Ctrl-H, and the `0x1e`/`0x1f` pair.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// Physical key, or [`KeyCode`]`(0)` when it is not known.
    pub code: KeyCode,
    /// W3C key value.
    pub key: Key,
    /// Down or up.
    pub state: ButtonState,
    /// Auto-repeat. Releases are never repeats.
    pub repeat: bool,
    /// Modifiers held at the change.
    pub modifiers: Modifiers,
    /// Modifiers the layout consumed to produce `key`.
    pub consumed: Modifiers,
    /// Text this press types. `None` on release, when Control or Logo is held,
    /// and for keys that do not type.
    pub text: Option<String>,
    /// Legacy encoding shared by two different keys.
    pub ambiguous: bool,
}

impl fmt::Display for KeyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "KeyCode({})", self.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyKind {
    Press,
    Repeat,
    Release,
}

/// A legacy press and the release synthesized immediately after it. An upper-case letter
/// without Caps Lock is a shifted one.
pub(crate) fn legacy_press(
    key: Key,
    mut modifiers: Modifiers,
    ambiguous: bool,
    text: Option<String>,
) -> Vec<KeyEvent> {
    let mut consumed = Modifiers::default();
    if let Key::Character(ref shown) = key {
        if !modifiers.caps_lock && upper_letter(shown) {
            modifiers.shift = true;
            consumed.shift = true;
        }
    }
    let press = KeyEvent {
        code: KeyCode(0),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed,
        text,
        ambiguous,
    };
    let release = release_of(&press, modifiers);
    vec![press, release]
}

/// The release of `press`: the same key, the modifiers held when it went up, no text.
pub(crate) fn release_of(press: &KeyEvent, modifiers: Modifiers) -> KeyEvent {
    KeyEvent {
        state: ButtonState::Released,
        repeat: false,
        modifiers,
        text: None,
        ..press.clone()
    }
}

/// Shift produced `shown` from a different key, or `shown` is a symbol Shift is held on.
/// A letter and a digit keep Shift: "J" and "shift-1" stay shifted. Space does too.
fn layout_used_shift(shown: &str, unshifted: Option<&str>) -> bool {
    if unshifted.is_some() && unshifted != Some(shown) {
        return true;
    }
    let mut chars = shown.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => !ch.is_alphanumeric() && !ch.is_whitespace(),
        _ => false,
    }
}

/// One upper-case letter that has a lower case: "J", "Å", not "1" or "ß".
fn upper_letter(text: &str) -> bool {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => ch.is_uppercase() && ch.to_lowercase().ne(ch.to_uppercase()),
        _ => false,
    }
}

/// One kitty `CSI u` event. No synthetic release: the terminal sends its own.
pub(crate) fn kitty_key(
    codepoint: u32,
    shifted: Option<u32>,
    base_layout: Option<u32>,
    modifiers: Modifiers,
    kind: KeyKind,
    text_points: &[u32],
    alt_prefix: bool,
) -> KeyEvent {
    let functional = functional(codepoint);
    let mut modifiers = modifiers;
    let mut text = text_from_points(text_points);
    if kind == KeyKind::Release || modifiers.control || modifiers.logo {
        text = None;
    }

    let unshifted = char::from_u32(codepoint).filter(|ch| !ch.is_control());
    let mut key = match functional.map(|(name, _)| name) {
        // The keypad's digits and operators.
        Some(name) if name.chars().count() == 1 => Key::Character(name.to_string()),
        Some(name) => Key::Named(name.to_string()),
        None => match unshifted {
            Some(ch) => Key::Character(ch.to_string()),
            None => Key::Unidentified,
        },
    };

    let mut consumed = Modifiers::default();
    // An upper-case key without Shift: the terminal sent the keysym, not the key.
    let upper = matches!(key, Key::Character(ref shown) if upper_letter(shown));
    if upper && functional.is_none() && !modifiers.shift && !modifiers.caps_lock {
        modifiers.shift = true;
        consumed.shift = true;
    }

    if modifiers.shift && functional.is_none() {
        if let Some(ch) = shifted.and_then(char::from_u32) {
            key = Key::Character(ch.to_string());
        } else if let Some(ch) = unshifted {
            if ch.is_ascii_lowercase() {
                key = Key::Character(ch.to_ascii_uppercase().to_string());
            }
        }
    }

    // Alt with text that is only the key itself: an Alt chord, which types nothing.
    if modifiers.alt && text.is_some() && text_is_key(text.as_deref(), &key) {
        text = None;
    }

    if let Some(ref typed) = text {
        if !typed.is_empty() {
            key = Key::Character(typed.clone());
        }
    }

    if modifiers.shift {
        if let Key::Character(ref shown) = key {
            let plain = unshifted.map(|ch| ch.to_string());
            // A letter keeps Shift, so "J" still matches. Anything else Shift produced
            // (":", "+", "!") must match the unshifted chord.
            if layout_used_shift(shown, plain.as_deref()) {
                consumed.shift = true;
            }
        }
    }
    if modifiers.alt && text.is_some() {
        consumed.alt = true;
    }
    if alt_prefix {
        modifiers.alt = true;
        text = None;
        consumed.alt = false;
    }

    let code = if let Some((_, evdev)) = functional {
        KeyCode(evdev)
    } else if let Some(evdev) = base_layout.and_then(char::from_u32).and_then(us_evdev) {
        KeyCode(evdev)
    } else {
        KeyCode(0)
    };

    KeyEvent {
        code,
        key,
        state: match kind {
            KeyKind::Release => ButtonState::Released,
            KeyKind::Press | KeyKind::Repeat => ButtonState::Pressed,
        },
        repeat: kind == KeyKind::Repeat,
        modifiers,
        consumed,
        text,
        ambiguous: false,
    }
}

/// Kitty modifier field: 1 plus a bitset. Hyper and meta have no flag here.
pub(crate) fn kitty_modifiers(field: u32) -> Modifiers {
    let bits = field.saturating_sub(1);
    Modifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        control: bits & 4 != 0,
        logo: bits & 8 != 0,
        caps_lock: bits & 64 != 0,
        num_lock: bits & 128 != 0,
        alt_graph: false,
    }
}

/// Whether `text` is what `key` types without Alt, case aside when Caps Lock made it.
fn text_is_key(text: Option<&str>, key: &Key) -> bool {
    match (text, key) {
        (Some(text), Key::Character(shown)) => {
            text == shown || text.to_lowercase() == shown.to_lowercase()
        }
        _ => false,
    }
}

fn text_from_points(points: &[u32]) -> Option<String> {
    let mut text = String::new();
    for point in points {
        let Some(ch) = char::from_u32(*point) else {
            continue;
        };
        if ch.is_control() {
            continue;
        }
        text.push(ch);
    }
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// US QWERTY glyph, shifted or not, to an evdev code.
pub(crate) fn us_evdev(ch: char) -> Option<u32> {
    Some(match ch {
        '1' | '!' => 2,
        '2' | '@' => 3,
        '3' | '#' => 4,
        '4' | '$' => 5,
        '5' | '%' => 6,
        '6' | '^' => 7,
        '7' | '&' => 8,
        '8' | '*' => 9,
        '9' | '(' => 10,
        '0' | ')' => 11,
        '-' | '_' => 12,
        '=' | '+' => 13,
        'q' | 'Q' => 16,
        'w' | 'W' => 17,
        'e' | 'E' => 18,
        'r' | 'R' => 19,
        't' | 'T' => 20,
        'y' | 'Y' => 21,
        'u' | 'U' => 22,
        'i' | 'I' => 23,
        'o' | 'O' => 24,
        'p' | 'P' => 25,
        '[' | '{' => 26,
        ']' | '}' => 27,
        'a' | 'A' => 30,
        's' | 'S' => 31,
        'd' | 'D' => 32,
        'f' | 'F' => 33,
        'g' | 'G' => 34,
        'h' | 'H' => 35,
        'j' | 'J' => 36,
        'k' | 'K' => 37,
        'l' | 'L' => 38,
        ';' | ':' => 39,
        '\'' | '"' => 40,
        '`' | '~' => 41,
        '\\' | '|' => 43,
        'z' | 'Z' => 44,
        'x' | 'X' => 45,
        'c' | 'C' => 46,
        'v' | 'V' => 47,
        'b' | 'B' => 48,
        'n' | 'N' => 49,
        'm' | 'M' => 50,
        ',' | '<' => 51,
        '.' | '>' => 52,
        '/' | '?' => 53,
        ' ' => 57,
        _ => return None,
    })
}

/// The evdev code of a named key that kitty sends in a `CSI` letter or `~` form (Tab, the
/// arrows, Home, End, paging, Insert, Delete, F1–F12, the keypad's middle key), or 0.
pub(crate) fn named_evdev(name: &str) -> u32 {
    (57348..=57375)
        .chain([9, 13, 27, 127, 57427])
        .filter_map(functional)
        .find(|(known, _)| *known == name)
        .map_or(0, |(_, evdev)| evdev)
}

/// Kitty's functional number, the W3C name, and the evdev code (0 if none).
///
/// Kitty's private-use numbers for `CSI u`: Insert is 57348, Caps Lock 57358,
/// F12 57375, F13 57376, then the keypad and media keys through 57454. The
/// legacy CSI letter and `~` forms name the same keys.
fn functional(code: u32) -> Option<(&'static str, u32)> {
    Some(match code {
        9 => ("Tab", 15),
        13 => ("Enter", 28),
        27 => ("Escape", 1),
        127 => ("Backspace", 14),
        57348 => ("Insert", 110),
        57349 => ("Delete", 111),
        57350 => ("ArrowLeft", 105),
        57351 => ("ArrowRight", 106),
        57352 => ("ArrowUp", 103),
        57353 => ("ArrowDown", 108),
        57354 => ("PageUp", 104),
        57355 => ("PageDown", 109),
        57356 => ("Home", 102),
        57357 => ("End", 107),
        57358 => ("CapsLock", 58),
        57359 => ("ScrollLock", 70),
        57360 => ("NumLock", 69),
        57361 => ("PrintScreen", 99),
        57362 => ("Pause", 119),
        57363 => ("ContextMenu", 127),
        57364 => ("F1", 59),
        57365 => ("F2", 60),
        57366 => ("F3", 61),
        57367 => ("F4", 62),
        57368 => ("F5", 63),
        57369 => ("F6", 64),
        57370 => ("F7", 65),
        57371 => ("F8", 66),
        57372 => ("F9", 67),
        57373 => ("F10", 68),
        57374 => ("F11", 87),
        57375 => ("F12", 88),
        57376..=57387 => {
            let n = code - 57376 + 13;
            return Some(function_name(n, 183 + (n - 13)));
        }
        57388..=57398 => return Some(function_name(code - 57376 + 13, 0)),
        57399 => ("0", 82),
        57400 => ("1", 79),
        57401 => ("2", 80),
        57402 => ("3", 81),
        57403 => ("4", 75),
        57404 => ("5", 76),
        57405 => ("6", 77),
        57406 => ("7", 71),
        57407 => ("8", 72),
        57408 => ("9", 73),
        57409 => (".", 83),
        57410 => ("/", 98),
        57411 => ("*", 55),
        57412 => ("-", 74),
        57413 => ("+", 78),
        57414 => ("Enter", 96),
        57415 => ("=", 117),
        57416 => (",", 121),
        57417 => ("ArrowLeft", 105),
        57418 => ("ArrowRight", 106),
        57419 => ("ArrowUp", 103),
        57420 => ("ArrowDown", 108),
        57421 => ("PageUp", 104),
        57422 => ("PageDown", 109),
        57423 => ("Home", 102),
        57424 => ("End", 107),
        57425 => ("Insert", 110),
        57426 => ("Delete", 111),
        57427 => ("Clear", 76),
        57428 => ("MediaPlay", 207),
        57429 => ("MediaPause", 0),
        57430 => ("MediaPlayPause", 164),
        57431 => ("MediaReverse", 0),
        57432 => ("MediaStop", 166),
        57433 => ("MediaFastForward", 208),
        57434 => ("MediaRewind", 168),
        57435 => ("MediaTrackNext", 163),
        57436 => ("MediaTrackPrevious", 165),
        57437 => ("MediaRecord", 167),
        57438 => ("AudioVolumeDown", 114),
        57439 => ("AudioVolumeUp", 115),
        57440 => ("AudioVolumeMute", 113),
        57441 => ("Shift", 42),
        57442 => ("Control", 29),
        57443 => ("Alt", 56),
        57444 => ("Meta", 125),
        57445 => ("Hyper", 0),
        57446 => ("Meta", 0),
        57447 => ("Shift", 54),
        57448 => ("Control", 97),
        57449 => ("Alt", 100),
        57450 => ("Meta", 126),
        57451 => ("Hyper", 0),
        57452 => ("Meta", 0),
        57453 => ("AltGraph", 100),
        57454 => ("AltGraph", 0),
        _ => return None,
    })
}

pub(crate) fn to_api(event: KeyEvent) -> sighurt_api::input::KeyEvent {
    sighurt_api::input::KeyEvent {
        code: sighurt_api::input::KeyCode(event.code.0),
        key: match event.key {
            Key::Character(text) => sighurt_api::input::Key::Character(text),
            Key::Named(name) => sighurt_api::input::Key::Named(name),
            Key::Dead(ch) => sighurt_api::input::Key::Dead(ch),
            Key::Unidentified => sighurt_api::input::Key::Unidentified,
        },
        state: match event.state {
            ButtonState::Pressed => sighurt_api::input::ButtonState::Pressed,
            ButtonState::Released => sighurt_api::input::ButtonState::Released,
        },
        repeat: event.repeat,
        modifiers: to_api_modifiers(event.modifiers),
        consumed: to_api_modifiers(event.consumed),
        text: event.text,
    }
}

pub(crate) fn to_api_modifiers(modifiers: Modifiers) -> sighurt_api::input::Modifiers {
    sighurt_api::input::Modifiers {
        shift: modifiers.shift,
        control: modifiers.control,
        alt: modifiers.alt,
        alt_graph: modifiers.alt_graph,
        logo: modifiers.logo,
        caps_lock: modifiers.caps_lock,
        num_lock: modifiers.num_lock,
    }
}

fn function_name(n: u32, evdev: u32) -> (&'static str, u32) {
    static NAMES: [&str; 23] = [
        "F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "F25",
        "F26", "F27", "F28", "F29", "F30", "F31", "F32", "F33", "F34", "F35",
    ];
    (NAMES[(n - 13) as usize], evdev)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn us_table_covers_the_letter_row() {
        assert_eq!(us_evdev('a'), Some(30));
        assert_eq!(us_evdev('A'), Some(30));
        assert_eq!(us_evdev('!'), Some(2));
        assert_eq!(us_evdev(' '), Some(57));
        assert_eq!(us_evdev('å'), None);
    }

    #[test]
    fn shift_a_consumes_shift_and_uses_the_base_layout() {
        let ev = kitty_key(
            b'a' as u32,
            Some(b'A' as u32),
            Some(b'a' as u32),
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            KeyKind::Press,
            &[b'A' as u32],
            false,
        );
        assert_eq!(ev.code, KeyCode(30));
        assert_eq!(ev.key, Key::Character("A".into()));
        assert!(ev.consumed.shift);
        assert_eq!(ev.text.as_deref(), Some("A"));
        assert!(!ev.ambiguous);
    }

    #[test]
    fn a_shifted_symbol_matches_its_unshifted_chord() {
        // The codepoint is already "+" and Shift is still set: the layout used it.
        let plus = kitty_key(
            b'+' as u32,
            None,
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            KeyKind::Press,
            &[b'+' as u32],
            false,
        );
        assert_eq!(plus.key, Key::Character("+".into()));
        assert!(plus.consumed.shift);
        assert_eq!(plus.text.as_deref(), Some("+"));

        // A letter keeps Shift, including when the text is still the lower case.
        let jay = kitty_key(
            b'j' as u32,
            None,
            None,
            Modifiers {
                shift: true,
                ..Modifiers::default()
            },
            KeyKind::Press,
            &[b'j' as u32],
            false,
        );
        assert_eq!(jay.key, Key::Character("j".into()));
        assert!(jay.modifiers.shift && !jay.consumed.shift);
    }

    #[test]
    fn functional_table_covers_kitty_keys() {
        assert_eq!(functional(13).unwrap().0, "Enter");
        assert_eq!(functional(57348).unwrap(), ("Insert", 110u32));
        assert_eq!(functional(57350).unwrap().0, "ArrowLeft");
        assert_eq!(functional(57358).unwrap().0, "CapsLock");
        assert_eq!(functional(57364).unwrap().0, "F1");
        assert_eq!(functional(57375).unwrap().0, "F12");
        assert_eq!(functional(57376).unwrap().0, "F13");
        assert_eq!(functional(57387).unwrap().0, "F24");
        assert_eq!(functional(57388).unwrap().1, 0, "F25 has no evdev code");
        assert_eq!(functional(57404).unwrap().0, "5");
        assert_eq!(functional(57414).unwrap(), ("Enter", 96u32));
        assert_eq!(functional(57454).unwrap().0, "AltGraph");
        assert!(functional(57347).is_none());
        assert!(functional(57455).is_none());
    }
}
