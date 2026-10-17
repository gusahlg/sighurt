//! The terminal session: raw mode, probe, frames, and restoration.
//!
//! One session per process. The lock is only taken on the owner thread.
//! Signal handlers and the panic hook load an atomic pointer and do not take
//! that lock, allocate, or call the previous owner. `restore` is safe to call
//! twice. Drop calls it too.
//!
//! Installing handlers leaks the state box. A handler that already loaded the
//! pointer can still use it after the session is dropped; a handler that loads
//! afterwards sees a null pointer and does nothing. Tests that install handlers
//! run in a child process.
//!
//! The panic hook restores only when the panicking thread is the one that
//! called [`Session::enter`]. A helper thread's panic must not put the
//! terminal back into cooked, blocking mode under the owner. `set_hook` is
//! not called while unwinding: Rust rejects that and aborts the process.

use std::io::{self, ErrorKind};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::pipe::PipeFlags;
use rustix::pty::OpenptFlags;
use rustix::termios::{tcgetattr, tcsetattr, OptionalActions, SpecialCodeIndex, Termios, Winsize};

use super::modes::{ENTER, LEAVE, QUERIES};
use super::output::Output;
use super::parser::{Input, Parser};
use super::probe::{self, Caps};

const DEFAULT_CAP: usize = 256 * 1024;

static LOCK: Mutex<()> = Mutex::new(());
static STATE: AtomicPtr<Shared> = AtomicPtr::new(std::ptr::null_mut());
static HOOK_ON: AtomicBool = AtomicBool::new(false);
/// Thread that called [`Session::enter`]. `0` when no session is active.
static OWNER: AtomicI32 = AtomicI32::new(0);
/// Write end of the `SIGWINCH` pipe. `-1` when no handler should write.
static WINCH_WRITE: AtomicI32 = AtomicI32::new(-1);
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;
static PREV_HOOK: AtomicPtr<PanicHook> = AtomicPtr::new(std::ptr::null_mut());

struct Shared {
    fd: RawFd,
    original: Termios,
    raw: Termios,
    original_flags: OFlags,
    raw_flags: OFlags,
    active: AtomicBool,
    suspended: AtomicBool,
    resumed: AtomicBool,
    /// Set before `sigaction`, so drop leaks the box rather than freeing it
    /// under a live handler.
    handlers: AtomicBool,
}

/// Raw mode on one terminal, and the bytes waiting to go out.
pub struct Session {
    fd: OwnedFd,
    state: *mut Shared,
    output: Output,
    parser: Parser,
    /// Read end of the self-pipe the `SIGWINCH` handler writes to.
    winch_read: OwnedFd,
    winch_write: OwnedFd,
    /// Held for the session lifetime so a second session waits instead of
    /// overlapping this one. Drop releases it.
    #[allow(dead_code)]
    guard: Option<MutexGuard<'static, ()>>,
}

impl Session {
    /// Put `fd` into raw mode, non-blocking, and send [`ENTER`].
    pub fn enter(fd: OwnedFd) -> io::Result<Self> {
        Self::enter_with(fd, DEFAULT_CAP)
    }

