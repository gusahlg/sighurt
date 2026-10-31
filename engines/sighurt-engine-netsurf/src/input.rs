//! Browser input mapped onto NetSurf's: pointer events become `browser_window_mouse_*` states,
//! keys become `NS_KEY_*` values or Unicode scalars, the wheel becomes a scroll in pixels.
//!
//! Pure functions and one small state machine, with no call into the core: the runtime sends
//! what they return. Positions are converted to buffer pixels once, by the caller, with the
//! view's `Metrics`; nothing here applies a scale.
//!
//! Owns: the translation tables and click, double-click and drag recognition. Must not own: what
//! a key does (the core's keymap decides what reaches the engine) or any NetSurf call.

use std::time::{Duration, Instant};

use sighurt_api::events::Cursor;
use sighurt_api::input::{
    ButtonState, Key, KeyEvent, Modifiers, PointerButton, PointerEvent, ScrollDelta,
};

use crate::ffi;

/// Distance, in buffer pixels, a pressed button moves before it is a drag.
const DRAG_DISTANCE: i32 = 5;
/// The longest gap between presses that counts as a double (or triple) click.
const MULTI_CLICK: Duration = Duration::from_millis(500);
/// Pixels a wheel notch scrolls: three lines.
const WHEEL_NOTCH: i32 = 48;

/// What a key press asks of the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// Nothing: a modifier alone, a release, a combination the engine does not take.
    Ignore,
    /// Send these keys, in order.
    Keys(Vec<u32>),
    /// Paste: the engine asks for the clipboard, then sends [`ffi::KEY_PASTE`].
    Paste,
}

/// The `BROWSER_MOUSE_MOD_*` bits.
pub fn modifier_bits(modifiers: &Modifiers) -> u32 {
    let mut bits = 0;
    if modifiers.shift {
        bits |= ffi::MOUSE_MOD_1;
    }
    if modifiers.control {
        bits |= ffi::MOUSE_MOD_2;
    }
    if modifiers.alt {
        bits |= ffi::MOUSE_MOD_3;
    }
    bits
}

/// What a key event does. Releases do nothing; text a press types is sent a scalar at a time.
pub fn key_action(event: &KeyEvent) -> KeyAction {
    if event.state != ButtonState::Pressed {
        return KeyAction::Ignore;
    }
    let ctrl = event.modifiers.control && !event.modifiers.alt_graph;
    let shift = event.modifiers.shift;
    if let Key::Named(name) = &event.key {
        let key = match (name.as_str(), ctrl) {
            ("Enter", _) => ffi::KEY_NL,
            ("Backspace", false) => ffi::KEY_DELETE_LEFT,
            ("Backspace", true) => ffi::KEY_DELETE_WORD_LEFT,
            ("Delete", false) => ffi::KEY_DELETE_RIGHT,
            ("Delete", true) => ffi::KEY_DELETE_WORD_RIGHT,
            ("Tab", _) if shift => ffi::KEY_SHIFT_TAB,
            ("Tab", _) => ffi::KEY_TAB,
            ("Escape", _) => ffi::KEY_ESCAPE,
            ("ArrowLeft", false) => ffi::KEY_LEFT,
            ("ArrowLeft", true) => ffi::KEY_WORD_LEFT,
            ("ArrowRight", false) => ffi::KEY_RIGHT,
            ("ArrowRight", true) => ffi::KEY_WORD_RIGHT,
            ("ArrowUp", _) => ffi::KEY_UP,
            ("ArrowDown", _) => ffi::KEY_DOWN,
            ("Home", false) => ffi::KEY_LINE_START,
            ("Home", true) => ffi::KEY_TEXT_START,
            ("End", false) => ffi::KEY_LINE_END,
            ("End", true) => ffi::KEY_TEXT_END,
            ("PageUp", _) => ffi::KEY_PAGE_UP,
            ("PageDown", _) => ffi::KEY_PAGE_DOWN,
            _ => return KeyAction::Ignore,
        };
        return KeyAction::Keys(vec![key]);
    }
    if ctrl {
        // Control with a letter is an editing command, never typed text.
        let Key::Character(text) = &event.key else {
            return KeyAction::Ignore;
        };
        return match text.to_ascii_lowercase().as_str() {
            "a" => KeyAction::Keys(vec![ffi::KEY_SELECT_ALL]),
            "c" => KeyAction::Keys(vec![ffi::KEY_COPY_SELECTION]),
            "x" => KeyAction::Keys(vec![ffi::KEY_CUT_SELECTION]),
            "z" if shift => KeyAction::Keys(vec![ffi::KEY_REDO]),
            "z" => KeyAction::Keys(vec![ffi::KEY_UNDO]),
            "y" => KeyAction::Keys(vec![ffi::KEY_REDO]),
            "v" => KeyAction::Paste,
            _ => KeyAction::Ignore,
        };
    }
    if event.modifiers.alt || event.modifiers.logo {
        return KeyAction::Ignore;
    }
    match &event.text {
        Some(text) => typed(text),
        None => KeyAction::Ignore,
    }
}

