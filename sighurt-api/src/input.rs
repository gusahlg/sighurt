//! Seat-aware input, as the host normalizes it.
//!
//! The host turns Wayland input into [`InputEvent`]s addressed to one target, with positions
//! already surface-local to that target. The browser considers its shortcuts first; the chrome
//! or the page with logical focus gets what is left.
//!
//! A key event keeps apart what the plan requires to stay apart, so international input
//! survives: the physical key ([`KeyCode`]), the layout's key value ([`Key`]), the modifier
//! state, auto-repeat, and the committed text. On a Swedish layout:
//!
//! | Keys | `code` | `key` | `text` |
//! |---|---|---|---|
//! | å | `BracketLeft` | `Character("å")` | `"å"` |
//! | AltGr + 2 | `Digit2` | `Character("@")`, with AltGr in `modifiers` and `consumed` | `"@"` |
//! | ¨ (dead) | `BracketRight` | `Dead(Some('¨'))` | none |
//! | then a | `KeyA` | `Character("ä")` (composed) | `"ä"` |
//! | Ctrl + a | `KeyA` | `Character("a")`, with Control in `modifiers` | none |
//! | Enter | `Enter` | `Named("Enter")` | none |
//!
//! `key` follows W3C `KeyboardEvent.key`: the press that completes a dead-key or compose
//! sequence carries the composed character, and a press that only advances a composition is a
//! [`Key::Dead`]. `text` is what the press types. On a press that types, `key` is the same
//! text as a `Character`, so an engine that inserts from its key events (Servo does) types the
//! same thing; editors Sighurt controls insert `text`. A press without `text` is a key or a
//! shortcut, never typing.
//!
//! Text from an input method arrives as [`TextInputEvent`]s instead, never also as key text.

use std::ops::Range;

use crate::geometry::{SurfacePoint, SurfaceRect};
use crate::ids::SeatId;

/// One input event from one seat.
#[derive(Clone, Debug, PartialEq)]
pub struct InputEvent {
    /// The seat the event came from.
    pub seat: SeatId,
    /// What happened.
    pub kind: InputKind,
}

/// The kinds of input.
#[derive(Clone, Debug, PartialEq)]
pub enum InputKind {
    /// Pointer motion, buttons and scrolling.
    Pointer(PointerEvent),
    /// A key press or release.
    Key(KeyEvent),
    /// An update from an input method.
    TextInput(TextInputEvent),
    /// Keyboard focus of the window changed.
    Focus(FocusEvent),
}

/// Whether a key or button went down or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ButtonState {
    /// Went down.
    Pressed,
    /// Went up.
    Released,
}

/// Pointer input on one target, in that target's surface-local coordinates.
///
/// While a button is held, the target that received the press keeps receiving motion and the
/// release, even when the pointer leaves it. Motion, buttons and scrolling carry the seat's
/// current modifiers, because the pointer's target need not be the one that saw the modifier
/// keys go down (or modifiers were already held when the window gained focus).
#[derive(Clone, Debug, PartialEq)]
pub enum PointerEvent {
    /// The pointer entered the target at `position`.
    Enter {
        /// Where it entered.
        position: SurfacePoint,
    },
    /// The pointer left the target.
    Leave,
    /// The pointer moved.
    Motion {
        /// The new position.
        position: SurfacePoint,
        /// The seat's modifiers, for drags that depend on them.
        modifiers: Modifiers,
    },
    /// A button went down or up.
    Button {
        /// Which button.
        button: PointerButton,
        /// Down or up.
        state: ButtonState,
        /// Where the pointer was.
        position: SurfacePoint,
        /// The seat's modifiers (Shift+click, Ctrl+click), whoever has keyboard focus.
        modifiers: Modifiers,
    },
    /// Scrolling.
    Scroll {
        /// How far.
        delta: ScrollDelta,
        /// Where the pointer was.
        position: SurfacePoint,
        /// The seat's modifiers (Ctrl+wheel).
        modifiers: Modifiers,
    },
}

/// A pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    /// The primary button.
    Left,
    /// The wheel button.
    Middle,
    /// The secondary button.
    Right,
    /// The "back" side button.
    Back,
    /// The "forward" side button.
    Forward,
    /// Any other button, by its Linux evdev code (`BTN_*`).
    Other(u32),
}

