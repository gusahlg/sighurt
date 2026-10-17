//! `sig`, the Sighurt browser executable.
//!
//! Process-wide setup and the choice of frontend. Setup follows decision 26, before any engine
//! starts: the panic hook logs and never exits; rustls gets the aws-lc-rs provider; the one
//! `log` logger is Servo's when Servo is compiled in and enabled, `env_logger` otherwise (the
//! frontend's launch does that). SIGUSR1 prints the diagnostics, SIGTERM and SIGINT quit cleanly
//! (the session is saved); their handlers only write to a pipe the loop reads. With WPE WebKit
//! compiled in, its JavaScriptCore suspends threads with SIGUSR2 instead of SIGUSR1
//! (`keep_webkit_off_sigusr1`). The one forced exit is the runtime's, after an engine that could
//! not shut down (decision 54), and the frontend is asked to clean up first.
//!
//! The Wayland window is the only frontend today ([`sighurt_gui::run`]).
//!
//! Must not own: browser policy (the core), the owner loop (the frontend), drawing (the chrome
//! and the engines), or any engine's internals.
//!
//! Sighurt is based on Oxide by Nikhil Ranjan (<https://github.com/niklabh/oxide>).

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use sighurt_core::config::DEFAULT_CONFIG;
use sighurt_core::router;
use sighurt_core::OpenRequest;
use sighurt_gui::Options;

const USAGE: &str = "\
Usage: sig [OPTIONS] [URL|PATH ...]

Opens each URL or file in its own tab, or the home page when none is given.

Options:
      --default-config     Print the built-in configuration and exit
      --diagnostics FILE   Write the diagnostics to FILE as JSON at exit and on SIGUSR1
      --engine ENGINE      The engine this run starts with, overriding [engines] default
  -h, --help               Print this help and exit
  -V, --version            Print the version and exit

Environment:
  SIG_DIAG=1      Print a diagnostics line for every page frame and chrome paint on stderr
  SIG_AUDIT_GL=1  Count Servo's GL reads of page pixels too (times every GL call; for tests)
  RUST_LOG        Log filter, such as RUST_LOG=debug

Signals: SIGUSR1 prints the diagnostics on stderr; SIGTERM and SIGINT quit, saving the tabs.
";

/// What the command line asks for.
struct Args {
    /// URLs and paths to open, as the core takes them.
    open: Vec<OpenRequest>,
    /// Where to write the diagnostics as JSON.
    diagnostics: Option<PathBuf>,
    /// The engine this run starts with, overriding `[engines] default`.
    engine: Option<String>,
}

/// The command line: arguments to run with, or the exit code of an option that did its job.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, ExitCode> {
    let mut parsed = Args {
        open: Vec::new(),
        diagnostics: None,
        engine: None,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Err(ExitCode::SUCCESS);
            }
            "-V" | "--version" => {
                println!(
                    "sig {} (Sighurt, based on Oxide by Nikhil Ranjan)\n\
                     License AGPL-3.0-or-later <https://gnu.org/licenses/agpl.html>; \
                     Oxide's code is Apache-2.0. There is NO WARRANTY.",
                    env!("CARGO_PKG_VERSION")
                );
                return Err(ExitCode::SUCCESS);
            }
            "--default-config" => {
                print!("{DEFAULT_CONFIG}");
                return Err(ExitCode::SUCCESS);
            }
            "--diagnostics" => match args.next() {
                Some(file) => parsed.diagnostics = Some(file.into()),
                None => return Err(usage_error("--diagnostics needs a file")),
            },
            "--engine" => match args.next() {
                Some(name) => parsed.engine = Some(name),
                None => return Err(usage_error("--engine needs an engine")),
            },
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(usage_error(&format!("unknown option {option}")))
            }
            _ => parsed.open.push(open_request(&arg)),
        }
    }
    Ok(parsed)
}

fn usage_error(message: &str) -> ExitCode {
    eprint!("sig: {message}\n\n{USAGE}");
    ExitCode::from(2)
}

/// A path that exists becomes an absolute `file:` URL; anything else is left for the core to
/// read as an address or a search.
fn open_request(arg: &str) -> OpenRequest {
    let path = Path::new(arg);
    let input = match path.exists() {
        true => router::file_url(&std::path::absolute(path).unwrap_or_else(|_| path.into())),
        false => arg.to_owned(),
    };
    OpenRequest {
        input,
        engine: None,
    }
}

