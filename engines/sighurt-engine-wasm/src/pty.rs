//! The terminal page's data plane: the bytes between a guest and the master side of a
//! pseudo-terminal the runtime opened for it ([`sighurt_api::services::PtyHandle`]).
//!
//! A STUB until the terminal lane implements it (docs/rewrite/pages-2026-11-05.md, D): a
//! reader thread filling a 1 MiB queue and stopping when it is full, so the program blocks;
//! non-blocking writes through a 64 KiB queue; resizing with `TIOCSWINSZ`; everything dropped
//! with the document's `HostState`. Until then a terminal reads, writes and resizes nothing, and
//! says it has ended.
//!
//! Owns: moving bytes. Must not own: starting or ending the program (the runtime does), or
//! deciding who may have a terminal (the core).

use sighurt_api::services::PtyHandle;

/// One terminal of a document: the master side the runtime handed over.
pub struct Pty {
    /// Kept so the terminal stays open for as long as the document holds it.
    _master: PtyHandle,
}

/// Why a terminal does no more: its program ended, or the data plane is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended;

impl Pty {
    /// Starts moving bytes for `master`.
    pub fn attach(master: PtyHandle) -> Self {
        Self { _master: master }
    }

    /// Output waiting for the guest, at most `buf.len()` bytes: how many were copied, `0` when
    /// there is none now.
    pub fn read(&self, _buf: &mut [u8]) -> Result<usize, Ended> {
        Err(Ended)
    }

    /// Queues `bytes` for the program's input: how many were taken.
    pub fn write(&self, _bytes: &[u8]) -> Result<usize, Ended> {
        Err(Ended)
    }

    /// Tells the program the terminal is `cols` by `rows` cells.
    pub fn resize(&self, _cols: u16, _rows: u16) -> Result<(), Ended> {
        Err(Ended)
    }
}
