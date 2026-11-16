//! What the process is asked from outside while a frontend runs: to quit, saving the session,
//! or to report its diagnostics.
//!
//! On Unix these are signals. `sig`'s handlers (its `main`) write each signal's number to a
//! non-blocking pipe; [`Signals`] holds the pipe's read end, which the owner loop waits on
//! (`AsFd`) and reads with [`Signals::take`]. SIGUSR1 asks for the report; SIGTERM, SIGINT and
//! SIGHUP ask to quit.
//!
//! Elsewhere (Windows) they are a queue: `sig`'s console control handler (its `main`) sends a
//! quit for Ctrl+C, Ctrl+Break and the console closing through a [`Sender`]
//! ([`Signals::channel`]), which runs the wake the frontend set with [`Signals::set_wake`]; the
//! owner loop then reads them with [`Signals::take`]. Nothing asks for a report there.
//! [`Signals::none`] is a queue nothing sends to.
//!
//! Owns: turning what arrives into [`Request`]s. Must not own: installing the handlers
//! (process-wide setup, the executable's) or what a request does (the frontend's).

#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(not(unix))]
use std::sync::{Arc, Mutex, PoisonError};

/// What the process was asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// Quit cleanly: SIGTERM, SIGINT or SIGHUP.
    Quit,
    /// Report the diagnostics: SIGUSR1.
    Report,
}

/// Where the requests arrive.
#[derive(Debug)]
pub struct Signals {
    #[cfg(unix)]
    pipe: OwnedFd,
    #[cfg(not(unix))]
    queue: Arc<Queue>,
}

/// The requests sent and not yet taken, and the wake that tells the owner loop.
#[cfg(not(unix))]
#[derive(Default)]
struct Queue {
    requests: Mutex<Vec<Request>>,
    wake: Mutex<Option<Box<dyn Fn() + Send>>>,
}

#[cfg(not(unix))]
impl std::fmt::Debug for Queue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Queue").finish_non_exhaustive()
    }
}

/// What sends requests to a [`Signals`]: the console control handler `sig` installs. At most
/// [`MAX_QUEUED`] wait untaken; more are dropped, and the owner already has one to act on.
#[cfg(not(unix))]
#[derive(Clone, Debug)]
pub struct Sender {
    queue: Arc<Queue>,
}

/// The most requests a [`Signals`] keeps untaken.
#[cfg(not(unix))]
pub const MAX_QUEUED: usize = 16;

#[cfg(not(unix))]
impl Sender {
    /// Queues `request` and wakes the owner loop, if it set a wake.
    pub fn send(&self, request: Request) {
        {
            let mut requests = self
                .queue
                .requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if requests.len() >= MAX_QUEUED {
                return;
            }
            requests.push(request);
        }
        let wake = self
            .queue
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(wake) = wake.as_ref() {
            wake();
        }
    }
}

impl Signals {
    /// The read end of the pipe `sig`'s signal handlers write signal numbers to, one byte each.
    /// It must be non-blocking.
    #[cfg(unix)]
    pub fn from_pipe(pipe: OwnedFd) -> Self {
        Self { pipe }
    }

    /// A queue nothing sends to: nothing ever arrives.
    #[cfg(not(unix))]
    pub fn none() -> Self {
        Self {
            queue: Arc::default(),
        }
    }

    /// A queue, and what sends to it.
    #[cfg(not(unix))]
    pub fn channel() -> (Sender, Self) {
        let signals = Self::none();
        let sender = Sender {
            queue: Arc::clone(&signals.queue),
        };
        (sender, signals)
    }

    /// Runs `wake` after each request sent from now on (on the sender's thread), and once now
    /// when requests are already waiting. Replaces the wake set before.
    #[cfg(not(unix))]
    pub fn set_wake(&self, wake: impl Fn() + Send + 'static) {
        let waiting = !self
            .queue
            .requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_empty();
        if waiting {
            wake();
        }
        *self
            .queue
            .wake
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Box::new(wake));
    }

    /// Everything that arrived since the last call, in order. Bytes that name no request (a
    /// signal `sig` does not handle this way) are dropped.
    pub fn take(&self) -> Vec<Request> {
        #[cfg(unix)]
        {
            let mut requests = Vec::new();
            let mut bytes = [0u8; 16];
            while let Ok(n @ 1..) = rustix::io::read(&self.pipe, &mut bytes) {
                requests.extend(bytes[..n].iter().filter_map(|&byte| request(byte)));
            }
            requests
        }
        #[cfg(not(unix))]
        std::mem::take(
            &mut *self
                .queue
                .requests
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
    }
}

/// The request a signal's byte stands for.
#[cfg(unix)]
fn request(byte: u8) -> Option<Request> {
    match i32::from(byte) {
        libc::SIGUSR1 => Some(Request::Report),
        libc::SIGTERM | libc::SIGINT | libc::SIGHUP => Some(Request::Quit),
        _ => None,
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

    #[test]
    fn signal_bytes_become_requests_and_others_are_dropped() {
        let (read, write) = rustix::pipe::pipe().unwrap();
        rustix::fs::fcntl_setfl(&read, rustix::fs::OFlags::NONBLOCK).unwrap();
        let signals = Signals::from_pipe(read);
        assert!(signals.take().is_empty());
        let bytes = [
            libc::SIGUSR1,
            libc::SIGTERM,
            libc::SIGWINCH,
            libc::SIGHUP,
            libc::SIGINT,
        ];
        rustix::io::write(&write, &bytes.map(|signal| signal as u8)).unwrap();
        assert_eq!(
            signals.take(),
            [Request::Report, Request::Quit, Request::Quit, Request::Quit]
        );
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn sent_requests_are_taken_in_order_and_each_wakes() {
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
        assert_eq!(signals.take(), [Request::Quit, Request::Report]);
        assert!(signals.take().is_empty());
        for _ in 0..2 * MAX_QUEUED {
            sender.send(Request::Quit);
        }
        assert_eq!(signals.take().len(), MAX_QUEUED);
        assert!(Signals::none().take().is_empty());
    }
}
