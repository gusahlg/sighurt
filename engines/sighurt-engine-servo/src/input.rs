//! `sighurt_api::input` events into Servo's input events, and Servo's editor and cursor state
//! back into the API's.
//!
//! Positions arrive surface-local to the view's target and are converted to Servo's device
//! pixels once, with the view's `Metrics` (`to_buffer`). Keys map the physical key
//! (`KeyCode::w3c_code` and `location`, parsed into Servo's `Code` and `Location`), the key value
//! (W3C `KeyboardEvent.key`, which already carries the composed character of a dead-key
//! sequence and types in Servo), modifiers (AltGr as `AltGraph`, never Ctrl+Alt) and
//! auto-repeat. [`Keys`] remembers what the page holds down, so losing focus releases it and a
//! release whose press went elsewhere never reaches the page. Input-method updates become
//! Servo's composition events ([`Composition`]), never also key text. Smooth scrolling and
//! wheel notches become wheel deltas in device pixels.
//!
//! Servo 0.7 takes a mouse event's modifiers only from the keyboard events it was sent (the
//! constellation's `active_keyboard_modifiers`), so a Ctrl+click whose Ctrl went to the chrome
//! would be a plain click to the page. Before a button press or release, [`Keys`] sends the
//! modifier keys (Shift, Control, Alt, Meta) whose state differs from what it last told Servo:
//! the page sees their `keydown`/`keyup`, as it would had they been pressed in it, and the
//! synthetic press is held like a real one (released on blur, or by the real release).
//!
//! Must not own: shortcut handling (the core consumes browser shortcuts before the page).

use std::collections::HashMap;

use servo::{
    Code, CompositionEvent, CompositionState, DeviceIntRect, DevicePoint, ImeEvent,
    InputEvent as ServoInput, InputMethodType, Key as ServoKey, KeyState, KeyboardEvent, Location,
    Modifiers as ServoModifiers, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, NamedKey, WebViewPoint, WheelDelta, WheelEvent,
    WheelMode,
};
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{Metrics, SurfacePoint, SurfaceRect};
use sighurt_api::input::{
    ButtonState, Key, KeyEvent, KeyLocation, Modifiers, PointerButton, PointerEvent, ScrollDelta,
    SurroundingText, TextInputEvent, TextInputState, TextPurpose,
};

/// Logical pixels one wheel notch (120 units) scrolls: three lines of 20 pixels, about what
/// other browsers scroll.
const NOTCH_PIXELS: f64 = 60.0;

/// The Servo event for pointer input at surface-local positions on a target with `metrics`.
pub(crate) fn pointer(event: &PointerEvent, metrics: &Metrics) -> ServoInput {
    let point = |position: &SurfacePoint| {
        let device = metrics.to_buffer(*position);
        WebViewPoint::Device(DevicePoint::new(device.x as f32, device.y as f32))
    };
    match event {
        PointerEvent::Enter { position } | PointerEvent::Motion { position, .. } => {
            ServoInput::MouseMove(MouseMoveEvent::new(point(position)))
        }
        PointerEvent::Leave => ServoInput::MouseLeftViewport(MouseLeftViewportEvent::default()),
        PointerEvent::Button {
            button,
            state,
            position,
            ..
        } => {
            let action = match state {
                ButtonState::Pressed => MouseButtonAction::Down,
                ButtonState::Released => MouseButtonAction::Up,
            };
            let button = match *button {
                PointerButton::Left => MouseButton::Primary,
                PointerButton::Middle => MouseButton::Auxiliary,
                PointerButton::Right => MouseButton::Secondary,
                PointerButton::Back => MouseButton::Back,
                PointerButton::Forward => MouseButton::Forward,
                PointerButton::Other(code) => MouseButton::Other(code as u16),
            };
            ServoInput::MouseButton(MouseButtonEvent::new(action, button, point(position)))
        }
        PointerEvent::Scroll {
            delta, position, ..
        } => {
            let (x, y) = match *delta {
                ScrollDelta::Pixels { x, y } => (x, y),
                ScrollDelta::Wheel { x, y } => (
                    f64::from(x) * NOTCH_PIXELS / 120.0,
                    f64::from(y) * NOTCH_PIXELS / 120.0,
                ),
            };
            // Logical to device pixels with the same ratio as positions; Servo's wheel delta
            // is the opposite of the DOM's (positive reveals content above).
            let device = metrics.to_buffer(SurfacePoint { x, y });
            let delta = WheelDelta {
                x: -device.x,
                y: -device.y,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            };
            ServoInput::Wheel(WheelEvent::new(delta, point(position)))
        }
    }
}

