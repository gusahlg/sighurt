//! Sighurt's normalized input as WPE events for one view.
//!
//! Positions arrive surface-local in logical pixels, which is what a `WPEView` takes (WebKit
//! applies the view's scale itself), so they pass through unchanged. Keys arrive translated by
//! the host's xkbcommon, with dead keys and compose already applied; WebKit wants an X keysym
//! and the hardware keycode (evdev + 8), and derives `key`, `code` and the typed text from them.
//! So the keysym is rebuilt from the host's `key`: a character's keysym (`å` is `aring`, and the
//! press completing `¨` + `a` carries `adiaeresis`), a dead key's `dead_*` keysym, and a table
//! for named keys. The view tracks pressed keys and buttons itself: WebKit needs the button
//! state on every pointer event, and losing focus releases every key it saw pressed.
//!
//! Must not own: focus policy or shortcuts (the browser consumed those already).

use std::time::Instant;

use sighurt_api::input::{
    ButtonState, Key, KeyEvent, Modifiers, PointerButton, PointerEvent, ScrollDelta,
};
use xkbcommon::xkb;

use crate::ffi::{self, WPEEvent};
use crate::platform::View;

/// One view's input state.
pub struct Input {
    start: Instant,
    /// Keycodes and keysyms of keys the view received as pressed.
    pressed_keys: Vec<(u32, u32)>,
    /// `WPE_MODIFIER_POINTER_BUTTON*` bits of held buttons.
    buttons: u32,
    position: (f64, f64),
}

impl Default for Input {
    fn default() -> Self {
        Self {
            start: Instant::now(),
            pressed_keys: Vec::new(),
            buttons: 0,
            position: (0.0, 0.0),
        }
    }
}

impl Input {
    fn time(&self) -> u32 {
        self.start.elapsed().as_millis() as u32
    }

    /// The WPE event for a pointer event, if WebKit has one.
    pub fn pointer(&mut self, view: &View, event: &PointerEvent) -> Option<*mut WPEEvent> {
        let view = view.as_ptr();
        let time = self.time();
        let mouse = ffi::WPE_INPUT_SOURCE_MOUSE;
        let moved = |input: &mut Self, kind, x: f64, y: f64, modifiers: u32| {
            let (dx, dy) = (x - input.position.0, y - input.position.1);
            input.position = (x, y);
            let mods = modifiers | input.buttons;
            unsafe { ffi::wpe_event_pointer_move_new(kind, view, mouse, time, mods, x, y, dx, dy) }
        };
        Some(match event {
            PointerEvent::Enter { position } => moved(
                self,
                ffi::WPE_EVENT_POINTER_ENTER,
                position.x,
                position.y,
                0,
            ),
            PointerEvent::Leave => {
                let (x, y) = self.position;
                moved(self, ffi::WPE_EVENT_POINTER_LEAVE, x, y, 0)
            }
            PointerEvent::Motion {
                position,
                modifiers,
            } => moved(
                self,
                ffi::WPE_EVENT_POINTER_MOVE,
                position.x,
                position.y,
                modifier_bits(modifiers),
            ),
            PointerEvent::Button {
                button,
                state,
                position,
                modifiers,
            } => {
                let button = match button {
                    PointerButton::Left => 1,
                    PointerButton::Middle => 2,
                    PointerButton::Right => 3,
                    PointerButton::Back => 8,
                    PointerButton::Forward => 9,
                    PointerButton::Other(_) => return None,
                };
                let bit = ffi::WPE_MODIFIER_POINTER_BUTTON1 << (button - 1).min(4);
                let (x, y) = (position.x, position.y);
                self.position = (x, y);
                let (kind, count) = match state {
                    ButtonState::Pressed => {
                        let count =
                            unsafe { ffi::wpe_view_compute_press_count(view, x, y, button, time) };
                        (ffi::WPE_EVENT_POINTER_DOWN, count)
                    }
                    ButtonState::Released => (ffi::WPE_EVENT_POINTER_UP, 0),
                };
                // WebKit reads the buttons held before this event changed them.
                let mods = modifier_bits(modifiers) | self.buttons;
                match state {
                    ButtonState::Pressed => self.buttons |= bit,
                    ButtonState::Released => self.buttons &= !bit,
                }
                unsafe {
                    ffi::wpe_event_pointer_button_new(
                        kind, view, mouse, time, mods, button, x, y, count,
                    )
                }
            }
            PointerEvent::Scroll {
                delta,
                position,
                modifiers,
            } => {
                // WebKit scrolls content down for negative deltas, as Wayland's own platform
                // passes `-value`; Sighurt's positive y is down.
                let (dx, dy, precise) = match *delta {
                    ScrollDelta::Wheel { x, y } => (-x as f64 / 120.0, -y as f64 / 120.0, false),
                    ScrollDelta::Pixels { x, y } => (-x, -y, true),
                };
                let mods = modifier_bits(modifiers) | self.buttons;
                unsafe {
                    ffi::wpe_event_scroll_new(
                        view,
                        mouse,
                        time,
                        mods,
                        dx,
                        dy,
                        precise.into(),
                        0,
                        position.x,
                        position.y,
                    )
                }
            }
        })
    }

