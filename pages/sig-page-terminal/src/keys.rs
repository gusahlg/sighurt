//! Key presses as the bytes a program in an xterm-compatible terminal reads.
//!
//! A key arrives as the `sig` input queue gives it: its W3C `KeyboardEvent.key`, the text it
//! typed and the modifiers held. Text is sent as UTF-8; Control with a letter or one of
//! `@[\]^_?` and their digit aliases is the C0 control byte; Alt puts ESC first; named keys
//! (arrows, Home, End, Page Up and Down, Insert, Delete, F1 to F12) are xterm's sequences, with
//! the modifier parameter (`ESC [ 1 ; m A`) when Shift, Alt or Control is held, and in the
//! application form (`ESC O A`) when the program asked for application cursor keys. Enter is CR,
//! Backspace DEL, Shift+Tab `ESC [ Z`. AltGr types text; Super is the browser's, never the
//! program's.
//!
//! Owns: the encoding. Must not own: which keys reach the page (the keymap's) or pastes
//! ([`crate::paste`]).

use sig_page_kit::sig::modifiers::{ALT, ALT_GRAPH, CONTROL, SHIFT, SUPER};

/// What the program asked of the keyboard.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modes {
    /// DECCKM: cursor keys (and Home, End) in the application form.
    pub application_cursor: bool,
}

/// The bytes for a press of `key` that typed `text` with `modifiers` held, or `None` for a key
/// that sends nothing (a modifier alone, Super chords, keys a terminal has no code for).
pub fn encode(key: &str, text: &str, modifiers: u8, modes: Modes) -> Option<Vec<u8>> {
    if modifiers & SUPER != 0 {
        return None;
    }
    // AltGr composes text; some layouts report it as Control and Alt as well.
    let (control, alt, shift) = if modifiers & ALT_GRAPH != 0 {
        (false, false, false)
    } else {
        (
            modifiers & CONTROL != 0,
            modifiers & ALT != 0,
            modifiers & SHIFT != 0,
        )
    };
    // xterm's modifier parameter: 1 plus Shift 1, Alt 2, Control 4.
    let parameter = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(control);
    let esc = |bytes: &[u8]| {
        let mut out = Vec::with_capacity(bytes.len() + 1);
        if alt {
            out.push(0x1b);
        }
        out.extend_from_slice(bytes);
        out
    };
    let cursor = |end: u8| -> Vec<u8> {
        if parameter > 1 {
            format!("\x1b[1;{parameter}{}", end as char).into_bytes()
        } else if modes.application_cursor {
            vec![0x1b, b'O', end]
        } else {
            vec![0x1b, b'[', end]
        }
    };
    let tilde = |number: u8| -> Vec<u8> {
        if parameter > 1 {
            format!("\x1b[{number};{parameter}~").into_bytes()
        } else {
            format!("\x1b[{number}~").into_bytes()
        }
    };
    let function = |end: u8| -> Vec<u8> {
        if parameter > 1 {
            format!("\x1b[1;{parameter}{}", end as char).into_bytes()
        } else {
            vec![0x1b, b'O', end]
        }
    };
    let bytes = match key {
        "Enter" => esc(b"\r"),
        "Tab" if shift => b"\x1b[Z".to_vec(),
        "Tab" => esc(b"\t"),
        "Backspace" if control => esc(&[0x08]),
        "Backspace" => esc(&[0x7f]),
        "Escape" => esc(&[0x1b]),
        "ArrowUp" => cursor(b'A'),
        "ArrowDown" => cursor(b'B'),
        "ArrowRight" => cursor(b'C'),
        "ArrowLeft" => cursor(b'D'),
        "Home" => cursor(b'H'),
        "End" => cursor(b'F'),
        "Insert" => tilde(2),
        "Delete" => tilde(3),
        "PageUp" => tilde(5),
        "PageDown" => tilde(6),
        "F1" => function(b'P'),
        "F2" => function(b'Q'),
        "F3" => function(b'R'),
        "F4" => function(b'S'),
        "F5" => tilde(15),
        "F6" => tilde(17),
        "F7" => tilde(18),
        "F8" => tilde(19),
        "F9" => tilde(20),
        "F10" => tilde(21),
        "F11" => tilde(23),
        "F12" => tilde(24),
        _ => {
            let mut chars = key.chars();
            let single = chars.next().filter(|_| chars.next().is_none());
            if control {
                let byte = single.and_then(control_byte)?;
                return Some(esc(&[byte]));
            }
            // A named key a terminal has no code for ("Shift", "CapsLock", "AudioVolumeUp").
            let typed = if text.is_empty() {
                single?.to_string()
            } else {
                text.to_string()
            };
            if typed.chars().any(char::is_control) {
                return None;
            }
            esc(typed.as_bytes())
        }
    };
    Some(bytes)
}