/// What the page holds down, so that losing focus releases exactly that.
#[derive(Debug, Default)]
pub(crate) struct Keys {
    /// Evdev code to the key value it went down with.
    pressed: HashMap<u32, ServoKey>,
    /// The modifiers Servo's constellation holds after the last keyboard event this view sent
    /// (it keeps one set for every WebView; another view's keys change it too).
    told: ServoModifiers,
}

/// The seat's state of a modifier.
type Held = fn(&Modifiers) -> bool;

/// The modifiers [`Keys::sync_modifiers`] keeps in step: Servo's flag, its key, the evdev code
/// of the left one, and the seat's state of it.
const SYNCED: [(ServoModifiers, NamedKey, u32, Held); 4] = [
    (ServoModifiers::SHIFT, NamedKey::Shift, 42, |m| m.shift),
    (ServoModifiers::CONTROL, NamedKey::Control, 29, |m| {
        m.control
    }),
    (ServoModifiers::ALT, NamedKey::Alt, 56, |m| m.alt),
    (ServoModifiers::META, NamedKey::Meta, 125, |m| m.logo),
];

impl Keys {
    /// The Servo event for `event`, or `None` for a release of a key the page doesn't hold (its
    /// press was a browser shortcut, or went to the chrome).
    pub fn key(&mut self, event: &KeyEvent) -> Option<ServoInput> {
        let key = servo_key(&event.key);
        let state = match event.state {
            ButtonState::Pressed => {
                self.pressed.insert(event.code.0, key.clone());
                KeyState::Down
            }
            ButtonState::Released => {
                self.pressed.remove(&event.code.0)?;
                KeyState::Up
            }
        };
        let mods = modifiers(event.modifiers);
        self.track(state, &key, mods);
        Some(ServoInput::Keyboard(KeyboardEvent::new_without_event(
            state,
            key,
            code(event.code.0),
            location(event.code.0),
            mods,
            event.repeat,
            false,
        )))
    }

    /// Key events for the modifier keys whose state differs between `wanted` (the seat's, as a
    /// pointer button event carries them) and what Servo was last told, so that the button
    /// event that follows has the seat's modifiers in the page.
    pub fn sync_modifiers(&mut self, wanted: Modifiers) -> Vec<ServoInput> {
        let mut out = Vec::new();
        for (flag, named, left, held) in SYNCED {
            let want = held(&wanted);
            if want == self.told.contains(flag) {
                continue;
            }
            let key = ServoKey::Named(named);
            let (state, raw) = if want {
                self.pressed.insert(left, key.clone());
                (KeyState::Down, left)
            } else {
                // The key the page holds for it, if any: its release is this one.
                let holding = self
                    .pressed
                    .iter()
                    .find(|(_, k)| modifier_flag(k) == Some(flag))
                    .map(|(&raw, _)| raw);
                if let Some(raw) = holding {
                    self.pressed.remove(&raw);
                }
                (KeyState::Up, holding.unwrap_or(left))
            };
            let before = self.told;
            self.track(state, &key, before);
            out.push(ServoInput::Keyboard(KeyboardEvent::new_without_event(
                state,
                key,
                code(raw),
                location(raw),
                before,
                false,
                false,
            )));
        }
        out
    }

    /// Mirrors the constellation: a keyboard event's modifiers become the active ones, changed
    /// by the event's own key when that is a modifier.
    fn track(&mut self, state: KeyState, key: &ServoKey, modifiers: ServoModifiers) {
        self.told = modifiers;
        if let Some(flag) = modifier_flag(key) {
            match state {
                KeyState::Down => self.told.insert(flag),
                KeyState::Up => self.told.remove(flag),
            }
        }
    }

