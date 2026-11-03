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
//! Two frontends drive one session machinery: the Wayland window ([`sighurt_gui::run`]) and,
//! with the `frontend-tui` feature, the terminal (`sighurt_tui::run`). `--gui` and `--tui` pick
//! one; without either, `sig` opens in the terminal when stdout is one and `TERM` isn't `dumb`,
//! and in a window otherwise (decision 89).
//! `--dump URL` needs neither: it prints the page as text from a session with no terminal
//! (`sighurt_tui::dump`).
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

const USAGE: &str = "\
Usage: sig [OPTIONS] [URL|PATH ...]

Opens each URL or file in its own tab, or the home page when none is given. In a terminal
(stdout is one and TERM isn't dumb) sig runs in it, showing pages as kitty graphics; otherwise
it opens a Wayland window.

Options:
      --default-config     Print the built-in configuration and exit
      --dump URL           Print URL's page as reflowed text with numbered links on stdout and
                           exit, with no terminal or window (needs an engine that gives page
                           text: Servo, WPE WebKit or the demo engine); exit code 2 on a timeout
      --width N            With --dump, wrap to N columns (default 80)
      --timeout SECS       With --dump, give up after SECS seconds (default 30)
      --diagnostics FILE   Write the diagnostics to FILE as JSON at exit and on SIGUSR1
      --engine ENGINE      The engine this run starts with, overriding [engines] default
      --gui                Open a Wayland window, even from a terminal
      --tui                Run in the terminal on stdout. Its log, and anything else written
                           to stderr, goes to $XDG_STATE_HOME/sighurt/tui.log
                           (~/.local/state/sighurt/tui.log); the directory is created
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
    /// `--gui` or `--tui`, if given.
    frontend: Option<Choice>,
    /// `--dump URL`, with its `--width` and `--timeout`.
    dump: Option<Dump>,
}

/// What `--dump` is asked for. Only the terminal frontend runs it.
#[cfg_attr(not(feature = "frontend-tui"), allow(dead_code))]
struct Dump {
    open: OpenRequest,
    width: u16,
    timeout: u64,
}

/// Which frontend runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    Gui,
    Tui,
}

