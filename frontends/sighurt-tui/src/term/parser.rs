//! What the terminal sends: keys, mouse, paste, capability replies and OSC 52 clipboard replies.
//!
//! `feed` never sleeps. A lone Escape arms a 25 ms timer; any longer partial
//! sequence arms a 1 s timer. The owner loop sleeps for [`Parser::poll_timeout`]
//! and then calls [`Parser::expire`]. Calling `expire` means that timer has
//! fired. An unfinished CSI is not abandoned on the Escape timer, so a reply
//! that arrives in pieces is still a reply.
//!
//! Bracketed paste is collected until `CSI 201 ~` and is not parsed as keys.
//! Payloads are capped (64 KiB for APC/OSC/DCS, 1 MiB for paste). Bytes past
//! the paste cap are discarded until the end marker, and that paste is one event.
//! An OSC 52 reply whose data is `?`, empty or not base64 is a reply with no
//! data. That payload is capped with the other OSC payloads; once the cap is
//! hit the reply is a failure (`truncated`), never a decoded prefix.
//!
//! When an OSC, DCS or APC string times out, the bytes still to come are
//! discarded until its terminator (`ESC \` or, for OSC, BEL) and are not
//! parsed as keys. A clipboard read's own deadline is separate. A timed-out
//! OSC 52 is reported as a truncated reply when that terminator arrives.
//!
//! Releases: once the terminal's reply to `CSI ? u` says it reports event
//! types (flag 2), a key in an escape sequence (`CSI … u`, `CSI … ~`, the
//! `CSI` letter forms) is held from its press until the terminal's own
//! release, which every form may carry as `;mods:3` (kitty and Rio send
//! `CSI D` for Left, then `CSI 1;1:3 D`). A release with no press reported
//! before it is dropped: Rio sends Ctrl+L as the byte `0x0c` and its release
//! as `CSI 108;1:3 u`, with the modifier cleared. Bytes that are not escape sequences (text, C0, SS3,
//! and every key when the terminal does not report event types) are a press
//! and a synthesized release.

use std::time::{Duration, Instant};

use super::keys::{
    kitty_key, kitty_modifiers, legacy_press, named_evdev, release_of, ButtonState, Key, KeyCode,
    KeyKind, Modifiers,
};

/// How long a lone Escape waits before it is a key.
pub const ESC_TIMEOUT: Duration = Duration::from_millis(25);

/// How long an unfinished sequence is kept before it is dropped.
pub const SEQ_TIMEOUT: Duration = Duration::from_secs(1);

const MAX_PAYLOAD: usize = 64 * 1024;
const MAX_PASTE: usize = 1024 * 1024;
const PASTE_END: &[u8] = b"\x1b[201~";
const MAX_PARAMS: usize = 16;
const MAX_SUBS: usize = 8;
/// Most keys held at once; the oldest is forgotten past this.
const MAX_HELD: usize = 16;
/// Kitty's "report event types" flag.
const EVENT_TYPES: u32 = 2;

/// Which key an escape sequence names, to pair a release with its press.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyId {
    /// `CSI code … u`, by its (unshifted) code.
    Unicode(u32),
    /// `CSI number … ~`.
    Tilde(u32),
    /// `CSI … A` and the other letter forms, by the final byte.
    Final(u8),
}

/// One thing the terminal sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Input {
    /// A key press, repeat or release.
    Key(super::keys::KeyEvent),
    /// An SGR mouse report (1006).
    Mouse(MouseEvent),
    /// A bracketed paste, lossy-decoded as UTF-8.
    Paste(String),
    /// Focus in (`true`, `CSI I`) or focus out (`false`, `CSI O`).
    Focus(bool),
    /// A capability reply. These are never keys, including when they arrive
    /// after the probe deadline.
    Reply(Reply),
}

/// SGR button, or none for a wheel report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// Primary button.
    Left,
    /// Middle button.
    Middle,
    /// Secondary button.
    Right,
    /// The back side button (code 128).
    Back,
    /// The forward side button (code 129).
    Forward,
    /// A wheel report has no button.
    None,
}

/// Which way a wheel event moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wheel {
    /// Away from the user.
    Up,
    /// Toward the user.
    Down,
    /// Toward the left.
    Left,
    /// Toward the right.
    Right,
}

/// One SGR 1006 report. Coordinates are 1-based, as the terminal sent them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MouseEvent {
    /// Which button, or [`MouseButton::None`] for a wheel.
    pub button: MouseButton,
    /// Press or release. Wheel reports use the terminal's `M`/`m` byte.
    pub state: ButtonState,
    /// Column, 1-based.
    pub column: u16,
    /// Row, 1-based.
    pub row: u16,
    /// Shift, Alt (the meta bit) and Control from the button code.
    pub modifiers: Modifiers,
    /// The motion bit (32) was set.
    pub motion: bool,
    /// Set when the wheel bit (64) was set.
    pub wheel: Option<Wheel>,
}

/// A reply to a startup query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    /// Primary DA. `sixel` is set when any parameter is 4.
    Da1 { params: Vec<u32>, sixel: bool },
    /// `CSI ? flags u`.
    KittyKeyboard { flags: u32 },
    /// APC payload that starts with `G`. `ok` means the payload contains `;OK`.
    KittyGraphics { raw: String, ok: bool },
    /// XTGETTCAP (`DCS 1 + r` or `DCS 0 + r`).
    XtGetTcap {
        found: bool,
        entries: Vec<(String, Option<String>)>,
    },
    /// OSC 10 or 11. `bell` is true when BEL terminated it instead of ST.
    OscColor {
        index: u16,
        spec: String,
        bell: bool,
    },
    /// OSC 52, the clipboard. `data` is missing when the terminal sent `?`, nothing, or bytes
    /// that are not base64 — and when `truncated` is set, which is not a value.
    /// `selection` is `c` or `p` when the terminal named one of those.
    Clipboard {
        selection: Option<u8>,
        data: Option<Vec<u8>>,
        /// The buffer filled before the terminator, or a timeout discarded the rest.
        truncated: bool,
    },
    /// `CSI 4 ; height ; width t`, the text area in pixels.
    TextAreaPixels { height: u32, width: u32 },
    /// `CSI 6 ; height ; width t`, one cell in pixels.
    CellPixels { height: u32, width: u32 },
}

#[derive(Debug)]
struct Buf {
    data: Vec<u8>,
    truncated: bool,
}

impl Buf {
    fn new() -> Self {
        Self {
            data: Vec::new(),
            truncated: false,
        }
    }

    fn push(&mut self, byte: u8) {
        if self.data.len() >= MAX_PAYLOAD {
            self.truncated = true;
            return;
        }
        self.data.push(byte);
    }
}

#[derive(Debug)]
struct Csi {
    private: Option<u8>,
    params: Vec<Vec<Option<u32>>>,
    open: bool,
    saw_digit: bool,
    value: u32,
    sep: bool,
    intermediates: Vec<u8>,
    started: bool,
}

impl Csi {
    fn new() -> Self {
        Self {
            private: None,
            params: Vec::new(),
            open: false,
            saw_digit: false,
            value: 0,
            sep: false,
            intermediates: Vec::new(),
            started: false,
        }
    }

    fn push_digit(&mut self, digit: u8) {
        self.started = true;
        self.ensure_open();
        self.value = self
            .value
            .saturating_mul(10)
            .saturating_add(u32::from(digit));
        self.saw_digit = true;
        self.sep = false;
    }

    fn separate(&mut self, sub: bool) {
        self.started = true;
        self.finish_field();
        if !sub {
            self.open = false;
        }
        self.sep = true;
    }

    fn ensure_open(&mut self) {
        if !self.open {
            if self.params.len() >= MAX_PARAMS {
                return;
            }
            self.params.push(Vec::new());
            self.open = true;
        }
    }

    fn finish_field(&mut self) {
        self.ensure_open();
        let Some(param) = self.params.last_mut() else {
            return;
        };
        if param.len() >= MAX_SUBS {
            self.saw_digit = false;
            self.value = 0;
            return;
        }
        param.push(if self.saw_digit {
            Some(self.value)
        } else {
            None
        });
        self.saw_digit = false;
        self.value = 0;
    }

    fn finish(&mut self) {
        if self.saw_digit || self.open {
            self.finish_field();
        } else if self.sep && self.params.len() < MAX_PARAMS {
            self.params.push(vec![None]);
        }
    }

    fn number(&self, index: usize) -> Option<u32> {
        self.params.get(index)?.first().copied().flatten()
    }
}

#[derive(Debug)]
struct Dcs {
    csi: Csi,
    data: Buf,
    in_data: bool,
    final_byte: u8,
}

impl Dcs {
    fn new() -> Self {
        Self {
            csi: Csi::new(),
            data: Buf::new(),
            in_data: false,
            final_byte: 0,
        }
    }
}

/// Why bytes are being discarded, after a string timed out.
#[derive(Clone, Copy, Debug)]
enum DiscardKind {
    /// OSC (`bell`), DCS or APC. Not a clipboard reply.
    String { bell: bool },
    /// OSC 52. `selection` is `c` or `p` when the buffer named one.
    Clipboard { selection: Option<u8> },
}

impl DiscardKind {
    fn bell(self) -> bool {
        match self {
            Self::String { bell } => bell,
            Self::Clipboard { .. } => true,
        }
    }
}

