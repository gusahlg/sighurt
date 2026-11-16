//! A bounded non-blocking writer.
//!
//! The terminal's reader can stop. Writes never wait for it: at most `cap`
//! unsent bytes sit in `pending`, and a frame that does not fit is dropped so
//! the newest frame can take its place. The queue is read from an offset and
//! compacted once that offset passes half the buffer, so a drain does not
//! slide the tail on every write.
//!
//! A clipboard sequence is not a frame. [`Output::write_reliable`] queues one
//! only when it fits in the cap, and refuses it without writing or queueing
//! when it does not. A later frame drop keeps those bytes whole. A newer
//! reliable sequence replaces one that is still wholly unsent.
//!
//! Bytes already handed to the kernel are tracked with [`Wire`]: where the
//! reader's parser is. Inside tmux that reader is tmux, whose DCS is not a
//! terminal's: an `ESC` inside it is the first of a pair (`ESC ESC` is one
//! `ESC` of the passthrough's content, `ESC \` ends it), and `CAN` is content,
//! not a cancel. A passthrough (`ESC P tmux ;`) is tracked twice: tmux's parser,
//! and the outer terminal's, which gets the content (doubled `ESC`s single
//! again) when tmux reads the end. The only passthrough `sig` writes is kitty
//! graphics, so ending a cut one also ends the kitty upload it may have begun.
//!
//! Every abort goes through one path, [`Output::abort`] (resize, leaving, a
//! frame dropped) and [`terminator`] (the fatal-signal and suspend restore,
//! which sees only the published [`Wire`]). Unsent bytes are dropped; an open
//! sequence is finished from the queue when its end is there and it is short
//! (not a DCS or a passthrough), and otherwise closed with [`terminator`]. What
//! an abort leaves in the queue is recovery and is never dropped by a later
//! abort: a second resize under backpressure keeps the first one's repair.
//!
//! A write is split so the state published *before* the syscall is one whose
//! [`terminator`] is right whichever prefix of that write the kernel accepts:
//! a fatal handler that runs during the syscall closes the sequence whether or
//! not the write's last bytes landed. The session stores that classification
//! before it calls `write`.

use std::io;
#[cfg(unix)]
use std::os::fd::AsFd;

#[cfg(unix)]
use rustix::io::Errno;

/// Where the queue's bytes go: a non-blocking write that takes what it can now. `Ok(0)` means
/// nothing fits yet (`EAGAIN`). On Unix, any descriptor; on Windows, the console's writer
/// thread (`console.rs`).
pub trait Sink {
    /// Writes a prefix of `buf` without blocking and returns its length.
    fn write_some(&self, buf: &[u8]) -> io::Result<usize>;
}

#[cfg(unix)]
impl<T: AsFd> Sink for T {
    fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
        write_partial(self, buf)
    }
}

#[cfg(not(unix))]
impl<S: Sink + ?Sized> Sink for &S {
    fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
        (**self).write_some(buf)
    }
}

/// The parser of whatever reads the bytes `sig` writes: tmux in a tmux pane, the terminal
/// otherwise. tmux's tables (3.8's `input.c`) where they differ from a terminal's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum Outer {
    /// Not inside a control sequence.
    Ground = 0,
    /// Saw `ESC`.
    Esc,
    /// `ESC` followed by intermediates.
    EscInt,
    /// Inside `CSI`.
    Csi,
    /// Inside `OSC`.
    Osc,
    /// Inside `APC`, `SOS` or `PM`.
    Str,
    /// Inside a string and the previous byte was `ESC` (an `ST` may finish).
    StrEsc,
    /// Inside a `DCS` that is not tmux's passthrough (a sixel).
    Dcs,
    /// Inside a `DCS` after `ESC`: `\` ends it, tmux keeps any other byte.
    DcsEsc,
    /// `ESC P`: tmux's `dcs_enter`.
    Pre0,
    /// `ESC P t`, `tm`, `tmu`, `tmux`: maybe the passthrough's prefix.
    Pre1,
    Pre2,
    Pre3,
    Pre4,
    /// Inside tmux's passthrough, after `ESC P tmux ;`.
    Pass,
    /// Inside the passthrough after `ESC`: `\` ends it, `ESC` is a content `ESC`.
    PassEsc,
    /// Published only, for the `\` that ends a passthrough: it landed or it did not.
    Closing,
    /// [`Outer::Closing`] and then `CAN`: the passthrough is open still, or ended.
    Closing1,
    /// [`Outer::Closing1`] and then `ESC`.
    Closing2,
}

const OUTERS: [Outer; 19] = [
    Outer::Ground,
    Outer::Esc,
    Outer::EscInt,
    Outer::Csi,
    Outer::Osc,
    Outer::Str,
    Outer::StrEsc,
    Outer::Dcs,
    Outer::DcsEsc,
    Outer::Pre0,
    Outer::Pre1,
    Outer::Pre2,
    Outer::Pre3,
    Outer::Pre4,
    Outer::Pass,
    Outer::PassEsc,
    Outer::Closing,
    Outer::Closing1,
    Outer::Closing2,
];

/// The outer terminal's parser, as of the passthrough content tmux hands it: what a cut
/// passthrough leaves the terminal in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum Inner {
    Ground = 0,
    Esc,
    EscInt,
    Csi,
    Osc,
    /// `APC` (a kitty command), `SOS`, `PM` or `DCS`.
    Str,
    StrEsc,
}

const INNERS: [Inner; 7] = [
    Inner::Ground,
    Inner::Esc,
    Inner::EscInt,
    Inner::Csi,
    Inner::Osc,
    Inner::Str,
    Inner::StrEsc,
];

/// Where a terminal parser is, given only the bytes already written: tmux's (or the
/// terminal's) and, through tmux's passthrough, the outer terminal's.
///
/// The signal path reads the same classification from shared state, one byte. It must
/// not look at [`Output`]'s buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wire {
    outer: Outer,
    inner: Inner,
}

impl Wire {
    /// Not inside any sequence, tmux's or the outer terminal's.
    pub const GROUND: Wire = Wire {
        outer: Outer::Ground,
        inner: Inner::Ground,
    };

    /// The value stored beside the session for the signal path.
    pub fn from_u8(value: u8) -> Self {
        Self {
            outer: OUTERS
                .get(usize::from(value & 0x1f))
                .copied()
                .unwrap_or(Outer::Ground),
            inner: INNERS
                .get(usize::from(value >> 5))
                .copied()
                .unwrap_or(Inner::Ground),
        }
    }

    /// The byte [`Self::from_u8`] reads back.
    pub fn as_u8(self) -> u8 {
        self.outer as u8 | (self.inner as u8) << 5
    }