/// The command line: arguments to run with, or the exit code of an option that did its job.
fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, ExitCode> {
    let mut parsed = Args {
        open: Vec::new(),
        diagnostics: None,
        engine: None,
        frontend: None,
        dump: None,
    };
    let (mut width, mut timeout) = (None, None);
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
            "--dump" => match args.next() {
                Some(url) => {
                    parsed.dump = Some(Dump {
                        open: open_request(&url),
                        width: 80,
                        timeout: 30,
                    })
                }
                None => return Err(usage_error("--dump needs a URL")),
            },
            "--width" | "--timeout" => {
                let value = args.next().and_then(|v| v.parse::<u64>().ok());
                match (arg.as_str(), value) {
                    ("--width", Some(n @ 1..=1000)) => width = Some(n as u16),
                    ("--timeout", Some(n @ 1..=86_400)) => timeout = Some(n),
                    ("--width", _) => {
                        return Err(usage_error("--width needs a number from 1 to 1000"))
                    }
                    _ => return Err(usage_error("--timeout needs a number of seconds")),
                }
            }
            "--gui" | "--tui" => {
                let choice = if arg == "--gui" {
                    Choice::Gui
                } else {
                    Choice::Tui
                };
                if parsed.frontend.is_some_and(|chosen| chosen != choice) {
                    return Err(usage_error("--gui and --tui exclude each other"));
                }
                parsed.frontend = Some(choice);
            }
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(usage_error(&format!("unknown option {option}")))
            }
            _ => parsed.open.push(open_request(&arg)),
        }
    }
    match &mut parsed.dump {
        Some(_) if parsed.frontend.is_some() => {
            return Err(usage_error("--dump excludes --gui and --tui"))
        }
        Some(dump) => {
            dump.width = width.unwrap_or(dump.width);
            dump.timeout = timeout.unwrap_or(dump.timeout);
            if !parsed.open.is_empty() {
                return Err(usage_error(
                    "--dump takes one URL, and nothing else to open",
                ));
            }
        }
        None if width.is_some() || timeout.is_some() => {
            return Err(usage_error("--width and --timeout are for --dump"))
        }
        None => {}
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

/// The frontend to run (decision 89): the one asked for, else the terminal when stdout is one
/// (`terminal`) and `term` (`TERM`) names a terminal that isn't `dumb`, else the window. The
/// terminal needs the `frontend-tui` feature and a terminal; asking for it without either is an
/// error saying which.
fn choose(asked: Option<Choice>, terminal: bool, term: Option<&str>) -> Result<Choice, String> {
    let built = cfg!(feature = "frontend-tui");
    match asked {
        Some(Choice::Tui) if !built => {
            Err("this sig was built without the terminal frontend (feature frontend-tui)".into())
        }
        Some(Choice::Tui) if !terminal => {
            Err("--tui needs a terminal, and stdout isn't one".into())
        }
        Some(choice) => Ok(choice),
        None if built && terminal && term.is_some_and(|t| !t.is_empty() && t != "dumb") => {
            Ok(Choice::Tui)
        }
        None => Ok(Choice::Gui),
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

/// WebKit settings its web processes inherit, unless the environment sets them already: no GL
/// video sink, which paints H.264 and AV1 from software decoders solid green (it imports their
/// YU12 DMA-BUFs; `experiments/wpe-issues/REPORT.md`), and no accessibility bus, which `sig`
/// doesn't provide (each web process warned that it couldn't connect). Changes the
/// environment, so it runs before any thread.
#[cfg(feature = "engine-wpe")]
fn webkit_environment() {
    for (name, value) in [
        ("WEBKIT_GST_DISABLE_GL_SINK", "1"),
        ("WEBKIT_A11Y_BUS_ADDRESS", ""),
    ] {
        if std::env::var_os(name).is_none() {
            std::env::set_var(name, value);
        }
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
    {
        keep_webkit_off_sigusr1();
        webkit_environment();
    }
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(code) => return code,
    };
    let terminal = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let term = std::env::var("TERM").ok();
    let frontend = match choose(args.frontend, terminal, term.as_deref()) {
        Ok(frontend) => frontend,
        Err(error) => {
            eprintln!("sig: {error}");
            return ExitCode::from(2);
        }
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
    if let Some(dump) = args.dump {
        return run_dump(dump, args.engine, signals);
    }
    // The core applies this on top of the configuration, including every reload. It has to be
    // set on this thread: that is the thread that reads the configuration.
    sighurt_core::set_starting_engine(args.engine);
    let result = match frontend {
        Choice::Gui => {
            let options = sighurt_gui::Options {
                open: args.open,
                diagnostics: args.diagnostics,
            };
            sighurt_gui::run(options, signals).map_err(|error| {
                let no_display = std::env::var_os("WAYLAND_DISPLAY").is_none();
                match no_display && cfg!(feature = "frontend-tui") {
                    true => format!("{error} (no WAYLAND_DISPLAY; in a terminal, try sig --tui)"),
                    false => error,
                }
            })
        }
        #[cfg(feature = "frontend-tui")]
        Choice::Tui => {
            let options = sighurt_tui::Options {
                open: args.open,
                diagnostics: args.diagnostics,
            };
            sighurt_tui::run(options, signals)
        }
        #[cfg(not(feature = "frontend-tui"))]
        Choice::Tui => unreachable!("`choose` refuses the terminal without frontend-tui"),
    };
    match result {
        Ok(code) => code,
        Err(error) => failed(&error),
    }
}

/// Prints a failure to start or run and gives the exit status: 2 when the configuration file
/// stopped the start (`sighurt_runtime::persistence::DID_NOT_START`, a usage error like bad
/// arguments), 1 otherwise.
fn failed(error: &str) -> ExitCode {
    eprintln!("sig: {error}");
    if error.ends_with(sighurt_runtime::persistence::DID_NOT_START) {
        ExitCode::from(2)
    } else {
        ExitCode::FAILURE
    }
}

/// `--dump`: the page as text on stdout, in a session with no terminal and no window. `engine`
/// is `--engine`; without it, the engine the routing names when that one gives page text, else
/// one that does (WPE WebKit first).
#[cfg(feature = "frontend-tui")]
fn run_dump(dump: Dump, engine: Option<String>, signals: OwnedFd) -> ExitCode {
    let mut open = dump.open;
    open.engine = engine;
    let options = sighurt_tui::DumpOptions {
        open,
        width: dump.width,
        timeout: std::time::Duration::from_secs(dump.timeout),
    };
    match sighurt_tui::dump(options, &signals) {
        Ok(code) => code,
        Err(error) => failed(&error),
    }
}

#[cfg(not(feature = "frontend-tui"))]
fn run_dump(_: Dump, _: Option<String>, _: OwnedFd) -> ExitCode {
    eprintln!("sig: this sig was built without the terminal frontend (feature frontend-tui)");
    ExitCode::from(2)
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

    #[test]
    fn dump_takes_a_url_a_width_and_a_timeout() {
        let parsed = args(&["--dump", "example.com", "--width", "100"])
            .ok()
            .unwrap();
        let dump = parsed.dump.unwrap();
        assert_eq!(
            (dump.open.input.as_str(), dump.width, dump.timeout),
            ("example.com", 100, 30)
        );
        let parsed = args(&["--timeout", "5", "--dump", "x"]).ok().unwrap();
        assert_eq!(parsed.dump.map(|d| (d.width, d.timeout)), Some((80, 5)));
        assert!(args(&["--dump"]).is_err());
        assert!(args(&["--dump", "x", "--width", "0"]).is_err());
        assert!(args(&["--dump", "x", "--width", "wide"]).is_err());
        assert!(args(&["--dump", "x", "--tui"]).is_err());
        assert!(args(&["--dump", "x", "y"]).is_err());
        assert!(args(&["--width", "20"]).is_err());
        assert!(USAGE.contains("--dump URL"));
    }

    #[test]
    fn gui_and_tui_pick_the_frontend_and_exclude_each_other() {
        assert_eq!(
            args(&["--tui", "x"]).ok().unwrap().frontend,
            Some(Choice::Tui)
        );
        assert_eq!(args(&["--gui"]).ok().unwrap().frontend, Some(Choice::Gui));
        assert_eq!(
            args(&["--gui", "--gui"]).ok().unwrap().frontend,
            Some(Choice::Gui)
        );
        assert!(args(&["--gui", "--tui"]).is_err());
        assert!(USAGE.contains("--tui") && USAGE.contains("tui.log"));
    }

    #[test]
    fn a_terminal_runs_the_terminal_frontend_unless_dumb() {
        let tui = if cfg!(feature = "frontend-tui") {
            Choice::Tui
        } else {
            Choice::Gui
        };
        assert_eq!(choose(None, true, Some("xterm-kitty")), Ok(tui));
        assert_eq!(choose(None, true, Some("dumb")), Ok(Choice::Gui));
        assert_eq!(choose(None, true, None), Ok(Choice::Gui));
        assert_eq!(choose(None, false, Some("xterm-kitty")), Ok(Choice::Gui));
        assert_eq!(
            choose(Some(Choice::Gui), true, Some("xterm")),
            Ok(Choice::Gui)
        );
        let asked = choose(Some(Choice::Tui), false, Some("xterm"));
        assert!(asked.is_err());
        if cfg!(feature = "frontend-tui") {
            assert_eq!(
                choose(Some(Choice::Tui), true, Some("dumb")),
                Ok(Choice::Tui)
            );
        } else {
            assert!(asked.unwrap_err().contains("frontend-tui"));
        }
    }
}
