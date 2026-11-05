//! The programs `sig://terminal` runs: opening a pseudo-terminal, starting the program on its
//! slave side, and ending it ([`PageRequest::PtyOpen`]).
//!
//! A STUB until the terminal lane implements it (docs/rewrite/pages-2026-11-05.md, D): the pty
//! opened with rustix; the program spawned with `setsid`, `TIOCSCTTY` and every descriptor past
//! 2 marked close-on-exec; its working directory `$HOME` (the configuration's directory for
//! [`PtyProgram::EditConfig`]); `sig`'s environment without what `main` set or what names the
//! terminal `sig` runs in, plus `TERM=xterm-256color` and `COLORTERM`; `[pages.terminal]
//! command`, else `$SHELL`, else `/bin/sh` (`$VISUAL` or `$EDITOR` on the configuration for
//! `EditConfig`); a waiter thread reaping it; cancelling sends `SIGHUP` to its process group,
//! then `SIGKILL` after a grace period. No clone of the master is kept here, so a crash of `sig`
//! hangs the session up in the kernel. Until then every request is unsupported.
//!
//! The core has already decided the request: the page is `sig://terminal`, `[pages] terminal` is
//! on, the user pressed a key or clicked in it, and the limits hold. The request stays held in
//! the core after its answer, so closing, navigating or quitting cancels it, and
//! [`Ptys::cancel`] ends the program.
//!
//! Owns: the programs. Must not own: who may have one (the core) or the bytes (the engine).

use std::path::Path;

use sighurt_api::ids::RequestId;
use sighurt_api::services::{PtyProgram, ServiceError, ServiceResult};

/// What a program is started with, from the configuration and the session's paths.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// `[pages.terminal] command`, if set: the shell to run instead of `$SHELL`.
    pub command: Option<&'a str>,
    /// The configuration file, for [`PtyProgram::EditConfig`].
    pub config: Option<&'a Path>,
}

/// The programs running, by the request that started each.
#[derive(Debug, Default)]
pub struct Ptys {}

impl Ptys {
    /// No programs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts `program` on a new terminal of `cols` by `rows` cells for `request`:
    /// `ServiceReply::Pty` with its master side.
    pub fn open(
        &mut self,
        _request: RequestId,
        _program: PtyProgram,
        _cols: u16,
        _rows: u16,
        _launch: Launch<'_>,
    ) -> ServiceResult {
        Err(ServiceError::Unsupported)
    }

    /// Ends the program `request` started, if it is one of these; whether it was.
    pub fn cancel(&mut self, _request: RequestId) -> bool {
        false
    }

    /// Ends every program, at shutdown.
    pub fn shutdown(&mut self) {}
}
