//! The programs of `sig://terminal`: opening a pseudo-terminal, starting the program on its
//! slave side, reaping it and ending it.
//!
//! The core has authorised a `PtyOpen` (the page's grant, `[pages] terminal`, a consumed
//! activation, the limits) before anything here runs. [`launch`] builds what runs from a closed
//! choice the guest made ([`Program`]): the guest never names a program, an argument, a
//! directory or a variable. [`Ptys::open`] opens the pty with its size already set, starts the
//! program as the leader of a new session with the slave as its controlling terminal, and hands
//! the master back; the runtime keeps no copy of it, so when `sig` ends, however it ends, the
//! kernel hangs the session up. A waiter thread reaps the child and records its exit
//! ([`Exit`]). Ending a [`Session`] (cancelling the request, closing the view, quitting) sends
//! SIGHUP to every process of the session, waits [`GRACE`] for them to go, and kills what is
//! left.
//!
//! The child inherits nothing it was not given: only descriptors 0 to 2 (the slave), default
//! signal dispositions and an empty signal mask, and `sig`'s environment without what `sig`
//! itself set and without the variables that would make the program think it runs in the
//! terminal `sig` was started from ([`scrubbed`]).
//!
//! Owns: the child processes and their sessions, until they are gone. Must not own: the master's
//! bytes (the Wasm engine's `pty` module carries them), the policy (the core's), or what the
//! page shows.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::OwnedFd;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags};
use rustix::process::{Pid, Signal};
use rustix::pty::OpenptFlags;
use rustix::termios::{InputModes, OptionalActions, Winsize};

/// Most programs with a live session in one process. The core holds pages to the same number.
pub const MAX_SESSIONS: usize = 8;
/// How long a session's processes have after SIGHUP before they are killed.
pub const GRACE: Duration = Duration::from_secs(2);
/// The editor when neither `VISUAL` nor `EDITOR` names one.
const EDITOR: &str = "vi";
/// The shell when neither the configuration nor `SHELL` names one.
const SHELL: &str = "/bin/sh";

/// What the page asked to run. The guest picks one of these; it names nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Program {
    /// The configured command, else the user's shell.
    Shell,
    /// The user's editor on the configuration file.
    EditConfig,
}

/// A program ready to start: its argument vector, working directory and whole environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    /// Never empty: [`launch`] makes sure of it.
    pub argv: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
}

