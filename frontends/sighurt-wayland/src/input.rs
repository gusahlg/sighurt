//! Seats, pointer and keyboard: raw Wayland input into `sighurt_api::input` events.
//!
//! Tracks each seat separately. Keyboard: xkbcommon keymaps and compose, the physical key, the
//! layout's key value, modifiers (AltGr separate from Ctrl and Alt) and the modifiers the layout
//! consumed, client-side key repeat on calloop timers, and committed text, so Swedish
//! characters, dead keys and compose sequences survive. Subsurfaces never get keyboard focus,
//! so keyboard input for the window is routed to the logical focus target the application sets
//! ([`Host::set_keyboard_focus`]); losing window activation reports
//! [`FocusEvent::Deactivated`] and stops repeat.
//!
//! Pointer: positions stay surface-local to the target under the pointer. The target that gets
//! a button press keeps receiving motion, buttons and scrolling, in its own coordinates, until
//! the last button is released, even when the pointer crosses into another surface; then the
//! target under the pointer gets an enter. If the compositor takes the pointer away while a
//! button is held, the drag recipient gets the releases, so no button stays stuck. Pointer
//! events carry the seat's current keyboard modifiers. Scrolling keeps smooth pixels and wheel
//! 120ths apart. The cursor is set by shape where `wp_cursor_shape_v1` exists, else from the
//! cursor theme (both through smithay-client-toolkit).
//!
//! Must not own: shortcut policy (the core decides) or input-method text (see `text_input`).

use std::time::{Duration, Instant};