    /// [`Self::enter`] with a pending-byte cap of `cap`.
    pub fn enter_with(fd: OwnedFd, cap: usize) -> io::Result<Self> {
        let guard = match LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !STATE.load(Ordering::Acquire).is_null() {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "a terminal session is already active",
            ));
        }
        let (winch_read, winch_write) =
            rustix::pipe::pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
        let original_flags = rustix::fs::fcntl_getfl(&fd)?;
        let raw_flags = original_flags | OFlags::NONBLOCK;
        rustix::fs::fcntl_setfl(&fd, raw_flags)?;
        let original = tcgetattr(&fd)?;
        let mut raw = original.clone();
        raw.make_raw();
        if let Err(err) = tcsetattr(&fd, OptionalActions::Now, &raw) {
            let _ = rustix::fs::fcntl_setfl(&fd, original_flags);
            return Err(io::Error::from(err));
        }
        let shared = Box::new(Shared {
            fd: fd.as_raw_fd(),
            original,
            raw,
            original_flags,
            raw_flags,
            active: AtomicBool::new(true),
            suspended: AtomicBool::new(false),
            resumed: AtomicBool::new(false),
            handlers: AtomicBool::new(false),
        });
        let state = Box::into_raw(shared);
        STATE.store(state, Ordering::Release);
        let mut session = Session {
            fd,
            state,
            output: Output::new(cap),
            parser: Parser::new(),
            winch_read,
            winch_write,
            guard: Some(guard),
        };
        session.write_frame(ENTER)?;
        OWNER.store(current_tid(), Ordering::Release);
        install_panic_hook();
        Ok(session)
    }

    fn shared(&self) -> &Shared {
        unsafe { &*self.state }
    }

    /// Restore the termios and file flags captured at [`Self::enter`], and
    /// send [`LEAVE`]. A second call is a success and writes nothing.
    pub fn restore(&mut self) -> io::Result<()> {
        self.output.clear();
        if restore_signal_safe(self.shared()) {
            Ok(())
        } else {
            Err(io::Error::other(
                "tcsetattr failed while restoring the terminal",
            ))
        }
    }

    /// `SIGTSTP` restores and stops; `SIGCONT` enters again; `SIGHUP` restores
    /// and re-raises. `SIGWINCH` writes one byte to [`Self::winch_fd`]. The
    /// state box is leaked.
    pub fn install_handlers(&mut self) -> io::Result<()> {
        self.shared().handlers.store(true, Ordering::Release);
        install_signal(libc::SIGTSTP, handle_tstp)?;
        install_signal(libc::SIGCONT, handle_cont)?;
        install_signal(libc::SIGHUP, handle_hup)?;
        install_signal(libc::SIGWINCH, handle_winch)?;
        WINCH_WRITE.store(self.winch_write.as_raw_fd(), Ordering::Release);
        Ok(())
    }

    /// The read end of the `SIGWINCH` pipe. Readable after [`Self::install_handlers`].
    pub fn winch_fd(&self) -> BorrowedFd<'_> {
        self.winch_read.as_fd()
    }

    /// Drop bytes the `SIGWINCH` handler wrote. The size check is separate.
    pub fn drain_winch(&self) {
        let mut buf = [0u8; 64];
        loop {
            match rustix::io::read(&self.winch_read, &mut buf) {
                Ok(0) => break,
                Ok(_) => continue,
                Err(Errno::INTR) => continue,
                Err(_) => break,
            }
        }
    }

    /// Whether `SIGCONT` has re-entered since the last call.
    pub fn take_resumed(&self) -> bool {
        self.shared().resumed.swap(false, Ordering::AcqRel)
    }

    /// The terminal file descriptor. Register it for readability.
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Duplicate of the terminal fd. The duplicate shares non-blocking mode.
    pub fn try_clone_fd(&self) -> io::Result<OwnedFd> {
        self.fd.try_clone()
    }

    /// The terminal's window size.
    pub fn winsize(&self) -> io::Result<Winsize> {
        Ok(rustix::termios::tcgetwinsize(&self.fd)?)
    }

    /// Bytes still waiting in the output cap.
    pub fn pending_len(&self) -> usize {
        self.output.pending_len()
    }

    /// Frames dropped because the reader was not taking them.
    pub fn dropped_frames(&self) -> u64 {
        self.output.drops()
    }

    /// Queue `frame`. Does not block when the terminal stops reading.
    pub fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.output.write_frame(&self.fd, frame)
    }

    /// Send the startup queries and collect replies until DA1 or `deadline`.
    ///
    /// A read happens only after `poll` says the fd is readable, and only for
    /// the time still inside `deadline`. `O_NONBLOCK` is put back first: a
    /// panic used to clear it and turn the follow-up `read` into a blocking
    /// canonical read. A terminal that never answers cannot stall this past
    /// the deadline.
    pub fn probe(&mut self, deadline: Duration) -> io::Result<Caps> {
        self.write_frame(QUERIES)?;
        let start = std::time::Instant::now();
        let mut caps = Caps::default();
        let mut saw_da1 = false;
        loop {
            let remaining = deadline.saturating_sub(start.elapsed());
            let wait = if saw_da1 { Duration::ZERO } else { remaining };
            if !self.prepare_read()? {
                break;
            }
            if !wait_readable(&self.fd, wait)? {
                break;
            }
            if !self.prepare_read()? {
                break;
            }
            let bytes = read_some(&self.fd)?;
            if bytes.is_empty() {
                if saw_da1 || start.elapsed() >= deadline {
                    break;
                }
                // `poll` woke with nothing to take. Wait again; do not spin.
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            for input in self.parser.feed(&bytes) {
                if probe::absorb(&mut caps, &input) {
                    saw_da1 = true;
                }
            }
        }
        Ok(caps)
    }

    /// Read whatever is already buffered and parse it with the probe's parser.
    pub fn read_input(&mut self) -> io::Result<Vec<Input>> {
        let mut inputs = Vec::new();
        loop {
            if !self.prepare_read()? {
                break;
            }
            let bytes = read_some(&self.fd)?;
            if bytes.is_empty() {
                break;
            }
            inputs.extend(self.parser.feed(&bytes));
        }
        Ok(inputs)
    }

    /// Time until a partial escape sequence should be expired.
    pub fn poll_timeout(&self) -> Option<Duration> {
        self.parser.poll_timeout()
    }

    /// Expire only if the timer has elapsed.
    pub fn expire_if_due(&mut self) -> Vec<Input> {
        self.parser.expire_if_due()
    }

    /// `false` once [`Self::restore`] has run. While active, force
    /// `O_NONBLOCK` so a cleared flag cannot turn the next read into a wait.
    fn prepare_read(&self) -> io::Result<bool> {
        if !self.shared().active.load(Ordering::Acquire) {
            return Ok(false);
        }
        let flags = rustix::fs::fcntl_getfl(&self.fd)?;
        if !flags.contains(OFlags::NONBLOCK) {
            rustix::fs::fcntl_setfl(&self.fd, flags | OFlags::NONBLOCK)?;
        }
        Ok(true)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let write = self.winch_write.as_raw_fd();
        if WINCH_WRITE.load(Ordering::Acquire) == write {
            WINCH_WRITE.store(-1, Ordering::Release);
        }
        let state = self.shared();
        let _ = restore_signal_safe(state);
        if OWNER.load(Ordering::Acquire) == current_tid() {
            OWNER.store(0, Ordering::Release);
        }
        // `set_hook` panics if called while this thread is unwinding, and
        // that panic aborts the process. Leave the hook installed; it
        // no-ops once `STATE` is cleared.
        if !std::thread::panicking() {
            uninstall_panic_hook();
        }
        let handlers = state.handlers.load(Ordering::Acquire);
        STATE.store(std::ptr::null_mut(), Ordering::Release);
        if !handlers {
            unsafe { drop(Box::from_raw(self.state)) };
        }
    }
}

