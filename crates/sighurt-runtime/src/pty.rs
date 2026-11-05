//! The programs of `sig://terminal`: opening a pseudo-terminal, starting the program on its
//! slave side, ending and reaping it.
//!
//! The core has authorised a `PtyOpen` (the page's grant, `[pages.terminal] enabled`, a
//! deliberate action in the shown page, the limits) before anything here runs. [`exec`] builds
//! what runs from a closed choice the guest made ([`PtyProgram`]): the guest never names a
//! program, an argument, a directory or a variable. [`Ptys::open`] opens the pty with its size
//! set, starts the program as the leader of a new session with the slave as its controlling
//! terminal, and gives the master away ([`Opened`]) for the Wasm engine to take; nothing here
//! keeps a copy of it. Every descriptor is close-on-exec from its creation, so no other child of
//! `sig` inherits a master or a slave.
//!
//! Each program has a watcher thread that holds the unreaped child and its pidfd. Cancelling
//! ([`Ptys::cancel`]; closing the tab, navigating away, reloading, quitting) wakes it: SIGHUP and
//! SIGCONT to the program's process group, SIGKILL to the group after [`GRACE`], and the program
//! reaped. Signals go out only while the program is unreaped, so its pid, which is also its
//! group's id, cannot have been reused. The program's own exit is reaped the same way and
//! recorded for the page ([`Opened::exit`]).
//!
//! What is guaranteed: the program `sig` started is signalled and reaped; no zombie and no child
//! of `sig` remains. What is best effort, as in any terminal: jobs the shell put in process
//! groups of their own, programs that ignore SIGHUP after `disown`, and detached sessions get the
//! kernel's hang-up when the terminal closes and may survive it.
//!
//! The child inherits nothing it was not given: descriptors 0 to 2 (the slave), an empty signal
//! mask, default dispositions, and `sig`'s environment without what `sig` itself set and without
//! the variables that describe the terminal `sig` was started from ([`scrubbed`]).
//!
//! Owns: the child processes until they are reaped. Must not own: the master's bytes (the Wasm
//! engine's `pty` module carries them), the policy (the core's), or what the page shows.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;
use rustix::pipe::PipeFlags;
use rustix::process::{Pid, PidfdFlags, Signal};
use rustix::pty::OpenptFlags;
use rustix::termios::{InputModes, OptionalActions, Winsize};
use sighurt_api::ids::RequestId;
use sighurt_api::services::{PtyProgram, ServiceError};

/// Most programs started and not yet reaped in one process. The core holds pages to the same
/// number.
pub const MAX_SESSIONS: usize = 8;
/// How long a program's process group has after SIGHUP before it is killed.
pub const GRACE: Duration = Duration::from_secs(2);
/// How long [`Ptys::shutdown`] waits past [`GRACE`] for killed programs to be reaped before it
/// gives up on them (a process stuck in the kernel cannot die until it leaves it).
const REAP_WAIT: Duration = Duration::from_secs(1);
/// The editor when neither `VISUAL` nor `EDITOR` names one.
const EDITOR: &str = "vi";
/// The shell when neither the configuration nor `SHELL` names one.
const SHELL: &str = "/bin/sh";

/// What a program is started with, from the configuration and the session's paths.
#[derive(Clone, Copy, Debug)]
pub struct Launch<'a> {
    /// `[pages.terminal] command`, if set: the shell to run instead of `$SHELL`.
    pub command: Option<&'a str>,
    /// The configuration file, for [`PtyProgram::EditConfig`].
    pub config: Option<&'a Path>,
}

/// A program ready to start: its argument vector, working directory and whole environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Exec {
    /// Never empty: [`exec`] makes sure of it.
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

