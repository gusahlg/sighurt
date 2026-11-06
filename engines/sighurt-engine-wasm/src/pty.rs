//! The bytes of `sig://terminal`: the take-once hand-over of a terminal's master from the
//! runtime, and the master side between the program and the page's guest.
//!
//! The runtime opens the terminal and starts its program; before it answers the page's
//! `PtyOpen` with the session ([`PtySession`]) it hands the master over here ([`hand_over`]).
//! The engine takes it, once, when the answer reaches the document ([`take`]); a master nobody
//! takes is dropped when the runtime ends the session ([`forget`]). No descriptor is in any
//! service type, and a guest only ever sees a handle of its document.
//!
//! [`Pty`] owns a taken master: an I/O thread reads the program's output into a queue of at
//! most [`MAX_OUTPUT`] bytes and stops reading while it is full, so a program that writes faster
//! than the guest reads blocks in its `write` instead of growing `sig`'s memory; what the guest
//! writes waits in a queue of at most [`MAX_INPUT`] bytes and goes to the master without
//! blocking anyone. The thread never blocks on the master: it polls it together with a wake-up
//! pipe, so it waits for room in a full queue, for input or for the end, and leaves when told.
//! The guest reads on its own cadence (every step), so nothing wakes the owner: output waits at
//! most one step, a frame while the page is shown, a second while it is hidden.
//!
//! [`Pty::close`], and dropping the [`Pty`] with the document's `HostState` (the view closed,
//! the document replaced, the runtime shut down), stops the thread at once, discards the input
//! not yet written and closes the master, which hangs the program's terminal up; the runtime
//! ends the program.
//!
//! Owns: moving bytes, and the masters on their way to a document. Must not own: starting or
//! ending the program (the runtime does), or deciding who may have a terminal (the core).

use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use rustix::event::{PollFd, PollFlags};
use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::pipe::PipeFlags;
use rustix::termios::Winsize;
use sighurt_api::ids::PtySession;

/// Most bytes of the program's output held for the guest; reading stops at this.
pub const MAX_OUTPUT: usize = 1 << 20;
/// Most bytes the guest may have waiting to reach the program.
pub const MAX_INPUT: usize = 64 * 1024;
/// Most bytes one `read` from the master takes.
const CHUNK: usize = 64 * 1024;

/// Masters handed over and not taken yet. Few: one per terminal opening, at most eight.
static HANDED: Mutex<Vec<(PtySession, OwnedFd)>> = Mutex::new(Vec::new());

/// The runtime hands `master`, the terminal of `session`, to the engine, before it answers.
pub fn hand_over(session: PtySession, master: OwnedFd) {
    let mut handed = HANDED.lock().unwrap_or_else(PoisonError::into_inner);
    handed.push((session, master));
}

/// The master of `session`, once: a second call, or one for a session never handed over or
/// forgotten, is `None`.
pub fn take(session: PtySession) -> Option<OwnedFd> {
    let mut handed = HANDED.lock().unwrap_or_else(PoisonError::into_inner);
    let i = handed.iter().position(|(s, _)| *s == session)?;
    Some(handed.swap_remove(i).1)
}

/// The runtime ended `session`: a master still waiting is closed.
pub fn forget(session: PtySession) {
    drop(take(session));
}

/// Why a terminal does no more: the program's side closed and everything it wrote was read, or
/// the page closed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ended;

/// One terminal of a document: the master side it took, and the thread that moves its bytes.
pub struct Pty {
    /// The master, shared with the thread only; `None` once closed.
    master: Option<Arc<OwnedFd>>,
    shared: Arc<Mutex<Queues>>,
    /// Written to whenever the thread has something new to do (and to stop it).
    wake: Option<OwnedFd>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Default)]
struct Queues {
    output: VecDeque<u8>,
    input: VecDeque<u8>,
    /// The master read end-of-file (or failed), or the terminal was closed.
    ended: bool,
    stop: bool,
}