#[derive(Debug)]
enum State {
    Ground,
    Utf8 {
        expect: u8,
        have: u8,
        cp: u32,
        alt: bool,
    },
    Esc {
        count: u8,
    },
    Csi(Csi),
    Ss3,
    Osc(Buf),
    OscEsc(Buf),
    Dcs(Dcs),
    DcsEsc(Dcs),
    Apc(Buf),
    ApcEsc(Buf),
    Ignore,
    IgnoreEsc,
    /// A string timed out. Bytes are ignored until ST or, for OSC, BEL.
    Discard {
        kind: DiscardKind,
    },
    /// Saw `ESC` while discarding. `\` finishes ST; anything else stays discarded.
    DiscardEsc {
        kind: DiscardKind,
    },
    Paste {
        data: Vec<u8>,
        matched: usize,
    },
}

/// Byte parser. It does not read a file descriptor.
#[derive(Debug)]
pub struct Parser {
    state: State,
    partial_at: Option<Instant>,
    /// True while the armed timer is the short Escape timer.
    esc_only: bool,
    /// An extra Escape was seen before this sequence (`ESC ESC …`).
    prefix_alt: bool,
    /// The kitty keyboard flags the terminal said are on (`CSI ? flags u`), 0 until then.
    kitty_flags: u32,
    /// Keys pressed and not yet released, when the terminal reports releases.
    held: Vec<(KeyId, super::keys::KeyEvent)>,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// An empty parser in the ground state.
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            partial_at: None,
            esc_only: false,
            prefix_alt: false,
            kitty_flags: 0,
            held: Vec::new(),
        }
    }

    /// Hands over a key the escape sequence `id` reported. With event types, a press or repeat
    /// is held until its release, and a release is reported only for a held key, as that key;
    /// without them, a press is followed by a synthesized release and a release is dropped.
    fn deliver(
        &mut self,
        id: KeyId,
        event: super::keys::KeyEvent,
        kind: KeyKind,
        out: &mut Vec<Input>,
    ) {
        if kind == KeyKind::Release {
            if let Some(at) = self.held.iter().position(|(held, _)| *held == id) {
                let (_, press) = self.held.remove(at);
                out.push(Input::Key(release_of(&press, event.modifiers)));
            }
            return;
        }
        if self.kitty_flags & EVENT_TYPES == 0 {
            let release = release_of(&event, event.modifiers);
            out.push(Input::Key(event));
            out.push(Input::Key(release));
            return;
        }
        self.held.retain(|(held, _)| *held != id);
        if self.held.len() >= MAX_HELD {
            self.held.remove(0);
        }
        self.held.push((id, event.clone()));
        out.push(Input::Key(event));
    }

    /// Parse the next bytes. Incomplete sequences stay buffered.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Input> {
        let mut out = Vec::new();
        for &byte in bytes {
            self.byte(byte, &mut out);
        }
        out
    }

    /// Time until [`Self::expire`] should be called, if a partial sequence is open.
    pub fn poll_timeout(&self) -> Option<Duration> {
        let start = self.partial_at?;
        let limit = if self.esc_only {
            ESC_TIMEOUT
        } else {
            SEQ_TIMEOUT
        };
        Some(limit.saturating_sub(start.elapsed()))
    }

    /// Whether the armed timer has already elapsed.
    pub fn timer_due(&self) -> bool {
        self.poll_timeout().is_some_and(|left| left.is_zero())
    }

    /// The armed timer fired. A lone Escape becomes a key. A bare introducer
    /// becomes Alt plus that byte. Anything with parameters is dropped, not
    /// reported as keys. An OSC, DCS or APC string that has started is
    /// discarded until its terminator, so the rest is not keys. Paste, and a
    /// string already being discarded, are left alone.
    pub fn expire(&mut self) -> Vec<Input> {
        if matches!(
            self.state,
            State::Paste { .. } | State::Ground | State::Discard { .. } | State::DiscardEsc { .. }
        ) {
            return Vec::new();
        }
        let state = std::mem::replace(&mut self.state, State::Ground);
        self.disarm();
        let mut out = Vec::new();
        match state {
            State::Esc { count } => {
                let mut modifiers = Modifiers::default();
                if count >= 2 {
                    modifiers.alt = true;
                }
                self.prefix_alt = false;
                extend_keys(
                    &mut out,
                    legacy_press(Key::Named("Escape".to_string()), modifiers, false, None),
                );
            }
            State::Ss3 => self.alt_char(b'O', &mut out),
            State::Csi(csi) if !csi.started => self.alt_char(b'[', &mut out),
            State::Osc(buf) => {
                if buf.data.is_empty() && !buf.truncated {
                    self.alt_char(b']', &mut out);
                } else {
                    self.prefix_alt = false;
                    self.state = State::Discard {
                        kind: osc_discard_kind(&buf),
                    };
                }
            }
            State::OscEsc(buf) => {
                self.prefix_alt = false;
                self.state = State::DiscardEsc {
                    kind: osc_discard_kind(&buf),
                };
            }
            State::Apc(buf) => {
                if buf.data.is_empty() && !buf.truncated {
                    self.alt_char(b'_', &mut out);
                } else {
                    self.prefix_alt = false;
                    self.state = State::Discard {
                        kind: DiscardKind::String { bell: false },
                    };
                }
            }
            State::ApcEsc(_) => {
                self.prefix_alt = false;
                self.state = State::DiscardEsc {
                    kind: DiscardKind::String { bell: false },
                };
            }
            State::Dcs(dcs) => {
                if !dcs.csi.started && !dcs.in_data {
                    self.alt_char(b'P', &mut out);
                } else {
                    self.prefix_alt = false;
                    self.state = State::Discard {
                        kind: DiscardKind::String { bell: false },
                    };
                }
            }
            State::DcsEsc(_) => {
                self.prefix_alt = false;
                self.state = State::DiscardEsc {
                    kind: DiscardKind::String { bell: false },
                };
            }
            _ => {
                self.prefix_alt = false;
            }
        }
        out
    }

    /// [`Self::expire`] when the timer has elapsed, otherwise nothing.
    pub fn expire_if_due(&mut self) -> Vec<Input> {
        if self.timer_due() {
            self.expire()
        } else {
            Vec::new()
        }
    }

    fn alt_char(&mut self, byte: u8, out: &mut Vec<Input>) {
        let modifiers = Modifiers {
            alt: true,
            ..Modifiers::default()
        };
        self.prefix_alt = false;
        let ch = char::from(byte);
        extend_keys(
            out,
            legacy_press(Key::Character(ch.to_string()), modifiers, false, None),
        );
    }

    fn arm(&mut self, esc_only: bool) {
        self.partial_at = Some(Instant::now());
        self.esc_only = esc_only;
    }

    fn disarm(&mut self) {
        self.partial_at = None;
        self.esc_only = false;
    }

    fn byte(&mut self, byte: u8, out: &mut Vec<Input>) {
        match std::mem::replace(&mut self.state, State::Ground) {
            State::Ground => self.ground(byte, out),
            State::Utf8 {
                expect,
                have,
                cp,
                alt,
            } => self.utf8(byte, expect, have, cp, alt, out),
            State::Esc { count } => self.escape(byte, count, out),
            State::Csi(mut csi) => self.csi(byte, &mut csi, out),
            State::Ss3 => self.ss3(byte, out),
            State::Osc(mut buf) => self.osc(byte, &mut buf, out),
            State::OscEsc(buf) => self.string_esc(byte, buf, true, out),
            State::Dcs(mut dcs) => self.dcs(byte, &mut dcs, out),
            State::DcsEsc(dcs) => {
                if byte == b'\\' {
                    self.disarm();
                    dispatch_dcs(dcs, out);
                } else {
                    self.prefix_alt = false;
                    self.escape(byte, 1, out);
                }
            }
            State::Apc(mut buf) => self.apc(byte, &mut buf, out),
            State::ApcEsc(buf) => self.string_esc(byte, buf, false, out),
            State::Ignore => self.ignore(byte),
            State::IgnoreEsc => {
                if byte == b'\\' {
                    self.disarm();
                } else {
                    self.escape(byte, 1, out);
                }
            }
            State::Discard { kind } => self.discard(byte, kind, out),
            State::DiscardEsc { kind } => self.discard_esc(byte, kind, out),
            State::Paste { data, matched } => self.paste(byte, data, matched, out),
        }
    }

    fn ground(&mut self, byte: u8, out: &mut Vec<Input>) {
        match byte {
            0x1b => {
                self.state = State::Esc { count: 1 };
                self.arm(true);
            }
            0x00..=0x1f | 0x7f => {
                self.emit_c0(byte, Modifiers::default(), out);
            }
            0x20..=0x7e => {
                let ch = char::from(byte);
                extend_keys(
                    out,
                    legacy_press(
                        Key::Character(ch.to_string()),
                        Modifiers::default(),
                        false,
                        Some(ch.to_string()),
                    ),
                );
            }
            _ => self.begin_utf8(byte, false, out),
        }
    }

    fn begin_utf8(&mut self, byte: u8, alt: bool, out: &mut Vec<Input>) {
        let Some((cp, expect)) = utf8_start(byte) else {
            return;
        };
        if expect == 0 {
            self.emit_char(char_or_drop(cp), alt, out);
        } else {
            self.state = State::Utf8 {
                expect,
                have: 0,
                cp,
                alt,
            };
            self.arm(false);
        }
    }

    fn utf8(&mut self, byte: u8, expect: u8, have: u8, cp: u32, alt: bool, out: &mut Vec<Input>) {
        if byte & 0xc0 != 0x80 {
            self.disarm();
            self.prefix_alt = false;
            self.byte(byte, out);
            return;
        }
        let cp = (cp << 6) | u32::from(byte & 0x3f);
        let have = have + 1;
        if have == expect {
            self.disarm();
            self.emit_char(char_or_drop(cp), alt, out);
        } else {
            self.state = State::Utf8 {
                expect,
                have,
                cp,
                alt,
            };
            self.arm(false);
        }
    }

    fn escape(&mut self, byte: u8, count: u8, out: &mut Vec<Input>) {
        if byte == 0x1b {
            self.state = State::Esc {
                count: count.saturating_add(1),
            };
            self.arm(true);
            return;
        }
        let prefixed = count >= 2;
        match byte {
            b'[' => {
                self.prefix_alt = prefixed;
                self.state = State::Csi(Csi::new());
                self.arm(false);
            }
            b']' => {
                self.prefix_alt = prefixed;
                self.state = State::Osc(Buf::new());
                self.arm(false);
            }
            b'P' => {
                self.prefix_alt = prefixed;
                self.state = State::Dcs(Dcs::new());
                self.arm(false);
            }
            b'_' => {
                self.prefix_alt = prefixed;
                self.state = State::Apc(Buf::new());
                self.arm(false);
            }
            b'X' | b'^' => {
                self.prefix_alt = false;
                self.state = State::Ignore;
                self.arm(false);
            }
            b'O' => {
                self.prefix_alt = prefixed;
                self.state = State::Ss3;
                self.arm(false);
            }
            0x80..=0xff => {
                self.prefix_alt = prefixed;
                self.disarm();
                self.begin_utf8(byte, true, out);
            }
            _ => {
                self.disarm();
                let modifiers = Modifiers {
                    alt: true,
                    ..Modifiers::default()
                };
                self.prefix_alt = false;
                if byte == 0x00 || (0x01..=0x1f).contains(&byte) || byte == 0x7f {
                    self.emit_c0(byte, modifiers, out);
                } else if byte.is_ascii() {
                    let ch = char::from(byte);
                    extend_keys(
                        out,
                        legacy_press(Key::Character(ch.to_string()), modifiers, false, None),
                    );
                }
            }
        }
    }

    fn csi(&mut self, byte: u8, csi: &mut Csi, out: &mut Vec<Input>) {
        match byte {
            0x18 | 0x1a => {
                self.prefix_alt = false;
                self.disarm();
            }
            0x1b => {
                self.prefix_alt = false;
                self.state = State::Esc { count: 1 };
                self.arm(true);
            }
            0x00..=0x1f | 0x7f => {
                // A C0 byte (Enter, a Ctrl chord) or DEL is not part of the CSI.
                // Abort and parse it, so Alt+[ does not swallow the next key.
                self.prefix_alt = false;
                self.disarm();
                self.ground(byte, out);
            }
            b'0'..=b'9' => {
                csi.push_digit(byte - b'0');
                self.state = State::Csi(take_csi(csi));
                self.arm(false);
            }
            b':' => {
                csi.separate(true);
                self.state = State::Csi(take_csi(csi));
                self.arm(false);
            }
            b';' => {
                csi.separate(false);
                self.state = State::Csi(take_csi(csi));
                self.arm(false);
            }
            b'?' | b'>' | b'<' | b'='
                if csi.private.is_none() && !csi.started && csi.intermediates.is_empty() =>
            {
                csi.private = Some(byte);
                csi.started = true;
                self.state = State::Csi(take_csi(csi));
                self.arm(false);
            }
            0x20..=0x2f => {
                if csi.intermediates.len() < 2 {
                    csi.intermediates.push(byte);
                }
                csi.started = true;
                self.state = State::Csi(take_csi(csi));
                self.arm(false);
            }
            0x40..=0x7e => {
                csi.finish();
                self.disarm();
                self.dispatch_csi(take_csi(csi), byte, out);
            }
            _ => {
                self.prefix_alt = false;
                self.disarm();
            }
        }
    }

    fn dispatch_csi(&mut self, csi: Csi, final_byte: u8, out: &mut Vec<Input>) {
        let alt = self.prefix_alt;
        self.prefix_alt = false;
        if csi.private == Some(b'<') && (final_byte == b'M' || final_byte == b'm') {
            if let Some(mouse) = mouse_from(&csi, final_byte) {
                out.push(Input::Mouse(mouse));
            }
            return;
        }
        // Focus in/out (`CSI I` / `CSI O`) once `CSI ? 1004` is set. No parameters.
        if csi.private.is_none()
            && csi.params.is_empty()
            && csi.intermediates.is_empty()
            && (final_byte == b'I' || final_byte == b'O')
        {
            out.push(Input::Focus(final_byte == b'I'));
            return;
        }
        if csi.private == Some(b'?') && final_byte == b'u' {
            if let Some(flags) = csi.number(0) {
                self.kitty_flags = flags;
                out.push(Input::Reply(Reply::KittyKeyboard { flags }));
            }
            return;
        }
        if csi.private == Some(b'?') && final_byte == b'c' {
            let params = csi
                .params
                .iter()
                .filter_map(|param| param.first().copied().flatten())
                .collect::<Vec<_>>();
            let sixel = params.contains(&4);
            out.push(Input::Reply(Reply::Da1 { params, sixel }));
            return;
        }
        if csi.private.is_none() && final_byte == b't' {
            match (csi.number(0), csi.number(1), csi.number(2)) {
                (Some(6), Some(height), Some(width)) => {
                    out.push(Input::Reply(Reply::CellPixels { height, width }));
                }
                (Some(4), Some(height), Some(width)) => {
                    out.push(Input::Reply(Reply::TextAreaPixels { height, width }));
                }
                _ => {}
            }
            return;
        }
        if csi.private.is_none() && final_byte == b'~' {
            match csi.number(0) {
                Some(200) => {
                    self.state = State::Paste {
                        data: Vec::new(),
                        matched: 0,
                    };
                    self.disarm();
                }
                Some(201) => {}
                Some(number) => {
                    if let Some(name) = tilde_key(number) {
                        let modifiers = csi_modifiers(&csi);
                        let kind = csi_kind(&csi);
                        self.emit_named(KeyId::Tilde(number), name, modifiers, kind, alt, out);
                    }
                }
                None => {}
            }
            return;
        }
        if csi.private.is_none() && final_byte == b'u' {
            self.emit_kitty(&csi, alt, out);
            return;
        }
        if csi.private.is_none() {
            if let Some(name) = legacy_final(final_byte, &csi) {
                let mut modifiers = csi_modifiers(&csi);
                if final_byte == b'Z' {
                    modifiers.shift = true;
                }
                let kind = csi_kind(&csi);
                self.emit_named(KeyId::Final(final_byte), name, modifiers, kind, alt, out);
            }
        }
    }

    fn emit_kitty(&mut self, csi: &Csi, alt: bool, out: &mut Vec<Input>) {
        let subs = csi.params.first().cloned().unwrap_or_default();
        let Some(Some(codepoint)) = subs.first().copied() else {
            return;
        };
        let shifted = subs.get(1).copied().flatten();
        let base = subs.get(2).copied().flatten();
        let modifier_field = csi
            .params
            .get(1)
            .and_then(|param| param.first().copied().flatten());
        let event = csi
            .params
            .get(1)
            .and_then(|param| param.get(1).copied().flatten())
            .unwrap_or(1);
        let text_points = csi
            .params
            .get(2)
            .map(|param| param.iter().copied().flatten().collect::<Vec<_>>())
            .unwrap_or_default();
        let kind = event_kind(event);
        let key = kitty_key(
            codepoint,
            shifted,
            base,
            kitty_modifiers(modifier_field.unwrap_or(1)),
            kind,
            &text_points,
            alt,
        );
        self.deliver(KeyId::Unicode(codepoint), key, kind, out);
    }

    /// A named key in a `CSI` letter or `~` form.
    fn emit_named(
        &mut self,
        id: KeyId,
        name: &'static str,
        modifiers: Modifiers,
        kind: KeyKind,
        alt: bool,
        out: &mut Vec<Input>,
    ) {
        let mut modifiers = modifiers;
        if alt {
            modifiers.alt = true;
        }
        let mut event = named_key(name, modifiers, kind);
        // A terminal that speaks the kitty protocol sends these forms for these keys only.
        if self.kitty_flags != 0 {
            event.code = super::keys::KeyCode(named_evdev(name));
        }
        self.deliver(id, event, kind, out);
    }

    /// A named key in an `SS3` form, which never says when it is released.
    fn emit_ss3(&mut self, name: &'static str, alt: bool, out: &mut Vec<Input>) {
        let modifiers = Modifiers {
            alt,
            ..Modifiers::default()
        };
        extend_keys(
            out,
            legacy_press(Key::Named(name.to_string()), modifiers, false, None),
        );
    }

    fn ss3(&mut self, byte: u8, out: &mut Vec<Input>) {
        self.disarm();
        let alt = self.prefix_alt;
        self.prefix_alt = false;
        let name = match byte {
            b'A' => "ArrowUp",
            b'B' => "ArrowDown",
            b'C' => "ArrowRight",
            b'D' => "ArrowLeft",
            b'H' => "Home",
            b'F' => "End",
            b'P' => "F1",
            b'Q' => "F2",
            b'R' => "F3",
            b'S' => "F4",
            _ => {
                if let Some((key, code, text)) = ss3_keypad(byte) {
                    self.emit_keypad(key, code, text, alt, out);
                } else {
                    // Not an SS3 key. `ESC O` was Alt+O; this byte is the next key.
                    self.alt_char(b'O', out);
                    self.byte(byte, out);
                }
                return;
            }
        };
        self.emit_ss3(name, alt, out);
    }

    /// A keypad key from application mode (DECKPAM). Digits and operators type
    /// their character; Enter does not. `code` is the keypad's evdev key.
    fn emit_keypad(
        &mut self,
        key: Key,
        code: u32,
        text: Option<&str>,
        alt: bool,
        out: &mut Vec<Input>,
    ) {
        let modifiers = Modifiers {
            alt,
            ..Modifiers::default()
        };
        // Alt is a chord and types nothing, same as `ESC a`.
        let text = if alt { None } else { text.map(str::to_string) };
        let mut events = legacy_press(key, modifiers, false, text);
        for event in &mut events {
            event.code = KeyCode(code);
        }
        extend_keys(out, events);
    }

    fn osc(&mut self, byte: u8, buf: &mut Buf, out: &mut Vec<Input>) {
        match byte {
            0x07 => {
                self.disarm();
                self.prefix_alt = false;
                dispatch_osc(&buf.data, true, buf.truncated, out);
            }
            0x18 | 0x1a => {
                self.prefix_alt = false;
                self.disarm();
            }
            0x1b => {
                self.state = State::OscEsc(take_buf(buf));
                self.arm(false);
            }
            _ => {
                buf.push(byte);
                self.state = State::Osc(take_buf(buf));
                self.arm(false);
            }
        }
    }

    fn apc(&mut self, byte: u8, buf: &mut Buf, _out: &mut Vec<Input>) {
        match byte {
            0x18 | 0x1a => {
                self.prefix_alt = false;
                self.disarm();
            }
            0x1b => {
                self.state = State::ApcEsc(take_buf(buf));
                self.arm(false);
            }
            _ => {
                buf.push(byte);
                self.state = State::Apc(take_buf(buf));
                self.arm(false);
            }
        }
    }

    fn string_esc(&mut self, byte: u8, buf: Buf, osc: bool, out: &mut Vec<Input>) {
        if byte == b'\\' {
            self.disarm();
            self.prefix_alt = false;
            if osc {
                dispatch_osc(&buf.data, false, buf.truncated, out);
            } else {
                dispatch_apc(&buf.data, out);
            }
        } else {
            self.prefix_alt = false;
            self.escape(byte, 1, out);
        }
    }

    fn dcs(&mut self, byte: u8, dcs: &mut Dcs, _out: &mut Vec<Input>) {
        if dcs.in_data {
            match byte {
                0x18 | 0x1a => {
                    self.prefix_alt = false;
                    self.disarm();
                }
                0x1b => {
                    self.state = State::DcsEsc(take_dcs(dcs));
                    self.arm(false);
                }
                _ => {
                    dcs.data.push(byte);
                    self.state = State::Dcs(take_dcs(dcs));
                    self.arm(false);
                }
            }
            return;
        }
        match byte {
            0x18 | 0x1a => {
                self.prefix_alt = false;
                self.disarm();
            }
            0x1b => {
                self.prefix_alt = false;
                self.state = State::Esc { count: 1 };
                self.arm(true);
            }
            b'0'..=b'9' => {
                dcs.csi.push_digit(byte - b'0');
                self.state = State::Dcs(take_dcs(dcs));
                self.arm(false);
            }
            b':' => {
                dcs.csi.separate(true);
                self.state = State::Dcs(take_dcs(dcs));
                self.arm(false);
            }
            b';' => {
                dcs.csi.separate(false);
                self.state = State::Dcs(take_dcs(dcs));
                self.arm(false);
            }
            0x20..=0x2f => {
                if dcs.csi.intermediates.len() < 2 {
                    dcs.csi.intermediates.push(byte);
                }
                dcs.csi.started = true;
                self.state = State::Dcs(take_dcs(dcs));
                self.arm(false);
            }
            0x40..=0x7e => {
                dcs.csi.finish();
                dcs.final_byte = byte;
                dcs.in_data = true;
                self.state = State::Dcs(take_dcs(dcs));
                self.arm(false);
            }
            _ => {
                self.prefix_alt = false;
                self.disarm();
            }
        }
    }

    /// Drop bytes of a string that timed out, until ST or (for OSC) BEL.
    /// Nothing here is a key. CAN and SUB end the string, as they do in ground.
    fn discard(&mut self, byte: u8, kind: DiscardKind, out: &mut Vec<Input>) {
        match byte {
            0x07 if kind.bell() => self.finish_discard(kind, out),
            0x18 | 0x1a => self.finish_discard(kind, out),
            0x1b => self.state = State::DiscardEsc { kind },
            _ => self.state = State::Discard { kind },
        }
    }

    fn discard_esc(&mut self, byte: u8, kind: DiscardKind, out: &mut Vec<Input>) {
        if byte == b'\\' {
            self.finish_discard(kind, out);
            return;
        }
        if byte == 0x1b {
            // The previous ESC was not ST. This one might be.
            self.state = State::DiscardEsc { kind };
            return;
        }
        self.discard(byte, kind, out);
    }

    fn finish_discard(&mut self, kind: DiscardKind, out: &mut Vec<Input>) {
        self.disarm();
        self.prefix_alt = false;
        if let DiscardKind::Clipboard { selection } = kind {
            out.push(Input::Reply(Reply::Clipboard {
                selection,
                data: None,
                truncated: true,
            }));
        }
    }

    fn ignore(&mut self, byte: u8) {
        match byte {
            0x18 | 0x1a => self.disarm(),
            0x1b => {
                self.state = State::IgnoreEsc;
                self.arm(false);
            }
            _ => {
                self.state = State::Ignore;
                self.arm(false);
            }
        }
    }

    fn paste(&mut self, byte: u8, mut data: Vec<u8>, matched: usize, out: &mut Vec<Input>) {
        if byte == PASTE_END[matched] {
            let matched = matched + 1;
            if matched == PASTE_END.len() {
                out.push(Input::Paste(String::from_utf8_lossy(&data).into_owned()));
                self.disarm();
                return;
            }
            self.state = State::Paste { data, matched };
            return;
        }
        // Past the cap, keep discarding until `CSI 201 ~`. Emitting now and
        // returning to ground would run the rest of the paste as keys.
        if data.len() >= MAX_PASTE {
            self.state = State::Paste {
                data,
                matched: usize::from(byte == PASTE_END[0]),
            };
            return;
        }
        let prefix = &PASTE_END[..matched];
        let room = MAX_PASTE - data.len();
        if prefix.len() >= room {
            data.extend_from_slice(&prefix[..room]);
            self.state = State::Paste {
                data,
                matched: usize::from(byte == PASTE_END[0]),
            };
            return;
        }
        data.extend_from_slice(prefix);
        if byte == PASTE_END[0] {
            self.state = State::Paste { data, matched: 1 };
            return;
        }
        data.push(byte);
        self.state = State::Paste { data, matched: 0 };
    }

    fn emit_char(&mut self, ch: Option<char>, alt: bool, out: &mut Vec<Input>) {
        let Some(ch) = ch else {
            return;
        };
        if ch.is_control() {
            return;
        }
        let mut modifiers = Modifiers::default();
        if alt || self.prefix_alt {
            modifiers.alt = true;
            self.prefix_alt = false;
            extend_keys(
                out,
                legacy_press(Key::Character(ch.to_string()), modifiers, false, None),
            );
            return;
        }
        extend_keys(
            out,
            legacy_press(
                Key::Character(ch.to_string()),
                modifiers,
                false,
                Some(ch.to_string()),
            ),
        );
    }

    fn emit_c0(&mut self, byte: u8, mut modifiers: Modifiers, out: &mut Vec<Input>) {
        if self.prefix_alt {
            modifiers.alt = true;
            self.prefix_alt = false;
        }
        let (key, extra, ambiguous) = c0_key(byte);
        modifiers.control |= extra.control;
        modifiers.shift |= extra.shift;
        modifiers.alt |= extra.alt;
        extend_keys(out, legacy_press(key, modifiers, ambiguous, None));
    }
}

