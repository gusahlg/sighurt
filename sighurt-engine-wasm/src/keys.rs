//! Keyboard input for guests, from the W3C `key` / `code` values the browser sends: the key
//! codes of the guest input ABI and typed text.

use sighurt_ipc::{MOD_CTRL, MOD_META};

/// Guest key code for a key, as polled through `api_key_down` / `api_key_pressed` (the SDK's
/// `KEY_*` constants). Letters and digits follow the layout (`key`), falling back to the physical
/// key (`code`) when the character isn't one (Shift+1 types "!"). Keys without a code are not
/// forwarded to the guest.
pub fn key_code(key: &str, code: &str) -> Option<u32> {
    Some(match key {
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
        _ => {
            let physical = code
                .strip_prefix("Key")
                .or_else(|| code.strip_prefix("Digit"));
            return char_code(key).or_else(|| physical.and_then(char_code));
        }
    })
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

/// The text a key press types: its character, unless it is a named key ("Enter") or Ctrl or
/// Meta make it a shortcut. Alt is allowed, since AltGr and macOS' Option type characters.
pub fn typed_text(key: &str, modifiers: u8) -> Option<&str> {
    let mut chars = key.chars();
    let printable = matches!((chars.next(), chars.next()), (Some(c), None) if !c.is_control());
    (printable && modifiers & (MOD_CTRL | MOD_META) == 0).then_some(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_ipc::{MOD_ALT, MOD_SHIFT};

    #[test]
    fn key_codes_match_the_sdk_constants() {
        assert_eq!(key_code("a", "KeyA"), Some(0));
        assert_eq!(key_code("z", "KeyZ"), Some(25));
        assert_eq!(key_code("0", "Digit0"), Some(26));
        assert_eq!(key_code("9", "Digit9"), Some(35));
        assert_eq!(key_code("Enter", "NumpadEnter"), Some(36));
        assert_eq!(key_code(" ", "Space"), Some(41));
        assert_eq!(key_code("ArrowLeft", "ArrowLeft"), Some(44));
        assert_eq!(key_code("PageDown", "PageDown"), Some(49));
    }

    #[test]
    fn shifted_and_foreign_characters_fall_back_to_the_physical_key() {
        assert_eq!(key_code("A", "KeyA"), Some(0));
        assert_eq!(key_code("!", "Digit1"), Some(27));
        assert_eq!(key_code("ф", "KeyA"), Some(0));
        // The layout wins where it types a letter (AZERTY's A sits on KeyQ).
        assert_eq!(key_code("a", "KeyQ"), Some(0));
    }

    #[test]
    fn unmapped_keys_are_dropped() {
        assert_eq!(key_code("F1", "F1"), None);
        assert_eq!(key_code(",", "Comma"), None);
        assert_eq!(key_code("Shift", "ShiftLeft"), None);
    }

    #[test]
    fn typed_text_is_printable_characters_only() {
        assert_eq!(typed_text("a", 0), Some("a"));
        assert_eq!(typed_text("A", MOD_SHIFT), Some("A"));
        assert_eq!(typed_text(" ", 0), Some(" "));
        assert_eq!(typed_text("é", 0), Some("é"));
        assert_eq!(typed_text("@", MOD_ALT), Some("@"));
        assert_eq!(typed_text("Enter", 0), None);
        assert_eq!(typed_text("Backspace", 0), None);
        assert_eq!(typed_text("a", MOD_CTRL), None);
        assert_eq!(typed_text("a", MOD_META), None);
    }
}