    /// Key-ups for every key the page holds, when it loses focus: the real releases go
    /// elsewhere.
    pub fn release_all(&mut self) -> Vec<ServoInput> {
        let mut pressed: Vec<_> = self.pressed.drain().collect();
        pressed.sort_by_key(|(code, _)| *code);
        if !pressed.is_empty() {
            self.told = ServoModifiers::empty();
        }
        pressed
            .into_iter()
            .map(|(raw, key)| {
                ServoInput::Keyboard(KeyboardEvent::new_without_event(
                    KeyState::Up,
                    key,
                    code(raw),
                    location(raw),
                    ServoModifiers::empty(),
                    false,
                    false,
                ))
            })
            .collect()
    }
}

/// Servo's modifier flag for a modifier key, as the constellation reads it (legacy keys too).
#[allow(deprecated)]
fn modifier_flag(key: &ServoKey) -> Option<ServoModifiers> {
    let ServoKey::Named(named) = key else {
        return None;
    };
    Some(match named {
        NamedKey::Alt => ServoModifiers::ALT,
        NamedKey::AltGraph => ServoModifiers::ALT_GRAPH,
        NamedKey::CapsLock => ServoModifiers::CAPS_LOCK,
        NamedKey::Control => ServoModifiers::CONTROL,
        NamedKey::Fn => ServoModifiers::FN,
        NamedKey::FnLock => ServoModifiers::FN_LOCK,
        NamedKey::Meta | NamedKey::Super => ServoModifiers::META,
        NamedKey::NumLock => ServoModifiers::NUM_LOCK,
        NamedKey::ScrollLock => ServoModifiers::SCROLL_LOCK,
        NamedKey::Shift => ServoModifiers::SHIFT,
        NamedKey::Symbol => ServoModifiers::SYMBOL,
        NamedKey::SymbolLock => ServoModifiers::SYMBOL_LOCK,
        NamedKey::Hyper => ServoModifiers::HYPER,
        _ => return None,
    })
}

fn servo_key(key: &Key) -> ServoKey {
    match key {
        Key::Character(text) => ServoKey::Character(text.clone()),
        Key::Named(name) => name
            .parse()
            .unwrap_or(ServoKey::Named(NamedKey::Unidentified)),
        Key::Dead(_) => ServoKey::Named(NamedKey::Dead),
        Key::Unidentified => ServoKey::Named(NamedKey::Unidentified),
    }
}

fn code(raw: u32) -> Code {
    sighurt_api::input::KeyCode(raw)
        .w3c_code()
        .and_then(|name| name.parse().ok())
        .unwrap_or(Code::Unidentified)
}

fn location(raw: u32) -> Location {
    match sighurt_api::input::KeyCode(raw).location() {
        KeyLocation::Standard => Location::Standard,
        KeyLocation::Left => Location::Left,
        KeyLocation::Right => Location::Right,
        KeyLocation::Numpad => Location::Numpad,
    }
}

