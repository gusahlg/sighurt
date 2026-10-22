//! The terminal session: raw mode, probe, frames, and restoration.
//!
//! One session per process. The lock is only taken on the owner thread.
//! Signal handlers and the panic hook load an atomic pointer and do not take
//! that lock, allocate, or call the previous owner. `restore` is safe to call
//! twice. Drop calls it too.
//!
//! The owner thread is the only writer, except on a path that has to restore
//! the terminal before the process dies. `SIGTSTP` and `SIGCONT` only set an
//! atomic and wake the self-pipe; [`Session::take_resumed`] restores, stops,
//! and enters raw mode again after `SIGCONT`. `SIGWINCH` only writes that
//! pipe. `SIGHUP`, the panic hook and [`restore_before_forced_exit`] still
//! write, and only with async-signal-safe calls (`write`, `poll`, `fcntl`,
//! `tcsetattr`, `clock_gettime`): no allocation and no lock. Handlers that
//! return save and restore `errno`, so a full self-pipe cannot replace the
//! interrupted call's error.
//!
//! Those fatal writers see an exact wire classification. Every output `write`
//! stores it *before* the syscall, as the state the terminal is in if the
//! kernel accepts a prefix and stops: inside sequence X from its first byte
//! until the byte that leaves X has been accepted. The classification and its
//! generation share one atomic, so a handler cannot observe a new generation
//! with a stale state. After a fatal writer takes the wire it bumps that
//! generation; the owner then drops the queue instead of resuming the tail.
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
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags};
use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::pipe::PipeFlags;
use rustix::pty::OpenptFlags;
use rustix::termios::{tcgetattr, tcsetattr, OptionalActions, SpecialCodeIndex, Termios, Winsize};

use super::modes::{ENTER, LEAVE, QUERIES};
use super::output::{advance, plan_write, terminator, Output, Wire};
use super::parser::{Input, Parser};
use super::probe::{self, Caps};

