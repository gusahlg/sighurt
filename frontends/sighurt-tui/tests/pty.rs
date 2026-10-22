//! Pty tests: golden bytes, the capability probe, restoration, and a stalled reader.
//!
//! Workers run in a child so a panic hook or a signal handler never lands in
//! the test process. In-process sessions share one mutex; the tests below take
//! it only for their own `Session`.

use std::cell::{Cell, RefCell};
use std::io::Read;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use calloop::generic::Generic;
use calloop::{EventLoop, Interest, Mode, PostAction};
use rustix::event::{PollFd, PollFlags};
use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitOptions};
use rustix::termios::{tcgetattr, tcsetattr, LocalModes, OptionalActions, Termios};

use sighurt_api::input::{ButtonState, Key};
use sighurt_tui::grid::{Attrs, Color, Grid};
use sighurt_tui::term::{
    open_pty, Capabilities, TermEvent, Terminal, ENTER, ESC_TIMEOUT, LEAVE, PROBE_DEADLINE,
};

const IMMEDIATE_REPLIES: &[u8] = b"\x1b[?31u\x1b_Gi=1;OK\x1b\\\x1bP1+r524742=726762\x1b\\\x1b]10;rgb:1111/2222/3333\x07\x1b]11;rgb:aaaa/bbbb/cccc\x1b\\\x1b[6;20;10t\x1b[4;1080;1920t\x1b[?62;4c";

/// Every pty test has to finish inside this, or the watchdog kills its child.
const TEST_LIMIT: Duration = Duration::from_secs(8);

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    static PIDS: RefCell<Option<Arc<Mutex<Vec<i32>>>>> = const { RefCell::new(None) };
}

/// Arms [`TEST_LIMIT`]. On expiry a watchdog `SIGKILL`s every child this
/// thread has spawned, and the next poll in this thread fails the test.
struct DeadlineGuard {
    stop: Option<mpsc::Sender<()>>,
    join: Option<thread::JoinHandle<()>>,
}

fn arm_deadline() -> DeadlineGuard {
    DEADLINE.with(|slot| slot.set(Some(Instant::now() + TEST_LIMIT)));
    let pids = Arc::new(Mutex::new(Vec::new()));
    PIDS.with(|slot| *slot.borrow_mut() = Some(Arc::clone(&pids)));
    let (tx, rx) = mpsc::channel();
    let join = thread::spawn(move || {
        if rx.recv_timeout(TEST_LIMIT).is_err() {
            let guard = match pids.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            for pid in guard.iter().copied() {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
    });
    DeadlineGuard {
        stop: Some(tx),
        join: Some(join),
    }
}

impl Drop for DeadlineGuard {
    fn drop(&mut self) {
        DEADLINE.with(|slot| slot.set(None));
        PIDS.with(|slot| *slot.borrow_mut() = None);
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn past_deadline() -> bool {
    DEADLINE.with(|slot| {
        slot.get()
            .is_some_and(|deadline| Instant::now() >= deadline)
    })
}

fn track_pid(pid: u32) {
    let pids = PIDS.with(|slot| slot.borrow().clone());
    let Some(pids) = pids else {
        return;
    };
    let mut guard = match pids.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.push(i32::try_from(pid).unwrap_or(i32::MAX));
}

fn fail_if_late(what: &str) {
    if past_deadline() {
        panic!("test deadline exceeded while {what}");
    }
}

fn worker_bin() -> std::path::PathBuf {
    if let Some(path) = option_env!("CARGO_BIN_EXE_sighurt_tui_worker") {
        return std::path::PathBuf::from(path);
    }
    let mut path = std::env::current_exe().expect("test executable");
    path.pop();
    if path.file_name().is_some_and(|name| name == "deps") {
        path.pop();
    }
    path.push("sighurt-tui-worker");
    path
}

fn set_nonblock(fd: &impl AsFd) {
    let flags = rustix::fs::fcntl_getfl(fd).expect("getfl");
    rustix::fs::fcntl_setfl(fd, flags | OFlags::NONBLOCK).expect("setfl");
}

fn try_write_all(fd: &OwnedFd, mut bytes: &[u8]) -> bool {
    let start = Instant::now();
    while !bytes.is_empty() {
        if start.elapsed() > Duration::from_secs(2) {
            return false;
        }
        match rustix::io::write(fd, bytes) {
            Ok(0) => return false,
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => thread::sleep(Duration::from_millis(1)),
            Err(_) => return false,
        }
    }
    true
}

fn write_all(fd: &OwnedFd, bytes: &[u8]) {
    fail_if_late("writing to the pty");
    if !try_write_all(fd, bytes) {
        panic!("pty write failed or timed out");
    }
}

fn read_once(fd: &OwnedFd, buf: &mut [u8]) -> usize {
    loop {
        match rustix::io::read(fd, &mut *buf) {
            Ok(n) => return n,
            Err(Errno::INTR) => continue,
            Err(Errno::AGAIN) => return 0,
            Err(err) => panic!("pty read: {err}"),
        }
    }
}

fn poll_in(fd: &OwnedFd, timeout: Duration) {
    if past_deadline() {
        return;
    }
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    let spec = rustix::event::Timespec::try_from(timeout).unwrap_or(rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    });
    loop {
        match rustix::event::poll(&mut fds, Some(&spec)) {
            Ok(_) => return,
            Err(Errno::INTR) => continue,
            Err(err) => panic!("poll: {err}"),
        }
    }
}

/// Bytes read up to and including `needle`. `Err` is what arrived before the
/// timeout. This does not panic: a panic here used to run the session's
/// panic hook on a helper thread and clear raw mode.
fn wait_for(master: &OwnedFd, needle: &[u8], timeout: Duration) -> Result<Vec<u8>, Vec<u8>> {
    let start = Instant::now();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while start.elapsed() < timeout {
        if past_deadline() {
            return Err(got);
        }
        let n = read_once(master, &mut buf);
        if n == 0 {
            let left = timeout.saturating_sub(start.elapsed());
            if left.is_zero() {
                break;
            }
            poll_in(master, left.min(Duration::from_millis(20)));
            continue;
        }
        got.extend_from_slice(&buf[..n]);
        if got.windows(needle.len()).any(|window| window == needle) {
            return Ok(got);
        }
    }
    Err(got)
}

fn read_until(master: &OwnedFd, needle: &[u8], timeout: Duration) -> Vec<u8> {
    match wait_for(master, needle, timeout) {
        Ok(got) => got,
        Err(got) if past_deadline() => {
            panic!("test deadline exceeded waiting for {needle:?}, got {got:?}")
        }
        Err(got) => panic!("timed out waiting for {needle:?}, got {got:?}"),
    }
}

fn reply_after(master: OwnedFd, delay: Duration, bytes: &'static [u8]) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        if wait_for(&master, b"\x1b[14t\x1b[c", Duration::from_secs(2)).is_ok() {
            if !delay.is_zero() {
                thread::sleep(delay);
            }
            // A panic here would run the session hook. Missed writes are not
            // fatal: the probe's own deadline is what the test checks.
            let _ = try_write_all(&master, bytes);
        }
    })
}