fn modifiers(m: Modifiers) -> ServoModifiers {
    [
        (m.shift, ServoModifiers::SHIFT),
        (m.control, ServoModifiers::CONTROL),
        (m.alt, ServoModifiers::ALT),
        (m.alt_graph, ServoModifiers::ALT_GRAPH),
        (m.logo, ServoModifiers::META),
        (m.caps_lock, ServoModifiers::CAPS_LOCK),
        (m.num_lock, ServoModifiers::NUM_LOCK),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .fold(ServoModifiers::empty(), |all, (_, flag)| all | flag)
}

/// An input method's composition in the focused editor, as Servo's composition events.
///
/// Servo inserts a composition update and selects it, and replaces that selection with the
/// text a composition ends with. It can't delete around the cursor, so
/// `delete_surrounding` is ignored, and ending a composition without text leaves the last
/// preedit in the field (Servo only drops the selection).
#[derive(Debug, Default)]
pub(crate) struct Composition {
    active: bool,
}

impl Composition {
    /// The Servo events for one input-method update: the commit, then the new preedit.
    pub fn update(&mut self, event: &TextInputEvent) -> Vec<ServoInput> {
        let mut out = Vec::new();
        let mut send = |state, data: &str| {
            out.push(ServoInput::Ime(ImeEvent::Composition(CompositionEvent {
                state,
                data: data.to_owned(),
            })));
        };
        if let Some(text) = &event.commit {
            if !self.active {
                send(CompositionState::Start, "");
            }
            send(CompositionState::End, text);
            self.active = false;
        }
        match &event.preedit {
            Some(preedit) => {
                if !self.active {
                    send(CompositionState::Start, "");
                    self.active = true;
                }
                send(CompositionState::Update, &preedit.text);
            }
            None if self.active => {
                send(CompositionState::End, "");
                self.active = false;
            }
            None => {}
        }
        out
    }

    /// The editor went away: whatever was being composed is abandoned, and Servo, whose editor
    /// is gone with it, is not told.
    pub fn reset(&mut self) {
        self.active = false;
    }

    /// The view lost focus while its editor still has it: the composition ends there, without
    /// text (Servo drops the preedit's selection and keeps its text, as a commit would).
    pub fn abandon(&mut self) -> Option<ServoInput> {
        std::mem::take(&mut self.active).then(|| {
            ServoInput::Ime(ImeEvent::Composition(CompositionEvent {
                state: CompositionState::End,
                data: String::new(),
            }))
        })
    }
}

/// The API's editor state for Servo's input-method request: the field's area in the page (device
/// pixels) as the caret rectangle, surface-local; its text and cursor as surrounding text
/// (Servo counts in UTF-16), never for a password field.
pub(crate) fn text_input_state(
    kind: InputMethodType,
    text: &str,
    insertion_point: Option<u32>,
    area: DeviceIntRect,
    multiline: bool,
    metrics: &Metrics,
) -> TextInputState {
    let purpose = match kind {
        InputMethodType::Password => TextPurpose::Password,
        InputMethodType::Email => TextPurpose::Email,
        InputMethodType::Url => TextPurpose::Url,
        InputMethodType::Number => TextPurpose::Number,
        InputMethodType::Tel => TextPurpose::Phone,
        InputMethodType::Date | InputMethodType::Month | InputMethodType::Week => TextPurpose::Date,
        InputMethodType::Time => TextPurpose::Time,
        InputMethodType::DatetimeLocal => TextPurpose::DateTime,
        InputMethodType::Color | InputMethodType::Search | InputMethodType::Text => {
            TextPurpose::Normal
        }
    };
    let surrounding = (purpose != TextPurpose::Password).then(|| {
        let cursor = utf8_offset(text, insertion_point.unwrap_or(0) as usize);
        SurroundingText {
            text: text.to_owned(),
            cursor,
            anchor: cursor,
        }
    });
    TextInputState {
        caret: to_surface(area, metrics),
        surrounding,
        purpose,
        multiline,
    }
}

/// The UTF-8 byte offset of `utf16` UTF-16 code units into `text`, clamped to its end.
fn utf8_offset(text: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, c) in text.char_indices() {
        if units >= utf16 {
            return index;
        }
        units += c.len_utf16();
    }
    text.len()
}

/// A rectangle in the view's device pixels, in surface-local logical pixels: the inverse of
/// `Metrics::to_buffer`, from the actual rounded buffer size, widened to whole pixels.
fn to_surface(area: DeviceIntRect, metrics: &Metrics) -> SurfaceRect {
    let (buffer, rect) = (metrics.buffer(), metrics.rect());
    // `device * logical / buffer`, rounded down (start) or up (end), in integers.
    let scale = |device: i32, logical: u32, buffer: u32, up: bool| -> i64 {
        let (n, d) = (
            i64::from(device) * i64::from(logical),
            i64::from(buffer.max(1)),
        );
        if up {
            n.div_euclid(d) + i64::from(n.rem_euclid(d) != 0)
        } else {
            n.div_euclid(d)
        }
    };
    let x0 = scale(area.min.x, rect.width, buffer.width, false);
    let y0 = scale(area.min.y, rect.height, buffer.height, false);
    let x1 = scale(area.max.x, rect.width, buffer.width, true);
    let y1 = scale(area.max.y, rect.height, buffer.height, true);
    SurfaceRect {
        x: x0 as i32,
        y: y0 as i32,
        width: (x1 - x0).max(0) as u32,
        height: (y1 - y0).max(0) as u32,
    }
}

