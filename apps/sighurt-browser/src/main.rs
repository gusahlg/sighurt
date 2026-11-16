//! `sig`, the Sighurt browser executable.
//!
//! Process-wide setup and the choice of frontend. Setup follows decision 26, before any engine
//! starts: the panic hook logs and never exits; rustls gets the aws-lc-rs provider; the one
//! `log` logger is Servo's when Servo is compiled in and enabled, `env_logger` otherwise (the
//! frontend's launch does that). SIGUSR1 prints the diagnostics, SIGTERM, SIGINT and SIGHUP quit
//! cleanly (the session is saved; the terminal frontend replaces SIGHUP's handler with its own);
//! their handlers only raise the request's flag and write to a pipe that wakes the loop.
//! SIGXFSZ is caught and does nothing, so a
//! write over the file size limit (`ulimit -f`) fails with EFBIG, as one to a full disk does,
//! instead of killing `sig`; children get the default back at exec. With WPE WebKit
//! compiled in, its JavaScriptCore suspends threads with SIGUSR2 instead of SIGUSR1
//! (`keep_webkit_off_sigusr1`). The one forced exit is the runtime's, after an engine that could
//! not shut down (decision 54), and the frontend is asked to clean up first.
//!
//! Two frontends drive one session machinery: with the `frontend-gui` feature the Wayland window
//! (`sighurt_gui::run`) and with `frontend-tui` the terminal (`sighurt_tui::run`). `--gui` and
//! `--tui` pick one; without either, `sig` opens in the terminal when stdout is one and `TERM`
//! isn't `dumb`, and in a window otherwise (decision 89). A build with one frontend says so when
//! asked for the other. On Windows a console has no `TERM`, so a console on stdout is the
//! terminal there unless `TERM` says `dumb`. Windows has no signals: a console control handler
//! quits cleanly for Ctrl+C, Ctrl+Break and the console closing (a second Ctrl+C or Ctrl+Break
//! while that quit is under way ends the process at once), and nothing asks for the report there
//! (docs/rewrite/PORTING.md).
//! `--dump URL` needs neither: it prints the page as text from a session with no terminal
//! (`sighurt_tui::dump`).
//!
//! Must not own: browser policy (the core), the owner loop (the frontend), drawing (the chrome
//! and the engines), or any engine's internals.
//!
//! Sighurt is based on Oxide by Nikhil Ranjan (<https://github.com/niklabh/oxide>).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use sighurt_core::config::DEFAULT_CONFIG;
use sighurt_core::router;
use sighurt_core::OpenRequest;
use sighurt_runtime::signals::Signals;

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

Signals: SIGUSR1 prints the diagnostics on stderr; SIGTERM, SIGINT and SIGHUP quit, saving the
tabs.
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
/// terminal needs the `frontend-tui` feature and a terminal, the window the `frontend-gui`
/// feature; asking for one without what it needs is an error saying which.
fn choose(asked: Option<Choice>, terminal: bool, term: Option<&str>) -> Result<Choice, String> {
    let (gui, tui) = (
        cfg!(feature = "frontend-gui"),
        cfg!(feature = "frontend-tui"),
    );
    let no_gui = "this sig was built without the window (feature frontend-gui)";
    match asked {
        Some(Choice::Gui) if !gui => Err(no_gui.into()),
        Some(Choice::Tui) if !tui => {
            Err("this sig was built without the terminal frontend (feature frontend-tui)".into())
        }
        Some(Choice::Tui) if !terminal => {
            Err("--tui needs a terminal, and stdout isn't one".into())
        }
        Some(choice) => Ok(choice),
        None if tui && terminal && names_a_terminal(term) => Ok(Choice::Tui),
        None if gui => Ok(Choice::Gui),
        None if tui => Err(format!(
            "{no_gui}; run it in a terminal (stdout one, and TERM set and not dumb)"
        )),
        None => {
            Err("this sig was built with no frontend (features frontend-gui, frontend-tui)".into())
        }
    }
}