/// How far to scroll. Positive `y` scrolls down and positive `x` right, as with DOM wheel
/// events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollDelta {
    /// Smooth scrolling (touchpads, continuous axes), in surface-local logical pixels.
    Pixels {
        /// Horizontal distance.
        x: f64,
        /// Vertical distance.
        y: f64,
    },
    /// Wheel detents, in 120ths of a detent as `wl_pointer.axis_value120` gives them: 120 is
    /// one notch, and high-resolution wheels send fractions.
    Wheel {
        /// Horizontal 120ths.
        x: i32,
        /// Vertical 120ths.
        y: i32,
    },
}

/// A key press or release.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct KeyEvent {
    /// The physical key, whatever the layout.
    pub code: KeyCode,
    /// What the key means in the current layout, with the current modifiers.
    pub key: Key,
    /// Down or up.
    pub state: ButtonState,
    /// Whether this press was generated by auto-repeat. Releases are never repeats.
    pub repeat: bool,
    /// Modifiers held when the key changed.
    pub modifiers: Modifiers,
    /// The subset of `modifiers` the layout used to produce `key`, such as Shift for "A" or
    /// AltGr for "@". Shortcut matching by key value ignores these, so AltGr + 2 is "@" rather
    /// than a Ctrl+Alt shortcut.
    pub consumed: Modifiers,
    /// Text this press types, after dead-key and compose processing. `None` for releases,
    /// modifier keys, dead keys and other presses that only advance a composition, keys that
    /// produce no text, and presses while Control or Logo is held (unless the layout consumed
    /// it): those are keys or shortcuts, not typing. Never holds control characters, so Enter,
    /// Tab, Backspace and Escape have no text either.
    pub text: Option<String>,
}

/// A physical key: its Linux evdev code (`KEY_*`), as `wl_keyboard.key` reports it. It names a
/// position on the keyboard, not a character.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyCode(pub u32);

