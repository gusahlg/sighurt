//! A bounded non-blocking writer.
//!
//! The terminal's reader can stop. Writes never wait for it: at most `cap`
//! bytes sit in `pending`, and a frame that does not fit is dropped so the
//! newest frame can take its place.
//!
//! Bytes already handed to the kernel are tracked with [`Wire`]. Dropping the
//! queue finishes that open sequence or ends it (a kitty graphics APC with
//! `ESC \`) so the terminal is not left inside the sequence.

use std::io;
use std::os::fd::AsFd;

use rustix::io::Errno;

/// Where a terminal parser is, given only the bytes already written.
///
/// The signal path reads the same classification from shared state. It must
/// not look at [`Output`]'s buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Wire {
    /// Not inside a control sequence.
    Ground = 0,
    /// Saw `ESC`.
    Esc = 1,
    /// Inside `CSI`.
    Csi = 2,
    /// Inside `OSC`.
    Osc = 3,
    /// Inside `APC` (kitty graphics).
    Apc = 4,
    /// Inside `DCS`.
    Dcs = 5,
    /// Inside `SOS` or `PM`.
    Sos = 6,
    /// Inside a string and the previous byte was `ESC` (an `ST` may finish).
    StrEsc = 7,
    /// `ESC` followed by intermediates.
    EscInt = 8,
}

impl Wire {
    /// The value stored beside the session for the signal path.
    pub fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Esc,
            2 => Self::Csi,
            3 => Self::Osc,
            4 => Self::Apc,
            5 => Self::Dcs,
            6 => Self::Sos,
            7 => Self::StrEsc,
            8 => Self::EscInt,
            _ => Self::Ground,
        }
    }

    fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Bytes that leave `wire` if nothing else will. Empty when already at ground.
///
/// `APC` / `DCS` / `OSC` end with `ST`. A half-written `ST` ends with `\`.
/// `CSI` and a bare `ESC` are cancelled.
pub fn terminator(wire: Wire) -> &'static [u8] {
    match wire {
        Wire::Ground => b"",
        Wire::StrEsc => b"\\",
        Wire::Osc | Wire::Apc | Wire::Dcs | Wire::Sos => b"\x1b\\",
        Wire::Esc | Wire::Csi | Wire::EscInt => b"\x18",
    }
}

/// `wire` after `bytes` have been written.
pub fn advance(mut wire: Wire, bytes: &[u8]) -> Wire {
    for &byte in bytes {
        wire = step(wire, byte);
    }
    wire
}

fn step(wire: Wire, byte: u8) -> Wire {
    // CAN and SUB cancel every sequence, including a string.
    if byte == 0x18 || byte == 0x1a {
        return Wire::Ground;
    }
    match wire {
        Wire::Ground => {
            if byte == 0x1b {
                Wire::Esc
            } else {
                Wire::Ground
            }
        }
        Wire::Esc => match byte {
            0x1b => Wire::Esc,
            b'[' => Wire::Csi,
            b']' => Wire::Osc,
            b'_' => Wire::Apc,
            b'P' => Wire::Dcs,
            b'X' | b'^' => Wire::Sos,
            0x20..=0x2f => Wire::EscInt,
            0x30..=0x7e => Wire::Ground,
            _ => Wire::Esc,
        },
        Wire::EscInt => match byte {
            0x1b => Wire::Esc,
            0x20..=0x2f => Wire::EscInt,
            0x30..=0x7e => Wire::Ground,
            _ => Wire::EscInt,
        },
        Wire::Csi => match byte {
            0x1b => Wire::Esc,
            0x40..=0x7e => Wire::Ground,
            _ => Wire::Csi,
        },
        Wire::Osc => match byte {
            0x07 => Wire::Ground,
            0x1b => Wire::StrEsc,
            _ => Wire::Osc,
        },
        Wire::Apc | Wire::Dcs | Wire::Sos => {
            if byte == 0x1b {
                Wire::StrEsc
            } else {
                wire
            }
        }
        Wire::StrEsc => match byte {
            b'\\' => Wire::Ground,
            _ => step(Wire::Esc, byte),
        },
    }
}

/// How many leading bytes finish the open sequence. `None` when they do not.
fn bytes_until_ground(wire: Wire, bytes: &[u8]) -> Option<usize> {
    if wire == Wire::Ground {
        return Some(0);
    }
    let mut wire = wire;
    for (index, &byte) in bytes.iter().enumerate() {
        wire = step(wire, byte);
        if wire == Wire::Ground {
            return Some(index + 1);
        }
    }
    None
}

/// Bytes waiting to be written, and how many frames were discarded.
#[derive(Debug)]
pub struct Output {
    pending: Vec<u8>,
    cap: usize,
    drops: u64,
    /// Parser state of bytes already accepted by `write`.
    wire: Wire,
    /// Last `wire_gen` applied from the signal path.
    seen_gen: u64,
}

impl Output {
    /// `cap` is the most bytes kept when the kernel will not take them.
    pub fn new(cap: usize) -> Self {
        Self {
            pending: Vec::new(),
            cap,
            drops: 0,
            wire: Wire::Ground,
            seen_gen: 0,
        }
    }