/// What runs for `program`. `launch.command` is split at white space with no shell and no
/// quoting; `vars` is `sig`'s environment. The working directory is `HOME` (`/` without one),
/// and the directory of the configuration file for `EditConfig`, which fails without one.
pub fn exec(
    program: PtyProgram,
    launch: Launch<'_>,
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Exec, String> {
    let vars: Vec<_> = vars.into_iter().collect();
    let var = |name: &str| {
        vars.iter()
            .find(|(key, value)| key == name && !value.is_empty())
            .map(|(_, value)| value.clone())
    };
    let words = |line: &OsStr| -> Vec<OsString> {
        line.to_string_lossy()
            .split_whitespace()
            .map(OsString::from)
            .collect()
    };
    let home = var("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    let (argv, cwd) = match program {
        PtyProgram::Shell => {
            let configured = launch.command.map(|line| words(OsStr::new(line)));
            let argv = configured
                .filter(|argv| !argv.is_empty())
                .or_else(|| var("SHELL").map(|shell| vec![shell]))
                .unwrap_or_else(|| vec![SHELL.into()]);
            (argv, home)
        }
        PtyProgram::EditConfig => {
            let config = launch
                .config
                .ok_or("this build has no configuration file")?;
            let mut argv = var("VISUAL")
                .or_else(|| var("EDITOR"))
                .map(|line| words(&line))
                .filter(|argv| !argv.is_empty())
                .unwrap_or_else(|| vec![EDITOR.into()]);
            argv.push(config.as_os_str().to_owned());
            let cwd = config
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .map_or(home, Path::to_path_buf);
            (argv, cwd)
        }
    };
    Ok(Exec {
        argv,
        cwd,
        env: scrubbed(vars),
    })
}

/// `vars` without what `sig`'s `main` set for WebKit (`JSC_*`, `WEBKIT_GST_DISABLE_GL_SINK`,
/// `WEBKIT_A11Y_BUS_ADDRESS`), `sig`'s own (`SIG_*`, `SIGHURT_*`) and what describes the
/// terminal `sig` itself may run in (tmux's, screen's, kitty's, WezTerm's, VTE's, the X window,
/// a stale size in `LINES` and `COLUMNS`, a `TERMCAP` override), with `TERM=xterm-256color` and
/// `COLORTERM=truecolor`, which is what the page's emulator is. Hygiene, not isolation: the
/// user's credentials, agent sockets and startup files stay theirs, as in any terminal.
pub fn scrubbed(vars: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<(OsString, OsString)> {
    const PREFIXES: &[&str] = &[
        "JSC_",
        "SIG_",
        "SIGHURT_",
        "TMUX",
        "KITTY_",
        "TERM_PROGRAM",
        "WEZTERM_",
    ];
    const NAMES: &[&str] = &[
        "STY",
        "VTE_VERSION",
        "WINDOWID",
        "TERM",
        "COLORTERM",
        "WEBKIT_GST_DISABLE_GL_SINK",
        "WEBKIT_A11Y_BUS_ADDRESS",
        "LINES",
        "COLUMNS",
        "TERMCAP",
    ];
    let mut env: Vec<_> = vars
        .into_iter()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            !NAMES.contains(&&*key) && !PREFIXES.iter().any(|prefix| key.starts_with(prefix))
        })
        .collect();
    env.push(("TERM".into(), "xterm-256color".into()));
    env.push(("COLORTERM".into(), "truecolor".into()));
    env
}

/// A program started on a new terminal: what the Wasm engine takes, once.
#[derive(Debug)]
pub struct Opened {
    /// The terminal's master side. Its only copy.
    pub master: OwnedFd,
    /// How the program ended, once it has been reaped: its exit code, or 128 plus the signal
    /// that ended it, as shells report it.
    pub exit: Arc<OnceLock<i32>>,
}

/// The programs running, by the request that started each, counted against [`MAX_SESSIONS`]
/// from before the spawn until they are reaped.
#[derive(Debug, Default)]
pub struct Ptys {
    live: Arc<AtomicUsize>,
    sessions: HashMap<RequestId, Session>,
}

impl Ptys {
    /// No programs.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts `program` on a new terminal of `cols` by `rows` cells for `request`. Fails, having
    /// started nothing and kept nothing open, when [`MAX_SESSIONS`] programs are live, `request`
    /// has one already, or the program cannot be started.
    pub fn open(
        &mut self,
        request: RequestId,
        program: PtyProgram,
        cols: u16,
        rows: u16,
        launch: Launch<'_>,
    ) -> Result<Opened, ServiceError> {
        let exec = exec(program, launch, std::env::vars_os()).map_err(ServiceError::Failed)?;
        self.open_exec(request, &exec, cols, rows)
    }

    fn open_exec(
        &mut self,
        request: RequestId,
        exec: &Exec,
        cols: u16,
        rows: u16,
    ) -> Result<Opened, ServiceError> {
        if self.sessions.contains_key(&request) {
            return Err(ServiceError::Failed("this terminal is open already".into()));
        }
        let (opened, session) = self.start(exec, cols, rows).map_err(ServiceError::Failed)?;
        self.sessions.insert(request, session);
        Ok(opened)
    }