/// Whether `TERM` names a terminal the frontend can use: set and not `dumb`. A Windows console
/// sets none, so there no `TERM` is a terminal too.
fn names_a_terminal(term: Option<&str>) -> bool {
    match term {
        Some(term) => !term.is_empty() && term != "dumb",
        None => cfg!(windows),
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

/// The signals `sig` handles: each handler raises its request's flag, then writes a byte to a
/// pipe to wake the loop (`sighurt_runtime::signals`). SIGXFSZ gets a handler that does
/// nothing: the write that went over the limit then fails with EFBIG.
#[cfg(unix)]
fn install_signal_handlers() -> std::io::Result<Signals> {
    use sighurt_runtime::signals::{request_for, Pending};
    use std::sync::Arc;
    // SAFETY: the action does nothing.
    unsafe { signal_hook_registry::register(libc::SIGXFSZ, || {})? };
    let (read, write) = signal_pipe()?;
    let write = Arc::new(write);
    let pending = Arc::new(Pending::default());
    for signal in [libc::SIGUSR1, libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
        let write = Arc::clone(&write);
        let pending = Arc::clone(&pending);
        let request = request_for(signal).expect("each of these asks for something");
        // SAFETY: the action stores to an atomic flag (lock-free) and calls write(2) on a
        // non-blocking pipe, both async-signal-safe; it allocates nothing and takes no lock. A
        // full pipe loses the byte, not the request: the flag holds it, and the full pipe
        // wakes the loop anyway.
        unsafe {
            signal_hook_registry::register(signal, move || {
                pending.raise(request);
                let _ = rustix::io::write(&*write, &[signal as u8]);
            })?;
        }
    }
    Ok(Signals::from_pipe(read, pending))
}

/// A close-on-exec, non-blocking pipe.
#[cfg(all(unix, not(target_vendor = "apple")))]
fn signal_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    use rustix::pipe::{pipe_with, PipeFlags};
    Ok(pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?)
}

/// A close-on-exec, non-blocking pipe. macOS has no `pipe2`; nothing can fork between the two
/// steps, since `main` makes it before any thread starts.
#[cfg(target_vendor = "apple")]
fn signal_pipe() -> std::io::Result<(std::os::fd::OwnedFd, std::os::fd::OwnedFd)> {
    let (read, write) = rustix::pipe::pipe()?;
    for end in [&read, &write] {
        rustix::io::fcntl_setfd(end, rustix::io::FdFlags::CLOEXEC)?;
        rustix::io::ioctl_fionbio(end, true)?;
    }
    Ok((read, write))
}

/// The console control handler, which sends a quit for Ctrl+C, Ctrl+Break, and the console
/// closing, the user logging off or the system shutting down. For the last three Windows ends
/// the process when the handler returns, so it does not return: the owner loop quits and
/// `main` returns, which ends the process with the session saved (Windows allows 5 s after a
/// close). A second Ctrl+C or Ctrl+Break is left to the default handler, which ends the process
/// at once.
#[cfg(windows)]
fn install_signal_handlers() -> std::io::Result<Signals> {
    use sighurt_runtime::signals::{Request, Sender};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use windows_sys::core::BOOL;
    use windows_sys::Win32::System::Console::{
        SetConsoleCtrlHandler, CTRL_BREAK_EVENT, CTRL_CLOSE_EVENT, CTRL_C_EVENT, CTRL_LOGOFF_EVENT,
        CTRL_SHUTDOWN_EVENT,
    };

    static SENDER: OnceLock<Sender> = OnceLock::new();
    static QUITTING: AtomicBool = AtomicBool::new(false);

    unsafe extern "system" fn handler(event: u32) -> BOOL {
        let Some(sender) = SENDER.get() else {
            return 0;
        };
        match event {
            CTRL_C_EVENT | CTRL_BREAK_EVENT => {
                if QUITTING.swap(true, Ordering::AcqRel) {
                    return 0;
                }
                sender.send(Request::Quit);
                1
            }
            CTRL_CLOSE_EVENT | CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => {
                QUITTING.store(true, Ordering::Release);
                sender.send(Request::Quit);
                loop {
                    std::thread::park();
                }
            }
            _ => 0,
        }
    }

    let (sender, signals) = Signals::channel();
    let _ = SENDER.set(sender);
    // SAFETY: the handler only reads statics and sends on a queue; it runs on a thread of its
    // own that Windows starts.
    if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(signals)
}

/// No signal reaches the loop here (see the module documentation).
#[cfg(not(any(unix, windows)))]
fn install_signal_handlers() -> std::io::Result<Signals> {
    Ok(Signals::none())
}

// With no frontend every arm of the frontend's match is unreachable, and so is what follows it.
#[cfg_attr(
    not(any(feature = "frontend-gui", feature = "frontend-tui")),
    allow(unreachable_code, unused_variables)
)]
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
    // `--dump` runs neither frontend (parse_args refuses it with --gui or --tui), so a build
    // without the window can dump with stdout redirected.
    let chosen = match args.dump {
        Some(_) => Ok(Choice::Tui),
        None => choose(args.frontend, terminal, term.as_deref()),
    };
    let frontend = match chosen {
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
    // Typed: a build with no frontend has only unreachable arms.
    let result: Result<ExitCode, String> = match frontend {
        #[cfg(feature = "frontend-gui")]
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
        #[cfg(not(feature = "frontend-gui"))]
        Choice::Gui => unreachable!("`choose` refuses the window without frontend-gui"),
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
fn run_dump(dump: Dump, engine: Option<String>, signals: Signals) -> ExitCode {
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
fn run_dump(_: Dump, _: Option<String>, _: Signals) -> ExitCode {
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
                // Windows' drive path is `/C:/…`, with forward slashes.
                format!(
                    "file://{}{}",
                    if cfg!(windows) { "/" } else { "" },
                    file.display()
                        .to_string()
                        .replace(std::path::MAIN_SEPARATOR, "/")
                )
                .as_str(),
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
        let (gui, built) = (
            cfg!(feature = "frontend-gui"),
            cfg!(feature = "frontend-tui"),
        );
        // The window where one is built; an error saying it isn't otherwise.
        let window = |chosen: Result<Choice, String>| match gui {
            true => assert_eq!(chosen, Ok(Choice::Gui)),
            false => assert!(chosen.unwrap_err().contains("frontend-gui")),
        };
        match built {
            true => assert_eq!(choose(None, true, Some("xterm-kitty")), Ok(Choice::Tui)),
            false => window(choose(None, true, Some("xterm-kitty"))),
        }
        window(choose(None, true, Some("dumb")));
        match built && cfg!(windows) {
            true => assert_eq!(choose(None, true, None), Ok(Choice::Tui)),
            false => window(choose(None, true, None)),
        }
        window(choose(None, false, Some("xterm-kitty")));
        window(choose(Some(Choice::Gui), true, Some("xterm")));
        let asked = choose(Some(Choice::Tui), false, Some("xterm"));
        assert!(asked.is_err());
        if built {
            assert_eq!(
                choose(Some(Choice::Tui), true, Some("dumb")),
                Ok(Choice::Tui)
            );
        } else {
            assert!(asked.unwrap_err().contains("frontend-tui"));
        }
    }
}
