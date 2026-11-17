//! `sig --tui` and `sig --dump` on Windows, against the demo engine: the real binary in a
//! pseudoconsole (ConPTY) with this test as the terminal on its other side, and `--dump` into a
//! pipe.
//!
//! ConPTY is a console host between `sig` and the test. It turns what `sig` writes into a
//! screen and sends its own rendering of that screen, and it answers the queries a console
//! answers itself (DA1, with no graphics). So these tests read the screen (with vt100), not
//! `sig`'s bytes, and `sig` shows pages as text there. What the console's modes are before
//! and after `sig` is asked of the console by this test binary itself, started in the same
//! pseudoconsole ([`print_console_modes_when_asked`]).
//!
//! Windows' known folders do not follow environment variables, so `sig` reads the
//! configuration of the user running the tests (`%APPDATA%\sighurt`). The tests write one
//! there, so they run only where `CI` is set (GitHub's runners), and one at a time.
//!
//! Windows only. Unix's are `tests/tui.rs`.

#![cfg(windows)]

use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::Console::{
    ClosePseudoConsole, CreatePseudoConsole, GetConsoleMode, COORD, HPCON,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

/// The configuration the tests run with: the demo engine opens `demo:` addresses.
const CONFIG: &str =
    "version = 1\n[engines]\ndefault = \"demo\"\n[routing.demo]\nschemes = [\"demo\"]\n";
/// The tests share the user's configuration directory.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// Whether these tests may write the user's configuration: only on CI.
fn allowed() -> bool {
    if std::env::var_os("CI").is_none() {
        eprintln!(
            "skipped: these tests write %APPDATA%\\sighurt, so they run only where CI is set"
        );
        return false;
    }
    true
}

/// Writes [`CONFIG`] where `sig` reads it, and removes the session and history files.
fn configure() -> PathBuf {
    let dir = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA")).join("sighurt");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("config.toml"), CONFIG).unwrap();
    for name in ["session.toml", "history.toml"] {
        let _ = fs::remove_file(dir.join(name));
    }
    dir
}

/// Not a test of its own: with `SIG_CONSOLE_MODES` set (by [`Console::modes`]) it prints the
/// console's input and output modes, as `modes <in> <out>`, on the console it was started in.
#[test]
fn print_console_modes_when_asked() {
    if std::env::var_os("SIG_CONSOLE_MODES").is_none() {
        return;
    }
    let mode = |name: &str| {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(name)
            .unwrap();
        let mut mode = 0;
        let handle = std::os::windows::io::AsRawHandle::as_raw_handle(&file);
        assert_ne!(unsafe { GetConsoleMode(handle as HANDLE, &mut mode) }, 0);
        mode
    };
    let (input, output) = (mode("CONIN$"), mode("CONOUT$"));
    let mut out = fs::OpenOptions::new().write(true).open("CONOUT$").unwrap();
    std::io::Write::write_all(
        &mut out,
        format!("\r\nmodes {input:#x} {output:#x}\r\n").as_bytes(),
    )
    .unwrap();
}

/// Not a test of its own: with `SIG_CONSOLE_CRASH` set (by
/// [`a_native_crash_in_raw_mode_puts_the_console_back`]) it enters raw mode on the console it
/// was started in as `sig` does, says `RAW`, and writes through a null pointer: an access
/// violation nothing handles, which ends the process.
#[test]
fn crash_in_raw_mode_when_asked() {
    use windows_sys::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_NOGPFAULTERRORBOX};
    if std::env::var_os("SIG_CONSOLE_CRASH").is_none() {
        return;
    }
    let _terminal = sighurt_tui::term::Terminal::enter_stdout(1 << 16).unwrap();
    // Straight to the console: the session's own queue is drained by an owner loop, which this
    // has not.
    thread::sleep(Duration::from_millis(500));
    let mut out = fs::OpenOptions::new().write(true).open("CONOUT$").unwrap();
    std::io::Write::write_all(&mut out, b"RAW").unwrap();
    thread::sleep(Duration::from_secs(2));
    // SAFETY: none: the point is the crash. No error box, so nothing waits for a click.
    unsafe {
        SetErrorMode(SEM_NOGPFAULTERRORBOX);
        std::ptr::null_mut::<u8>().write_volatile(1);
    }
}