/// Bounds a missing answer. Not an ordering device: the helper writes only
/// after it has read the probe's query batch.
const BATCH_BOUND: Duration = Duration::from_secs(4);

struct QueryReply {
    /// When the reply bytes had been accepted by the master.
    wrote: mpsc::Receiver<Instant>,
    thread: thread::JoinHandle<()>,
}

/// Write `bytes` once the probe's query batch (`CSI 14 t`, then DA1) is on
/// `master`. The probe is what unblocks the write.
fn reply_when_queried(master: OwnedFd, bytes: &'static [u8]) -> QueryReply {
    let (tx, rx) = mpsc::channel();
    let thread = thread::spawn(move || {
        // A panic here would run the session hook on this thread.
        if wait_for(&master, b"\x1b[14t\x1b[c", BATCH_BOUND).is_err() {
            return;
        }
        if try_write_all(&master, bytes) {
            let _ = tx.send(Instant::now());
        }
    });
    QueryReply { wrote: rx, thread }
}

impl QueryReply {
    fn join(self) {
        self.thread.join().expect("reply thread");
    }
}

fn drain_for(master: &OwnedFd, timeout: Duration) -> Vec<u8> {
    let start = Instant::now();
    let mut got = Vec::new();
    let mut buf = [0u8; 4096];
    while start.elapsed() < timeout {
        if past_deadline() {
            break;
        }
        let n = read_once(master, &mut buf);
        if n == 0 {
            let left = timeout.saturating_sub(start.elapsed());
            if left.is_zero() {
                break;
            }
            poll_in(master, left.min(Duration::from_millis(20)));
            continue;
        }
        got.extend_from_slice(&buf[..n]);
    }
    got
}

fn contains_in_order(haystack: &[u8], needles: &[&[u8]]) -> bool {
    let mut from = 0;
    for needle in needles {
        match haystack[from..]
            .windows(needle.len())
            .position(|window| window == *needle)
        {
            Some(pos) => from += pos + needle.len(),
            None => return false,
        }
    }
    true
}

fn cooked(slave: &OwnedFd) -> Termios {
    let mut ios = tcgetattr(slave).expect("tcgetattr");
    ios.local_modes
        .insert(LocalModes::ICANON | LocalModes::ECHO);
    tcsetattr(slave, OptionalActions::Now, &ios).expect("tcsetattr");
    tcgetattr(slave).expect("tcgetattr after cooked")
}

struct PtyChild {
    master: OwnedFd,
    slave: OwnedFd,
    child: Option<Child>,
    original: Termios,
    stderr: Arc<Mutex<Vec<u8>>>,
    stderr_thread: Option<thread::JoinHandle<()>>,
}