    /// Nothing is open: no terminator is needed.
    pub fn is_ground(self) -> bool {
        self == Self::GROUND
    }

    fn with(self, outer: Outer) -> Self {
        Self { outer, ..self }
    }

    /// Inside a DCS or a passthrough, or the outer terminal is inside a cut passthrough's
    /// content: an abort closes it and does not finish it from the queue.
    fn in_string_of_tmux(self) -> bool {
        self.inner != Inner::Ground
            || matches!(
                self.outer,
                Outer::Dcs
                    | Outer::DcsEsc
                    | Outer::Pre0
                    | Outer::Pre1
                    | Outer::Pre2
                    | Outer::Pre3
                    | Outer::Pre4
                    | Outer::Pass
                    | Outer::PassEsc
                    | Outer::Closing
                    | Outer::Closing1
                    | Outer::Closing2
            )
    }

    /// The outer terminal may have been handed a cut passthrough: closing needs a recovery
    /// passthrough with the end of a kitty upload.
    fn needs_upload_end(self) -> bool {
        self.inner != Inner::Ground
            || matches!(
                self.outer,
                Outer::Pass | Outer::PassEsc | Outer::Closing | Outer::Closing1 | Outer::Closing2
            )
    }

    /// The exact states a published-only state stands for. One for every other state.
    fn members(self) -> ([Wire; 2], usize) {
        let i = self.inner;
        let after_can = step_inner(i, CAN);
        match self.outer {
            Outer::Closing => (
                [
                    Wire {
                        outer: Outer::PassEsc,
                        inner: i,
                    },
                    Wire::GROUND.inner_of(i),
                ],
                2,
            ),
            Outer::Closing1 => (
                [
                    Wire {
                        outer: Outer::Pass,
                        inner: after_can,
                    },
                    Wire::GROUND.inner_of(i),
                ],
                2,
            ),
            Outer::Closing2 => (
                [
                    Wire {
                        outer: Outer::PassEsc,
                        inner: after_can,
                    },
                    Wire {
                        outer: Outer::Esc,
                        inner: i,
                    },
                ],
                2,
            ),
            _ => ([self, self], 1),
        }
    }

    fn inner_of(self, inner: Inner) -> Self {
        Self { inner, ..self }
    }
}

const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const ESC: u8 = 0x1b;

/// What opens tmux's passthrough.
const PASS_OPEN: &[u8] = b"\x1bPtmux;";
/// An empty last chunk: ends a kitty upload a cut left waiting for its `m=0`. Harmless when
/// no upload was open (a transmit of nothing, with no reply).
const UPLOAD_END: &[u8] = b"\x1b_Gq=2,m=0;\x1b\\";

/// The bytes that leave a [`Wire`]: at most 40, on the stack, so the signal path can build
/// them without allocating.
#[derive(Clone, Copy, Debug)]
pub struct Terminator {
    bytes: [u8; 40],
    len: usize,
}

impl Terminator {
    /// The bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// Nothing to write: already at ground.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, bytes: &[u8]) {
        self.bytes[self.len..self.len + bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
    }

    /// `bytes` as passthrough content: every `ESC` doubled.
    fn push_doubled(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == ESC {
                self.push(&[ESC]);
            }
            self.push(&[byte]);
        }
    }

    /// Inside a passthrough (or a DCS) whose last byte may have been an unpaired `ESC`: a
    /// `CAN`, which tmux keeps as content either way (dropping that `ESC`), then what ends
    /// the outer terminal's sequence and its kitty upload, then the passthrough's end.
    fn push_pass_end(&mut self, inner: Inner) {
        self.push(&[CAN]);
        // After `CAN` a string may still be open (a terminal that takes `CAN` as text in a
        // string); `ST` ends it, and is nothing where `CAN` did.
        if inner_is_string(inner) {
            self.push_doubled(b"\x1b\\");
        }
        self.push_doubled(UPLOAD_END);
        self.push(b"\x1b\\");
    }
}

fn inner_is_string(inner: Inner) -> bool {
    matches!(inner, Inner::Osc | Inner::Str | Inner::StrEsc)
}

/// Bytes that leave `wire` if nothing else will. Empty when already at ground.
///
/// `APC` and `OSC` end with `ST`, a half-written `ST` with `\`. `CSI` and a bare `ESC` are
/// cancelled. A DCS gets `CAN` and `ST`: tmux keeps the `CAN` as content whether or not an
/// `ESC` before it landed, so the `ST` after it is a real one either way. A passthrough is
/// ended the same way, and inside it first the outer terminal's sequence and the kitty
/// upload are ended, so tmux hands the outer terminal the cut content and its end at once.
pub fn terminator(wire: Wire) -> Terminator {
    let mut out = Terminator {
        bytes: [0; 40],
        len: 0,
    };
    let fresh = |out: &mut Terminator, inner: Inner| {
        out.push(PASS_OPEN);
        out.push_pass_end(inner);
    };
    match wire.outer {
        Outer::Ground => {}
        Outer::Esc | Outer::EscInt | Outer::Csi => out.push(&[CAN]),
        Outer::Osc | Outer::Str => out.push(b"\x1b\\"),
        Outer::StrEsc => out.push(b"\\"),
        Outer::Dcs
        | Outer::DcsEsc
        | Outer::Pre0
        | Outer::Pre1
        | Outer::Pre2
        | Outer::Pre3
        | Outer::Pre4 => out.push(b"\x18\x1b\\"),
        Outer::Pass | Outer::PassEsc => {
            out.push_pass_end(wire.inner);
            return out;
        }
        Outer::Closing => {
            out.push(b"\x18\x1b\\");
            fresh(&mut out, wire.inner);
            return out;
        }
        Outer::Closing1 => {
            out.push(b"\x1b\\");
            fresh(&mut out, wire.inner);
            return out;
        }
        Outer::Closing2 => {
            out.push(b"\\");
            fresh(&mut out, wire.inner);
            return out;
        }
    }
    if wire.inner != Inner::Ground {
        fresh(&mut out, wire.inner);
    }
    out
}

/// `wire` after `bytes` have been written.
pub fn advance(mut wire: Wire, bytes: &[u8]) -> Wire {
    for &byte in bytes {
        wire = step(wire, byte);
    }
    wire
}

