//! The terminal frontend and `--dump` on a system with neither a Unix terminal nor a Windows
//! console: both refuse, saying so (docs/rewrite/PORTING.md).
//!
//! Owns: the refusal and the options' shapes. Must not own: anything that runs.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use sighurt_core::OpenRequest;
use sighurt_runtime::signals::Signals;

/// Why nothing runs.
const UNSUPPORTED: &str =
    "the terminal frontend needs a Unix terminal or a Windows console; this system has neither";

/// What `sig` asks the terminal frontend for, as on Unix.
pub struct Options {
    /// URLs and paths to open, as the core takes them.
    pub open: Vec<OpenRequest>,
    /// Where to write the diagnostics as JSON.
    pub diagnostics: Option<PathBuf>,
}

/// What `sig --dump` asks for, as on Unix.
pub struct DumpOptions {
    /// The page to print.
    pub open: OpenRequest,
    /// Columns to wrap to.
    pub width: u16,
    /// How long to wait for the page before giving up.
    pub timeout: Duration,
}

/// Refuses: no terminal frontend here.
pub fn run(_options: Options, _signals: Signals) -> Result<ExitCode, String> {
    Err(UNSUPPORTED.into())
}

/// Refuses: `--dump` runs on the terminal frontend's I/O.
pub fn dump(_options: DumpOptions, _signals: &Signals) -> Result<ExitCode, String> {
    Err(UNSUPPORTED.into())
}