/// Servo's cursor as the API's: the same CSS keywords.
pub(crate) fn cursor(cursor: servo::Cursor) -> Cursor {
    use servo::Cursor as S;
    match cursor {
        S::None => Cursor::None,
        S::Default => Cursor::Default,
        S::Pointer => Cursor::Pointer,
        S::ContextMenu => Cursor::ContextMenu,
        S::Help => Cursor::Help,
        S::Progress => Cursor::Progress,
        S::Wait => Cursor::Wait,
        S::Cell => Cursor::Cell,
        S::Crosshair => Cursor::Crosshair,
        S::Text => Cursor::Text,
        S::VerticalText => Cursor::VerticalText,
        S::Alias => Cursor::Alias,
        S::Copy => Cursor::Copy,
        S::Move => Cursor::Move,
        S::NoDrop => Cursor::NoDrop,
        S::NotAllowed => Cursor::NotAllowed,
        S::Grab => Cursor::Grab,
        S::Grabbing => Cursor::Grabbing,
        S::EResize => Cursor::EResize,
        S::NResize => Cursor::NResize,
        S::NeResize => Cursor::NeResize,
        S::NwResize => Cursor::NwResize,
        S::SResize => Cursor::SResize,
        S::SeResize => Cursor::SeResize,
        S::SwResize => Cursor::SwResize,
        S::WResize => Cursor::WResize,
        S::EwResize => Cursor::EwResize,
        S::NsResize => Cursor::NsResize,
        S::NeswResize => Cursor::NeswResize,
        S::NwseResize => Cursor::NwseResize,
        S::ColResize => Cursor::ColResize,
        S::RowResize => Cursor::RowResize,
        S::AllScroll => Cursor::AllScroll,
        S::ZoomIn => Cursor::ZoomIn,
        S::ZoomOut => Cursor::ZoomOut,
    }
}

#[cfg(test)]
mod tests {
    use servo::DeviceIntPoint;
    use sighurt_api::geometry::{Scale, WindowRect};
    use sighurt_api::ids::LayoutEpoch;
    use sighurt_api::input::{KeyCode, Preedit};

    use super::*;

    fn metrics(numerator: u32) -> Metrics {
        let rect = WindowRect {
            x: 0,
            y: 40,
            width: 853,
            height: 560,
        };
        Metrics::new(
            LayoutEpoch::next(),
            rect,
            Scale::from_fractional(numerator).unwrap(),
        )
    }

    fn device(event: ServoInput) -> (f32, f32) {
        match event.point() {
            Some(WebViewPoint::Device(p)) => (p.x, p.y),
            other => panic!("no device point: {other:?}"),
        }
    }

    #[test]
    fn positions_are_converted_once_from_the_rounded_buffer() {
        let at = |x, y| PointerEvent::Motion {
            position: SurfacePoint { x, y },
            modifiers: Modifiers::default(),
        };
        // 100 %: surface-local is device.
        assert_eq!(
            device(pointer(&at(100.0, 100.0), &metrics(120))),
            (100.0, 100.0)
        );
        // 150 %: 853 logical pixels are 1280 buffer pixels, so the right edge lands on 1280.
        let m = metrics(180);
        assert_eq!(m.buffer().width, 1280);
        assert_eq!(device(pointer(&at(853.0, 0.0), &m)).0, 1280.0);
        let (x, y) = device(pointer(&at(100.0, 100.0), &m));
        assert!(
            (x - 150.06).abs() < 0.01 && (y - 150.0).abs() < 0.01,
            "{x} {y}"
        );
    }

    #[test]
    fn scrolling_down_is_a_negative_servo_delta_in_device_pixels() {
        let scroll = |delta| PointerEvent::Scroll {
            delta,
            position: SurfacePoint { x: 1.0, y: 1.0 },
            modifiers: Modifiers::default(),
        };
        let wheel = |event| match event {
            ServoInput::Wheel(w) => (w.delta.x, w.delta.y),
            other => panic!("{other:?}"),
        };
        let one_notch_down = scroll(ScrollDelta::Wheel { x: 0, y: 120 });
        assert_eq!(
            wheel(pointer(&one_notch_down, &metrics(120))),
            (-0.0, -60.0)
        );
        let pixels = scroll(ScrollDelta::Pixels { x: 0.0, y: 10.0 });
        assert_eq!(wheel(pointer(&pixels, &metrics(240))), (-0.0, -20.0));
    }