fn extend_keys(out: &mut Vec<Input>, keys: Vec<super::keys::KeyEvent>) {
    out.extend(keys.into_iter().map(Input::Key));
}

fn take_csi(csi: &mut Csi) -> Csi {
    std::mem::replace(csi, Csi::new())
}

fn take_buf(buf: &mut Buf) -> Buf {
    std::mem::replace(buf, Buf::new())
}

fn take_dcs(dcs: &mut Dcs) -> Dcs {
    std::mem::replace(dcs, Dcs::new())
}

fn char_or_drop(cp: u32) -> Option<char> {
    char::from_u32(cp).filter(|_| !matches!(cp, 0x80..=0x9f))
}

fn utf8_start(byte: u8) -> Option<(u32, u8)> {
    if byte & 0xe0 == 0xc0 {
        Some((u32::from(byte & 0x1f), 1))
    } else if byte & 0xf0 == 0xe0 {
        Some((u32::from(byte & 0x0f), 2))
    } else if byte & 0xf8 == 0xf0 {
        Some((u32::from(byte & 0x07), 3))
    } else {
        None
    }
}

fn c0_key(byte: u8) -> (Key, Modifiers, bool) {
    let mut control = Modifiers::default();
    match byte {
        0x00 => {
            control.control = true;
            (Key::Character(" ".to_string()), control, false)
        }
        0x08 => (Key::Named("Backspace".to_string()), control, true),
        0x09 => (Key::Named("Tab".to_string()), control, true),
        0x0d => (Key::Named("Enter".to_string()), control, true),
        0x7f => (Key::Named("Backspace".to_string()), control, false),
        0x01..=0x1a => {
            control.control = true;
            let ch = char::from(b'a' + (byte - 1));
            (Key::Character(ch.to_string()), control, false)
        }
        0x1c => {
            control.control = true;
            (Key::Character("\\".to_string()), control, false)
        }
        0x1d => {
            control.control = true;
            (Key::Character("]".to_string()), control, false)
        }
        0x1e => {
            control.control = true;
            (Key::Character("^".to_string()), control, true)
        }
        0x1f => {
            control.control = true;
            (Key::Character("_".to_string()), control, true)
        }
        _ => (Key::Unidentified, control, false),
    }
}

