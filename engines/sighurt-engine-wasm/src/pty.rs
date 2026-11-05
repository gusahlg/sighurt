//! The bytes of `sig://terminal`: the master side of the pty the runtime started its program
//! on, between that program and the page's guest.
//!
//! The runtime opened the pty and started the program, and handed the master over once, by
//! value: the guest knows its terminal only by the session's id, never by a descriptor. [`Pty`]
//! owns the descriptor from then on: an I/O thread reads the
//! program's output into a queue of at most [`MAX_OUTPUT`] bytes and stops reading while it is
//! full, so a program that writes faster than the guest reads blocks in its `write` instead of
//! growing `sig`'s memory; what the guest writes waits in a queue of at most [`MAX_INPUT`]
//! bytes and goes to the master without blocking anyone. The guest reads with [`Pty::read`] on
//! its own cadence (every step), so nothing wakes the owner: output waits at most one step,
//! a frame while the page is shown, a second while it is hidden.
//!
//! The thread never blocks: it polls the master together with a wake-up pipe, so it waits for
//! room in a full queue, for input, or for the end, and leaves when told. [`Pty::close`] (and
//! dropping the [`Pty`], with the document's `HostState`: the view closed, the document
//! replaced, the runtime shut down) stops it at once, discards the input not yet written and
//! closes the master, which hangs the program's terminal up; the runtime ends the program.
//! [`Pty::status`] tells the guest which of running, end of file, cancelled and exited (with the
//! status the runtime recorded when it reaped the program) the terminal is.
//!
//! Owns: the master descriptor and its two queues. Must not own: the program (the runtime's
//! `pty`), what the bytes mean (the guest's emulator), or the policy (the core's).

use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;

use rustix::event::{PollFd, PollFlags};
use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::pipe::PipeFlags;
use rustix::termios::Winsize;

/// Most bytes of the program's output held for the guest; reading stops at this.
pub const MAX_OUTPUT: usize = 1 << 20;
/// Most bytes the guest may have waiting to reach the program.
pub const MAX_INPUT: usize = 64 * 1024;
/// Most bytes one `read` from the master takes.
const CHUNK: usize = 64 * 1024;

/// What [`Pty::read`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// This many bytes, at the start of the buffer.
    Bytes(usize),
    /// Nothing yet.
    Nothing,
    /// The program's side of the terminal is closed and everything it wrote has been read.
    Ended,
}

/// Where a terminal is, for the guest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// The program's side is open, or it closed with output still to read.
    Running,
    /// The program's side closed and everything was read; the program is not reaped yet.
    EndOfFile,
    /// The page closed it.
    Cancelled,
    /// The program's side closed, everything was read, and the program ended with this exit
    /// code (128 plus the signal for a killed one).
    Exited(i32),
}

/// The master of one page's pty and the thread that moves its bytes.
pub struct Pty {
    /// The master, shared with the thread only; `None` once closed.
    master: Option<Arc<OwnedFd>>,
    shared: Arc<Mutex<Queues>>,
    /// Written to whenever the thread has something new to do (and to stop it).
    wake: OwnedFd,
    thread: Option<JoinHandle<()>>,
    /// The runtime records the program's exit here when it reaps it.
    exit: Arc<OnceLock<i32>>,
}

#[derive(Default)]
struct Queues {
    output: VecDeque<u8>,
    input: VecDeque<u8>,
    /// The master read end-of-file (or failed).
    ended: bool,
    stop: bool,
}

