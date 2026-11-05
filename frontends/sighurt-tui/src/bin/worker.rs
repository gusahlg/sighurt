//! Child process the pty tests drive. Not a user-facing tool.
//!
//! Each worker enters raw mode on stdout and exits by the path that worker
//! names: a normal restore, a panic, suspend/resume, hangup, or
//! [`sighurt_tui::term::restore_before_forced_exit`] before `_exit`.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::thread;
use std::time::Duration;

use sighurt_tui::term::{restore_before_forced_exit, Terminal};

fn main() {
    if let Err(err) = dispatch() {
        eprintln!("sighurt-tui-worker: {err}");
        std::process::exit(1);
    }
}

fn dispatch() -> io::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--worker") => match args.next().as_deref() {
            Some("enter-exit") => worker_enter_exit(),
            Some("panic") => worker_panic(),
            Some("suspend") => worker_suspend(),
            Some("hangup") => worker_hangup(),
            Some("force-exit") => worker_force_exit(),
            Some(other) => Err(io::Error::other(format!("unknown worker {other}"))),
            None => Err(io::Error::other("missing worker name")),
        },
        Some(other) => Err(io::Error::other(format!("unknown argument {other}"))),
        None => Err(io::Error::other("missing --worker")),
    }
}

fn stdout_fd() -> io::Result<OwnedFd> {
    std::io::stdout().as_fd().try_clone_to_owned()
}

fn worker_enter_exit() -> io::Result<()> {
    let mut terminal = Terminal::enter(stdout_fd()?)?;
    terminal.write_frame(b"READY")?;
    terminal.restore()?;
    terminal.write_frame(b"DONE")?;
    Ok(())
}

fn worker_panic() -> io::Result<()> {
    let mut terminal = Terminal::enter(stdout_fd()?)?;
    terminal.write_frame(b"READY")?;
    let _keep = terminal;
    panic!("sighurt-tui worker panic");
}

fn worker_suspend() -> io::Result<()> {
    let mut terminal = Terminal::enter(stdout_fd()?)?;
    terminal.install_handlers()?;
    terminal.write_frame(b"READY")?;
    loop {
        if terminal.take_resumed() {
            terminal.write_frame(b"CONT")?;
            terminal.restore()?;
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn worker_hangup() -> io::Result<()> {
    let mut terminal = Terminal::enter(stdout_fd()?)?;
    terminal.install_handlers()?;
    terminal.write_frame(b"READY")?;
    // As the owner loop does: the flag, then a normal restore and exit.
    while !terminal.take_hangup() {
        thread::sleep(Duration::from_millis(20));
    }
    terminal.write_frame(b"HUP")?;
    terminal.restore()?;
    Ok(())
}

fn worker_force_exit() -> io::Result<()> {
    let mut terminal = Terminal::enter(stdout_fd()?)?;
    terminal.write_frame(b"READY")?;
    // Skip Drop: this is the path before a forced `_exit`.
    restore_before_forced_exit();
    let _terminal = terminal;
    unsafe { libc::_exit(0) }
}
