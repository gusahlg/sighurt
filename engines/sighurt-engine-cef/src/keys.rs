//! The host's key, modifier and cursor vocabulary in CEF's: Windows virtual-key codes,
//! `EVENTFLAG_*` masks and `cef_cursor_type_t`.
//!
//! Must not own: any CEF object or state. Pure tables, unit-tested.

use sighurt_api::events::Cursor;
use sighurt_api::input::{KeyCode, Modifiers};

/// `EVENTFLAG_*` of `cef_event_flags_t`.
pub mod flag {
    /// Caps lock is on.
    pub const CAPS_LOCK: u32 = 1 << 0;
    /// Shift is down.
    pub const SHIFT: u32 = 1 << 1;
    /// Control is down.
    pub const CONTROL: u32 = 1 << 2;
    /// Alt is down.
    pub const ALT: u32 = 1 << 3;
    /// The left button is down.
    pub const LEFT_BUTTON: u32 = 1 << 4;
    /// The middle button is down.
    pub const MIDDLE_BUTTON: u32 = 1 << 5;
    /// The right button is down.
    pub const RIGHT_BUTTON: u32 = 1 << 6;
    /// The logo key is down.
    pub const COMMAND: u32 = 1 << 7;
    /// Num lock is on.
    pub const NUM_LOCK: u32 = 1 << 8;
    /// The key is on the keypad.
    pub const IS_KEY_PAD: u32 = 1 << 9;
    /// The key is the left one of a pair.
    pub const IS_LEFT: u32 = 1 << 10;
    /// The key is the right one of a pair.
    pub const IS_RIGHT: u32 = 1 << 11;
    /// AltGr is down.
    pub const ALTGR: u32 = 1 << 12;
    /// The key press is a repeat.
    pub const IS_REPEAT: u32 = 1 << 13;
}

/// The `modifiers` field of a key or mouse event. AltGr is never Control+Alt.
pub fn modifiers(m: &Modifiers) -> u32 {
    let mut flags = 0;
    for (on, bit) in [
        (m.caps_lock, flag::CAPS_LOCK),
        (m.shift, flag::SHIFT),
        (m.control, flag::CONTROL),
        (m.alt, flag::ALT),
        (m.logo, flag::COMMAND),
        (m.num_lock, flag::NUM_LOCK),
        (m.alt_graph, flag::ALTGR),
    ] {
        if on {
            flags |= bit;
        }
    }
    flags
}

/// The Windows virtual-key code of the physical key `code` (what `windows_key_code` wants),
/// or 0 for one with no code.
pub fn virtual_key(code: KeyCode) -> i32 {
    let Some(name) = code.w3c_code() else {
        return 0;
    };
    if let Some(letter) = name.strip_prefix("Key") {
        return i32::from(letter.as_bytes()[0]); // 'A' = 0x41
    }
    if let Some(digit) = name.strip_prefix("Digit") {
        return i32::from(digit.as_bytes()[0]); // '0' = 0x30
    }
    if let Some(number) = name.strip_prefix("Numpad") {
        if let Ok(n) = number.parse::<i32>() {
            return 0x60 + n;
        }
    }
    if let Some(number) = name.strip_prefix('F') {
        if let Ok(n) = number.parse::<i32>() {
            return 0x70 + n - 1;
        }
    }
    match name {
        "Backspace" => 0x08,
        "Tab" => 0x09,
        "Enter" | "NumpadEnter" => 0x0D,
        "ShiftLeft" | "ShiftRight" => 0x10,
        "ControlLeft" | "ControlRight" => 0x11,
        "AltLeft" | "AltRight" => 0x12,
        "Pause" => 0x13,
        "CapsLock" => 0x14,
        "Escape" => 0x1B,
        "Space" => 0x20,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        "End" => 0x23,
        "Home" => 0x24,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "PrintScreen" => 0x2C,
        "Insert" => 0x2D,
        "Delete" => 0x2E,
        "MetaLeft" => 0x5B,
        "MetaRight" => 0x5C,
        "ContextMenu" => 0x5D,
        "NumpadMultiply" => 0x6A,
        "NumpadAdd" => 0x6B,
        "NumpadSubtract" => 0x6D,
        "NumpadDecimal" => 0x6E,
        "NumpadDivide" => 0x6F,
        "NumLock" => 0x90,
        "ScrollLock" => 0x91,
        "Semicolon" => 0xBA,
        "Equal" => 0xBB,
        "Comma" => 0xBC,
        "Minus" => 0xBD,
        "Period" => 0xBE,
        "Slash" => 0xBF,
        "Backquote" => 0xC0,
        "BracketLeft" => 0xDB,
        "Backslash" | "IntlBackslash" => 0xDC,
        "BracketRight" => 0xDD,
        "Quote" => 0xDE,
        _ => 0,
    }
}

/// The page's cursor from `cef_cursor_type_t` (the order of its declaration).
pub fn cursor(kind: i32) -> Cursor {
    match kind {
        0 => Cursor::Default,
        1 => Cursor::Crosshair,
        2 => Cursor::Pointer,
        3 => Cursor::Text,
        4 => Cursor::Wait,
        5 => Cursor::Help,
        6 => Cursor::EResize,
        7 => Cursor::NResize,
        8 => Cursor::NeResize,
        9 => Cursor::NwResize,
        10 => Cursor::SResize,
        11 => Cursor::SeResize,
        12 => Cursor::SwResize,
        13 => Cursor::WResize,
        14 => Cursor::NsResize,
        15 => Cursor::EwResize,
        16 => Cursor::NeswResize,
        17 => Cursor::NwseResize,
        18 => Cursor::ColResize,
        19 => Cursor::RowResize,
        20..=28 => Cursor::AllScroll,
        29 => Cursor::Move,
        30 => Cursor::VerticalText,
        31 => Cursor::Cell,
        32 => Cursor::ContextMenu,
        33 => Cursor::Alias,
        34 => Cursor::Progress,
        35 => Cursor::NoDrop,
        36 => Cursor::Copy,
        37 => Cursor::None,
        38 => Cursor::NotAllowed,
        39 => Cursor::ZoomIn,
        40 => Cursor::ZoomOut,
        41 => Cursor::Grab,
        42 => Cursor::Grabbing,
        _ => Cursor::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_digits_and_function_keys() {
        assert_eq!(virtual_key(KeyCode(30)), 0x41); // KeyA
        assert_eq!(virtual_key(KeyCode(2)), 0x31); // Digit1
        assert_eq!(virtual_key(KeyCode(59)), 0x70); // F1
        assert_eq!(virtual_key(KeyCode(88)), 0x7B); // F12
        assert_eq!(virtual_key(KeyCode(82)), 0x60); // Numpad0
        assert_eq!(virtual_key(KeyCode(28)), 0x0D); // Enter
        assert_eq!(virtual_key(KeyCode(51)), 0xBC); // Comma
        assert_eq!(virtual_key(KeyCode(9999)), 0);
    }

    #[test]
    fn altgr_is_its_own_flag() {
        let altgr = Modifiers {
            alt_graph: true,
            ..Modifiers::default()
        };
        assert_eq!(modifiers(&altgr), flag::ALTGR);
        let chord = Modifiers {
            control: true,
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(modifiers(&chord), flag::CONTROL | flag::SHIFT);
    }

    #[test]
    fn cursors() {
        assert_eq!(cursor(0), Cursor::Default);
        assert_eq!(cursor(2), Cursor::Pointer);
        assert_eq!(cursor(3), Cursor::Text);
        assert_eq!(cursor(999), Cursor::Default);
    }
}