    /// Ends the program `request` started, without waiting; whether there was one. A program
    /// that has exited already is only forgotten.
    pub fn cancel(&mut self, request: RequestId) -> bool {
        self.sessions.remove(&request).is_some()
    }

    /// Ends every program and waits until they are reaped, at most [`GRACE`] and a second: for
    /// quitting, before the engines shut down.
    pub fn shutdown(&mut self) {
        let exits: Vec<_> = self
            .sessions
            .drain()
            .map(|(_, session)| session.exit.clone())
            .collect();
        let deadline = Instant::now() + GRACE + REAP_WAIT;
        for exit in exits {
            exit.wait_until(deadline);
        }
    }

    /// Programs started and not yet reaped.
    #[cfg(test)]
    fn live(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }

    /// Opens a pty and starts `exec` on it, counted in [`Ptys::live`] until it is reaped.
    fn start(&self, exec: &Exec, cols: u16, rows: u16) -> Result<(Opened, Session), String> {
        if self
            .live
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                (live < MAX_SESSIONS).then_some(live + 1)
            })
            .is_err()
        {
            return Err(format!("{MAX_SESSIONS} terminal programs are running"));
        }
        let counted = Counted(self.live.clone());
        let failed = |err: io::Error| {
            let program = exec.argv[0].to_string_lossy();
            format!("could not start {program}: {err}")
        };
        let (woken, wake) =
            rustix::pipe::pipe_with(PipeFlags::CLOEXEC).map_err(|e| failed(e.into()))?;
        let (master, mut child) = spawn(exec, cols, rows).map_err(failed)?;
        // The child cannot be reaped before the watcher waits for it, so its pid is its own.
        let pidfd = match rustix::process::pidfd_open(Pid::from_child(&child), PidfdFlags::empty())
        {
            Ok(pidfd) => pidfd,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(failed(err.into()));
            }
        };
        // To kill the program if there is no thread to watch it.
        let spare = pidfd.try_clone();
        let exit = Arc::new(Exit::default());
        let shown = Arc::new(OnceLock::new());
        let (watched, told) = (exit.clone(), shown.clone());
        let watcher = std::thread::Builder::new()
            .name("sighurt-pty-watch".into())
            .spawn(move || {
                let status = watch(child, &pidfd, &woken);
                let _ = told.set(exit_code(status));
                watched.set(status);
                drop(counted);
            });
        if let Err(err) = watcher {
            // The closure owned the child and dropped it unreaped: kill it, though it stays a
            // zombie until sig exits.
            if let Ok(pidfd) = spare {
                let _ = rustix::process::pidfd_send_signal(&pidfd, Signal::KILL);
            }
            return Err(format!("could not watch the program: {err}"));
        }
        Ok((
            Opened {
                master,
                exit: shown,
            },
            Session { _wake: wake, exit },
        ))
    }
}

/// Gives back a program's place in [`Ptys::live`] when it has been reaped.
#[derive(Debug)]
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A program the watcher thread holds. Dropping it closes `_wake`, which tells the watcher to
/// end the program.
#[derive(Debug)]
struct Session {
    _wake: OwnedFd,
    exit: Arc<Exit>,
}

/// The watcher: waits for the program to exit or for the session to be dropped; on the latter,
/// SIGHUP and SIGCONT to its process group, SIGKILL after [`GRACE`]. Then reaps it.
fn watch(mut child: Child, pidfd: &OwnedFd, woken: &OwnedFd) -> ExitStatus {
    let group = Pid::from_child(&child);
    let mut kill_at: Option<Instant> = None;
    let mut killed = false;
    loop {
        let mut fds = vec![PollFd::new(pidfd, PollFlags::IN)];
        if kill_at.is_none() {
            fds.push(PollFd::new(woken, PollFlags::IN));
        }
        let timeout = kill_at
            .filter(|_| !killed)
            .map(|at| at.saturating_duration_since(Instant::now()))
            .and_then(|left| Timespec::try_from(left).ok());
        match rustix::event::poll(&mut fds, timeout.as_ref()) {
            Ok(_) | Err(Errno::INTR) => {}
            // Nothing to wait with: end it now.
            Err(_) => kill_at = Some(Instant::now()),
        }
        if fds[0]
            .revents()
            .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR)
        {
            break;
        }
        // The session was dropped (its end of the pipe closed).
        if fds.get(1).is_some_and(|fd| !fd.revents().is_empty()) {
            let _ = rustix::process::kill_process_group(group, Signal::HUP);
            let _ = rustix::process::kill_process_group(group, Signal::CONT);
            kill_at = Some(Instant::now() + GRACE);
        }
        if !killed && kill_at.is_some_and(|at| Instant::now() >= at) {
            let _ = rustix::process::kill_process_group(group, Signal::KILL);
            // In case it left its group.
            let _ = rustix::process::pidfd_send_signal(pidfd, Signal::KILL);
            killed = true;
        }
    }
    loop {
        match child.wait() {
            Ok(status) => return status,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            // Only ECHILD: someone else reaped it. Report it as killed.
            Err(_) => return ExitStatus::from_raw(libc::SIGKILL),
        }
    }
}

