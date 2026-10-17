//! A bounded non-blocking writer.
//!
//! The terminal's reader can stop. Writes never wait for it: at most `cap`
//! bytes sit in `pending`, and a frame that does not fit is dropped so the
//! newest frame can take its place.

use std::io;
use std::os::fd::AsFd;

use rustix::io::Errno;

/// Bytes waiting to be written, and how many frames were discarded.
#[derive(Debug)]
pub struct Output {
    pending: Vec<u8>,
    cap: usize,
    drops: u64,
}

impl Output {
    /// `cap` is the most bytes kept when the kernel will not take them.
    pub fn new(cap: usize) -> Self {
        Self {
            pending: Vec::new(),
            cap,
            drops: 0,
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

    /// Forget anything not yet written. Used when leaving the terminal.
    pub fn clear(&mut self) {
        self.pending.clear();
    }

    /// Write one frame. `EAGAIN` stores what fits; it does not block.
    pub fn write_frame<Fd: AsFd>(&mut self, fd: Fd, frame: &[u8]) -> io::Result<()> {
        self.drain(&fd)?;
        if self.pending.is_empty() {
            let wrote = write_partial(&fd, frame)?;
            let rest = &frame[wrote..];
            if rest.is_empty() {
                return Ok(());
            }
            if rest.len() <= self.cap {
                self.pending.extend_from_slice(rest);
            } else {
                self.drops += 1;
            }
            return Ok(());
        }
        if self.pending.len().saturating_add(frame.len()) > self.cap {
            self.drops += 1;
            self.pending.clear();
            if frame.len() <= self.cap {
                self.pending.extend_from_slice(frame);
            }
            return Ok(());
        }
        self.pending.extend_from_slice(frame);
        self.drain(fd)?;
        Ok(())
    }

    fn drain<Fd: AsFd>(&mut self, fd: Fd) -> io::Result<()> {
        while !self.pending.is_empty() {
            let wrote = write_partial(&fd, &self.pending)?;
            if wrote == 0 {
                break;
            }
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