fn csi_modifiers(csi: &Csi) -> Modifiers {
    kitty_modifiers(csi.number(1).unwrap_or(1))
}

/// The event type in the modifier field's second part (`CSI 1;1:3 D`), a press without one.
fn csi_kind(csi: &Csi) -> KeyKind {
    let event = csi
        .params
        .get(1)
        .and_then(|param| param.get(1).copied().flatten());
    event_kind(event.unwrap_or(1))
}

fn event_kind(event: u32) -> KeyKind {
    match event {
        2 => KeyKind::Repeat,
        3 => KeyKind::Release,
        _ => KeyKind::Press,
    }
}

/// A named key's event: no physical code (the `CSI` forms don't name one), no text.
fn named_key(name: &str, modifiers: Modifiers, kind: KeyKind) -> super::keys::KeyEvent {
    super::keys::KeyEvent {
        code: super::keys::KeyCode(0),
        key: Key::Named(name.to_string()),
        state: match kind {
            KeyKind::Release => ButtonState::Released,
            KeyKind::Press | KeyKind::Repeat => ButtonState::Pressed,
        },
        repeat: kind == KeyKind::Repeat,
        modifiers,
        consumed: Modifiers::default(),
        text: None,
        ambiguous: false,
    }
}

/// Application-keypad final (`SS3` after DECKPAM). Arrows and F1–F4 are not here.
///
/// Digits and operators are characters. Enter is Enter. The codes are the
/// keypad's evdev keys, the same ones kitty numbers 57399..=57416.
fn ss3_keypad(byte: u8) -> Option<(Key, u32, Option<&'static str>)> {
    Some(match byte {
        b'p' => (Key::Character("0".to_string()), 82, Some("0")),
        b'q' => (Key::Character("1".to_string()), 79, Some("1")),
        b'r' => (Key::Character("2".to_string()), 80, Some("2")),
        b's' => (Key::Character("3".to_string()), 81, Some("3")),
        b't' => (Key::Character("4".to_string()), 75, Some("4")),
        b'u' => (Key::Character("5".to_string()), 76, Some("5")),
        b'v' => (Key::Character("6".to_string()), 77, Some("6")),
        b'w' => (Key::Character("7".to_string()), 71, Some("7")),
        b'x' => (Key::Character("8".to_string()), 72, Some("8")),
        b'y' => (Key::Character("9".to_string()), 73, Some("9")),
        b'j' => (Key::Character("*".to_string()), 55, Some("*")),
        b'k' => (Key::Character("+".to_string()), 78, Some("+")),
        b'l' => (Key::Character(",".to_string()), 121, Some(",")),
        b'm' => (Key::Character("-".to_string()), 74, Some("-")),
        b'n' => (Key::Character(".".to_string()), 83, Some(".")),
        b'o' => (Key::Character("/".to_string()), 98, Some("/")),
        b'X' => (Key::Character("=".to_string()), 117, Some("=")),
        b'M' => (Key::Named("Enter".to_string()), 96, None),
        _ => return None,
    })
}