use calloop::RegistrationToken;
use sighurt_api::events::Cursor;
use sighurt_api::geometry::{SurfacePoint, WindowRect};
use sighurt_api::ids::{SeatId, TargetId, WindowId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers,
    PointerButton, PointerEvent, ScrollDelta,
};
use smithay_client_toolkit::seat::pointer::{
    AxisScroll, CursorIcon, PointerData, PointerEvent as SctkPointerEvent, PointerEventKind,
    PointerHandler, ThemeSpec, ThemedPointer,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{AxisSource, WlPointer};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use xkbcommon::xkb;

use crate::connection::{Host, HostEvent, SurfaceOwner, TimerEvent};

/// One seat's devices and state.
pub(crate) struct Seat {
    pub id: SeatId,
    pub wl: WlSeat,
    pub keyboard: Option<Keyboard>,
    pub pointer: Option<Pointer>,
    pub modifiers: Modifiers,
    /// The serial of the latest input event, for requests that need one (the clipboard).
    pub serial: Option<u32>,
    pub text_input: Option<crate::text_input::TextInput>,
    pub data_device: Option<smithay_client_toolkit::data_device_manager::data_device::DataDevice>,
    pub primary: Option<smithay_client_toolkit::primary_selection::device::PrimarySelectionDevice>,
}

pub(crate) struct Keyboard {
    wl: WlKeyboard,
    xkb: Option<Xkb>,
    /// The window with this seat's keyboard focus.
    window: Option<WindowId>,
    repeat: Repeat,
    repeat_timer: Option<RegistrationToken>,
}

pub(crate) struct Pointer {
    themed: ThemedPointer<PointerData>,
    router: PointerRouter<SurfaceOwner>,
    /// The cursor last set, and whether it must be set again (after an enter).
    cursor: Option<Cursor>,
}

impl Keyboard {
    /// Stops repeat and hands back its timer, for the caller to remove.
    pub fn take_repeat_timer(&mut self) -> Option<RegistrationToken> {
        self.repeat.cancel();
        self.repeat_timer.take()
    }
}

impl Seat {
    /// Drops every reference to `window`, which closed. A key repeating into it stops; its
    /// timer is handed back for the caller to remove.
    #[must_use]
    pub fn forget_window(&mut self, window: WindowId) -> Option<RegistrationToken> {
        let mut repeat_timer = None;
        if let Some(keyboard) = self.keyboard.as_mut().filter(|k| k.window == Some(window)) {
            keyboard.window = None;
            repeat_timer = keyboard.take_repeat_timer();
        }
        if let Some(pointer) = self.pointer.as_mut() {
            if pointer
                .router
                .target()
                .is_some_and(|s| s.window() == window)
            {
                pointer.router = PointerRouter::default();
            }
        }
        if let Some(ti) = self.text_input.as_mut() {
            ti.forget_window(window);
        }
        repeat_timer
    }

    pub fn new(wl: WlSeat) -> Self {
        Self {
            id: SeatId::next(),
            wl,
            keyboard: None,
            pointer: None,
            modifiers: Modifiers::default(),
            serial: None,
            text_input: None,
            data_device: None,
            primary: None,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Keyboard translation

/// Indices of the modifiers Sighurt reports, in one keymap.
struct ModIndices {
    shift: u32,
    control: u32,
    alt: u32,
    alt_graph: u32,
    logo: u32,
    caps_lock: u32,
    num_lock: u32,
}

/// A keymap, its state and the compose state: turns key codes into [`KeyEvent`]s.
pub(crate) struct Xkb {
    keymap: xkb::Keymap,
    state: xkb::State,
    compose: Option<xkb::compose::State>,
    mods: ModIndices,
}

/// The locale's compose table, or `en_US.UTF-8`'s when the locale has none (the "C" locale),
/// so dead keys work whatever the locale.
fn compose_table(context: &xkb::Context) -> Option<xkb::compose::Table> {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .into_iter()
        .filter_map(std::env::var_os)
        .find(|value| !value.is_empty());
    let from = |name: &std::ffi::OsStr| {
        xkb::compose::Table::new_from_locale(context, name, xkb::compose::COMPILE_NO_FLAGS).ok()
    };
    locale
        .as_deref()
        .and_then(from)
        .or_else(|| from("en_US.UTF-8".as_ref()))
}

impl Xkb {
    /// Translation for `keymap`, with dead keys and compose from `compose`.
    pub fn new(keymap: xkb::Keymap, compose: Option<&xkb::compose::Table>) -> Self {
        // The real modifiers every xkeyboard-config layout uses: AltGr (ISO level 3) is Mod5.
        let index = |name: &str| keymap.mod_get_index(name);
        let mods = ModIndices {
            shift: index(xkb::MOD_NAME_SHIFT),
            control: index(xkb::MOD_NAME_CTRL),
            alt: index(xkb::MOD_NAME_ALT),
            alt_graph: index(xkb::MOD_NAME_ISO_LEVEL3_SHIFT),
            logo: index(xkb::MOD_NAME_LOGO),
            caps_lock: index(xkb::MOD_NAME_CAPS),
            num_lock: index(xkb::MOD_NAME_NUM),
        };
        Self {
            state: xkb::State::new(&keymap),
            compose: compose
                .map(|table| xkb::compose::State::new(table, xkb::compose::STATE_NO_FLAGS)),
            keymap,
            mods,
        }
    }

    /// A keymap from a `wl_keyboard.keymap` file descriptor.
    fn from_fd(fd: std::os::fd::OwnedFd, size: u32) -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        // SAFETY: the compositor sent this fd and size for exactly this purpose; xkbcommon maps
        // it privately and read-only, as wl_keyboard version 7 requires.
        let keymap = unsafe {
            xkb::Keymap::new_from_fd(
                &context,
                fd,
                size as usize,
                xkb::KEYMAP_FORMAT_TEXT_V1,
                xkb::COMPILE_NO_FLAGS,
            )
        };
        let keymap = keymap.ok().flatten()?;
        Some(Self::new(keymap, compose_table(&context).as_ref()))
    }

    fn active(&self, index: u32) -> bool {
        index != xkb::MOD_INVALID
            && self
                .state
                .mod_index_is_active(index, xkb::STATE_MODS_EFFECTIVE)
    }

    fn mods_where(&self, keep: impl Fn(u32) -> bool) -> Modifiers {
        let m = &self.mods;
        let has = |index| index != xkb::MOD_INVALID && keep(index);
        Modifiers {
            shift: has(m.shift),
            control: has(m.control),
            alt: has(m.alt),
            alt_graph: has(m.alt_graph),
            logo: has(m.logo),
            caps_lock: has(m.caps_lock),
            num_lock: has(m.num_lock),
        }
    }

    /// The modifiers active now.
    pub fn modifiers(&self) -> Modifiers {
        self.mods_where(|index| self.active(index))
    }

    /// Applies `wl_keyboard.modifiers`.
    fn update_mask(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        self.state
            .update_mask(depressed, latched, locked, 0, 0, group);
    }

    /// Translates evdev key `code`. A press goes through dead-key and compose processing unless
    /// it is an auto-repeat. Also returns whether the press may auto-repeat: presses that only
    /// advance or finish a composition don't.
    pub fn key(&mut self, code: u32, state: ButtonState, repeat: bool) -> (KeyEvent, bool) {
        let keycode = xkb::Keycode::new(code + 8);
        let sym = self.state.key_get_one_sym(keycode);
        let modifiers = self.modifiers();
        let consumed = self.mods_where(|index| {
            self.active(index) && self.state.mod_index_is_consumed(keycode, index)
        });
        let plain = plain_key(sym);
        let typing_allowed =
            (!modifiers.control || consumed.control) && (!modifiers.logo || consumed.logo);
        let mut event = KeyEvent {
            code: KeyCode(code),
            key: plain.clone(),
            state,
            repeat,
            modifiers,
            consumed,
            text: None,
        };
        if state == ButtonState::Released {
            return (event, false);
        }
        let mut repeatable = self.keymap.key_repeats(keycode);
        let status = match self.compose.as_mut() {
            Some(compose) if !repeat => match compose.feed(sym) {
                xkb::compose::FeedResult::Accepted => compose.status(),
                xkb::compose::FeedResult::Ignored => xkb::compose::Status::Nothing,
            },
            _ => xkb::compose::Status::Nothing,
        };
        match status {
            xkb::compose::Status::Composing => {
                event.key = Key::Dead(dead_char(sym.raw()));
                repeatable = false;
            }
            xkb::compose::Status::Composed => {
                let composed = self.compose.as_mut().and_then(|c| {
                    let text = c.utf8();
                    c.reset();
                    text
                });
                match composed.filter(|t| !t.is_empty() && !t.chars().any(char::is_control)) {
                    Some(text) => {
                        event.key = Key::Character(text.clone());
                        event.text = typing_allowed.then_some(text);
                    }
                    None => event.key = Key::Unidentified,
                }
                repeatable = false;
            }
            xkb::compose::Status::Cancelled => {
                // The key broke a sequence: reported as itself, typing nothing, as xkbcommon
                // recommends.
                if let Some(compose) = self.compose.as_mut() {
                    compose.reset();
                }
                repeatable = false;
            }
            xkb::compose::Status::Nothing => {
                if let Key::Character(text) = &plain {
                    event.text = typing_allowed.then(|| text.clone());
                }
            }
        }
        (event, repeatable)
    }

    /// Forgets a half-typed composition, when focus leaves.
    fn reset_compose(&mut self) {
        if let Some(compose) = self.compose.as_mut() {
            compose.reset();
        }
    }

    #[cfg(test)]
    fn press_modifier(&mut self, code: u32, down: bool) {
        let direction = if down {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        self.state
            .update_key(xkb::Keycode::new(code + 8), direction);
    }
}

/// The W3C key value of `sym` without composition: a named key, a character, a dead key.
fn plain_key(sym: xkb::Keysym) -> Key {
    let raw = sym.raw();
    if let Some(name) = named_key(raw) {
        return Key::Named(name.into());
    }
    if (xkb::keysyms::KEY_dead_grave..=xkb::keysyms::KEY_dead_greek).contains(&raw) {
        return Key::Dead(dead_char(raw));
    }
    // keysym_to_utf8 applies no Control transformation, so Ctrl+A stays "a" (with Control).
    let text = xkb::keysym_to_utf8(sym);
    let text = text.trim_end_matches('\0');
    if text.is_empty() || text.chars().any(char::is_control) {
        Key::Unidentified
    } else {
        Key::Character(text.into())
    }
}

/// The spacing character a dead key adds.
#[allow(non_upper_case_globals)] // xkbcommon's keysym names.
fn dead_char(sym: u32) -> Option<char> {
    use xkb::keysyms::*;
    Some(match sym {
        KEY_dead_grave => '`',
        KEY_dead_acute => '´',
        KEY_dead_circumflex => '^',
        KEY_dead_tilde => '~',
        KEY_dead_macron => '¯',
        KEY_dead_breve => '˘',
        KEY_dead_abovedot => '˙',
        KEY_dead_diaeresis => '¨',
        KEY_dead_abovering => '˚',
        KEY_dead_doubleacute => '˝',
        KEY_dead_caron => 'ˇ',
        KEY_dead_cedilla => '¸',
        KEY_dead_ogonek => '˛',
        _ => return None,
    })
}

/// W3C `KeyboardEvent.key` names of keys that produce no character.
#[allow(non_upper_case_globals)] // xkbcommon's keysym names.
fn named_key(sym: u32) -> Option<&'static str> {
    use xkb::keysyms::*;
    Some(match sym {
        KEY_BackSpace => "Backspace",
        KEY_Tab | KEY_ISO_Left_Tab | KEY_KP_Tab => "Tab",
        KEY_Return | KEY_KP_Enter | KEY_ISO_Enter => "Enter",
        KEY_Escape => "Escape",
        KEY_Delete | KEY_KP_Delete => "Delete",
        KEY_Insert | KEY_KP_Insert => "Insert",
        KEY_Home | KEY_KP_Home => "Home",
        KEY_End | KEY_KP_End => "End",
        KEY_Left | KEY_KP_Left => "ArrowLeft",
        KEY_Up | KEY_KP_Up => "ArrowUp",
        KEY_Right | KEY_KP_Right => "ArrowRight",
        KEY_Down | KEY_KP_Down => "ArrowDown",
        KEY_Page_Up | KEY_KP_Page_Up => "PageUp",
        KEY_Page_Down | KEY_KP_Page_Down => "PageDown",
        KEY_KP_Begin => "Clear",
        KEY_Shift_L | KEY_Shift_R => "Shift",
        KEY_Control_L | KEY_Control_R => "Control",
        KEY_Alt_L | KEY_Alt_R => "Alt",
        KEY_ISO_Level3_Shift => "AltGraph",
        KEY_Mode_switch => "ModeChange",
        KEY_Meta_L | KEY_Meta_R | KEY_Super_L | KEY_Super_R => "Meta",
        KEY_Hyper_L | KEY_Hyper_R => "Hyper",
        KEY_Caps_Lock => "CapsLock",
        KEY_Num_Lock => "NumLock",
        KEY_Scroll_Lock => "ScrollLock",
        KEY_Pause => "Pause",
        KEY_Print => "PrintScreen",
        KEY_Menu => "ContextMenu",
        KEY_Multi_key => "Compose",
        KEY_XF86Back => "BrowserBack",
        KEY_XF86Forward => "BrowserForward",
        KEY_XF86Reload | KEY_XF86Refresh => "BrowserRefresh",
        KEY_XF86Stop => "BrowserStop",
        KEY_XF86HomePage => "BrowserHome",
        KEY_XF86Search => "BrowserSearch",
        KEY_XF86Favorites => "BrowserFavorites",
        KEY_XF86AudioMute => "AudioVolumeMute",
        KEY_XF86AudioLowerVolume => "AudioVolumeDown",
        KEY_XF86AudioRaiseVolume => "AudioVolumeUp",
        KEY_XF86AudioPlay => "MediaPlayPause",
        KEY_XF86AudioStop => "MediaStop",
        KEY_XF86AudioPrev => "MediaTrackPrevious",
        KEY_XF86AudioNext => "MediaTrackNext",
        KEY_F1..=KEY_F24 => FUNCTION_KEYS[(sym - KEY_F1) as usize],
        _ => return None,
    })
}

const FUNCTION_KEYS: [&str; 24] = [
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12", "F13", "F14", "F15",
    "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24",
];

// ---------------------------------------------------------------------------------------------
// Key repeat

/// Client-side key repeat, from `wl_keyboard.repeat_info`: which key repeats and when next.
#[derive(Debug)]
pub(crate) struct Repeat {
    /// Repeats per second; 0 disables repeat.
    rate: u32,
    delay: Duration,
    key: Option<u32>,
    next: Option<Instant>,
}

impl Default for Repeat {
    /// X's defaults, until the compositor says otherwise.
    fn default() -> Self {
        Self {
            rate: 25,
            delay: Duration::from_millis(600),
            key: None,
            next: None,
        }
    }
}

impl Repeat {
    /// `wl_keyboard.repeat_info`. A rate of 0 or less disables repeat.
    pub fn set_info(&mut self, rate: i32, delay: i32) {
        self.rate = u32::try_from(rate).unwrap_or(0);
        self.delay = Duration::from_millis(u64::try_from(delay).unwrap_or(0));
        if self.rate == 0 {
            self.cancel();
        }
    }

    /// A repeating key went down at `now`; it replaces any other. Returns when to repeat it.
    pub fn press(&mut self, key: u32, now: Instant) -> Option<Instant> {
        if self.rate == 0 {
            self.cancel();
            return None;
        }
        self.key = Some(key);
        self.next = Some(now + self.delay);
        self.next
    }

    /// `key` went up. Returns whether that stopped the repeat.
    pub fn release(&mut self, key: u32) -> bool {
        let stops = self.key == Some(key);
        if stops {
            self.cancel();
        }
        stops
    }

    pub fn cancel(&mut self) {
        self.key = None;
        self.next = None;
    }

    /// The timer fired at `now`: the key to repeat, and when to fire next. Repeats keep their
    /// cadence from the first one; a late loop skips the missed ones instead of bursting.
    pub fn fire(&mut self, now: Instant) -> Option<(u32, Instant)> {
        let (key, scheduled) = (self.key?, self.next?);
        let interval = Duration::from_secs(1) / self.rate;
        let mut next = scheduled + interval;
        if next <= now {
            next = now + interval;
        }
        self.next = Some(next);
        Some((key, next))
    }
}

// ---------------------------------------------------------------------------------------------
// Pointer routing

/// What the router asks to deliver, to surface `K`, in `K`'s coordinates.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Routed<K> {
    Enter(K, SurfacePoint),
    Leave(K),
    Motion(K, SurfacePoint),
    Button(K, u32, ButtonState, SurfacePoint),
    Scroll(K, SurfacePoint),
}

/// Which surface pointer events go to: the one under the pointer, or, while buttons are held,
/// the one that got the first press.
#[derive(Debug)]
pub(crate) struct PointerRouter<K> {
    /// The surface under the pointer and the position on it.
    hover: Option<(K, SurfacePoint)>,
    /// The drag recipient, while any button is held, and its last position.
    grab: Option<(K, SurfacePoint)>,
    buttons: Vec<u32>,
}

impl<K> Default for PointerRouter<K> {
    fn default() -> Self {
        Self {
            hover: None,
            grab: None,
            buttons: Vec::new(),
        }
    }
}

/// Converts a position on one surface into another's coordinates.
pub(crate) trait Rects<K> {
    fn rect(&self, surface: K) -> Option<WindowRect>;

    fn convert(&self, from: K, to: K, point: SurfacePoint) -> SurfacePoint
    where
        K: PartialEq + Copy,
    {
        if from == to {
            return point;
        }
        match (self.rect(from), self.rect(to)) {
            (Some(from), Some(to)) => to.to_surface(from.to_window(point)),
            _ => point,
        }
    }
}

impl<K: Copy + PartialEq> PointerRouter<K> {
    /// The surface that gets pointer input now.
    pub fn target(&self) -> Option<K> {
        self.grab.or(self.hover).map(|(k, _)| k)
    }

    fn route_to_grab(
        &mut self,
        rects: &impl Rects<K>,
        from: K,
        point: SurfacePoint,
    ) -> (K, SurfacePoint) {
        match self.grab.as_mut() {
            Some((grab, last)) => {
                *last = rects.convert(from, *grab, point);
                (*grab, *last)
            }
            None => (from, point),
        }
    }

    pub fn enter(
        &mut self,
        surface: K,
        point: SurfacePoint,
        rects: &impl Rects<K>,
        out: &mut Vec<Routed<K>>,
    ) {
        self.hover = Some((surface, point));
        match self.grab {
            None => out.push(Routed::Enter(surface, point)),
            Some(_) => {
                let (grab, point) = self.route_to_grab(rects, surface, point);
                out.push(Routed::Motion(grab, point));
            }
        }
    }

    pub fn leave(&mut self, surface: K, out: &mut Vec<Routed<K>>) {
        if self.hover.is_some_and(|(k, _)| k == surface) {
            self.hover = None;
        }
        if self.grab.is_none() {
            out.push(Routed::Leave(surface));
        }
    }

    pub fn motion(&mut self, point: SurfacePoint, rects: &impl Rects<K>, out: &mut Vec<Routed<K>>) {
        let Some((surface, _)) = self.hover else {
            return;
        };
        self.hover = Some((surface, point));
        let (target, point) = self.route_to_grab(rects, surface, point);
        out.push(Routed::Motion(target, point));
    }

    pub fn button(
        &mut self,
        button: u32,
        state: ButtonState,
        rects: &impl Rects<K>,
        out: &mut Vec<Routed<K>>,
    ) {
        match state {
            ButtonState::Pressed => {
                let Some((surface, point)) = self.hover else {
                    return;
                };
                if self.grab.is_none() {
                    self.grab = Some((surface, point));
                }
                if !self.buttons.contains(&button) {
                    self.buttons.push(button);
                }
                let (target, point) = self.route_to_grab(rects, surface, point);
                out.push(Routed::Button(target, button, state, point));
            }
            ButtonState::Released => {
                let Some((grab, last)) = self.grab else {
                    // A release without a press we saw (held before the pointer entered).
                    if let Some((surface, point)) = self.hover {
                        out.push(Routed::Button(surface, button, state, point));
                    }
                    return;
                };
                let point = match self.hover {
                    Some((surface, point)) => rects.convert(surface, grab, point),
                    None => last,
                };
                out.push(Routed::Button(grab, button, state, point));
                self.buttons.retain(|&b| b != button);
                if self.buttons.is_empty() {
                    self.grab = None;
                    match self.hover {
                        Some((surface, _)) if surface == grab => {}
                        Some((surface, point)) => {
                            out.push(Routed::Leave(grab));
                            out.push(Routed::Enter(surface, point));
                        }
                        None => out.push(Routed::Leave(grab)),
                    }
                }
            }
        }
    }

    pub fn scroll(&mut self, out: &mut Vec<Routed<K>>, rects: &impl Rects<K>) {
        if let Some((surface, point)) = self.hover {
            let (target, point) = self.route_to_grab(rects, surface, point);
            out.push(Routed::Scroll(target, point));
        } else if let Some((grab, last)) = self.grab {
            out.push(Routed::Scroll(grab, last));
        }
    }

    /// The end of a `wl_pointer.frame`. If the compositor took the pointer away from every
    /// surface while buttons were held, their releases will never come: deliver them now.
    pub fn end_frame(&mut self, out: &mut Vec<Routed<K>>) {
        if self.hover.is_none() {
            self.cancel(out);
        }
    }

    /// The device went away: ends a drag (its recipient gets the releases and the leave), or
    /// else the surface under the pointer gets its leave. Each surface that had the pointer
    /// gets one leave; one hovered during a drag never got an enter, so it gets none.
    pub fn drop_device(&mut self, out: &mut Vec<Routed<K>>) {
        match self.grab {
            Some(_) => self.cancel(out),
            None => {
                if let Some((surface, _)) = self.hover {
                    out.push(Routed::Leave(surface));
                }
            }
        }
        self.hover = None;
    }

    /// Ends a drag without its releases: the device went away, or its target did.
    pub fn cancel(&mut self, out: &mut Vec<Routed<K>>) {
        if let Some((grab, last)) = self.grab.take() {
            for button in self.buttons.drain(..) {
                out.push(Routed::Button(grab, button, ButtonState::Released, last));
            }
            out.push(Routed::Leave(grab));
        }
    }

    /// `surface` is gone: nothing more goes to it.
    pub fn forget(&mut self, surface: K) {
        if self.hover.is_some_and(|(k, _)| k == surface) {
            self.hover = None;
        }
        if self.grab.is_some_and(|(k, _)| k == surface) {
            self.grab = None;
            self.buttons.clear();
        }
    }
}

/// A Linux `BTN_*` code as a pointer button.
fn pointer_button(code: u32) -> PointerButton {
    use smithay_client_toolkit::seat::pointer::*;
    match code {
        BTN_LEFT => PointerButton::Left,
        BTN_RIGHT => PointerButton::Right,
        BTN_MIDDLE => PointerButton::Middle,
        BTN_SIDE | BTN_BACK => PointerButton::Back,
        BTN_EXTRA | BTN_FORWARD => PointerButton::Forward,
        other => PointerButton::Other(other),
    }
}

/// One frame of axis events as a scroll: wheel 120ths for wheels, else smooth pixels.
fn scroll_delta(h: &AxisScroll, v: &AxisScroll, source: Option<AxisSource>) -> Option<ScrollDelta> {
    let steps = |a: &AxisScroll| {
        if a.value120 != 0 {
            a.value120
        } else {
            a.discrete * 120
        }
    };
    let (x, y) = (steps(h), steps(v));
    let wheel_source = matches!(source, Some(AxisSource::Wheel | AxisSource::WheelTilt));
    if x != 0 || y != 0 {
        Some(ScrollDelta::Wheel { x, y })
    } else if (h.absolute != 0.0 || v.absolute != 0.0) && !wheel_source {
        Some(ScrollDelta::Pixels {
            x: h.absolute,
            y: v.absolute,
        })
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Wayland glue

/// Surface rectangles in window coordinates, for converting between surfaces.
struct HostRects<'a> {
    host: &'a Host,
}

impl Rects<SurfaceOwner> for HostRects<'_> {
    fn rect(&self, surface: SurfaceOwner) -> Option<WindowRect> {
        match surface {
            SurfaceOwner::Root(window) => {
                let size = self.host.windows.get(&window)?.size;
                Some(WindowRect {
                    x: 0,
                    y: 0,
                    width: size.width,
                    height: size.height,
                })
            }
            SurfaceOwner::Target(_, target) => {
                Some(self.host.targets.get(&target)?.borrow().metrics.rect())
            }
        }
    }
}

impl Host {
    /// Sets the target that gets `window`'s keyboard input: the page, the chrome, an overlay,
    /// or `None`. Keyboard focus belongs to the window; this is the logical focus inside it.
    /// Stops key repeat, so a held key doesn't start repeating into the new target.
    pub fn set_keyboard_focus(&mut self, window: WindowId, target: Option<TargetId>) {
        let Some(w) = self.windows.get_mut(&window) else {
            return;
        };
        if w.focus == target {
            return;
        }
        w.focus = target;
        for seat in &mut self.seats {
            if let Some(keyboard) = seat.keyboard.as_mut().filter(|k| k.window == Some(window)) {
                keyboard.repeat.cancel();
                if let Some(token) = keyboard.repeat_timer.take() {
                    self.loop_ops.remove(token);
                }
            }
        }
    }

    /// Sets the pointer's appearance over `target`, as an engine's `CursorChanged` or the
    /// chrome's hover asks. Applies at once where the pointer is over the target.
    pub fn set_cursor(&mut self, target: TargetId, cursor: Cursor) {
        let Some(t) = self.targets.get(&target) else {
            return;
        };
        t.borrow_mut().cursor = cursor;
        for index in 0..self.seats.len() {
            self.apply_cursor(index);
        }
    }

    /// Sets seat `index`'s cursor to that of the surface that gets its pointer input.
    fn apply_cursor(&mut self, index: usize) {
        let Some(pointer) = self.seats[index].pointer.as_ref() else {
            return;
        };
        let cursor = match pointer.router.target() {
            Some(SurfaceOwner::Target(_, target)) => self
                .targets
                .get(&target)
                .map(|t| t.borrow().cursor)
                .unwrap_or_default(),
            _ => Cursor::Default,
        };
        if pointer.cursor == Some(cursor) {
            return;
        }
        let result = match cursor {
            Cursor::None => pointer.themed.hide_cursor(),
            _ => {
                let icon = cursor.css_name().parse().unwrap_or(CursorIcon::Default);
                pointer.themed.set_cursor(&self.conn, icon)
            }
        };
        if result.is_ok() {
            if let Some(pointer) = self.seats[index].pointer.as_mut() {
                pointer.cursor = Some(cursor);
            }
        }
    }

    fn seat_index(&self, wl: &WlSeat) -> Option<usize> {
        self.seats.iter().position(|seat| &seat.wl == wl)
    }

    /// Delivers routed pointer events of seat `index`.
    fn deliver_pointer(
        &mut self,
        index: usize,
        routed: Vec<Routed<SurfaceOwner>>,
        scroll: Option<ScrollDelta>,
    ) {
        let (seat, modifiers) = (self.seats[index].id, self.seats[index].modifiers);
        for routed in routed {
            let (surface, event) = match routed {
                Routed::Enter(s, position) => (s, PointerEvent::Enter { position }),
                Routed::Leave(s) => (s, PointerEvent::Leave),
                Routed::Motion(s, position) => (
                    s,
                    PointerEvent::Motion {
                        position,
                        modifiers,
                    },
                ),
                Routed::Button(s, button, state, position) => (
                    s,
                    PointerEvent::Button {
                        button: pointer_button(button),
                        state,
                        position,
                        modifiers,
                    },
                ),
                Routed::Scroll(s, position) => match scroll {
                    Some(delta) => (
                        s,
                        PointerEvent::Scroll {
                            delta,
                            position,
                            modifiers,
                        },
                    ),
                    None => continue,
                },
            };
            self.events.push_back(HostEvent::Input {
                window: surface.window(),
                target: surface.target(),
                event: InputEvent {
                    seat,
                    kind: InputKind::Pointer(event),
                },
            });
        }
    }

    /// Routes one key event of seat `index` to its window's focus target.
    fn deliver_key(&mut self, index: usize, event: KeyEvent) {
        let seat = &self.seats[index];
        let Some(window) = seat.keyboard.as_ref().and_then(|k| k.window) else {
            return;
        };
        let target = self.windows.get(&window).and_then(|w| w.focus);
        self.events.push_back(HostEvent::Input {
            window,
            target,
            event: InputEvent {
                seat: seat.id,
                kind: InputKind::Key(event),
            },
        });
    }

    fn deliver_focus(&mut self, index: usize, window: WindowId, focus: FocusEvent) {
        let target = self.windows.get(&window).and_then(|w| w.focus);
        self.events.push_back(HostEvent::Input {
            window,
            target,
            event: InputEvent {
                seat: self.seats[index].id,
                kind: InputKind::Focus(focus),
            },
        });
    }

    /// Stops seat `index`'s key repeat.
    fn stop_repeat(&mut self, index: usize) {
        if let Some(keyboard) = self.seats[index].keyboard.as_mut() {
            keyboard.repeat.cancel();
            if let Some(token) = keyboard.repeat_timer.take() {
                self.loop_ops.remove(token);
            }
        }
    }

    /// The repeat timer of `seat` fired at `now`. Returns when it fires next.
    pub(crate) fn on_key_repeat(&mut self, seat: SeatId, now: Instant) -> Option<Instant> {
        let index = self.seats.iter().position(|s| s.id == seat)?;
        let keyboard = self.seats[index].keyboard.as_mut()?;
        let Some((code, next)) = keyboard.repeat.fire(now) else {
            keyboard.repeat_timer = None;
            return None;
        };
        let (event, _) = keyboard.xkb.as_mut()?.key(code, ButtonState::Pressed, true);
        self.deliver_key(index, event);
        Some(next)
    }

    /// A seat lost its pointer: end any drag so no button stays pressed.
    fn drop_pointer(&mut self, index: usize) {
        if let Some(mut pointer) = self.seats[index].pointer.take() {
            let mut routed = Vec::new();
            pointer.router.drop_device(&mut routed);
            self.deliver_pointer(index, routed, None);
            // ThemedPointer's drop releases the wl_pointer and destroys the cursor surface.
        }
    }

    /// A seat lost its keyboard, or the keyboard left the window.
    fn keyboard_left(&mut self, index: usize) {
        self.stop_repeat(index);
        // The compositor sends no modifiers to a window without keyboard focus.
        self.seats[index].modifiers = Modifiers::default();
        let Some(keyboard) = self.seats[index].keyboard.as_mut() else {
            return;
        };
        if let Some(xkb) = keyboard.xkb.as_mut() {
            xkb.reset_compose();
        }
        if let Some(window) = keyboard.window.take() {
            self.deliver_focus(index, window, FocusEvent::Deactivated);
        }
    }

    /// Nothing may reach `surface` any more (its target was retired).
    pub(crate) fn forget_surface(&mut self, surface: SurfaceOwner) {
        for seat in &mut self.seats {
            if let Some(pointer) = seat.pointer.as_mut() {
                pointer.router.forget(surface);
            }
        }
    }
}

impl SeatHandler for Host {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, wl: WlSeat) {
        let mut seat = Seat::new(wl);
        self.attach_seat_services(&mut seat, qh);
        self.seats.push(seat);
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        wl: WlSeat,
        capability: Capability,
    ) {
        let Some(index) = self.seat_index(&wl) else {
            return;
        };
        match capability {
            Capability::Keyboard if self.seats[index].keyboard.is_none() => {
                let id = self.seats[index].id;
                self.seats[index].keyboard = Some(Keyboard {
                    wl: wl.get_keyboard(qh, id),
                    xkb: None,
                    window: None,
                    repeat: Repeat::default(),
                    repeat_timer: None,
                });
            }
            Capability::Pointer if self.seats[index].pointer.is_none() => {
                let surface = self.compositor.create_surface(qh);
                let themed = self.seat_state.get_pointer_with_theme(
                    qh,
                    &wl,
                    self.shm.wl_shm(),
                    surface,
                    ThemeSpec::System,
                );
                match themed {
                    Ok(themed) => {
                        self.seats[index].pointer = Some(Pointer {
                            themed,
                            router: PointerRouter::default(),
                            cursor: None,
                        })
                    }
                    Err(error) => eprintln!("sighurt-wayland: no pointer: {error}"),
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        wl: WlSeat,
        capability: Capability,
    ) {
        let Some(index) = self.seat_index(&wl) else {
            return;
        };
        match capability {
            Capability::Keyboard => {
                self.keyboard_left(index);
                if let Some(keyboard) = self.seats[index].keyboard.take() {
                    if keyboard.wl.version() >= 3 {
                        keyboard.wl.release();
                    }
                }
            }
            Capability::Pointer => self.drop_pointer(index),
            _ => {}
        }
    }

    fn remove_seat(&mut self, conn: &Connection, qh: &QueueHandle<Self>, wl: WlSeat) {
        for capability in [Capability::Keyboard, Capability::Pointer] {
            self.remove_capability(conn, qh, wl.clone(), capability);
        }
        if let Some(index) = self.seat_index(&wl) {
            let seat = self.seats.remove(index);
            if self.active_seat == Some(seat.id) {
                self.active_seat = None;
            }
        }
    }
}

impl PointerHandler for Host {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        wl: &WlPointer,
        events: &[SctkPointerEvent],
    ) {
        let Some(index) = self
            .seats
            .iter()
            .position(|s| s.pointer.as_ref().is_some_and(|p| p.themed.pointer() == wl))
        else {
            return;
        };
        let Some(mut pointer) = self.seats[index].pointer.take() else {
            return;
        };
        let mut routed = Vec::new();
        let mut scroll = None;
        let mut entered = false;
        {
            let rects = HostRects { host: self };
            for event in events {
                let Some(&surface) = self.surfaces.get(&event.surface.id()) else {
                    continue; // A cursor surface, or one already retired.
                };
                let point = SurfacePoint {
                    x: event.position.0,
                    y: event.position.1,
                };
                let router = &mut pointer.router;
                match &event.kind {
                    PointerEventKind::Enter { .. } => {
                        entered = true;
                        router.enter(surface, point, &rects, &mut routed);
                    }
                    PointerEventKind::Leave { .. } => router.leave(surface, &mut routed),
                    PointerEventKind::Motion { .. } => router.motion(point, &rects, &mut routed),
                    PointerEventKind::Press { button, .. } => {
                        router.button(*button, ButtonState::Pressed, &rects, &mut routed)
                    }
                    PointerEventKind::Release { button, .. } => {
                        router.button(*button, ButtonState::Released, &rects, &mut routed)
                    }
                    PointerEventKind::Axis {
                        horizontal,
                        vertical,
                        source,
                        ..
                    } => {
                        scroll = scroll_delta(horizontal, vertical, *source);
                        if scroll.is_some() {
                            router.scroll(&mut routed, &rects);
                        }
                    }
                }
            }
            pointer.router.end_frame(&mut routed);
        }
        let serial = events.iter().rev().find_map(|e| match e.kind {
            PointerEventKind::Press { serial, .. } | PointerEventKind::Release { serial, .. } => {
                Some(serial)
            }
            _ => None,
        });
        if entered {
            pointer.cursor = None; // An enter needs the cursor set again, with its serial.
        }
        let seat = &mut self.seats[index];
        seat.pointer = Some(pointer);
        if let Some(serial) = serial {
            seat.serial = Some(serial);
            self.active_seat = Some(seat.id);
        }
        self.deliver_pointer(index, routed, scroll);
        self.apply_cursor(index);
    }
}

impl Dispatch<WlKeyboard, SeatId> for Host {
    fn event(
        host: &mut Host,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        seat_id: &SeatId,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        let Some(index) = host.seats.iter().position(|s| s.id == *seat_id) else {
            return;
        };
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if format != WEnum::Value(wl_keyboard::KeymapFormat::XkbV1) {
                    return;
                }
                let xkb = Xkb::from_fd(fd, size);
                if xkb.is_none() {
                    eprintln!("sighurt-wayland: the compositor's keymap could not be compiled");
                }
                host.stop_repeat(index);
                if let Some(keyboard) = host.seats[index].keyboard.as_mut() {
                    keyboard.xkb = xkb;
                }
            }
            wl_keyboard::Event::Enter {
                serial, surface, ..
            } => {
                let Some(window) = host.surfaces.get(&surface.id()).map(|s| s.window()) else {
                    return;
                };
                let seat = &mut host.seats[index];
                seat.serial = Some(serial);
                host.active_seat = Some(seat.id);
                if let Some(keyboard) = seat.keyboard.as_mut() {
                    keyboard.window = Some(window);
                }
                host.deliver_focus(index, window, FocusEvent::Activated);
            }
            wl_keyboard::Event::Leave { .. } => host.keyboard_left(index),
            wl_keyboard::Event::Key {
                serial, key, state, ..
            } => {
                let (state, repeat) = match state {
                    WEnum::Value(wl_keyboard::KeyState::Pressed) => (ButtonState::Pressed, false),
                    WEnum::Value(wl_keyboard::KeyState::Released) => (ButtonState::Released, false),
                    // wl_keyboard 10: the compositor repeats keys itself.
                    WEnum::Value(wl_keyboard::KeyState::Repeated) => (ButtonState::Pressed, true),
                    _ => return,
                };
                let seat = &mut host.seats[index];
                seat.serial = Some(serial);
                host.active_seat = Some(seat.id);
                let Some(keyboard) = seat.keyboard.as_mut() else {
                    return;
                };
                if repeat {
                    // The compositor repeats keys itself: no client-side repeat on top.
                    keyboard.repeat.set_info(0, 0);
                }
                let Some(xkb) = keyboard.xkb.as_mut() else {
                    return;
                };
                let (event, repeatable) = xkb.key(key, state, repeat);
                let mut timer = None;
                match state {
                    ButtonState::Pressed if repeatable && !repeat => {
                        timer = keyboard.repeat.press(key, Instant::now());
                    }
                    ButtonState::Released if keyboard.repeat.release(key) => {
                        if let Some(token) = keyboard.repeat_timer.take() {
                            host.loop_ops.remove(token);
                        }
                    }
                    _ => {}
                }
                if let Some(at) = timer {
                    if let Some(old) = keyboard.repeat_timer.take() {
                        host.loop_ops.remove(old);
                    }
                    let token = host.loop_ops.timer(at, TimerEvent::KeyRepeat(*seat_id));
                    if let Some(keyboard) = host.seats[index].keyboard.as_mut() {
                        keyboard.repeat_timer = token;
                    }
                }
                host.deliver_key(index, event);
            }
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                let seat = &mut host.seats[index];
                if let Some(xkb) = seat.keyboard.as_mut().and_then(|k| k.xkb.as_mut()) {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, group);
                    seat.modifiers = xkb.modifiers();
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                if let Some(keyboard) = host.seats[index].keyboard.as_mut() {
                    keyboard.repeat.set_info(rate, delay);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Keys by evdev code on a Swedish (se) layout.
    const KEY_2: u32 = 3;
    const KEY_A: u32 = 30;
    const KEY_ARING: u32 = 26; // å, where US layouts have [
    const KEY_DEAD_DIAERESIS: u32 = 27; // ¨, where US layouts have ]
    const KEY_ODIAERESIS: u32 = 39; // ö
    const KEY_ADIAERESIS: u32 = 40; // ä
    const KEY_ENTER: u32 = 28;
    const LEFT_SHIFT: u32 = 42;
    const LEFT_CTRL: u32 = 29;
    const ALTGR: u32 = 100;

    fn swedish() -> Xkb {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(&context, "evdev", "pc105", "se", "", None, 0)
            .expect("the Swedish layout from xkeyboard-config");
        let compose = xkb::compose::Table::new_from_locale(
            &context,
            "en_US.UTF-8".as_ref(),
            xkb::compose::COMPILE_NO_FLAGS,
        )
        .expect("the en_US.UTF-8 compose table");
        Xkb::new(keymap, Some(&compose))
    }

    fn press(xkb: &mut Xkb, code: u32) -> KeyEvent {
        xkb.key(code, ButtonState::Pressed, false).0
    }

    #[test]
    fn swedish_letters_type_themselves() {
        let mut xkb = swedish();
        for (code, letter, w3c) in [
            (KEY_ARING, "å", "BracketLeft"),
            (KEY_ADIAERESIS, "ä", "Quote"),
            (KEY_ODIAERESIS, "ö", "Semicolon"),
        ] {
            let event = press(&mut xkb, code);
            assert_eq!(event.key, Key::Character(letter.into()));
            assert_eq!(event.text.as_deref(), Some(letter));
            assert_eq!(event.code.w3c_code(), Some(w3c));
        }
        xkb.press_modifier(LEFT_SHIFT, true);
        let event = press(&mut xkb, KEY_ARING);
        assert_eq!(event.text.as_deref(), Some("Å"));
        assert!(event.modifiers.shift && event.consumed.shift);
    }

    #[test]
    fn altgr_is_its_own_modifier() {
        let mut xkb = swedish();
        xkb.press_modifier(ALTGR, true);
        let event = press(&mut xkb, KEY_2);
        assert_eq!(event.key, Key::Character("@".into()));
        assert_eq!(event.text.as_deref(), Some("@"));
        assert!(event.modifiers.alt_graph && event.consumed.alt_graph);
        assert!(
            !event.modifiers.control && !event.modifiers.alt,
            "not Ctrl+Alt"
        );
        xkb.press_modifier(ALTGR, false);
        let altgr = xkb.key(ALTGR, ButtonState::Pressed, false).0;
        assert_eq!(altgr.key, Key::Named("AltGraph".into()));
    }

    #[test]
    fn a_dead_key_composes_with_the_next() {
        let mut xkb = swedish();
        let (dead, repeats) = xkb.key(KEY_DEAD_DIAERESIS, ButtonState::Pressed, false);
        assert_eq!(dead.key, Key::Dead(Some('¨')));
        assert_eq!(dead.text, None);
        assert!(!repeats);
        let release = xkb.key(KEY_DEAD_DIAERESIS, ButtonState::Released, false).0;
        assert_eq!(release.text, None);
        let (composed, repeats) = xkb.key(KEY_A, ButtonState::Pressed, false);
        assert_eq!(composed.key, Key::Character("ä".into()));
        assert_eq!(composed.text.as_deref(), Some("ä"));
        assert_eq!(composed.code.w3c_code(), Some("KeyA"));
        assert!(!repeats, "a composed character doesn't auto-repeat");
        // The composition is over: the next a is an a.
        assert_eq!(press(&mut xkb, KEY_A).text.as_deref(), Some("a"));
    }

    #[test]
    fn a_broken_sequence_types_nothing() {
        let mut xkb = swedish();
        press(&mut xkb, KEY_DEAD_DIAERESIS);
        let broken = press(&mut xkb, KEY_2);
        assert_eq!(broken.text, None);
        assert_eq!(press(&mut xkb, KEY_2).text.as_deref(), Some("2"));
    }

    #[test]
    fn control_makes_a_shortcut_not_text() {
        let mut xkb = swedish();
        xkb.press_modifier(LEFT_CTRL, true);
        let event = press(&mut xkb, KEY_A);
        assert_eq!(
            event.key,
            Key::Character("a".into()),
            "no Control transformation"
        );
        assert_eq!(event.text, None);
        assert!(event.modifiers.control && !event.consumed.control);
    }

    #[test]
    fn named_keys_have_no_text() {
        let mut xkb = swedish();
        let (enter, repeats) = xkb.key(KEY_ENTER, ButtonState::Pressed, false);
        assert_eq!(enter.key, Key::Named("Enter".into()));
        assert_eq!(enter.text, None);
        assert!(repeats);
        let shift = press(&mut xkb, LEFT_SHIFT);
        assert_eq!(shift.key, Key::Named("Shift".into()));
        assert_eq!(shift.text, None);
    }

    #[test]
    fn repeats_are_marked_and_bypass_compose() {
        let mut xkb = swedish();
        let (event, _) = xkb.key(KEY_A, ButtonState::Pressed, true);
        assert!(event.repeat);
        assert_eq!(event.text.as_deref(), Some("a"));
    }

    #[test]
    fn repeat_follows_delay_and_rate() {
        let start = Instant::now();
        let mut repeat = Repeat::default();
        repeat.set_info(25, 600);
        let first = repeat.press(KEY_A, start).unwrap();
        assert_eq!(first - start, Duration::from_millis(600));
        let (key, second) = repeat.fire(first).unwrap();
        assert_eq!(key, KEY_A);
        assert_eq!(second - first, Duration::from_millis(40));
        // A timer that fires late keeps the cadence it can and never bursts.
        let late = second + Duration::from_millis(500);
        let (_, third) = repeat.fire(late).unwrap();
        assert_eq!(third - late, Duration::from_millis(40));
    }

    #[test]
    fn repeat_stops_on_release_and_can_be_disabled() {
        let now = Instant::now();
        let mut repeat = Repeat::default();
        repeat.press(KEY_A, now);
        assert!(
            !repeat.release(KEY_2),
            "another key's release doesn't stop it"
        );
        assert!(repeat.release(KEY_A));
        assert_eq!(repeat.fire(now + Duration::from_secs(1)), None);
        repeat.set_info(0, 600);
        assert_eq!(repeat.press(KEY_A, now), None);
        // A new key replaces the repeating one.
        repeat.set_info(30, 200);
        repeat.press(KEY_A, now);
        repeat.press(KEY_2, now);
        assert_eq!(repeat.fire(now + Duration::from_secs(1)).unwrap().0, KEY_2);
    }

    // Two surfaces of a window: chrome at y 0–40, page below it.
    #[derive(Clone, Copy, Debug, PartialEq)]
    enum S {
        Chrome,
        Page,
    }

    struct Layout;

    impl Rects<S> for Layout {
        fn rect(&self, surface: S) -> Option<WindowRect> {
            Some(match surface {
                S::Chrome => WindowRect {
                    x: 0,
                    y: 0,
                    width: 800,
                    height: 40,
                },
                S::Page => WindowRect {
                    x: 0,
                    y: 40,
                    width: 800,
                    height: 560,
                },
            })
        }
    }

    fn at(x: f64, y: f64) -> SurfacePoint {
        SurfacePoint { x, y }
    }

    const LEFT: u32 = 0x110;

    #[test]
    fn a_drag_keeps_its_recipient_across_surfaces() {
        let mut router = PointerRouter::default();
        let mut out = Vec::new();
        router.enter(S::Page, at(100.0, 10.0), &Layout, &mut out);
        router.button(LEFT, ButtonState::Pressed, &Layout, &mut out);
        // The compositor moves focus to the chrome while the button is held.
        router.leave(S::Page, &mut out);
        router.enter(S::Chrome, at(100.0, 30.0), &Layout, &mut out);
        router.motion(at(120.0, 20.0), &Layout, &mut out);
        router.end_frame(&mut out);
        router.button(LEFT, ButtonState::Released, &Layout, &mut out);
        assert_eq!(
            out,
            vec![
                Routed::Enter(S::Page, at(100.0, 10.0)),
                Routed::Button(S::Page, LEFT, ButtonState::Pressed, at(100.0, 10.0)),
                // Chrome (100, 30) is page (100, -10): the page's own coordinates.
                Routed::Motion(S::Page, at(100.0, -10.0)),
                Routed::Motion(S::Page, at(120.0, -20.0)),
                Routed::Button(S::Page, LEFT, ButtonState::Released, at(120.0, -20.0)),
                Routed::Leave(S::Page),
                Routed::Enter(S::Chrome, at(120.0, 20.0)),
            ]
        );
    }

    #[test]
    fn losing_the_pointer_mid_drag_releases_the_buttons() {
        let mut router = PointerRouter::default();
        let mut out = Vec::new();
        router.enter(S::Chrome, at(5.0, 5.0), &Layout, &mut out);
        router.button(LEFT, ButtonState::Pressed, &Layout, &mut out);
        out.clear();
        router.leave(S::Chrome, &mut out);
        router.end_frame(&mut out);
        assert_eq!(
            out,
            vec![
                Routed::Button(S::Chrome, LEFT, ButtonState::Released, at(5.0, 5.0)),
                Routed::Leave(S::Chrome),
            ]
        );
        assert_eq!(router.target(), None);
    }

    #[test]
    fn a_pointer_that_goes_away_leaves_each_surface_once() {
        // Mid-drag over the surface that got the press.
        let mut router = PointerRouter::default();
        let mut out = Vec::new();
        router.enter(S::Page, at(5.0, 5.0), &Layout, &mut out);
        router.button(LEFT, ButtonState::Pressed, &Layout, &mut out);
        out.clear();
        router.drop_device(&mut out);
        assert_eq!(
            out,
            vec![
                Routed::Button(S::Page, LEFT, ButtonState::Released, at(5.0, 5.0)),
                Routed::Leave(S::Page),
            ]
        );
        // Mid-drag over another surface, which never got an enter.
        let mut router = PointerRouter::default();
        router.enter(S::Page, at(5.0, 5.0), &Layout, &mut out);
        router.button(LEFT, ButtonState::Pressed, &Layout, &mut out);
        router.leave(S::Page, &mut out);
        router.enter(S::Chrome, at(5.0, 30.0), &Layout, &mut out);
        out.clear();
        router.drop_device(&mut out);
        assert_eq!(
            out,
            vec![
                Routed::Button(S::Page, LEFT, ButtonState::Released, at(5.0, -10.0)),
                Routed::Leave(S::Page),
            ]
        );
        // No buttons: the surface under the pointer.
        let mut router = PointerRouter::default();
        router.enter(S::Chrome, at(5.0, 5.0), &Layout, &mut out);
        out.clear();
        router.drop_device(&mut out);
        assert_eq!(out, vec![Routed::Leave(S::Chrome)]);
        assert_eq!(router.target(), None);
    }

    #[test]
    fn without_buttons_input_follows_the_pointer() {
        let mut router = PointerRouter::default();
        let mut out = Vec::new();
        router.enter(S::Chrome, at(5.0, 39.0), &Layout, &mut out);
        router.leave(S::Chrome, &mut out);
        router.enter(S::Page, at(5.0, 0.0), &Layout, &mut out);
        router.scroll(&mut out, &Layout);
        router.end_frame(&mut out);
        assert_eq!(
            out,
            vec![
                Routed::Enter(S::Chrome, at(5.0, 39.0)),
                Routed::Leave(S::Chrome),
                Routed::Enter(S::Page, at(5.0, 0.0)),
                Routed::Scroll(S::Page, at(5.0, 0.0)),
            ]
        );
    }

    #[test]
    fn wheels_and_touchpads_scroll_in_their_own_units() {
        let wheel = AxisScroll {
            absolute: 15.0,
            value120: 120,
            ..AxisScroll::default()
        };
        assert_eq!(
            scroll_delta(&AxisScroll::default(), &wheel, Some(AxisSource::Wheel)),
            Some(ScrollDelta::Wheel { x: 0, y: 120 })
        );
        let old_wheel = AxisScroll {
            absolute: 10.0,
            discrete: -1,
            ..AxisScroll::default()
        };
        assert_eq!(
            scroll_delta(&old_wheel, &AxisScroll::default(), None),
            Some(ScrollDelta::Wheel { x: -120, y: 0 })
        );
        let finger = AxisScroll {
            absolute: 3.5,
            ..AxisScroll::default()
        };
        assert_eq!(
            scroll_delta(&AxisScroll::default(), &finger, Some(AxisSource::Finger)),
            Some(ScrollDelta::Pixels { x: 0.0, y: 3.5 })
        );
        let stop = AxisScroll {
            stop: true,
            ..AxisScroll::default()
        };
        assert_eq!(scroll_delta(&stop, &stop, Some(AxisSource::Finger)), None);
    }
}
