//! Keyboard input for guests: the key codes of the guest input ABI, from the host's key events.
//!
//! Typed text is not made here: it is the key event's committed `text` (or an input method's
//! commit), which already went through the layout, dead keys and compose.
//!
//! Owns: the mapping to the SDK's `KEY_*` codes. Must not own: shortcut policy, which the
//! browser applied before the page got the key.

use sighurt_api::input::{Key, KeyCode, Modifiers};

/// Guest key code for a key, as polled through `api_key_down` / `api_key_pressed` (the SDK's
/// `KEY_*` constants). Letters and digits follow the layout (`key`), falling back to the physical
/// key (`code`) when the character isn't one (Shift+1 types "!"). Keys without a guest code are
/// not forwarded to the guest.
pub fn key_code(key: &Key, code: KeyCode) -> Option<u32> {
    let named = match key {
        Key::Named(name) => name.as_str(),
        Key::Character(text) if text == " " => " ",
        Key::Character(text) => return char_code(text).or_else(|| physical(code)),
        Key::Dead(_) | Key::Unidentified => return physical(code),
    };
    Some(match named {
        "Enter" => 36,
        "Escape" => 37,
        "Tab" => 38,
        "Backspace" => 39,
        "Delete" => 40,
        " " => 41,
        "ArrowUp" => 42,
        "ArrowDown" => 43,
        "ArrowLeft" => 44,
        "ArrowRight" => 45,
        "Home" => 46,
        "End" => 47,
        "PageUp" => 48,
        "PageDown" => 49,
        _ => return None,
    })
}

/// The guest code of the letter or digit key at `code`'s position on a US layout.
fn physical(code: KeyCode) -> Option<u32> {
    let name = code.w3c_code()?;
    char_code(
        name.strip_prefix("Key")
            .or_else(|| name.strip_prefix("Digit"))?,
    )
}

/// Code of a single letter (0–25) or digit (26–35).
fn char_code(s: &str) -> Option<u32> {
    let mut chars = s.chars();
    let c = chars.next()?.to_ascii_lowercase();
    match (c, chars.next()) {
        ('a'..='z', None) => Some(c as u32 - 'a' as u32),
        ('0'..='9', None) => Some(26 + c as u32 - '0' as u32),
        _ => None,
    }
}

/// The guest's modifier flags (`api_modifiers`): Shift, Ctrl and Alt. The Super key counts as
/// Ctrl, as Cmd did on macOS; AltGr is not Alt.
pub fn guest_modifiers(modifiers: Modifiers) -> (bool, bool, bool) {
    (
        modifiers.shift,
        modifiers.control || modifiers.logo,
        modifiers.alt,
    )
}

/// Whether no modifier that changes a key's meaning is held.
pub fn plain(modifiers: Modifiers) -> bool {
    !(modifiers.shift
        || modifiers.control
        || modifiers.alt
        || modifiers.alt_graph
        || modifiers.logo)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: KeyCode = KeyCode(30);
    const KEY_Q: KeyCode = KeyCode(16);
    const DIGIT_1: KeyCode = KeyCode(2);
    const DIGIT_0: KeyCode = KeyCode(11);

    fn char(s: &str) -> Key {
        Key::Character(s.into())
    }

    fn named(s: &str) -> Key {
        Key::Named(s.into())
    }

    #[test]
    fn key_codes_match_the_sdk_constants() {
        assert_eq!(key_code(&char("a"), KEY_A), Some(0));
        assert_eq!(key_code(&char("z"), KeyCode(44)), Some(25));
        assert_eq!(key_code(&char("0"), DIGIT_0), Some(26));
        assert_eq!(key_code(&char("9"), KeyCode(10)), Some(35));
        assert_eq!(key_code(&named("Enter"), KeyCode(96)), Some(36));
        assert_eq!(key_code(&char(" "), KeyCode(57)), Some(41));
        assert_eq!(key_code(&named("ArrowLeft"), KeyCode(105)), Some(44));
        assert_eq!(key_code(&named("PageDown"), KeyCode(109)), Some(49));
    }

    #[test]
    fn shifted_and_foreign_characters_fall_back_to_the_physical_key() {
        assert_eq!(key_code(&char("A"), KEY_A), Some(0));
        assert_eq!(key_code(&char("!"), DIGIT_1), Some(27));
        assert_eq!(key_code(&char("ф"), KEY_A), Some(0));
        // A dead key on a letter's position, and the Swedish å on BracketLeft.
        assert_eq!(key_code(&Key::Dead(Some('¨')), KEY_A), Some(0));
        assert_eq!(key_code(&char("å"), KeyCode(26)), None);
        // The layout wins where it types a letter (AZERTY's A sits on KeyQ).
        assert_eq!(key_code(&char("a"), KEY_Q), Some(0));
    }

    #[test]
    fn unmapped_keys_are_dropped() {
        assert_eq!(key_code(&named("F1"), KeyCode(59)), None);
        assert_eq!(key_code(&char(","), KeyCode(51)), None);
        assert_eq!(key_code(&named("Shift"), KeyCode(42)), None);
        assert_eq!(key_code(&Key::Unidentified, KeyCode(0)), None);
    }

    #[test]
    fn modifiers_map_to_the_guests_three() {
        let with = |f: fn(&mut Modifiers)| {
            let mut modifiers = Modifiers::default();
            f(&mut modifiers);
            modifiers
        };
        assert_eq!(
            guest_modifiers(with(|m| m.logo = true)),
            (false, true, false)
        );
        assert_eq!(
            guest_modifiers(with(|m| m.alt_graph = true)),
            (false, false, false)
        );
        assert!(plain(with(|m| m.caps_lock = true)));
        assert!(!plain(with(|m| m.alt_graph = true)));
    }
}