impl PtyChild {
    fn spawn(worker: &str, stdin_is_slave: bool) -> Self {
        let (master, slave) = open_pty().expect("openpty");
        set_nonblock(&master);
        let original = cooked(&slave);
        let mut command = Command::new(worker_bin());
        command.arg("--worker").arg(worker).stderr(Stdio::piped());
        // A new process group whose parent is this test, in the same session.
        // The runner's group is often orphaned, and the kernel then discards
        // SIGTSTP instead of stopping the process.
        command.process_group(0);
        if stdin_is_slave {
            command
                .stdin(Stdio::from(slave.try_clone().expect("dup slave")))
                .stdout(Stdio::null());
        } else {
            command
                .stdin(Stdio::null())
                .stdout(Stdio::from(slave.try_clone().expect("dup slave")));
        }
        let mut child = command.spawn().expect("spawn worker");
        track_pid(child.id());
        let stderr_buf = Arc::new(Mutex::new(Vec::new()));
        let shared = Arc::clone(&stderr_buf);
        let mut stderr = child.stderr.take().expect("stderr");
        let stderr_thread = thread::spawn(move || {
            let mut buf = [0u8; 1024];
            loop {
                match stderr.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => shared
                        .lock()
                        .expect("stderr lock")
                        .extend_from_slice(&buf[..n]),
                    Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        });
        Self {
            master,
            slave,
            child: Some(child),
            original,
            stderr: stderr_buf,
            stderr_thread: Some(stderr_thread),
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("child still running")
    }

    fn pid(&self) -> Pid {
        Pid::from_child(self.child.as_ref().expect("child"))
    }

    fn wait_timeout(&mut self, timeout: Duration) -> ExitStatus {
        let start = Instant::now();
        let child = self.child_mut();
        loop {
            if past_deadline() || start.elapsed() > timeout {
                let _ = child.kill();
                let status = child.wait().expect("wait after kill");
                if past_deadline() {
                    panic!("test deadline exceeded ({status:?})");
                }
                panic!("worker timed out ({status:?})");
            }
            match child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(err) => panic!("try_wait: {err}"),
            }
        }
    }

    fn stderr_text(&self) -> Vec<u8> {
        self.stderr.lock().expect("stderr lock").clone()
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(handle) = self.stderr_thread.take() {
            let _ = handle.join();
        }
    }
}

fn child_fail(code: i32, msg: &str) -> ! {
    // `eprintln` lands in libtest's capture, which `_exit` discards.
    let line = format!("{msg}\n");
    unsafe {
        libc::write(2, line.as_ptr().cast(), line.len());
        libc::_exit(code);
    }
}

/// Run this test again in a child whose stdout is `slave`. Handlers must not
/// land in the test process. The child `_exit`s: flushing stdout would block
/// once the pty buffer is full.
fn spawn_self(test: &str, env_key: &str, slave: &OwnedFd) -> Child {
    let child = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", test, "--nocapture"])
        .env(env_key, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(slave.try_clone().expect("dup slave")))
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn reexec");
    track_pid(child.id());
    child
}

fn read_stderr(child: &mut Child) -> String {
    let Some(stderr) = child.stderr.take() else {
        return String::new();
    };
    let mut err = String::new();
    let mut file = stderr;
    let _ = file.read_to_string(&mut err);
    err
}

fn wait_stopped(pty: &mut PtyChild) {
    let start = Instant::now();
    let pid = pty.pid();
    loop {
        if past_deadline() || start.elapsed() > Duration::from_secs(2) {
            let _ = pty.child_mut().kill();
            if past_deadline() {
                panic!("test deadline exceeded before the worker stopped");
            }
            panic!("worker did not stop");
        }
        match rustix::process::waitpid(Some(pid), WaitOptions::NOHANG | WaitOptions::UNTRACED) {
            Ok(Some((_, status))) if status.stopped() => {
                assert_eq!(status.stopping_signal(), Some(Signal::TSTP.as_raw()));
                return;
            }
            Ok(Some((_, status))) => panic!("worker changed state before stop: {status:?}"),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(err) => panic!("waitpid: {err}"),
        }
    }
}

#[test]
fn pty_golden_bytes_and_double_restore() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let flags_before = rustix::fs::fcntl_getfl(&slave).expect("getfl");
    let original = cooked(&slave);
    let mut session = Terminal::enter(slave).expect("enter");
    let entered = read_until(&master, ENTER, Duration::from_secs(1));
    assert!(
        entered.windows(ENTER.len()).any(|window| window == ENTER),
        "{entered:?}"
    );

    let fd = session.try_clone_fd().expect("dup");
    let raw = tcgetattr(&fd).expect("raw termios");
    assert!(!raw.local_modes.contains(LocalModes::ICANON));
    assert!(!raw.local_modes.contains(LocalModes::ECHO));

    let mut grid = Grid::new(8, 3);
    grid.put_cluster(
        3,
        1,
        "A",
        Color::Rgb(1, 2, 3),
        Color::Default,
        Attrs {
            bold: true,
            ..Attrs::default()
        },
    );
    session.write_frame(&grid.diff()).expect("cell");
    let cell = read_until(&master, b"\x1b[?2026l", Duration::from_secs(1));
    assert_eq!(cell, b"\x1b[?2026h\x1b[2;4H\x1b[1;38;2;1;2;3mA\x1b[?2026l");

    let mut wide = Grid::new(4, 1);
    wide.put_cluster(0, 0, "あ", Color::Default, Color::Default, Attrs::default());
    session.write_frame(&wide.diff()).expect("wide");
    let wide_bytes = read_until(&master, b"\x1b[?2026l", Duration::from_secs(1));
    assert_eq!(wide_bytes, b"\x1b[?2026h\x1b[H\xe3\x81\x82\x1b[?2026l");

    session.restore().expect("restore");
    session.restore().expect("second restore");
    let left = drain_for(&master, Duration::from_millis(200));
    assert_eq!(left, LEAVE);
    let fd = session.try_clone_fd().expect("dup after restore");
    assert!(sighurt_tui::term::termios_matches(
        &tcgetattr(&fd).expect("restored"),
        &original
    ));
    assert_eq!(rustix::fs::fcntl_getfl(&fd).expect("flags"), flags_before);
}