/// Same restoration as [`Session::restore`], for the moment before `_exit`.
///
/// Does not allocate, lock, or run destructors: the body is `write`,
/// `tcsetattr` and `fcntl` on the fd captured at enter. Idempotent, including
/// when no session is active. A second call writes nothing.
pub fn restore_before_forced_exit() {
    if let Some(state) = load_state() {
        let _ = restore_signal_safe(state);
    }
}

/// Open a pseudoterminal. The master is first, the slave second.
pub fn open_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    let flags = OpenptFlags::RDWR | OpenptFlags::CLOEXEC | OpenptFlags::NOCTTY;
    let master = rustix::pty::openpt(flags)?;
    rustix::pty::grantpt(&master)?;
    rustix::pty::unlockpt(&master)?;
    let slave = rustix::pty::ioctl_tiocgptpeer(&master, flags)?;
    Ok((master, slave))
}

/// Termios fields the restoration tests compare. `Termios` is not `PartialEq`.
pub fn termios_matches(left: &Termios, right: &Termios) -> bool {
    if left.input_modes != right.input_modes
        || left.output_modes != right.output_modes
        || left.control_modes != right.control_modes
        || left.local_modes != right.local_modes
        || left.line_discipline != right.line_discipline
        || left.input_speed() != right.input_speed()
        || left.output_speed() != right.output_speed()
    {
        return false;
    }
    const CODES: &[SpecialCodeIndex] = &[
        SpecialCodeIndex::VINTR,
        SpecialCodeIndex::VQUIT,
        SpecialCodeIndex::VERASE,
        SpecialCodeIndex::VKILL,
        SpecialCodeIndex::VEOF,
        SpecialCodeIndex::VTIME,
        SpecialCodeIndex::VMIN,
        SpecialCodeIndex::VSWTC,
        SpecialCodeIndex::VSTART,
        SpecialCodeIndex::VSTOP,
        SpecialCodeIndex::VSUSP,
        SpecialCodeIndex::VEOL,
        SpecialCodeIndex::VREPRINT,
        SpecialCodeIndex::VDISCARD,
        SpecialCodeIndex::VWERASE,
        SpecialCodeIndex::VLNEXT,
        SpecialCodeIndex::VEOL2,
    ];
    CODES
        .iter()
        .all(|index| left.special_codes[*index] == right.special_codes[*index])
}

