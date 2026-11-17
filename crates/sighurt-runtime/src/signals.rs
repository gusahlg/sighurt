//! What the process is asked from outside while a frontend runs: to quit, saving the session,
//! or to report its diagnostics.
//!
//! What was asked and not yet taken is one flag per [`Request`] ([`Pending`]): a flood of one
//! kind can't push out the other, and asking twice before the owner looks is asking once.
//! Raising a flag is one lock-free atomic store, so a signal handler may do it.
//!
//! On Unix the requests are signals. `sig`'s handlers (its `main`) raise the flag of the
//! signal's request ([`request_for`]) and then write a byte to a non-blocking pipe, only to wake
//! the owner loop: a byte lost to a full pipe loses nothing, since the pipe is then readable
//! anyway. [`Signals`] holds the pipe's read end, which the owner loop waits on (`AsFd`), and
//! [`Signals::take`] empties the pipe before it takes the flags, so a request raised after the
//! pipe was read is either taken now or wakes the loop again. SIGUSR1 asks for the report;
//! SIGTERM, SIGINT and SIGHUP ask to quit.
//!
//! Elsewhere (Windows) `sig`'s console control handler (its `main`) sends a quit for Ctrl+C,
//! Ctrl+Break and the console closing through a [`Sender`] ([`Signals::channel`]), which raises
//! the flag and runs the wake the frontend set with [`Signals::set_wake`]; the owner loop then
//! takes the flags with [`Signals::take`]. A report is asked for by setting a named event, whose
//! thread (`main`'s too) sends it the same way. [`Signals::none`] is one nothing sends to.
//!
//! Owns: turning what arrives into [`Request`]s. Must not own: installing the handlers
//! (process-wide setup, the executable's) or what a request does (the frontend's).

#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(not(unix))]
use std::sync::{Mutex, PoisonError};

/// What the process was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Quit cleanly: SIGTERM, SIGINT or SIGHUP.
    Quit,
    /// Report the diagnostics: SIGUSR1.
    Report,
}

/// The requests asked and not yet taken: one flag for each.
#[derive(Debug, Default)]
pub struct Pending {
    quit: AtomicBool,
    report: AtomicBool,
}

impl Pending {
    /// Marks `request` as asked. One lock-free atomic store: async-signal-safe.
    pub fn raise(&self, request: Request) {
        match request {
            Request::Quit => self.quit.store(true, Ordering::Release),
            Request::Report => self.report.store(true, Ordering::Release),
        }
    }

    /// What was asked since the last call, each at most once: a report before a quit, so a
    /// report asked with a quit is still made.
    fn take(&self) -> Vec<Request> {
        let mut requests = Vec::new();
        if self.report.swap(false, Ordering::AcqRel) {
            requests.push(Request::Report);
        }
        if self.quit.swap(false, Ordering::AcqRel) {
            requests.push(Request::Quit);
        }
        requests
    }
}

/// Where the requests arrive.
#[derive(Debug)]
pub struct Signals {
    pending: Arc<Pending>,
    #[cfg(unix)]
    pipe: OwnedFd,
    #[cfg(not(unix))]
    wake: Arc<Wake>,
}

/// The wake the frontend set ([`Signals::set_wake`]).
#[cfg(not(unix))]
#[derive(Default)]
struct Wake(Mutex<Option<Box<dyn Fn() + Send>>>);

#[cfg(not(unix))]
impl std::fmt::Debug for Wake {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wake").finish_non_exhaustive()
    }
}

/// What sends requests to a [`Signals`]: the console control handler `sig` installs.
#[cfg(not(unix))]
#[derive(Clone, Debug)]
pub struct Sender {
    pending: Arc<Pending>,
    wake: Arc<Wake>,
}

#[cfg(not(unix))]
impl Sender {
    /// Raises `request` and wakes the owner loop, if it set a wake.
    pub fn send(&self, request: Request) {
        self.pending.raise(request);
        let wake = self.wake.0.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(wake) = wake.as_ref() {
            wake();
        }
    }
}

/// The request a signal asks for: SIGUSR1 a report, SIGTERM, SIGINT and SIGHUP a quit.
#[cfg(unix)]
pub fn request_for(signal: i32) -> Option<Request> {
    match signal {
        libc::SIGUSR1 => Some(Request::Report),
        libc::SIGTERM | libc::SIGINT | libc::SIGHUP => Some(Request::Quit),
        _ => None,
    }
}

impl Signals {
    /// The flags `sig`'s signal handlers raise, and the read end of the pipe they then write a
    /// byte to, to wake the loop. The pipe must be non-blocking.
    #[cfg(unix)]
    pub fn from_pipe(pipe: OwnedFd, pending: Arc<Pending>) -> Self {
        Self { pending, pipe }
    }

    /// Nothing sends to it: nothing ever arrives.
    #[cfg(not(unix))]
    pub fn none() -> Self {
        Self {
            pending: Arc::default(),
            wake: Arc::default(),
        }
    }

