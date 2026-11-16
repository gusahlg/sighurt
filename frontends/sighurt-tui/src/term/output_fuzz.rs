//! Seeded random tests of the output layer against parsers of its own: tmux 3.8's (its
//! `input.c` tables, DCS passthrough included) and, behind it, the outer terminal's, as a
//! VT500 parser and as a strict one (a string ends only at `ST`; `CAN` in it is text), with
//! the kitty upload each has open.
//!
//! A case writes frames made of what `sig` writes (text, CSI, OSC, sixel, kitty transfers
//! and deletes in tmux's passthrough, or bare kitty chunks outside tmux) through a sink that
//! takes a random prefix of each write, aborts any number of times between them (with or
//! without bytes to add), and leaves as the session does. A fatal signal is also simulated
//! during random writes: what was accepted, the [`terminator`] of the state published
//! before that write, then `LEAVE`. Every time, tmux and both outer terminals end at
//! ground, no kitty upload is left open, and tmux never took a bare APC (it would be the
//! pane's title).

// The parser's fuzz tests include it too; each copy has its own watchdog, which is all it
// keeps.
#[allow(clippy::duplicate_mod)]
#[path = "../../../../tools/fuzz/mod.rs"]
mod fuzz;

use std::cell::{Cell, RefCell};
use std::io;
use std::time::Duration;

use fuzz::{iterations, run, Rng};

use super::*;
use crate::term::LEAVE;

/// tmux's parser states, as far as they matter here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum T {
    Ground,
    Esc,
    EscInt,
    Csi,
    Osc,
    Apc,
    /// SOS, PM, and tmux's `ESC k` rename: strings that end like an APC.
    Other,
    DcsEnter,
    DcsHandler,
    DcsEscape,
    DcsIgnore,
}

/// The outer terminal: a VT500 parser, or a strict one.
#[derive(Clone, Debug)]
struct Outer2 {
    strict: bool,
    state: O,
    buf: Vec<u8>,
    upload_open: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum O {
    Ground,
    Esc,
    Csi,
    Str(bool),
    StrEsc(bool),
}

impl Outer2 {
    fn new(strict: bool) -> Self {
        Self {
            strict,
            state: O::Ground,
            buf: Vec::new(),
            upload_open: false,
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        self.state = match self.state {
            O::Ground => match byte {
                0x1b => O::Esc,
                _ => O::Ground,
            },
            O::Esc => self.esc(byte),
            O::Csi => match byte {
                0x18 | 0x1a => O::Ground,
                0x1b => O::Esc,
                0x40..=0x7e => O::Ground,
                _ => O::Csi,
            },
            O::Str(apc) => match byte {
                0x1b => O::StrEsc(apc),
                0x18 | 0x1a if !self.strict => O::Ground,
                _ => {
                    self.buf.push(byte);
                    O::Str(apc)
                }
            },
            O::StrEsc(apc) => match byte {
                b'\\' => {
                    if apc {
                        self.dispatch();
                    }
                    O::Ground
                }
                // The first `ESC` was text; this one may begin the `ST`.
                0x1b if self.strict => {
                    self.buf.push(0x1b);
                    O::StrEsc(apc)
                }
                _ if self.strict => {
                    self.buf.extend_from_slice(&[0x1b, byte]);
                    O::Str(apc)
                }
                _ => self.esc(byte),
            },
        };
    }

    fn esc(&mut self, byte: u8) -> O {
        match byte {
            0x18 | 0x1a => O::Ground,
            0x1b => O::Esc,
            b'[' => O::Csi,
            b'_' => {
                self.buf.clear();
                O::Str(true)
            }
            b']' | b'P' | b'X' | b'^' => {
                self.buf.clear();
                O::Str(false)
            }
            0x20..=0x2f => O::Esc,
            _ => O::Ground,
        }
    }

    /// A kitty command: `m=1` opens an upload, any other ends it.
    fn dispatch(&mut self) {
        let Some(rest) = self.buf.strip_prefix(b"G") else {
            return;
        };
        let control = rest.split(|&b| b == b';').next().unwrap_or_default();
        self.upload_open = control.split(|&b| b == b',').any(|key| key == b"m=1");
    }

    fn settled(&self) -> bool {
        self.state == O::Ground && !self.upload_open
    }
}

#[derive(Clone, Debug)]
struct Tmux {
    state: T,
    buf: Vec<u8>,
    bare_apcs: u32,
    outers: [Outer2; 2],
}

impl Tmux {
    fn new() -> Self {
        Self {
            state: T::Ground,
            buf: Vec::new(),
            bare_apcs: 0,
            outers: [Outer2::new(false), Outer2::new(true)],
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.byte(byte);
        }
    }