/// The keys that type `text`: its scalars, without control characters.
pub fn typed(text: &str) -> KeyAction {
    let keys: Vec<u32> = text
        .chars()
        .filter(|c| !c.is_control())
        .map(u32::from)
        .collect();
    if keys.is_empty() {
        KeyAction::Ignore
    } else {
        KeyAction::Keys(keys)
    }
}

/// The browser cursor for NetSurf's `gui_pointer_shape`.
pub fn cursor(shape: i32) -> Cursor {
    match shape {
        1 => Cursor::Pointer,
        2 => Cursor::Text,
        3 => Cursor::ContextMenu,
        4 => Cursor::NResize,
        5 => Cursor::SResize,
        6 => Cursor::WResize,
        7 => Cursor::EResize,
        8 => Cursor::NeResize,
        9 => Cursor::SwResize,
        10 => Cursor::NwResize,
        11 => Cursor::SeResize,
        12 => Cursor::Crosshair,
        13 => Cursor::Move,
        14 => Cursor::Wait,
        15 => Cursor::Help,
        16 => Cursor::NoDrop,
        17 => Cursor::NotAllowed,
        18 => Cursor::Progress,
        _ => Cursor::Default,
    }
}

/// What a pointer event asks of the engine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PointerAction {
    /// `nsh_view_mouse` with these bits.
    Mouse {
        /// Buffer position.
        x: i32,
        /// Buffer position.
        y: i32,
        /// `MOUSE_*` state bits.
        state: u32,
        /// `MOUSE_MOD_*` bits.
        mods: u32,
    },
    /// A plain left click on release: the runtime may hold it if it is on a link
    /// (`NavigationRequested`), and otherwise sends it as [`PointerAction::Mouse`].
    LeftClick {
        /// Buffer position.
        x: i32,
        /// Buffer position.
        y: i32,
        /// `MOUSE_*` state bits of the click.
        state: u32,
        /// `MOUSE_MOD_*` bits.
        mods: u32,
    },
    /// `nsh_view_scroll`.
    Scroll {
        /// Buffer position.
        x: i32,
        /// Buffer position.
        y: i32,
        /// Pixels right.
        dx: i32,
        /// Pixels down.
        dy: i32,
    },
}

/// One view's pointer: held buttons, click counting and drag recognition.
#[derive(Debug, Default)]
pub struct Pointer {
    left: Option<Held>,
    last_press: Option<(Instant, i32, i32, u32)>,
}

#[derive(Debug)]
struct Held {
    x: i32,
    y: i32,
    /// 1, 2 or 3: the click count of this press.
    clicks: u32,
    dragging: bool,
}

fn multi(clicks: u32) -> u32 {
    match clicks {
        2 => ffi::MOUSE_DOUBLE_CLICK,
        3.. => ffi::MOUSE_TRIPLE_CLICK,
        _ => 0,
    }
}

