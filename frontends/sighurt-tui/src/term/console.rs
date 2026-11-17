//! The terminal session on Windows: the console in VT mode, probe, frames, and restoration.
//!
//! The console takes and gives the same bytes a Unix terminal does once its input handle has
//! `ENABLE_VIRTUAL_TERMINAL_INPUT` and its output `ENABLE_VIRTUAL_TERMINAL_PROCESSING` (Windows
//! 10 1809 and later, and every Windows Terminal), so the parser, the probe and the encoders
//! are Unix's. What differs is how the bytes move and what "raw" means:
//!
//! - Raw mode is console modes. Input: VT input, window (resize) records, mouse input, and the
//!   extended flags without quick edit (so a click goes to `sig`, not to the console's
//!   selection); no line input, echo or processed input, so Ctrl+C is the byte 0x03 as in a raw
//!   Unix terminal. Output: VT processing, processed output and no automatic carriage return
//!   after a line feed (a raw Unix terminal's `OPOST` off). Both code pages are UTF-8. The
//!   modes and code pages found at [`Session::enter`] are put back by [`Session::restore`], by
//!   `Drop`, by the panic hook (on the thread that entered), by [`restore_before_forced_exit`]
//!   and, after a native crash, by an unhandled-exception filter ([`on_crash`]).
//! - Input is read by a thread of its own (`sighurt-console-in`): it waits on the console's
//!   input handle and a stop event, reads the input records, turns the characters of key-down
//!   records into UTF-8 (the VT sequences arrive one character per record) and keeps at most
//!   [`INBOX_CAP`] bytes for the owner, waiting while that is full, so the console's own buffer
//!   holds the rest. It wakes the owner loop with a calloop ping for each read, and for a
//!   window-size record (the owner then reads the size again). Mouse, focus and menu records
//!   are dropped: in VT mode the console sends mouse reports and focus changes as sequences.
//! - Output is written by a thread of its own (`sighurt-console-out`), one chunk of at most
//!   [`WRITE_CHUNK`] bytes at a time, cut at a UTF-8 character's boundary: a console write
//!   blocks while the terminal does not read (a slow Windows Terminal, a paused console), and
//!   the owner must not. The owner's queue ([`Output`]) hands the writer a chunk when it is idle
//!   and keeps the rest; the writer wakes the owner loop with a second ping when a chunk is
//!   out. A chunk handed over counts as written, as bytes the kernel accepted do on Unix.
//! - There are no signals. Ctrl+C is a key; Ctrl+Break and the console closing are `sig`'s
//!   console control handler's (a quit, `sighurt_runtime::signals`). There is no job control,
//!   so nothing suspends or resumes, and no hangup: a closed console fails the reads and the
//!   writes. A native crash (an access violation, a stack overflow: an exception nothing handles)
//!   reaches [`on_crash`] on the thread that crashed, which has a helper thread write [`LEAVE`]
//!   (waiting for it at most 200 ms: a stalled console must not hold the crash up) and puts the
//!   modes back, taking no lock, then lets the crash go on as before. What
//!   ends the process without an exception (`abort`, Rust's `__fastfail`, `TerminateProcess`)
//!   still leaves the modes as they were.
//!
//! Restoring waits up to [`RESTORE_BUDGET`] for the writer to finish its chunk, then writes
//! what closes a sequence left open on the wire ([`terminator`]) and [`LEAVE`], and puts the
//! modes back even when the writer did not finish in time.
//!
//! Owns: the console's modes, the two threads and their buffers. Must not own: the parser, the
//! cell grid, page pixels, or what a key does.