/// What runs for `program`. `command` is `[pages.terminal] command`, split at white space with
/// no shell and no quoting; `config` is the configuration file `EditConfig` opens; `vars` is
/// `sig`'s environment. The working directory is `HOME` (`/` without one), and the directory of
/// the configuration file for `EditConfig`.
pub fn launch(
    program: Program,
    command: Option<&str>,
    config: &Path,
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Launch {
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
        Program::Shell => {
            let configured = command.map(|line| words(OsStr::new(line)));
            let argv = configured
                .filter(|argv| !argv.is_empty())
                .or_else(|| var("SHELL").map(|shell| vec![shell]))
                .unwrap_or_else(|| vec![SHELL.into()]);
            (argv, home)
        }
        Program::EditConfig => {
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
    Launch {
        argv,
        cwd,
        env: scrubbed(vars),
    }
}

/// `vars` without what `sig`'s `main` set for WebKit (`JSC_*`, `WEBKIT_GST_DISABLE_GL_SINK`,
/// `WEBKIT_A11Y_BUS_ADDRESS`), `sig`'s own (`SIG_*`, `SIGHURT_*`) and what describes the
/// terminal `sig` itself may run in (tmux's, screen's, kitty's, WezTerm's, VTE's, the X window),
/// with `TERM=xterm-256color` and `COLORTERM=truecolor`, which is what the page's emulator is.
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

/// How a session's program ended, once it has: filled in by the waiter thread that reaped it.
#[derive(Debug, Default)]
pub struct Exit {
    status: Mutex<Option<ExitStatus>>,
    gone: Condvar,
}

impl Exit {
    /// The status, if the program has ended and been reaped.
    pub fn status(&self) -> Option<ExitStatus> {
        *self.status.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits until the program has been reaped or `deadline` has passed; whether it was.
    pub fn wait_until(&self, deadline: Instant) -> bool {
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

/// The number a page shows for an exit: the exit code, or 128 plus the signal that ended it,
/// as shells report it.
pub fn exit_code(status: ExitStatus) -> i32 {
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1)
}

/// The sessions of one process: counts them against [`MAX_SESSIONS`].
#[derive(Debug, Default)]
pub struct Ptys {
    live: Arc<AtomicUsize>,
}

impl Ptys {
    /// Programs started and not yet reaped.
    pub fn live(&self) -> usize {
        self.live.load(Ordering::Acquire)
    }

    /// Opens a pty of `cols` × `rows` cells and starts `launch` on it. Returns the master and
    /// the session; `exited` runs on the waiter thread once the program has been reaped (to
    /// wake whoever reports it). Fails, having started nothing and kept nothing open, when
    /// [`MAX_SESSIONS`] programs are live or the program cannot be started.
    pub fn open(
        &self,
        launch: &Launch,
        cols: u16,
        rows: u16,
        exited: impl FnOnce(ExitStatus) + Send + 'static,
    ) -> Result<(OwnedFd, Session), String> {
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
        let (master, child) = spawn(launch, cols, rows).map_err(|err| {
            let program = launch.argv[0].to_string_lossy();
            format!("could not start {program}: {err}")
        })?;
        let pid = Pid::from_child(&child);
        let exit = Arc::new(Exit::default());
        let waiter = exit.clone();
        let started = std::thread::Builder::new()
            .name("sighurt-pty-wait".into())
            .spawn(move || reap(child, &waiter, counted, exited));
        if let Err(err) = started {
            // The child went with the closure: kill it so it does not run unwatched (it stays a
            // zombie until sig exits; there is no thread to reap it).
            let _ = rustix::process::kill_process_group(pid, Signal::KILL);
            return Err(format!("could not start the waiter thread: {err}"));
        }
        Ok((
            master,
            Session {
                pid,
                exit,
                ended: false,
            },
        ))
    }
}

/// Gives back a session's place in [`Ptys::live`] when dropped.
struct Counted(Arc<AtomicUsize>);

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

fn reap(mut child: Child, exit: &Exit, counted: Counted, exited: impl FnOnce(ExitStatus)) {
    let status = loop {
        match child.wait() {
            Ok(status) => break status,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            // Only ECHILD: someone else reaped it. Report it as killed.
            Err(_) => break ExitStatus::from_raw(libc::SIGKILL),
        }
    };
    exit.set(status);
    drop(counted);
    exited(status);
}

/// A started program. Dropping it ends the session as [`Session::cancel`] does.
#[derive(Debug)]
pub struct Session {
    /// The program's pid, which is also its session's and process group's id.
    pid: Pid,
    exit: Arc<Exit>,
    ended: bool,
}

impl Session {
    pub fn pid(&self) -> u32 {
        self.pid.as_raw_nonzero().get() as u32
    }

    /// How the program ended, shared with the waiter thread.
    pub fn exit(&self) -> &Arc<Exit> {
        &self.exit
    }

    /// Ends the session without waiting: SIGHUP to its processes now, SIGKILL to those left
    /// after [`GRACE`], on a thread of its own.
    pub fn cancel(mut self) {
        self.ended = true;
        hang_up(self.pid);
        let (pid, exit) = (self.pid, self.exit.clone());
        let ended = std::thread::Builder::new()
            .name("sighurt-pty-end".into())
            .spawn(move || finish(pid, &exit, Instant::now() + GRACE));
        if ended.is_err() {
            kill_session(self.pid, Signal::KILL);
        }
    }

    /// Ends every session in `sessions` and waits for them, at most [`GRACE`] and the time
    /// killing what is left takes: for quitting.
    pub fn end_all(sessions: impl IntoIterator<Item = Session>) {
        let mut sessions: Vec<_> = sessions.into_iter().collect();
        for session in &mut sessions {
            session.ended = true;
            hang_up(session.pid);
        }
        let deadline = Instant::now() + GRACE;
        for session in &sessions {
            finish(session.pid, &session.exit, deadline);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.ended {
            Session {
                pid: self.pid,
                exit: self.exit.clone(),
                ended: false,
            }
            .cancel();
            self.ended = true;
        }
    }
}

/// SIGHUP, then SIGCONT for what is stopped, to every process of `session`.
fn hang_up(session: Pid) {
    kill_session(session, Signal::HUP);
    kill_session(session, Signal::CONT);
}

/// Waits until `deadline` for the leader to be reaped and the rest of the session to go, then
/// kills what is left.
fn finish(session: Pid, exit: &Exit, deadline: Instant) {
    exit.wait_until(deadline);
    while !members(session).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    kill_session(session, Signal::KILL);
}

/// Sends `signal` to every process of `session`, and to its process group (which covers a
/// member `/proc` did not list yet).
///
/// Session and group ids cannot be reused while a process has them, so what this signals is in
/// the session the page started; once the session is empty, its number would have to be taken
/// by a new session leader between the scan and the signal for this to reach anything else.
fn kill_session(session: Pid, signal: Signal) {
    let _ = rustix::process::kill_process_group(session, signal);
    for pid in members(session) {
        let _ = rustix::process::kill_process(pid, signal);
    }
}

/// The live processes (not zombies) whose session is `session`, from `/proc`.
fn members(session: Pid) -> Vec<Pid> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for entry in dir.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i32>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // "pid (comm) state ppid pgrp session …"; comm may hold anything, so start after the
        // last parenthesis.
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        let fields: Vec<_> = rest.split_whitespace().collect();
        let (Some(state), Some(sid)) = (fields.first(), fields.get(3)) else {
            continue;
        };
        if *state != "Z"
            && sid.parse::<i32>().ok() == Some(session.as_raw_nonzero().get())
            && seen.insert(pid)
        {
            found.extend(Pid::from_raw(pid));
        }
    }
    found
}

/// Opens the pty, sets its size and starts the program on it. The parent's copies of the slave
/// are closed before this returns, so the master reads end-of-file once the session has closed
/// its side.
fn spawn(launch: &Launch, cols: u16, rows: u16) -> io::Result<(OwnedFd, Child)> {
    let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    rustix::io::fcntl_setfd(&master, rustix::io::FdFlags::CLOEXEC)?;
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
    // Line editing that knows UTF-8: Backspace in a cooked read removes a whole character.
    let mut termios = rustix::termios::tcgetattr(&slave)?;
    termios.input_modes |= InputModes::IUTF8;
    rustix::termios::tcsetattr(&slave, OptionalActions::Now, &termios)?;

    let mut command = Command::new(&launch.argv[0]);
    command
        .args(&launch.argv[1..])
        .env_clear()
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&launch.cwd)
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // SAFETY: the closure runs in the child between fork and exec, where only async-signal-safe
    // functions may be called. It calls only setsid, ioctl, sigaction (through `signal`),
    // sigprocmask, close_range (a raw syscall) and fcntl, all async-signal-safe, allocates
    // nothing and takes no lock. `io::Error::last_os_error` only reads errno.
    unsafe {
        command.pre_exec(|| {
            // A session of its own, so the program has no controlling terminal yet and
            // signals to its group reach nothing of sig's.
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            // Std has put the slave on 0 to 2: make it the controlling terminal.
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            // Dispositions sig ignores (and std resets only SIGPIPE) would be inherited
            // across exec; handlers are reset by exec anyway. Errors are the signals that
            // cannot be changed (SIGKILL, SIGSTOP, those the C library keeps).
            for signal in 1..65 {
                libc::signal(signal, libc::SIG_DFL);
            }
            let mut empty = std::mem::zeroed::<libc::sigset_t>();
            libc::sigemptyset(&mut empty);
            libc::sigprocmask(libc::SIG_SETMASK, &empty, std::ptr::null_mut());
            // Mark, don't close, every other descriptor: std's pipe that reports a failed
            // exec must survive until the exec.
            let marked = libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            );
            if marked == -1 {
                // A kernel before 5.11: mark them one by one, up to the descriptor limit.
                let mut limit = std::mem::zeroed::<libc::rlimit>();
                let max = if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
                    limit.rlim_cur.min(1 << 20) as i32
                } else {
                    1024
                };
                for fd in 3..max {
                    libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                }
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
    use std::sync::mpsc;

    use rustix::io::Errno;
    use rustix::process::WaitOptions;

    use super::*;

    /// The tests count this process's descriptors and processes: one at a time.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A private home with a marker in its name, and the environment of a test child.
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
            ("KEEP_ME".into(), "yes".into()),
        ];
        (home, vars)
    }

    /// `/bin/sh -c script` as the page's shell would run (`launch` splits a configured
    /// command at white space, so the script is put in afterwards).
    fn sh(script: &str, vars: &[(OsString, OsString)]) -> Launch {
        let mut launch = launch(
            Program::Shell,
            None,
            Path::new("/nonexistent/config.toml"),
            vars.iter().cloned(),
        );
        launch.argv = vec!["/bin/sh".into(), "-c".into(), script.into()];
        launch
    }

    /// Everything the master gives until end-of-file, or `limit`.
    fn read_all(master: &impl AsFd, limit: Duration) -> String {
        let deadline = Instant::now() + limit;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(
                !left.is_zero(),
                "no end-of-file: {}",
                String::from_utf8_lossy(&out)
            );
            let mut fds = [rustix::event::PollFd::new(
                master,
                rustix::event::PollFlags::IN,
            )];
            let timeout = rustix::event::Timespec::try_from(left).unwrap();
            match rustix::event::poll(&mut fds, Some(&timeout)) {
                Ok(0) | Err(Errno::INTR) => continue,
                Ok(_) => {}
                Err(err) => panic!("poll: {err}"),
            }
            match rustix::io::read(master, &mut buf) {
                Ok(0) | Err(Errno::IO) => return String::from_utf8_lossy(&out).into_owned(),
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(Errno::INTR | Errno::AGAIN) => {}
                Err(err) => panic!("read: {err}"),
            }
        }
    }

    fn reaped(pid: u32) -> bool {
        let pid = Pid::from_raw(pid as i32).unwrap();
        matches!(
            rustix::process::waitpid(Some(pid), WaitOptions::NOHANG),
            Err(Errno::CHILD)
        ) && !Path::new(&format!("/proc/{}", pid.as_raw_nonzero())).exists()
    }

    fn open(
        ptys: &Ptys,
        launch: &Launch,
        cols: u16,
        rows: u16,
    ) -> (OwnedFd, Session, mpsc::Receiver<ExitStatus>) {
        let (tx, rx) = mpsc::channel();
        let (master, session) = ptys
            .open(launch, cols, rows, move |status| {
                let _ = tx.send(status);
            })
            .unwrap();
        (master, session, rx)
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
        let shell = launch(Program::Shell, None, config, vars(&[]));
        assert_eq!(shell.argv, ["/bin/zsh"]);
        assert_eq!(shell.cwd, Path::new("/home/u"));
        let configured = launch(Program::Shell, Some(" fish  -l "), config, vars(&[]));
        assert_eq!(configured.argv, ["fish", "-l"]);
        let blank = launch(Program::Shell, Some("  "), config, vars(&[]));
        assert_eq!(blank.argv, ["/bin/zsh"]);
        let bare = launch(Program::Shell, None, config, Vec::new());
        assert_eq!(
            (bare.argv, bare.cwd),
            (vec![OsString::from("/bin/sh")], "/".into())
        );

        let edit = launch(
            Program::EditConfig,
            Some("fish"),
            config,
            vars(&[("EDITOR", "nano"), ("VISUAL", "nvim -u NONE")]),
        );
        assert_eq!(edit.argv, ["nvim", "-u", "NONE", config.to_str().unwrap()]);
        assert_eq!(edit.cwd, Path::new("/home/u/.config/sighurt"));
        let editor = launch(
            Program::EditConfig,
            None,
            config,
            vars(&[("EDITOR", "nano")]),
        );
        assert_eq!(editor.argv, ["nano", config.to_str().unwrap()]);
        let vi = launch(Program::EditConfig, None, config, vars(&[("VISUAL", "")]));
        assert_eq!(vi.argv, ["vi", config.to_str().unwrap()]);
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
        // The test process holds descriptors of its own (the harness's, the home's): the child
        // must have none of them.
        let script = "ls /proc/self/fd | tr '\\n' ' '; echo; \
                      env | sort | tr '\\n' ' '; echo; \
                      tty; pwd; \
                      grep -E '^Sig(Ign|Blk)' /proc/self/status | tr '\\n' ' '; echo; \
                      [ -t 0 ] && [ -t 1 ] && [ -t 2 ] && echo all-ttys";
        // As sig ignores and blocks signals, so does this test while it starts the child: an
        // ignored disposition and the forking thread's mask would otherwise be inherited. A
        // stray descriptor without CLOEXEC, too.
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
        let (master, session, rx) = open(&ptys, &sh(script, &vars), 80, 24);
        // SAFETY: restores what the block above changed.
        unsafe {
            libc::signal(libc::SIGUSR2, ignored);
            libc::pthread_sigmask(libc::SIG_SETMASK, &mask, std::ptr::null_mut());
        }
        drop(stray);
        let out = read_all(&master, Duration::from_secs(10));
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
        ] {
            assert!(!env.contains(gone), "{gone} in {env}");
        }
        assert!(lines[2].starts_with("/dev/pts/"), "{out}");
        assert_eq!(Path::new(lines[3]), home.path().canonicalize().unwrap());
        // sh may ignore signals for itself; what it inherited shows in its child (grep).
        assert!(lines[4].contains("SigIgn:\t0000000000000000"), "{out}");
        assert!(lines[4].contains("SigBlk:\t0000000000000000"), "{out}");
        assert_eq!(lines[5], "all-ttys", "{out}");
        let status = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(exit_code(status), 0);
        assert!(reaped(session.pid()));
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn the_program_leads_its_own_session_with_a_controlling_tty() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (master, session, _rx) = open(
            &ptys,
            &sh("cat /proc/self/stat; exec </dev/tty && echo ctty", &vars),
            80,
            24,
        );
        let out = read_all(&master, Duration::from_secs(10));
        let stat = out.lines().next().unwrap();
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .unwrap()
            .1
            .split_whitespace()
            .collect();
        // `cat` is the shell's child: its group and session are the shell's pid.
        let pid = session.pid().to_string();
        assert_eq!(
            (fields[2], fields[3]),
            (pid.as_str(), pid.as_str()),
            "{stat}"
        );
        // A controlling terminal: the tty_nr field is set, and /dev/tty opens.
        assert_ne!(fields[4], "0", "{stat}");
        assert!(out.contains("ctty"), "{out}");
    }

    #[test]
    fn the_size_and_a_resize_reach_the_program() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        // The program waits for a line, then reports the size again.
        let (master, _session, rx) =
            open(&ptys, &sh("stty size; read x; stty size", &vars), 91, 37);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while !String::from_utf8_lossy(&seen).contains("37 91") {
            assert!(
                Instant::now() < deadline,
                "{}",
                String::from_utf8_lossy(&seen)
            );
            if let Ok(n) = rustix::io::read(&master, &mut buf) {
                seen.extend_from_slice(&buf[..n]);
            }
        }
        rustix::termios::tcsetwinsize(
            &master,
            Winsize {
                ws_row: 50,
                ws_col: 132,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        rustix::io::write(&master, b"\n").unwrap();
        let out = read_all(&master, Duration::from_secs(10));
        assert!(out.contains("50 132"), "{out}");
        assert_eq!(
            exit_code(rx.recv_timeout(Duration::from_secs(5)).unwrap()),
            0
        );
    }

    #[test]
    fn the_exit_status_is_reported() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (master, session, rx) = open(&ptys, &sh("exit 7", &vars), 80, 24);
        let _ = read_all(&master, Duration::from_secs(10));
        assert_eq!(
            exit_code(rx.recv_timeout(Duration::from_secs(5)).unwrap()),
            7
        );
        assert_eq!(session.exit().status().map(exit_code), Some(7));
        let (_master, session, rx) = open(&ptys, &sh("kill -TERM $$", &vars), 80, 24);
        assert_eq!(
            exit_code(rx.recv_timeout(Duration::from_secs(5)).unwrap()),
            128 + 15
        );
        assert!(reaped(session.pid()));
    }

    #[test]
    fn cancel_ends_a_program_that_ignores_sighup() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        // The shell ignores HUP and has a background child that ignores it too.
        let script = "trap '' HUP; (trap '' HUP; exec sleep 1000) & echo ready; wait";
        let (master, session, rx) = open(&ptys, &sh(script, &vars), 80, 24);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = Vec::new();
        let mut buf = [0u8; 256];
        while !String::from_utf8_lossy(&seen).contains("ready") {
            assert!(Instant::now() < deadline);
            if let Ok(n) = rustix::io::read(&master, &mut buf) {
                seen.extend_from_slice(&buf[..n]);
            }
        }
        let pid = Pid::from_raw(session.pid() as i32).unwrap();
        assert_eq!(members(pid).len(), 2, "the shell and sleep");
        let started = Instant::now();
        session.cancel();
        let status = rx.recv_timeout(GRACE + Duration::from_secs(3)).unwrap();
        assert!(started.elapsed() >= GRACE - Duration::from_millis(100));
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        let deadline = Instant::now() + Duration::from_secs(2);
        while !members(pid).is_empty() {
            assert!(Instant::now() < deadline, "sleep survived");
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(master);
        assert!(reaped(pid.as_raw_nonzero().get() as u32));
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn a_program_that_obeys_sighup_ends_at_once() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (_master, session, rx) = open(&ptys, &sh("echo up; exec sleep 1000", &vars), 80, 24);
        std::thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        let pid = session.pid();
        Session::end_all([session]);
        assert!(started.elapsed() < GRACE, "{:?}", started.elapsed());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap().signal(),
            Some(libc::SIGHUP)
        );
        assert!(reaped(pid));
    }

    #[test]
    fn dropping_a_session_ends_it() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (_master, session, rx) = open(&ptys, &sh("exec sleep 1000", &vars), 80, 24);
        let pid = session.pid();
        drop(session);
        assert!(rx.recv_timeout(GRACE + Duration::from_secs(2)).is_ok());
        assert!(reaped(pid));
    }

    #[test]
    fn eight_sessions_at_most() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let mut open_ones = Vec::new();
        for _ in 0..MAX_SESSIONS {
            open_ones.push(open(&ptys, &sh("exec sleep 1000", &vars), 80, 24));
        }
        assert_eq!(ptys.live(), MAX_SESSIONS);
        let refused = ptys.open(&sh("exit 0", &vars), 80, 24, |_| {});
        assert!(refused.unwrap_err().contains("8 terminal programs"));
        let (master, session, rx) = open_ones.pop().unwrap();
        session.cancel();
        rx.recv_timeout(GRACE + Duration::from_secs(2)).unwrap();
        drop(master);
        assert_eq!(ptys.live(), MAX_SESSIONS - 1);
        open_ones.push(open(&ptys, &sh("exec sleep 1000", &vars), 80, 24));
        Session::end_all(open_ones.drain(..).map(|(_, session, _)| session));
        let deadline = Instant::now() + Duration::from_secs(2);
        while ptys.live() > 0 {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_program_that_does_not_exist_starts_nothing_and_leaks_nothing() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let fds = || std::fs::read_dir("/proc/self/fd").unwrap().count();
        let before = fds();
        let mut launch = sh("", &vars);
        launch.argv = vec!["/nonexistent/program".into()];
        let err = ptys.open(&launch, 80, 24, |_| {}).unwrap_err();
        assert!(err.contains("/nonexistent/program"), "{err}");
        assert_eq!(fds(), before);
        assert_eq!(ptys.live(), 0);
    }

    #[test]
    fn the_master_reads_end_of_file_once_the_session_is_gone() {
        let _serial = serial();
        let (_home, vars) = home();
        let ptys = Ptys::default();
        let (master, _session, _rx) = open(&ptys, &sh("echo bye", &vars), 80, 24);
        // read_all panics without end-of-file: no copy of the slave stays open in sig.
        assert!(read_all(&master, Duration::from_secs(10)).contains("bye"));
    }
}