#[test]
fn probe_accepts_an_immediate_batch() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let master_for_reply = master.try_clone().expect("dup master");
    let reply = reply_when_queried(master_for_reply, IMMEDIATE_REPLIES);
    let mut session = Terminal::enter(slave).expect("enter");
    // [`PROBE_DEADLINE`] is 150 ms. The helper can easily miss that while it
    // waits to be scheduled, so this bound only fails a terminal that never
    // answers. Returning before it means the batch ended the probe.
    let start = Instant::now();
    let caps = session.probe(BATCH_BOUND).expect("probe").clone();
    let returned = Instant::now();
    let wrote = reply
        .wrote
        .recv_timeout(BATCH_BOUND)
        .expect("the batch was written");
    let elapsed = returned.saturating_duration_since(start);
    let since_write = returned.saturating_duration_since(wrote);
    // `wrote` is after the kernel accepted the batch. Lingering until
    // `BATCH_BOUND` would mean the probe ignored it. Waking a few hundred
    // milliseconds late under load still passes.
    assert!(
        since_write < Duration::from_secs(3) && elapsed < BATCH_BOUND + Duration::from_secs(1),
        "probe took {elapsed:?}, {since_write:?} after the batch (bound {BATCH_BOUND:?})"
    );
    assert_eq!(caps.da1.as_deref(), Some(&[62, 4][..]));
    assert!(caps.sixel);
    assert_eq!(caps.kitty_keyboard, Some(31));
    assert_eq!(caps.kitty_graphics_ok, Some(true));
    let xt = caps.xtgettcap.expect("xtgettcap");
    assert!(xt.found);
    assert_eq!(xt.entries, vec![("RGB".into(), Some("rgb".into()))]);
    assert_eq!(caps.osc10.as_deref(), Some("rgb:1111/2222/3333"));
    assert_eq!(caps.osc11.as_deref(), Some("rgb:aaaa/bbbb/cccc"));
    assert_eq!(caps.cell_pixels, Some((20, 10)));
    assert_eq!(caps.text_area_pixels, Some((1080, 1920)));
    assert_eq!(session.size().cell_width, 10);
    assert_eq!(session.size().cell_height, 20);
    session.restore().expect("restore");
    reply.join();
}

#[test]
fn probe_keeps_a_late_reply_as_a_reply() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let mut session = Terminal::enter(slave).expect("enter");
    let start = Instant::now();
    // No answer exists until this returns. "Late" is that fact, not a sleep
    // that might not have started yet.
    let deadline = Duration::from_millis(40);
    let caps = session.probe(deadline).expect("probe");
    let elapsed = start.elapsed();
    assert!(caps.da1.is_none(), "late DA1 was absorbed: {caps:?}");
    assert!(elapsed < Duration::from_secs(2), "probe waited {elapsed:?}");
    write_all(&master, b"\x1b[?62;4c");
    let inputs = session.read_events().expect("read");
    assert!(
        inputs
            .iter()
            .all(|event| !matches!(event, TermEvent::Key(_))),
        "late DA1 became a key: {inputs:?}"
    );
    assert!(
        session.capabilities().sixel,
        "late DA1 was not kept as a reply: {:?}",
        session.capabilities()
    );
    assert_eq!(session.capabilities().da1.as_deref(), Some(&[62, 4][..]));
    session.restore().expect("restore");
}

#[test]
fn probe_with_no_reply_uses_the_deadline() {
    let _deadline = arm_deadline();
    let (_master, slave) = open_pty().expect("openpty");
    let mut session = Terminal::enter(slave).expect("enter");
    let start = Instant::now();
    let caps = session.probe(PROBE_DEADLINE).expect("probe");
    let elapsed = start.elapsed();
    assert!(caps.da1.is_none());
    assert!(caps.kitty_keyboard.is_none());
    assert!(
        elapsed >= Duration::from_millis(140) && elapsed < Duration::from_millis(500),
        "deadline elapsed {elapsed:?}"
    );
    session.restore().expect("restore");
}

#[test]
fn probe_ignores_malformed_replies_and_later_keys_stay_keys() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let master_for_reply = master.try_clone().expect("dup master");
    let reply = reply_after(
        master_for_reply,
        Duration::ZERO,
        b"\x1b[?62;4x\x1b_Nope\x1b\\\x1b]99;nope\x07",
    );
    let mut session = Terminal::enter(slave).expect("enter");
    let caps = session.probe(Duration::from_millis(40)).expect("probe");
    assert_eq!(caps, &Capabilities::default());
    write_all(&master, b"q");
    let inputs = session.read_events().expect("read");
    assert!(
        matches!(
            inputs.as_slice(),
            [TermEvent::Key(press), TermEvent::Key(release)]
                if press.key == Key::Character("q".into())
                    && press.state == ButtonState::Pressed
                    && release.key == press.key
                    && release.state == ButtonState::Released
                    && release.text.is_none()
        ),
        "legacy q should be a press and a synthesized release: {inputs:?}"
    );
    session.restore().expect("restore");
    reply.join().expect("reply thread");
}