fn step(wire: Wire, byte: u8) -> Wire {
    let to = |outer| wire.with(outer);
    match wire.outer {
        Outer::Ground => match byte {
            ESC => to(Outer::Esc),
            _ => to(Outer::Ground),
        },
        Outer::Esc => match byte {
            CAN | SUB => to(Outer::Ground),
            ESC => to(Outer::Esc),
            b'[' => to(Outer::Csi),
            b']' => to(Outer::Osc),
            b'_' | b'X' | b'^' => to(Outer::Str),
            b'P' => to(Outer::Pre0),
            0x20..=0x2f => to(Outer::EscInt),
            0x30..=0x7e => to(Outer::Ground),
            _ => to(Outer::Esc),
        },
        Outer::EscInt => match byte {
            CAN | SUB => to(Outer::Ground),
            ESC => to(Outer::Esc),
            0x20..=0x2f => to(Outer::EscInt),
            0x30..=0x7e => to(Outer::Ground),
            _ => to(Outer::EscInt),
        },
        Outer::Csi => match byte {
            CAN | SUB => to(Outer::Ground),
            ESC => to(Outer::Esc),
            0x40..=0x7e => to(Outer::Ground),
            _ => to(Outer::Csi),
        },
        // A string is ended by `ST` (or `BEL` for OSC). `CAN` is taken as text here: tmux
        // and most terminals cancel on it, but a terminator that relies on `ST` is right
        // either way.
        Outer::Osc => match byte {
            0x07 => to(Outer::Ground),
            ESC => to(Outer::StrEsc),
            _ => to(Outer::Osc),
        },
        Outer::Str => match byte {
            ESC => to(Outer::StrEsc),
            _ => to(Outer::Str),
        },
        Outer::StrEsc => match byte {
            b'\\' => to(Outer::Ground),
            _ => step(to(Outer::Esc), byte),
        },
        // tmux's `dcs_enter` (and parameters), where `CAN` and `ESC` still abort. Taken as
        // the DCS body here: a terminal that keeps `CAN` in a string still has it open, and
        // the body's terminator is harmless where it was aborted.
        Outer::Pre0 => match byte {
            ESC => to(Outer::DcsEsc),
            b't' => to(Outer::Pre1),
            _ => to(Outer::Dcs),
        },
        // tmux's `dcs_handler`: everything is content but `ESC`.
        Outer::Pre1 | Outer::Pre2 | Outer::Pre3 | Outer::Pre4 | Outer::Dcs => {
            match (wire.outer, byte) {
                (_, ESC) => to(Outer::DcsEsc),
                (Outer::Pre1, b'm') => to(Outer::Pre2),
                (Outer::Pre2, b'u') => to(Outer::Pre3),
                (Outer::Pre3, b'x') => to(Outer::Pre4),
                (Outer::Pre4, b';') => to(Outer::Pass),
                _ => to(Outer::Dcs),
            }
        }
        // tmux's `dcs_escape`: `\` ends the DCS, any other byte is content (the `ESC` is not).
        Outer::DcsEsc => match byte {
            b'\\' => to(Outer::Ground),
            _ => to(Outer::Dcs),
        },
        Outer::Pass => match byte {
            ESC => to(Outer::PassEsc),
            _ => Wire {
                outer: Outer::Pass,
                inner: step_inner(wire.inner, byte),
            },
        },
        // tmux hands the content to the outer terminal now; the inner state is the terminal's.
        Outer::PassEsc => match byte {
            b'\\' => to(Outer::Ground),
            _ => Wire {
                outer: Outer::Pass,
                inner: step_inner(wire.inner, byte),
            },
        },
        // Only [`terminator`]'s bytes follow these. Anything else keeps the published state
        // that covers the most ([`Outer::Closing`]).
        Outer::Closing => match byte {
            CAN => to(Outer::Closing1),
            _ => unexpected(wire),
        },
        Outer::Closing1 => match byte {
            ESC => to(Outer::Closing2),
            _ => unexpected(wire),
        },
        Outer::Closing2 => match byte {
            b'\\' => to(Outer::Ground),
            _ => unexpected(wire),
        },
    }
}

/// A byte no writer sends from `wire`. Debug builds say so; release keeps the widest cover.
fn unexpected(wire: Wire) -> Wire {
    debug_assert!(false, "unexpected byte after {wire:?}");
    Wire {
        outer: Outer::Closing,
        inner: Inner::Str,
    }
}

/// The outer terminal's parser on passthrough content.
fn step_inner(inner: Inner, byte: u8) -> Inner {
    match inner {
        Inner::Ground => match byte {
            ESC => Inner::Esc,
            _ => Inner::Ground,
        },
        Inner::Esc => match byte {
            CAN | SUB => Inner::Ground,
            ESC => Inner::Esc,
            b'[' => Inner::Csi,
            b']' => Inner::Osc,
            b'_' | b'X' | b'^' | b'P' => Inner::Str,
            0x20..=0x2f => Inner::EscInt,
            0x30..=0x7e => Inner::Ground,
            _ => Inner::Esc,
        },
        Inner::EscInt => match byte {
            CAN | SUB => Inner::Ground,
            ESC => Inner::Esc,
            0x20..=0x2f => Inner::EscInt,
            0x30..=0x7e => Inner::Ground,
            _ => Inner::EscInt,
        },
        Inner::Csi => match byte {
            CAN | SUB => Inner::Ground,
            ESC => Inner::Esc,
            0x40..=0x7e => Inner::Ground,
            _ => Inner::Csi,
        },
        Inner::Osc => match byte {
            0x07 => Inner::Ground,
            ESC => Inner::StrEsc,
            _ => Inner::Osc,
        },
        Inner::Str => match byte {
            ESC => Inner::StrEsc,
            _ => Inner::Str,
        },
        Inner::StrEsc => match byte {
            b'\\' => Inner::Ground,
            _ => step_inner(Inner::Esc, byte),
        },
    }
}

/// Whether [`terminator`] of `publish` is right for `state`: it leaves `state` at ground and,
/// once at ground, writes nothing but `CAN`, `ST` or a recovery passthrough (no text, no other
/// sequence). Where the outer terminal may have a cut passthrough, `publish`'s terminator must
/// end its upload too, from a string as well when `state`'s content is in one.
fn covers(publish: Wire, state: Wire) -> bool {
    if publish == state {
        return true;
    }
    let (members, count) = state.members();
    members[..count]
        .iter()
        .all(|&member| covers_exact(publish, member))
}

fn covers_exact(publish: Wire, state: Wire) -> bool {
    if state.needs_upload_end()
        && !(publish.needs_upload_end()
            && (inner_is_string(publish.inner) || !inner_is_string(state.inner)))
    {
        return false;
    }
    let term = terminator(publish);
    let mut wire = state;
    let mut rest = term.as_bytes();
    // `ESC P` alone: tmux aborts it on `CAN`, a terminal that keeps `CAN` in a string does
    // not. Only an `ST` (after a `CAN` or not) ends it for both.
    if state.outer == Outer::Pre0 {
        let Some(after) = rest
            .strip_prefix(b"\x18\x1b\\")
            .or_else(|| rest.strip_prefix(b"\x1b\\"))
        else {
            return false;
        };
        wire = Wire::GROUND.inner_of(state.inner);
        rest = after;
    }
    while !rest.is_empty() {
        if wire.outer == Outer::Ground {
            if let Some(after) = rest.strip_prefix(PASS_OPEN) {
                wire = advance(wire, PASS_OPEN);
                rest = after;
                continue;
            }
            match rest {
                [CAN | SUB, after @ ..] => rest = after,
                [ESC, b'\\', after @ ..] => rest = after,
                _ => return false,
            }
            continue;
        }
        wire = step(wire, rest[0]);
        rest = &rest[1..];
    }
    wire.is_ground()
}

