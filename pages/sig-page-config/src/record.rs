//! The `sig` input queue's records as the editor's inputs: a key press or repeat becomes a
//! key the editor knows (with Control, a shortcut) or the text it typed, and a paste stays a
//! paste. The record's layout is `sig-page-kit`'s; this takes its fields.
//!
//! Must not own: reading the queue (the wasm entry points), or what an input does (the
//! editor).

use crate::editor::{Input, Key, Mods};

/// Record kinds.
pub const PRESS: u8 = 1;
pub const REPEAT: u8 = 2;
pub const PASTE: u8 = 3;

/// Modifier bits.
const SHIFT: u8 = 1;
const CONTROL: u8 = 2;
const ALT: u8 = 4;
const ALT_GRAPH: u8 = 16;

/// The input a record stands for, if the editor has a use for it. `key` is the W3C
/// `KeyboardEvent.key` value, `text` what the press typed or the pasted text.
pub fn input(kind: u8, modifiers: u8, key: &str, text: &str) -> Option<Input> {
    match kind {
        PASTE => return Some(Input::Paste(text.to_owned())),
        PRESS | REPEAT => {}
        _ => return None,
    }
    // AltGr is never Control and Alt.
    let altgr = modifiers & ALT_GRAPH != 0;
    let mods = Mods {
        shift: modifiers & SHIFT != 0,
        ctrl: modifiers & CONTROL != 0 && !altgr,
        alt: modifiers & ALT != 0 && !altgr,
    };
    let named = match key {
        "Enter" => Some(Key::Enter),
        "Escape" => Some(Key::Escape),
        "Tab" => Some(Key::Tab),
        "Backspace" => Some(Key::Backspace),
        "Delete" => Some(Key::Delete),
        "ArrowLeft" => Some(Key::Left),
        "ArrowRight" => Some(Key::Right),
        "ArrowUp" => Some(Key::Up),
        "ArrowDown" => Some(Key::Down),
        "Home" => Some(Key::Home),
        "End" => Some(Key::End),
        "PageUp" => Some(Key::PageUp),
        "PageDown" => Some(Key::PageDown),
        _ => None,
    };
    if let Some(key) = named {
        return Some(Input::Key(key, mods));
    }
    let mut chars = key.chars();
    if let (true, Some(c), None) = (mods.ctrl, chars.next(), chars.next()) {
        return Some(Input::Key(Key::Char(c.to_ascii_lowercase()), mods));
    }
    (!text.is_empty() && !mods.ctrl && !mods.alt).then(|| Input::Text(text.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_become_keys_shortcuts_text_and_pastes() {
        assert_eq!(
            input(PRESS, 0, "Enter", "\r"),
            Some(Input::Key(Key::Enter, Mods::NONE))
        );
        assert_eq!(
            input(REPEAT, SHIFT, "ArrowLeft", ""),
            Some(Input::Key(Key::Left, Mods::SHIFT))
        );
        assert_eq!(input(PRESS, 0, "a", "a"), Some(Input::Text("a".into())));
        assert_eq!(input(PRESS, SHIFT, "Ä", "Ä"), Some(Input::Text("Ä".into())));
        assert_eq!(
            input(PRESS, CONTROL | SHIFT, "Z", ""),
            Some(Input::Key(Key::Char('z'), Mods::CTRL_SHIFT))
        );
        assert_eq!(
            input(PRESS, CONTROL, "s", "\u{13}"),
            Some(Input::Key(Key::Char('s'), Mods::CTRL))
        );
        // AltGr types; it is not Control and Alt.
        assert_eq!(
            input(PRESS, CONTROL | ALT | ALT_GRAPH, "@", "@"),
            Some(Input::Text("@".into()))
        );
        assert_eq!(input(PRESS, ALT, "x", "x"), None);
        assert_eq!(input(PRESS, 0, "Shift", ""), None);
        assert_eq!(
            input(PASTE, 0, "", "a\tb\n"),
            Some(Input::Paste("a\tb\n".into()))
        );
        assert_eq!(input(9, 0, "a", "a"), None);
    }
}