#[test]
fn a_stalled_reader_does_not_block_the_writer() {
    let _deadline = arm_deadline();
    let (_master, slave) = open_pty().expect("openpty");
    let mut session = Terminal::enter_with(slave, 4096).expect("enter");
    let frame = vec![b'Z'; 1024];
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(800) && session.dropped_frames() == 0 {
        session.write_frame(&frame).expect("frame");
    }
    assert!(
        session.dropped_frames() > 0,
        "pending {} drops {}",
        session.pending_len(),
        session.dropped_frames()
    );
    assert!(session.pending_len() <= 4096);
    assert!(start.elapsed() < Duration::from_secs(1));
    let started = Instant::now();
    let restored = session.restore();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "restore blocked for {:?}",
        started.elapsed()
    );
    // Nobody is reading the pty, so the reset bytes may not land. That is an
    // error, not a hang, and Drop must return as well.
    let _ = restored;
    let started = Instant::now();
    drop(session);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "drop blocked for {:?}",
        started.elapsed()
    );
}

#[test]
fn restore_and_drop_return_when_the_reader_never_reads() {
    let _deadline = arm_deadline();
    let (_master, slave) = open_pty().expect("openpty");
    let mut session = Terminal::enter_with(slave, 4096).expect("enter");
    let frame = vec![b'Q'; 1024];
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(800) && session.dropped_frames() == 0 {
        session.write_frame(&frame).expect("frame");
    }
    assert!(session.dropped_frames() > 0, "pty never filled");
    // A 1024-byte write is rejected while the reset sequence would still fit.
    // Fill that tail one byte at a time, or restore can finish and this test
    // never reaches the full-buffer path.
    session.discard_pending();
    let tail = Instant::now();
    while tail.elapsed() < Duration::from_millis(800) && session.pending_len() == 0 {
        session.write_frame(b"Q").expect("tail");
    }
    assert!(session.pending_len() > 0, "the pty still accepted a byte");
    let started = Instant::now();
    let error = session
        .restore()
        .expect_err("a full pty must report an incomplete reset");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "restore blocked for {:?}",
        started.elapsed()
    );
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    let started = Instant::now();
    drop(session);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "drop blocked for {:?}",
        started.elapsed()
    );
}

#[test]
fn normal_exit_restores_termios() {
    let _deadline = arm_deadline();
    let mut pty = PtyChild::spawn("enter-exit", false);
    let status = pty.wait_timeout(Duration::from_secs(2));
    assert!(
        status.success(),
        "{status:?} stderr {:?}",
        pty.stderr_text()
    );
    let output = drain_for(&pty.master, Duration::from_millis(200));
    assert!(
        contains_in_order(&output, &[ENTER, b"READY", LEAVE, b"DONE"]),
        "{output:?}"
    );
    assert!(sighurt_tui::term::termios_matches(
        &tcgetattr(&pty.slave).expect("termios"),
        &pty.original
    ));
}

#[test]
fn panic_in_the_child_restores_termios() {
    let _deadline = arm_deadline();
    let mut pty = PtyChild::spawn("panic", false);
    let status = pty.wait_timeout(Duration::from_secs(2));
    assert!(!status.success(), "{status:?}");
    let output = drain_for(&pty.master, Duration::from_millis(200));
    assert!(
        contains_in_order(&output, &[ENTER, b"READY", LEAVE]),
        "{output:?}"
    );
    assert!(
        !output.windows(8).any(|window| window == b"panicked"),
        "panic text leaked onto the pty: {output:?}"
    );
    assert!(sighurt_tui::term::termios_matches(
        &tcgetattr(&pty.slave).expect("termios"),
        &pty.original
    ));
    let err = pty.stderr_text();
    assert!(
        err.windows(8).any(|window| window == b"panicked"),
        "stderr {:?}",
        String::from_utf8_lossy(&err)
    );
}

#[test]
fn sigtstp_restores_and_sigcont_reenters() {
    let _deadline = arm_deadline();
    let mut pty = PtyChild::spawn("suspend", false);
    let started = read_until(&pty.master, b"READY", Duration::from_secs(2));
    let raw = tcgetattr(&pty.slave).expect("raw");
    assert!(!raw.local_modes.contains(LocalModes::ICANON));
    assert!(!raw.local_modes.contains(LocalModes::ECHO));
    rustix::process::kill_process(pty.pid(), Signal::TSTP).expect("SIGTSTP");
    wait_stopped(&mut pty);
    assert!(sighurt_tui::term::termios_matches(
        &tcgetattr(&pty.slave).expect("stopped termios"),
        &pty.original
    ));
    let stopped = drain_for(&pty.master, Duration::from_millis(200));
    let mut until_stop = started;
    until_stop.extend(stopped);
    assert!(
        contains_in_order(&until_stop, &[ENTER, b"READY", LEAVE]),
        "{until_stop:?}"
    );
    rustix::process::kill_process(pty.pid(), Signal::CONT).expect("SIGCONT");
    let status = pty.wait_timeout(Duration::from_secs(2));
    assert!(
        status.success(),
        "{status:?} stderr {:?}",
        pty.stderr_text()
    );
    let rest = drain_for(&pty.master, Duration::from_millis(200));
    assert!(
        contains_in_order(&rest, &[ENTER, b"CONT", LEAVE]),
        "{rest:?}"
    );
    assert!(sighurt_tui::term::termios_matches(
        &tcgetattr(&pty.slave).expect("final termios"),
        &pty.original
    ));
}

#[test]
fn sighup_restores_and_terminates() {
    let _deadline = arm_deadline();
    let mut pty = PtyChild::spawn("hangup", false);
    let started = read_until(&pty.master, b"READY", Duration::from_secs(2));
    rustix::process::kill_process(pty.pid(), Signal::HUP).expect("SIGHUP");
    let status = pty.wait_timeout(Duration::from_secs(2));
    assert_eq!(status.signal(), Some(libc::SIGHUP), "{status:?}");
    let output = drain_for(&pty.master, Duration::from_millis(200));
    let mut all = started;
    all.extend(output);
    assert!(
        contains_in_order(&all, &[ENTER, b"READY", LEAVE]),
        "{all:?}"
    );
    assert!(sighurt_tui::term::termios_matches(
        &tcgetattr(&pty.slave).expect("termios"),
        &pty.original
    ));
}