/// The panic hook of decision 26: the default report, logged too, and never an exit.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Panics Servo catches itself (image decoding).
        #[cfg(feature = "engine-servo")]
        if sighurt_engine_servo::should_panic_hook_suppress_termination() {
            return;
        }
        // Servo turns a script thread's logged panic into `notify_crashed`.
        log::error!("panic: {info}");
        default(info);
    }));
}

/// Gives WebKit's JavaScriptCore SIGUSR2 for suspending threads (`JSC_SIGNAL_FOR_GC`), unless
/// the environment names a signal already. Its default is SIGUSR1, `sig`'s diagnostics signal:
/// WebKit's handler would replace `sig`'s, and a SIGUSR1 sent to `sig` would suspend the thread
/// that took it, for good (decision 78). Changes the environment, so it runs before any thread.
#[cfg(feature = "engine-wpe")]
fn keep_webkit_off_sigusr1() {
    if std::env::var_os("JSC_SIGNAL_FOR_GC").is_none() {
        std::env::set_var("JSC_SIGNAL_FOR_GC", libc::SIGUSR2.to_string());
    }
}

/// The signals `sig` handles, and the pipe their handlers write the signal number to.
fn install_signal_handlers() -> std::io::Result<OwnedFd> {
    use rustix::pipe::{pipe_with, PipeFlags};
    let (read, write) = pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
    let write = Arc::new(write);
    for signal in [libc::SIGUSR1, libc::SIGTERM, libc::SIGINT] {
        let write = Arc::clone(&write);
        // SAFETY: the action only calls write(2) on a non-blocking pipe, which is
        // async-signal-safe; it allocates nothing and takes no lock. A full pipe loses the
        // byte, and the loop already has the same signal to read.
        unsafe {
            signal_hook_registry::register(signal, move || {
                let _ = rustix::io::write(&*write, &[signal as u8]);
            })?;
        }
    }
    Ok(read)
}

fn main() -> ExitCode {
    #[cfg(feature = "engine-wpe")]
    keep_webkit_off_sigusr1();
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(code) => return code,
    };
    install_panic_hook();
    if rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .is_err()
    {
        log::warn!("a TLS provider was already installed");
    }
    let signals = match install_signal_handlers() {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("sig: cannot handle signals: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The core applies this on top of the configuration, including every reload. It has to be
    // set on this thread: that is the thread that reads the configuration.
    sighurt_core::set_starting_engine(args.engine);
    let options = Options {
        open: args.open,
        diagnostics: args.diagnostics,
    };
    match sighurt_gui::run(options, signals) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("sig: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Args, ExitCode> {
        parse_args(list.iter().map(|s| s.to_string()))
    }

    #[test]
    fn paths_become_file_urls_and_the_rest_stays() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("page.html");
        std::fs::write(&file, "").unwrap();
        let parsed = args(&[file.to_str().unwrap(), "example.com", "demo:pattern"])
            .ok()
            .unwrap();
        let inputs: Vec<_> = parsed.open.iter().map(|r| r.input.as_str()).collect();
        assert_eq!(
            inputs,
            [
                format!("file://{}", file.display()).as_str(),
                "example.com",
                "demo:pattern"
            ]
        );
    }

    #[test]
    fn options() {
        let parsed = args(&["--diagnostics", "/tmp/d.json", "x"]).ok().unwrap();
        assert_eq!(parsed.diagnostics, Some(PathBuf::from("/tmp/d.json")));
        assert_eq!(parsed.open.len(), 1);
        assert!(args(&["--diagnostics"]).is_err());
        assert!(args(&["--frobnicate"]).is_err());
        assert!(args(&["--version"]).is_err());
    }

    #[test]
    fn engine_is_the_engine_this_run_starts_with() {
        let parsed = args(&["--engine", "wpe", "example.com"]).ok().unwrap();
        assert_eq!(parsed.engine.as_deref(), Some("wpe"));
        assert_eq!(parsed.open.len(), 1);
        assert_eq!(parsed.open[0].input, "example.com");
        assert!(args(&["--engine"]).is_err());
        assert!(args(&["--nope"]).is_err());
        assert!(USAGE.contains("--engine ENGINE"));
    }
}