    /// The WPE event for a key event, if its key has a keysym.
    pub fn key(&mut self, view: &View, event: &KeyEvent) -> Option<*mut WPEEvent> {
        let view = view.as_ptr();
        let keycode = event.code.0 + 8;
        let keysym = keysym(event)?;
        let mut mods = modifier_bits(&event.modifiers);
        // As WPE's own seat: a modifier key's event already includes (or no longer includes)
        // its own modifier.
        let own = match keysym {
            xkb::keysyms::KEY_Control_L | xkb::keysyms::KEY_Control_R => {
                ffi::WPE_MODIFIER_KEYBOARD_CONTROL
            }
            xkb::keysyms::KEY_Shift_L | xkb::keysyms::KEY_Shift_R => {
                ffi::WPE_MODIFIER_KEYBOARD_SHIFT
            }
            xkb::keysyms::KEY_Alt_L | xkb::keysyms::KEY_Alt_R => ffi::WPE_MODIFIER_KEYBOARD_ALT,
            xkb::keysyms::KEY_Super_L | xkb::keysyms::KEY_Super_R => {
                ffi::WPE_MODIFIER_KEYBOARD_META
            }
            _ => 0,
        };
        let kind = match event.state {
            ButtonState::Pressed => {
                mods |= own;
                if !self.pressed_keys.iter().any(|&(code, _)| code == keycode) {
                    self.pressed_keys.push((keycode, keysym));
                }
                ffi::WPE_EVENT_KEYBOARD_KEY_DOWN
            }
            ButtonState::Released => {
                mods &= !own;
                self.pressed_keys.retain(|&(code, _)| code != keycode);
                ffi::WPE_EVENT_KEYBOARD_KEY_UP
            }
        };
        let time = self.time();
        let source = ffi::WPE_INPUT_SOURCE_KEYBOARD;
        Some(unsafe {
            ffi::wpe_event_keyboard_new(kind, view, source, time, mods, keycode, keysym)
        })
    }

    /// Key-up events for every key the view still holds, which it must see before losing focus.
    pub fn release_keys(&mut self, view: &View) -> Vec<*mut WPEEvent> {
        let view = view.as_ptr();
        let time = self.time();
        let source = ffi::WPE_INPUT_SOURCE_KEYBOARD;
        self.pressed_keys
            .drain(..)
            .map(|(keycode, keysym)| unsafe {
                ffi::wpe_event_keyboard_new(
                    ffi::WPE_EVENT_KEYBOARD_KEY_UP,
                    view,
                    source,
                    time,
                    0,
                    keycode,
                    keysym,
                )
            })
            .collect()
    }
}

fn modifier_bits(modifiers: &Modifiers) -> u32 {
    let mut bits = 0;
    if modifiers.control {
        bits |= ffi::WPE_MODIFIER_KEYBOARD_CONTROL;
    }
    if modifiers.shift {
        bits |= ffi::WPE_MODIFIER_KEYBOARD_SHIFT;
    }
    if modifiers.alt {
        bits |= ffi::WPE_MODIFIER_KEYBOARD_ALT;
    }
    if modifiers.logo {
        bits |= ffi::WPE_MODIFIER_KEYBOARD_META;
    }
    if modifiers.caps_lock {
        bits |= ffi::WPE_MODIFIER_KEYBOARD_CAPS_LOCK;
    }
    bits
}

