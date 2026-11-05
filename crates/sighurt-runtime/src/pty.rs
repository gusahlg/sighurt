//! The programs `sig://terminal` runs: opening a pseudo-terminal, starting the program on its
//! slave side, handing the master to the engine, and ending the program
//! ([`PageRequest::PtyOpen`](sighurt_api::services::PageRequest::PtyOpen)).
//!
//! A STUB until the terminal lane implements it (docs/rewrite/pages-2026-11-05.md, D and
//! A2–A6): the pty opened with rustix, every descriptor close-on-exec; the spawn recipe of A6
//! (argv, environment and paths prepared before a `pre_exec` that allocates nothing; a new
//! session, the slave as controlling terminal and fds 0–2, default signal dispositions and
//! mask, sane termios and the size set); the working directory `$HOME` (the configuration's
//! directory for [`PtyProgram::EditConfig`]); `sig`'s environment without the variables A6 lists,
//! plus `TERM` and `COLORTERM`; `[pages.terminal] command`, else `$SHELL`, else `/bin/sh`,
//! split into executable and arguments without a shell (`$VISUAL` or `$EDITOR` on the
//! configuration file for `EditConfig`); the master handed to the engine with
//! `sighurt_engine_wasm::pty::hand_over` under a new [`PtySession`] before the answer; the
//! child held through a pidfd and reaped; cancelling sends `SIGHUP` to its process group, then
//! `SIGKILL` after a grace period. No clone of the master is kept here, so a crash of `sig`
//! hangs the session up in the kernel. Until then every request is unsupported.
//!
//! The core has already decided the request and tracks the session. Once the program has ended
//! and been reaped, whether by itself or by [`Ptys::cancel`], [`Ptys::ended`] reports it, and the
//! session answers the request again with `ServiceReply::PtyEnded`, which ends the core's
//! record. [`Ptys::shutdown`] runs before the engines shut down.
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
    /// No programs. `wake` wakes the owner loop when a program has ended.
    pub fn new(_wake: std::sync::Arc<dyn Fn() + Send + Sync>) -> Self {
        Self::default()
    }

    /// Starts `program` on a new terminal of `cols` by `rows` cells for `request`:
    /// `ServiceReply::Pty` with a new session, its master handed to the engine.
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

    /// Ends the program `request` started, if it is one of these: `SIGHUP`, then `SIGKILL`
    /// after the grace period. Whether it was one. Its end comes through [`Self::ended`].
    pub fn cancel(&mut self, _request: RequestId) -> bool {
        false
    }

    /// The programs that ended and were reaped since the last call, with their exit status
    /// (`None` when a signal ended them).
    pub fn ended(&mut self) -> Vec<(RequestId, Option<i32>)> {
        Vec::new()
    }

    /// Ends every program, at shutdown, before the engines shut down.
    pub fn shutdown(&mut self) {}
}