impl KeyCode {
    /// The key's W3C `KeyboardEvent.code` name, such as "KeyA", "Digit1", "ShiftLeft" or
    /// "Numpad0": the name engines report to content and key bindings use for physical keys.
    /// `None` for keys that aren't in this table; engines report those as "Unidentified".
    ///
    /// Codes follow `linux/input-event-codes.h`; names follow the W3C UI Events code values.
    pub fn w3c_code(self) -> Option<&'static str> {
        Some(match self.0 {
            1 => "Escape",
            2 => "Digit1",
            3 => "Digit2",
            4 => "Digit3",
            5 => "Digit4",
            6 => "Digit5",
            7 => "Digit6",
            8 => "Digit7",
            9 => "Digit8",
            10 => "Digit9",
            11 => "Digit0",
            12 => "Minus",
            13 => "Equal",
            14 => "Backspace",
            15 => "Tab",
            16 => "KeyQ",
            17 => "KeyW",
            18 => "KeyE",
            19 => "KeyR",
            20 => "KeyT",
            21 => "KeyY",
            22 => "KeyU",
            23 => "KeyI",
            24 => "KeyO",
            25 => "KeyP",
            26 => "BracketLeft",
            27 => "BracketRight",
            28 => "Enter",
            29 => "ControlLeft",
            30 => "KeyA",
            31 => "KeyS",
            32 => "KeyD",
            33 => "KeyF",
            34 => "KeyG",
            35 => "KeyH",
            36 => "KeyJ",
            37 => "KeyK",
            38 => "KeyL",
            39 => "Semicolon",
            40 => "Quote",
            41 => "Backquote",
            42 => "ShiftLeft",
            43 => "Backslash",
            44 => "KeyZ",
            45 => "KeyX",
            46 => "KeyC",
            47 => "KeyV",
            48 => "KeyB",
            49 => "KeyN",
            50 => "KeyM",
            51 => "Comma",
            52 => "Period",
            53 => "Slash",
            54 => "ShiftRight",
            55 => "NumpadMultiply",
            56 => "AltLeft",
            57 => "Space",
            58 => "CapsLock",
            59 => "F1",
            60 => "F2",
            61 => "F3",
            62 => "F4",
            63 => "F5",
            64 => "F6",
            65 => "F7",
            66 => "F8",
            67 => "F9",
            68 => "F10",
            69 => "NumLock",
            70 => "ScrollLock",
            71 => "Numpad7",
            72 => "Numpad8",
            73 => "Numpad9",
            74 => "NumpadSubtract",
            75 => "Numpad4",
            76 => "Numpad5",
            77 => "Numpad6",
            78 => "NumpadAdd",
            79 => "Numpad1",
            80 => "Numpad2",
            81 => "Numpad3",
            82 => "Numpad0",
            83 => "NumpadDecimal",
            85 => "Lang5",
            86 => "IntlBackslash",
            87 => "F11",
            88 => "F12",
            89 => "IntlRo",
            90 => "Lang3",
            91 => "Lang4",
            92 => "Convert",
            93 => "KanaMode",
            94 => "NonConvert",
            96 => "NumpadEnter",
            97 => "ControlRight",
            98 => "NumpadDivide",
            99 => "PrintScreen",
            100 => "AltRight",
            102 => "Home",
            103 => "ArrowUp",
            104 => "PageUp",
            105 => "ArrowLeft",
            106 => "ArrowRight",
            107 => "End",
            108 => "ArrowDown",
            109 => "PageDown",
            110 => "Insert",
            111 => "Delete",
            113 => "AudioVolumeMute",
            114 => "AudioVolumeDown",
            115 => "AudioVolumeUp",
            117 => "NumpadEqual",
            119 => "Pause",
            121 => "NumpadComma",
            122 => "Lang1",
            123 => "Lang2",
            124 => "IntlYen",
            125 => "MetaLeft",
            126 => "MetaRight",
            127 => "ContextMenu",
            128 => "BrowserStop",
            156 => "BrowserFavorites",
            158 => "BrowserBack",
            159 => "BrowserForward",
            163 => "MediaTrackNext",
            164 => "MediaPlayPause",
            165 => "MediaTrackPrevious",
            166 => "MediaStop",
            172 => "BrowserHome",
            173 => "BrowserRefresh",
            183 => "F13",
            184 => "F14",
            185 => "F15",
            186 => "F16",
            187 => "F17",
            188 => "F18",
            189 => "F19",
            190 => "F20",
            191 => "F21",
            192 => "F22",
            193 => "F23",
            194 => "F24",
            217 => "BrowserSearch",
            _ => return None,
        })
    }

    /// Where the key sits, for keys the keyboard has more than once (Shift, Enter, digits).
    pub fn location(self) -> KeyLocation {
        match self.0 {
            29 | 42 | 56 | 125 => KeyLocation::Left,
            54 | 97 | 100 | 126 => KeyLocation::Right,
            55 | 71..=83 | 96 | 98 | 117 | 121 => KeyLocation::Numpad,
            _ => KeyLocation::Standard,
        }
    }
}

/// Where a key sits on the keyboard, as W3C `KeyboardEvent.location` has it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum KeyLocation {
    /// The only key of its kind, or the main block's.
    Standard,
    /// The left one of a pair, such as ShiftLeft.
    Left,
    /// The right one of a pair, such as AltRight (AltGr on many layouts).
    Right,
    /// On the numeric keypad.
    Numpad,
}

/// The layout-dependent meaning of a key, with W3C `KeyboardEvent.key` semantics: the press
/// that completes a dead-key or compose sequence is the composed character, and a press that
/// only advances a composition is [`Key::Dead`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    /// A key that produces a character in the current layout: "a", "A", "å", "@".
    Character(String),
    /// A named key that produces no character, by its W3C key name: "Enter", "ArrowLeft",
    /// "Backspace", "Shift", "AltGraph", "F5".
    Named(String),
    /// A dead key, which composes with the next key, or another press that only advances a
    /// composition (the keys of a Compose sequence before the last). Holds the character it
    /// adds (such as '¨') when known.
    Dead(Option<char>),
    /// A key the layout gives no meaning.
    Unidentified,
}