use std::ffi::OsStr;
use std::io::{self, ErrorKind};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use calloop::ping::{make_ping, Ping, PingSource};
use windows_sys::Win32::Foundation::{
    GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, WriteFile, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::Console::{
    FlushConsoleInputBuffer, GetConsoleCP, GetConsoleMode, GetConsoleOutputCP,
    GetConsoleScreenBufferInfo, GetNumberOfConsoleInputEvents, ReadConsoleInputW, SetConsoleCP,
    SetConsoleMode, SetConsoleOutputCP, CONSOLE_SCREEN_BUFFER_INFO, DISABLE_NEWLINE_AUTO_RETURN,
    ENABLE_EXTENDED_FLAGS, ENABLE_MOUSE_INPUT, ENABLE_PROCESSED_OUTPUT,
    ENABLE_VIRTUAL_TERMINAL_INPUT, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    INPUT_RECORD, KEY_EVENT, WINDOW_BUFFER_SIZE_EVENT,
};
use windows_sys::Win32::System::Diagnostics::Debug::{
    SetUnhandledExceptionFilter, EXCEPTION_POINTERS, LPTOP_LEVEL_EXCEPTION_FILTER,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, GetCurrentThreadId, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
    INFINITE,
};

use super::modes::{startup_queries, ENTER, LEAVE};
use super::output::{terminator, Output, Sink, Wire};
use super::parser::{Input, Parser};
use super::probe::{self, Caps};

/// Bytes one [`Session::read_input`] call parses before it returns.
pub const INPUT_BYTE_BUDGET: usize = 64 * 1024;
/// Events one [`Session::read_input`] call returns before it stops (as on Unix).
pub const INPUT_EVENT_CAP: usize = 256;
/// How long restoration may wait for the console to take bytes.
const RESTORE_BUDGET: Duration = Duration::from_millis(250);
/// The most input bytes the reader keeps for the owner.
pub const INBOX_CAP: usize = 64 * 1024;
/// The most bytes one console write takes.
pub const WRITE_CHUNK: usize = 64 * 1024;
/// Input records one read takes.
const RECORDS: usize = 128;
/// The UTF-8 code page.
const CP_UTF8: u32 = 65001;

/// A session is active. One per process, as on Unix.
static ACTIVE: AtomicBool = AtomicBool::new(false);
/// What the panic hook and [`restore_before_forced_exit`] restore.
static CURRENT: Mutex<Option<Arc<Shared>>> = Mutex::new(None);
static HOOK_ON: AtomicBool = AtomicBool::new(false);
type PanicHook = Box<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;
static PREV_HOOK: Mutex<Option<PanicHook>> = Mutex::new(None);
/// The unhandled-exception filter is installed (once per process, never removed).
static FILTER_ON: AtomicBool = AtomicBool::new(false);
/// The filter there was before [`on_crash`], as a function address (0: none).
static PREV_FILTER: AtomicUsize = AtomicUsize::new(0);
/// The crash helper's events (0: no helper): [`on_crash`] sets the first, the helper the
/// second when the leave sequence is out.
static CRASH_GO: AtomicUsize = AtomicUsize::new(0);
static CRASH_DONE: AtomicUsize = AtomicUsize::new(0);
/// The console and the wire the crash helper writes for.
static CRASH_OUTPUT: AtomicUsize = AtomicUsize::new(0);
static CRASH_WIRE: AtomicU8 = AtomicU8::new(0);
/// How long [`on_crash`] waits for the helper's write.
const CRASH_WRITE_MS: u32 = 200;

/// Window size, the shape of Unix's `struct winsize`. The console reports no pixel size.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// A console handle shared with a thread. Console handles may be used from any thread.
struct Handle(OwnedHandle);

impl Handle {
    fn raw(&self) -> HANDLE {
        self.0.as_raw_handle() as HANDLE
    }
}

/// The modes found and the modes set, the published wire, and both threads' shared halves.
struct Shared {
    input: Arc<Handle>,
    writer: Arc<Writer>,
    original_in: u32,
    original_out: u32,
    original_cp_in: u32,
    original_cp_out: u32,
    /// The console's modes are raw. Cleared once they are put back.
    active: AtomicBool,
    /// [`LEAVE`] has gone out whole.
    protocol_restored: AtomicBool,
    /// The [`Wire`] after the last byte the writer was handed.
    wire: AtomicU8,
    /// The thread that entered: the panic hook restores only on it.
    owner: u32,
}

/// What the reader keeps for the owner.
#[derive(Default)]
struct Inbox {
    bytes: std::collections::VecDeque<u8>,
    /// Reading the console failed: it is gone. The owner's next read reports it.
    failed: Option<String>,
    stop: bool,
}

/// The input thread's shared half.
struct Reader {
    inbox: Mutex<Inbox>,
    /// Signalled when bytes arrive and when the owner takes some.
    changed: Condvar,
    /// A manual-reset event that ends the thread's wait on the console.
    stop: Handle,
    wake: Ping,
}

/// What the writer has been handed.
#[derive(Default)]
struct Outbox {
    chunk: Vec<u8>,
    busy: bool,
    /// Writing failed; the console is gone.
    failed: Option<String>,
    /// Restored: nothing more is taken.
    closed: bool,
    stop: bool,
}

/// The output thread's shared half.
struct Writer {
    output: Handle,
    outbox: Mutex<Outbox>,
    /// Signalled when a chunk is handed over, when one is out, and to stop.
    changed: Condvar,
    wake: Ping,
}

/// Raw mode on the console, and the bytes waiting to go out.
pub struct Session {
    shared: Arc<Shared>,
    reader: Arc<Reader>,
    output: Output,
    parser: Parser,
    /// Bytes taken from the reader and not yet parsed.
    held: Vec<u8>,
    held_at: usize,
    more_input: bool,
    /// What the user typed while [`Self::probe`] waited, at most [`INPUT_EVENT_CAP`].
    early: Vec<Input>,
    /// The pings' sources, until the owner loop takes them ([`Self::take_wakes`]).
    sources: Option<(PingSource, PingSource)>,
    input_thread: Option<JoinHandle<()>>,
    output_thread: Option<JoinHandle<()>>,
}

impl Session {
    /// Put the console into raw VT mode, with a pending-byte cap of `cap`, and send [`ENTER`].
    pub fn enter_with(cap: usize) -> io::Result<Self> {
        if ACTIVE.swap(true, Ordering::AcqRel) {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "a terminal session is already active",
            ));
        }
        match Self::start(cap) {
            Ok(session) => Ok(session),
            Err(error) => {
                ACTIVE.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    fn start(cap: usize) -> io::Result<Self> {
        let input = Arc::new(open_console("CONIN$")?);
        let output = open_console("CONOUT$")?;
        let original_in = console_mode(&input)?;
        let original_out = console_mode(&output)?;
        let original_cp_in = unsafe { GetConsoleCP() };
        let original_cp_out = unsafe { GetConsoleOutputCP() };
        let (in_ping, in_source) = make_ping()?;
        let (out_ping, out_source) = make_ping()?;
        let stop = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if stop.is_null() {
            return Err(io::Error::last_os_error());
        }
        let stop = Handle(unsafe { OwnedHandle::from_raw_handle(stop) });
        // No quick edit (the extended flags without it), line input, echo or processed input.
        let raw_in = ENABLE_VIRTUAL_TERMINAL_INPUT
            | ENABLE_WINDOW_INPUT
            | ENABLE_MOUSE_INPUT
            | ENABLE_EXTENDED_FLAGS;
        let raw_out = original_out
            | ENABLE_PROCESSED_OUTPUT
            | ENABLE_VIRTUAL_TERMINAL_PROCESSING
            | DISABLE_NEWLINE_AUTO_RETURN;
        if unsafe { SetConsoleMode(output.raw(), raw_out) } == 0 {
            // A console without DISABLE_NEWLINE_AUTO_RETURN; VT processing is what matters.
            let vt = original_out | ENABLE_PROCESSED_OUTPUT | ENABLE_VIRTUAL_TERMINAL_PROCESSING;
            if unsafe { SetConsoleMode(output.raw(), vt) } == 0 {
                return Err(io::Error::other(format!(
                    "this console has no VT mode ({}); sig needs Windows 10 1809 or later",
                    io::Error::last_os_error()
                )));
            }
        }
        if unsafe { SetConsoleMode(input.raw(), raw_in) } == 0 {
            let error = io::Error::last_os_error();
            unsafe { SetConsoleMode(output.raw(), original_out) };
            return Err(io::Error::other(format!(
                "the console's input has no VT mode: {error}"
            )));
        }
        unsafe {
            SetConsoleCP(CP_UTF8);
            SetConsoleOutputCP(CP_UTF8);
        }
        let reader = Arc::new(Reader {
            inbox: Mutex::new(Inbox::default()),
            changed: Condvar::new(),
            stop,
            wake: in_ping,
        });
        let writer = Arc::new(Writer {
            output,
            outbox: Mutex::new(Outbox::default()),
            changed: Condvar::new(),
            wake: out_ping,
        });
        let shared = Arc::new(Shared {
            input: Arc::clone(&input),
            writer: Arc::clone(&writer),
            original_in,
            original_out,
            original_cp_in,
            original_cp_out,
            active: AtomicBool::new(true),
            protocol_restored: AtomicBool::new(false),
            wire: AtomicU8::new(Wire::GROUND.as_u8()),
            owner: unsafe { GetCurrentThreadId() },
        });
        let input_thread = {
            let reader = Arc::clone(&reader);
            let input = Arc::clone(&input);
            std::thread::Builder::new()
                .name("sighurt-console-in".into())
                .spawn(move || read_console(&input, &reader))
        };
        let output_thread = {
            let writer = Arc::clone(&writer);
            std::thread::Builder::new()
                .name("sighurt-console-out".into())
                .spawn(move || write_console(&writer))
        };
        let (input_thread, output_thread) = match (input_thread, output_thread) {
            (Ok(input), Ok(output)) => (input, output),
            (input, output) => {
                // The modes go back; a thread that did start ends with its handles.
                restore_shared(&shared);
                lock(&reader.inbox).stop = true;
                reader.changed.notify_all();
                unsafe { SetEvent(reader.stop.raw()) };
                lock(&writer.outbox).stop = true;
                writer.changed.notify_all();
                let error = input.err().or(output.err());
                return Err(error.unwrap_or_else(|| io::Error::other("no thread")));
            }
        };
        *lock(&CURRENT) = Some(Arc::clone(&shared));
        let mut session = Session {
            shared,
            reader,
            output: Output::new(cap),
            parser: Parser::new(),
            held: Vec::new(),
            held_at: 0,
            more_input: false,
            early: Vec::new(),
            sources: Some((in_source, out_source)),
            input_thread: Some(input_thread),
            output_thread: Some(output_thread),
        };
        session.write_frame(ENTER)?;
        install_panic_hook();
        install_crash_filter();
        Ok(session)
    }

    /// The input ping's source (the console has input, or a new size) and the output ping's
    /// (a chunk is out), for the owner loop. `None` after the first call.
    pub fn take_wakes(&mut self) -> Option<(PingSource, PingSource)> {
        self.sources.take()
    }

    /// Put the console's modes and code pages back, and send [`LEAVE`]. A second call after
    /// that succeeded writes nothing. Input the console still holds is discarded, as Unix's
    /// `TCIFLUSH` does: a key typed in raw mode must not reach the shell.
    pub fn restore(&mut self) -> io::Result<()> {
        let sent = restore_shared(&self.shared);
        unsafe { FlushConsoleInputBuffer(self.shared.input.raw()) };
        match sent {
            true => Ok(()),
            false => Err(io::Error::new(
                ErrorKind::TimedOut,
                "terminal protocol was not fully restored",
            )),
        }
    }

    /// Nothing to install: the console has no signals (see the module documentation).
    pub fn install_handlers(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// Nothing is written on a window-size record: the owner reads the size on every read.
    pub fn drain_winch(&self) {}

    /// Never: the console has no job control.
    pub fn take_resumed(&self) -> bool {
        false
    }

    /// Never: a closed console comes as `sig`'s quit, and fails reads and writes.
    pub fn take_hangup(&self) -> bool {
        false
    }

    /// The console window's size in cells (not its buffer's, which can be taller).
    pub fn winsize(&self) -> io::Result<Winsize> {
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { std::mem::zeroed() };
        if unsafe { GetConsoleScreenBufferInfo(self.shared.writer.output.raw(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let window = info.srWindow;
        let cols = i32::from(window.Right) - i32::from(window.Left) + 1;
        let rows = i32::from(window.Bottom) - i32::from(window.Top) + 1;
        Ok(Winsize {
            ws_row: u16::try_from(rows.max(1)).unwrap_or(u16::MAX),
            ws_col: u16::try_from(cols.max(1)).unwrap_or(u16::MAX),
            ws_xpixel: 0,
            ws_ypixel: 0,
        })
    }

    /// Bytes still waiting in the output cap.
    pub fn pending_len(&self) -> usize {
        self.output.pending_len()
    }

    /// Frames dropped because the console was not taking them.
    pub fn dropped_frames(&self) -> u64 {
        self.output.drops()
    }

    /// The most unsent bytes the output queue keeps.
    pub fn output_cap(&self) -> usize {
        self.output.cap()
    }

    /// Queue `bytes` whole, or not at all. See [`Output::write_reliable`].
    pub fn write_reliable(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let shared = &self.shared;
        self.output
            .write_reliable(&*shared.writer, bytes, |wire| publish(shared, wire))
    }

    /// Queue `frame`. Does not block when the console stops taking bytes.
    pub fn write_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        let shared = &self.shared;
        self.output
            .write_frame(&*shared.writer, frame, |wire| publish(shared, wire))
    }

    /// Drop bytes not yet handed to the writer, end what the written ones left open, then
    /// `extra`.
    pub fn abort(&mut self, extra: &[u8]) {
        self.output.abort(extra);
        publish(&self.shared, Wire::from_u8(self.output.wire_byte()));
    }

    /// Whether [`Self::read_input`] left input that a later call should take.
    pub fn input_pending(&self) -> bool {
        self.more_input || self.held_at < self.held.len()
    }

    /// Send the startup queries and collect replies until DA1 or `deadline`, as on Unix. Input
    /// that is not a reply is kept for [`Self::read_input`] once DA1 has answered, and the
    /// input ping is rung so the owner reads it.
    pub fn probe(&mut self, deadline: Duration) -> io::Result<Caps> {
        let queries = startup_queries(std::env::var_os("TERM").as_deref());
        self.write_frame(queries)?;
        let start = Instant::now();
        let mut caps = Caps::default();
        let mut saw_da1 = false;
        let mut early = Vec::new();
        loop {
            // The queries may still wait behind ENTER: hand the writer more as it takes them,
            // waking at least every 10 ms while any are left.
            self.write_frame(&[])?;
            let remaining = deadline.saturating_sub(start.elapsed());
            let mut wait = if saw_da1 { Duration::ZERO } else { remaining };
            if self.pending_len() > 0 {
                wait = wait.min(Duration::from_millis(10));
            }
            let bytes = self.reader.take_waiting(INBOX_CAP, wait)?;
            if bytes.is_empty() {
                if saw_da1 || start.elapsed() >= deadline {
                    break;
                }
                continue;
            }
            for input in self.parser.feed(&bytes) {
                if probe::absorb(&mut caps, &input) {
                    saw_da1 = true;
                } else if !matches!(input, Input::Reply(_)) && early.len() < INPUT_EVENT_CAP {
                    push_coalesced(&mut early, input);
                }
            }
        }
        if saw_da1 && !early.is_empty() {
            self.early = early;
            self.reader.wake.ping();
        }
        Ok(caps)
    }

    /// Read what the reader holds, up to [`INPUT_BYTE_BUDGET`] and [`INPUT_EVENT_CAP`], and
    /// parse it. Input the probe kept comes first. As on Unix.
    pub fn read_input(&mut self) -> io::Result<Vec<Input>> {
        let mut inputs = std::mem::take(&mut self.early);
        let mut parsed = 0usize;
        let mut hit_budget = false;
        while inputs.len() < INPUT_EVENT_CAP && parsed < INPUT_BYTE_BUDGET {
            if self.held_at >= self.held.len() {
                self.held.clear();
                self.held_at = 0;
                if !self.shared.active.load(Ordering::Acquire) {
                    break;
                }
                let room = (INPUT_BYTE_BUDGET - parsed).min(8192);
                let bytes = self.reader.take_waiting(room, Duration::ZERO)?;
                if bytes.is_empty() {
                    break;
                }
                self.held = bytes;
            }
            let available = self.held.len() - self.held_at;
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
            self.reader.wake.ping();
        } else if hit_budget {
            self.more_input = self.reader.has_bytes();
        }
        Ok(inputs)
    }

    /// Time until a partial escape sequence should be expired; zero while input is pending.
    pub fn poll_timeout(&self) -> Option<Duration> {
        if self.input_pending() {
            return Some(Duration::ZERO);
        }
        self.parser.poll_timeout()
    }

    /// Expire only if the timer has elapsed.
    pub fn expire_if_due(&mut self) -> Vec<Input> {
        self.parser.expire_if_due()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = restore_shared(&self.shared);
        if !std::thread::panicking() {
            uninstall_panic_hook();
        }
        *lock(&CURRENT) = None;
        // The reader ends at once: it waits on the stop event, or for room the owner frees.
        {
            let mut inbox = lock(&self.reader.inbox);
            inbox.stop = true;
        }
        self.reader.changed.notify_all();
        unsafe { SetEvent(self.reader.stop.raw()) };
        if let Some(thread) = self.input_thread.take() {
            let _ = thread.join();
        }
        // The writer ends once its chunk is out; a console that never takes it keeps that
        // thread (and the handle it owns) until the process ends, rather than this waiting.
        let idle = {
            let mut outbox = lock(&self.shared.writer.outbox);
            outbox.stop = true;
            !outbox.busy
        };
        self.shared.writer.changed.notify_all();
        if let Some(thread) = self.output_thread.take() {
            if idle {
                let _ = thread.join();
            }
        }
        ACTIVE.store(false, Ordering::Release);
    }
}

/// [`Session::restore`] for the moment before the process is ended: waits at most
/// [`RESTORE_BUDGET`]. Idempotent, including when no session is active.
pub fn restore_before_forced_exit() {
    let current = match CURRENT.try_lock() {
        Ok(current) => current.clone(),
        Err(_) => None,
    };
    if let Some(shared) = current {
        let _ = restore_shared(&shared);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn open_console(name: &str) -> io::Result<Handle> {
    let wide: Vec<u16> = OsStr::new(name).encode_wide().chain([0]).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(io::Error::other(format!(
            "cannot open the console ({name}): {}",
            io::Error::last_os_error()
        )));
    }
    Ok(Handle(unsafe { OwnedHandle::from_raw_handle(handle) }))
}

fn console_mode(handle: &Handle) -> io::Result<u32> {
    let mut mode = 0;
    if unsafe { GetConsoleMode(handle.raw(), &mut mode) } == 0 {
        return Err(io::Error::other(format!(
            "not a console: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(mode)
}

/// Remembers the wire the bytes handed to the writer leave. Always `true`: no other writer
/// takes the wire here.
fn publish(shared: &Shared, wire: Wire) -> bool {
    shared.wire.store(wire.as_u8(), Ordering::Release);
    true
}

/// Waits for the writer's chunk, writes what closes an open sequence and [`LEAVE`], and puts
/// the modes back. Whether [`LEAVE`] went out whole.
fn restore_shared(shared: &Shared) -> bool {
    let deadline = Instant::now() + RESTORE_BUDGET;
    if !shared.protocol_restored.load(Ordering::Acquire) {
        let wire = Wire::from_u8(shared.wire.load(Ordering::Acquire));
        let mut bytes = terminator(wire).as_bytes().to_vec();
        bytes.extend_from_slice(LEAVE);
        if shared.writer.write_last(&bytes, deadline) {
            shared.protocol_restored.store(true, Ordering::Release);
            shared.wire.store(Wire::GROUND.as_u8(), Ordering::Release);
        }
    }
    if shared.active.swap(false, Ordering::AcqRel) {
        unsafe {
            SetConsoleMode(shared.input.raw(), shared.original_in);
            SetConsoleMode(shared.writer.output.raw(), shared.original_out);
            SetConsoleCP(shared.original_cp_in);
            SetConsoleOutputCP(shared.original_cp_out);
        }
    }
    shared.protocol_restored.load(Ordering::Acquire)
}

impl Reader {
    /// Takes up to `max` bytes, waiting up to `wait` for some. Empty when none came.
    fn take_waiting(&self, max: usize, wait: Duration) -> io::Result<Vec<u8>> {
        let deadline = Instant::now() + wait;
        let mut inbox = lock(&self.inbox);
        loop {
            if !inbox.bytes.is_empty() {
                let n = max.min(inbox.bytes.len());
                let bytes: Vec<u8> = inbox.bytes.drain(..n).collect();
                drop(inbox);
                self.changed.notify_all();
                return Ok(bytes);
            }
            if let Some(failed) = inbox.failed.take() {
                return Err(io::Error::other(failed));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || inbox.stop {
                return Ok(Vec::new());
            }
            inbox = self
                .changed
                .wait_timeout(inbox, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn has_bytes(&self) -> bool {
        !lock(&self.inbox).bytes.is_empty()
    }
}

/// The input thread: waits on the console's input handle and the stop event, and turns key
/// records into bytes for the owner.
fn read_console(input: &Handle, reader: &Reader) {
    let mut records: Vec<INPUT_RECORD> = vec![unsafe { std::mem::zeroed() }; RECORDS];
    let mut utf16 = Utf16::default();
    let handles = [input.raw(), reader.stop.raw()];
    loop {
        {
            let mut inbox = lock(&reader.inbox);
            while inbox.bytes.len() >= INBOX_CAP && !inbox.stop {
                inbox = reader
                    .changed
                    .wait(inbox)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if inbox.stop {
                return;
            }
        }
        let woke = unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, INFINITE) };
        if woke != WAIT_OBJECT_0 {
            // The stop event, or a failed wait: either way this thread is done.
            if woke != WAIT_OBJECT_0 + 1 {
                fail(
                    reader,
                    format!(
                        "waiting on the console failed: {}",
                        io::Error::last_os_error()
                    ),
                );
            }
            return;
        }
        let mut count = 0u32;
        if unsafe { GetNumberOfConsoleInputEvents(input.raw(), &mut count) } == 0 {
            fail(
                reader,
                format!("reading the console failed: {}", io::Error::last_os_error()),
            );
            return;
        }
        if count == 0 {
            // Signalled with nothing to read: look again shortly rather than spin.
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let mut read = 0u32;
        let ok = unsafe {
            ReadConsoleInputW(input.raw(), records.as_mut_ptr(), RECORDS as u32, &mut read)
        };
        if ok == 0 {
            fail(
                reader,
                format!("reading the console failed: {}", io::Error::last_os_error()),
            );
            return;
        }
        let mut bytes = Vec::new();
        let mut resized = false;
        for record in &records[..read as usize] {
            match u32::from(record.EventType) {
                KEY_EVENT => {
                    let key = unsafe { record.Event.KeyEvent };
                    let unit = unsafe { key.uChar.UnicodeChar };
                    if key.bKeyDown != 0 && unit != 0 {
                        for _ in 0..key.wRepeatCount.max(1) {
                            utf16.push(unit, &mut bytes);
                        }
                    }
                }
                WINDOW_BUFFER_SIZE_EVENT => resized = true,
                _ => {}
            }
        }
        if !bytes.is_empty() {
            lock(&reader.inbox).bytes.extend(bytes);
            reader.changed.notify_all();
        }
        if read > 0 || resized {
            reader.wake.ping();
        }
    }
}

fn fail(reader: &Reader, message: String) {
    lock(&reader.inbox).failed = Some(message);
    reader.changed.notify_all();
    reader.wake.ping();
}

/// UTF-16 code units from key records, as UTF-8. A high surrogate waits for its low one; a
/// lone surrogate is U+FFFD.
#[derive(Default)]
struct Utf16 {
    high: Option<u16>,
}

impl Utf16 {
    fn push(&mut self, unit: u16, out: &mut Vec<u8>) {
        let c = match (self.high.take(), unit) {
            (None, 0xD800..=0xDBFF) => {
                self.high = Some(unit);
                return;
            }
            (Some(high), 0xDC00..=0xDFFF) => {
                let code =
                    0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(unit) - 0xDC00);
                char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
            }
            (Some(_), 0xD800..=0xDBFF) => {
                out.extend_from_slice("\u{FFFD}".as_bytes());
                self.high = Some(unit);
                return;
            }
            (Some(_), _) => {
                out.extend_from_slice("\u{FFFD}".as_bytes());
                char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER)
            }
            (None, _) => char::from_u32(u32::from(unit)).unwrap_or(char::REPLACEMENT_CHARACTER),
        };
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    }
}

impl Writer {
    /// Waits up to `deadline` for the chunk in flight, then writes `bytes` as the last thing
    /// this writer takes, and waits for them too. Whether they went out.
    fn write_last(&self, bytes: &[u8], deadline: Instant) -> bool {
        let (mut outbox, idle) = self.wait_idle(lock(&self.outbox), deadline);
        if !idle || outbox.failed.is_some() || outbox.stop {
            outbox.closed = true;
            return false;
        }
        outbox.chunk.clear();
        outbox.chunk.extend_from_slice(bytes);
        outbox.busy = true;
        outbox.closed = true;
        self.changed.notify_all();
        let (outbox, idle) = self.wait_idle(outbox, deadline);
        idle && outbox.failed.is_none()
    }

    /// Waits until no chunk is in flight or `deadline` passes; whether none is.
    fn wait_idle<'a>(
        &'a self,
        mut outbox: MutexGuard<'a, Outbox>,
        deadline: Instant,
    ) -> (MutexGuard<'a, Outbox>, bool) {
        while outbox.busy {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return (outbox, false);
            }
            outbox = self
                .changed
                .wait_timeout(outbox, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        (outbox, true)
    }
}

/// The owner's queue hands the writer a chunk when it is idle: as much of `buf` as
/// [`WRITE_CHUNK`] allows, ending on a UTF-8 character's boundary. `Ok(0)` while a chunk is in
/// flight, and after restoration (the bytes are dropped with the queue).
impl Sink for Writer {
    fn write_some(&self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut outbox = lock(&self.outbox);
        if let Some(failed) = &outbox.failed {
            return Err(io::Error::other(failed.clone()));
        }
        if outbox.busy || outbox.closed || outbox.stop {
            return Ok(0);
        }
        let n = utf8_cut(buf, WRITE_CHUNK);
        outbox.chunk.clear();
        outbox.chunk.extend_from_slice(&buf[..n]);
        outbox.busy = true;
        drop(outbox);
        self.changed.notify_all();
        Ok(n)
    }
}

/// The longest prefix of `buf`, at most `max` bytes, that does not end inside a UTF-8
/// character; `max` itself when no boundary is in reach (bytes that are not UTF-8).
fn utf8_cut(buf: &[u8], max: usize) -> usize {
    if buf.len() <= max {
        return buf.len();
    }
    let mut n = max;
    while n > 0 && n > max.saturating_sub(4) && (buf[n] & 0xC0) == 0x80 {
        n -= 1;
    }
    if n == 0 || (buf[n] & 0xC0) == 0x80 {
        max
    } else {
        n
    }
}

/// The output thread: writes each chunk it is handed whole, then wakes the owner loop.
fn write_console(writer: &Writer) {
    let mut chunk = Vec::new();
    loop {
        {
            let mut outbox = lock(&writer.outbox);
            while !outbox.busy && !outbox.stop {
                outbox = writer
                    .changed
                    .wait(outbox)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            if !outbox.busy {
                return;
            }
            std::mem::swap(&mut chunk, &mut outbox.chunk);
        }
        let result = write_all(writer.output.raw(), &chunk);
        {
            let mut outbox = lock(&writer.outbox);
            if let Err(error) = result {
                outbox.failed = Some(format!("writing to the console failed: {error}"));
            }
            outbox.busy = false;
            std::mem::swap(&mut chunk, &mut outbox.chunk);
            chunk.clear();
        }
        writer.changed.notify_all();
        writer.wake.ping();
    }
}

fn write_all(handle: HANDLE, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let len = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                handle,
                bytes.as_ptr(),
                len,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        if written == 0 {
            return Err(io::Error::new(
                ErrorKind::WriteZero,
                "the console took nothing",
            ));
        }
        bytes = &bytes[written as usize..];
    }
    Ok(())
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

/// Restores on a panic of the thread that entered, then runs the hook installed before.
fn install_panic_hook() {
    if HOOK_ON.swap(true, Ordering::AcqRel) {
        return;
    }
    *lock(&PREV_HOOK) = Some(std::panic::take_hook());
    std::panic::set_hook(Box::new(|info| {
        let current = match CURRENT.try_lock() {
            Ok(current) => current.clone(),
            Err(_) => None,
        };
        if let Some(shared) = current {
            if shared.owner == unsafe { GetCurrentThreadId() } {
                let _ = restore_shared(&shared);
            }
        }
        if let Ok(previous) = PREV_HOOK.try_lock() {
            if let Some(previous) = previous.as_ref() {
                previous(info);
            }
        }
    }));
}

/// Installs [`on_crash`] as the process's unhandled-exception filter, once, and starts the
/// helper thread that writes the leave sequence for it. Without the helper (no events or no
/// thread) the filter still puts the modes back.
fn install_crash_filter() {
    if FILTER_ON.swap(true, Ordering::AcqRel) {
        return;
    }
    // SAFETY: two auto-reset events, unnamed, not set.
    let (go, done) = unsafe {
        (
            CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()),
            CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()),
        )
    };
    if !go.is_null() && !done.is_null() {
        let (go_raw, done_raw) = (go as usize, done as usize);
        let started = std::thread::Builder::new()
            .name("sighurt-console-crash".into())
            .spawn(move || write_on_crash(go_raw as HANDLE, done_raw as HANDLE));
        if started.is_ok() {
            CRASH_DONE.store(done_raw, Ordering::Release);
            CRASH_GO.store(go_raw, Ordering::Release);
        }
    }
    // SAFETY: `on_crash` is a filter for the life of the process.
    let previous = unsafe { SetUnhandledExceptionFilter(Some(on_crash)) };
    PREV_FILTER.store(
        previous.map_or(0, |filter| filter as usize),
        Ordering::Release,
    );
}

/// The helper of [`on_crash`]: waits until a crash asks, then writes what closes the sequence
/// left open and [`LEAVE`] to the console it names, and says it is done. If the console does
/// not take the bytes the write blocks here, not on the crashing thread.
fn write_on_crash(go: HANDLE, done: HANDLE) {
    loop {
        // SAFETY: an event this thread was given, never closed.
        if unsafe { WaitForSingleObject(go, INFINITE) } != WAIT_OBJECT_0 {
            return;
        }
        let output = CRASH_OUTPUT.load(Ordering::Acquire) as HANDLE;
        let wire = Wire::from_u8(CRASH_WIRE.load(Ordering::Acquire));
        let _ = write_all(output, terminator(wire).as_bytes());
        let _ = write_all(output, LEAVE);
        // SAFETY: as above.
        unsafe { SetEvent(done) };
    }
}

/// The unhandled-exception filter: on the thread that crashed, with the process in an unknown
/// state, so it takes no lock (`try_lock` only), allocates nothing and waits at most
/// [`CRASH_WRITE_MS`]. Its helper thread writes what closes a sequence left open and [`LEAVE`]
/// to the console (a chunk the writer thread is writing may still be cut; a console that takes
/// nothing gets nothing); then it puts the modes and code pages back, and hands the exception
/// on: to the filter before it, else to Windows (which ends the process).
unsafe extern "system" fn on_crash(info: *const EXCEPTION_POINTERS) -> i32 {
    /// `EXCEPTION_CONTINUE_SEARCH`: the system goes on as if there were no filter.
    const CONTINUE_SEARCH: i32 = 0;
    if let Ok(current) = CURRENT.try_lock() {
        if let Some(shared) = current.as_ref() {
            // The leave sequence needs the console's VT mode, so it goes before the modes are
            // put back; a console write blocks while output is stalled, so the helper thread
            // writes it and this waits at most CRASH_WRITE_MS, then goes on either way.
            let go = CRASH_GO.load(Ordering::Acquire);
            if go != 0 && !shared.protocol_restored.swap(true, Ordering::AcqRel) {
                CRASH_OUTPUT.store(shared.writer.output.raw() as usize, Ordering::Release);
                CRASH_WIRE.store(shared.wire.load(Ordering::Acquire), Ordering::Release);
                // SAFETY: the helper's two events, which live as long as the process.
                unsafe {
                    SetEvent(go as HANDLE);
                    WaitForSingleObject(
                        CRASH_DONE.load(Ordering::Acquire) as HANDLE,
                        CRASH_WRITE_MS,
                    );
                }
            }
            if shared.active.swap(false, Ordering::AcqRel) {
                // SAFETY: console handles shared with every thread; plain calls.
                unsafe {
                    SetConsoleMode(shared.input.raw(), shared.original_in);
                    SetConsoleMode(shared.writer.output.raw(), shared.original_out);
                    SetConsoleCP(shared.original_cp_in);
                    SetConsoleOutputCP(shared.original_cp_out);
                }
            }
        }
    }
    let previous = PREV_FILTER.load(Ordering::Acquire);
    if previous == 0 {
        return CONTINUE_SEARCH;
    }
    // SAFETY: the address SetUnhandledExceptionFilter returned, a filter's.
    let previous: LPTOP_LEVEL_EXCEPTION_FILTER = unsafe { std::mem::transmute(previous) };
    match previous {
        // SAFETY: the system's own call, passed on.
        Some(previous) => unsafe { previous(info) },
        None => CONTINUE_SEARCH,
    }
}

fn uninstall_panic_hook() {
    if !HOOK_ON.swap(false, Ordering::AcqRel) {
        return;
    }
    if let Some(previous) = lock(&PREV_HOOK).take() {
        std::panic::set_hook(previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_records_become_utf8_and_lone_surrogates_are_replaced() {
        let mut utf16 = Utf16::default();
        let mut out = Vec::new();
        for unit in "a\u{e4}\u{1F600}".encode_utf16() {
            utf16.push(unit, &mut out);
        }
        assert_eq!(out, "a\u{e4}\u{1F600}".as_bytes());
        out.clear();
        utf16.push(0xD83D, &mut out);
        utf16.push(u16::from(b'x'), &mut out);
        utf16.push(0xDE00, &mut out);
        assert_eq!(String::from_utf8(out).unwrap(), "\u{FFFD}x\u{FFFD}");
    }

    #[test]
    fn a_chunk_ends_on_a_character_boundary() {
        let text = "\u{e4}".repeat(10);
        let bytes = text.as_bytes();
        assert_eq!(utf8_cut(bytes, 100), 20);
        assert_eq!(utf8_cut(bytes, 5), 4);
        assert_eq!(utf8_cut(bytes, 4), 4);
        assert_eq!(utf8_cut(&[0x80; 10], 5), 5);
    }
}