/// The state to publish for one byte when neither the state before it nor the one after it
/// covers the other: the `\` of a string's `ST` (published as the string, whose whole `ST`
/// is right whether or not that `\` landed), the `\` that ends a passthrough.
fn join(before: Wire, after: Wire) -> Wire {
    let candidates = [
        before.with(Outer::Str),
        before.with(Outer::Dcs),
        before.with(Outer::Closing),
    ];
    for candidate in candidates {
        if covers(candidate, before) && covers(candidate, after) {
            return candidate;
        }
    }
    debug_assert!(false, "no state covers {before:?} and {after:?}");
    after
}

/// The distinct states one write can leave, so a new publish state is checked against all.
struct Seen {
    states: [Wire; 8],
    len: usize,
}

impl Seen {
    fn contains(&self, wire: Wire) -> bool {
        self.states[..self.len].contains(&wire)
    }

    fn all(&self, f: impl Fn(Wire) -> bool) -> bool {
        self.states[..self.len].iter().all(|&state| f(state))
    }

    /// `false` when full: the write ends after this byte.
    fn push(&mut self, wire: Wire) -> bool {
        if self.len == self.states.len() {
            return false;
        }
        self.states[self.len] = wire;
        self.len += 1;
        self.len < self.states.len()
    }
}

/// How many leading bytes of `bytes` one `write` may send, and the [`Wire`]
/// to publish before that syscall.
///
/// The published state's [`terminator`] is right for every state the write can leave the
/// wire in, from no byte accepted to all of them ([`covers`]). A write ends before the byte
/// whose state the published one does not cover and that does not cover the earlier ones.
/// A single such byte is published as a state that covers both sides of it.
pub(super) fn plan_write(start: Wire, bytes: &[u8]) -> (usize, Wire) {
    if bytes.is_empty() {
        return (0, start);
    }
    let mut publish = start;
    let mut seen = Seen {
        states: [start; 8],
        len: 1,
    };
    let mut wire = start;
    for (index, &byte) in bytes.iter().enumerate() {
        let next = step(wire, byte);
        if next != wire && !seen.contains(next) {
            if !covers(publish, next) {
                if seen.all(|state| covers(next, state)) {
                    publish = next;
                } else if index > 0 {
                    return (index, publish);
                } else {
                    publish = join(start, next);
                }
            }
            if !seen.push(next) {
                return (index + 1, publish);
            }
        }
        wire = next;
    }
    (bytes.len(), publish)
}

/// How many leading bytes finish the open sequence. `None` when they do not.
fn bytes_until_ground(wire: Wire, bytes: &[u8]) -> Option<usize> {
    if wire.is_ground() {
        return Some(0);
    }
    let mut wire = wire;
    for (index, &byte) in bytes.iter().enumerate() {
        wire = step(wire, byte);
        if wire.is_ground() {
            return Some(index + 1);
        }
    }
    None
}

/// Bytes waiting to be written, and how many frames were discarded.
#[derive(Debug)]
pub struct Output {
    pending: Vec<u8>,
    /// First byte not yet accepted by `write`. Earlier bytes stay until compact.
    pending_at: usize,
    cap: usize,
    /// Unsent bytes at the front of the pending tail that must be written whole.
    /// A frame drop keeps them. Zero when the tail has none.
    reliable: usize,
    /// Unsent bytes at the front that an abort left: the end of the cut sequence and what
    /// it was asked to add. No later abort drops them and no reliable write replaces them.
    sticky: usize,
    drops: u64,
    /// Parser state of bytes already accepted by `write`.
    wire: Wire,
    /// Last `wire_gen` applied from the signal path.
    #[cfg_attr(not(unix), allow(dead_code))]
    seen_gen: u64,
    /// Bytes [`Self::compact`] moved. A quadratic drain moves far more than the frame.
    #[cfg(test)]
    shifted: u64,
}

impl Output {
    /// `cap` is the most bytes kept when the kernel will not take them.
    pub fn new(cap: usize) -> Self {
        Self {
            pending: Vec::new(),
            pending_at: 0,
            cap,
            reliable: 0,
            sticky: 0,
            drops: 0,
            wire: Wire::GROUND,
            seen_gen: 0,
            #[cfg(test)]
            shifted: 0,
        }
    }

    /// Bytes still waiting.
    pub fn pending_len(&self) -> usize {
        self.pending.len() - self.pending_at
    }

    /// The most unsent bytes this queue keeps.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Frames discarded because they did not fit in the cap.
    pub fn drops(&self) -> u64 {
        self.drops
    }

    /// Wire byte stored for the signal path.
    pub fn wire_byte(&self) -> u8 {
        self.wire.as_u8()
    }

    // Only the Unix session's fatal-signal path uses it.
    #[cfg_attr(not(unix), allow(dead_code))]
    /// A signal handler moved the terminal on its own.
    ///
    /// The queued tail belonged to the sequence it aborted, so it is dropped.
    /// Returns whether the handler had run since the last sync.
    pub fn sync_handler(&mut self, wire: u8, gen: u64) -> bool {
        if gen == self.seen_gen {
            return false;
        }
        self.wire = Wire::from_u8(wire);
        self.seen_gen = gen;
        self.pending.clear();
        self.pending_at = 0;
        self.reliable = 0;
        self.sticky = 0;
        true
    }

    // Only the Unix session's fatal-signal path uses it.
    #[cfg_attr(not(unix), allow(dead_code))]
    /// Account for `bytes` that were just written.
    pub fn advance_wire(&mut self, bytes: &[u8]) {
        self.wire = advance(self.wire, bytes);
    }

    // Only the Unix session's fatal-signal path uses it.
    #[cfg_attr(not(unix), allow(dead_code))]
    /// Put `bytes`, the unwritten rest of [`Self::take_inflight`], back in front of the
    /// queue. They are written whole before anything else: used when a restore write stops
    /// early.
    pub fn prepend(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.reclaim_spent();
        let mut combined = Vec::with_capacity(bytes.len() + self.pending_len());
        combined.extend_from_slice(bytes);
        combined.extend_from_slice(&self.pending[self.pending_at..]);
        self.pending = combined;
        self.pending_at = 0;
        self.sticky = self
            .sticky
            .saturating_add(bytes.len())
            .min(self.pending_len());
    }