fn legacy_final(final_byte: u8, csi: &Csi) -> Option<&'static str> {
    Some(match final_byte {
        b'A' => "ArrowUp",
        b'B' => "ArrowDown",
        b'C' => "ArrowRight",
        b'D' => "ArrowLeft",
        b'H' => "Home",
        b'F' => "End",
        b'Z' => "Tab",
        // The keypad's middle key (kitty's KP_BEGIN), as kitty's table names it.
        b'E' => "Clear",
        // Kitty sends F1, F2 and F4 bare (`CSI P`); F3 is `CSI 13 ~`, since a bare `CSI R`
        // is a cursor position report.
        b'P' => "F1",
        b'Q' => "F2",
        b'S' => "F4",
        b'R' if !csi.params.is_empty() => "F3",
        _ => return None,
    })
}

fn tilde_key(number: u32) -> Option<&'static str> {
    Some(match number {
        2 => "Insert",
        3 => "Delete",
        5 => "PageUp",
        6 => "PageDown",
        7 => "Home",
        8 => "End",
        11 => "F1",
        12 => "F2",
        13 => "F3",
        14 => "F4",
        15 => "F5",
        17 => "F6",
        18 => "F7",
        19 => "F8",
        20 => "F9",
        21 => "F10",
        23 => "F11",
        24 => "F12",
        29 => "ContextMenu",
        _ => return None,
    })
}

fn mouse_from(csi: &Csi, final_byte: u8) -> Option<MouseEvent> {
    let button_code = csi.number(0)?;
    let column = u16::try_from(csi.number(1)?).unwrap_or(u16::MAX);
    let row = u16::try_from(csi.number(2)?).unwrap_or(u16::MAX);
    let modifiers = Modifiers {
        shift: button_code & 4 != 0,
        alt: button_code & 8 != 0,
        control: button_code & 16 != 0,
        ..Modifiers::default()
    };
    let motion = button_code & 32 != 0;
    let low = button_code & 3;
    let (button, wheel) = if button_code & 64 != 0 {
        let wheel = match low {
            0 => Wheel::Up,
            1 => Wheel::Down,
            2 => Wheel::Left,
            _ => Wheel::Right,
        };
        (MouseButton::None, Some(wheel))
    } else if button_code & 128 != 0 {
        let button = if low == 1 {
            MouseButton::Forward
        } else {
            MouseButton::Back
        };
        (button, None)
    } else {
        let button = match low {
            0 => MouseButton::Left,
            1 => MouseButton::Middle,
            2 => MouseButton::Right,
            _ => MouseButton::None,
        };
        (button, None)
    };
    Some(MouseEvent {
        button,
        state: if final_byte == b'm' {
            ButtonState::Released
        } else {
            ButtonState::Pressed
        },
        column,
        row,
        modifiers,
        motion,
        wheel,
    })
}

fn dispatch_osc(payload: &[u8], bell: bool, truncated: bool, out: &mut Vec<Input>) {
    let text = String::from_utf8_lossy(payload);
    let Some((index, spec)) = text.split_once(';') else {
        return;
    };
    let Ok(index) = index.parse::<u16>() else {
        return;
    };
    if index == 52 {
        // A prefix of the base64 is not the clipboard. The terminator still ends the string.
        out.push(Input::Reply(Reply::Clipboard {
            selection: selection_byte(spec),
            data: if truncated {
                None
            } else {
                clipboard_data(spec)
            },
            truncated,
        }));
        return;
    }
    if index == 10 || index == 11 {
        out.push(Input::Reply(Reply::OscColor {
            index,
            spec: spec.to_string(),
            bell,
        }));
    }
}

/// `c` or `p` from OSC 52's selection field, when the terminal named one of those.
fn selection_byte(spec: &str) -> Option<u8> {
    match spec.split(';').next().unwrap_or("").trim() {
        "c" => Some(b'c'),
        "p" => Some(b'p'),
        _ => None,
    }
}

/// A timed-out OSC is a clipboard reply only when the buffer already says OSC 52.
fn osc_discard_kind(buf: &Buf) -> DiscardKind {
    let text = String::from_utf8_lossy(&buf.data);
    match text.split_once(';') {
        Some(("52", spec)) => DiscardKind::Clipboard {
            selection: selection_byte(spec),
        },
        _ => DiscardKind::String { bell: true },
    }
}

/// OSC 52's data field: base64, or nothing when it is `?`, empty or not base64.
fn clipboard_data(spec: &str) -> Option<Vec<u8>> {
    let data = spec.split_once(';').map(|(_, data)| data).unwrap_or("");
    let data = data.trim();
    if data.is_empty() || data == "?" {
        None
    } else {
        super::clipboard::decode(data)
    }
}

fn dispatch_apc(payload: &[u8], out: &mut Vec<Input>) {
    let raw = String::from_utf8_lossy(payload).into_owned();
    if raw.starts_with('G') {
        let ok = raw.contains(";OK");
        out.push(Input::Reply(Reply::KittyGraphics { raw, ok }));
    }
}

fn dispatch_dcs(dcs: Dcs, out: &mut Vec<Input>) {
    if dcs.final_byte != b'r' || !dcs.csi.intermediates.contains(&b'+') {
        return;
    }
    let Some(status) = dcs.csi.number(0) else {
        return;
    };
    if status > 1 {
        return;
    }
    let raw = String::from_utf8_lossy(&dcs.data.data);
    let mut entries = Vec::new();
    if !raw.is_empty() {
        for part in raw.split(';') {
            if part.is_empty() {
                continue;
            }
            if let Some((name, value)) = part.split_once('=') {
                entries.push((decode_hex(name), Some(decode_hex(value))));
            } else {
                entries.push((decode_hex(part), None));
            }
        }
    }
    out.push(Input::Reply(Reply::XtGetTcap {
        found: status == 1,
        entries,
    }));
}