impl Pointer {
    /// Translates `event`. `at` converts a surface position to buffer pixels.
    pub fn event(
        &mut self,
        event: &PointerEvent,
        at: impl Fn(sighurt_api::geometry::SurfacePoint) -> (i32, i32),
        now: Instant,
    ) -> Vec<PointerAction> {
        match event {
            PointerEvent::Enter { position } => {
                let (x, y) = at(*position);
                vec![PointerAction::Mouse {
                    x,
                    y,
                    state: ffi::MOUSE_HOVER,
                    mods: 0,
                }]
            }
            PointerEvent::Leave => {
                self.left = None;
                vec![PointerAction::Mouse {
                    x: -1,
                    y: -1,
                    state: ffi::MOUSE_LEAVE,
                    mods: 0,
                }]
            }
            PointerEvent::Motion {
                position,
                modifiers,
            } => {
                let (x, y) = at(*position);
                let mods = modifier_bits(modifiers);
                match &mut self.left {
                    Some(held) if !held.dragging => {
                        if (x - held.x).abs() >= DRAG_DISTANCE
                            || (y - held.y).abs() >= DRAG_DISTANCE
                        {
                            held.dragging = true;
                            vec![PointerAction::Mouse {
                                x: held.x,
                                y: held.y,
                                state: ffi::MOUSE_DRAG_1 | ffi::MOUSE_HOLDING_1,
                                mods,
                            }]
                        } else {
                            Vec::new()
                        }
                    }
                    Some(_) => vec![PointerAction::Mouse {
                        x,
                        y,
                        state: ffi::MOUSE_HOLDING_1 | ffi::MOUSE_DRAG_ON,
                        mods,
                    }],
                    None => vec![PointerAction::Mouse {
                        x,
                        y,
                        state: ffi::MOUSE_HOVER,
                        mods,
                    }],
                }
            }
            PointerEvent::Button {
                button,
                state,
                position,
                modifiers,
            } => {
                let (x, y) = at(*position);
                let mods = modifier_bits(modifiers);
                match (button, state) {
                    (PointerButton::Left, ButtonState::Pressed) => {
                        let clicks = match self.last_press {
                            Some((then, px, py, n))
                                if now.duration_since(then) <= MULTI_CLICK
                                    && (x - px).abs() < DRAG_DISTANCE
                                    && (y - py).abs() < DRAG_DISTANCE =>
                            {
                                (n % 3) + 1
                            }
                            _ => 1,
                        };
                        self.last_press = Some((now, x, y, clicks));
                        self.left = Some(Held {
                            x,
                            y,
                            clicks,
                            dragging: false,
                        });
                        vec![PointerAction::Mouse {
                            x,
                            y,
                            state: ffi::MOUSE_PRESS_1 | multi(clicks),
                            mods,
                        }]
                    }
                    (PointerButton::Left, ButtonState::Released) => match self.left.take() {
                        Some(held) if held.dragging => vec![PointerAction::Mouse {
                            x,
                            y,
                            state: ffi::MOUSE_HOVER,
                            mods,
                        }],
                        Some(held) => vec![PointerAction::LeftClick {
                            x: held.x,
                            y: held.y,
                            state: ffi::MOUSE_CLICK_1 | multi(held.clicks),
                            mods,
                        }],
                        None => Vec::new(),
                    },
                    (PointerButton::Right, ButtonState::Pressed) => vec![PointerAction::Mouse {
                        x,
                        y,
                        state: ffi::MOUSE_PRESS_2,
                        mods,
                    }],
                    (PointerButton::Right, ButtonState::Released) => vec![PointerAction::Mouse {
                        x,
                        y,
                        state: ffi::MOUSE_CLICK_2,
                        mods,
                    }],
                    _ => Vec::new(),
                }
            }
            PointerEvent::Scroll {
                delta, position, ..
            } => {
                let (x, y) = at(*position);
                let (dx, dy) = match *delta {
                    ScrollDelta::Wheel { x, y } => (x * WHEEL_NOTCH / 120, y * WHEEL_NOTCH / 120),
                    ScrollDelta::Pixels { x, y } => (x.round() as i32, y.round() as i32),
                };
                if dx == 0 && dy == 0 {
                    Vec::new()
                } else {
                    vec![PointerAction::Scroll { x, y, dx, dy }]
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::SurfacePoint;
    use sighurt_api::input::KeyCode;

    use super::*;

    fn at(p: SurfacePoint) -> (i32, i32) {
        (p.x as i32, p.y as i32)
    }

    fn key(key: Key, text: Option<&str>, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            code: KeyCode(0),
            key,
            state: ButtonState::Pressed,
            repeat: false,
            modifiers,
            consumed: Modifiers::default(),
            text: text.map(str::to_owned),
        }
    }

    fn button(state: ButtonState, x: f64, y: f64) -> PointerEvent {
        PointerEvent::Button {
            button: PointerButton::Left,
            state,
            position: SurfacePoint { x, y },
            modifiers: Modifiers::default(),
        }
    }

    #[test]
    fn text_is_typed_a_scalar_at_a_time_and_accents_survive() {
        let event = key(
            Key::Character("é".into()),
            Some("héllo"),
            Modifiers::default(),
        );
        assert_eq!(
            key_action(&event),
            KeyAction::Keys("héllo".chars().map(u32::from).collect())
        );
        let tab = key(Key::Named("Tab".into()), Some("\t"), Modifiers::default());
        assert_eq!(key_action(&tab), KeyAction::Keys(vec![ffi::KEY_TAB]));
    }

    #[test]
    fn control_letters_are_editing_commands_not_text() {
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        let a = key(Key::Character("a".into()), Some("\u{1}"), ctrl);
        assert_eq!(key_action(&a), KeyAction::Keys(vec![ffi::KEY_SELECT_ALL]));
        let v = key(Key::Character("v".into()), None, ctrl);
        assert_eq!(key_action(&v), KeyAction::Paste);
        let q = key(Key::Character("q".into()), None, ctrl);
        assert_eq!(key_action(&q), KeyAction::Ignore);
        let word = key(Key::Named("ArrowLeft".into()), None, ctrl);
        assert_eq!(key_action(&word), KeyAction::Keys(vec![ffi::KEY_WORD_LEFT]));
    }

    #[test]
    fn a_release_and_a_lone_modifier_do_nothing() {
        let mut release = key(Key::Character("a".into()), Some("a"), Modifiers::default());
        release.state = ButtonState::Released;
        assert_eq!(key_action(&release), KeyAction::Ignore);
        let shift = key(Key::Named("Shift".into()), None, Modifiers::default());
        assert_eq!(key_action(&shift), KeyAction::Ignore);
    }

    #[test]
    fn press_then_release_is_a_click_and_a_drag_is_not() {
        let mut p = Pointer::default();
        let t = Instant::now();
        let press = p.event(&button(ButtonState::Pressed, 10.0, 10.0), at, t);
        assert_eq!(
            press,
            vec![PointerAction::Mouse {
                x: 10,
                y: 10,
                state: ffi::MOUSE_PRESS_1,
                mods: 0
            }]
        );
        let release = p.event(&button(ButtonState::Released, 10.0, 10.0), at, t);
        assert_eq!(
            release,
            vec![PointerAction::LeftClick {
                x: 10,
                y: 10,
                state: ffi::MOUSE_CLICK_1,
                mods: 0
            }]
        );
        // A drag: press, move past the threshold, release.
        p.event(
            &button(ButtonState::Pressed, 100.0, 100.0),
            at,
            t + Duration::from_secs(2),
        );
        let motion = |x| PointerEvent::Motion {
            position: SurfacePoint { x, y: 100.0 },
            modifiers: Modifiers::default(),
        };
        assert!(
            p.event(&motion(102.0), at, t).is_empty(),
            "under the threshold"
        );
        let drag = p.event(&motion(120.0), at, t);
        assert!(matches!(
            drag[0],
            PointerAction::Mouse { state, .. } if state == ffi::MOUSE_DRAG_1 | ffi::MOUSE_HOLDING_1
        ));
        let release = p.event(&button(ButtonState::Released, 120.0, 100.0), at, t);
        assert!(matches!(release[0], PointerAction::Mouse { state: 0, .. }));
    }

    #[test]
    fn two_quick_presses_are_a_double_click() {
        let mut p = Pointer::default();
        let t = Instant::now();
        p.event(&button(ButtonState::Pressed, 5.0, 5.0), at, t);
        p.event(&button(ButtonState::Released, 5.0, 5.0), at, t);
        let second = p.event(
            &button(ButtonState::Pressed, 5.0, 5.0),
            at,
            t + Duration::from_millis(200),
        );
        assert!(matches!(
            second[0],
            PointerAction::Mouse { state, .. } if state == ffi::MOUSE_PRESS_1 | ffi::MOUSE_DOUBLE_CLICK
        ));
    }

    #[test]
    fn a_wheel_notch_scrolls_down_by_pixels() {
        let mut p = Pointer::default();
        let scroll = PointerEvent::Scroll {
            delta: ScrollDelta::Wheel { x: 0, y: 120 },
            position: SurfacePoint { x: 3.0, y: 4.0 },
            modifiers: Modifiers::default(),
        };
        assert_eq!(
            p.event(&scroll, at, Instant::now()),
            vec![PointerAction::Scroll {
                x: 3,
                y: 4,
                dx: 0,
                dy: WHEEL_NOTCH
            }]
        );
    }

    #[test]
    fn netsurf_pointer_shapes_map_to_cursors() {
        assert_eq!(cursor(0), Cursor::Default);
        assert_eq!(cursor(1), Cursor::Pointer);
        assert_eq!(cursor(2), Cursor::Text);
        assert_eq!(cursor(14), Cursor::Wait);
    }
}