    /// [`Self::abort`] with nothing to add.
    pub fn discard_to_boundary(&mut self) {
        self.abort(&[]);
    }

    /// Drop everything that has not started on the wire, and leave the terminal (and, through
    /// tmux, the outer terminal) at ground once what stays is written; then `extra`.
    ///
    /// Bytes an earlier abort left, and a reliable sequence, are kept whole. An open sequence
    /// is then finished from the queue when its end is queued and it is not a DCS or a
    /// passthrough (a kitty APC chunk, a CSI, an OSC), and otherwise closed with
    /// [`terminator`]: a sixel is one DCS, and a truncated sixel is valid, so the rest of its
    /// body is not kept; a cut kitty transfer through tmux is ended inside its passthrough.
    /// What is left, `extra` included, is never dropped by a later abort.
    pub fn abort(&mut self, extra: &[u8]) {
        self.reclaim_spent();
        let unsent = self.pending_len();
        let protect = self.reliable.max(self.sticky).min(unsent);
        let start = self.pending_at;
        let after = advance(self.wire, &self.pending[start..start + protect]);
        let mut keep = protect;
        let mut term = terminator(Wire::GROUND);
        if !after.is_ground() {
            let tail = (protect == 0 && !after.in_string_of_tmux())
                .then(|| bytes_until_ground(after, &self.pending[start..]))
                .flatten();
            match tail {
                Some(end) => keep = end,
                None => term = terminator(after),
            }
        }
        self.pending.truncate(start + keep);
        self.pending.extend_from_slice(term.as_bytes());
        self.pending.extend_from_slice(extra);
        self.compact();
        if keep > protect || !term.is_empty() || !extra.is_empty() {
            self.sticky = self.pending_len();
        } else {
            self.sticky = self.sticky.min(self.pending_len());
        }
        self.reliable = self.reliable.min(self.pending_len());
    }

    // Only the Unix session's fatal-signal path uses it.
    #[cfg_attr(not(unix), allow(dead_code))]
    /// [`Self::discard_to_boundary`] and take those bytes. The caller writes them, whole:
    /// all of them are recovery or a reliable sequence. [`Self::prepend`] puts back what a
    /// write did not take.
    pub fn take_inflight(&mut self) -> Vec<u8> {
        self.discard_to_boundary();
        self.compact();
        self.pending_at = 0;
        self.reliable = 0;
        self.sticky = 0;
        std::mem::take(&mut self.pending)
    }

    /// Write `bytes` whole, or write nothing.
    ///
    /// `Ok(false)` means `bytes` do not fit in the cap, or the queue holds
    /// something this must not replace (a frame, recovery, or a reliable sequence that
    /// has already started on the wire). The queue is unchanged then.
    /// `Ok(true)` means `bytes` were written or queued whole. A wholly unsent
    /// reliable sequence already queued is replaced.
    pub fn write_reliable<Fd, P>(
        &mut self,
        fd: Fd,
        bytes: &[u8],
        mut publish: P,
    ) -> io::Result<bool>
    where
        Fd: Sink,
        P: FnMut(Wire) -> bool,
    {
        if bytes.is_empty() {
            return Ok(true);
        }
        if bytes.len() > self.cap {
            return Ok(false);
        }
        self.reclaim_spent();
        if self.reliable > self.pending_len() {
            self.reliable = self.pending_len();
        }
        let replace = self.wire.is_ground()
            && self.sticky == 0
            && self.reliable > 0
            && self.reliable == self.pending_len();
        if self.pending_len() > 0 || !self.wire.is_ground() {
            if !replace {
                return Ok(false);
            }
            self.pending.clear();
            self.pending_at = 0;
            self.reliable = 0;
        }
        let wrote = self.write_span(&fd, bytes, &mut publish)?;
        if wrote.handoff {
            self.reliable = 0;
            return Ok(true);
        }
        let rest = &bytes[wrote.bytes..];
        if rest.is_empty() {
            self.reliable = 0;
            return Ok(true);
        }
        self.pending.extend_from_slice(rest);
        self.reliable = rest.len();
        Ok(true)
    }

    /// Write one frame. `EAGAIN` stores what fits; it does not block.
    ///
    /// A frame that does not fit is dropped. Reliable bytes queued by
    /// [`Self::write_reliable`] and recovery an abort left are kept whole and are not part
    /// of that drop.
    ///
    /// `publish` runs before each `write` with the [`Wire`] that syscall will
    /// leave on the wire if it stops early, and again with the exact state
    /// after the bytes the kernel accepted. `false` means a fatal handler
    /// moved the wire; this stops and does not queue the rest.
    pub fn write_frame<Fd, P>(&mut self, fd: Fd, frame: &[u8], mut publish: P) -> io::Result<()>
    where
        Fd: Sink,
        P: FnMut(Wire) -> bool,
    {
        if self.drain(&fd, &mut publish)? {
            return Ok(());
        }
        if self.pending_len() == 0 && !self.wire.is_ground() {
            // The previous frame ended on the wire inside a sequence and its
            // tail was not kept (or a handler closed it and stopped). Close it first.
            self.abort(&[]);
            if self.drain(&fd, &mut publish)? {
                return Ok(());
            }
        }
        if self.pending_len() == 0 {
            let wrote = self.write_span(&fd, frame, &mut publish)?;
            if wrote.handoff {
                return Ok(());
            }
            let rest = &frame[wrote.bytes..];
            if !rest.is_empty() {
                self.queue_rest(rest);
            }
            return Ok(());
        }
        if frame.is_empty() {
            return Ok(());
        }
        if self.pending_len().saturating_add(frame.len()) > self.cap {
            self.drops += 1;
            self.abort(&[]);
            if self.wire.is_ground() && self.pending_len() == 0 && frame.len() <= self.cap {
                self.pending.extend_from_slice(frame);
            }
            return Ok(());
        }
        self.pending.extend_from_slice(frame);
        let _ = self.drain(&fd, &mut publish)?;
        Ok(())
    }

    /// The unsent rest of a frame whose start was written. More than the cap: dropped, and
    /// the sequence it cut is finished or closed as an abort would.
    fn queue_rest(&mut self, rest: &[u8]) {
        if rest.len() <= self.cap {
            self.pending.extend_from_slice(rest);
            return;
        }
        self.drops += 1;
        if self.wire.is_ground() {
            return;
        }
        let end = (!self.wire.in_string_of_tmux())
            .then(|| bytes_until_ground(self.wire, rest))
            .flatten()
            .filter(|&end| end <= self.cap);
        match end {
            Some(end) => self.pending.extend_from_slice(&rest[..end]),
            None => self
                .pending
                .extend_from_slice(terminator(self.wire).as_bytes()),
        }
        self.sticky = self.pending_len();
    }

