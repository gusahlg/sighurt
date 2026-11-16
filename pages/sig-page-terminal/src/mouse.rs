//! Pointer events as the bytes a program that asked for the mouse reads (xterm's mouse
//! tracking).
//!
//! The program picks what is reported (`vt100` keeps its choice): presses only (X10, mode 9),
//! presses and releases and the wheel (1000), and motion while a button is held (1002) or always
//! (1003); and how: the original one-byte-per-value form (`ESC [ M`, which cannot say a column
//! or row past 223, so nothing is sent there), the UTF-8 form (1005) or SGR's (1006:
//! `ESC [ < b ; x ; y M`, `m` for a release). Shift, Alt and Control add 4, 8 and 16 to the
//! button (not in X10 mode), motion 32; the wheel is buttons 64 to 67, a release in the original
//! forms button 3. Shift with the pointer is the user's, never the program's: it selects text
//! ([`crate::app`]).
//!
//! Owns: the encoding. Must not own: deciding which events are the program's ([`crate::app`]).

use vt100::{MouseProtocolEncoding as Encoding, MouseProtocolMode as Mode};

/// A button, or the wheel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    /// Motion with no button held.
    None,
}

/// What happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Press,
    Release,
    Motion,
}

/// Modifiers held, as the report carries them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// The report of `event` of `button` at cell (`col`, `row`), counted from 0, under the
/// program's `mode` and `encoding`; `None` when the mode does not report it or the encoding
/// cannot say where.
pub fn encode(
    mode: Mode,
    encoding: Encoding,
    event: Event,
    button: Button,
    col: u16,
    row: u16,
    held: Held,
) -> Option<Vec<u8>> {
    let wheel = matches!(
        button,
        Button::WheelUp | Button::WheelDown | Button::WheelLeft | Button::WheelRight
    );
    let reported = match mode {
        Mode::None => false,
        Mode::Press => event == Event::Press && !wheel,
        Mode::PressRelease => event != Event::Motion,
        Mode::ButtonMotion => event != Event::Motion || button != Button::None,
        Mode::AnyMotion => true,
    };
    // A wheel has no release.
    if !reported || (wheel && event == Event::Release) {
        return None;
    }
    let mut code: u32 = match button {
        Button::Left => 0,
        Button::Middle => 1,
        Button::Right => 2,
        Button::None => 3,
        Button::WheelUp => 64,
        Button::WheelDown => 65,
        Button::WheelLeft => 66,
        Button::WheelRight => 67,
    };
    if event == Event::Release && encoding != Encoding::Sgr {
        code = 3;
    }
    if event == Event::Motion {
        code += 32;
    }
    if mode != Mode::Press {
        code += 4 * u32::from(held.shift) + 8 * u32::from(held.alt) + 16 * u32::from(held.control);
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    match encoding {
        Encoding::Sgr => {
            let end = if event == Event::Release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes())
        }
        Encoding::Default => {
            let byte = |v: u32| u8::try_from(v + 32).ok();
            Some(vec![0x1b, b'[', b'M', byte(code)?, byte(x)?, byte(y)?])
        }
        Encoding::Utf8 => {
            let mut out = b"\x1b[M".to_vec();
            for v in [code, x, y] {
                // Values past 2015 cannot be written in two bytes, as xterm limits them.
                let c = char::from_u32(v + 32).filter(|_| v + 32 < 2048)?;
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sgr(event: Event, button: Button, held: Held) -> Option<String> {
        encode(Mode::AnyMotion, Encoding::Sgr, event, button, 4, 9, held)
            .map(|b| String::from_utf8(b).unwrap())
    }

    #[test]
    fn sgr_reports() {
        let none = Held::default();
        assert_eq!(
            sgr(Event::Press, Button::Left, none).unwrap(),
            "\x1b[<0;5;10M"
        );
        assert_eq!(
            sgr(Event::Release, Button::Left, none).unwrap(),
            "\x1b[<0;5;10m"
        );
        assert_eq!(
            sgr(Event::Press, Button::Right, none).unwrap(),
            "\x1b[<2;5;10M"
        );
        assert_eq!(
            sgr(Event::Motion, Button::Left, none).unwrap(),
            "\x1b[<32;5;10M"
        );
        assert_eq!(
            sgr(Event::Motion, Button::None, none).unwrap(),
            "\x1b[<35;5;10M"
        );
        assert_eq!(
            sgr(Event::Press, Button::WheelUp, none).unwrap(),
            "\x1b[<64;5;10M"
        );
        assert_eq!(
            sgr(Event::Press, Button::WheelDown, none).unwrap(),
            "\x1b[<65;5;10M"
        );
        assert_eq!(sgr(Event::Release, Button::WheelDown, none), None);
        let held = Held {
            shift: true,
            alt: true,
            control: true,
        };
        assert_eq!(
            sgr(Event::Press, Button::Middle, held).unwrap(),
            "\x1b[<29;5;10M"
        );
    }

    #[test]
    fn the_modes_choose_what_is_reported() {
        let none = Held::default();
        let at = |mode, event, button| encode(mode, Encoding::Sgr, event, button, 0, 0, none);
        assert_eq!(at(Mode::None, Event::Press, Button::Left), None);
        assert!(at(Mode::Press, Event::Press, Button::Left).is_some());
        assert_eq!(at(Mode::Press, Event::Release, Button::Left), None);
        assert_eq!(at(Mode::Press, Event::Press, Button::WheelUp), None);
        assert!(at(Mode::PressRelease, Event::Release, Button::Left).is_some());
        assert!(at(Mode::PressRelease, Event::Press, Button::WheelUp).is_some());
        assert_eq!(at(Mode::PressRelease, Event::Motion, Button::Left), None);
        assert!(at(Mode::ButtonMotion, Event::Motion, Button::Left).is_some());
        assert_eq!(at(Mode::ButtonMotion, Event::Motion, Button::None), None);
        assert!(at(Mode::AnyMotion, Event::Motion, Button::None).is_some());
        // X10 carries no modifiers.
        let shift = Held {
            shift: true,
            ..none
        };
        assert_eq!(
            encode(
                Mode::Press,
                Encoding::Sgr,
                Event::Press,
                Button::Left,
                0,
                0,
                shift
            ),
            Some(b"\x1b[<0;1;1M".to_vec())
        );
    }

    #[test]
    fn the_original_and_utf8_forms() {
        let none = Held::default();
        let default = |event, col| {
            encode(
                Mode::PressRelease,
                Encoding::Default,
                event,
                Button::Left,
                col,
                2,
                none,
            )
        };
        assert_eq!(default(Event::Press, 0), Some(b"\x1b[M !#".to_vec()));
        assert_eq!(default(Event::Release, 0), Some(b"\x1b[M#!#".to_vec()));
        assert_eq!(
            default(Event::Press, 222),
            Some(vec![0x1b, b'[', b'M', 32, 255, 35])
        );
        assert_eq!(default(Event::Press, 223), None, "past what a byte says");
        let utf8 = encode(
            Mode::PressRelease,
            Encoding::Utf8,
            Event::Press,
            Button::Left,
            300,
            2,
            none,
        );
        assert_eq!(utf8, Some("\x1b[M \u{14d}#".as_bytes().to_vec()));
        let far = encode(
            Mode::PressRelease,
            Encoding::Utf8,
            Event::Press,
            Button::Left,
            2100,
            2,
            none,
        );
        assert_eq!(far, None);
    }
}
