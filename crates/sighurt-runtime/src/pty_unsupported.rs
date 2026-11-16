//! The programs of `sig://terminal` and `sig://config` on a system this build cannot start them
//! on: anything but Linux, for now. [`Ptys::open`] refuses with the reason ([`UNSUPPORTED`]),
//! no program is ever started, and [`editor`] finds none. The module has the shape of the Linux
//! one (`pty.rs`), which needs pidfds and `close_range`; macOS needs a kqueue `EVFILT_PROC`
//! watch and a loop that closes descriptors in the child instead, Windows ConPTY and a job
//! object (docs/rewrite/PORTING.md).
//!
//! Owns: the refusal. Must not own: the policy (the core's) or what the page shows.

use std::ffi::OsString;
use std::path::Path;
use std::sync::Arc;

use sighurt_api::ids::RequestId;
use sighurt_api::services::{PtyProgram, ServiceError, ServiceResult};

/// Whether this build starts programs on this system: never here.
pub const RUNS_PROGRAMS: bool = false;

/// Why no program starts here.
pub const UNSUPPORTED: &str = "sig://terminal and sig://config run programs on Linux only, \
                               for now: this system has no terminal programs in sig";

/// What a program would be started with, from the configuration and the session's paths.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// `[pages.terminal] command`.
    pub command: Option<&'a str>,
    /// `[pages.config] editor`.
    pub editor: Option<&'a str>,
    /// The configuration file.
    pub config: Option<&'a Path>,
}

/// Where an editor comes from. [`editor`] finds none here, so this is never made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorFrom {
    /// `[pages.config] editor`.
    Setting,
    /// `$VISUAL`.
    Visual,
    /// `$EDITOR`.
    Editor,
    /// A known editor on `PATH`.
    Path,
}

impl EditorFrom {
    /// Its name in `PageInfo::Editor`'s `from` record.
    pub fn name(self) -> &'static str {
        match self {
            Self::Setting => "setting",
            Self::Visual => "VISUAL",
            Self::Editor => "EDITOR",
            Self::Path => "PATH",
        }
    }
}

/// No editor: none can run here.
pub fn editor(
    _setting: Option<&str>,
    _vars: &[(OsString, OsString)],
) -> Option<(Vec<OsString>, EditorFrom)> {
    None
}

/// No programs, ever.
#[derive(Debug, Default)]
pub struct Ptys;

impl Ptys {
    /// No programs. Nothing ends, so `wake` is never called.
    pub fn new(_wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        Self
    }

    /// Refuses: [`UNSUPPORTED`].
    pub fn open(
        &mut self,
        _request: RequestId,
        _program: PtyProgram,
        _cols: u16,
        _rows: u16,
        _launch: Launch<'_>,
    ) -> ServiceResult {
        Err(ServiceError::Failed(UNSUPPORTED.into()))
    }

    /// Nothing was started, so nothing is cancelled.
    pub fn cancel(&mut self, _request: RequestId) -> bool {
        false
    }

    /// Nothing ends.
    pub fn ended(&mut self) -> Vec<(RequestId, Option<i32>, PtyProgram)> {
        Vec::new()
    }

    /// Nothing to end.
    pub fn shutdown(&mut self) {}
}