    /// `true` when a fatal handler took the wire and the caller must stop.
    fn drain<Fd, P>(&mut self, fd: &Fd, publish: &mut P) -> io::Result<bool>
    where
        Fd: Sink,
        P: FnMut(Wire) -> bool,
    {
        self.reclaim_spent();
        while self.pending_at < self.pending.len() {
            let start = self.pending_at;
            let (len, announced) = plan_write(self.wire, &self.pending[start..]);
            if len == 0 || !publish(announced) {
                return Ok(true);
            }
            let wrote = match fd.write_some(&self.pending[start..start + len]) {
                Ok(n) => n,
                Err(err) => {
                    let _ = publish(self.wire);
                    return Err(err);
                }
            };
            if wrote == 0 {
                let _ = publish(self.wire);
                break;
            }
            self.wire = advance(self.wire, &self.pending[start..start + wrote]);
            let handoff = !publish(self.wire);
            self.consume(wrote);
            if handoff || wrote < len {
                return Ok(handoff);
            }
        }
        Ok(false)
    }

    fn reclaim_spent(&mut self) {
        if self.pending_at >= self.pending.len() {
            self.pending.clear();
            self.pending_at = 0;
        }
    }

    fn consume(&mut self, wrote: usize) {
        self.reliable = self.reliable.saturating_sub(wrote);
        self.sticky = self.sticky.saturating_sub(wrote);
        self.pending_at += wrote;
        if self.pending_at >= self.pending.len() {
            self.pending.clear();
            self.pending_at = 0;
            return;
        }
        if self.pending_at > self.pending.len() / 2 {
            self.compact();
        }
    }

    /// Slide the unsent tail to the front. Called once the read offset passes half.
    fn compact(&mut self) {
        if self.pending_at == 0 {
            return;
        }
        let from = self.pending_at;
        let moved = self.pending.len() - from;
        self.pending.copy_within(from.., 0);
        self.pending.truncate(moved);
        self.pending_at = 0;
        #[cfg(test)]
        {
            self.shifted += u64::try_from(moved).unwrap_or(u64::MAX);
        }
    }

    fn write_span<Fd, P>(&mut self, fd: &Fd, buf: &[u8], publish: &mut P) -> io::Result<SpanWrite>
    where
        Fd: Sink,
        P: FnMut(Wire) -> bool,
    {
        let mut written = 0;
        let mut rest = buf;
        while !rest.is_empty() {
            let (len, announced) = plan_write(self.wire, rest);
            if len == 0 || !publish(announced) {
                return Ok(SpanWrite {
                    bytes: written,
                    handoff: true,
                });
            }
            let chunk = &rest[..len];
            let n = match fd.write_some(chunk) {
                Ok(n) => n,
                Err(err) => {
                    let _ = publish(self.wire);
                    if written == 0 {
                        return Err(err);
                    }
                    break;
                }
            };
            if n == 0 {
                let _ = publish(self.wire);
                break;
            }
            self.wire = advance(self.wire, &chunk[..n]);
            let handoff = !publish(self.wire);
            written += n;
            rest = &rest[n..];
            if handoff || n < len {
                return Ok(SpanWrite {
                    bytes: written,
                    handoff,
                });
            }
        }
        Ok(SpanWrite {
            bytes: written,
            handoff: false,
        })
    }
}

struct SpanWrite {
    bytes: usize,
    /// A fatal handler published its own wire. Do not queue what follows.
    handoff: bool,
}

