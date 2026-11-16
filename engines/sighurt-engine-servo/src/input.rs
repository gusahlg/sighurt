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
//! Servo 0.7 has no smooth scrolling: a notch was one 60-pixel jump, which stays the default.
//! With `[engines.servo] smooth-scroll = true`, [`SmoothScroll`] plays a
//! notch out over [`SMOOTH_DURATION`] as a run of smaller pixel wheel events: one when the notch
//! comes, then one after each frame the view presents (so the display's rate paces it), or after
//! [`SMOOTH_FALLBACK`] without a frame (the runtime pumps at that deadline). Nothing steps when
//! no scroll is in flight. A page sees those wheel events, as it
//! sees a touchpad's, instead of one per notch: other browsers animate the scroll after one DOM
//! event, which Servo's public API can't do without bypassing the page's `preventDefault`
//! (`WebView::notify_scroll_event`). Touchpad (pixel) deltas go to Servo as they come.
//!
//! Servo's script thread merges a wheel event it has not processed yet with the next one of the
//! same delta mode, summing the deltas and keeping the newer point. So [`SmoothScroll`] never
//! sends an event for one point while Servo has not reported the latest wheel event it was sent
//! for another point handled (`WebViewDelegate::notify_input_event_handled`; the view tells it
//! with [`SmoothScroll::dispatched`] and the runtime with [`SmoothScroll::handled`]). What waits
//! goes out as Servo answers, remainders of cut-short runs first, also while the view is hidden.
//! The wait is bounded by [`GATE_TIMEOUT`], after which a script thread that busy may still
//! merge two events. A touchpad's pixel events are never held, so one at another point can
//! still merge with a pending smooth event.
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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::{Duration, Instant};

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

/// How long a wheel notch's scroll plays out: about what WebKit's smooth scrolling takes (6 or 7
/// frames a notch at 60 Hz).
const SMOOTH_DURATION: Duration = Duration::from_millis(120);

/// The longest a run waits for the view's next frame before it steps anyway: a step that
/// changed nothing on screen (the page's end, a `preventDefault`) brings no frame.
const SMOOTH_FALLBACK: Duration = Duration::from_millis(25);

/// The frame interval assumed until two frames of a run have been seen, and its bounds.
const FRAME_GUESS: Duration = Duration::from_millis(16);
const FRAME_MIN: Duration = Duration::from_millis(4);
const FRAME_MAX: Duration = Duration::from_millis(33);

/// The most cut-short runs [`SmoothScroll`] keeps owing their remainders.
const MAX_OWED: usize = 4;

/// The longest [`SmoothScroll`] holds an event for a point back because Servo has not said it
/// handled the wheel event it was sent for another point.
const GATE_TIMEOUT: Duration = Duration::from_millis(500);

/// Wheel notches played out as a run of smaller wheel events, one per frame of the view.
#[derive(Debug)]
pub(crate) struct SmoothScroll {
    /// `[engines.servo] smooth-scroll`. Off, no run is ever made: [`Self::add`] is not called
    /// (the view sends the notch as it is), so nothing steps and nothing is due.
    enabled: bool,
    run: Option<Run>,
    /// Motion a notch at another point cut short, or that [`Self::finish`] ended: what its run
    /// had not scrolled, still owed to its own point, oldest first. One event of it goes out
    /// before each step of `run`, at most [`MAX_OWED`] of them (the oldest is dropped beyond
    /// that).
    owed: Vec<((f64, f64), WebViewPoint)>,
    /// The latest wheel event sent to Servo that it has not reported handled.
    pending: Option<Pending>,
    /// When the state last changed, which is when something held back became due.
    touched: Instant,
    /// The view's latest frame, and the interval between its last two while a run lasted.
    last_frame: Option<Instant>,
    interval: Duration,
}

/// A wheel event Servo has not reported handled. Servo's script thread merges a pending wheel
/// event with the next one and keeps only the newer point, so an event for another point must
/// wait until this one is handled.
#[derive(Debug)]
struct Pending {
    /// The hash of Servo's opaque `InputEventId`.
    id: u64,
    point: WebViewPoint,
    sent: Instant,
}