/// A pseudoconsole of 80×24 cells, what it renders, and the process started in it.
struct Console {
    console: HPCON,
    input: HANDLE,
    screen: vt100::Parser,
    output: mpsc::Receiver<Vec<u8>>,
    reader: Option<thread::JoinHandle<()>>,
    process: Option<HANDLE>,
}

impl Console {
    fn new() -> Self {
        let (mut input_read, mut input) = (std::ptr::null_mut(), std::ptr::null_mut());
        let (mut output, mut output_write) = (std::ptr::null_mut(), std::ptr::null_mut());
        unsafe {
            assert_ne!(
                CreatePipe(&mut input_read, &mut input, std::ptr::null(), 0),
                0
            );
            assert_ne!(
                CreatePipe(&mut output, &mut output_write, std::ptr::null(), 0),
                0
            );
        }
        let mut console: HPCON = 0;
        let size = COORD { X: 80, Y: 24 };
        let made = unsafe { CreatePseudoConsole(size, input_read, output_write, 0, &mut console) };
        assert_eq!(made, 0, "CreatePseudoConsole: {made:#x}");
        // The pseudoconsole holds its own copies.
        unsafe {
            CloseHandle(input_read);
            CloseHandle(output_write);
        }
        let (sent, received) = mpsc::channel();
        let output = output as usize;
        let reader = thread::spawn(move || {
            let mut buf = vec![0u8; 1 << 16];
            loop {
                let mut read = 0u32;
                let ok = unsafe {
                    ReadFile(
                        output as HANDLE,
                        buf.as_mut_ptr(),
                        buf.len() as u32,
                        &mut read,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 || read == 0 {
                    break;
                }
                if sent.send(buf[..read as usize].to_vec()).is_err() {
                    break;
                }
            }
            unsafe { CloseHandle(output as HANDLE) };
        });
        Self {
            console,
            input,
            screen: vt100::Parser::new(24, 80, 0),
            output: received,
            reader: Some(reader),
            process: None,
        }
    }

    /// Starts `program ARGS` in the pseudoconsole, with `env` added to this process's.
    fn start(&mut self, program: &OsStr, args: &[&str], env: &[(&str, &str)]) {
        let mut line: Vec<u16> = Vec::new();
        line.push(u16::from(b'"'));
        line.extend(program.encode_wide());
        line.push(u16::from(b'"'));
        for arg in args {
            line.push(u16::from(b' '));
            line.extend(OsStr::new(arg).encode_wide());
        }
        line.push(0);
        let mut block: Vec<u16> = Vec::new();
        for (key, value) in std::env::vars_os()
            .filter(|(key, _)| {
                !env.iter()
                    .any(|(k, _)| OsStr::new(k).eq_ignore_ascii_case(key))
            })
            .filter(|(key, _)| !key.eq_ignore_ascii_case("TERM"))
            .chain(env.iter().map(|(k, v)| ((*k).into(), (*v).into())))
        {
            block.extend(key.encode_wide());
            block.push(u16::from(b'='));
            block.extend(value.encode_wide());
            block.push(0);
        }
        block.push(0);
        unsafe {
            let mut size = 0usize;
            InitializeProcThreadAttributeList(std::ptr::null_mut(), 1, 0, &mut size);
            let mut list = vec![0u8; size];
            let attributes = list.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
            assert_ne!(
                InitializeProcThreadAttributeList(attributes, 1, 0, &mut size),
                0
            );
            assert_ne!(
                UpdateProcThreadAttribute(
                    attributes,
                    0,
                    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                    self.console as *const std::ffi::c_void,
                    std::mem::size_of::<HPCON>(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                ),
                0
            );
            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
            // Invalid standard handles, so the child takes the pseudoconsole's and not this
            // process's (a pipe, under cargo test).
            startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdOutput = INVALID_HANDLE_VALUE;
            startup.StartupInfo.hStdError = INVALID_HANDLE_VALUE;
            startup.lpAttributeList = attributes;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            let created = CreateProcessW(
                std::ptr::null(),
                line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                EXTENDED_STARTUPINFO_PRESENT | 0x400, // CREATE_UNICODE_ENVIRONMENT
                block.as_ptr().cast(),
                std::ptr::null(),
                &startup.StartupInfo,
                &mut info,
            );
            DeleteProcThreadAttributeList(attributes);
            assert_ne!(
                created,
                0,
                "CreateProcessW: {}",
                std::io::Error::last_os_error()
            );
            CloseHandle(info.hThread);
            self.process = Some(info.hProcess);
        }
    }

    /// Reads what the pseudoconsole rendered until `done` holds for the screen's text, or
    /// `timeout`. Returns whether it holds.
    fn wait(&mut self, timeout: Duration, done: impl Fn(&str) -> bool) -> bool {
        let start = Instant::now();
        loop {
            while let Ok(bytes) = self.output.try_recv() {
                self.screen.process(&bytes);
            }
            if done(&self.text()) {
                return true;
            }
            if start.elapsed() > timeout {
                return false;
            }
            if let Ok(bytes) = self.output.recv_timeout(Duration::from_millis(20)) {
                self.screen.process(&bytes);
            }
        }
    }

    fn text(&self) -> String {
        self.screen.screen().contents()
    }

    fn type_bytes(&self, bytes: &[u8]) {
        let mut written = 0u32;
        let ok = unsafe {
            WriteFile(
                self.input,
                bytes.as_ptr(),
                bytes.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        assert!(ok != 0 && written as usize == bytes.len(), "typing failed");
    }

    /// Waits up to `timeout` for the process to end; its exit code.
    fn exit(&mut self, timeout: Duration) -> Option<u32> {
        let process = self.process?;
        let start = Instant::now();
        loop {
            self.wait(Duration::ZERO, |_| false);
            if unsafe { WaitForSingleObject(process, 20) } == WAIT_OBJECT_0 {
                let mut code = 0u32;
                unsafe { GetExitCodeProcess(process, &mut code) };
                unsafe { CloseHandle(process) };
                self.process = None;
                self.wait(Duration::from_millis(100), |_| false);
                return Some(code);
            }
            if start.elapsed() > timeout {
                return None;
            }
        }
    }

    /// The process started last, while it runs.
    #[cfg(feature = "engine-wasm")]
    fn pid(&self) -> u32 {
        use windows_sys::Win32::System::Threading::GetProcessId;
        // SAFETY: a process handle this console holds.
        unsafe { GetProcessId(self.process.expect("a process")) }
    }

    /// Starts this test binary in the pseudoconsole for the not-a-test `name`, with `env` set.
    fn start_myself(&mut self, name: &str, env: &[(&str, &str)]) {
        let me = std::env::current_exe().unwrap();
        self.start(
            me.as_os_str(),
            &["--exact", name, "--nocapture", "--test-threads", "1"],
            env,
        );
    }

    /// The console's input and output modes now, from a process started in it for that.
    fn modes(&mut self) -> String {
        self.start_myself(
            "print_console_modes_when_asked",
            &[("SIG_CONSOLE_MODES", "1")],
        );
        assert_eq!(
            self.exit(Duration::from_secs(20)),
            Some(0),
            "{}",
            self.text()
        );
        let text = self.text();
        let line = text
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with("modes "))
            .unwrap_or_else(|| panic!("no modes printed\n{text}"));
        line.trim().to_owned()
    }
}

impl Drop for Console {
    fn drop(&mut self) {
        if let Some(process) = self.process.take() {
            unsafe {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
        unsafe {
            CloseHandle(self.input);
            ClosePseudoConsole(self.console);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[test]
fn sig_in_a_console_shows_the_page_opens_an_address_and_quits_restoring_the_console() {
    if !allowed() {
        return;
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = configure();
    let mut console = Console::new();
    let before = console.modes();
    // No `TERM` and no `--tui`: a console on stdout is the terminal on Windows.
    console.start(
        OsStr::new(env!("CARGO_BIN_EXE_sig")),
        &["demo://pattern"],
        &[],
    );
    let shown = console.wait(Duration::from_secs(30), |text| {
        text.contains("Demo") && text.contains("Animation page")
    });
    let log = || fs::read_to_string(dir.join("tui.log")).unwrap_or_default();
    assert!(
        shown,
        "the page did not show\n{}\ntui.log:\n{}",
        console.text(),
        log()
    );
    // The evidence, with --nocapture: what the console showed at each step.
    eprintln!("modes before sig: {before}\nthe page:\n{}", console.text());
    // The configuration is watched: a binding written into the file while sig runs is used
    // once sig has read it again (`z` does nothing before that).
    fs::write(
        dir.join("config.toml"),
        format!("{CONFIG}[keys]\n\"z\" = \"page.open demo://animation\"\n"),
    )
    .unwrap();
    let reloaded = Instant::now();
    let animation = |text: &str| {
        text.lines()
            .next()
            .is_some_and(|line| line.contains("Demo: animation"))
    };
    let mut bound = false;
    while !bound && reloaded.elapsed() < Duration::from_secs(15) {
        console.type_bytes(b"z");
        bound = console.wait(Duration::from_millis(500), animation);
    }
    assert!(
        bound,
        "the changed configuration was not read\n{}\ntui.log:\n{}",
        console.text(),
        log()
    );
    eprintln!(
        "z bound by the file after {:?}:\n{}",
        reloaded.elapsed(),
        console.text()
    );

    // `o` opens the address line, Enter opens what was typed.
    console.type_bytes(b"o");
    let editing = console.wait(Duration::from_secs(10), |text| {
        text.lines()
            .last()
            .is_some_and(|line| line.contains("demo://animation"))
    });
    assert!(
        editing,
        "`o` did not open the address line\n{}",
        console.text()
    );
    eprintln!("the address line:\n{}", console.text());
    console.type_bytes(b"demo://resize\r");
    // The new page's title, on the top line.
    let navigated = console.wait(Duration::from_secs(10), |text| {
        text.lines()
            .next()
            .is_some_and(|line| line.contains("Demo: resize"))
    });
    assert!(
        navigated,
        "typing an address did not open it\n{}",
        console.text()
    );
    eprintln!("the typed address:\n{}", console.text());

    console.type_bytes(b"q");
    let code = console.exit(Duration::from_secs(20));
    assert_eq!(
        code,
        Some(0),
        "sig did not quit cleanly\n{}\ntui.log:\n{}",
        console.text(),
        log()
    );
    let after = console.modes();
    eprintln!("sig exited with 0; modes after: {after}");
    assert_eq!(after, before, "the console's modes were not restored");
}

/// sig://help with the Wasm engine on Windows: forced into graphics (sixel, which the console
/// takes or drops), the page's guest runs and the tab loads without an error, which a report
/// asked for by setting `sig`'s report event (`Local\sighurt-report-<pid>`) says; then `q`
/// quits.
#[cfg(feature = "engine-wasm")]
#[test]
fn sig_help_runs_its_guest_and_the_report_event_asks_for_a_report() {
    use windows_sys::Win32::System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE};
    if !allowed() {
        return;
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = configure();
    let tui = "[tui]\nmode = \"graphics\"\ngraphics = \"sixel\"\n";
    fs::write(dir.join("config.toml"), format!("{CONFIG}{tui}")).unwrap();
    let report = std::env::temp_dir().join(format!("sig-console-{}.json", std::process::id()));
    let _ = fs::remove_file(&report);
    let mut console = Console::new();
    let path = report.to_str().unwrap();
    console.start(
        OsStr::new(env!("CARGO_BIN_EXE_sig")),
        &["--diagnostics", path, "sig://help"],
        &[],
    );
    let log = || fs::read_to_string(dir.join("tui.log")).unwrap_or_default();
    let name: Vec<u16> = format!("Local\\sighurt-report-{}", console.pid())
        .encode_utf16()
        .chain([0])
        .collect();
    // `main` makes the event before the frontend starts.
    let start = Instant::now();
    let event = loop {
        // SAFETY: a NUL-terminated name; the handle is checked.
        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if !event.is_null() || start.elapsed() > Duration::from_secs(30) {
            break event;
        }
        console.wait(Duration::from_millis(100), |_| false);
    };
    assert!(
        !event.is_null(),
        "no report event: {}\n{}\ntui.log:\n{}",
        std::io::Error::last_os_error(),
        console.text(),
        log()
    );
    // The report, without white space, until the help tab has loaded.
    let start = Instant::now();
    let mut last = String::new();
    let loaded = |json: &str| {
        let wasm = json.find("\"name\":\"wasm\"").map(|at| &json[at..]);
        let running = wasm.is_some_and(|rest| {
            let entry = rest[1..]
                .find("\"name\":")
                .map_or(rest, |end| &rest[..=end]);
            entry.contains("\"state\":\"running\"")
        });
        running
            && json.contains("\"exit\":false")
            && json.contains("\"loading\":false")
            && json.contains("\"error\":false")
            && !json.contains("\"error\":true")
    };
    while start.elapsed() < Duration::from_secs(30) {
        // SAFETY: the event just opened.
        unsafe { SetEvent(event) };
        console.wait(Duration::from_millis(500), |_| false);
        last = fs::read_to_string(&report)
            .unwrap_or_default()
            .split_whitespace()
            .collect();
        if loaded(&last) {
            break;
        }
    }
    // SAFETY: opened above, closed once.
    unsafe { CloseHandle(event) };
    assert!(
        loaded(&last),
        "no report of a loaded sig://help\nreport: {last}\n{}\ntui.log:\n{}",
        console.text(),
        log()
    );
    eprintln!("the report: {last}\nthe screen:\n{}", console.text());
    console.type_bytes(b"q");
    let code = console.exit(Duration::from_secs(20));
    assert_eq!(code, Some(0), "{}\ntui.log:\n{}", console.text(), log());
    let _ = fs::remove_file(&report);
}

/// An access violation in raw mode (not a panic, which the panic hook restores): the
/// unhandled-exception filter puts the console's modes back before the process ends.
#[test]
fn a_native_crash_in_raw_mode_puts_the_console_back() {
    if !allowed() {
        return;
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let mut console = Console::new();
    let before = console.modes();
    console.start_myself(
        "crash_in_raw_mode_when_asked",
        &[("SIG_CONSOLE_CRASH", "1")],
    );
    let raw = console.wait(Duration::from_secs(20), |text| text.contains("RAW"));
    assert!(
        raw,
        "the crashing process did not start\n{}",
        console.text()
    );
    let code = console.exit(Duration::from_secs(20));
    // STATUS_ACCESS_VIOLATION.
    assert_eq!(code, Some(0xC000_0005), "{}", console.text());
    let after = console.modes();
    eprintln!("modes before: {before}; after the crash: {after}");
    assert_eq!(after, before, "the console's modes were not put back");
}

#[test]
fn dump_prints_the_page_as_text_to_a_pipe() {
    if !allowed() {
        return;
    }
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    configure();
    let output = Command::new(env!("CARGO_BIN_EXE_sig"))
        .args(["--dump", "demo://pattern", "--width", "40"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let (out, err) = (
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(output.status.success(), "{:?}\n{err}", output.status);
    assert!(out.starts_with("Demo: pattern\n"), "{out}");
    assert!(out.contains("Animation page[1]"), "{out}");
    assert!(
        out.contains("References\n\n1. demo:animation\n2. demo:resize\n"),
        "{out}"
    );
    assert!(out.lines().all(|line| line.chars().count() <= 40), "{out}");
    assert!(!out.contains('\x1b'), "escape sequences in the dump");
    eprintln!("sig --dump demo://pattern --width 40:\n{out}");

    let output = Command::new(env!("CARGO_BIN_EXE_sig"))
        .args(["--dump", "demo://nope"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{err}");
    assert!(output.stdout.is_empty());
    assert!(
        err.starts_with("sig: ") && err.lines().count() == 1,
        "{err}"
    );
}