    fn set(&mut self, next: T) {
        if self.state == T::Apc && next != T::Apc {
            self.bare_apcs += 1;
        }
        if matches!(next, T::DcsEnter) {
            self.buf.clear();
        }
        self.state = next;
    }

    fn byte(&mut self, byte: u8) {
        // INPUT_STATE_ANYWHERE: every state but the DCS handler and its escape.
        if !matches!(self.state, T::DcsHandler | T::DcsEscape) {
            match byte {
                0x18 | 0x1a => return self.set(T::Ground),
                0x1b => return self.set(T::Esc),
                _ => {}
            }
        }
        let next = match self.state {
            T::Ground => T::Ground,
            T::Esc => match byte {
                0x20..=0x2f => T::EscInt,
                b'P' => T::DcsEnter,
                b'X' | b'^' | b'k' => T::Other,
                b'[' => T::Csi,
                b']' => T::Osc,
                b'_' => T::Apc,
                0x30..=0x7e => T::Ground,
                _ => T::Esc,
            },
            T::EscInt => match byte {
                0x30..=0x7e => T::Ground,
                _ => T::EscInt,
            },
            T::Csi => match byte {
                0x40..=0x7e => T::Ground,
                _ => T::Csi,
            },
            T::Osc => match byte {
                0x07 => T::Ground,
                _ => T::Osc,
            },
            T::Apc => T::Apc,
            T::Other => T::Other,
            T::DcsEnter => match byte {
                0x3a => T::DcsIgnore,
                0x40..=0x7e => {
                    self.buf.push(byte);
                    T::DcsHandler
                }
                _ => T::DcsEnter,
            },
            T::DcsHandler => match byte {
                0x1b => T::DcsEscape,
                _ => {
                    self.buf.push(byte);
                    T::DcsHandler
                }
            },
            T::DcsEscape => match byte {
                b'\\' => {
                    if let Some(content) = self.buf.strip_prefix(b"tmux;") {
                        let content = content.to_vec();
                        for outer in &mut self.outers {
                            outer.feed(&content);
                        }
                    }
                    T::Ground
                }
                _ => {
                    self.buf.push(byte);
                    T::DcsHandler
                }
            },
            T::DcsIgnore => T::DcsIgnore,
        };
        self.set(next);
    }

