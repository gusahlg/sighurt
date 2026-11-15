//! What the process is asked from outside while a frontend runs: to quit, saving the session,
//! or to report its diagnostics.
//!
//! On Unix these are signals. `sig`'s handlers (its `main`) write each signal's number to a
//! non-blocking pipe; [`Signals`] holds the pipe's read end, which the owner loop waits on
//! (`AsFd`) and reads with [`Signals::take`]. SIGUSR1 asks for the report; SIGTERM, SIGINT and
//! SIGHUP ask to quit. Other systems have no such source yet (`Signals::none`): nothing asks for
//! a report, and the system's default action ends the process (Ctrl+C in a Windows console)
//! without saving the session (docs/rewrite/PORTING.md).
//!
//! Owns: turning what arrives into [`Request`]s. Must not own: installing the handlers
//! (process-wide setup, the executable's) or what a request does (the frontend's).

#[cfg(unix)]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

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
}

impl Signals {
    /// The read end of the pipe `sig`'s signal handlers write signal numbers to, one byte each.
    /// It must be non-blocking.
    #[cfg(unix)]
    pub fn from_pipe(pipe: OwnedFd) -> Self {
        Self { pipe }
    }

    /// No source: nothing ever arrives, and the system's default actions apply.
    #[cfg(not(unix))]
    pub fn none() -> Self {
        Self {}
    }

    /// Everything that arrived since the last call, in order. Bytes that name no request (a
    /// signal `sig` does not handle this way) are dropped.
    pub fn take(&self) -> Vec<Request> {
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut requests = Vec::new();
        #[cfg(unix)]
        {
            let mut bytes = [0u8; 16];
            while let Ok(n @ 1..) = rustix::io::read(&self.pipe, &mut bytes) {
                requests.extend(bytes[..n].iter().filter_map(|&byte| request(byte)));
            }
        }
        requests
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