fn current_tid() -> i32 {
    // `gettid` does not fail. A thread id is a positive pid.
    let tid = unsafe { libc::syscall(libc::SYS_gettid) };
    tid as i32
}

fn load_state() -> Option<&'static Shared> {
    let ptr = STATE.load(Ordering::Acquire);
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { &*ptr })
    }
}

fn restore_signal_safe(state: &Shared) -> bool {
    if !state.active.load(Ordering::Acquire) {
        return true;
    }
    write_all_nb(state.fd, LEAVE);
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    if tcsetattr(fd, OptionalActions::Now, &state.original).is_err() {
        return false;
    }
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    let _ = rustix::fs::fcntl_setfl(fd, state.original_flags);
    state.active.store(false, Ordering::Release);
    true
}

fn reenter(state: &Shared) {
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    if tcsetattr(fd, OptionalActions::Now, &state.raw).is_err() {
        return;
    }
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    let _ = rustix::fs::fcntl_setfl(fd, state.raw_flags);
    write_all_nb(state.fd, ENTER);
    state.active.store(true, Ordering::Release);
    state.resumed.store(true, Ordering::Release);
}

fn write_all_nb(fd: RawFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match rustix::io::write(borrowed, bytes) {
            Ok(0) => break,
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::INTR) => continue,
            Err(_) => break,
        }
    }
}

fn install_panic_hook() {
    if HOOK_ON.swap(true, Ordering::AcqRel) {
        return;
    }
    let previous: PanicHook = std::panic::take_hook();
    PREV_HOOK.store(Box::into_raw(Box::new(previous)), Ordering::Release);
    std::panic::set_hook(Box::new(|info| {
        if OWNER.load(Ordering::Acquire) == current_tid() {
            if let Some(state) = load_state() {
                let _ = restore_signal_safe(state);
            }
        }
        let ptr = PREV_HOOK.load(Ordering::Acquire);
        if !ptr.is_null() {
            unsafe { (*ptr)(info) };
        }
    }));
}