    fn key_event(code: u32, key: Key, state: ButtonState, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            code: KeyCode(code),
            key,
            state,
            repeat: false,
            modifiers,
            consumed: Modifiers::default(),
            text: None,
        }
    }

    /// The key, code, location, modifiers and state of a keyboard event.
    fn keyboard(event: ServoInput) -> (ServoKey, Code, Location, ServoModifiers, KeyState) {
        match event {
            ServoInput::Keyboard(k) => {
                let e = k.event;
                (e.key, e.code, e.location, e.modifiers, e.state)
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn swedish_keys_reach_the_page_with_their_physical_code_and_value() {
        let mut keys = Keys::default();
        let none = Modifiers::default();
        let mut press = |code, key, modifiers| {
            keyboard(
                keys.key(&key_event(code, key, ButtonState::Pressed, modifiers))
                    .unwrap(),
            )
        };
        // å on a Swedish layout is the key US layouts call BracketLeft.
        let (key, code, ..) = press(26, Key::Character("å".into()), none);
        assert_eq!(
            (key, code),
            (ServoKey::Character("å".into()), Code::BracketLeft)
        );
        // ¨ is dead; the a after it types the composed ä.
        let (key, ..) = press(27, Key::Dead(Some('¨')), none);
        assert_eq!(key, ServoKey::Named(NamedKey::Dead));
        let (key, code, ..) = press(30, Key::Character("ä".into()), none);
        assert_eq!((key, code), (ServoKey::Character("ä".into()), Code::KeyA));
        // AltGr + 2 is "@" with AltGraph, not Control and Alt.
        let altgr = Modifiers {
            alt_graph: true,
            ..none
        };
        let (.., modifiers, _) = press(3, Key::Character("@".into()), altgr);
        assert_eq!(modifiers, ServoModifiers::ALT_GRAPH);
        // Named keys by their W3C name; the right Alt key is on the right.
        let (key, _, location, ..) = press(100, Key::Named("AltGraph".into()), none);
        assert_eq!(
            (key, location),
            (ServoKey::Named(NamedKey::AltGraph), Location::Right)
        );
    }

    #[test]
    fn a_click_tells_servo_the_modifiers_its_keys_went_elsewhere_for() {
        let mut keys = Keys::default();
        let none = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..none
        };
        // Ctrl went down while the address bar had focus: the page never saw it.
        let synced: Vec<_> = keys
            .sync_modifiers(ctrl)
            .into_iter()
            .map(keyboard)
            .collect();
        assert_eq!(synced.len(), 1);
        let (key, code, location, before, state) = synced[0].clone();
        assert_eq!(
            (key, code, location, before, state),
            (
                ServoKey::Named(NamedKey::Control),
                Code::ControlLeft,
                Location::Left,
                ServoModifiers::empty(),
                KeyState::Down
            )
        );
        // Told once: the next click with Ctrl sends nothing.
        assert!(keys.sync_modifiers(ctrl).is_empty());
        // The real release of that key reaches the page.
        let up = keys.key(&key_event(
            29,
            Key::Named("Control".into()),
            ButtonState::Released,
            ctrl,
        ));
        assert!(up.is_some());
        assert!(keys.sync_modifiers(none).is_empty());
        // Released elsewhere instead: the next plain click releases it first.
        keys.sync_modifiers(ctrl);
        let synced: Vec<_> = keys
            .sync_modifiers(none)
            .into_iter()
            .map(keyboard)
            .collect();
        assert_eq!(synced.len(), 1);
        assert_eq!(synced[0].4, KeyState::Up);
        assert!(keys.release_all().is_empty(), "nothing is left held");
        // A synthetic press is held like a real one: blur releases it.
        keys.sync_modifiers(Modifiers {
            shift: true,
            ..none
        });
        let released: Vec<_> = keys.release_all().into_iter().map(keyboard).collect();
        assert_eq!(released[0].0, ServoKey::Named(NamedKey::Shift));
        assert!(keys.sync_modifiers(none).is_empty());
    }

    #[test]
    fn an_abandoned_composition_ends_in_servo() {
        let mut composition = Composition::default();
        assert!(composition.abandon().is_none());
        composition.update(&TextInputEvent {
            preedit: Some(Preedit {
                text: "a".into(),
                cursor: None,
            }),
            ..TextInputEvent::default()
        });
        match composition.abandon() {
            Some(ServoInput::Ime(ImeEvent::Composition(end))) => {
                assert_eq!((end.state, end.data.as_str()), (CompositionState::End, ""))
            }
            other => panic!("{other:?}"),
        }
        assert!(composition.abandon().is_none());
    }

    #[test]
    fn losing_focus_releases_what_the_page_holds_and_nothing_else() {
        let mut keys = Keys::default();
        let none = Modifiers::default();
        let ctrl = Modifiers {
            control: true,
            ..none
        };
        keys.key(&key_event(
            29,
            Key::Named("Control".into()),
            ButtonState::Pressed,
            ctrl,
        ));
        keys.key(&key_event(
            30,
            Key::Character("a".into()),
            ButtonState::Pressed,
            ctrl,
        ));
        // A release whose press the page never saw (a browser shortcut) doesn't reach it.
        assert!(keys
            .key(&key_event(
                38,
                Key::Character("l".into()),
                ButtonState::Released,
                ctrl
            ))
            .is_none());
        let released: Vec<_> = keys.release_all().into_iter().map(keyboard).collect();
        assert_eq!(released.len(), 2);
        assert!(released.iter().all(|k| k.4 == KeyState::Up));
        assert_eq!(released[0].0, ServoKey::Named(NamedKey::Control));
        assert_eq!(released[1].0, ServoKey::Character("a".into()));
        assert!(keys.release_all().is_empty(), "released once");
        // The real release, later, is not the page's any more.
        assert!(keys
            .key(&key_event(
                30,
                Key::Character("a".into()),
                ButtonState::Released,
                none
            ))
            .is_none());
    }

    fn compositions(events: Vec<ServoInput>) -> Vec<(CompositionState, String)> {
        events
            .into_iter()
            .map(|e| match e {
                ServoInput::Ime(ImeEvent::Composition(c)) => (c.state, c.data),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn input_method_text_arrives_as_one_composition() {
        let mut ime = Composition::default();
        let preedit = |text: &str| TextInputEvent {
            preedit: Some(Preedit {
                text: text.into(),
                cursor: None,
            }),
            ..TextInputEvent::default()
        };
        assert_eq!(
            compositions(ime.update(&preedit("k"))),
            [
                (CompositionState::Start, String::new()),
                (CompositionState::Update, "k".into())
            ]
        );
        assert_eq!(
            compositions(ime.update(&preedit("かん"))),
            [(CompositionState::Update, "かん".into())]
        );
        let commit = TextInputEvent {
            commit: Some("漢".into()),
            ..TextInputEvent::default()
        };
        assert_eq!(
            compositions(ime.update(&commit)),
            [(CompositionState::End, "漢".into())]
        );
        // A commit with no composition before it still starts one.
        assert_eq!(
            compositions(ime.update(&commit)),
            [
                (CompositionState::Start, String::new()),
                (CompositionState::End, "漢".into())
            ]
        );
    }

    #[test]
    fn password_fields_give_the_input_method_no_text() {
        let area = DeviceIntRect::new(DeviceIntPoint::new(150, 90), DeviceIntPoint::new(450, 120));
        let m = metrics(180);
        let password = text_input_state(
            InputMethodType::Password,
            "hunter2",
            Some(3),
            area,
            false,
            &m,
        );
        assert_eq!(password.purpose, TextPurpose::Password);
        assert_eq!(password.surrounding, None);
        // The field's area, back in surface-local logical pixels: at 150 %, 1280 buffer pixels
        // are 853 logical ones, so x 150..450 covers 99.96..299.88, widened to 99..300.
        assert_eq!(
            password.caret,
            SurfaceRect {
                x: 99,
                y: 60,
                width: 201,
                height: 20
            }
        );
        // Servo's cursor counts UTF-16 units: after "a😀" is byte 5.
        let text = text_input_state(InputMethodType::Text, "a😀b", Some(3), area, false, &m);
        assert_eq!(text.surrounding.unwrap().cursor, 5);
    }

    #[test]
    fn cursors_keep_their_css_names() {
        assert_eq!(cursor(servo::Cursor::NeswResize).css_name(), "nesw-resize");
        assert_eq!(cursor(servo::Cursor::Pointer), Cursor::Pointer);
    }
}