/// The X keysym of a key the host translated.
fn keysym(event: &KeyEvent) -> Option<u32> {
    use xkb::keysyms::*;
    let right = matches!(event.code.0, 54 | 97 | 100 | 126);
    let side = |left, right_side| if right { right_side } else { left };
    Some(match &event.key {
        Key::Character(text) => {
            let mut chars = text.chars();
            let (c, None) = (chars.next()?, chars.next()) else {
                return None; // Several characters can't be one keysym.
            };
            xkb::utf32_to_keysym(c as u32).raw()
        }
        Key::Dead(c) => match c {
            Some('`') => KEY_dead_grave,
            Some('´' | '\'') => KEY_dead_acute,
            Some('^') => KEY_dead_circumflex,
            Some('~') => KEY_dead_tilde,
            Some('¯') => KEY_dead_macron,
            Some('˘') => KEY_dead_breve,
            Some('˙') => KEY_dead_abovedot,
            Some('¨' | '"') => KEY_dead_diaeresis,
            Some('˚') => KEY_dead_abovering,
            Some('˝') => KEY_dead_doubleacute,
            Some('ˇ') => KEY_dead_caron,
            Some('¸') => KEY_dead_cedilla,
            _ => return None,
        },
        Key::Named(name) => match name.as_str() {
            "Enter" => KEY_Return,
            "Tab" => KEY_Tab,
            "Backspace" => KEY_BackSpace,
            "Escape" => KEY_Escape,
            "Delete" => KEY_Delete,
            "Insert" => KEY_Insert,
            "Home" => KEY_Home,
            "End" => KEY_End,
            "PageUp" => KEY_Page_Up,
            "PageDown" => KEY_Page_Down,
            "ArrowUp" => KEY_Up,
            "ArrowDown" => KEY_Down,
            "ArrowLeft" => KEY_Left,
            "ArrowRight" => KEY_Right,
            "Shift" => side(KEY_Shift_L, KEY_Shift_R),
            "Control" => side(KEY_Control_L, KEY_Control_R),
            "Alt" => side(KEY_Alt_L, KEY_Alt_R),
            "Meta" => side(KEY_Super_L, KEY_Super_R),
            "AltGraph" => KEY_ISO_Level3_Shift,
            "CapsLock" => KEY_Caps_Lock,
            "NumLock" => KEY_Num_Lock,
            "ScrollLock" => KEY_Scroll_Lock,
            "Pause" => KEY_Pause,
            "PrintScreen" => KEY_Print,
            "ContextMenu" => KEY_Menu,
            // F1..F24 and the rest share their names with keysyms.
            other => {
                let keysym = xkb::keysym_from_name(other, xkb::KEYSYM_NO_FLAGS).raw();
                if keysym == KEY_NoSymbol {
                    return None;
                }
                keysym
            }
        },
        Key::Unidentified => return None,
    })
}

#[cfg(test)]
mod tests {
    use sighurt_api::input::KeyCode;

    use super::*;

    fn key(code: u32, key: Key) -> KeyEvent {
        KeyEvent {
            code: KeyCode(code),
            key,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers: Modifiers::default(),
            consumed: Modifiers::default(),
            text: None,
        }
    }

    #[test]
    fn swedish_letters_and_a_dead_key_have_their_keysyms() {
        use xkb::keysyms::*;
        let c = |s: &str| Key::Character(s.into());
        assert_eq!(keysym(&key(26, c("å"))), Some(KEY_aring));
        assert_eq!(keysym(&key(40, c("ä"))), Some(KEY_adiaeresis));
        assert_eq!(keysym(&key(39, c("Ö"))), Some(KEY_Odiaeresis));
        assert_eq!(
            keysym(&key(27, Key::Dead(Some('¨')))),
            Some(KEY_dead_diaeresis)
        );
        assert_eq!(keysym(&key(3, c("@"))), Some(KEY_at));
        assert_eq!(
            keysym(&key(28, Key::Named("Enter".into()))),
            Some(KEY_Return)
        );
        assert_eq!(
            keysym(&key(54, Key::Named("Shift".into()))),
            Some(KEY_Shift_R)
        );
        assert_eq!(keysym(&key(59, Key::Named("F1".into()))), Some(KEY_F1));
        assert_eq!(keysym(&key(30, c("ab"))), None);
    }
}