const DEFAULT_CAP: usize = 256 * 1024;
/// Bytes one [`Session::read_input`] call parses before it returns.
pub const INPUT_BYTE_BUDGET: usize = 64 * 1024;
/// Events one [`Session::read_input`] call returns before it stops.
///
/// A legacy key is a press and a release, so the call may overshoot by one
/// byte's events. The rest stays in the kernel or in [`Session`]'s hold, and
/// [`Session::input_pending`] is set so the owner reads again without sleeping.
pub const INPUT_EVENT_CAP: usize = 256;
/// How long restoration may wait for the terminal to accept bytes.
const RESTORE_BUDGET: Duration = Duration::from_millis(250);

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
    /// Raw mode is in effect. Cleared only after termios restoration succeeds,
    /// so a later retry can still send [`LEAVE`] when this is already false.
    active: AtomicBool,
    /// How many bytes of [`LEAVE`] have been written. A retry continues here.
    leave_at: AtomicUsize,
    /// [`LEAVE`] has been written in full.
    protocol_restored: AtomicBool,
    /// [`Wire`] in the low 8 bits, generation above that. See the module docs.
    /// The fatal path reads this and does not look at the output queue.
    wire_packed: AtomicU64,
    /// `SIGTSTP` wants the owner to restore and stop. The handler does not
    /// write the terminal or stop the process itself.
    suspend_requested: AtomicBool,
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
    /// Bytes taken from the terminal and not yet parsed.
    held: Vec<u8>,
    held_at: usize,
    /// [`Self::read_input`] stopped with more input already available.
    more_input: bool,
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
            leave_at: AtomicUsize::new(0),
            protocol_restored: AtomicBool::new(false),
            wire_packed: AtomicU64::new(u64::from(Wire::Ground as u8)),
            suspend_requested: AtomicBool::new(false),
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
            held: Vec::new(),
            held_at: 0,
            more_input: false,
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
    /// send [`LEAVE`].
    ///
    /// Termios and the reset bytes are tracked apart: a termios success does
    /// not forget a [`LEAVE`] that the terminal did not accept, and a later
    /// call sends the rest. Bytes already started on the wire are finished or
    /// closed before [`LEAVE`]. The write waits at most [`RESTORE_BUDGET`],
    /// and every attempt forces non-blocking mode first so a full pty cannot
    /// block the write. The captured flags are put back afterwards. When the
    /// reset bytes do not all land, this returns an error; it does not hang.
    /// A call after both parts have succeeded writes nothing.
    pub fn restore(&mut self) -> io::Result<()> {
        let done = !self.shared().active.load(Ordering::Acquire)
            && self.shared().protocol_restored.load(Ordering::Acquire);
        if done {
            return Ok(());
        }
        let deadline = mono_after(RESTORE_BUDGET);
        if !self.shared().protocol_restored.load(Ordering::Acquire) {
            // Once LEAVE has started, the queue was already finished or
            // abandoned. Cancelling it again would cut the half-written reset.
            let started = self.shared().leave_at.load(Ordering::Acquire) > 0;
            let queue_done = started || self.finish_queued(deadline);
            if queue_done {
                write_protocol(self.shared(), deadline);
            }
        }
        if !restore_termios(self.shared()) {
            return Err(io::Error::other(
                "tcsetattr failed while restoring the terminal",
            ));
        }
        // `write_protocol` forced non-blocking mode. Put the captured flags
        // back now that the writes have stopped, including when they timed out.
        restore_original_flags(self.shared());
        if self.shared().protocol_restored.load(Ordering::Acquire) {
            return Ok(());
        }
        log::warn!("terminal protocol was not fully restored");
        Err(io::Error::new(
            ErrorKind::TimedOut,
            "terminal protocol was not fully restored",
        ))
    }

    /// `SIGTSTP` asks the owner to stop; the owner does it in
    /// [`Self::take_resumed`]. `SIGCONT` only wakes the self-pipe. `SIGHUP`
    /// restores and re-raises, because the process is about to die.
    /// `SIGWINCH` writes one byte to [`Self::winch_fd`]. The state box is leaked.
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

    /// Performs a pending `SIGTSTP` on this thread, then reports whether
    /// `SIGCONT` has re-entered since the last call.
    ///
    /// The handler only sets a flag. Stopping here, on the owner, is what
    /// keeps a signal from writing into the middle of an escape sequence.
    /// The call blocks in `SIGTSTP` until `SIGCONT`. The owner loop calls
    /// this every turn.
    pub fn take_resumed(&self) -> bool {
        if self
            .shared()
            .suspend_requested
            .swap(false, Ordering::AcqRel)
        {
            self.suspend_owner();
        }
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
        self.sync_wire();
        let state = self.state;
        // `state` is this session's box. The closure runs before this returns.
        self.output.write_frame(&self.fd, frame, |wire| {
            store_wire_keep_gen(unsafe { &*state }, wire)
        })
    }

    /// Drop bytes not yet written. An open sequence is kept or closed.
    /// See [`super::Terminal::discard_pending`].
    pub fn discard_pending(&mut self) {
        self.sync_wire();
        self.output.discard_to_boundary();
        self.publish_wire();
    }

    /// Whether [`Self::read_input`] left input that a later call should take.
    pub fn input_pending(&self) -> bool {
        self.more_input || self.held_at < self.held.len()
    }

    /// Send the startup queries and collect replies until DA1 or `deadline`.
    ///
    /// A read happens only after `poll` says the fd is readable, and only for
    /// the time still inside `deadline`. The timeout is sampled after
    /// `prepare_read`: those `fcntl` calls can outlast the time remaining, and
    /// a timeout chosen earlier would let `poll` run past the deadline.
    /// `O_NONBLOCK` is put back first: a panic used to clear it and turn the
    /// follow-up `read` into a blocking canonical read. A terminal that never
    /// answers cannot stall this past the deadline.
    pub fn probe(&mut self, deadline: Duration) -> io::Result<Caps> {
        self.write_frame(QUERIES)?;
        let start = std::time::Instant::now();
        let mut caps = Caps::default();
        let mut saw_da1 = false;
        loop {
            if !self.prepare_read()? {
                break;
            }
            let remaining = deadline.saturating_sub(start.elapsed());
            let wait = if saw_da1 { Duration::ZERO } else { remaining };
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

    /// Read what is already buffered, up to [`INPUT_BYTE_BUDGET`] and
    /// [`INPUT_EVENT_CAP`], and parse it with the probe's parser.
    ///
    /// Consecutive mouse-motion reports collapse to the latest one. When the
    /// budget stops the call early, [`Self::input_pending`] is set and
    /// [`Self::poll_timeout`] is zero so the owner comes back without sleeping.
    pub fn read_input(&mut self) -> io::Result<Vec<Input>> {
        let mut inputs = Vec::new();
        let mut parsed = 0usize;
        let mut hit_budget = false;
        while inputs.len() < INPUT_EVENT_CAP && parsed < INPUT_BYTE_BUDGET {
            if self.held_at >= self.held.len() {
                self.held.clear();
                self.held_at = 0;
                if !self.prepare_read()? {
                    break;
                }
                let mut buf = [0u8; 8192];
                let room = (INPUT_BYTE_BUDGET - parsed).min(buf.len());
                let n = read_into(&self.fd, &mut buf[..room])?;
                if n == 0 {
                    break;
                }
                self.held.extend_from_slice(&buf[..n]);
            }
            let available = self.held.len() - self.held_at;
            // A legacy key is two events per byte. Feeding `room / 2` bytes
            // cannot run far past the event cap; one byte is the minimum.
            let room = INPUT_EVENT_CAP - inputs.len();
            let allow = (room / 2)
                .max(1)
                .min(available)
                .min(INPUT_BYTE_BUDGET - parsed);
            let end = self.held_at + allow;
            let got = self.parser.feed(&self.held[self.held_at..end]);
            self.held_at = end;
            parsed += allow;
            for input in got {
                push_coalesced(&mut inputs, input);
            }
            if parsed >= INPUT_BYTE_BUDGET || inputs.len() >= INPUT_EVENT_CAP {
                hit_budget = true;
                break;
            }
        }
        if self.held_at >= self.held.len() {
            self.held.clear();
            self.held_at = 0;
        }
        let stashed = self.held_at < self.held.len();
        self.more_input = stashed;
        if stashed {
            self.poke_winch();
        } else if hit_budget {
            self.more_input = wait_readable(&self.fd, Duration::ZERO).unwrap_or(false);
        }
        Ok(inputs)
    }

    /// Time until a partial escape sequence should be expired.
    ///
    /// [`Duration::ZERO`] when [`Self::read_input`] left input pending, so the
    /// owner loop does not sleep before the next read.
    pub fn poll_timeout(&self) -> Option<Duration> {
        if self.shared().suspend_requested.load(Ordering::Acquire) {
            return Some(Duration::ZERO);
        }
        if self.more_input || self.held_at < self.held.len() {
            return Some(Duration::ZERO);
        }
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

    /// Copy a fatal writer's wire over the queue when it has taken the generation.
    fn sync_wire(&mut self) {
        let (wire, gen) = load_wire(self.shared());
        self.output.sync_handler(wire as u8, gen);
    }

    /// Publish the queue's wire unless a fatal writer has taken it.
    fn publish_wire(&mut self) {
        let wire = Wire::from_u8(self.output.wire_byte());
        if !store_wire_keep_gen(self.shared(), wire) {
            self.sync_wire();
        }
    }

    /// Restore, stop, and enter again after `SIGCONT`. Runs on the owner.
    ///
    /// The output queue is not written here (`take_resumed` is shared). The
    /// open sequence is closed from the published wire, then [`LEAVE`].
    /// [`reenter`] bumps the generation so the next owner write drops the tail.
    fn suspend_owner(&self) {
        let state = self.shared();
        let deadline = mono_after(RESTORE_BUDGET);
        write_protocol(state, deadline);
        if !restore_termios(state) {
            log::warn!("tcsetattr failed while suspending");
        } else {
            restore_original_flags(state);
        }
        if !state.protocol_restored.load(Ordering::Acquire) {
            log::warn!("terminal protocol was not fully restored before suspend");
        }
        // Block in the default stop. `SIGCONT` resumes inside `raise`, and
        // this thread — not the `SIGCONT` handler — enters raw mode again.
        set_default(libc::SIGTSTP);
        unblock_signal(libc::SIGTSTP);
        unsafe { libc::raise(libc::SIGTSTP) };
        let _ = install_signal(libc::SIGTSTP, handle_tstp);
        reenter(state);
    }

    /// Write the open sequence's tail, or the bytes that close it.
    ///
    /// `false` means the deadline ran out with bytes still queued. The caller
    /// must not start [`LEAVE`] on top of that sequence.
    fn finish_queued(&mut self, deadline: Mono) -> bool {
        self.sync_wire();
        let bytes = self.output.take_inflight();
        if bytes.is_empty() {
            self.publish_wire();
            return true;
        }
        let (n, handoff) = write_tracked(self.shared(), &bytes, Some(deadline));
        if handoff {
            // A fatal writer took the wire. The tail belongs to the sequence
            // it already closed.
            return false;
        }
        self.output.advance_wire(&bytes[..n]);
        let done = n == bytes.len();
        if !done {
            self.output.prepend(&bytes[n..]);
        }
        done
    }

    /// Wake the owner through the `SIGWINCH` pipe. One extra byte only makes
    /// the loop call [`Self::read_input`] again; the size check is idempotent.
    fn poke_winch(&self) {
        let byte = [1u8];
        let _ = rustix::io::write(&self.winch_write, &byte);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let write = self.winch_write.as_raw_fd();
        if WINCH_WRITE.load(Ordering::Acquire) == write {
            WINCH_WRITE.store(-1, Ordering::Release);
        }
        if std::thread::panicking() {
            let _ = restore_signal_safe(self.shared());
        } else {
            let _ = self.restore();
        }
        if OWNER.load(Ordering::Acquire) == current_tid() {
            OWNER.store(0, Ordering::Release);
        }
        // `set_hook` panics if called while this thread is unwinding, and
        // that panic aborts the process. Leave the hook installed; it
        // no-ops once `STATE` is cleared.
        if !std::thread::panicking() {
            uninstall_panic_hook();
        }
        let handlers = self.shared().handlers.load(Ordering::Acquire);
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
    restore_with_deadline(state, mono_after(RESTORE_BUDGET))
}

/// Write whatever is left of the reset, then restore termios.
///
/// `protocol_restored` and `active` are independent. A full [`LEAVE`] sets the
/// first. A successful `tcsetattr` clears `active`. Either may already be done.
/// The write polls for `POLLOUT` until `deadline` and does not allocate.
/// This is the signal-handler path: it cannot see the output queue, so an open
/// sequence is closed with [`terminator`] before [`LEAVE`].
fn restore_with_deadline(state: &Shared, deadline: Mono) -> bool {
    write_protocol(state, deadline);
    let termios_ok = restore_termios(state);
    if termios_ok {
        // Writes forced non-blocking mode. The captured flags go back even
        // when the reset bytes did not all land, so a later retry's first
        // `write` is not a blocking write on a full pty — that retry forces
        // non-blocking mode again before it writes.
        restore_original_flags(state);
    }
    // The owner, if this returns, must not resume the queue this closed.
    take_wire(state);
    termios_ok && state.protocol_restored.load(Ordering::Acquire)
}

/// Put the captured termios back. Does not change file flags and does not write.
fn restore_termios(state: &Shared) -> bool {
    if !state.active.load(Ordering::Acquire) {
        return true;
    }
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    if tcsetattr(fd, OptionalActions::Now, &state.original).is_err() {
        return false;
    }
    state.active.store(false, Ordering::Release);
    true
}

fn force_nonblock(state: &Shared) {
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    let _ = rustix::fs::fcntl_setfl(fd, state.raw_flags);
}

fn restore_original_flags(state: &Shared) {
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    let _ = rustix::fs::fcntl_setfl(fd, state.original_flags);
}

/// Close an open control sequence, then write [`LEAVE`] from `leave_at`.
///
/// Once [`LEAVE`] has started, a retry continues it instead of cancelling the
/// half-written sequence. Nothing is written when the previous call finished.
fn write_protocol(state: &Shared, deadline: Mono) {
    if state.protocol_restored.load(Ordering::Acquire) {
        return;
    }
    let mut off = state.leave_at.load(Ordering::Acquire);
    if off > LEAVE.len() {
        off = LEAVE.len();
    }
    if off == 0 {
        let (wire, _) = load_wire(state);
        let term = terminator(wire);
        if !term.is_empty() {
            let (n, handoff) = write_tracked(state, term, Some(deadline));
            if handoff || n == 0 {
                return;
            }
            if n < term.len() {
                return;
            }
        }
    }
    let (n, handoff) = write_tracked(state, &LEAVE[off..], Some(deadline));
    if n > 0 {
        off += n;
        state.leave_at.store(off, Ordering::Release);
    }
    if handoff {
        return;
    }
    if off >= LEAVE.len() {
        state.protocol_restored.store(true, Ordering::Release);
    }
}

fn reenter(state: &Shared) {
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    if tcsetattr(fd, OptionalActions::Now, &state.raw).is_err() {
        return;
    }
    let fd = unsafe { BorrowedFd::borrow_raw(state.fd) };
    let _ = rustix::fs::fcntl_setfl(fd, state.raw_flags);
    // `swap` remembers whether the previous restore finished, then arms the
    // next one. ENTER is scanned from ground only when that restore finished.
    let restored = state.protocol_restored.swap(false, Ordering::AcqRel);
    state.leave_at.store(0, Ordering::Release);
    state.active.store(true, Ordering::Release);
    if restored {
        let _ = store_wire_keep_gen(state, Wire::Ground);
    }
    let _ = write_tracked(state, ENTER, None);
    // Drop whatever the suspension cut off, instead of resuming that tail.
    take_wire(state);
    state.resumed.store(true, Ordering::Release);
}

/// Write `bytes` from the published wire, storing the classification before
/// each syscall. See the module docs. `deadline` forces non-blocking mode and
/// polls until then; `None` is one non-blocking attempt per chunk.
///
/// The boolean is `true` when a fatal writer took the generation and this
/// must not keep writing.
fn write_tracked(state: &Shared, bytes: &[u8], deadline: Option<Mono>) -> (usize, bool) {
    if deadline.is_some() {
        force_nonblock(state);
    }
    let (mut start, _) = load_wire(state);
    let mut written = 0;
    let mut rest = bytes;
    while !rest.is_empty() {
        let (len, publish) = plan_write(start, rest);
        if len == 0 {
            break;
        }
        if !store_wire_keep_gen(state, publish) {
            return (written, true);
        }
        let n = match deadline {
            Some(deadline) => write_deadline(state.fd, &rest[..len], deadline),
            None => write_some_nb(state.fd, &rest[..len]),
        };
        if n == 0 {
            let _ = store_wire_keep_gen(state, start);
            break;
        }
        let exact = advance(start, &rest[..n]);
        if !store_wire_keep_gen(state, exact) {
            return (written + n, true);
        }
        start = exact;
        written += n;
        rest = &rest[n..];
        if n < len {
            break;
        }
    }
    (written, false)
}

fn write_some_nb(fd: RawFd, bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    loop {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match rustix::io::write(borrowed, bytes) {
            Ok(n) => return n,
            Err(Errno::INTR) => continue,
            Err(_) => return 0,
        }
    }
}

fn load_wire(state: &Shared) -> (Wire, u64) {
    unpack_wire(state.wire_packed.load(Ordering::Acquire))
}

/// Store `wire` without changing the generation. `false` when a fatal writer
/// has taken the generation; the previous value is left in place.
fn store_wire_keep_gen(state: &Shared, wire: Wire) -> bool {
    let mut current = state.wire_packed.load(Ordering::Acquire);
    loop {
        let (_stored, gen) = unpack_wire(current);
        let next = pack_wire(gen, wire);
        if next == current {
            return true;
        }
        match state.wire_packed.compare_exchange_weak(
            current,
            next,
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(observed) => {
                if unpack_wire(observed).1 != gen {
                    return false;
                }
                current = observed;
            }
        }
    }
}

/// Publish `wire` and bump the generation, so the owner drops its queue.
fn take_wire(state: &Shared) {
    let (wire, _) = load_wire(state);
    store_wire_bump(state, wire);
}

fn store_wire_bump(state: &Shared, wire: Wire) {
    let mut current = state.wire_packed.load(Ordering::Acquire);
    loop {
        let gen = unpack_wire(current).1.wrapping_add(1);
        match state.wire_packed.compare_exchange_weak(
            current,
            pack_wire(gen, wire),
            Ordering::Release,
            Ordering::Acquire,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

fn pack_wire(gen: u64, wire: Wire) -> u64 {
    (gen << 8) | u64::from(wire as u8)
}

fn unpack_wire(packed: u64) -> (Wire, u64) {
    (Wire::from_u8(packed as u8), packed >> 8)
}

/// `poll` for `POLLOUT` until `deadline`, then `write`. Both are async-signal-safe.
/// A hard error or a timeout returns how many bytes were accepted.
/// The fd must already be non-blocking: the first `write` is not preceded by `poll`.
/// `EINTR` does not restart `deadline`; the time left is read again.
fn write_deadline(fd: RawFd, mut bytes: &[u8], deadline: Mono) -> usize {
    let mut written = 0;
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match rustix::io::write(borrowed, bytes) {
            Ok(0) | Err(Errno::AGAIN) => {
                if !wait_out(fd, deadline) {
                    break;
                }
            }
            Ok(n) => {
                bytes = &bytes[n..];
                written += n;
            }
            Err(Errno::INTR) => continue,
            Err(_) => break,
        }
    }
    written
}

fn wait_out(fd: RawFd, deadline: Mono) -> bool {
    let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
    poll_flags(&borrowed, PollFlags::OUT, Some(deadline)).unwrap_or(false)
}

/// `CLOCK_MONOTONIC` time. `clock_gettime` is async-signal-safe; this is the
/// deadline clock for restoration, including the `SIGHUP` handler.
#[derive(Clone, Copy)]
struct Mono {
    sec: i64,
    nsec: i64,
}

fn mono_now() -> Mono {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        let _ = libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    Mono {
        sec: ts.tv_sec,
        nsec: ts.tv_nsec,
    }
}

fn mono_after(budget: Duration) -> Mono {
    mono_now().saturating_add(budget)
}

impl Mono {
    fn saturating_add(self, extra: Duration) -> Self {
        let add_sec = i64::try_from(extra.as_secs()).unwrap_or(i64::MAX);
        let mut sec = self.sec.saturating_add(add_sec);
        let mut nsec = self.nsec.saturating_add(i64::from(extra.subsec_nanos()));
        if nsec >= 1_000_000_000 {
            sec = sec.saturating_add(nsec / 1_000_000_000);
            nsec %= 1_000_000_000;
        }
        Self { sec, nsec }
    }

    /// Time left until `deadline`. `None` when it has passed.
    fn remaining_until(self, deadline: Self) -> Option<Duration> {
        if self.sec > deadline.sec || (self.sec == deadline.sec && self.nsec >= deadline.nsec) {
            return None;
        }
        let mut sec = deadline.sec - self.sec;
        let mut nsec = deadline.nsec - self.nsec;
        if nsec < 0 {
            sec -= 1;
            nsec += 1_000_000_000;
        }
        let secs = u64::try_from(sec).unwrap_or(0);
        let nanos = u32::try_from(nsec).unwrap_or(0);
        Some(Duration::new(secs, nanos))
    }
}

/// Wait until `fd` matches `flags`.
///
/// `deadline` is absolute. `EINTR` reads the clock again and waits only for
/// what is left, so a repeated `SIGWINCH` cannot extend it. `None` is one
/// non-blocking check; `EINTR` retries that check and does not wait.
fn poll_flags<Fd: AsFd>(fd: &Fd, flags: PollFlags, deadline: Option<Mono>) -> io::Result<bool> {
    loop {
        let timeout = match deadline {
            Some(deadline) => match mono_now().remaining_until(deadline) {
                Some(left) => left,
                None => return Ok(false),
            },
            None => Duration::ZERO,
        };
        let spec = rustix::event::Timespec::try_from(timeout).unwrap_or(rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        });
        let mut fds = [PollFd::new(fd, flags)];
        match rustix::event::poll(&mut fds, Some(&spec)) {
            Ok(0) => return Ok(false),
            Ok(_) => {
                let ready = fds[0].revents();
                return Ok(ready.contains(flags)
                    || ready.contains(PollFlags::HUP)
                    || ready.contains(PollFlags::ERR));
            }
            Err(Errno::INTR) => continue,
            Err(err) => return Err(io::Error::from(err)),
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
    let saved = errno_raw();
    if let Some(state) = load_state() {
        // The owner restores and stops. Writing here would race a partial APC.
        state.suspend_requested.store(true, Ordering::Release);
        wake_winch();
    }
    set_errno_raw(saved);
}

extern "C" fn handle_cont(_signal: libc::c_int) {
    let saved = errno_raw();
    // Resuming after our own `SIGTSTP` returns inside the owner's `raise`.
    // An external continue only has to wake the loop. Neither path writes.
    wake_winch();
    set_errno_raw(saved);
}

extern "C" fn handle_winch(_signal: libc::c_int) {
    let saved = errno_raw();
    wake_winch();
    set_errno_raw(saved);
}

fn errno_raw() -> libc::c_int {
    unsafe { *libc::__errno_location() }
}

fn set_errno_raw(saved: libc::c_int) {
    unsafe {
        *libc::__errno_location() = saved;
    }
}

/// One byte on the `SIGWINCH` pipe. Async-signal-safe: a `write` of one byte.
fn wake_winch() {
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
    let saved = errno_raw();
    if let Some(state) = load_state() {
        let _ = restore_signal_safe(state);
        set_default(libc::SIGHUP);
        // Unblock before `raise`, or the signal stays pending and this handler
        // returns into the owner loop.
        unblock_signal(libc::SIGHUP);
        unsafe { libc::raise(libc::SIGHUP) };
    }
    set_errno_raw(saved);
}

fn wait_readable<Fd: AsFd>(fd: Fd, timeout: Duration) -> io::Result<bool> {
    let deadline = if timeout.is_zero() {
        None
    } else {
        Some(mono_after(timeout))
    };
    poll_flags(&fd, PollFlags::IN, deadline)
}

fn push_coalesced(out: &mut Vec<Input>, input: Input) {
    if let Input::Mouse(ref mouse) = input {
        if mouse.motion {
            if let Some(Input::Mouse(previous)) = out.last_mut() {
                if previous.motion {
                    *previous = mouse.clone();
                    return;
                }
            }
        }
    }
    out.push(input);
}

fn read_into<Fd: AsFd>(fd: Fd, buf: &mut [u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    loop {
        match rustix::io::read(&fd, &mut *buf) {
            Ok(n) => return Ok(n),
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => return Ok(0),
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