/// What [`SmoothScroll`] keeps of an `InputEventId`, which Servo lets nobody make.
fn id_key(id: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

/// One smooth scroll in flight. Deltas are Servo's device pixels with its wheel sign.
#[derive(Debug)]
struct Run {
    start: Instant,
    /// What the whole run scrolls.
    total: (f64, f64),
    /// What its events have scrolled so far: whole pixels until the last one, and always between
    /// nothing and `total`.
    sent: (f64, f64),
    /// Where the latest notch pointed.
    point: WebViewPoint,
    /// When the run last sent an event.
    stepped: Instant,
}

impl SmoothScroll {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            run: None,
            owed: Vec::new(),
            pending: None,
            touched: Instant::now(),
            last_frame: None,
            interval: FRAME_GUESS,
        }
    }

    /// Whether wheel notches are played out ([`Self::add`]) or sent as they are.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Records that the view sent Servo the wheel event `id` at `point` at `now`, whatever sent
    /// it (a step, an owed remainder, a touchpad, a notch with smooth scrolling off). Servo
    /// merges it with the next wheel event it has not processed yet, so [`Self::step`] sends
    /// nothing for another point until [`Self::handled`] says it was. Does nothing while smooth
    /// scrolling is off.
    pub fn dispatched(&mut self, id: impl Hash, point: WebViewPoint, now: Instant) {
        if self.enabled {
            self.pending = Some(Pending {
                id: id_key(id),
                point,
                sent: now,
            });
        }
    }

    /// Servo reported the wheel event `id` handled. It answers in order, so only the latest one
    /// sent matters; what was held back for it is due at once.
    pub fn handled(&mut self, id: impl Hash, now: Instant) {
        if self.pending.as_ref().is_some_and(|p| p.id == id_key(id)) {
            self.pending = None;
            self.touched = now;
        }
    }

    /// Whether an event for `point` may go to Servo at `now`: nothing is pending for another
    /// point, or the pending event is older than [`GATE_TIMEOUT`] (a script thread that busy
    /// may still merge the two; the wait is bounded rather than the scroll lost).
    fn open(&self, point: WebViewPoint, now: Instant) -> bool {
        self.pending.as_ref().is_none_or(|p| {
            p.point == point || now.saturating_duration_since(p.sent) >= GATE_TIMEOUT
        })
    }

    /// Adds the notch `wheel` (a [`pointer`] wheel event) at `now`. What a run in flight has
    /// not scrolled yet joins the new one, which starts again from `now` and steps at the
    /// view's next frame; a new run steps at once, so the page moves with the notch. A notch
    /// at another point than the run's does not take over its remainder: that is still owed to
    /// the old point and goes out there, before the new run's next step. The new run's first
    /// event waits while Servo still has a wheel event for another point ([`Self::dispatched`]).
    pub fn add(&mut self, wheel: &WheelEvent, now: Instant) -> Option<ServoInput> {
        let mut previous = self.run.take();
        if let Some(run) = &previous {
            if run.point != wheel.point {
                self.owe(run);
                previous = None;
            }
        }
        let in_flight = previous.is_some() || !self.owed.is_empty();
        let (x, y, stepped) = previous.map_or((0.0, 0.0, now), |run| {
            (
                run.total.0 - run.sent.0,
                run.total.1 - run.sent.1,
                run.stepped,
            )
        });
        self.run = Some(Run {
            start: now,
            total: (x + wheel.delta.x, y + wheel.delta.y),
            sent: (0.0, 0.0),
            point: wheel.point,
            stepped,
        });
        self.touched = now;
        if in_flight {
            None
        } else {
            self.step(now)
        }
    }

    /// Queues what `run` has not scrolled as owed to its point.
    fn owe(&mut self, run: &Run) {
        let rest = (run.total.0 - run.sent.0, run.total.1 - run.sent.1);
        if rest != (0.0, 0.0) {
            if self.owed.len() == MAX_OWED {
                self.owed.remove(0);
            }
            self.owed.push((rest, run.point));
        }
    }

    /// The view presented a frame at `now`: the next event, which the next frame shows.
    pub fn frame(&mut self, now: Instant) -> Option<ServoInput> {
        // Only the gap between two frames of the run says how long a frame is: one that began
        // before the run (an idle view's last frame) would shorten the look-ahead at once.
        if let (Some(run), Some(last)) = (&self.run, self.last_frame) {
            if last < run.start {
                self.last_frame = Some(now);
                return self.step(now);
            }
            let gap = now.saturating_duration_since(last);
            if gap <= FRAME_MAX * 2 {
                self.interval = gap.clamp(FRAME_MIN, FRAME_MAX);
            }
        }
        self.last_frame = Some(now);
        self.step(now)
    }

    /// The next event when no frame came for [`SMOOTH_FALLBACK`], or when what was held back
    /// for Servo's answer is due. Call it again after a call that sent an event was recorded
    /// with [`Self::dispatched`]: it stops at once for another point.
    pub fn fallback(&mut self, now: Instant) -> Option<ServoInput> {
        match self.deadline() {
            Some(due) if now >= due => self.step(now),
            _ => None,
        }
    }

    /// When [`Self::fallback`] steps: nothing while nothing is owed and no run is in flight; for
    /// an event held back for Servo's answer, the end of [`GATE_TIMEOUT`] (the answer
    /// itself makes it due at once); for a run, [`SMOOTH_FALLBACK`] after its last event.
    pub fn deadline(&self) -> Option<Instant> {
        let (point, due) = match (self.owed.first(), &self.run) {
            (Some((_, point)), _) => (*point, self.touched),
            (None, Some(run)) => (run.point, run.stepped + SMOOTH_FALLBACK),
            (None, None) => return None,
        };
        match &self.pending {
            Some(p) if p.point != point => Some(p.sent + GATE_TIMEOUT),
            _ => Some(due),
        }
    }

    /// The next event: the oldest owed remainder, else the run's, which takes the run to where
    /// its curve is one frame after `now` (the frame it will be shown in; the last sends what
    /// is left and ends the run). `None` when Servo has not answered for another point.
    fn step(&mut self, now: Instant) -> Option<ServoInput> {
        if let Some(&(delta, point)) = self.owed.first() {
            if let Some(run) = self.run.as_mut() {
                // A run that has not started waits for its turn and starts then.
                if run.sent == (0.0, 0.0) {
                    run.start = now;
                }
            }
            if !self.open(point, now) {
                return None;
            }
            self.owed.remove(0);
            self.touched = now;
            if let Some(run) = self.run.as_mut() {
                run.stepped = now;
            }
            return Some(wheel(delta, point));
        }
        let point = self.run.as_ref()?.point;
        let open = self.open(point, now);
        let run = self.run.as_mut()?;
        if !open {
            if run.sent == (0.0, 0.0) {
                run.start = now;
            }
            return None;
        }
        let elapsed = (now + self.interval).saturating_duration_since(run.start);
        let t = elapsed.as_secs_f64() / SMOOTH_DURATION.as_secs_f64();
        let target = if t >= 1.0 {
            run.total
        } else {
            // Ease out (cubic): fast at the notch, slowing to a stop.
            let p = 1.0 - (1.0 - t).powi(3);
            ((run.total.0 * p).round(), (run.total.1 * p).round())
        };
        // The look-ahead is the interval between frames, which can shorten between two steps and
        // put the curve behind what was sent: never step back, hold instead. Rounding to whole
        // pixels can pass a fractional total: never go beyond it either.
        let bounded = |target: f64, sent: f64, total: f64| {
            if total >= 0.0 {
                target.max(sent).min(total)
            } else {
                target.min(sent).max(total)
            }
        };
        let target = (
            bounded(target.0, run.sent.0, run.total.0),
            bounded(target.1, run.sent.1, run.total.1),
        );
        let delta = (target.0 - run.sent.0, target.1 - run.sent.1);
        run.sent = target;
        run.stepped = now;
        let point = run.point;
        if t >= 1.0 {
            self.run = None;
        }
        self.touched = now;
        (delta != (0.0, 0.0)).then(|| wheel(delta, point))
    }

    /// Ends the run in flight, whose remainder joins what is owed, and returns the next event
    /// Servo may have at `now` (the oldest remainder). Servo merges wheel events for different
    /// points that are pending together, so this is no longer everything at once: call it
    /// until it returns `None`, recording each event with [`Self::dispatched`]; what it holds
    /// back goes out through [`Self::fallback`] as Servo answers, hidden or not.
    pub fn finish(&mut self, now: Instant) -> Option<ServoInput> {
        if let Some(run) = self.run.take() {
            self.owe(&run);
        }
        self.step(now)
    }

    /// Drops the run, what it owes and the wheel event Servo has not answered: the document they
    /// were meant for is gone.
    pub fn clear(&mut self) {
        self.run = None;
        self.owed.clear();
        self.pending = None;
    }
}