/// Which modifiers are active. AltGr is its own modifier, never reported as Ctrl + Alt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    /// Shift.
    pub shift: bool,
    /// Control.
    pub control: bool,
    /// Alt (the left Alt key on most layouts).
    pub alt: bool,
    /// AltGr (ISO level 3 shift), which selects characters like "@" on many layouts.
    pub alt_graph: bool,
    /// The Super/Windows/Command key (xkb's "Logo", W3C `metaKey`).
    pub logo: bool,
    /// Caps Lock is on.
    pub caps_lock: bool,
    /// Num Lock is on.
    pub num_lock: bool,
}

/// One atomic update from an input method, as `zwp_text_input_v3.done` groups it. Apply in
/// this order: delete around the cursor, insert `commit`, then show `preedit`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct TextInputEvent {
    /// Text to delete around the cursor (or around the selection), before the commit.
    pub delete_surrounding: Option<DeleteSurrounding>,
    /// Text to insert at the cursor, replacing the previous preedit.
    pub commit: Option<String>,
    /// The composition to show at the cursor until the next update. `None` clears it.
    pub preedit: Option<Preedit>,
}

/// How much text to delete around the cursor, in UTF-8 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeleteSurrounding {
    /// Bytes before the cursor.
    pub before: u32,
    /// Bytes after the cursor.
    pub after: u32,
}

/// Text being composed, shown but not yet committed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Preedit {
    /// The composition.
    pub text: String,
    /// The cursor or highlighted range within `text`, in UTF-8 byte offsets. `None` hides the
    /// cursor.
    pub cursor: Option<Range<usize>>,
}

/// What an editable element tells the input method, from the engine or the chrome.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TextInputState {
    /// The caret, in the reporting target's surface-local coordinates. The host translates it
    /// into the surface the text-input protocol uses.
    pub caret: SurfaceRect,
    /// The text around the cursor, when the editor can provide it. Never for a password field.
    pub surrounding: Option<SurroundingText>,
    /// What the field is for, so the input method can adapt to it.
    pub purpose: TextPurpose,
    /// Whether the field takes more than one line.
    pub multiline: bool,
}

/// What an editable field is for, like the purposes of `zwp_text_input_v3.set_content_type`.
/// The host always sends [`TextPurpose::Password`] with the `sensitive_data` and `hidden_text`
/// hints, so an input method neither shows nor learns what is typed there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TextPurpose {
    /// Ordinary text, including search fields.
    Normal,
    /// A password.
    Password,
    /// An email address.
    Email,
    /// A URL, such as the address bar.
    Url,
    /// A number.
    Number,
    /// A telephone number.
    Phone,
    /// A date.
    Date,
    /// A time of day.
    Time,
    /// A date and time.
    DateTime,
}

/// Text around the cursor, for input methods that look at context.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SurroundingText {
    /// The text, typically the current paragraph.
    pub text: String,
    /// The cursor, as a UTF-8 byte offset into `text`.
    pub cursor: usize,
    /// The other end of the selection, equal to `cursor` when nothing is selected.
    pub anchor: usize,
}

/// A change of the window's keyboard focus for one seat, as the compositor reports it. This is
/// separate from which part of the window (page, address bar, ...) has logical focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FocusEvent {
    /// The window gained keyboard focus.
    Activated,
    /// The window lost keyboard focus. Held keys, auto-repeat and pointer captures end:
    /// receivers must not keep treating keys as pressed.
    Deactivated,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_keys_of_a_swedish_layout() {
        // å, ö, ä and the <> key of ISO keyboards, which US layouts lack.
        let codes = [26, 39, 40, 86].map(|code| KeyCode(code).w3c_code());
        assert_eq!(
            codes,
            [
                Some("BracketLeft"),
                Some("Semicolon"),
                Some("Quote"),
                Some("IntlBackslash")
            ]
        );
        assert_eq!(KeyCode(0).w3c_code(), None);
    }

    #[test]
    fn locations_tell_paired_keys_apart() {
        assert_eq!(KeyCode(100).w3c_code(), Some("AltRight"));
        assert_eq!(KeyCode(100).location(), KeyLocation::Right);
        assert_eq!(KeyCode(42).location(), KeyLocation::Left);
        assert_eq!(KeyCode(96).location(), KeyLocation::Numpad);
        assert_eq!(KeyCode(28).location(), KeyLocation::Standard);
        // ArrowLeft is not a left-hand key.
        assert_eq!(KeyCode(105).location(), KeyLocation::Standard);
    }
}