#[test]
fn prepare_forced_exit_restores_before_exit() {
    let _deadline = arm_deadline();
    let mut pty = PtyChild::spawn("force-exit", false);
    let status = pty.wait_timeout(Duration::from_secs(2));
    assert!(
        status.success(),
        "{status:?} stderr {:?}",
        pty.stderr_text()
    );
    let output = drain_for(&pty.master, Duration::from_millis(200));
    assert!(
        contains_in_order(&output, &[ENTER, b"READY", LEAVE]),
        "{output:?}"
    );
    assert!(!contains_in_order(&output, &[b"DONE"]));
    assert!(sighurt_tui::term::termios_matches(
        &tcgetattr(&pty.slave).expect("termios"),
        &pty.original
    ));
}

struct App {
    terminal: Terminal,
    events: Vec<TermEvent>,
}

#[test]
fn calloop_reads_the_fd_the_app_owns() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let terminal = Terminal::enter(slave).expect("enter");
    let read_fd = terminal.try_clone_fd().expect("dup");
    let mut eloop: EventLoop<App> = EventLoop::try_new().expect("loop");
    let source = Generic::new(read_fd, Interest::READ, Mode::Level);
    eloop
        .handle()
        .insert_source(source, |readiness, _, app: &mut App| {
            if readiness.readable || readiness.error {
                app.events.extend(app.terminal.read_events()?);
            }
            Ok(PostAction::Continue)
        })
        .expect("register");

    let mut app = App {
        terminal,
        events: Vec::new(),
    };
    write_all(&master, b"\x1b[97");
    eloop
        .dispatch(Some(Duration::from_millis(30)), &mut app)
        .expect("partial dispatch");
    assert!(app.events.is_empty(), "{:?}", app.events);
    let timeout = app
        .terminal
        .poll_timeout()
        .expect("partial sequence armed a timer");
    assert!(
        timeout > ESC_TIMEOUT,
        "CSI used the Escape timer: {timeout:?}"
    );
    assert!(app.terminal.expire_if_due().is_empty());

    write_all(&master, b"u");
    eloop
        .dispatch(Some(Duration::from_millis(200)), &mut app)
        .expect("complete dispatch");
    assert!(
        matches!(
            app.events.first(),
            Some(TermEvent::Key(key)) if key.key == Key::Character("a".into())
        ),
        "{:?}",
        app.events
    );

    app.events.clear();
    write_all(&master, b"\x1b");
    eloop
        .dispatch(Some(Duration::ZERO), &mut app)
        .expect("escape dispatch");
    assert!(app.events.is_empty(), "{:?}", app.events);
    let escape_wait = app.terminal.poll_timeout().expect("escape timer");
    assert!(escape_wait <= ESC_TIMEOUT);
    assert!(app.terminal.expire_if_due().is_empty());
    let start = Instant::now();
    eloop
        .dispatch(Some(escape_wait), &mut app)
        .expect("wait for escape");
    assert!(
        start.elapsed() + Duration::from_millis(5) >= escape_wait.min(Duration::from_millis(15)),
        "dispatch returned without waiting"
    );
    let mut expired = app.terminal.expire_if_due();
    if expired.is_empty() {
        thread::sleep(Duration::from_millis(30));
        expired = app.terminal.expire_if_due();
    }
    assert!(
        matches!(
            expired.first(),
            Some(TermEvent::Key(key))
                if key.key == Key::Named("Escape".into()) && key.state == ButtonState::Pressed
        ),
        "{expired:?}"
    );
    app.terminal.restore().expect("restore");
}

#[test]
fn a_helper_panic_does_not_restore_the_terminal() {
    let _deadline = arm_deadline();
    let (_master, slave) = open_pty().expect("openpty");
    let mut session = Terminal::enter(slave).expect("enter");
    let fd = session.try_clone_fd().expect("dup");
    let flags_before = rustix::fs::fcntl_getfl(&fd).expect("getfl");
    assert!(flags_before.contains(OFlags::NONBLOCK));
    let helper = thread::spawn(|| {
        let caught = catch_unwind(AssertUnwindSafe(|| panic!("helper panic")));
        assert!(caught.is_err());
    });
    helper.join().expect("helper thread");
    let fd = session.try_clone_fd().expect("dup after panic");
    let flags = rustix::fs::fcntl_getfl(&fd).expect("getfl after panic");
    assert!(
        flags.contains(OFlags::NONBLOCK),
        "helper panic cleared O_NONBLOCK: {flags:?}"
    );
    let ios = tcgetattr(&fd).expect("termios after panic");
    assert!(
        !ios.local_modes.contains(LocalModes::ICANON),
        "helper panic restored cooked mode"
    );
    let start = Instant::now();
    let caps = session.probe(Duration::from_millis(80)).expect("probe");
    let elapsed = start.elapsed();
    assert!(caps.da1.is_none());
    assert!(
        elapsed < Duration::from_millis(400),
        "probe blocked after the helper panic: {elapsed:?}"
    );
    session.restore().expect("restore");
}