fn decode_hex(text: &str) -> String {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(2) || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return text.to_string();
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        let high = hex_nibble(bytes[index]);
        let low = hex_nibble(bytes[index + 1]);
        out.push((high << 4) | low);
        index += 2;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

fn hex_nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        b'a'..=b'f' => byte - b'a' + 10,
        b'A'..=b'F' => byte - b'A' + 10,
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::super::keys::{ButtonState, Key, KeyCode, KeyEvent};
    use super::*;

    fn keys_of(bytes: &[u8]) -> Vec<KeyEvent> {
        Parser::new()
            .feed(bytes)
            .into_iter()
            .filter_map(|input| match input {
                Input::Key(key) => Some(key),
                _ => None,
            })
            .collect()
    }

    /// Keys from a parser told that the terminal reports event types, as kitty and Rio do
    /// after `CSI > 31 u`.
    fn kitty_keys_of(bytes: &[u8]) -> Vec<KeyEvent> {
        let mut parser = Parser::new();
        parser.feed(b"\x1b[?31u");
        parser
            .feed(bytes)
            .into_iter()
            .filter_map(|input| match input {
                Input::Key(key) => Some(key),
                _ => None,
            })
            .collect()
    }

    /// (key, pressed, text) of each event, for comparing whole sequences.
    fn summary(keys: &[KeyEvent]) -> Vec<(String, bool, Option<String>)> {
        keys.iter()
            .map(|key| {
                let name = match &key.key {
                    Key::Character(text) | Key::Named(text) => text.clone(),
                    other => format!("{other:?}"),
                };
                (name, key.state == ButtonState::Pressed, key.text.clone())
            })
            .collect()
    }

    fn reply(bytes: &[u8]) -> Reply {
        match Parser::new().feed(bytes).as_slice() {
            [Input::Reply(reply)] => reply.clone(),
            other => panic!("expected one reply, got {other:?}"),
        }
    }

    #[test]
    fn kitty_shift_a_uses_the_base_layout_and_the_text() {
        let keys = kitty_keys_of(b"\x1b[97:65:97;2:1;65u");
        assert_eq!(
            keys.len(),
            1,
            "kitty events are not given a synthetic release"
        );
        assert_eq!(keys[0].code, KeyCode(30));
        assert_eq!(keys[0].key, Key::Character("A".into()));
        assert!(keys[0].modifiers.shift && keys[0].consumed.shift);
        assert_eq!(keys[0].text.as_deref(), Some("A"));
        assert_eq!(keys[0].state, ButtonState::Pressed);
    }

    #[test]
    fn kitty_release_and_repeat_keep_their_event_type() {
        let keys = kitty_keys_of(b"\x1b[97:65:97;2:1;65u\x1b[97:65:97;2:3u");
        assert_eq!(keys.len(), 2);
        let release = &keys[1];
        assert_eq!(release.state, ButtonState::Released);
        assert!(release.text.is_none());
        assert_eq!(release.code, KeyCode(30));
        let repeat = kitty_keys_of(b"\x1b[97::97;1:2u");
        assert!(repeat[0].repeat && repeat[0].state == ButtonState::Pressed);
        assert_eq!(repeat[0].code, KeyCode(30));
    }

    #[test]
    fn rio_csi_u_without_a_base_layout_uses_the_us_evdev_code() {
        // Rio sends no base-layout field. Press and release share KEY_A, so
        // the core can pair them. `A` is lower-cased onto the same code.
        let keys = kitty_keys_of(b"\x1b[97u\x1b[97;1:3u");
        assert_eq!(keys.len(), 2, "{keys:?}");
        assert_eq!(keys[0].code, KeyCode(30), "KEY_A");
        assert_eq!(keys[1].code, KeyCode(30));
        assert_eq!(keys[0].key, Key::Character("a".into()));
        assert_eq!(keys[1].state, ButtonState::Released);
        let upper = kitty_keys_of(b"\x1b[65u");
        assert_eq!(upper[0].code, KeyCode(30), "A folds onto KEY_A");
        let unknown = kitty_keys_of(b"\x1b[229u");
        assert_eq!(unknown[0].code, KeyCode(0), "å is not a US key");
    }

    #[test]
    fn emoji_without_a_base_layout_has_no_evdev_code() {
        let keys = kitty_keys_of(b"\x1b[128512u");
        assert_eq!(keys[0].key, Key::Character("😀".into()));
        assert_eq!(keys[0].code, KeyCode(0));
        let with_base = kitty_keys_of(b"\x1b[128512:97:97;1:1;128512u");
        assert_eq!(with_base[0].code, KeyCode(30));
        assert_eq!(with_base[0].text.as_deref(), Some("😀"));
    }

    #[test]
    fn tab_and_ctrl_i_stay_distinct_under_kitty() {
        let tab = kitty_keys_of(b"\x1b[9u");
        assert_eq!(tab.len(), 1);
        assert_eq!(tab[0].key, Key::Named("Tab".into()));
        assert_eq!(tab[0].code, KeyCode(15));
        assert!(!tab[0].ambiguous);
        let ctrl_i = kitty_keys_of(b"\x1b[105;5u");
        assert_eq!(ctrl_i[0].key, Key::Character("i".into()));
        assert!(ctrl_i[0].modifiers.control);
        assert_eq!(ctrl_i[0].code, KeyCode(23), "KEY_I, not Tab");
        assert!(ctrl_i[0].text.is_none());
        assert!(!ctrl_i[0].ambiguous);
    }

    #[test]
    fn legacy_tab_enter_and_backspace_are_ambiguous_and_release() {
        let tab = keys_of(b"\t");
        assert_eq!(tab.len(), 2);
        assert!(tab[0].ambiguous && tab[0].code == KeyCode(0));
        assert_eq!(tab[1].state, ButtonState::Released);
        let enter = keys_of(b"\r");
        assert!(enter[0].ambiguous);
        assert_eq!(enter[0].key, Key::Named("Enter".into()));
        let backspace = keys_of(b"\x08");
        assert!(backspace[0].ambiguous);
        let del = keys_of(b"\x7f");
        assert!(!del[0].ambiguous);
        assert_eq!(del[0].key, Key::Named("Backspace".into()));
    }

    #[test]
    fn legacy_arrows_function_keys_and_alt_prefix() {
        let up = keys_of(b"\x1b[A");
        assert_eq!(up[0].key, Key::Named("ArrowUp".into()));
        assert_eq!(up[0].code, KeyCode(0));
        assert_eq!(up[1].state, ButtonState::Released);
        let f5 = keys_of(b"\x1b[15~");
        assert_eq!(f5[0].key, Key::Named("F5".into()));
        let f1 = keys_of(b"\x1bOP");
        assert_eq!(f1[0].key, Key::Named("F1".into()));
        let alt_a = keys_of(b"\x1ba");
        assert_eq!(alt_a.len(), 2);
        assert!(alt_a[0].modifiers.alt);
        assert!(alt_a[0].text.is_none());
        let ctrl_a = keys_of(b"\x01");
        assert_eq!(ctrl_a[0].key, Key::Character("a".into()));
        assert!(ctrl_a[0].modifiers.control && ctrl_a[0].text.is_none());
    }

    #[test]
    fn lone_escape_waits_for_expire_and_a_partial_csi_does_not() {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b").is_empty());
        assert!(parser.poll_timeout().unwrap() <= ESC_TIMEOUT);
        assert!(parser.expire_if_due().is_empty());
        let escaped = parser.expire();
        assert_eq!(escaped.len(), 2);
        assert_eq!(
            match &escaped[0] {
                Input::Key(key) => &key.key,
                _ => panic!("key"),
            },
            &Key::Named("Escape".into())
        );

        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b[?62;").is_empty());
        assert!(parser.poll_timeout().unwrap() > ESC_TIMEOUT);
        assert!(parser.expire().is_empty(), "a partial reply is not a key");
        let letter = parser.feed(b"a");
        assert!(matches!(
            &letter[0],
            Input::Key(key) if key.key == Key::Character("a".into())
        ));
    }

    #[test]
    fn a_split_reply_completes_as_a_reply() {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b[?62;").is_empty());
        let rest = parser.feed(b"4c");
        match rest.as_slice() {
            [Input::Reply(Reply::Da1 { sixel: true, .. })] => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_paste_past_the_cap_is_one_paste_and_no_keys() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"\x1b[200~");
        bytes.extend(std::iter::repeat_n(b'a', MAX_PASTE));
        // `q` would quit and `x` would close a tab if this tail were keys.
        // `\x1b[A` is an arrow only outside a paste.
        bytes.extend_from_slice(b"q\x1b[Ax\x1b[201~z");
        let inputs = Parser::new().feed(&bytes);
        assert_eq!(inputs.len(), 3, "{inputs:?}");
        match &inputs[0] {
            Input::Paste(text) => {
                assert_eq!(text.len(), MAX_PASTE);
                assert!(
                    text.bytes().all(|byte| byte == b'a'),
                    "tail leaked into the paste"
                );
            }
            other => panic!("paste: {other:?}"),
        }
        match &inputs[1] {
            Input::Key(key) => {
                assert_eq!(key.key, Key::Character("z".into()));
                assert_eq!(key.state, ButtonState::Pressed);
            }
            other => panic!("after the paste: {other:?}"),
        }
        assert!(matches!(
            &inputs[2],
            Input::Key(key) if key.state == ButtonState::Released
        ));
    }

    #[test]
    fn alt_o_and_alt_bracket_do_not_eat_the_next_key() {
        // `z` is not a keypad final. `x` is numpad 8 (`ESC O x`).
        let keys = keys_of(b"\x1bOz");
        assert_eq!(keys.len(), 4, "{keys:?}");
        assert_eq!(keys[0].key, Key::Character("O".into()));
        assert!(keys[0].modifiers.alt);
        assert_eq!(keys[0].state, ButtonState::Pressed);
        assert_eq!(keys[2].key, Key::Character("z".into()));
        assert!(!keys[2].modifiers.alt);
        assert_eq!(keys[2].text.as_deref(), Some("z"));

        // Enter aborts the CSI. It is delivered, and the following key is too.
        let keys = keys_of(b"\x1b[\rq");
        assert!(
            keys.iter()
                .any(|key| key.key == Key::Named("Enter".into())
                    && key.state == ButtonState::Pressed),
            "{keys:?}"
        );
        assert!(
            keys.iter()
                .any(|key| key.key == Key::Character("q".into())
                    && key.state == ButtonState::Pressed),
            "{keys:?}"
        );
        // A known SS3 final is still that key, not Alt+O.
        let f1 = keys_of(b"\x1bOP");
        assert_eq!(f1[0].key, Key::Named("F1".into()));
    }

    /// DECKPAM sends the numpad as `SS3`. Numpad 1 is `ESC O q`, which must not
    /// be Alt+O and then `q` (`browser.quit`).
    #[test]
    fn application_keypad_ss3_is_a_keypad_key() {
        let digits = [
            (b'p', "0", 82u32),
            (b'q', "1", 79),
            (b'r', "2", 80),
            (b's', "3", 81),
            (b't', "4", 75),
            (b'u', "5", 76),
            (b'v', "6", 77),
            (b'w', "7", 71),
            (b'x', "8", 72),
            (b'y', "9", 73),
        ];
        for (byte, shown, code) in digits {
            let keys = keys_of(&[0x1b, b'O', byte]);
            assert_eq!(keys.len(), 2, "{byte} {keys:?}");
            assert_eq!(keys[0].key, Key::Character(shown.into()), "{byte}");
            assert_eq!(keys[0].code, KeyCode(code), "{byte}");
            assert_eq!(keys[0].text.as_deref(), Some(shown), "{byte}");
            assert!(!keys[0].modifiers.alt, "{byte}");
            assert_eq!(keys[0].state, ButtonState::Pressed);
            assert_eq!(keys[1].state, ButtonState::Released);
            assert_eq!(keys[1].code, KeyCode(code));
            assert!(keys[1].text.is_none());
        }
        let ops = [
            (b'j', "*", 55u32),
            (b'k', "+", 78),
            (b'l', ",", 121),
            (b'm', "-", 74),
            (b'n', ".", 83),
            (b'o', "/", 98),
            (b'X', "=", 117),
        ];
        for (byte, shown, code) in ops {
            let keys = keys_of(&[0x1b, b'O', byte]);
            assert_eq!(keys[0].key, Key::Character(shown.into()), "{byte} {keys:?}");
            assert_eq!(keys[0].code, KeyCode(code), "{byte}");
            assert_eq!(keys[0].text.as_deref(), Some(shown));
            assert!(!keys[0].modifiers.alt);
        }
        let enter = keys_of(b"\x1bOM");
        assert_eq!(enter[0].key, Key::Named("Enter".into()));
        assert_eq!(enter[0].code, KeyCode(96));
        assert!(enter[0].text.is_none());
        assert!(!enter[0].modifiers.alt);
        assert_eq!(enter[1].state, ButtonState::Released);
        assert_eq!(enter[1].code, KeyCode(96));
    }

    #[test]
    fn mouse_paste_and_replies() {
        let mouse = Parser::new().feed(b"\x1b[<0;3;4M");
        match &mouse[0] {
            Input::Mouse(event) => {
                assert_eq!(event.button, MouseButton::Left);
                assert_eq!((event.column, event.row), (3, 4));
                assert_eq!(event.state, ButtonState::Pressed);
            }
            other => panic!("{other:?}"),
        }
        let wheel = Parser::new().feed(b"\x1b[<65;1;1M");
        match &wheel[0] {
            Input::Mouse(event) => assert_eq!(event.wheel, Some(Wheel::Down)),
            other => panic!("{other:?}"),
        }
        let pasted = Parser::new().feed(b"\x1b[200~not a key \x1b[A\x1b[201~");
        assert_eq!(pasted, vec![Input::Paste("not a key \x1b[A".into())]);

        match reply(b"\x1b[?62;4c") {
            Reply::Da1 {
                sixel: true,
                params,
            } => assert!(params.contains(&4)),
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b[?31u") {
            Reply::KittyKeyboard { flags: 31 } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b_Gi=1;OK\x1b\\") {
            Reply::KittyGraphics { ok: true, .. } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1bP1+r524742=726762\x1b\\") {
            Reply::XtGetTcap {
                found: true,
                entries,
            } => {
                assert_eq!(entries, vec![("RGB".into(), Some("rgb".into()))]);
            }
            other => panic!("{other:?}"),
        }
        match reply(b"\x1bP0+r524742\x1b\\") {
            Reply::XtGetTcap { found: false, .. } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b]10;rgb:1111/2222/3333\x07") {
            Reply::OscColor {
                index: 10,
                bell: true,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\") {
            Reply::OscColor {
                index: 11,
                bell: false,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b[6;20;10t") {
            Reply::CellPixels {
                height: 20,
                width: 10,
            } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b[4;1080;1920t") {
            Reply::TextAreaPixels {
                height: 1080,
                width: 1920,
            } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b]52;c;aGVq\x1b\\") {
            Reply::Clipboard {
                selection: Some(b'c'),
                data,
                truncated: false,
            } => assert_eq!(data.as_deref(), Some(b"hej".as_slice())),
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b]52;p;aGVq\x1b\\") {
            Reply::Clipboard {
                selection: Some(b'p'),
                data,
                truncated: false,
            } => assert_eq!(data.as_deref(), Some(b"hej".as_slice())),
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b]52;c;?\x1b\\") {
            Reply::Clipboard {
                data: None,
                truncated: false,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b]52;c;\x1b\\") {
            Reply::Clipboard {
                data: None,
                truncated: false,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        match reply(b"\x1b]52;c;!!!!\x1b\\") {
            Reply::Clipboard {
                data: None,
                truncated: false,
                ..
            } => {}
            other => panic!("{other:?}"),
        }

        let focus = Parser::new().feed(b"\x1b[I\x1b[O");
        assert_eq!(focus, vec![Input::Focus(true), Input::Focus(false)]);
    }

    /// `YWFq` is base64 for `aaj`. The `q` would quit if the tail were keys.
    #[test]
    fn a_timeout_mid_osc_yields_no_key_events() {
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b]52;c;").is_empty());
        assert!(parser.expire().is_empty());
        // An arrow and a `q` sit in the discarded tail, before ST.
        let tail = parser.feed(b"YW\x1b[AqFq\x1b\\");
        assert!(
            tail.iter().all(|input| !matches!(input, Input::Key(_))),
            "{tail:?}"
        );
        match tail.as_slice() {
            [Input::Reply(Reply::Clipboard {
                selection: Some(b'c'),
                data: None,
                truncated: true,
            })] => {}
            other => panic!("{other:?}"),
        }
        let quit = parser.feed(b"q");
        assert!(
            matches!(
                &quit[0],
                Input::Key(key) if key.key == Key::Character("q".into())
                    && key.state == ButtonState::Pressed
            ),
            "{quit:?}"
        );

        // BEL ends an OSC the same way. A colour reply is not a clipboard event.
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b]10;rgb:").is_empty());
        assert!(parser.expire().is_empty());
        let colour = parser.feed(b"1111\x07");
        assert!(colour.is_empty(), "{colour:?}");

        // A bare introducer is still Alt plus that byte.
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b]").is_empty());
        let alt = parser.expire();
        assert!(
            matches!(
                &alt[0],
                Input::Key(key) if key.key == Key::Character("]".into()) && key.modifiers.alt
            ),
            "{alt:?}"
        );

        // DCS and APC tails are not keys either.
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1bP1+r").is_empty());
        assert!(parser.expire().is_empty());
        let dcs = parser.feed(b"q\x1b[A\x1b\\");
        assert!(
            dcs.iter().all(|input| !matches!(input, Input::Key(_))),
            "{dcs:?}"
        );
        let mut parser = Parser::new();
        assert!(parser.feed(b"\x1b_Gq=").is_empty());
        assert!(parser.expire().is_empty());
        let apc = parser.feed(b"q\x1b\\z");
        assert_eq!(apc.len(), 2, "{apc:?}");
        assert!(matches!(
            &apc[0],
            Input::Key(key) if key.key == Key::Character("z".into())
        ));
    }

    #[test]
    fn a_truncated_osc52_reply_is_a_failure() {
        let mut bytes = b"\x1b]52;c;".to_vec();
        bytes.extend(std::iter::repeat_n(b'A', MAX_PAYLOAD));
        bytes.extend_from_slice(b"\x1b\\q");
        let inputs = Parser::new().feed(&bytes);
        match inputs.as_slice() {
            [Input::Reply(Reply::Clipboard {
                selection: Some(b'c'),
                data: None,
                truncated: true,
            }), Input::Key(key), Input::Key(release)] => {
                assert_eq!(key.key, Key::Character("q".into()));
                assert_eq!(key.state, ButtonState::Pressed);
                assert_eq!(release.state, ButtonState::Released);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn control_suppresses_associated_text() {
        let keys = kitty_keys_of(b"\x1b[97;5;97u");
        assert!(keys[0].modifiers.control);
        assert!(keys[0].text.is_none());
        assert_eq!(keys[0].key, Key::Character("a".into()));
    }

    /// What Rio 0.5.28 sent after `CSI > 31 u`. Captured in a private sway: wtype held each
    /// key for 40 ms, and the query reply was `CSI ? 31 u`. A release often clears the
    /// modifier (`:3` with field 1). wtype's keymap reports Shift+J's text as "j".
    #[test]
    fn rio_recorded_sequences() {
        let owned = |s: &str| s.to_owned();
        let keys = kitty_keys_of(
            b"\x1b[111;1;111u\x1b[111;1:3u\x1b[97;1;97u\x1b[97;1:3u\x1b[103;1;103u\x1b[103;1:3u\
              \x1b[106;2;106u\x1b[106;1:3u\x1b[13u\x1b[13;1:3u\x1b[9u\x1b[9;1:3u\x1b[9;2u\x1b[9;1:3u\
              \x1b[127u\x1b[127;1:3u\x1b[27u\x1b[27;1:3u",
        );
        assert_eq!(
            summary(&keys),
            vec![
                (owned("o"), true, Some(owned("o"))),
                (owned("o"), false, None),
                (owned("a"), true, Some(owned("a"))),
                (owned("a"), false, None),
                (owned("g"), true, Some(owned("g"))),
                (owned("g"), false, None),
                (owned("j"), true, Some(owned("j"))),
                (owned("j"), false, None),
                (owned("Enter"), true, None),
                (owned("Enter"), false, None),
                (owned("Tab"), true, None),
                (owned("Tab"), false, None),
                (owned("Tab"), true, None),
                (owned("Tab"), false, None),
                (owned("Backspace"), true, None),
                (owned("Backspace"), false, None),
                (owned("Escape"), true, None),
                (owned("Escape"), false, None),
            ]
        );
        assert!(keys[6].modifiers.shift, "Shift+J");
        assert!(keys[12].modifiers.shift, "Shift+Tab");
        assert!(!keys[13].modifiers.shift, "the release clears Shift");
        assert_eq!(keys[8].code, KeyCode(28));
        assert!(keys.iter().all(|key| !key.ambiguous));

        let keys = kitty_keys_of(
            b"\x1b[D\x1b[1;1:3D\x1b[C\x1b[1;1:3C\x1b[A\x1b[1;1:3A\x1b[B\x1b[1;1:3B\
              \x1b[H\x1b[1;1:3H\x1b[F\x1b[1;1:3F\x1b[5~\x1b[5;1:3~\x1b[6~\x1b[6;1:3~\
              \x1b[3~\x1b[3;1:3~\x1b[P\x1b[1;1:3P\x1b[Q\x1b[1;1:3Q\x1b[13~\x1b[13;1:3~\
              \x1b[S\x1b[1;1:3S\x1b[15~\x1b[15;1:3~\x1b[24~\x1b[24;1:3~",
        );
        let names = [
            "ArrowLeft",
            "ArrowRight",
            "ArrowUp",
            "ArrowDown",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Delete",
            "F1",
            "F2",
            "F3",
            "F4",
            "F5",
            "F12",
        ];
        let expected: Vec<_> = names
            .iter()
            .flat_map(|name| [(owned(name), true, None), (owned(name), false, None)])
            .collect();
        assert_eq!(summary(&keys), expected);
        assert_eq!(keys[0].code, KeyCode(105), "Left has its evdev code");

        // Ctrl+L is a C0 press. The CSI release names no held press, so it adds nothing.
        let keys = kitty_keys_of(b"\x0c\x1b[108;1:3u");
        assert_eq!(
            summary(&keys),
            vec![(owned("l"), true, None), (owned("l"), false, None)]
        );
        assert!(keys[0].modifiers.control);

        // Ctrl+D is a CSI press, not EOF. Alt+1 types nothing. ':' and '+' carry no Shift.
        let keys = kitty_keys_of(
            b"\x1b[100;5u\x1b[100;1:3u\x1b[49;3;49u\x1b[49;1:3u\x1b[58;1;58u\x1b[58;1:3u\
              \x1b[43;1;43u\x1b[43;1:3u",
        );
        assert!(keys[0].modifiers.control && keys[0].text.is_none());
        assert_eq!(keys[0].key, Key::Character("d".into()));
        assert!(keys[2].modifiers.alt && keys[2].text.is_none());
        assert!(!keys[2].consumed.alt);
        assert_eq!(keys[3].key, Key::Character("1".into()));
        assert!(!keys[3].modifiers.alt, "the release clears Alt");
        assert_eq!(keys[4].text.as_deref(), Some(":"));
        assert!(!keys[4].modifiers.shift && !keys[4].consumed.shift);
        assert_eq!(keys[6].text.as_deref(), Some("+"));
        assert!(!keys[6].consumed.shift);
    }

    /// What kitty 0.49.2 sent after `CSI > 31 u`, same capture. Its base-layout field is
    /// 57344 for a key wtype's one-key keymap invented, which is not a US key.
    #[test]
    fn kitty_recorded_sequences() {
        let keys = kitty_keys_of(
            b"\x1b[111::57344;;111u\x1b[111::57344;1:3u\x1b[106::57344;2;106u\x1b[106::57344;1:3u\
              \x1b[13u\x1b[13;1:3u\x1b[9;2u\x1b[9;1:3u\x1b[127u\x1b[127;1:3u\
              \x1b[108::57344;5u\x1b[108::57344;1:3u\x1b[100::57344;5u\x1b[100::57344;1:3u\
              \x1b[49::57344;3u\x1b[49::57344;1:3u\x1b[58::57344;;58u\x1b[43::57344;;43u\
              \x1b[D\x1b[1;1:3D\x1b[24~\x1b[24;1:3~",
        );
        assert_eq!(keys[0].key, Key::Character("o".into()));
        assert_eq!(keys[0].text.as_deref(), Some("o"));
        assert_eq!(
            keys[0].code,
            KeyCode(24),
            "57344 is not a US key; o is KEY_O"
        );
        assert!(keys[2].modifiers.shift && keys[2].key == Key::Character("j".into()));
        assert!(!keys[3].modifiers.shift);
        assert_eq!(keys[4].key, Key::Named("Enter".into()));
        assert!(keys[6].modifiers.shift && keys[6].key == Key::Named("Tab".into()));
        assert_eq!(keys[8].key, Key::Named("Backspace".into()));
        assert!(keys[10].modifiers.control && keys[10].text.is_none());
        assert_eq!(keys[10].key, Key::Character("l".into()));
        assert!(keys[12].modifiers.control);
        assert_eq!(keys[12].key, Key::Character("d".into()));
        assert!(keys[14].modifiers.alt && keys[14].text.is_none());
        assert_eq!(keys[14].key, Key::Character("1".into()));
        assert_eq!(keys[16].text.as_deref(), Some(":"));
        assert_eq!(keys[17].text.as_deref(), Some("+"));
        assert!(!keys[16].consumed.shift && !keys[17].consumed.shift);
        assert_eq!(keys[18].key, Key::Named("ArrowLeft".into()));
        assert_eq!(keys[18].code, KeyCode(105));
        assert_eq!(keys[20].key, Key::Named("F12".into()));
        assert_eq!(summary(&keys).len(), 22);

        // A PUA functional key, the form the table is for, and å.
        let keys = kitty_keys_of(b"\x1b[57350u\x1b[57414u\x1b[229;1;229u");
        assert_eq!(keys[0].key, Key::Named("ArrowLeft".into()));
        assert_eq!(keys[0].code, KeyCode(105));
        assert_eq!(keys[1].key, Key::Named("Enter".into()));
        assert_eq!(keys[1].code, KeyCode(96));
        assert_eq!(keys[2].text.as_deref(), Some("å"));
    }

    #[test]
    fn a_release_without_its_press_is_dropped_and_repeats_are_held() {
        // Pressed before sig started, or a press the terminal lost.
        assert!(kitty_keys_of(b"\x1b[13;1:3u\x1b[1;1:3D").is_empty());
        let keys = kitty_keys_of(b"\x1b[1;1:2D\x1b[1;1:2D\x1b[1;1:3D");
        assert_eq!(keys.len(), 3);
        assert!(keys[0].repeat && keys[1].repeat);
        assert_eq!(keys[2].state, ButtonState::Released);
        assert!(!keys[2].repeat);
    }

    #[test]
    fn without_event_types_every_form_synthesizes_its_release() {
        // No reply to `CSI ? u`: the terminal may not send releases.
        let keys = keys_of(b"\x1b[13u\x1b[D\x1b[1;1:3D");
        assert_eq!(keys.len(), 4, "{keys:?}");
        assert_eq!(keys[1].state, ButtonState::Released);
        assert_eq!(keys[3].state, ButtonState::Released);
        assert_eq!(keys[2].code, KeyCode(0), "legacy keys have no evdev code");
    }

    #[test]
    fn legacy_upper_case_letters_are_shifted() {
        let keys = keys_of(b"J:");
        assert!(keys[0].modifiers.shift && keys[0].consumed.shift);
        assert_eq!(keys[0].text.as_deref(), Some("J"));
        assert!(!keys[2].modifiers.shift);
    }

    #[test]
    fn a_location_line_typed_in_one_read_keeps_every_key() {
        // `o`, then an address and Enter in the same read, as kitty sends them.
        let mut bytes = Vec::new();
        for ch in "ogoogle.com".chars() {
            let code = ch as u32;
            bytes.extend(format!("\x1b[{code};1;{code}u\x1b[{code};1:3u").bytes());
        }
        bytes.extend(b"\x1b[13u\x1b[13;1:3u");
        let keys = kitty_keys_of(&bytes);
        let typed: String = keys.iter().filter_map(|key| key.text.clone()).collect();
        assert_eq!(typed, "ogoogle.com");
        assert_eq!(keys.len(), 24);
        assert_eq!(keys[22].key, Key::Named("Enter".into()));
    }
}