/// The control byte Control and `key` make, as xterm makes it.
fn control_byte(key: char) -> Option<u8> {
    Some(match key.to_ascii_lowercase() {
        c @ 'a'..='z' => c as u8 - b'a' + 1,
        '@' | ' ' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '?' | '8' => 0x7f,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: &str, text: &str, modifiers: u8) -> Option<Vec<u8>> {
        encode(key, text, modifiers, Modes::default())
    }

    fn app(key: &str, modifiers: u8) -> Option<Vec<u8>> {
        let modes = Modes {
            application_cursor: true,
        };
        encode(key, "", modifiers, modes)
    }

    fn bytes(s: &str) -> Option<Vec<u8>> {
        Some(s.as_bytes().to_vec())
    }

    #[test]
    fn text_is_utf8() {
        assert_eq!(key("a", "a", 0), bytes("a"));
        assert_eq!(key("A", "A", SHIFT), bytes("A"));
        assert_eq!(key("ä", "ä", 0), bytes("ä"));
        assert_eq!(key("Dead", "", 0), None, "a dead key types nothing yet");
        // A composed character arrives on the press that completes it.
        assert_eq!(key("é", "é", 0), bytes("é"));
        assert_eq!(key(" ", " ", 0), bytes(" "));
        // AltGr types what the layout puts there, even if Control and Alt are reported too.
        assert_eq!(key("@", "@", ALT_GRAPH | CONTROL | ALT), bytes("@"));
    }

    #[test]
    fn control_chords_are_c0_bytes() {
        assert_eq!(key("c", "", CONTROL), Some(vec![3]));
        assert_eq!(key("C", "", CONTROL | SHIFT), Some(vec![3]));
        assert_eq!(key("d", "\u{4}", CONTROL), Some(vec![4]));
        assert_eq!(key("[", "", CONTROL), Some(vec![0x1b]));
        assert_eq!(
            key("\\", "", CONTROL),
            Some(vec![0x1c]),
            "ctrl-\\, nvim's exit"
        );
        assert_eq!(key("n", "", CONTROL), Some(vec![0x0e]));
        assert_eq!(key("@", "", CONTROL), Some(vec![0]));
        assert_eq!(key(" ", " ", CONTROL), Some(vec![0]));
        assert_eq!(key("?", "", CONTROL), Some(vec![0x7f]));
        assert_eq!(key("1", "", CONTROL), None, "no code for it");
    }

    #[test]
    fn alt_puts_escape_first() {
        assert_eq!(key("x", "x", ALT), bytes("\x1bx"));
        assert_eq!(key("x", "", CONTROL | ALT), Some(vec![0x1b, 0x18]));
        assert_eq!(key("Enter", "", ALT), bytes("\x1b\r"));
        assert_eq!(key("Backspace", "", ALT), Some(vec![0x1b, 0x7f]));
    }

    #[test]
    fn editing_keys() {
        assert_eq!(key("Enter", "\r", 0), bytes("\r"));
        assert_eq!(key("Tab", "\t", 0), bytes("\t"));
        assert_eq!(key("Tab", "", SHIFT), bytes("\x1b[Z"));
        assert_eq!(key("Backspace", "", 0), Some(vec![0x7f]));
        assert_eq!(key("Backspace", "", CONTROL), Some(vec![0x08]));
        assert_eq!(key("Escape", "", 0), Some(vec![0x1b]));
    }

    #[test]
    fn cursor_keys_follow_the_application_mode() {
        assert_eq!(key("ArrowUp", "", 0), bytes("\x1b[A"));
        assert_eq!(key("ArrowLeft", "", 0), bytes("\x1b[D"));
        assert_eq!(key("Home", "", 0), bytes("\x1b[H"));
        assert_eq!(key("End", "", 0), bytes("\x1b[F"));
        assert_eq!(app("ArrowUp", 0), bytes("\x1bOA"));
        assert_eq!(app("End", 0), bytes("\x1bOF"));
        // With a modifier the form is the same in both modes.
        assert_eq!(key("ArrowRight", "", CONTROL), bytes("\x1b[1;5C"));
        assert_eq!(app("ArrowRight", CONTROL), bytes("\x1b[1;5C"));
        assert_eq!(key("ArrowDown", "", SHIFT), bytes("\x1b[1;2B"));
        assert_eq!(key("Home", "", SHIFT | ALT | CONTROL), bytes("\x1b[1;8H"));
    }

    #[test]
    fn tilde_and_function_keys() {
        assert_eq!(key("Insert", "", 0), bytes("\x1b[2~"));
        assert_eq!(key("Delete", "", 0), bytes("\x1b[3~"));
        assert_eq!(key("PageUp", "", 0), bytes("\x1b[5~"));
        assert_eq!(key("PageDown", "", SHIFT), bytes("\x1b[6;2~"));
        assert_eq!(key("F1", "", 0), bytes("\x1bOP"));
        assert_eq!(key("F4", "", 0), bytes("\x1bOS"));
        assert_eq!(key("F1", "", SHIFT), bytes("\x1b[1;2P"));
        assert_eq!(key("F5", "", 0), bytes("\x1b[15~"));
        assert_eq!(key("F10", "", 0), bytes("\x1b[21~"));
        assert_eq!(key("F12", "", CONTROL), bytes("\x1b[24;5~"));
    }

    #[test]
    fn keys_a_terminal_has_no_code_for_send_nothing() {
        for name in [
            "Shift",
            "Control",
            "Alt",
            "CapsLock",
            "AudioVolumeUp",
            "F13",
        ] {
            assert_eq!(key(name, "", 0), None, "{name}");
        }
        assert_eq!(key("a", "a", SUPER), None);
        assert_eq!(key("x", "\u{1b}", 0), None, "control text from a frontend");
    }
}