fn lock(queues: &Mutex<Queues>) -> MutexGuard<'_, Queues> {
    queues.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Pty {
    /// Takes over `master`: makes it non-blocking and starts the thread that moves its bytes.
    /// `exit` is where the runtime records how the program ended.
    pub fn new(master: OwnedFd, exit: Arc<OnceLock<i32>>) -> io::Result<Self> {
        let flags = rustix::fs::fcntl_getfl(&master)?;
        rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK)?;
        let master = Arc::new(master);
        let (woken, wake) = rustix::pipe::pipe_with(PipeFlags::CLOEXEC | PipeFlags::NONBLOCK)?;
        let shared = Arc::new(Mutex::new(Queues::default()));
        let (fd, queues) = (master.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("sighurt-wasm-pty".into())
            .spawn(move || pump(&fd, &queues, &woken))?;
        Ok(Self {
            master: Some(master),
            shared,
            wake,
            thread: Some(thread),
            exit,
        })
    }

    /// Where the terminal is.
    pub fn status(&self) -> Status {
        let queues = lock(&self.shared);
        if self.master.is_none() {
            Status::Cancelled
        } else if !queues.ended || !queues.output.is_empty() {
            Status::Running
        } else {
            self.exit
                .get()
                .map_or(Status::EndOfFile, |&code| Status::Exited(code))
        }
    }

    /// Stops the thread, discards what was not written yet and closes the master. Reads find
    /// the end, writes are refused, and the status is [`Status::Cancelled`] from then on.
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

    /// Moves the program's output into `out`, as much as fits.
    pub fn read(&self, out: &mut [u8]) -> Read {
        let mut queues = lock(&self.shared);
        if queues.output.is_empty() {
            return if queues.ended {
                Read::Ended
            } else {
                Read::Nothing
            };
        }
        let was_full = queues.output.len() >= MAX_OUTPUT;
        let n = out.len().min(queues.output.len());
        for (slot, byte) in out.iter_mut().zip(queues.output.drain(..n)) {
            *slot = byte;
        }
        drop(queues);
        if was_full {
            self.wake();
        }
        Read::Bytes(n)
    }

    /// Queues `bytes` for the program, as many as there is room for ([`MAX_INPUT`] waiting);
    /// returns how many. Nothing is accepted once the terminal has ended.
    pub fn write(&self, bytes: &[u8]) -> usize {
        let mut queues = lock(&self.shared);
        if queues.ended {
            return 0;
        }
        let n = bytes.len().min(MAX_INPUT - queues.input.len());
        queues.input.extend(&bytes[..n]);
        drop(queues);
        if n > 0 {
            self.wake();
        }
        n
    }

    /// Sets the terminal's size, which signals the program's foreground group (SIGWINCH).
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let master = self.master.as_deref().ok_or(io::ErrorKind::NotConnected)?;
        rustix::termios::tcsetwinsize(
            master,
            Winsize {
                ws_row: rows,
                ws_col: cols,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )?;
        Ok(())
    }

    fn wake(&self) {
        // A full pipe already holds a wake-up.
        let _ = rustix::io::write(&self.wake, &[1]);
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
        (Pty::new(master, Arc::default()).unwrap(), slave)
    }

    /// Reads from `pty` until `want` bytes came or `Ended`.
    fn read_from(pty: &Pty, want: usize) -> (Vec<u8>, bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        while got.len() < want {
            assert!(Instant::now() < deadline, "{} of {want}", got.len());
            match pty.read(&mut buf) {
                Read::Bytes(n) => got.extend_from_slice(&buf[..n]),
                Read::Nothing => std::thread::sleep(Duration::from_millis(1)),
                Read::Ended => return (got, true),
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
            sent += pty.write(&input[sent..]);
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
    fn the_end_comes_after_the_last_output_and_then_the_exit() {
        let (pty, slave) = pty();
        rustix::io::write(&slave, b"last words").unwrap();
        drop(slave);
        // Closed with output left: still running for the guest.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(pty.status(), Status::Running);
        let (got, ended) = read_from(&pty, usize::MAX);
        assert!(ended);
        assert_eq!(got, b"last words");
        assert_eq!(pty.read(&mut [0; 8]), Read::Ended);
        assert_eq!(pty.write(b"x"), 0);
        assert_eq!(pty.status(), Status::EndOfFile, "not reaped yet");
        pty.exit.set(3).unwrap();
        assert_eq!(pty.status(), Status::Exited(3));
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
        assert_eq!(pty.write(&[b'x'; 100]), 100);
        let started = Instant::now();
        pty.close();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(pty.status(), Status::Cancelled);
        assert!(lock(&pty.shared).input.is_empty());
        assert_eq!(pty.read(&mut [0; 8]), Read::Ended);
        assert_eq!(pty.write(b"x"), 0);
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
        pty.write(b"queued");
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
}
