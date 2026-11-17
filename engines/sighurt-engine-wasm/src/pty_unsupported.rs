//! The bytes of `sig://terminal` on a system where the runtime starts no terminal programs
//! (anything but Linux and macOS, for now: `sighurt_runtime::pty`). Nothing is ever handed
//! over, so [`take`] finds nothing and a [`Pty`] is never made. The module has the shape of the
//! Unix one (`pty.rs`), whose master is a descriptor polled with a wake-up pipe; Windows would
//! move ConPTY's two pipe handles instead (docs/rewrite/PORTING.md).
//!
//! Owns: nothing that runs. Must not own: starting or ending a program (the runtime's).

use sighurt_api::ids::PtySession;

/// Most bytes of a program's output held for the guest.
pub const MAX_OUTPUT: usize = 1 << 20;
/// Most bytes the guest may have waiting to reach the program.
pub const MAX_INPUT: usize = 64 * 1024;

/// A terminal's master: there is none on this system.
pub enum Master {}

/// Never called here: the runtime starts no program.
pub fn hand_over(_session: PtySession, master: Master) {
    match master {}
}

/// Nothing was handed over: always `None`.
pub fn take(_session: PtySession) -> Option<Master> {
    None
}

/// Nothing waits, so nothing is closed.
pub fn forget(_session: PtySession) {}

/// Why a terminal does no more.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended;

/// One terminal of a document: never made here.
pub struct Pty {
    never: Master,
}

impl Pty {
    /// Unreachable: there is no master to attach.
    pub fn attach(master: Master) -> Self {
        Self { never: master }
    }

    /// Unreachable.
    pub fn read(&self, _buf: &mut [u8]) -> Result<usize, Ended> {
        match self.never {}
    }

    /// Unreachable.
    pub fn write(&self, _bytes: &[u8]) -> Result<usize, Ended> {
        match self.never {}
    }

    /// Unreachable.
    pub fn resize(&self, _cols: u16, _rows: u16) -> Result<(), Ended> {
        match self.never {}
    }

    /// Unreachable.
    pub fn close(&mut self) {
        match self.never {}
    }
}