#[test]
fn a_slow_reader_still_receives_the_full_restore() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let mut session = Terminal::enter(slave).expect("enter");
    let _entered = drain_for(&master, Duration::from_millis(40));

    let mut apc = Vec::with_capacity(128 * 1024);
    apc.extend_from_slice(b"\x1b_Gq=2,m=0;");
    apc.extend(std::iter::repeat_n(b'A', 96 * 1024));
    apc.extend_from_slice(b"\x1b\\");
    session.write_frame(&apc).expect("apc");

    let master_for_reader = master.try_clone().expect("dup master");
    let reader = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        match wait_for(&master_for_reader, LEAVE, Duration::from_millis(700)) {
            Ok(got) => got,
            Err(got) => got,
        }
    });
    session.restore().expect("restore");
    let got = reader.join().expect("reader");
    let leave_at = got
        .windows(LEAVE.len())
        .position(|window| window == LEAVE)
        .unwrap_or_else(|| panic!("LEAVE missing from {} bytes: {got:?}", got.len()));
    assert!(
        got[..leave_at].windows(2).any(|window| window == b"\x1b\\"),
        "open APC was not closed before LEAVE"
    );
    assert!(got[leave_at..].starts_with(LEAVE));
    assert!(LEAVE
        .windows(b"[?2026l".len())
        .any(|window| window == b"[?2026l"));

    session.restore().expect("second restore");
    let extra = drain_for(&master, Duration::from_millis(80));
    assert!(
        extra.is_empty(),
        "second restore wrote {} bytes: {extra:?}",
        extra.len()
    );
}

#[test]
fn a_two_megabyte_paste_is_not_one_batch() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let mut session = Terminal::enter(slave).expect("enter");
    let _entered = drain_for(&master, Duration::from_millis(40));

    let stop = Arc::new(AtomicBool::new(false));
    let written = Arc::new(AtomicUsize::new(0));
    let writer_fd = master.try_clone().expect("dup master");
    let stop_writer = Arc::clone(&stop);
    let written_writer = Arc::clone(&written);
    let writer = thread::spawn(move || {
        let chunk = [b'a'; 4096];
        while written_writer.load(Ordering::Relaxed) < 2 * 1024 * 1024
            && !stop_writer.load(Ordering::Relaxed)
        {
            match rustix::io::write(&writer_fd, &chunk) {
                Ok(0) => break,
                Ok(n) => {
                    written_writer.fetch_add(n, Ordering::Relaxed);
                }
                Err(Errno::INTR) => continue,
                Err(Errno::AGAIN) => thread::sleep(Duration::from_millis(1)),
                Err(_) => break,
            }
        }
    });

    let mut batches = 0usize;
    let mut largest = 0usize;
    let start = Instant::now();
    while written.load(Ordering::Relaxed) < 2 * 1024 * 1024
        && start.elapsed() < Duration::from_secs(6)
    {
        let events = session.read_events().expect("read");
        if events.is_empty() {
            thread::sleep(Duration::from_millis(1));
            continue;
        }
        assert!(
            events.len() <= 512,
            "one read_events returned {} events",
            events.len()
        );
        largest = largest.max(events.len());
        batches += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let drain_until = Instant::now() + Duration::from_millis(200);
    while Instant::now() < drain_until {
        let events = session.read_events().expect("drain");
        if !events.is_empty() {
            assert!(events.len() <= 512, "drain batch {}", events.len());
        }
    }
    writer.join().expect("writer");
    let sent = written.load(Ordering::Relaxed);
    assert!(
        sent > 64 * 1024,
        "writer only queued {sent} bytes across {batches} batches"
    );
    assert!(
        batches >= 2,
        "batches {batches}, largest {largest}, sent {sent}"
    );
    assert!(largest <= 512 && largest > 0, "largest {largest}");
    session.restore().expect("restore");
}

#[test]
fn a_late_kitty_reply_is_a_resize() {
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    set_nonblock(&master);
    let mut session = Terminal::enter(slave).expect("enter");
    let caps = session.probe(Duration::from_millis(30)).expect("probe");
    assert!(caps.kitty_graphics_ok.is_none(), "{caps:?}");
    write_all(&master, b"\x1b_Gi=1;OK\x1b\\");
    let events = session.read_events().expect("read");
    assert!(
        events
            .iter()
            .any(|event| matches!(event, TermEvent::Resize(_))),
        "late kitty reply did not relayout: {events:?} caps {:?}",
        session.capabilities()
    );
    assert_eq!(session.capabilities().kitty_graphics_ok, Some(true));
    session.restore().expect("restore");
}

#[test]
fn restore_deadline_holds_under_sigwinch() {
    if std::env::var_os("SIGHURT_RESTORE_WINCH_CHILD").is_some() {
        restore_deadline_child();
    }
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    // Keep `master` open and unread so the child's restore has to wait out
    // the budget, and so closing the master does not deliver `SIGHUP`.
    let _master = master;
    let mut child = spawn_self(
        "restore_deadline_holds_under_sigwinch",
        "SIGHURT_RESTORE_WINCH_CHILD",
        &slave,
    );
    // The child aims SIGWINCH at its own restore thread. A process-directed
    // signal can be taken by libtest's other thread and never interrupt poll.
    let start = Instant::now();
    let limit = Duration::from_secs(4);
    let (status, err) = loop {
        if past_deadline() || start.elapsed() > limit {
            let _ = child.kill();
            let status = child.wait().expect("wait after kill");
            let err = read_stderr(&mut child);
            if past_deadline() {
                panic!("test deadline exceeded ({status:?}) stderr {err}");
            }
            panic!("restore did not finish under SIGWINCH ({status:?}) stderr {err}");
        }
        match child.try_wait() {
            Ok(Some(status)) => break (status, read_stderr(&mut child)),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(err) => panic!("try_wait: {err}"),
        }
    };
    assert!(status.success(), "child status {status:?} stderr {err}");
}

