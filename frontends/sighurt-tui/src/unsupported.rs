//! The terminal frontend and `--dump` on a system without a Unix terminal (Windows): both refuse,
//! saying so. A port needs a console backend (`SetConsoleMode` with VT input and output, resize
//! from `WINDOW_BUFFER_SIZE_EVENT` records, a console control handler in place of the signals)
//! and an owner loop that waits on console handles (docs/rewrite/PORTING.md).
//!
//! Owns: the refusal and the options' shapes. Must not own: anything that runs.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use sighurt_core::OpenRequest;
use sighurt_runtime::signals::Signals;

/// Why nothing runs.
const UNSUPPORTED: &str =
    "the terminal frontend needs a Unix terminal; this system has none in sig";

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

/// Refuses: `--dump` writes through the terminal frontend's Unix I/O.
pub fn dump(_options: DumpOptions, _signals: &Signals) -> Result<ExitCode, String> {
    Err(UNSUPPORTED.into())
}