fn uninstall_panic_hook() {
    HOOK_ON.store(false, Ordering::Release);
    let ptr = PREV_HOOK.swap(std::ptr::null_mut(), Ordering::AcqRel);
    if !ptr.is_null() {
        let previous = unsafe { Box::from_raw(ptr) };
        std::panic::set_hook(*previous);
    }
}

fn install_signal(signal: libc::c_int, handler: extern "C" fn(libc::c_int)) -> io::Result<()> {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handler as usize;
        action.sa_flags = libc::SA_RESTART;
        if libc::sigemptyset(&mut action.sa_mask) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn unblock_signal(signal: libc::c_int) {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut set) != 0 {
            return;
        }
        if libc::sigaddset(&mut set, signal) != 0 {
            return;
        }
        // The worker is one thread, but a Rust process is linked with
        // libpthread. `pthread_sigmask` is the call that is specified there.
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
    }
}

fn set_default(signal: libc::c_int) {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = libc::SIG_DFL;
        action.sa_flags = 0;
        let _ = libc::sigemptyset(&mut action.sa_mask);
        let _ = libc::sigaction(signal, &action, std::ptr::null_mut());
    }
}

extern "C" fn handle_tstp(_signal: libc::c_int) {
    let Some(state) = load_state() else {
        return;
    };
    let _ = restore_signal_safe(state);
    state.suspended.store(true, Ordering::Release);
    // The signal is blocked for the rest of this handler, so a `raise` while
    // it is blocked only marks it pending. Reset the action, unblock, then
    // raise: the default action stops the process inside `raise`, and
    // `SIGCONT` resumes there. Reinstall only after that. Reinstalling while
    // the stop is still pending delivers it back to this handler, and the
    // process never stops.
    set_default(libc::SIGTSTP);
    unblock_signal(libc::SIGTSTP);
    unsafe { libc::raise(libc::SIGTSTP) };
    let _ = install_signal(libc::SIGTSTP, handle_tstp);
}

extern "C" fn handle_cont(_signal: libc::c_int) {
    let Some(state) = load_state() else {
        return;
    };
    if state.suspended.swap(false, Ordering::AcqRel) {
        reenter(state);
    }
}

extern "C" fn handle_winch(_signal: libc::c_int) {
    let fd = WINCH_WRITE.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    let byte = [0u8; 1];
    unsafe {
        libc::write(fd, byte.as_ptr().cast(), 1);
    }
}

extern "C" fn handle_hup(_signal: libc::c_int) {
    let Some(state) = load_state() else {
        return;
    };
    let _ = restore_signal_safe(state);
    set_default(libc::SIGHUP);
    // Unblock before `raise`, or the signal stays pending and this handler
    // returns into the owner loop.
    unblock_signal(libc::SIGHUP);
    unsafe { libc::raise(libc::SIGHUP) };
}

fn wait_readable<Fd: AsFd>(fd: Fd, timeout: Duration) -> io::Result<bool> {
    let mut fds = [rustix::event::PollFd::new(
        &fd,
        rustix::event::PollFlags::IN,
    )];
    let spec = rustix::event::Timespec::try_from(timeout).unwrap_or(rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    });
    loop {
        match rustix::event::poll(&mut fds, Some(&spec)) {
            Ok(0) => return Ok(false),
            Ok(_) => {
                let ready = fds[0].revents();
                return Ok(ready.contains(rustix::event::PollFlags::IN)
                    || ready.contains(rustix::event::PollFlags::HUP)
                    || ready.contains(rustix::event::PollFlags::ERR));
            }
            Err(Errno::INTR) => continue,
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}

fn read_some<Fd: AsFd>(fd: Fd) -> io::Result<Vec<u8>> {
    let mut buf = [0u8; 8192];
    loop {
        match rustix::io::read(&fd, &mut buf) {
            Ok(0) => return Ok(Vec::new()),
            Ok(n) => return Ok(buf[..n].to_vec()),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => return Ok(Vec::new()),
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}