/// How a program ended, once it has: filled in by the watcher that reaped it.
#[derive(Debug, Default)]
struct Exit {
    status: Mutex<Option<ExitStatus>>,
    gone: Condvar,
}

impl Exit {
    /// The status, if the program has ended and been reaped.
    #[cfg(test)]
    fn status(&self) -> Option<ExitStatus> {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until the program has been reaped or `deadline` has passed; whether it was.
    fn wait_until(&self, deadline: Instant) -> bool {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        while status.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            status = self
                .gone
                .wait_timeout(status, left)
                .map_or_else(|poisoned| poisoned.into_inner().0, |(status, _)| status);
        }
        true
    }

    fn set(&self, status: ExitStatus) {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
        self.gone.notify_all();
    }
}

/// The number a page shows for an exit: the exit code, or 128 plus the signal that ended it.
fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1)
}

/// Opens the pty, sets its size and starts the program on it. The parent's copies of the slave
/// are closed before this returns, so the master reads end-of-file once the session has closed
/// its side.
fn spawn(exec: &Exec, cols: u16, rows: u16) -> io::Result<(OwnedFd, Child)> {
    let master =
        rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let name = rustix::pty::ptsname(&master, Vec::new())?;
    let slave = rustix::fs::open(
        name.as_c_str(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    rustix::termios::tcsetwinsize(
        &slave,
        Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;
    // A new pty starts with the kernel's default modes (cooked, echo, signals, Backspace as
    // DEL), whatever the terminal sig runs in is set to. Add UTF-8 line editing, so Backspace in
    // a cooked read removes a whole character.
    let mut termios = rustix::termios::tcgetattr(&slave)?;
    termios.input_modes |= InputModes::IUTF8;
    rustix::termios::tcsetattr(&slave, OptionalActions::Now, &termios)?;

    let mut command = Command::new(&exec.argv[0]);
    command
        .args(&exec.argv[1..])
        .env_clear()
        .envs(exec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&exec.cwd)
        // `try_clone` duplicates with close-on-exec; std puts them on 0 to 2 in the child.
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: the closure runs in the child between fork and exec, where only async-signal-safe
    // functions may be called. Everything it needs was prepared before the fork (std builds argv,
    // the environment and the directory as C strings first); it calls only setsid, ioctl,
    // tcsetpgrp, getpid, sigaction (through `signal`), sigprocmask and close_range (a raw
    // syscall), all async-signal-safe; it allocates nothing, logs nothing and takes no lock.
    // `io::Error::last_os_error` only reads errno.
    unsafe {
        command.pre_exec(|| {
            // A session of its own: no controlling terminal yet, and signals to its group
            // reach nothing of sig's.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // Std has put the slave on 0 to 2: make it the controlling terminal, with this
            // process's group in the foreground.
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1
                || libc::tcsetpgrp(0, libc::getpid()) == -1
            {
                return Err(io::Error::last_os_error());
            }
            // Ignored dispositions survive exec (Rust ignores SIGPIPE; std resets only that
            // one); handlers are reset by exec anyway. The errors are the signals that cannot
            // be changed (SIGKILL, SIGSTOP, those the C library keeps).
            for signal in 1..65 {
                libc::signal(signal, libc::SIG_DFL);
            }
            let mut empty = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut empty);
            libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
            // Seal every other descriptor: mark, don't close, so std's pipe that reports a
            // failed exec survives until the exec. A kernel that can't (before 5.11) fails the
            // launch.
            let sealed = libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            );
            if sealed == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    // `command` holds the slave descriptors; drop them now, not at the end of the scope.
    drop(command);
    Ok((master, child))
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsFd;

    use rustix::process::WaitOptions;

    use super::*;

    /// The tests count this process's descriptors and children: one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A private home, and the environment of a test child.
    fn home() -> (tempfile::TempDir, Vec<(OsString, OsString)>) {
        let home = tempfile::Builder::new()
            .prefix("sig-pty-home")
            .tempdir()
            .unwrap();
        let vars = vec![
            ("HOME".into(), home.path().into()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_default()),
            ("SHELL".into(), "/bin/sh".into()),
            ("TMUX".into(), "/tmp/tmux-1000/default,1,0".into()),
            ("TMUX_PANE".into(), "%1".into()),
            ("JSC_SIGNAL_FOR_GC".into(), "12".into()),
            ("SIGHURT_CEF_PREFIX".into(), "/x".into()),
            ("SIG_DIAG".into(), "1".into()),
            ("KITTY_WINDOW_ID".into(), "3".into()),
            ("TERM".into(), "tmux-256color".into()),
            ("TERM_PROGRAM".into(), "tmux".into()),
            ("STY".into(), "1.pts".into()),
            ("WINDOWID".into(), "7".into()),
            ("VTE_VERSION".into(), "7000".into()),
            ("WEZTERM_PANE".into(), "1".into()),
            ("LINES".into(), "24".into()),
            ("COLUMNS".into(), "80".into()),
            ("KEEP_ME".into(), "yes".into()),
        ];
        (home, vars)
    }

    /// `/bin/sh -c script` as the page's shell would run (`exec` splits a configured command at
    /// white space, so the script is put in afterwards).
    fn sh(script: &str, vars: &[(OsString, OsString)]) -> Exec {
        let no_config = Launch {
            command: None,
            config: None,
        };
        let mut exec = exec(PtyProgram::Shell, no_config, vars.iter().cloned()).unwrap();
        exec.argv = vec!["/bin/sh".into(), "-c".into(), script.into()];
        exec
    }

    fn start(ptys: &Ptys, exec: &Exec, cols: u16, rows: u16) -> (Opened, Session, Arc<Exit>) {
        let (opened, session) = ptys.start(exec, cols, rows).unwrap();
        let exit = session.exit.clone();
        (opened, session, exit)
    }

    /// How the program ended, within `limit`.
    fn exited(exit: &Exit, limit: Duration) -> ExitStatus {
        assert!(exit.wait_until(Instant::now() + limit), "not reaped");
        exit.status().unwrap()
    }

    /// Reads from the master until `want` appears, or end-of-file with `want` empty.
    fn read_until(master: &impl AsFd, want: &str, limit: Duration) -> String {
        let deadline = Instant::now() + limit;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let text = String::from_utf8_lossy(&out).into_owned();
            if !want.is_empty() && text.contains(want) {
                return text;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "waiting for {want:?}: {text}");
            let mut fds = [PollFd::new(master, PollFlags::IN)];
            match rustix::event::poll(&mut fds, Some(&Timespec::try_from(left).unwrap())) {
                Ok(0) | Err(Errno::INTR) => continue,
                Ok(_) => {}
                Err(err) => panic!("poll: {err}"),
            }
            match rustix::io::read(master, &mut buf) {
                Ok(0) | Err(Errno::IO) if want.is_empty() => return text,
                Ok(0) | Err(Errno::IO) => panic!("end of file waiting for {want:?}: {text}"),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(err) => panic!("read: {err}"),
            }
        }
    }

    /// Everything until end-of-file.
    fn read_all(master: &impl AsFd) -> String {
        read_until(master, "", Duration::from_secs(10))
    }

    /// The pid the script printed as `pid=<n>`.
    fn pid_in(text: &str) -> i32 {
        let at = text.find("pid=").expect("no pid") + 4;
        text[at..]
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Whether `pid` is neither alive nor a zombie of this process.
    fn gone(pid: i32) -> bool {
        let pid = Pid::from_raw(pid).unwrap();
        matches!(
            rustix::process::waitpid(Some(pid), WaitOptions::NOHANG),
            Err(Errno::CHILD)
        ) && !Path::new(&format!("/proc/{}", pid.as_raw_nonzero())).exists()
    }

    #[test]
    fn the_program_comes_from_a_closed_choice() {
        let config = Path::new("/home/u/.config/sighurt/config.toml");
        let vars = |extra: &[(&str, &str)]| -> Vec<(OsString, OsString)> {
            [("HOME", "/home/u"), ("SHELL", "/bin/zsh")]
                .iter()
                .chain(extra)
                .map(|(k, v)| (k.into(), v.into()))
                .collect()
        };
        let run = |program, command, extra: &[(&str, &str)]| {
            let launch = Launch {
                command,
                config: Some(config),
            };
            exec(program, launch, vars(extra)).unwrap()
        };
        let shell = run(PtyProgram::Shell, None, &[]);
        assert_eq!(shell.argv, ["/bin/zsh"]);
        assert_eq!(shell.cwd, Path::new("/home/u"));
        let configured = run(PtyProgram::Shell, Some(" fish  -l "), &[]);
        assert_eq!(configured.argv, ["fish", "-l"]);
        let blank = run(PtyProgram::Shell, Some("  "), &[]);
        assert_eq!(blank.argv, ["/bin/zsh"]);
        let none = Launch {
            command: None,
            config: None,
        };
        let bare = exec(PtyProgram::Shell, none, Vec::new()).unwrap();
        assert_eq!(
            (bare.argv, bare.cwd),
            (vec![OsString::from("/bin/sh")], "/".into())
        );

        // The editor is split into words, never given to a shell; the path is one argument of
        // its own, spaces and all.
        let spaced = Path::new("/home/u/my config/sighurt/config.toml");
        let edit = exec(
            PtyProgram::EditConfig,
            Launch {
                command: Some("fish"),
                config: Some(spaced),
            },
            vars(&[("EDITOR", "nano"), ("VISUAL", "nvim -u NONE")]),
        )
        .unwrap();
        assert_eq!(edit.argv, ["nvim", "-u", "NONE", spaced.to_str().unwrap()]);
        assert_eq!(edit.cwd, Path::new("/home/u/my config/sighurt"));
        let editor = run(PtyProgram::EditConfig, None, &[("EDITOR", "nano")]);
        assert_eq!(editor.argv, ["nano", config.to_str().unwrap()]);
        let vi = run(PtyProgram::EditConfig, None, &[("VISUAL", "")]);
        assert_eq!(vi.argv, ["vi", config.to_str().unwrap()]);
        assert!(exec(PtyProgram::EditConfig, none, vars(&[])).is_err());
    }

    #[test]
    fn the_environment_is_scrubbed() {
        let (_home, vars) = home();
        let env = scrubbed(vars);
        let names: Vec<_> = env.iter().map(|(k, _)| k.to_str().unwrap()).collect();
        assert_eq!(
            names,
            ["HOME", "PATH", "SHELL", "KEEP_ME", "TERM", "COLORTERM"],
            "{env:?}"
        );
        assert!(env.contains(&("TERM".into(), "xterm-256color".into())));
        assert!(env.contains(&("COLORTERM".into(), "truecolor".into())));
    }

    #[test]
    fn the_child_has_a_tty_its_fds_its_environment_and_default_signals() {
        let _serial = serial();
        let (home, vars) = home();
        let ptys = Ptys::default();
        let script = "ls /proc/self/fd | tr '\\n' ' '; echo; \
                      env | sort | tr '\\n' ' '; echo; \
                      tty; pwd; \
                      grep -E '^Sig(Ign|Blk)' /proc/self/status | tr '\\n' ' '; echo; \
                      [ -t 0 ] && [ -t 1 ] && [ -t 2 ] && echo all-ttys";
        // As sig ignores and blocks signals, so does this test while it starts the child: an
        // ignored disposition and the forking thread's mask would otherwise be inherited. A
        // stray descriptor without close-on-exec, too.
        let stray = std::fs::File::open("/dev/null").unwrap();
        rustix::io::fcntl_setfd(&stray, rustix::io::FdFlags::empty()).unwrap();
        // SAFETY: plain libc calls on this thread's mask and on a signal no test uses.
        let (ignored, mask) = unsafe {
            let ignored = libc::signal(libc::SIGUSR2, libc::SIG_IGN);
            let mut block = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut block);
            libc::sigaddset(&mut block, libc::SIGUSR1);
            let mut mask = std::mem::zeroed::<libc::sigset_t>();
            libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut mask);
            (ignored, mask)
        };
        let (opened, _session, exit) = start(&ptys, &sh(script, &vars), 80, 24);
        // SAFETY: restores what the block above changed.
        unsafe {
            libc::signal(libc::SIGUSR2, ignored);
            libc::pthread_sigmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
        }
        drop(stray);
        let out = read_all(&opened.master);
        let lines: Vec<_> = out.lines().map(str::trim).collect();
        // `ls` itself opens its directory: 0, 1, 2 and that one.
        let fds: Vec<_> = lines[0].split_whitespace().collect();
        assert_eq!(fds.len(), 4, "{out}");
        assert_eq!(&fds[..3], ["0", "1", "2"], "{out}");
        let env = lines[1];
        assert!(env.contains("TERM=xterm-256color"), "{env}");
        assert!(env.contains("COLORTERM=truecolor"), "{env}");
        assert!(env.contains("KEEP_ME=yes"), "{env}");
        for gone in [
            "TMUX",
            "JSC_",
            "SIGHURT_",
            "SIG_DIAG",
            "KITTY_",
            "TERM_PROGRAM",
            "STY=",
            "LINES=",
            "COLUMNS=",
        ] {
            assert!(!env.contains(gone), "{gone} in {env}");
        }
        assert!(lines[2].starts_with("/dev/pts/"), "{out}");
        assert_eq!(Path::new(lines[3]), home.path().canonicalize().unwrap());
        // What `grep`, the shell's child, inherited.
        assert!(lines[4].contains("SigIgn:\t0000000000000000"), "{out}");
        assert!(lines[4].contains("SigBlk:\t0000000000000000"), "{out}");
        assert_eq!(lines[5], "all-ttys", "{out}");
        assert_eq!(exit_code(exited(&exit, Duration::from_secs(5))), 0);
        assert_eq!(opened.exit.get(), Some(&0));
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn the_program_leads_its_own_session_in_the_foreground_of_its_tty() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (opened, _session, _exit) = start(
            &ptys,
            &sh(
                "echo pid=$$; cat /proc/self/stat; exec </dev/tty && echo ctty",
                &vars,
            ),
            80,
            24,
        );
        let out = read_all(&opened.master);
        let stat = out.lines().nth(1).unwrap();
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        // `cat` is the shell's child: its group and session are the shell's pid, and its group
        // is the terminal's foreground group (tpgid).
        let pid = pid_in(&out).to_string();
        assert_eq!(
            (fields[2], fields[3], fields[5]),
            (pid.as_str(), pid.as_str(), pid.as_str()),
            "{stat}"
        );
        assert_ne!(fields[4], "0", "a controlling tty: {stat}");
        assert!(out.contains("ctty"), "{out}");
    }

    #[test]
    fn the_size_and_a_resize_reach_the_program() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (opened, _session, exit) =
            start(&ptys, &sh("stty size; read x; stty size", &vars), 91, 37);
        read_until(&opened.master, "37 91", Duration::from_secs(10));
        rustix::termios::tcsetwinsize(
            &opened.master,
            Winsize {
                ws_row: 50,
                ws_col: 132,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        rustix::io::write(&opened.master, b"\n").unwrap();
        assert!(read_all(&opened.master).contains("50 132"));
        assert_eq!(exit_code(exited(&exit, Duration::from_secs(5))), 0);
    }

    #[test]
    fn the_exit_status_is_reported() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (opened, _session, exit) = start(&ptys, &sh("exit 7", &vars), 80, 24);
        let _ = read_all(&opened.master);
        assert_eq!(exit_code(exited(&exit, Duration::from_secs(5))), 7);
        assert_eq!(opened.exit.get(), Some(&7), "the page sees it");
        let (opened, _session, exit) = start(&ptys, &sh("kill -TERM $$", &vars), 80, 24);
        assert_eq!(exit_code(exited(&exit, Duration::from_secs(5))), 128 + 15);
        assert_eq!(opened.exit.get(), Some(&(128 + 15)));
    }

    #[test]
    fn cancelling_kills_a_program_that_ignores_sighup_after_the_grace() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let script = "trap '' HUP; echo pid=$$; exec sleep 1000";
        let (opened, session, exit) = start(&ptys, &sh(script, &vars), 80, 24);
        let pid = pid_in(&read_until(&opened.master, "pid=", Duration::from_secs(10)));
        // Not ended, though its terminal is still open.
        std::thread::sleep(Duration::from_millis(100));
        let started = Instant::now();
        drop(session);
        let status = exited(&exit, GRACE + Duration::from_secs(3));
        assert!(started.elapsed() >= GRACE - Duration::from_millis(100));
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(gone(pid));
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn a_program_that_obeys_sighup_ends_at_once() {
        let _serial = serial();
        let (_home, vars) = home();
        let mut ptys = Ptys::new();
        let request = RequestId::next();
        let opened = ptys
            .open_exec(request, &sh("echo pid=$$; exec sleep 1000", &vars), 80, 24)
            .unwrap();
        let pid = pid_in(&read_until(&opened.master, "pid=", Duration::from_secs(10)));
        let started = Instant::now();
        ptys.shutdown();
        assert!(started.elapsed() < GRACE, "{:?}", started.elapsed());
        assert_eq!(opened.exit.get(), Some(&(128 + libc::SIGHUP)));
        assert!(gone(pid));
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn cancelling_at_once_after_the_spawn_still_reaps() {
        let _serial = serial();
        let (_home, vars) = home();
        let mut ptys = Ptys::new();
        // The reply never reaches a page: the request is cancelled as soon as it is answered.
        for _ in 0..20 {
            let request = RequestId::next();
            let opened = ptys
                .open_exec(request, &sh("exec sleep 1000", &vars), 80, 24)
                .unwrap();
            assert!(ptys.cancel(request));
            drop(opened);
        }
        let deadline = Instant::now() + GRACE + Duration::from_secs(2);
        while ptys.live() > 0 {
            assert!(Instant::now() < deadline, "{} left", ptys.live());
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn shutdown_kills_what_ignores_sighup_within_the_grace() {
        let _serial = serial();
        let (_home, vars) = home();
        let mut ptys = Ptys::new();
        let mut opened = Vec::new();
        for _ in 0..3 {
            opened.push(
                ptys.open_exec(
                    RequestId::next(),
                    &sh(
                        "trap '' HUP TERM; echo pid=$$; while :; do sleep 1; done",
                        &vars,
                    ),
                    80,
                    24,
                )
                .unwrap(),
            );
        }
        for one in &opened {
            read_until(&one.master, "pid=", Duration::from_secs(10));
            assert_eq!(one.exit.get(), None);
        }
        let started = Instant::now();
        ptys.shutdown();
        assert!(started.elapsed() < GRACE + REAP_WAIT);
        for one in &opened {
            assert_eq!(one.exit.get(), Some(&(128 + libc::SIGKILL)));
        }
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn eight_programs_at_most_counted_until_reaped() {
        let _serial = serial();
        let (_home, vars) = home();
        let mut ptys = Ptys::new();
        let mut requests = Vec::new();
        let mut masters = Vec::new();
        for _ in 0..MAX_SESSIONS {
            let request = RequestId::next();
            masters.push(
                ptys.open_exec(request, &sh("exec sleep 1000", &vars), 80, 24)
                    .unwrap(),
            );
            requests.push(request);
        }
        assert_eq!(ptys.live(), MAX_SESSIONS);
        let refused = ptys.open_exec(RequestId::next(), &sh("exit 0", &vars), 80, 24);
        assert!(matches!(refused, Err(ServiceError::Failed(m)) if m.contains("8 terminal")));
        // A cancelled program holds its place until it is reaped.
        let first = requests.remove(0);
        assert!(ptys.cancel(first));
        let deadline = Instant::now() + GRACE + Duration::from_secs(2);
        while ptys.live() == MAX_SESSIONS {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ptys
            .open_exec(RequestId::next(), &sh("exec sleep 1000", &vars), 80, 24)
            .is_ok());
        assert!(
            ptys.open_exec(requests[0], &sh("exit 0", &vars), 80, 24)
                .is_err(),
            "one per request"
        );
        ptys.shutdown();
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn a_program_that_does_not_exist_starts_nothing_and_leaks_nothing() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let fds = || std::fs::read_dir("/proc/self/fd").unwrap().count();
        let before = fds();
        let mut exec = sh("", &vars);
        exec.argv = vec!["/nonexistent/program".into()];
        let err = ptys.start(&exec, 80, 24).unwrap_err();
        assert!(err.contains("/nonexistent/program"), "{err}");
        assert_eq!(fds(), before);
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn the_master_reads_end_of_file_once_the_session_is_gone() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (opened, _session, _exit) = start(&ptys, &sh("echo bye", &vars), 80, 24);
        // read_all panics without end-of-file: no copy of the slave stays open in sig.
        assert!(read_all(&opened.master).contains("bye"));
    }
}