fn restore_deadline_child() -> ! {
    let slave = match std::io::stdout().as_fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(err) => child_fail(3, &format!("stdout: {err}")),
    };
    let mut terminal = match Terminal::enter_with(slave, 4096) {
        Ok(terminal) => terminal,
        Err(err) => child_fail(3, &format!("enter: {err}")),
    };
    if let Err(err) = terminal.install_handlers() {
        child_fail(3, &format!("handlers: {err}"));
    }
    let frame = vec![b'Z'; 1024];
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(800) && terminal.dropped_frames() == 0 {
        if terminal.write_frame(&frame).is_err() {
            child_fail(3, "frame write failed");
        }
    }
    if terminal.dropped_frames() == 0 {
        child_fail(3, "pty never filled");
    }
    terminal.discard_pending();
    let tail = Instant::now();
    while tail.elapsed() < Duration::from_millis(800) && terminal.pending_len() == 0 {
        if terminal.write_frame(b"Q").is_err() {
            child_fail(3, "tail write failed");
        }
    }
    if terminal.pending_len() == 0 {
        child_fail(3, "the pty still accepted a byte");
    }
    let tid = unsafe { libc::gettid() };
    let stop = Arc::new(AtomicBool::new(false));
    let stop_signaler = Arc::clone(&stop);
    let signaler = thread::spawn(move || {
        while !stop_signaler.load(Ordering::Relaxed) {
            unsafe { libc::syscall(libc::SYS_tkill, tid, libc::SIGWINCH) };
            thread::sleep(Duration::from_millis(5));
        }
    });
    let started = Instant::now();
    let restore_result = terminal.restore();
    stop.store(true, Ordering::Relaxed);
    let _ = signaler.join();
    match restore_result {
        Err(err) if err.kind() == std::io::ErrorKind::TimedOut => {}
        Err(err) => child_fail(4, &format!("restore error {err:?}")),
        Ok(()) => child_fail(4, "restore finished; the pty was not full"),
    }
    let elapsed = started.elapsed();
    // The budget is 250 ms. A poll that restarts on each SIGWINCH stays
    // inside that window only when the remaining time is recomputed.
    if elapsed > Duration::from_millis(700) {
        child_fail(2, &format!("restore took {elapsed:?}"));
    }
    unsafe { libc::_exit(0) }
}

#[test]
fn full_winch_pipe_restores_errno() {
    if std::env::var_os("SIGHURT_ERRNO_CHILD").is_some() {
        errno_child();
    }
    let _deadline = arm_deadline();
    let (master, slave) = open_pty().expect("openpty");
    let _master = master;
    let mut child = spawn_self(
        "full_winch_pipe_restores_errno",
        "SIGHURT_ERRNO_CHILD",
        &slave,
    );
    let start = Instant::now();
    let limit = Duration::from_secs(4);
    loop {
        if past_deadline() || start.elapsed() > limit {
            let _ = child.kill();
            let status = child.wait().expect("wait after kill");
            let err = read_stderr(&mut child);
            if past_deadline() {
                panic!("test deadline exceeded ({status:?}) stderr {err}");
            }
            panic!("errno child timed out ({status:?}) stderr {err}");
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let err = read_stderr(&mut child);
                assert!(status.success(), "child status {status:?} stderr {err}");
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(err) => panic!("try_wait: {err}"),
        }
    }
}

fn errno_child() -> ! {
    let slave = match std::io::stdout().as_fd().try_clone_to_owned() {
        Ok(fd) => fd,
        Err(err) => child_fail(3, &format!("stdout: {err}")),
    };
    let mut terminal = match Terminal::enter(slave) {
        Ok(terminal) => terminal,
        Err(err) => child_fail(3, &format!("enter: {err}")),
    };
    if let Err(err) = terminal.install_handlers() {
        child_fail(3, &format!("handlers: {err}"));
    }
    let fd = terminal.winch_fd().as_raw_fd();
    let mut cap = unsafe { libc::fcntl(fd, libc::F_GETPIPE_SZ) };
    if cap > 4096 {
        let resized = unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, 4096) };
        if resized > 0 {
            cap = resized;
        }
    }
    if cap <= 0 || cap > 65536 {
        child_fail(3, &format!("pipe capacity {cap}"));
    }
    // One byte per SIGWINCH. `cap` writes fill the pipe; the next one's
    // `EAGAIN` must not replace the errno the interrupted code had stored.
    for _ in 0..cap {
        unsafe { libc::raise(libc::SIGWINCH) };
    }
    unsafe { *libc::__errno_location() = libc::ENOENT };
    unsafe { libc::raise(libc::SIGWINCH) };
    let now = unsafe { *libc::__errno_location() };
    if now != libc::ENOENT {
        child_fail(
            2,
            &format!("handler left errno {now}, want {}", libc::ENOENT),
        );
    }
    unsafe { libc::_exit(0) }
}
