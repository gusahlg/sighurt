//! Input methods through `zwp_text_input_v3`.
//!
//! The application tells the host which target's editor wants an input method, and its state
//! ([`Host::set_text_input`]): the chrome's address bar, or the focused page's field from an
//! engine's `TextInputChanged`. While the seat's text-input focus is on the window, the host
//! enables text input with that state: the surrounding text (never for a password), the content
//! type (a password always with the `sensitive_data` and `hidden_text` hints, a multi-line field
//! with `multiline`), and the caret rectangle, translated from the reporting target's
//! coordinates into those of the surface that has text-input focus (the root, which the
//! compositor gives keyboard focus). `None` disables it.
//!
//! Each field is its own input-method session: `enable` is sent again for a new field, which
//! resets what the input method knows (surrounding text, preedit), as the protocol requires;
//! later states of the same field only update it. The api's state has no field identity, so a
//! new field is recognized by its target, its kind (purpose, multi-line) or the surrounding
//! text it lacks. Two fields of one target that look alike are one session unless `None`
//! comes between them (an engine's `TextInputChanged` with no state, which must then not be
//! coalesced away); a field identity in `TextInputState` would end that limit.
//!
//! The input method's preedit, commit and delete-surrounding requests become one
//! [`TextInputEvent`] per `done`, for the target that enabled it. Text an input method commits
//! arrives this way only, never also as key text: while an input method is active, the
//! compositor sends it the keys instead of the window.
//!
//! When the compositor has no text-input protocol, keyboard and compose input still work, and
//! [`Capabilities::text_input`](crate::Capabilities::text_input) says input methods are
//! unavailable.
//!
//! Must not own: the editors themselves (chrome and engines own them).

use sighurt_api::geometry::{SurfaceRect, WindowRect};
use sighurt_api::ids::{SeatId, TargetId, WindowId};
use sighurt_api::input::{
    DeleteSurrounding, InputEvent, InputKind, Preedit, SurroundingText, TextInputEvent,
    TextInputState, TextPurpose,
};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use crate::connection::{Host, HostEvent, SurfaceOwner};

/// The most surrounding text the protocol allows in one request, in bytes.
const MAX_SURROUNDING: usize = 4000;

/// One seat's text input.
pub(crate) struct TextInput {
    wl: ZwpTextInputV3,
    /// The surface with this seat's text-input focus.
    focus: Option<WlSurface>,
    /// The editor it is enabled for.
    enabled: Option<(WindowId, TargetId)>,
    /// The state last sent, with the caret as sent, so an unchanged state is not sent again.
    sent: Option<(TargetId, TextInputState, SurfaceRect)>,
    pending: TextInputEvent,
    /// Whether a preedit is showing, to clear it when focus leaves.
    preedit_shown: bool,
}

impl TextInput {
    pub fn new(
        manager: &ZwpTextInputManagerV3,
        seat: &wayland_client::protocol::wl_seat::WlSeat,
        id: SeatId,
        qh: &QueueHandle<Host>,
    ) -> Self {
        Self {
            wl: manager.get_text_input(seat, qh, id),
            focus: None,
            enabled: None,
            sent: None,
            pending: TextInputEvent::default(),
            preedit_shown: false,
        }
    }
}

impl TextInput {
    /// `window` closed: its editor no longer has the input method.
    pub fn forget_window(&mut self, window: WindowId) {
        if self.enabled.is_some_and(|(w, _)| w == window) {
            self.wl.disable();
            self.wl.commit();
            self.enabled = None;
            self.sent = None;
        }
    }
}

impl Drop for TextInput {
    fn drop(&mut self) {
        self.wl.destroy();
    }
}

/// The content type the protocol takes for `state`.
fn content_type(state: &TextInputState) -> (ContentHint, ContentPurpose) {
    let purpose = match state.purpose {
        TextPurpose::Normal => ContentPurpose::Normal,
        TextPurpose::Password => ContentPurpose::Password,
        TextPurpose::Email => ContentPurpose::Email,
        TextPurpose::Url => ContentPurpose::Url,
        TextPurpose::Number => ContentPurpose::Number,
        TextPurpose::Phone => ContentPurpose::Phone,
        TextPurpose::Date => ContentPurpose::Date,
        TextPurpose::Time => ContentPurpose::Time,
        TextPurpose::DateTime => ContentPurpose::Datetime,
    };
    let mut hint = ContentHint::None;
    if state.purpose == TextPurpose::Password {
        hint |= ContentHint::SensitiveData | ContentHint::HiddenText;
    }
    if state.multiline {
        hint |= ContentHint::Multiline;
    }
    (hint, purpose)
}