#[cfg(unix)]
fn write_partial<Fd: AsFd>(fd: Fd, buf: &[u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    loop {
        match rustix::io::write(&fd, buf) {
            Ok(n) => return Ok(n),
            Err(Errno::INTR) => continue,
            // `WOULDBLOCK` is `EAGAIN` on Linux.
            Err(Errno::AGAIN) => return Ok(0),
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}

#[cfg(test)]
#[path = "output_fuzz.rs"]
mod fuzz_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn at(outer: Outer) -> Wire {
        Wire::GROUND.with(outer)
    }

    fn unsent(output: &Output) -> &[u8] {
        &output.pending[output.pending_at..]
    }

    #[test]
    fn an_open_apc_is_closed_with_st() {
        let mut output = Output::new(64);
        output.wire = at(Outer::Str);
        output.pending.extend_from_slice(b"not-st-yet");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"\x1b\\");
    }

    #[test]
    fn an_apc_chunk_is_finished_and_the_rest_dropped() {
        let mut output = Output::new(64);
        output.wire = at(Outer::Str);
        output.pending.extend_from_slice(b"AAAA\x1b\\NEXT");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"AAAA\x1b\\");
    }

    /// A resize mid-sixel must not keep the rest of the DCS body. `CAN ST` ends it, also in
    /// tmux, which takes `CAN` as content and would take an `ESC` after an `ESC` as one.
    #[test]
    fn an_open_dcs_is_ended_without_the_body() {
        let mut output = Output::new(64);
        output.wire = at(Outer::Dcs);
        output.pending.extend_from_slice(b"q\"1;1#0!10~\x1b\\NEXT");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"\x18\x1b\\");

        let mut output = Output::new(4096);
        output.wire = at(Outer::Dcs);
        output.pending.extend_from_slice(b"SENT");
        output.pending_at = 4;
        output.pending.extend(std::iter::repeat_n(b'~', 2000));
        output.pending.extend_from_slice(b"\x1b\\");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"\x18\x1b\\");
        assert_eq!(output.pending_at, 0);
    }

    #[test]
    fn an_open_osc_is_kept_through_its_end() {
        let mut output = Output::new(64);
        output.wire = at(Outer::Osc);
        output.pending.extend_from_slice(b"0;title\x07NEXT");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"0;title\x07");
    }

    #[test]
    fn ground_discards_unsent_frames() {
        let mut output = Output::new(64);
        output.pending.extend_from_slice(b"\x1b[2Jwaiting");
        output.discard_to_boundary();
        assert!(output.pending.is_empty());
        assert_eq!(output.wire, Wire::GROUND);
    }

    #[test]
    fn csi_is_completed_when_its_final_is_queued() {
        let mut output = Output::new(64);
        output.wire = at(Outer::Csi);
        output.pending.extend_from_slice(b"31mNEXT");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"31m");
    }

    #[test]
    fn advance_sees_a_finished_kitty_chunk() {
        let wire = advance(Wire::GROUND, b"\x1b_Gq=2,m=0;AAAA\x1b\\");
        assert_eq!(wire, Wire::GROUND);
        let open = advance(Wire::GROUND, b"\x1b_Gq=2,m=0;AAAA");
        assert_eq!(open, at(Outer::Str));
        assert_eq!(terminator(open).as_bytes(), b"\x1b\\");
    }

    /// tmux's passthrough: the doubled `ESC`s are content, so the inner `ST` is not the end
    /// (the old model called it ground there), and the outer terminal's state is tracked.
    #[test]
    fn a_passthrough_is_open_until_its_own_st() {
        let mut wire = Wire::GROUND;
        let bytes = b"\x1bPtmux;\x1b\x1b_Gq=2,m=1;AAAA\x1b\x1b\\\x1b\\";
        let mut states = Vec::new();
        for &byte in bytes.iter() {
            wire = step(wire, byte);
            states.push(wire);
        }
        // Every byte before the last leaves something open.
        assert!(states[..states.len() - 1].iter().all(|s| !s.is_ground()));
        assert_eq!(wire, Wire::GROUND);
        // Right after the inner ST: tmux is still in the passthrough, the outer terminal at
        // ground.
        let inner_st = bytes.len() - 2;
        assert_eq!(
            states[inner_st - 1],
            Wire {
                outer: Outer::Pass,
                inner: Inner::Ground
            }
        );
        // Inside the inner APC.
        let open = advance(Wire::GROUND, b"\x1bPtmux;\x1b\x1b_Gq=2,m=1;AA");
        assert_eq!(
            open,
            Wire {
                outer: Outer::Pass,
                inner: Inner::Str
            }
        );
        // Closed inside the passthrough: the outer terminal's APC and the upload end, then
        // the passthrough's own ST.
        assert_eq!(
            terminator(open).as_bytes(),
            b"\x18\x1b\x1b\\\x1b\x1b_Gq=2,m=0;\x1b\x1b\\\x1b\\"
        );
        // A plain DCS (a sixel) is not a passthrough.
        let sixel = advance(Wire::GROUND, b"\x1bPq#0~~");
        assert_eq!(sixel, at(Outer::Dcs));
        assert_eq!(advance(sixel, b"\x1b\\"), Wire::GROUND);
        // `ESC P tm` then `ESC \`: tmux ends a DCS there (the prefix never matched).
        assert_eq!(advance(Wire::GROUND, b"\x1bPtm\x1b\\"), Wire::GROUND);
    }

    #[test]
    fn the_wire_round_trips_through_its_byte() {
        for outer in OUTERS {
            for inner in INNERS {
                let wire = Wire { outer, inner };
                assert_eq!(Wire::from_u8(wire.as_u8()), wire);
            }
        }
        assert_eq!(Wire::from_u8(0), Wire::GROUND);
    }

    /// Each state's terminator leaves that state at ground, and every exact state's
    /// terminator fits its buffer.
    #[test]
    fn every_terminator_reaches_ground() {
        for outer in OUTERS {
            for inner in INNERS {
                let wire = Wire { outer, inner };
                assert!(covers(wire, wire) || wire.is_ground());
                let (members, count) = wire.members();
                for &member in &members[..count] {
                    assert!(covers_exact(wire, member), "{wire:?} for {member:?}");
                }
            }
        }
    }

    #[test]
    fn plan_write_publishes_a_state_whose_terminator_fits_every_prefix() {
        let streams: &[&[u8]] = &[
            b"\x1b_Gq=2;AAAA\x1b\\",
            b"text\x1b[31mmore\x1b]52;c;YQ==\x1b\\tail",
            b"\x1bPq#0;2;0;0;0#0!40~-\x1b\\",
            b"\x1bPtmux;\x1b\x1b_Ga=T,q=2,m=1;AAAA\x1b\x1b\\\x1b\x1b_Gq=2,m=0;BB\x1b\x1b\\\x1b\\x",
        ];
        for stream in streams {
            let mut wire = Wire::GROUND;
            let mut rest = *stream;
            while !rest.is_empty() {
                let (len, publish) = plan_write(wire, rest);
                assert!(len > 0);
                let mut state = wire;
                assert!(covers(publish, state), "{publish:?} for {state:?}");
                for &byte in &rest[..len] {
                    state = step(state, byte);
                    assert!(covers(publish, state), "{publish:?} for {state:?}");
                }
                wire = state;
                rest = &rest[len..];
            }
            assert_eq!(wire, Wire::GROUND);
        }
        // The ESC of ST is published as the string: a handler then sends the whole ST.
        assert_eq!(plan_write(at(Outer::Str), b"\x1b\\"), (2, at(Outer::Str)));
        assert_eq!(plan_write(at(Outer::StrEsc), b"\\"), (1, at(Outer::Str)));
        assert_eq!(plan_write(Wire::GROUND, b"text"), (4, Wire::GROUND));
        // The `\` that ends a passthrough is published as both sides of it.
        let closing = plan_write(at(Outer::PassEsc), b"\\x");
        assert_eq!(closing.1.outer, Outer::Closing);
    }

    #[test]
    fn discard_ignores_bytes_already_written() {
        let mut output = Output::new(64);
        output.wire = at(Outer::Str);
        output.pending.extend_from_slice(b"SENTAAAA\x1b\\NEXT");
        output.pending_at = 4;
        output.discard_to_boundary();
        assert_eq!(output.pending, b"AAAA\x1b\\");
        assert_eq!(output.pending_len(), 6);
        assert_eq!(output.pending_at, 0);
    }

    /// Finding 3 of review B: a resize under backpressure cut a kitty transfer through tmux;
    /// a second resize before the terminal read again used to keep only the inner APC's
    /// end and drop the repair, leaving tmux in the passthrough. The repair stays now.
    #[test]
    fn a_second_abort_keeps_the_first_ones_recovery() {
        let mut output = Output::new(1 << 16);
        let transfer =
            b"\x1bPtmux;\x1b\x1b_Ga=T,q=2,m=1;AAAA\x1b\x1b\\\x1b\x1b_Gq=2,m=0;BBBB\x1b\x1b\\\x1b\\";
        // The terminal took the transfer up to the middle of the first chunk.
        let cut = 30;
        output.wire = advance(Wire::GROUND, &transfer[..cut]);
        output.pending.extend_from_slice(&transfer[cut..]);
        output.pending.extend_from_slice(b"\x1b[2Jnext frame");
        output.abort(b"EXTRA");
        let first = unsent(&output).to_vec();
        assert_eq!(
            advance(output.wire, &first[..first.len() - 5]),
            Wire::GROUND
        );
        assert!(first.ends_with(b"EXTRA"));
        // The next layout's frame is queued behind it, then dropped by the second resize.
        output.pending.extend_from_slice(b"\x1b[Hframe two");
        output.abort(&[]);
        assert_eq!(unsent(&output), &first[..]);
        // Half of the recovery went out, then a third abort: the rest stays whole.
        output.wire = advance(output.wire, &first[..3]);
        output.pending_at += 3;
        output.sticky -= 3;
        output.pending.extend_from_slice(b"\x1b[Hframe three");
        output.abort(&[]);
        assert_eq!(unsent(&output), &first[3..]);
        assert_eq!(
            advance(output.wire, &first[3..first.len() - 5]),
            Wire::GROUND
        );
    }

    /// The abort's own terminator, for every cut of a tmux transfer and of a sixel: what is
    /// written after the cut leaves both parsers at ground.
    #[test]
    fn every_cut_of_a_passthrough_is_recovered() {
        let streams: &[&[u8]] = &[
            b"\x1bPtmux;\x1b\x1b_Ga=T,q=2,m=1;AAAA\x1b\x1b\\\x1b\x1b_Gq=2,m=0;BBBB\x1b\x1b\\\x1b\\",
            b"\x1bPq#0;2;0;0;0#0!40~-\x1b\\",
        ];
        for stream in streams {
            for cut in 0..=stream.len() {
                let mut output = Output::new(1 << 16);
                output.wire = advance(Wire::GROUND, &stream[..cut]);
                output.pending.extend_from_slice(&stream[cut..]);
                output.discard_to_boundary();
                assert_eq!(
                    advance(output.wire, unsent(&output)),
                    Wire::GROUND,
                    "cut at {cut}"
                );
            }
        }
    }

    /// A 3 MiB kitty frame used to `drain(..wrote)` after every syscall and copy
    /// gigabytes. Compaction moves each surviving byte a handful of times.
    #[test]
    fn draining_a_three_megabyte_frame_copies_linearly() {
        let mut frame = Vec::new();
        let payload = vec![b'A'; 4096];
        while frame.len() < 3 * 1024 * 1024 {
            frame.extend_from_slice(b"\x1b_Gq=2,m=0;");
            frame.extend_from_slice(&payload);
            frame.extend_from_slice(b"\x1b\\");
        }
        let n = frame.len();
        let mut output = Output::new(n);
        output.pending = frame;
        // A pty takes a few kilobytes a write.
        let sink = Takes(16 * 1024);
        while output.pending_len() > 0 {
            let handoff = output.drain(&&sink, &mut |_| true).expect("drain");
            assert!(!handoff);
        }
        assert_eq!(output.pending_len(), 0);
        assert!(output.pending.is_empty());
        assert_eq!(output.wire, Wire::GROUND);
        assert_eq!(output.drops(), 0);
        assert!(output.shifted > 0, "shifted {}", output.shifted);
        assert!(
            output.shifted < (n as u64) * 2,
            "shifted {} for {n} bytes",
            output.shifted
        );
    }

    /// Takes at most this many bytes a write.
    struct Takes(usize);

    impl Sink for &Takes {
        fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len().min(self.0))
        }
    }

    fn devnull() -> std::fs::File {
        let path = if cfg!(windows) { "NUL" } else { "/dev/null" };
        std::fs::File::options()
            .write(true)
            .open(path)
            .expect("the null device")
    }

    /// The null device as a sink, where descriptors are not one.
    #[cfg(not(unix))]
    impl Sink for std::fs::File {
        fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
            std::io::Write::write(&mut &*self, buf)
        }
    }

    /// A pipe whose buffer is full, so the next write returns `EAGAIN`.
    #[cfg(unix)]
    fn full_pipe() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        // `pipe` and `fcntl`: macOS has no `pipe2`.
        let (reader, writer) = rustix::pipe::pipe().expect("pipe");
        rustix::io::ioctl_fionbio(&writer, true).expect("non-blocking");
        let chunk = vec![0u8; 4096];
        loop {
            match rustix::io::write(&writer, &chunk) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(rustix::io::Errno::AGAIN) => break,
                Err(err) => panic!("fill: {err}"),
            }
        }
        (reader, writer)
    }

    #[test]
    fn an_oversized_reliable_write_is_refused_before_it_is_queued() {
        let mut output = Output::new(32);
        let big = vec![b'x'; 33];
        assert!(!output
            .write_reliable(devnull(), &big, |_| true)
            .expect("write"));
        assert_eq!(output.pending_len(), 0);
        assert_eq!(output.reliable, 0);

        output.pending.extend_from_slice(b"keep");
        output.reliable = 4;
        assert!(!output
            .write_reliable(devnull(), &big, |_| true)
            .expect("write"));
        assert_eq!(unsent(&output), b"keep");
        assert_eq!(output.reliable, 4);
    }

    #[cfg(unix)]
    #[test]
    fn a_newer_reliable_write_replaces_an_unsent_one_and_a_frame_drop_keeps_it() {
        let (_reader, writer) = full_pipe();
        let mut output = Output::new(64);
        let first = b"\x1b]52;c;YQ==\x1b\\";
        let second = b"\x1b]52;c;Yg==\x1b\\";
        assert!(output
            .write_reliable(&writer, first, |_| true)
            .expect("write"));
        assert_eq!(unsent(&output), first);
        assert!(output
            .write_reliable(&writer, second, |_| true)
            .expect("replace"));
        assert_eq!(unsent(&output), second);
        assert_eq!(output.wire, Wire::GROUND);

        let frame = vec![b'F'; 200];
        output
            .write_frame(&writer, &frame, |_| true)
            .expect("frame");
        assert_eq!(unsent(&output), second);
        assert!(output.drops() >= 1);
        assert!(!unsent(&output).contains(&b'F'));

        // Ground would otherwise drop the whole queue. The clipboard stays, the frame goes.
        let mut output = Output::new(64);
        output.pending.extend_from_slice(second);
        output.pending.extend_from_slice(&frame);
        output.reliable = second.len();
        output.discard_to_boundary();
        assert_eq!(unsent(&output), second);

        // A sequence already started on the wire is not replaced, and its payload is not
        // swapped for a bare terminator.
        let mut output = Output::new(64);
        output.wire = at(Outer::Osc);
        output.pending.extend_from_slice(b"aGVq\x1b\\");
        output.pending.extend_from_slice(&frame);
        output.reliable = b"aGVq\x1b\\".len();
        assert!(!output
            .write_reliable(devnull(), second, |_| true)
            .expect("in flight"));
        assert_eq!(&unsent(&output)[..b"aGVq\x1b\\".len()], b"aGVq\x1b\\");
        output.discard_to_boundary();
        assert_eq!(unsent(&output), b"aGVq\x1b\\");
    }
}