    /// Bytes still waiting.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Frames discarded because they did not fit in the cap.
    pub fn drops(&self) -> u64 {
        self.drops
    }

    /// Wire byte stored for the signal path.
    pub fn wire_byte(&self) -> u8 {
        self.wire.as_u8()
    }

    /// Generation last taken from the signal path.
    pub fn seen_gen(&self) -> u64 {
        self.seen_gen
    }

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
        true
    }

    /// Account for `bytes` that were just written.
    pub fn advance_wire(&mut self, bytes: &[u8]) {
        self.wire = advance(self.wire, bytes);
    }

    /// Put `bytes` back in front of the queue. Used when a restore write stops early.
    pub fn prepend(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut combined = Vec::with_capacity(bytes.len() + self.pending.len());
        combined.extend_from_slice(bytes);
        combined.append(&mut self.pending);
        self.pending = combined;
    }

    /// Drop everything that has not started on the wire.
    ///
    /// An open sequence is kept through its end, or replaced with [`terminator`]
    /// when the end is not in the queue. A kitty APC chunk therefore finishes
    /// or is closed with `ST`.
    pub fn discard_to_boundary(&mut self) {
        if self.wire == Wire::Ground {
            self.pending.clear();
            return;
        }
        match bytes_until_ground(self.wire, &self.pending) {
            Some(end) => self.pending.truncate(end),
            None => {
                let term = terminator(self.wire);
                self.pending.clear();
                self.pending.extend_from_slice(term);
            }
        }
    }

    /// [`Self::discard_to_boundary`] and take those bytes. The caller writes them.
    pub fn take_inflight(&mut self) -> Vec<u8> {
        self.discard_to_boundary();
        std::mem::take(&mut self.pending)
    }

    /// Write one frame. `EAGAIN` stores what fits; it does not block.
    pub fn write_frame<Fd: AsFd>(&mut self, fd: Fd, frame: &[u8]) -> io::Result<()> {
        self.drain(&fd)?;
        if self.pending.is_empty() && self.wire != Wire::Ground {
            // The previous frame ended on the wire inside a sequence and its
            // tail was not kept. Close it before the next frame.
            self.pending.extend_from_slice(terminator(self.wire));
        }
        if self.pending.is_empty() {
            let wrote = write_partial(&fd, frame)?;
            self.wire = advance(self.wire, &frame[..wrote]);
            let rest = &frame[wrote..];
            if !rest.is_empty() {
                self.queue_rest(rest);
            }
            return Ok(());
        }
        if self.pending.len().saturating_add(frame.len()) > self.cap {
            self.drops += 1;
            self.discard_to_boundary();
            if self.wire == Wire::Ground && self.pending.is_empty() && frame.len() <= self.cap {
                self.pending.extend_from_slice(frame);
            }
            return Ok(());
        }
        self.pending.extend_from_slice(frame);
        self.drain(fd)?;
        Ok(())
    }

    fn queue_rest(&mut self, rest: &[u8]) {
        if rest.len() <= self.cap {
            self.pending.extend_from_slice(rest);
            return;
        }
        self.drops += 1;
        if self.wire == Wire::Ground {
            return;
        }
        match bytes_until_ground(self.wire, rest) {
            Some(end) if end <= self.cap => self.pending.extend_from_slice(&rest[..end]),
            _ => self.pending.extend_from_slice(terminator(self.wire)),
        }
    }

    fn drain<Fd: AsFd>(&mut self, fd: Fd) -> io::Result<()> {
        while !self.pending.is_empty() {
            let wrote = write_partial(&fd, &self.pending)?;
            if wrote == 0 {
                break;
            }
            self.wire = advance(self.wire, &self.pending[..wrote]);
            self.pending.drain(..wrote);
        }
        Ok(())
    }
}

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
mod tests {
    use super::*;

    #[test]
    fn an_open_apc_is_closed_with_st() {
        let mut output = Output::new(64);
        output.wire = Wire::Apc;
        output.pending.extend_from_slice(b"not-st-yet");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"\x1b\\");
    }

    #[test]
    fn an_apc_chunk_is_finished_and_the_rest_dropped() {
        let mut output = Output::new(64);
        output.wire = Wire::Apc;
        output.pending.extend_from_slice(b"AAAA\x1b\\NEXT");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"AAAA\x1b\\");
    }

    #[test]
    fn ground_discards_unsent_frames() {
        let mut output = Output::new(64);
        output.pending.extend_from_slice(b"\x1b[2Jwaiting");
        output.discard_to_boundary();
        assert!(output.pending.is_empty());
        assert_eq!(output.wire, Wire::Ground);
    }

    #[test]
    fn csi_is_completed_when_its_final_is_queued() {
        let mut output = Output::new(64);
        output.wire = Wire::Csi;
        output.pending.extend_from_slice(b"31mNEXT");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"31m");
    }

    #[test]
    fn advance_sees_a_finished_kitty_chunk() {
        let wire = advance(Wire::Ground, b"\x1b_Gq=2,m=0;AAAA\x1b\\");
        assert_eq!(wire, Wire::Ground);
        let open = advance(Wire::Ground, b"\x1b_Gq=2,m=0;AAAA");
        assert_eq!(open, Wire::Apc);
        assert_eq!(terminator(open), b"\x1b\\");
    }
}