/// `surrounding` cut to what the protocol takes: at most [`MAX_SURROUNDING`] bytes around the
/// cursor, on character boundaries, with the cursor and anchor made relative to the cut. The
/// anchor is clamped into the cut when the selection is longer.
fn clip_surrounding(surrounding: &SurroundingText) -> (String, usize, usize) {
    let text = &surrounding.text;
    let cursor = surrounding.cursor.min(text.len());
    let anchor = surrounding.anchor.min(text.len());
    if text.len() <= MAX_SURROUNDING {
        return (text.clone(), cursor, anchor);
    }
    let floor = |mut i: usize| {
        while !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let start = floor(cursor.saturating_sub(MAX_SURROUNDING / 2));
    let end = floor((start + MAX_SURROUNDING).min(text.len()));
    let clamp = |i: usize| i.clamp(start, end) - start;
    (text[start..end].to_owned(), clamp(cursor), clamp(anchor))
}

/// Whether `state`, from the target that sent `sent`, is another field: one of another kind, or
/// one without the surrounding text `sent` had, which only a new session clears.
fn is_new_field(sent: &TextInputState, state: &TextInputState) -> bool {
    sent.purpose != state.purpose
        || sent.multiline != state.multiline
        || (sent.surrounding.is_some() && state.surrounding.is_none())
}

/// `caret`, reported by a target placed at `target` (window coordinates), in the coordinates of
/// the surface placed at `focus` that has text-input focus: translated once, through the window.
/// The engine's numbers are not trusted: the result is clamped to what `set_cursor_rectangle`'s
/// `int`s can carry, never wrapped.
fn caret_on(caret: SurfaceRect, target: WindowRect, focus: WindowRect) -> SurfaceRect {
    let shift = |at: i32, from: i32, to: i32| {
        let moved = i64::from(at) + i64::from(from) - i64::from(to);
        moved.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    };
    let length = |l: u32| l.min(i32::MAX as u32);
    SurfaceRect {
        x: shift(caret.x, target.x, focus.x),
        y: shift(caret.y, target.y, focus.y),
        width: length(caret.width),
        height: length(caret.height),
    }
}

impl Host {
    /// Tells the host which editor of `window` wants an input method: `target`'s, with `state`,
    /// or none. Send it again whenever the caret, surrounding text or field changes. When focus
    /// moves between two fields of the same target with the same purpose and multi-line flag,
    /// the input method starts over with the new field only if `None` came in between (see the
    /// module documentation).
    pub fn set_text_input(
        &mut self,
        window: WindowId,
        target: TargetId,
        state: Option<TextInputState>,
    ) {
        let Some(w) = self.windows.get_mut(&window) else {
            return;
        };
        match state {
            Some(state) => w.text_input = Some((target, state)),
            // Only the editor that has it can turn it off.
            None if w.text_input.as_ref().is_some_and(|(t, _)| *t == target) => w.text_input = None,
            None => return,
        }
        for index in 0..self.seats.len() {
            self.sync_text_input(index);
        }
    }

    /// Brings seat `index`'s text input in line with its focus window's editor.
    pub(crate) fn sync_text_input(&mut self, index: usize) {
        let Some(ti) = self.seats[index].text_input.as_ref() else {
            return;
        };
        let owner = ti
            .focus
            .as_ref()
            .and_then(|s| self.surfaces.get(&s.id()).copied());
        let rect_of = |target: &TargetId| Some(self.targets.get(target)?.borrow().metrics.rect());
        let wanted = owner.and_then(|owner| {
            let window = self.windows.get(&owner.window())?;
            let (target, state) = window.text_input.clone()?;
            // The surface with text-input focus: the root (window coordinates) or a target.
            let focus_rect = match owner {
                SurfaceOwner::Root(_) => WindowRect::default(),
                SurfaceOwner::Target(_, focus) => rect_of(&focus)?,
            };
            Some((window.id, target, state, rect_of(&target)?, focus_rect))
        });
        let ti = self.seats[index]
            .text_input
            .as_mut()
            .expect("checked above");
        let Some((window, target, state, target_rect, focus_rect)) = wanted else {
            if ti.enabled.take().is_some() {
                ti.wl.disable();
                ti.wl.commit();
                ti.sent = None;
            }
            return;
        };
        let unchanged = ti.sent.as_ref().is_some_and(|(t, sent, caret)| {
            *t == target
                && *sent == state
                && *caret == caret_on(state.caret, target_rect, focus_rect)
        });
        if unchanged && ti.enabled == Some((window, target)) {
            return;
        }
        let new_field = ti.enabled != Some((window, target))
            || ti
                .sent
                .as_ref()
                .is_some_and(|(_, sent, _)| is_new_field(sent, &state));
        if new_field {
            ti.wl.enable();
            ti.enabled = Some((window, target));
        }
        match &state.surrounding {
            Some(surrounding) if state.purpose != TextPurpose::Password => {
                let (text, cursor, anchor) = clip_surrounding(surrounding);
                ti.wl
                    .set_surrounding_text(text, cursor as i32, anchor as i32);
            }
            _ => {}
        }
        let (hint, purpose) = content_type(&state);
        ti.wl.set_content_type(hint, purpose);
        let caret = caret_on(state.caret, target_rect, focus_rect);
        ti.wl
            .set_cursor_rectangle(caret.x, caret.y, caret.width as i32, caret.height as i32);
        ti.wl.commit();
        ti.sent = Some((target, state, caret));
    }

    fn deliver_text_input(&mut self, index: usize, event: TextInputEvent) {
        let seat = &self.seats[index];
        let Some((window, target)) = seat.text_input.as_ref().and_then(|t| t.enabled) else {
            return;
        };
        self.events.push_back(HostEvent::Input {
            window,
            target: Some(target),
            event: InputEvent {
                seat: seat.id,
                kind: InputKind::TextInput(event),
            },
        });
    }
}

impl Dispatch<ZwpTextInputV3, SeatId> for Host {
    fn event(
        host: &mut Host,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        seat: &SeatId,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        let Some(index) = host.seats.iter().position(|s| s.id == *seat) else {
            return;
        };
        let Some(ti) = host.seats[index].text_input.as_mut() else {
            return;
        };
        match event {
            zwp_text_input_v3::Event::Enter { surface } => {
                ti.focus = Some(surface);
                ti.enabled = None;
                ti.sent = None;
                host.sync_text_input(index);
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                // The compositor ignores requests until the next enter; the editor must drop
                // its preedit.
                ti.focus = None;
                let had_preedit = std::mem::take(&mut ti.preedit_shown);
                if had_preedit {
                    host.deliver_text_input(index, TextInputEvent::default());
                }
                if let Some(ti) = host.seats[index].text_input.as_mut() {
                    ti.enabled = None;
                    ti.sent = None;
                }
            }
            zwp_text_input_v3::Event::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                ti.pending.preedit = text.map(|text| {
                    let cursor = match (usize::try_from(cursor_begin), usize::try_from(cursor_end))
                    {
                        (Ok(begin), Ok(end)) if begin <= end && end <= text.len() => {
                            Some(begin..end)
                        }
                        _ => None,
                    };
                    Preedit { text, cursor }
                });
            }
            zwp_text_input_v3::Event::CommitString { text } => ti.pending.commit = text,
            zwp_text_input_v3::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                ti.pending.delete_surrounding = Some(DeleteSurrounding {
                    before: before_length,
                    after: after_length,
                });
            }
            zwp_text_input_v3::Event::Done { .. } => {
                let event = std::mem::take(&mut ti.pending);
                let showing = event.preedit.is_some();
                let changes = showing
                    || ti.preedit_shown
                    || event.commit.is_some()
                    || event.delete_surrounding.is_some();
                ti.preedit_shown = showing;
                if changes {
                    host.deliver_text_input(index, event);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for Host {
    fn event(
        _: &mut Host,
        _: &ZwpTextInputManagerV3,
        _: <ZwpTextInputManagerV3 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caret_is_translated_once_into_the_focused_surface() {
        // A caret at (10, 20) in the page, which sits below a 64 px chrome.
        let caret = SurfaceRect {
            x: 10,
            y: 20,
            width: 2,
            height: 18,
        };
        let page = WindowRect {
            x: 0,
            y: 64,
            width: 800,
            height: 536,
        };
        let root = WindowRect::default();
        assert_eq!(
            caret_on(caret, page, root),
            SurfaceRect {
                x: 10,
                y: 84,
                width: 2,
                height: 18
            }
        );
        // Were a compositor to give text-input focus to the page itself, nothing moves.
        assert_eq!(caret_on(caret, page, page), caret);
        let chrome = WindowRect {
            x: 0,
            y: 0,
            width: 800,
            height: 64,
        };
        assert_eq!(caret_on(caret, page, chrome).y, 84);
        // A page reporting nonsense is clamped, not wrapped into a caret elsewhere.
        let wild = SurfaceRect {
            x: i32::MAX,
            y: i32::MIN,
            width: u32::MAX,
            height: u32::MAX,
        };
        let below = WindowRect {
            x: 10,
            y: 64,
            ..page
        };
        let shifted = caret_on(wild, below, chrome);
        assert_eq!((shifted.x, shifted.y), (i32::MAX, i32::MIN + 64));
        assert_eq!(
            (shifted.width, shifted.height),
            (i32::MAX as u32, i32::MAX as u32)
        );
        let back = caret_on(wild, root, below);
        assert_eq!((back.x, back.y), (i32::MAX - 10, i32::MIN));
    }

    fn field(purpose: TextPurpose, multiline: bool) -> TextInputState {
        TextInputState {
            caret: SurfaceRect::default(),
            surrounding: None,
            purpose,
            multiline,
        }
    }

    #[test]
    fn passwords_are_hidden_from_the_input_method() {
        let (hint, purpose) = content_type(&field(TextPurpose::Password, false));
        assert_eq!(purpose, ContentPurpose::Password);
        assert!(hint.contains(ContentHint::SensitiveData | ContentHint::HiddenText));
        let (hint, purpose) = content_type(&field(TextPurpose::Url, false));
        assert_eq!(purpose, ContentPurpose::Url);
        assert_eq!(hint, ContentHint::None);
        let (hint, _) = content_type(&field(TextPurpose::Normal, true));
        assert_eq!(hint, ContentHint::Multiline);
    }

    #[test]
    fn another_kind_of_field_is_a_new_session() {
        let mut text = field(TextPurpose::Normal, false);
        text.surrounding = Some(SurroundingText {
            text: "hej".into(),
            cursor: 3,
            anchor: 3,
        });
        // Typing and moving the caret in the same field update it.
        let mut typed = text.clone();
        typed.surrounding = Some(SurroundingText {
            text: "hej då".into(),
            cursor: 7,
            anchor: 7,
        });
        typed.caret.x += 30;
        assert!(!is_new_field(&text, &typed));
        // A password field after it, another purpose, and a field that can't say what surrounds
        // the cursor are new sessions.
        assert!(is_new_field(&text, &field(TextPurpose::Password, false)));
        assert!(is_new_field(&text, &field(TextPurpose::Normal, true)));
        assert!(is_new_field(&text, &field(TextPurpose::Normal, false)));
        let password = field(TextPurpose::Password, false);
        assert!(!is_new_field(&password, &password.clone()));
    }

    #[test]
    fn short_surrounding_text_is_sent_whole() {
        let surrounding = SurroundingText {
            text: "räksmörgås".into(),
            cursor: 3,
            anchor: 1,
        };
        assert_eq!(clip_surrounding(&surrounding), ("räksmörgås".into(), 3, 1));
    }

    #[test]
    fn long_surrounding_text_is_cut_around_the_cursor_on_char_boundaries() {
        // 10 200 bytes of three-byte characters: byte 4000 is inside one.
        let text = "€".repeat(3400);
        let surrounding = SurroundingText {
            text,
            cursor: 6000,
            anchor: 21,
        };
        let (cut, cursor, anchor) = clip_surrounding(&surrounding);
        // The cut starts at byte 3999, the boundary before 4000, and ends at 7998.
        assert_eq!(cut.len(), 3999);
        assert!(cut.chars().all(|c| c == '€'));
        assert_eq!(cursor, 2001);
        assert_eq!(anchor, 0, "the far anchor is clamped into the cut");
    }
}
