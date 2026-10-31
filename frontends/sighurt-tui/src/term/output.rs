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
//! Bytes already handed to the kernel are tracked with [`Wire`]. Dropping the
//! queue finishes that open sequence or ends it (a kitty graphics APC with
//! `ESC \`; an open DCS with `ST` and none of the body still queued) so the
//! terminal is not left inside the sequence.
//!
//! A write is split so the state published *before* the syscall is the state
//! on the wire if that syscall stops early: inside a sequence from its first
//! byte until the byte that leaves the sequence has been accepted. The session
//! stores that classification before it calls `write`.

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

/// How many leading bytes of `bytes` one `write` may send, and the [`Wire`]
/// to publish before that syscall.
///
/// The published state stays inside a sequence until the byte that reaches
/// ground is accepted, and it is never [`Wire::StrEsc`] before the `ESC` of
/// `ST` has been accepted: a fatal handler that runs during the syscall then
/// sends a terminator that still closes the sequence.
pub(super) fn plan_write(start: Wire, bytes: &[u8]) -> (usize, Wire) {
    if bytes.is_empty() {
        return (0, start);
    }
    let mut wire = start;
    let mut publish = start;
    for (index, &byte) in bytes.iter().enumerate() {
        let next = step(wire, byte);
        // The byte that leaves the sequence is its own syscall. Publish the
        // state from before it; ground is stored only after `write` returns.
        if next == Wire::Ground && wire != Wire::Ground {
            if index == 0 {
                return (1, wire);
            }
            return (index, publish);
        }
        // `ESC` of `ST` would be `StrEsc`, whose terminator is only `\`.
        // Publish the string state (`APC` / `OSC` / …) so the handler sends
        // the whole `ST` if this `ESC` has not landed yet.
        if next == Wire::StrEsc && wire != Wire::StrEsc {
            if index == 0 {
                return (1, wire);
            }
            return (index, publish);
        }
        if next != Wire::Ground {
            publish = next;
        }
        wire = next;
    }
    (bytes.len(), publish)
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
    /// First byte not yet accepted by `write`. Earlier bytes stay until compact.
    pending_at: usize,
    cap: usize,
    /// Unsent bytes at the front of the pending tail that must be written whole.
    /// A frame drop keeps them. Zero when the tail has none.
    reliable: usize,
    drops: u64,
    /// Parser state of bytes already accepted by `write`.
    wire: Wire,
    /// Last `wire_gen` applied from the signal path.
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
            drops: 0,
            wire: Wire::Ground,
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
        self.reclaim_spent();
        let mut combined = Vec::with_capacity(bytes.len() + self.pending_len());
        combined.extend_from_slice(bytes);
        combined.extend_from_slice(&self.pending[self.pending_at..]);
        self.pending = combined;
        self.pending_at = 0;
    }

    /// Drop everything that has not started on the wire.
    ///
    /// An open sequence is kept through its end, or replaced with [`terminator`]
    /// when the end is not in the queue. A kitty APC chunk therefore finishes
    /// or is closed with `ST`. An open `DCS` is always closed with `ST`: a
    /// sixel is one DCS, and a truncated sixel is valid, so the rest of the
    /// body is not kept. Reliable bytes are the exception: they stay whole,
    /// and what follows them is what this drops.
    pub fn discard_to_boundary(&mut self) {
        self.reclaim_spent();
        if self.reliable > self.pending_len() {
            self.reliable = self.pending_len();
        }
        if self.reliable > 0 {
            let keep = self.reliable;
            if self.wire == Wire::Ground {
                self.pending.truncate(self.pending_at + keep);
                self.compact();
                return;
            }
            let tail = &self.pending[self.pending_at..self.pending_at + keep];
            if let Some(end) = bytes_until_ground(self.wire, tail) {
                self.pending.truncate(self.pending_at + end);
                self.reliable = end;
                self.compact();
                return;
            }
            // The reliable prefix does not reach ground. Keep it and close
            // after it. Never replace the prefix with a short terminator.
            let term = terminator(self.wire);
            self.pending.truncate(self.pending_at + keep);
            if keep + term.len() <= self.cap {
                self.pending.extend_from_slice(term);
                self.reliable = keep + term.len();
            }
            self.compact();
            return;
        }
        if self.wire == Wire::Ground {
            self.pending.clear();
            self.pending_at = 0;
            return;
        }
        if self.wire != Wire::Dcs {
            if let Some(end) = bytes_until_ground(self.wire, &self.pending[self.pending_at..]) {
                self.pending.truncate(self.pending_at + end);
                self.compact();
                return;
            }
        }
        let term = terminator(self.wire);
        self.pending.clear();
        self.pending_at = 0;
        self.pending.extend_from_slice(term);
    }

    /// [`Self::discard_to_boundary`] and take those bytes. The caller writes them.
    ///
    /// The `usize` is how many leading bytes of the returned buffer are a
    /// reliable prefix. [`Self::set_reliable_prefix`] puts that back when the
    /// write stops early.
    pub fn take_inflight(&mut self) -> (Vec<u8>, usize) {
        self.discard_to_boundary();
        let reliable = self.reliable.min(self.pending_len());
        self.pending_at = 0;
        self.reliable = 0;
        (std::mem::take(&mut self.pending), reliable)
    }

    /// Mark `bytes` leading unsent bytes as a reliable prefix.
    pub fn set_reliable_prefix(&mut self, bytes: usize) {
        self.reliable = bytes.min(self.pending_len());
    }

    /// Write `bytes` whole, or write nothing.
    ///
    /// `Ok(false)` means `bytes` do not fit in the cap, or the queue holds
    /// something this must not replace (a frame, or a reliable sequence that
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
        Fd: AsFd,
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
        let replace =
            self.wire == Wire::Ground && self.reliable > 0 && self.reliable == self.pending_len();
        if self.pending_len() > 0 || self.wire != Wire::Ground {
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
    /// [`Self::write_reliable`] are kept whole and are not part of that drop.
    ///
    /// `publish` runs before each `write` with the [`Wire`] that syscall will
    /// leave on the wire if it stops early, and again with the exact state
    /// after the bytes the kernel accepted. `false` means a fatal handler
    /// moved the wire; this stops and does not queue the rest.
    pub fn write_frame<Fd, P>(&mut self, fd: Fd, frame: &[u8], mut publish: P) -> io::Result<()>
    where
        Fd: AsFd,
        P: FnMut(Wire) -> bool,
    {
        if self.drain(&fd, &mut publish)? {
            return Ok(());
        }
        if self.pending_len() == 0 && self.wire != Wire::Ground {
            // The previous frame ended on the wire inside a sequence and its
            // tail was not kept. Close it before the next frame.
            self.pending.extend_from_slice(terminator(self.wire));
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
        if self.pending_len().saturating_add(frame.len()) > self.cap {
            self.drops += 1;
            self.discard_to_boundary();
            if self.wire == Wire::Ground && self.pending_len() == 0 && frame.len() <= self.cap {
                self.pending.extend_from_slice(frame);
            }
            return Ok(());
        }
        self.pending.extend_from_slice(frame);
        let _ = self.drain(fd, &mut publish)?;
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

    /// `true` when a fatal handler took the wire and the caller must stop.
    fn drain<Fd, P>(&mut self, fd: Fd, publish: &mut P) -> io::Result<bool>
    where
        Fd: AsFd,
        P: FnMut(Wire) -> bool,
    {
        self.reclaim_spent();
        while self.pending_at < self.pending.len() {
            let start = self.pending_at;
            let (len, announced) = plan_write(self.wire, &self.pending[start..]);
            if len == 0 || !publish(announced) {
                return Ok(true);
            }
            let wrote = match write_partial(&fd, &self.pending[start..start + len]) {
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
        Fd: AsFd,
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
            let n = match write_partial(fd, chunk) {
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

    /// A resize mid-sixel must not keep the rest of the DCS body. `ST` ends it.
    #[test]
    fn an_open_dcs_is_ended_with_st_without_the_body() {
        let mut output = Output::new(64);
        output.wire = Wire::Dcs;
        output.pending.extend_from_slice(b"q\"1;1#0!10~\x1b\\NEXT");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"\x1b\\");

        let mut output = Output::new(4096);
        output.wire = Wire::Dcs;
        output.pending.extend_from_slice(b"SENT");
        output.pending_at = 4;
        output.pending.extend(std::iter::repeat_n(b'~', 2000));
        output.pending.extend_from_slice(b"\x1b\\");
        output.discard_to_boundary();
        assert_eq!(output.pending, b"\x1b\\");
        assert_eq!(output.pending_at, 0);
    }

    #[test]
    fn an_open_osc_is_kept_through_its_end() {
        let mut output = Output::new(64);
        output.wire = Wire::Osc;
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

    #[test]
    fn plan_write_keeps_an_apc_open_until_its_terminator_lands() {
        let bytes = b"\x1b_Gq=2;AAAA\x1b\\";
        let mut wire = Wire::Ground;
        let mut rest = &bytes[..];
        let mut saw_apc = false;
        while !rest.is_empty() {
            let (len, publish) = plan_write(wire, rest);
            assert!(len > 0);
            if publish == Wire::Apc {
                saw_apc = true;
            }
            // Ground is not published while any byte of this chunk is still unsent.
            if wire != Wire::Ground || rest[..len].contains(&0x1b) {
                assert_ne!(publish, Wire::Ground);
            }
            wire = advance(wire, &rest[..len]);
            rest = &rest[len..];
        }
        assert!(saw_apc);
        assert_eq!(wire, Wire::Ground);

        // The ESC of ST is published as APC, not StrEsc: the handler's
        // terminator is then the whole ST even if that ESC has not landed.
        assert_eq!(plan_write(Wire::Apc, b"\x1b\\"), (1, Wire::Apc));
        assert_eq!(plan_write(Wire::StrEsc, b"\\"), (1, Wire::StrEsc));
        assert_eq!(plan_write(Wire::Csi, b"31m"), (2, Wire::Csi));
        assert_eq!(plan_write(Wire::Ground, b"text"), (4, Wire::Ground));
    }

    #[test]
    fn discard_ignores_bytes_already_written() {
        let mut output = Output::new(64);
        output.wire = Wire::Apc;
        output.pending.extend_from_slice(b"SENTAAAA\x1b\\NEXT");
        output.pending_at = 4;
        output.discard_to_boundary();
        assert_eq!(output.pending, b"AAAA\x1b\\");
        assert_eq!(output.pending_len(), 6);
        assert_eq!(output.pending_at, 0);
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
        let devnull = std::fs::File::options()
            .write(true)
            .open("/dev/null")
            .expect("/dev/null");
        let handoff = output.drain(&devnull, &mut |_| true).expect("drain");
        assert!(!handoff);
        assert_eq!(output.pending_len(), 0);
        assert!(output.pending.is_empty());
        assert_eq!(output.wire, Wire::Ground);
        assert_eq!(output.drops(), 0);
        assert!(output.shifted > 0, "shifted {}", output.shifted);
        assert!(
            output.shifted < (n as u64) * 2,
            "shifted {} for {n} bytes",
            output.shifted
        );
    }

    fn unsent(output: &Output) -> &[u8] {
        &output.pending[output.pending_at..]
    }

    fn devnull() -> std::fs::File {
        std::fs::File::options()
            .write(true)
            .open("/dev/null")
            .expect("/dev/null")
    }

    /// A pipe whose buffer is full, so the next write returns `EAGAIN`.
    fn full_pipe() -> (std::os::fd::OwnedFd, std::os::fd::OwnedFd) {
        let (reader, writer) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )
        .expect("pipe");
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
        assert_eq!(output.wire, Wire::Ground);

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
        output.wire = Wire::Osc;
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