    /// The flags, and what raises them.
    #[cfg(not(unix))]
    pub fn channel() -> (Sender, Self) {
        let signals = Self::none();
        let sender = Sender {
            pending: Arc::clone(&signals.pending),
            wake: Arc::clone(&signals.wake),
        };
        (sender, signals)
    }

    /// Runs `wake` after each request sent from now on (on the sender's thread), and once now
    /// when a request is already waiting. Replaces the wake set before.
    #[cfg(not(unix))]
    pub fn set_wake(&self, wake: impl Fn() + Send + 'static) {
        let mut slot = self.wake.0.lock().unwrap_or_else(PoisonError::into_inner);
        *slot = Some(Box::new(wake));
        let waiting = self.pending.quit.load(Ordering::Acquire)
            || self.pending.report.load(Ordering::Acquire);
        if waiting {
            if let Some(wake) = slot.as_ref() {
                wake();
            }
        }
    }

    /// What was asked since the last call: a report, then a quit, each at most once.
    pub fn take(&self) -> Vec<Request> {
        // Empty the pipe first: a handler raises its flag before it writes, so a flag raised
        // after this read is taken below or leaves a byte that wakes the loop again.
        #[cfg(unix)]
        {
            let mut bytes = [0u8; 64];
            while let Ok(1..) = rustix::io::read(&self.pipe, &mut bytes) {}
        }
        self.pending.take()
    }
}

#[cfg(unix)]
impl AsFd for Signals {
    /// The descriptor to wait on for readability.
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.pipe.as_fd()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A non-blocking pipe and the flags, as `sig`'s `main` makes them.
    fn signals() -> (Signals, Arc<Pending>, OwnedFd) {
        let (read, write) = rustix::pipe::pipe().unwrap();
        for end in [&read, &write] {
            rustix::io::ioctl_fionbio(end, true).unwrap();
        }
        let pending = Arc::new(Pending::default());
        (
            Signals::from_pipe(read, Arc::clone(&pending)),
            pending,
            write,
        )
    }

    /// What `sig`'s handler does for `signal`: raise its flag, then write a byte to wake.
    fn deliver(pending: &Pending, write: &OwnedFd, signal: i32) {
        if let Some(request) = request_for(signal) {
            pending.raise(request);
        }
        let _ = rustix::io::write(write, &[0]);
    }

    #[test]
    fn signals_become_requests_once_each_and_others_are_dropped() {
        let (signals, pending, write) = signals();
        assert!(signals.take().is_empty());
        for signal in [
            libc::SIGUSR1,
            libc::SIGTERM,
            libc::SIGWINCH,
            libc::SIGHUP,
            libc::SIGINT,
        ] {
            deliver(&pending, &write, signal);
        }
        assert_eq!(signals.take(), [Request::Report, Request::Quit]);
        assert!(signals.take().is_empty());
        deliver(&pending, &write, libc::SIGWINCH);
        assert!(signals.take().is_empty());
    }

    /// Finding 8 of review C: a pipe full of report requests used to drop the byte of the one
    /// quit that came after them.
    #[test]
    fn a_quit_after_a_pipe_full_of_reports_is_taken() {
        let (signals, pending, write) = signals();
        let mut written = 0;
        while rustix::io::write(&write, &[libc::SIGUSR1 as u8]).is_ok() {
            pending.raise(Request::Report);
            written += 1;
        }
        assert!(written > 0);
        // The pipe is full: this byte is lost, the flag is not.
        deliver(&pending, &write, libc::SIGTERM);
        assert_eq!(signals.take(), [Request::Report, Request::Quit]);
        assert!(signals.take().is_empty());
        // The pipe is empty again, so the next request wakes the loop.
        deliver(&pending, &write, libc::SIGTERM);
        let mut fds = [rustix::event::PollFd::new(
            &signals,
            rustix::event::PollFlags::IN,
        )];
        let zero = rustix::time::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        assert_eq!(rustix::event::poll(&mut fds, Some(&zero)).unwrap(), 1);
        assert_eq!(signals.take(), [Request::Quit]);
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn sent_requests_are_taken_once_each_and_each_wakes() {
        let (sender, signals) = Signals::channel();
        sender.send(Request::Quit);
        let woken = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&woken);
        signals.set_wake(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        assert_eq!(woken.load(Ordering::SeqCst), 1, "one was already waiting");
        sender.send(Request::Report);
        assert_eq!(woken.load(Ordering::SeqCst), 2);
        assert_eq!(signals.take(), [Request::Report, Request::Quit]);
        assert!(signals.take().is_empty());
        // Many reports never push a quit out.
        for _ in 0..1000 {
            sender.send(Request::Report);
        }
        sender.send(Request::Quit);
        assert_eq!(signals.take(), [Request::Report, Request::Quit]);
        assert!(Signals::none().take().is_empty());
    }
}