fn lock(queues: &Mutex<Queues>) -> MutexGuard<'_, Queues> {
    queues.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pty {
    /// Starts moving bytes for `master`: makes it non-blocking and starts the thread. A master
    /// that can't be set up (no thread, no pipe) is closed, and the terminal has ended.
    pub fn attach(master: OwnedFd) -> Self {
        let shared = Arc::new(Mutex::new(Queues::default()));
        match Self::start(master, &shared) {
            Ok((master, wake, thread)) => Self {
                master: Some(master),
                shared,
                wake: Some(wake),
                thread: Some(thread),
            },
            Err(err) => {
                tracing::warn!("terminal: could not attach the master: {err}");
                lock(&shared).ended = true;
                Self {
                    master: None,
                    shared,
                    wake: None,
                    thread: None,
                }
            }
        }
    }

    fn start(
        master: OwnedFd,
        shared: &Arc<Mutex<Queues>>,
    ) -> io::Result<(Arc<OwnedFd>, OwnedFd, JoinHandle<()>)> {
        let flags = rustix::fs::fcntl_getfl(&master)?;
        rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;
        let master = Arc::new(master);
        let (woken, wake) = rustix::pipe::pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
        let (fd, queues) = (master.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("sighurt-wasm-pty".into())
            .spawn(move || pump(&fd, &queues, &woken))?;
        Ok((master, wake, thread))
    }

    /// Output waiting for the guest, at most `buf.len()` bytes: how many were copied, `0` when
    /// there is none now. Never blocks.
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, Ended> {
        let mut queues = lock(&self.shared);
        if queues.output.is_empty() {
            return if queues.ended { Err(Ended) } else { Ok(0) };
        }
        let was_full = queues.output.len() >= MAX_OUTPUT;
        let n = buf.len().min(queues.output.len());
        for (slot, byte) in buf.iter_mut().zip(queues.output.drain(..n)) {
            *slot = byte;
        }
        drop(queues);
        if was_full {
            self.wake();
        }
        Ok(n)
    }

    /// Queues `bytes` for the program's input: how many were taken, as many as there is room
    /// for ([`MAX_INPUT`] waiting). Never blocks.
    pub fn write(&self, bytes: &[u8]) -> Result<usize, Ended> {
        let mut queues = lock(&self.shared);
        if queues.ended {
            return Err(Ended);
        }
        let n = bytes.len().min(MAX_INPUT - queues.input.len());
        queues.input.extend(&bytes[..n]);
        drop(queues);
        if n > 0 {
            self.wake();
        }
        Ok(n)
    }

    /// Tells the program the terminal is `cols` by `rows` cells (TIOCSWINSZ on the master,
    /// which signals its foreground group).
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), Ended> {
        let master = self.master.as_deref().ok_or(Ended)?;
        let size = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        rustix::termios::tcsetwinsize(master, size).map_err(|_| Ended)
    }

    /// Stops the thread, discards what was not written or read yet and closes the master. Reads
    /// and writes find the end from then on.
    pub fn close(&mut self) {
        {
            let mut queues = lock(&self.shared);
            queues.stop = true;
            queues.ended = true;
            queues.input.clear();
            queues.output.clear();
        }
        self.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.master = None;
    }

    fn wake(&self) {
        // A full pipe already holds a wake-up.
        if let Some(wake) = &self.wake {
            let _ = rustix::io::write(wake, &[1]);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        self.close();
    }
}

/// The I/O thread: waits for the master or a wake-up, reads while the output queue has room,
/// writes while the input queue has bytes, until it is stopped.
fn pump(master: &OwnedFd, queues: &Mutex<Queues>, woken: &OwnedFd) {
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let (read, write) = {
            let queues = lock(queues);
            if queues.stop {
                return;
            }
            let open = !queues.ended;
            (
                open && queues.output.len() < MAX_OUTPUT,
                open && !queues.input.is_empty(),
            )
        };
        let mut wanted = PollFlags::empty();
        wanted.set(PollFlags::IN, read);
        wanted.set(PollFlags::OUT, write);
        // A master with nothing wanted is left out: its hang-up would be reported anyway, and
        // the loop would spin.
        let mut fds = vec![PollFd::new(woken, PollFlags::IN)];
        if !wanted.is_empty() {
            fds.push(PollFd::new(master, wanted));
        }
        match rustix::event::poll(&mut fds, None) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(_) => {
                lock(queues).ended = true;
                return;
            }
        }
        let ready = fds.get(1).map_or(PollFlags::empty(), PollFd::revents);
        let woke = fds[0].revents().contains(PollFlags::IN);
        drop(fds);
        if woke {
            let mut sink = [0u8; 64];
            while rustix::io::read(woken, &mut sink).is_ok_and(|n| n > 0) {}
        }
        let broken = PollFlags::HUP | PollFlags::ERR;
        if read && ready.intersects(PollFlags::IN | broken) {
            let room = MAX_OUTPUT - lock(queues).output.len();
            let n = room.min(CHUNK);
            match rustix::io::read(master, &mut chunk[..n]) {
                Ok(0) => lock(queues).ended = true,
                Ok(n) => lock(queues).output.extend(&chunk[..n]),
                Err(Errno::AGAIN | Errno::INTR) => {}
                // EIO: every descriptor of the slave is closed.
                Err(_) => lock(queues).ended = true,
            }
        }
        if write && ready.intersects(PollFlags::OUT | broken) {
            let mut queues = lock(queues);
            let (front, _) = queues.input.as_slices();
            match rustix::io::write(master, front) {
                Ok(n) => {
                    queues.input.drain(..n);
                }
                Err(Errno::AGAIN | Errno::INTR) => {}
                Err(_) => {
                    queues.input.clear();
                    queues.ended = true;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use rustix::fs::Mode;
    use rustix::pty::OpenptFlags;
    use rustix::termios::OptionalActions;

    use super::*;

    /// A pty in raw mode: the master for a [`Pty`], the slave as the program's side.
    fn pty() -> (Pty, OwnedFd) {
        let master = rustix::pty::openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        rustix::pty::grantpt(&master).unwrap();
        rustix::pty::unlockpt(&master).unwrap();
        let name = rustix::pty::ptsname(&master, Vec::new()).unwrap();
        let slave = rustix::fs::open(
            name.as_c_str(),
            OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let mut termios = rustix::termios::tcgetattr(&slave).unwrap();
        termios.make_raw();
        rustix::termios::tcsetattr(&slave, OptionalActions::Now, &termios).unwrap();
        (Pty::attach(master), slave)
    }

    /// Reads from `pty` until `want` bytes came or `Ended`.
    fn read_from(pty: &Pty, want: usize) -> (Vec<u8>, bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while got.len() < want {
            assert!(Instant::now() < deadline, "{} of {want}", got.len());
            match pty.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(1)),
                Ok(n) => got.extend_from_slice(&buf[..n]),
                Err(Ended) => return (got, true),
            }
        }
        (got, false)
    }

    fn read_slave(slave: &OwnedFd, want: usize) -> Vec<u8> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while got.len() < want {
            assert!(Instant::now() < deadline, "{} of {want}", got.len());
            let mut fds = [PollFd::new(slave, PollFlags::IN)];
            let timeout = rustix::event::Timespec::try_from(Duration::from_millis(50)).unwrap();
            if rustix::event::poll(&mut fds, Some(&timeout)).unwrap() > 0 {
                let n = rustix::io::read(slave, &mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
        }
        got
    }

    #[test]
    fn bytes_go_both_ways_in_order() {
        let (pty, slave) = pty();
        let output: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let writer = {
            let slave = slave.try_clone().unwrap();
            let output = output.clone();
            std::thread::spawn(move || {
                let mut left = &output[..];
                while !left.is_empty() {
                    let n = rustix::io::write(&slave, left).unwrap();
                    left = &left[n..];
                }
            })
        };
        assert_eq!(read_from(&pty, output.len()).0, output);
        writer.join().unwrap();

        let input: Vec<u8> = (0..MAX_INPUT as u32 * 3).map(|i| (i % 253) as u8).collect();
        let mut sent = 0;
        let mut received = Vec::new();
        let mut buf = [0u8; 4096];
        while received.len() < input.len() {
            sent += pty.write(&input[sent..]).unwrap();
            assert!(lock(&pty.shared).input.len() <= MAX_INPUT);
            let mut fds = [PollFd::new(&slave, PollFlags::IN)];
            let timeout = rustix::event::Timespec::try_from(Duration::from_millis(50)).unwrap();
            if rustix::event::poll(&mut fds, Some(&timeout)).unwrap() > 0 {
                let n = rustix::io::read(&slave, &mut buf).unwrap();
                received.extend_from_slice(&buf[..n]);
            }
        }
        assert_eq!(received, input);
    }

    #[test]
    fn a_flood_is_held_to_the_output_queue_and_blocks_the_program() {
        let (pty, slave) = pty();
        let flags = rustix::fs::fcntl_getfl(&slave).unwrap();
        rustix::fs::fcntl_setfl(&slave, flags | OFlags::NONBLOCK).unwrap();
        // The program writes until the terminal stops taking bytes.
        let block = vec![b'y'; 4096];
        let mut written = 0;
        let mut stalled_since = None;
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            assert!(Instant::now() < deadline, "never blocked after {written}");
            match rustix::io::write(&slave, &block) {
                Ok(n) => {
                    written += n;
                    stalled_since = None;
                }
                Err(Errno::AGAIN) => {
                    let since = *stalled_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > Duration::from_millis(300) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(err) => panic!("{err}"),
            }
        }
        let held = lock(&pty.shared).output.len();
        assert_eq!(held, MAX_OUTPUT, "the queue fills and stops there");
        // The kernel's own buffers hold a little more than the queue: far less than a MiB.
        assert!((MAX_OUTPUT..2 * MAX_OUTPUT).contains(&written), "{written}");
        // Reading makes room, and the program can go on.
        let (got, _) = read_from(&pty, MAX_OUTPUT);
        assert_eq!(got.len(), MAX_OUTPUT);
        let deadline = Instant::now() + Duration::from_secs(5);
        while rustix::io::write(&slave, &block).is_err() {
            assert!(Instant::now() < deadline, "the program stayed blocked");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn the_end_comes_after_the_last_output() {
        let (pty, slave) = pty();
        rustix::io::write(&slave, b"last words").unwrap();
        drop(slave);
        // Closed with output left: the guest reads it first.
        std::thread::sleep(Duration::from_millis(50));
        assert!(pty.write(b"x").is_err(), "nobody reads input any more");
        let (got, ended) = read_from(&pty, usize::MAX);
        assert!(ended);
        assert_eq!(got, b"last words");
        assert_eq!(pty.read(&mut [0; 8]), Err(Ended));
    }

    #[test]
    fn closing_with_a_full_queue_stops_at_once_and_discards_the_input() {
        let (mut pty, slave) = pty();
        let flags = rustix::fs::fcntl_getfl(&slave).unwrap();
        rustix::fs::fcntl_setfl(&slave, flags | OFlags::NONBLOCK).unwrap();
        let block = vec![b'y'; 4096];
        let deadline = Instant::now() + Duration::from_secs(20);
        while lock(&pty.shared).output.len() < MAX_OUTPUT {
            assert!(Instant::now() < deadline);
            let _ = rustix::io::write(&slave, &block);
        }
        // Input the program never reads: it stays queued.
        assert_eq!(pty.write(&[b'x'; 100]), Ok(100));
        let started = Instant::now();
        pty.close();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert!(lock(&pty.shared).input.is_empty());
        assert_eq!(pty.read(&mut [0; 8]), Err(Ended));
        assert_eq!(pty.write(b"x"), Err(Ended));
        assert!(pty.resize(80, 24).is_err());
        pty.close();
        drop(pty);
        let mut fds = [PollFd::new(&slave, PollFlags::IN)];
        rustix::event::poll(&mut fds, None).unwrap();
        assert!(
            fds[0].revents().contains(PollFlags::HUP),
            "the master is closed"
        );
    }

    #[test]
    fn a_resize_reaches_the_slave() {
        let (pty, slave) = pty();
        pty.resize(132, 43).unwrap();
        let size = rustix::termios::tcgetwinsize(&slave).unwrap();
        assert_eq!((size.ws_col, size.ws_row), (132, 43));
    }

    #[test]
    fn dropping_it_stops_the_thread_and_hangs_the_terminal_up() {
        let (pty, slave) = pty();
        let master = Arc::downgrade(pty.master.as_ref().unwrap());
        pty.write(b"queued").unwrap();
        assert_eq!(read_slave(&slave, 6), b"queued");
        let started = Instant::now();
        drop(pty);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(master.upgrade().is_none(), "the master is closed");
        let mut fds = [PollFd::new(&slave, PollFlags::IN)];
        rustix::event::poll(&mut fds, None).unwrap();
        assert!(
            fds[0].revents().contains(PollFlags::HUP),
            "{:?}",
            fds[0].revents()
        );
    }

    #[test]
    fn a_master_is_taken_once() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        let session = PtySession::next();
        hand_over(session, a.into());
        assert!(take(PtySession::next()).is_none());
        assert!(take(session).is_some());
        assert!(take(session).is_none());
        let (c, _d) = std::os::unix::net::UnixStream::pair().unwrap();
        let other = PtySession::next();
        hand_over(other, c.into());
        forget(other);
        assert!(take(other).is_none());
    }

    #[test]
    fn a_closed_peer_ends_the_terminal() {
        // A socket stands in for a master whose program side is gone.
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(b);
        let pty = Pty::attach(a.into());
        let (_, ended) = read_from(&pty, 1);
        assert!(ended);
        assert!(pty.resize(80, 24).is_err(), "no terminal to resize");
    }
}