/// A pixel wheel event of `delta` (device pixels, Servo's sign) at `point`.
fn wheel((x, y): (f64, f64), point: WebViewPoint) -> ServoInput {
    let delta = WheelDelta {
        x,
        y,
        z: 0.0,
        mode: WheelMode::DeltaPixel,
    };
    ServoInput::Wheel(WheelEvent::new(delta, point))
}

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
            wheel((-device.x, -device.y), point(position))
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

    fn notch(y: f64) -> WheelEvent {
        let delta = WheelDelta {
            x: 0.0,
            y,
            z: 0.0,
            mode: WheelMode::DeltaPixel,
        };
        WheelEvent::new(delta, WebViewPoint::Device(DevicePoint::new(5.0, 6.0)))
    }

    fn dy(event: Option<ServoInput>) -> f64 {
        match event {
            Some(ServoInput::Wheel(w)) => w.delta.y,
            None => 0.0,
            other => panic!("{other:?}"),
        }
    }

    /// Plays a run out with a frame every `frame` from `start` until it ends: the deltas and
    /// the number of frames.
    fn play(scroll: &mut SmoothScroll, start: Instant, frame: Duration) -> (Vec<f64>, usize) {
        let mut deltas = Vec::new();
        let mut now = start;
        while scroll.deadline().is_some() {
            now += frame;
            deltas.push(dy(scroll.frame(now)));
            assert!(deltas.len() < 100, "a run ends");
        }
        let frames = deltas.len();
        (deltas, frames)
    }

    #[test]
    fn a_notch_plays_out_over_frames_and_scrolls_exactly_its_delta() {
        let mut scroll = SmoothScroll::new(true);
        let start = Instant::now();
        let first = dy(scroll.add(&notch(-75.0), start));
        assert!(
            first < 0.0 && first > -75.0,
            "the first step moves at once: {first}"
        );
        let (rest, frames) = play(&mut scroll, start, Duration::from_micros(16_667));
        assert_eq!(first + rest.iter().sum::<f64>(), -75.0);
        // 120 ms at 60 Hz: about 7 frames, slowing down.
        assert!((5..=8).contains(&frames), "{frames} frames");
        assert!(rest.last().unwrap().abs() <= first.abs());
        assert!(scroll.deadline().is_none(), "nothing is due after the run");
        assert!(scroll.frame(start + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn a_faster_display_gets_more_steps() {
        let mut scroll = SmoothScroll::new(true);
        let start = Instant::now();
        let first = dy(scroll.add(&notch(-60.0), start));
        let (rest, frames) = play(&mut scroll, start, Duration::from_micros(8_333));
        assert_eq!(first + rest.iter().sum::<f64>(), -60.0);
        assert!((12..=16).contains(&frames), "{frames} frames at 120 Hz");
    }

    #[test]
    fn without_frames_the_run_steps_at_its_fallback() {
        let mut scroll = SmoothScroll::new(true);
        let start = Instant::now();
        let mut sent = dy(scroll.add(&notch(-60.0), start));
        assert!(scroll.fallback(start + Duration::from_millis(1)).is_none());
        assert_eq!(scroll.deadline(), Some(start + SMOOTH_FALLBACK));
        let mut steps = 0;
        while let Some(due) = scroll.deadline() {
            sent += dy(scroll.fallback(due));
            steps += 1;
            assert!(steps < 20, "a run ends without frames");
        }
        assert_eq!(sent, -60.0);
    }

    #[test]
    fn a_notch_during_a_run_adds_what_is_left_and_waits_for_a_frame() {
        let mut scroll = SmoothScroll::new(true);
        let start = Instant::now();
        let frame = Duration::from_micros(16_667);
        let mut sent = dy(scroll.add(&notch(-60.0), start));
        sent += dy(scroll.frame(start + frame));
        assert!(scroll.add(&notch(-60.0), start + frame).is_none());
        let (rest, _) = play(&mut scroll, start + frame, frame);
        assert_eq!(sent + rest.iter().sum::<f64>(), -120.0);
    }

    #[test]
    fn finishing_sends_the_rest_at_once() {
        let mut scroll = SmoothScroll::new(true);
        let first = dy(scroll.add(&notch(60.0), Instant::now()));
        assert_eq!(first + dy(scroll.finish(Instant::now())), 60.0);
        assert!(scroll.deadline().is_none());
        assert!(scroll.finish(Instant::now()).is_none());
    }

    const TOTALS: [f64; 6] = [-0.625, 0.625, -75.0, 75.0, -0.3, 1.5];

    /// A delta that points the way the whole notch does, or is nothing.
    fn along(total: f64, delta: f64) -> bool {
        delta == 0.0 || (delta < 0.0) == (total < 0.0)
    }

    fn assert_exact(total: f64, deltas: &[f64]) {
        let mut sum = 0.0;
        for &delta in deltas {
            assert!(along(total, delta), "{delta} reverses a notch of {total}");
            sum += delta;
            assert!(sum.abs() <= total.abs(), "{sum} overshoots {total}");
        }
        assert_eq!(sum, total, "{deltas:?}");
    }

    #[test]
    fn a_fractional_notch_never_overshoots_or_reverses() {
        let frame = Duration::from_micros(16_667);
        for total in TOTALS {
            let mut scroll = SmoothScroll::new(true);
            let start = Instant::now();
            let mut deltas = vec![dy(scroll.add(&notch(total), start))];
            deltas.extend(play(&mut scroll, start, frame).0);
            assert_exact(total, &deltas);
        }
    }

    #[test]
    fn a_fractional_notch_cut_short_never_reverses() {
        let frame = Duration::from_micros(16_667);
        for total in TOTALS {
            for cut in 0..9u32 {
                let mut scroll = SmoothScroll::new(true);
                let start = Instant::now();
                let mut deltas = vec![dy(scroll.add(&notch(total), start))];
                for k in 1..=cut {
                    if scroll.deadline().is_some() {
                        deltas.push(dy(scroll.frame(start + frame * k)));
                    }
                }
                while let Some(rest) = scroll.finish(Instant::now()) {
                    deltas.push(dy(Some(rest)));
                }
                assert_exact(total, &deltas);
            }
        }
    }

    fn notch_at(y: f64, point: WebViewPoint) -> WheelEvent {
        WheelEvent::new(
            WheelDelta {
                x: 0.0,
                y,
                z: 0.0,
                mode: WheelMode::DeltaPixel,
            },
            point,
        )
    }

    fn event(input: Option<ServoInput>) -> WheelEvent {
        match input.unwrap() {
            ServoInput::Wheel(event) => event,
            other => panic!("{other:?}"),
        }
    }

    fn at(x: f32, y: f32) -> WebViewPoint {
        WebViewPoint::Device(DevicePoint::new(x, y))
    }

    #[test]
    fn astra_review_smooth_scroll_never_reverses_a_single_notch() {
        let mut scroll = SmoothScroll::new(true);
        let t = Instant::now();
        assert!(scroll.frame(t).is_none());
        let first = event(scroll.add(&notch_at(-60.0, at(5.0, 6.0)), t + Duration::from_millis(1)));
        assert!(first.delta.y < 0.0);
        let next = event(scroll.frame(t + Duration::from_millis(2)));
        assert!(
            next.delta.y <= 0.0,
            "one downward notch reversed by {} pixels",
            next.delta.y
        );
    }

    #[test]
    fn astra_review_smooth_scroll_does_not_move_old_delta_to_a_new_target() {
        let mut scroll = SmoothScroll::new(true);
        let t = Instant::now();
        let a = at(5.0, 6.0);
        let b = at(500.0, 600.0);
        let first = event(scroll.add(&notch_at(-60.0, a), t));
        assert_eq!(first.point, a);
        assert!(scroll
            .add(&notch_at(-60.0, b), t + Duration::from_millis(10))
            .is_none());
        // The old point's remainder goes first, then the new run's.
        let owed = event(scroll.finish(Instant::now()));
        assert_eq!(owed.point, a);
        assert_eq!(first.delta.y + owed.delta.y, -60.0);
        let rest = event(scroll.finish(Instant::now()));
        assert_eq!(rest.point, b);
        assert!(
            rest.delta.y >= -60.0,
            "new target received {} pixels from a 60-pixel notch",
            rest.delta.y
        );
    }

    #[test]
    fn a_cut_short_run_finishes_at_its_own_point_before_the_new_one_steps() {
        let mut scroll = SmoothScroll::new(true);
        let t = Instant::now();
        let (a, b) = (at(5.0, 6.0), at(500.0, 600.0));
        let first = event(scroll.add(&notch_at(-60.0, a), t));
        assert!(scroll
            .add(&notch_at(-60.0, b), t + Duration::from_millis(10))
            .is_none());
        let frame = Duration::from_micros(16_667);
        let owed = event(scroll.frame(t + frame));
        assert_eq!(owed.point, a);
        assert_eq!(first.delta.y + owed.delta.y, -60.0);
        let mut sent_b = 0.0;
        let mut now = t + frame;
        while scroll.deadline().is_some() {
            now += frame;
            // A step that rounds to nothing sends nothing.
            if let Some(ServoInput::Wheel(step)) = scroll.frame(now) {
                assert_eq!(step.point, b);
                sent_b += step.delta.y;
            }
            assert!(now < t + Duration::from_secs(1), "a run ends");
        }
        assert_eq!(sent_b, -60.0);
        // The same, drained by finish: each point gets exactly its notch.
        let mut scroll = SmoothScroll::new(true);
        let first = event(scroll.add(&notch_at(-60.0, a), t));
        scroll.add(&notch_at(-60.0, b), t);
        let (mut on_a, mut on_b) = (first.delta.y, 0.0);
        while let Some(ServoInput::Wheel(w)) = scroll.finish(Instant::now()) {
            if w.point == a {
                on_a += w.delta.y;
            } else {
                on_b += w.delta.y;
            }
        }
        assert_eq!((on_a, on_b), (-60.0, -60.0));
    }

    /// Sends `input` as the view does: records it with Servo's id `id` when there is one.
    fn send(
        scroll: &mut SmoothScroll,
        input: Option<ServoInput>,
        id: u32,
        now: Instant,
    ) -> Option<WheelEvent> {
        let ServoInput::Wheel(wheel) = input? else {
            panic!("not a wheel event")
        };
        scroll.dispatched(id, wheel.point, now);
        Some(wheel)
    }

    #[test]
    fn a_notch_at_another_point_waits_for_servos_answer() {
        let mut scroll = SmoothScroll::new(true);
        let t = Instant::now();
        let (a, b) = (at(5.0, 6.0), at(500.0, 600.0));
        let frame = Duration::from_micros(16_667);
        let add = scroll.add(&notch_at(-60.0, a), t);
        let mut on_a = send(&mut scroll, add, 1, t).unwrap().delta.y;
        let add = scroll.add(&notch_at(-60.0, b), t + frame);
        assert!(add.is_none());
        // The old point's remainder is not held back by its own point's event.
        let owed = {
            let input = scroll.frame(t + frame * 2);
            send(&mut scroll, input, 2, t + frame * 2)
        }
        .unwrap();
        assert_eq!(owed.point, a);
        on_a += owed.delta.y;
        assert_eq!(on_a, -60.0);
        // Nothing at B until Servo answers for the latest event at A, however many frames pass.
        for k in 3..12 {
            assert!(scroll.frame(t + frame * k).is_none(), "frame {k}");
        }
        assert_eq!(scroll.deadline(), Some(t + frame * 2 + GATE_TIMEOUT));
        // An answer for an earlier event does not open the gate.
        scroll.handled(1u32, t + frame * 12);
        assert!(scroll.frame(t + frame * 12).is_none());
        // The answer does, and the held run starts its curve then: it does not jump.
        let answered = t + frame * 13;
        scroll.handled(2u32, answered);
        assert!(scroll.deadline().unwrap() <= answered);
        let step = {
            let input = scroll.fallback(answered);
            send(&mut scroll, input, 3, answered)
        }
        .unwrap();
        assert_eq!(step.point, b);
        assert!(
            step.delta.y < 0.0 && step.delta.y > -60.0,
            "{}",
            step.delta.y
        );
        let mut on_b = step.delta.y;
        let mut now = answered;
        while scroll.deadline().is_some() {
            now += frame;
            if let Some(w) = {
                let input = scroll.frame(now);
                send(&mut scroll, input, 4, now)
            } {
                assert_eq!(w.point, b);
                on_b += w.delta.y;
            }
            scroll.handled(4u32, now);
            assert!(now < t + Duration::from_secs(2), "a run ends");
        }
        assert_eq!(on_b, -60.0);
    }

    #[test]
    fn finishing_sends_one_point_at_a_time_and_the_rest_as_servo_answers() {
        let mut scroll = SmoothScroll::new(true);
        let t = Instant::now();
        let (a, b) = (at(5.0, 6.0), at(500.0, 600.0));
        let add = scroll.add(&notch_at(-60.0, a), t);
        let mut on_a = send(&mut scroll, add, 1, t).unwrap().delta.y;
        assert!(scroll.add(&notch_at(-60.0, b), t).is_none());
        // A's remainder goes; B's waits for the answer to that.
        let owed = {
            let input = scroll.finish(t);
            send(&mut scroll, input, 2, t)
        }
        .unwrap();
        assert_eq!(owed.point, a);
        on_a += owed.delta.y;
        assert!(scroll.finish(t).is_none());
        assert!(scroll.fallback(t + Duration::from_millis(100)).is_none());
        assert_eq!(scroll.deadline(), Some(t + GATE_TIMEOUT));
        // Servo answers: B's whole notch goes, at B, and nothing is due after it.
        let answered = t + Duration::from_millis(30);
        scroll.handled(2u32, answered);
        assert!(scroll.deadline().unwrap() <= answered);
        let rest = {
            let input = scroll.fallback(answered);
            send(&mut scroll, input, 3, answered)
        }
        .unwrap();
        assert_eq!((on_a, rest.point, rest.delta.y), (-60.0, b, -60.0));
        assert!(scroll.finish(answered).is_none());
        assert!(scroll.deadline().is_none());
    }

    #[test]
    fn the_gate_opens_after_its_timeout() {
        let mut scroll = SmoothScroll::new(true);
        let t = Instant::now();
        let (a, b) = (at(5.0, 6.0), at(500.0, 600.0));
        let add = scroll.add(&notch_at(-60.0, a), t);
        send(&mut scroll, add, 1, t).unwrap();
        // The same point's events are never held.
        let again = {
            let input = scroll.finish(t);
            send(&mut scroll, input, 2, t)
        };
        assert!(again.is_some());
        scroll.add(&notch_at(-60.0, b), t);
        assert!(scroll.frame(t + Duration::from_millis(10)).is_none());
        let due = scroll.deadline().unwrap();
        assert_eq!(due, t + GATE_TIMEOUT);
        assert!(scroll.fallback(due - Duration::from_millis(1)).is_none());
        let step = event(scroll.fallback(due));
        assert_eq!(step.point, b);
    }

    #[test]
    fn with_smooth_scrolling_off_nothing_is_recorded_or_due() {
        let mut scroll = SmoothScroll::new(false);
        let t = Instant::now();
        scroll.dispatched(1u32, at(1.0, 1.0), t);
        scroll.handled(1u32, t);
        assert!(scroll.deadline().is_none());
        assert!(scroll.fallback(t + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn clearing_drops_a_run_and_what_it_owes() {
        let mut scroll = SmoothScroll::new(true);
        let t = Instant::now();
        scroll.add(&notch_at(-60.0, at(1.0, 1.0)), t);
        scroll.add(&notch_at(-60.0, at(2.0, 2.0)), t);
        scroll.clear();
        assert!(scroll.deadline().is_none());
        assert!(scroll.frame(t + Duration::from_secs(1)).is_none());
        assert!(scroll.finish(Instant::now()).is_none());
    }

    #[test]
    fn with_smooth_scrolling_off_nothing_is_ever_due() {
        let mut scroll = SmoothScroll::new(false);
        assert!(!scroll.enabled());
        assert!(SmoothScroll::new(true).enabled());
        // The view never calls `add` while it is off, so no run exists to step.
        let now = Instant::now();
        assert!(scroll.deadline().is_none());
        assert!(scroll.frame(now).is_none());
        assert!(scroll.fallback(now + Duration::from_secs(1)).is_none());
        assert!(scroll.finish(Instant::now()).is_none());
        assert!(scroll.deadline().is_none());
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
