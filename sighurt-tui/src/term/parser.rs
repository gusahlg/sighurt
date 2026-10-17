//! What the terminal sends: keys, mouse, paste and capability replies.
//!
//! `feed` never sleeps. A lone Escape arms a 25 ms timer; any longer partial
//! sequence arms a 1 s timer. The owner loop sleeps for [`Parser::poll_timeout`]
//! and then calls [`Parser::expire`]. Calling `expire` means that timer has
//! fired. An unfinished CSI is not abandoned on the Escape timer, so a reply
//! that arrives in pieces is still a reply.
//!
//! Bracketed paste is collected until `CSI 201 ~` and is not parsed as keys.
//! Payloads are capped (64 KiB for APC/OSC/DCS, 1 MiB for paste).

use std::time::{Duration, Instant};

use super::keys::{kitty_key, kitty_modifiers, legacy_press, ButtonState, Key, KeyKind, Modifiers};

/// How long a lone Escape waits before it is a key.
pub const ESC_TIMEOUT: Duration = Duration::from_millis(25);

/// How long an unfinished sequence is kept before it is dropped.
pub const SEQ_TIMEOUT: Duration = Duration::from_secs(1);

const MAX_PAYLOAD: usize = 64 * 1024;
const MAX_PASTE: usize = 1024 * 1024;
const PASTE_END: &[u8] = b"\x1b[201~";
const MAX_PARAMS: usize = 16;
const MAX_SUBS: usize = 8;

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
        }
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
    /// reported as keys. Paste is left alone.
    pub fn expire(&mut self) -> Vec<Input> {
        if matches!(self.state, State::Paste { .. } | State::Ground) {
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
            State::Osc(buf) if buf.data.is_empty() => self.alt_char(b']', &mut out),
            State::Apc(buf) if buf.data.is_empty() => self.alt_char(b'_', &mut out),
            State::Dcs(dcs) if !dcs.csi.started && !dcs.in_data => self.alt_char(b'P', &mut out),
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
                self.state = State::Csi(take_csi(csi));
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
                        self.emit_named(name, modifiers, alt, out);
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
                self.emit_named(name, modifiers, alt, out);
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
        let kind = match event {
            2 => KeyKind::Repeat,
            3 => KeyKind::Release,
            _ => KeyKind::Press,
        };
        out.push(Input::Key(kitty_key(
            codepoint,
            shifted,
            base,
            kitty_modifiers(modifier_field.unwrap_or(1)),
            kind,
            &text_points,
            alt,
        )));
    }

    fn emit_named(
        &mut self,
        name: &'static str,
        modifiers: Modifiers,
        alt: bool,
        out: &mut Vec<Input>,
    ) {
        let mut modifiers = modifiers;
        if alt {
            modifiers.alt = true;
        }
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
            _ => return,
        };
        self.emit_named(name, Modifiers::default(), alt, out);
    }

    fn osc(&mut self, byte: u8, buf: &mut Buf, out: &mut Vec<Input>) {
        match byte {
            0x07 => {
                self.disarm();
                self.prefix_alt = false;
                dispatch_osc(&buf.data, true, out);
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
                dispatch_osc(&buf.data, false, out);
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
        if data.len() >= MAX_PASTE {
            out.push(Input::Paste(String::from_utf8_lossy(&data).into_owned()));
            self.disarm();
            self.byte(byte, out);
            return;
        }
        data.extend_from_slice(&PASTE_END[..matched]);
        let restart = byte == PASTE_END[0];
        if !restart {
            if data.len() >= MAX_PASTE {
                out.push(Input::Paste(String::from_utf8_lossy(&data).into_owned()));
                self.disarm();
                self.byte(byte, out);
                return;
            }
            data.push(byte);
        }
        self.state = State::Paste {
            data,
            matched: usize::from(restart),
        };
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

fn legacy_final(final_byte: u8, csi: &Csi) -> Option<&'static str> {
    Some(match final_byte {
        b'A' => "ArrowUp",
        b'B' => "ArrowDown",
        b'C' => "ArrowRight",
        b'D' => "ArrowLeft",
        b'H' => "Home",
        b'F' => "End",
        b'Z' => "Tab",
        b'P' | b'Q' | b'R' | b'S' if !csi.params.is_empty() => match final_byte {
            b'P' => "F1",
            b'Q' => "F2",
            b'R' => "F3",
            b'S' => "F4",
            _ => return None,
        },
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

fn dispatch_osc(payload: &[u8], bell: bool, out: &mut Vec<Input>) {
    let text = String::from_utf8_lossy(payload);
    let Some((index, spec)) = text.split_once(';') else {
        return;
    };
    let Ok(index) = index.parse::<u16>() else {
        return;
    };
    if index == 10 || index == 11 {
        out.push(Input::Reply(Reply::OscColor {
            index,
            spec: spec.to_string(),
            bell,
        }));
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

    fn reply(bytes: &[u8]) -> Reply {
        match Parser::new().feed(bytes).as_slice() {
            [Input::Reply(reply)] => reply.clone(),
            other => panic!("expected one reply, got {other:?}"),
        }
    }

    #[test]
    fn kitty_shift_a_uses_the_base_layout_and_the_text() {
        let keys = keys_of(b"\x1b[97:65:97;2:1;65u");
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
        let release = keys_of(b"\x1b[97:65:97;2:3u");
        assert_eq!(release.len(), 1);
        assert_eq!(release[0].state, ButtonState::Released);
        assert!(release[0].text.is_none());
        let repeat = keys_of(b"\x1b[97::97;1:2u");
        assert!(repeat[0].repeat && repeat[0].state == ButtonState::Pressed);
        assert_eq!(repeat[0].code, KeyCode(30));
    }

    #[test]
    fn emoji_without_a_base_layout_has_no_evdev_code() {
        let keys = keys_of(b"\x1b[128512u");
        assert_eq!(keys[0].key, Key::Character("😀".into()));
        assert_eq!(keys[0].code, KeyCode(0));
        let with_base = keys_of(b"\x1b[128512:97:97;1:1;128512u");
        assert_eq!(with_base[0].code, KeyCode(30));
        assert_eq!(with_base[0].text.as_deref(), Some("😀"));
    }

    #[test]
    fn tab_and_ctrl_i_stay_distinct_under_kitty() {
        let tab = keys_of(b"\x1b[9u");
        assert_eq!(tab.len(), 1);
        assert_eq!(tab[0].key, Key::Named("Tab".into()));
        assert_eq!(tab[0].code, KeyCode(15));
        assert!(!tab[0].ambiguous);
        let ctrl_i = keys_of(b"\x1b[105;5u");
        assert_eq!(ctrl_i[0].key, Key::Character("i".into()));
        assert!(ctrl_i[0].modifiers.control);
        assert_eq!(ctrl_i[0].code, KeyCode(0));
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

        let focus = Parser::new().feed(b"\x1b[I\x1b[O");
        assert_eq!(focus, vec![Input::Focus(true), Input::Focus(false)]);
    }

    #[test]
    fn control_suppresses_associated_text() {
        let keys = keys_of(b"\x1b[97;5;97u");
        assert!(keys[0].modifiers.control);
        assert!(keys[0].text.is_none());
        assert_eq!(keys[0].key, Key::Character("a".into()));
    }
}
