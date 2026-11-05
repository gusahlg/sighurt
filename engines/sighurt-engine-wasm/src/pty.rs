//! The terminal page's data plane: the bytes between a guest and the master side of a
//! pseudo-terminal the runtime opened for it, and the take-once hand-over of that master.
//!
//! The runtime opens the terminal and starts its program; before it answers the page's
//! `PtyOpen` with the session ([`PtySession`]) it hands the master over here ([`hand_over`]).
//! The engine takes it, once, when the answer reaches the document ([`take`]); a master nobody
//! takes is dropped when the runtime ends the session ([`forget`]). No descriptor is in any
//! service type, and a guest only ever sees a handle of its document.
//!
//! The data plane ([`Pty`]) is a STUB until the terminal lane implements it
//! (docs/rewrite/pages-2026-11-05.md, D and A3): non-blocking reads and writes with bounded
//! queues (1 MiB out, 64 KiB in), an explicit wake-up for cancellation, an orderly reader exit,
//! resizing with `TIOCSWINSZ`; closing stops writes and drops queued input at once; everything
//! dropped with the document's `HostState`. Until then a terminal reads, writes and resizes
//! nothing, and says it has ended.
//!
//! Owns: moving bytes, and the masters on their way to a document. Must not own: starting or
//! ending the program (the runtime does), or deciding who may have a terminal (the core).

use std::os::fd::OwnedFd;
use std::sync::{Mutex, PoisonError};

use sighurt_api::ids::PtySession;

/// Masters handed over and not taken yet. Few: one per terminal opening, at most eight.
static HANDED: Mutex<Vec<(PtySession, OwnedFd)>> = Mutex::new(Vec::new());

/// The runtime hands `master`, the terminal of `session`, to the engine, before it answers.
pub fn hand_over(session: PtySession, master: OwnedFd) {
    let mut handed = HANDED.lock().unwrap_or_else(PoisonError::into_inner);
    handed.push((session, master));
}

/// The master of `session`, once: a second call, or one for a session never handed over or
/// forgotten, is `None`.
pub fn take(session: PtySession) -> Option<OwnedFd> {
    let mut handed = HANDED.lock().unwrap_or_else(PoisonError::into_inner);
    let i = handed.iter().position(|(s, _)| *s == session)?;
    Some(handed.swap_remove(i).1)
}

/// The runtime ended `session`: a master still waiting is closed.
pub fn forget(session: PtySession) {
    drop(take(session));
}

/// One terminal of a document: the master side it took.
pub struct Pty {
    /// Kept so the terminal stays open for as long as the document holds it.
    _master: OwnedFd,
}

/// Why a terminal does no more: its program ended, or the data plane is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended;

impl Pty {
    /// Starts moving bytes for `master`.
    pub fn attach(master: OwnedFd) -> Self {
        Self { _master: master }
    }

    /// Output waiting for the guest, at most `buf.len()` bytes: how many were copied, `0` when
    /// there is none now. Never blocks.
    pub fn read(&self, _buf: &mut [u8]) -> Result<usize, Ended> {
        Err(Ended)
    }

    /// Queues `bytes` for the program's input: how many were taken. Never blocks.
    pub fn write(&self, _bytes: &[u8]) -> Result<usize, Ended> {
        Err(Ended)
    }

    /// Tells the program the terminal is `cols` by `rows` cells.
    pub fn resize(&self, _cols: u16, _rows: u16) -> Result<(), Ended> {
        Err(Ended)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_master_is_taken_once() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let session = PtySession::next();
        hand_over(session, a.into());
        assert!(take(PtySession::next()).is_none());
        assert!(take(session).is_some());
        assert!(take(session).is_none());
        let (c, _d) = std::os::unix::net::UnixStream::pair().unwrap();
        let other = PtySession::next();
        hand_over(other, c.into());
        forget(other);
        assert!(take(other).is_none());
    }
}