    fn check(&self, what: &str, bytes: &[u8]) {
        assert_eq!(
            self.state,
            T::Ground,
            "tmux not at ground {what}: {bytes:?}"
        );
        assert_eq!(self.bare_apcs, 0, "tmux took a bare APC {what}: {bytes:?}");
        for outer in &self.outers {
            assert!(
                outer.settled(),
                "outer terminal {outer:?} {what}: {bytes:?}"
            );
        }
    }
}

/// What reads the bytes: tmux with a terminal behind it, or a terminal (both parsers).
fn check(tmux: bool, bytes: &[u8], what: &str) {
    if tmux {
        let mut reader = Tmux::new();
        reader.feed(bytes);
        reader.check(what, bytes);
    } else {
        for strict in [false, true] {
            let mut outer = Outer2::new(strict);
            outer.feed(bytes);
            // Outside tmux the app ends a cut kitty upload; the parsers only must be at ground.
            assert_eq!(outer.state, O::Ground, "terminal {what}: {bytes:?}");
        }
    }
}

const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=";

fn payload(rng: &mut Rng, max: usize) -> Vec<u8> {
    (0..rng.below(max + 1)).map(|_| *rng.pick(B64)).collect()
}

/// One kitty transfer as chunks, `m=1` on every chunk but the last.
fn chunks(rng: &mut Rng) -> Vec<Vec<u8>> {
    let count = rng.range(1, 4);
    (0..count)
        .map(|index| {
            let more = if index + 1 < count { "1" } else { "0" };
            let control = if index == 0 {
                format!("a=T,U=1,f=32,o=z,i=7,c=4,r=2,q=2,m={more}")
            } else {
                format!("q=2,m={more}")
            };
            let mut chunk = format!("\x1b_G{control};").into_bytes();
            chunk.extend(payload(rng, 24));
            chunk.extend_from_slice(b"\x1b\\");
            chunk
        })
        .collect()
}

fn doubled(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for &byte in bytes {
        if byte == 0x1b {
            out.push(0x1b);
        }
        out.push(byte);
    }
    out
}

fn fragment(rng: &mut Rng, tmux: bool) -> Vec<u8> {
    match rng.below(7) {
        0 => (0..rng.below(12)).map(|_| b' ' + rng.byte() % 95).collect(),
        1 => format!("\x1b[{};{}H", rng.below(40), rng.below(90)).into_bytes(),
        2 => {
            let mut osc = b"\x1b]52;c;".to_vec();
            osc.extend(payload(rng, 16));
            osc.extend_from_slice(b"\x1b\\");
            osc
        }
        3 => {
            let mut sixel = b"\x1bPq".to_vec();
            sixel.extend((0..rng.below(30)).map(|_| *rng.pick(b"#0;2~-?@!5")));
            sixel.extend_from_slice(b"\x1b\\");
            sixel
        }
        4 if tmux => {
            let mut transfer = b"\x1bPtmux;".to_vec();
            for chunk in chunks(rng) {
                transfer.extend(doubled(&chunk));
            }
            transfer.extend_from_slice(b"\x1b\\");
            transfer
        }
        4 => chunks(rng).concat(),
        5 if tmux => {
            let mut delete = b"\x1bPtmux;".to_vec();
            delete.extend(doubled(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\"));
            delete.extend_from_slice(b"\x1b\\");
            delete
        }
        _ => b"\x1b[?2026h\x1b[2J\x1b[?2026l".to_vec(),
    }
}

fn frame(rng: &mut Rng, tmux: bool) -> Vec<u8> {
    (0..rng.range(1, 6))
        .flat_map(|_| fragment(rng, tmux))
        .collect()
}

/// Takes a random prefix of each write. A simulated fatal signal sometimes runs during it.
struct Taker {
    rng: RefCell<Rng>,
    accepted: RefCell<Vec<u8>>,
    /// The state published before the write now in progress.
    announced: Cell<Wire>,
    /// Takes everything (the restore).
    open: Cell<bool>,
    tmux: bool,
    signals: Cell<u32>,
}

impl Sink for &Taker {
    fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
        let mut rng = self.rng.borrow_mut();
        let n = if self.open.get() || rng.one_in(3) {
            buf.len()
        } else {
            rng.below(buf.len() + 1)
        };
        if rng.one_in(6) {
            // A fatal handler, after this write took `n` bytes: the published state's
            // terminator, then LEAVE.
            let mut bytes = self.accepted.borrow().clone();
            bytes.extend_from_slice(&buf[..n]);
            bytes.extend_from_slice(terminator(self.announced.get()).as_bytes());
            bytes.extend_from_slice(LEAVE);
            check(self.tmux, &bytes, "after a fatal signal");
            self.signals.set(self.signals.get() + 1);
        }
        self.accepted.borrow_mut().extend_from_slice(&buf[..n]);
        Ok(n)
    }
}

fn case(rng: &mut Rng, tmux: bool) -> Vec<String> {
    let mut log = Vec::new();
    let taker = Taker {
        rng: RefCell::new(rng.clone()),
        accepted: RefCell::new(Vec::new()),
        announced: Cell::new(Wire::GROUND),
        open: Cell::new(false),
        tmux,
        signals: Cell::new(0),
    };
    let mut output = Output::new(rng.range(16, 400));
    let publish = |wire: Wire| {
        taker.announced.set(wire);
        true
    };
    for _ in 0..rng.range(1, 14) {
        match rng.below(6) {
            0 | 1 => {
                let frame = frame(rng, tmux);
                log.push(format!("frame {}", frame.len()));
                output.write_frame(&taker, &frame, publish).expect("frame");
            }
            2 => {
                log.push("drain".into());
                output.write_frame(&taker, &[], publish).expect("drain");
            }
            3 => {
                let extra: &[u8] = match rng.below(3) {
                    0 if tmux => b"\x1bPtmux;\x1b\x1b_Gq=2,m=0;\x1b\x1b\\\x1b\\",
                    0 => b"\x1b_Gq=2,m=0;\x1b\\",
                    _ => b"",
                };
                log.push(format!("abort +{}", extra.len()));
                output.abort(extra);
            }
            4 => {
                let mut osc = b"\x1b]52;c;".to_vec();
                osc.extend(payload(rng, 12));
                osc.extend_from_slice(b"\x1b\\");
                log.push("reliable".into());
                let _ = output
                    .write_reliable(&taker, &osc, publish)
                    .expect("reliable");
            }
            _ => {
                log.push("resize".into());
                output.abort(&[]);
                let frame = frame(rng, tmux);
                output.write_frame(&taker, &frame, publish).expect("frame");
            }
        }
        let accepted = taker.accepted.borrow().clone();
        assert_eq!(
            output.wire,
            advance(Wire::GROUND, &accepted),
            "the wire is what was written"
        );
    }
    // Leaving, as `Session::restore`: the queue finished or closed, then the published
    // state's terminator and LEAVE.
    // The session writes it as every write is, the state published before each syscall.
    taker.open.set(true);
    let rest = output.take_inflight();
    let mut wire = output.wire;
    let mut left = &rest[..];
    while !left.is_empty() {
        let (len, announced) = plan_write(wire, left);
        taker.announced.set(announced);
        let n = (&taker).write_some(&left[..len]).expect("restore");
        wire = advance(wire, &left[..n]);
        left = &left[n..];
    }
    let mut bytes = taker.accepted.borrow().clone();
    bytes.extend_from_slice(terminator(wire).as_bytes());
    bytes.extend_from_slice(LEAVE);
    check(tmux, &bytes, "after leaving");
    log.push(format!("signals {}", taker.signals.get()));
    log
}

#[test]
fn fuzz_output_leaves_tmux_and_the_terminal_at_ground() {
    run(
        "fuzz_term_output",
        iterations(100) * 4,
        Duration::from_secs(2),
        |case_| {
            let tmux = !case_.rng.one_in(4);
            let mut rng = case_.rng.clone();
            let log = case(&mut rng, tmux);
            case_.note(|| format!("tmux={tmux} {log:?}"));
        },
    );
}

/// Every cut of every fragment, recovered by an abort: ground.
#[test]
fn fuzz_output_every_cut_of_a_fragment_is_recovered() {
    run(
        "fuzz_term_output_cuts",
        iterations(100),
        Duration::from_secs(2),
        |case_| {
            let tmux = !case_.rng.one_in(4);
            let bytes = frame(&mut case_.rng, tmux);
            case_.note(|| format!("tmux={tmux} {bytes:?}"));
            for cut in 0..=bytes.len() {
                let mut output = Output::new(1 << 16);
                output.wire = advance(Wire::GROUND, &bytes[..cut]);
                output.pending.extend_from_slice(&bytes[cut..]);
                for _ in 0..case_.rng.range(1, 3) {
                    output.abort(&[]);
                }
                let mut all = bytes[..cut].to_vec();
                all.extend_from_slice(&output.pending[output.pending_at..]);
                all.extend_from_slice(LEAVE);
                check(tmux, &all, &format!("cut at {cut}"));
            }
        },
    );
}
