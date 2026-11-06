//! `sig --tui` against the demo engine, in a pty, with a fake terminal on the master side.
//!
//! The fake terminal answers the probe like a terminal with kitty graphics, 10×20-pixel cells
//! and 80×24 cells, records everything `sig` writes, and types and resizes as a user would.
//! `sig` is this package's binary, built by cargo for the test with the features the test
//! requires (`frontend-tui`, `engine-demo`). It has no display: `WAYLAND_DISPLAY` and `DISPLAY`
//! are removed from its environment.
//!
//! Each test is bounded: a watchdog kills `sig`'s process group after [`LIMIT`], and dropping
//! the pty kills it too, so nothing is left running.

use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::OFlags;
use rustix::io::Errno;
use rustix::termios::{tcgetattr, tcsetwinsize, Termios, Winsize};
use sighurt_tui::term::{open_pty, termios_matches};

/// How long one test may take before the watchdog kills `sig`.
const LIMIT: Duration = Duration::from_secs(40);
/// `[tui] fps` in the test's configuration.
const FPS: u64 = 10;
/// The probe's last query (DA1): the fake terminal answers when it sees it.
const DA1: &[u8] = b"\x1b[c";
/// How long `sig` may take no input before a write to it gives up.
const STALL: Duration = Duration::from_secs(30);
/// Kitty graphics, cells of 10×20 pixels, and DA1. No kitty keyboard: keys arrive as a legacy
/// terminal sends them.
const REPLIES: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c";
/// The same with the kitty keyboard flags `sig` pushes (31), as kitty and Rio answer.
const KITTY_REPLIES: &[u8] = b"\x1b[?31u\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c";
/// Cells of 10×20 pixels and DA1 with sixel, and no kitty graphics reply.
const SIXEL_REPLIES: &[u8] = b"\x1b[6;20;10t\x1b[?62;4c";
/// [`REPLIES`] after a `q` the user typed while the terminal had not answered yet.
const Q_THEN_REPLIES: &[u8] = b"q\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c";

/// The history file a test's `sig` starts with.
#[derive(Clone, Copy)]
enum Seed<'a> {
    /// None.
    None,
    /// One that holds this.
    File(&'a str),
    /// A named pipe: `sig`'s reader waits on it until the test writes the file
    /// ([`Terminal::feed_history`]).
    Pipe,
}

/// `sig --tui` on the slave side of a pty, and the fake terminal on the master side.
struct Terminal {
    master: OwnedFd,
    slave: OwnedFd,
    child: Child,
    /// The termios before `sig` made the pty raw.
    original: Termios,
    /// Everything `sig` wrote.
    got: Vec<u8>,
    answered: bool,
    /// What the terminal answers the probe with.
    replies: &'static [u8],
    dir: PathBuf,
    stop: Option<mpsc::Sender<()>>,
}

impl Terminal {
    /// Starts `sig --tui --diagnostics DIR/diag.json ARGS` with a configuration of its own.
    fn start(name: &str, args: &[&str]) -> Self {
        Self::start_with(name, args, REPLIES)
    }

    /// [`Self::start`] for a terminal that answers the probe with `replies`.
    fn start_with(name: &str, args: &[&str], replies: &'static [u8]) -> Self {
        Self::start_in(name, args, replies, "")
    }

    /// [`Self::start_with`] with `tui` more lines in the configuration's `[tui]` table.
    fn start_in(name: &str, args: &[&str], replies: &'static [u8], tui: &str) -> Self {
        Self::start_seeded(name, args, replies, tui, Seed::None)
    }

    /// [`Self::start_in`] with `TERM=tmux-256color`, as in a tmux pane.
    fn start_in_tmux(name: &str, args: &[&str], replies: &'static [u8], tui: &str) -> Self {
        Self::start_as(name, args, replies, tui, Seed::None, "tmux-256color", false)
    }

    /// [`Self::start_in`] with `history` as the history file (`state/sighurt/history.toml`).
    fn start_seeded(
        name: &str,
        args: &[&str],
        replies: &'static [u8],
        tui: &str,
        history: Seed<'_>,
    ) -> Self {
        Self::start_as(name, args, replies, tui, history, "xterm-kitty", false)
    }

    /// [`Self::start`] with the pty as `sig`'s controlling terminal, in a session of its own, as
    /// in a terminal emulator: closing the master then sends it `SIGHUP`.
    fn start_controlling(name: &str, args: &[&str]) -> Self {
        Self::start_as(name, args, REPLIES, "", Seed::None, "xterm-kitty", true)
    }

    /// [`Self::start_seeded`] with `TERM` set to `term`, and the pty as the controlling terminal
    /// when `controlling`.
    fn start_as(
        name: &str,
        args: &[&str],
        replies: &'static [u8],
        tui: &str,
        history: Seed<'_>,
        term: &str,
        controlling: bool,
    ) -> Self {
        let dir = std::env::temp_dir().join(format!("sig-tui-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config/sighurt")).unwrap();
        // A home of its own: whatever sig starts (sig://terminal's shell) never sees the user's.
        fs::create_dir_all(dir.join("home")).unwrap();
        if !matches!(history, Seed::None) {
            fs::create_dir_all(dir.join("state/sighurt")).unwrap();
        }
        let history_file = dir.join("state/sighurt/history.toml");
        match history {
            Seed::None => {}
            Seed::File(text) => fs::write(&history_file, text).unwrap(),
            Seed::Pipe => rustix::fs::mknodat(
                rustix::fs::CWD,
                &history_file,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::from_raw_mode(0o600),
                0,
            )
            .unwrap(),
        }
        fs::write(
            dir.join("config/sighurt/config.toml"),
            format!(
                "version = 1\n[engines]\ndefault = \"demo\"\n[routing.demo]\nschemes = \
                 [\"demo\"]\n[tui]\nfps = {FPS}\n{tui}\n"
            ),
        )
        .unwrap();
        let (master, slave) = open_pty().unwrap();
        let flags = rustix::fs::fcntl_getfl(&master).unwrap();
        rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK).unwrap();
        tcsetwinsize(&slave, winsize(80, 24)).unwrap();
        let original = tcgetattr(&slave).unwrap();
        let stderr = fs::File::create(dir.join("stderr")).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_sig"));
        if controlling {
            // SAFETY: setsid(2) and ioctl(2) only, between fork and exec. The session's id is
            // the child's pid, which the watchdog kills as a process group.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        } else {
            command.process_group(0);
        }
        let child = command
            .arg("--tui")
            .arg("--diagnostics")
            .arg(dir.join("diag.json"))
            .args(args)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("HOME", dir.join("home"))
            .env("TERM", term)
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env_remove("SIG_DIAG")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(stderr)
            .spawn()
            .unwrap();
        let (stop, stopped) = mpsc::channel::<()>();
        let group = child.id() as i32;
        thread::spawn(move || {
            if stopped.recv_timeout(LIMIT) == Err(mpsc::RecvTimeoutError::Timeout) {
                // SAFETY: kill(2) on the child's own process group.
                unsafe { libc::kill(-group, libc::SIGKILL) };
            }
        });
        Self {
            master,
            slave,
            child,
            original,
            got: Vec::new(),
            answered: false,
            replies,
            dir,
            stop: Some(stop),
        }
    }

    /// Reads what `sig` wrote, answering the probe once, until `done` holds for what arrived
    /// from `since` on, or `timeout`. Returns whether it holds.
    fn wait(&mut self, since: usize, timeout: Duration, done: impl Fn(&[u8]) -> bool) -> bool {
        let start = Instant::now();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match rustix::io::read(&self.master, &mut buf) {
                Ok(n) if n > 0 => {
                    self.got.extend_from_slice(&buf[..n]);
                    if !self.answered && has(&self.got, DA1) {
                        self.answered = true;
                        self.answer();
                    }
                    if done(&self.got[since.min(self.got.len())..]) {
                        return true;
                    }
                    continue;
                }
                Ok(_) | Err(Errno::AGAIN) | Err(Errno::INTR) => {}
                // EIO: the slave side has no process left.
                Err(_) => return done(&self.got[since.min(self.got.len())..]),
            }
            // What arrived before the wait may already satisfy it, with nothing more to come.
            if start.elapsed() > timeout {
                return done(&self.got[since.min(self.got.len())..]);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Answers the probe with `replies`, reading what `sig` writes meanwhile, so neither side
    /// waits on a full pty however long the answer is.
    fn answer(&mut self) {
        let mut rest = self.replies;
        let mut buf = vec![0u8; 1 << 16];
        let mut progress = Instant::now();
        while !rest.is_empty() {
            match rustix::io::write(&self.master, rest) {
                Ok(n) => {
                    rest = &rest[n..];
                    progress = Instant::now();
                }
                Err(Errno::AGAIN) | Err(Errno::INTR) => {
                    if let Ok(n) = rustix::io::read(&self.master, &mut buf) {
                        self.got.extend_from_slice(&buf[..n]);
                    }
                    self.assert_taking(progress);
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("writing to the pty: {error}"),
            }
        }
    }

    /// Fails the test when `sig` has taken no input since `progress` for [`STALL`]: it is gone
    /// or stuck, and the pty would refuse the rest for ever.
    fn assert_taking(&self, progress: Instant) {
        assert!(
            progress.elapsed() < STALL,
            "sig took no more input for {STALL:?}\n{}",
            self.context()
        );
    }

    fn type_bytes(&self, mut bytes: &[u8]) {
        let mut progress = Instant::now();
        while !bytes.is_empty() {
            match rustix::io::write(&self.master, bytes) {
                Ok(n) => {
                    bytes = &bytes[n..];
                    progress = Instant::now();
                }
                Err(Errno::AGAIN) | Err(Errno::INTR) => {
                    self.assert_taking(progress);
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("writing to the pty: {error}"),
            }
        }
    }

    /// Writes `text` into the history file made with [`Seed::Pipe`], which `sig`'s reader is
    /// waiting on, and closes it.
    fn feed_history(&self, text: &str) {
        let pipe = rustix::fs::open(
            self.dir.join("state/sighurt/history.toml"),
            OFlags::WRONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .expect("sig is reading the history");
        assert_eq!(rustix::io::write(&pipe, text.as_bytes()), Ok(text.len()));
    }

    /// Resizes the pty and tells `sig`, as a terminal emulator does.
    fn resize(&self, cols: u16, rows: u16) {
        tcsetwinsize(&self.slave, winsize(cols, rows)).unwrap();
        // SAFETY: kill(2) on the child.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGWINCH) };
    }

    /// Waits for `sig` to exit, reading what it writes meanwhile.
    fn exit(&mut self) -> ExitStatus {
        let start = Instant::now();
        loop {
            self.wait(self.got.len(), Duration::from_millis(20), |_| false);
            if let Some(status) = self.child.try_wait().unwrap() {
                self.wait(self.got.len(), Duration::from_millis(50), |_| false);
                return status;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "sig did not exit"
            );
        }
    }

    /// What went wrong, for an assertion's message.
    fn context(&self) -> String {
        let read = |name: &str| fs::read_to_string(self.dir.join(name)).unwrap_or_default();
        format!(
            "stderr:\n{}\ntui.log:\n{}\nscreen (text):\n{}",
            read("stderr"),
            read("state/sighurt/tui.log"),
            printable(&self.got)
        )
    }

    /// The diagnostics' counter `name`, from the report `sig` wrote at exit.
    fn counter(&self, name: &str) -> u64 {
        let json = fs::read_to_string(self.dir.join("diag.json")).expect("diagnostics");
        let at = json.find(&format!("\"{name}\":")).expect(name) + name.len() + 3;
        let digits: String = json[at..]
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().unwrap()
    }

    /// The active tab (from 1) and the number of tabs, from a report `sig` writes on SIGUSR1.
    fn tabs(&mut self) -> (u64, usize) {
        let path = self.dir.join("diag.json");
        let _ = fs::remove_file(&path);
        // SAFETY: kill(2) on the child.
        unsafe { libc::kill(self.child.id() as i32, libc::SIGUSR1) };
        let start = Instant::now();
        loop {
            self.wait(self.got.len(), Duration::from_millis(20), |_| false);
            let json = fs::read_to_string(&path).unwrap_or_default();
            if let (Some(active), Some(tabs)) =
                (json.find("\"active_tab\":"), json.find("\"tabs\":"))
            {
                let digits: String = json[active + 13..]
                    .trim_start()
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                // Each tab's report has one "reloads".
                let count = json[tabs..].matches("\"reloads\"").count();
                if let Ok(active) = digits.parse() {
                    return (active, count);
                }
            }
            assert!(
                start.elapsed() < Duration::from_secs(5),
                "no report\n{}",
                self.context()
            );
        }
    }

    /// Asks for a report (SIGUSR1) until its JSON, without whitespace, satisfies `done`, for up
    /// to five seconds. Returns the last one.
    fn report_until(&mut self, done: impl Fn(&str) -> bool) -> String {
        let path = self.dir.join("diag.json");
        let start = Instant::now();
        let mut last = String::new();
        while start.elapsed() < Duration::from_secs(5) {
            let _ = fs::remove_file(&path);
            // SAFETY: kill(2) on the child.
            unsafe { libc::kill(self.child.id() as i32, libc::SIGUSR1) };
            let asked = Instant::now();
            while asked.elapsed() < Duration::from_secs(1) {
                self.wait(self.got.len(), Duration::from_millis(20), |_| false);
                let json = fs::read_to_string(&path).unwrap_or_default();
                if json.contains("\"tabs\":") {
                    last = json.split_whitespace().collect();
                    break;
                }
            }
            if done(&last) {
                break;
            }
        }
        last
    }

    /// The terminal is as it was: alternate screen left, termios back.
    fn assert_restored(&self) {
        assert!(
            has(&self.got, b"\x1b[?1049h"),
            "never entered the alternate screen"
        );
        let left = self.got.windows(8).rposition(|w| w == b"\x1b[?1049l");
        let entered = self.got.windows(8).rposition(|w| w == b"\x1b[?1049h");
        assert!(
            left > entered,
            "the alternate screen was not left\n{}",
            self.context()
        );
        let now = tcgetattr(&self.slave).unwrap();
        assert!(
            termios_matches(&now, &self.original),
            "termios not restored"
        );
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: kill(2) on the child's own process group.
        unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        let _ = self.child.wait();
        drop(self.stop.take());
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

fn winsize(cols: u16, rows: u16) -> Winsize {
    Winsize {
        ws_col: cols,
        ws_row: rows,
        ws_xpixel: cols * 10,
        ws_ypixel: rows * 20,
    }
}

fn has(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Whether `text` is on the screen. A space the grid diff left unchanged is `CSI C`, not `0x20`.
fn shows(bytes: &[u8], text: &str) -> bool {
    let mut shown = String::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            if bytes.get(i..i + 3) == Some(b"\x1b[C") {
                shown.push(' ');
                i += 3;
                continue;
            }
            i += 1;
            match bytes.get(i) {
                Some(b'[') => {
                    i += 1;
                    while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                        i += 1;
                    }
                    i += 1;
                }
                Some(b']' | b'_' | b'P') => {
                    i += 1;
                    while i < bytes.len()
                        && bytes[i] != 0x07
                        && bytes.get(i..i + 2) != Some(b"\x1b\\")
                    {
                        i += 1;
                    }
                }
                _ => {}
            }
            continue;
        }
        let byte = bytes[i];
        if byte.is_ascii_graphic() || byte == b' ' {
            shown.push(byte as char);
        }
        i += 1;
    }
    shown.contains(text)
}

/// A sixel image: `ESC P`, parameters of digits and semicolons, `q`, then `ESC \`.
/// The probe's `ESC P + q` is not one (`+` is not a parameter).
fn sixel_image(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b'P' {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        if !bytes[j].is_ascii_digit() {
            i += 2;
            continue;
        }
        while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
            j += 1;
        }
        if bytes.get(j) != Some(&b'q') {
            i += 2;
            continue;
        }
        if bytes[j + 1..].windows(2).any(|window| window == b"\x1b\\") {
            return true;
        }
        i = j + 1;
    }
    false
}

/// Control and payload of each kitty `a=T` chunk whose APC ended with ST (`ESC \`).
///
/// The header (`s=`, `c=`) is sent before the payload, and one `write` stops before the `ESC`
/// of ST. A reader that stops at the header snapshots a prefix. Under load that write is short,
/// the pty accepts the header and part of the payload, and a later complete frame of the same
/// page does not compare equal.
fn kitty_placements(bytes: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i] != 0x1b || bytes[i + 1] != b'_' || bytes[i + 2] != b'G' {
            i += 1;
            continue;
        }
        let control_at = i + 3;
        let Some(mark) = bytes[control_at..]
            .iter()
            .position(|&byte| byte == b';' || byte == 0x1b)
        else {
            break;
        };
        if bytes[control_at + mark] != b';' {
            i = control_at + mark;
            continue;
        }
        let control = &bytes[control_at..control_at + mark];
        let payload_at = control_at + mark + 1;
        let Some(end) = bytes[payload_at..].iter().position(|&byte| byte == 0x1b) else {
            break;
        };
        let st = payload_at + end;
        if bytes.get(st + 1) != Some(&b'\\') {
            i = st + 1;
            continue;
        }
        if control.starts_with(b"a=T,") {
            out.push((control, &bytes[payload_at..st]));
        }
        i = st + 2;
    }
    out
}

/// A kitty image transmission placed over `columns`×`rows` cells, `width`×`height` pixels.
/// The placement counts once its first chunk's APC has been terminated.
fn image(bytes: &[u8], columns: u16, rows: u16, width: u32, height: u32) -> bool {
    let size = format!("s={width},v={height},");
    let cells = format!("c={columns},r={rows},");
    kitty_placements(bytes)
        .into_iter()
        .any(|(control, _payload)| has(control, size.as_bytes()) && has(control, cells.as_bytes()))
}

/// The pixels of each finished kitty `a=T` chunk, in the order they were sent.
fn images(bytes: &[u8]) -> Vec<&[u8]> {
    kitty_placements(bytes)
        .into_iter()
        .map(|(_control, payload)| payload)
        .collect()
}

/// Standard base64 with padding, the encoding an OSC 52 clipboard carries.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    let mut index = 0;
    while index + 3 <= data.len() {
        let n = (u32::from(data[index]) << 16)
            | (u32::from(data[index + 1]) << 8)
            | u32::from(data[index + 2]);
        for shift in [18, 12, 6, 0] {
            out.push(ALPHABET[((n >> shift) & 0x3f) as usize] as char);
        }
        index += 3;
    }
    let rest = &data[index..];
    if let [a, b] = rest {
        let n = (u32::from(*a) << 16) | (u32::from(*b) << 8);
        for shift in [18, 12, 6] {
            out.push(ALPHABET[((n >> shift) & 0x3f) as usize] as char);
        }
        out.push('=');
    } else if let [a] = rest {
        let n = u32::from(*a) << 16;
        for shift in [18, 12] {
            out.push(ALPHABET[((n >> shift) & 0x3f) as usize] as char);
        }
        out.push_str("==");
    }
    out
}

/// `ESC ] 52 ; c ; <base64 of data> ESC \`.
fn osc52(data: &[u8]) -> Vec<u8> {
    let mut out = b"\x1b]52;c;".to_vec();
    out.extend(base64(data).into_bytes());
    out.extend_from_slice(b"\x1b\\");
    out
}

/// `ESC ] 52 ; c ; ? ESC \`, the clipboard query.
const OSC52_QUERY: &[u8] = b"\x1b]52;c;?\x1b\\";

/// The top line dropped `INSERT MODE`.
///
/// The diff paints only cells that changed, and a space that was already blank is `CSI C`,
/// which [`shows`] reads as a space. The cursor is placed on the first cell that changed,
/// so the cleared indicator is one run as long as `INSERT MODE`. A full rewrite of the
/// line has `[1/1]` and not the indicator.
fn insert_mode_left(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && !shows(bytes, "INSERT MODE")
        && (shows(bytes, "[1/1]") || shows(bytes, &" ".repeat("INSERT MODE".len())))
}

/// The printable text of `bytes`, image payloads left out, for messages.
fn printable(bytes: &[u8]) -> String {
    let mut out = String::new();
    for apc in bytes.split(|&byte| byte == 0x1b) {
        let apc = match apc.starts_with(b"_G") || apc.starts_with(b"P") {
            true => &apc[..apc.len().min(60)],
            false => apc,
        };
        out.extend(
            apc.iter()
                .filter(|b| b.is_ascii_graphic() || **b == b' ')
                .map(|&b| b as char),
        );
        out.push(' ');
    }
    let skip = out.len().saturating_sub(3000);
    out[skip..].to_owned()
}

#[test]
fn a_demo_page_is_an_image_in_the_page_rows_and_quitting_restores_the_terminal() {
    let started = Instant::now();
    let mut term = Terminal::start("main", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        has(got, b"\x1b[?1049h") && image(got, 80, 22, 800, 440) && has(got, b"[1/1]")
    });
    assert!(shown, "the page did not show\n{}", term.context());
    assert!(
        has(&term.got, b"Demo:"),
        "the top line has no title\n{}",
        term.context()
    );

    // `o` opens the address line (the terminal's default keys), Enter opens what was typed:
    // another page, so another image in the same place.
    let pattern = images(&term.got).last().unwrap().to_vec();
    let since = term.got.len();
    term.type_bytes(b"o");
    let editing = term.wait(since, Duration::from_secs(10), |got| {
        // The bottom line, `→ ` and the address, with the cursor shown.
        has(got, "\x1b[24H→".as_bytes()) && has(got, b"demo://pattern") && has(got, b"\x1b[?25h")
    });
    assert!(
        editing,
        "`o` did not open the address line\n{}",
        term.context()
    );
    let since = term.got.len();
    term.type_bytes(b"demo://resize\r");
    let navigated = term.wait(since, Duration::from_secs(10), |got| {
        image(got, 80, 22, 800, 440) && images(got).iter().any(|pixels| *pixels != pattern)
    });
    assert!(
        navigated,
        "typing an address did not open it\n{}",
        term.context()
    );

    let since = term.got.len();
    term.resize(100, 30);
    let resized = term.wait(since, Duration::from_secs(10), |got| {
        image(got, 100, 28, 1000, 560)
    });
    assert!(resized, "no image of the new size\n{}", term.context());

    term.type_bytes(b"q");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();

    let frames = term.counter("page_frames_presented");
    let seconds = started.elapsed().as_secs() + 1;
    assert!(frames >= 3, "{frames} frames presented");
    assert!(
        frames <= FPS * seconds,
        "{frames} frames in {seconds} s, above {FPS} a second"
    );
    assert_eq!(term.counter("frames_dropped"), 0, "{}", term.context());
}

/// Ladybird in the terminal: its frames are copied into the terminal's CPU target and sent as a
/// kitty image, like any engine's. Needs the shim (nix/build-ladybird-shim.sh); without it the
/// test says so and passes. Run with `--features frontend-tui,engine-demo,engine-ladybird`.
#[cfg(feature = "engine-ladybird")]
#[test]
fn a_ladybird_page_is_an_image_in_the_page_rows() {
    if sighurt_engine_ladybird::ffi::load(None).is_err() {
        eprintln!("skipped: no Ladybird shim (nix/build-ladybird-shim.sh)");
        return;
    }
    let page = std::env::temp_dir().join(format!("sig-tui-ladybird-{}.html", std::process::id()));
    fs::write(
        &page,
        "<!doctype html><title>Ladybird in a terminal</title>\
         <body style='margin:0;background:#ff0000'><h1>hello</h1>",
    )
    .unwrap();
    let url = format!("file://{}", page.display());
    let mut term = Terminal::start("ladybird", &["--engine", "ladybird", &url]);
    let shown = term.wait(0, Duration::from_secs(30), |got| {
        has(got, b"\x1b[?1049h")
            && image(got, 80, 22, 800, 440)
            && has(got, b"[1/1]")
            && has(got, b"Ladybird in a terminal")
    });
    assert!(shown, "the page did not show\n{}", term.context());
    term.type_bytes(b"q");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
    assert_eq!(term.counter("frames_dropped"), 0, "{}", term.context());
    let _ = fs::remove_file(&page);
}

#[test]
fn sigterm_quits_and_restores_the_terminal() {
    let started = Instant::now();
    let mut term = Terminal::start("sigterm", &["demo://animation"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    // An animation asks for every frame it can get: a second of it is at most `FPS` frames.
    thread::sleep(Duration::from_secs(1));
    // SAFETY: kill(2) on the child.
    unsafe { libc::kill(term.child.id() as i32, libc::SIGTERM) };
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
    assert!(Path::new(&term.dir.join("state/sighurt/tui.log")).exists());
    let frames = term.counter("page_frames_presented");
    let seconds = started.elapsed().as_secs() + 1;
    assert!(frames >= 2, "{frames} frames of an animation");
    assert!(
        frames <= FPS * seconds,
        "{frames} frames in {seconds} s, above {FPS} a second"
    );
}

/// A kitty placement is the frame's pixels only after ST. The header alone is not a frame:
/// `plan_write` sends it ahead of the payload, and a short write leaves a prefix.
#[test]
fn a_partial_kitty_placement_is_not_the_frames_pixels() {
    let partial = b"\x1b_Ga=T,f=32,o=z,t=d,s=800,v=440,i=1,c=80,r=22,C=1,q=2,m=1;eJzPREFIX";
    assert!(!image(partial, 80, 22, 800, 440));
    assert!(images(partial).is_empty());
    let mut done = partial.to_vec();
    done.extend_from_slice(b"\x1b\\");
    assert!(image(&done, 80, 22, 800, 440));
    assert_eq!(images(&done).as_slice(), [b"eJzPREFIX".as_slice()]);
    // A later chunk of the same image is not another placement.
    done.extend_from_slice(b"\x1b_Gq=2,m=0;TAIL\x1b\\");
    assert_eq!(images(&done).as_slice(), [b"eJzPREFIX".as_slice()]);
}

/// `text` as Rio sends it with the kitty keyboard protocol on: each key a press (with its text)
/// and a release, in `CSI u`, and Enter, Escape and Left as named keys.
fn kitty_keys(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let key = match ch {
            '\r' => "\x1b[13u\x1b[13;1:3u".to_owned(),
            '\x1b' => "\x1b[27u\x1b[27;1:3u".to_owned(),
            // Left, as kitty and Rio send it: the press bare, the release with its event type.
            '←' => "\x1b[D\x1b[1;1:3D".to_owned(),
            'A'..='Z' => {
                let (code, shifted) = (ch.to_ascii_lowercase() as u32, ch as u32);
                format!("\x1b[{code}:{shifted};2;{shifted}u\x1b[{code}:{shifted};2:3u")
            }
            _ => {
                let code = ch as u32;
                format!("\x1b[{code};1;{code}u\x1b[{code};1:3u")
            }
        };
        out.extend(key.into_bytes());
    }
    out
}

#[test]
fn kitty_keys_open_addresses_tabs_and_quit() {
    let mut term = Terminal::start_with("kitty-keys", &["demo://pattern"], KITTY_REPLIES);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440) && has(got, b"[1/1]")
    });
    assert!(shown, "the page did not show\n{}", term.context());
    let pattern = images(&term.got).last().unwrap().to_vec();

    // `o`, an address and Enter in one write: every key after `o` goes to the address line.
    let since = term.got.len();
    term.type_bytes(&kitty_keys("odemo://resize\r"));
    let navigated = term.wait(since, Duration::from_secs(10), |got| {
        images(got).iter().any(|pixels| *pixels != pattern)
    });
    assert!(navigated, "the address was not opened\n{}", term.context());

    // Left moves the cursor once per key press, not once more for its release: "pattrn",
    // Left, Left, "e" is "pattern".
    let since = term.got.len();
    term.type_bytes(&kitty_keys("odemo://pattrn←←e\r"));
    let back = term.wait(since, Duration::from_secs(10), |got| {
        images(got).last().is_some_and(|pixels| *pixels == pattern)
    });
    assert!(
        back,
        "the edited address was not demo://pattern (snapshot {} bytes, last {:?})\n{}",
        pattern.len(),
        images(&term.got[since..]).last().map(|pixels| pixels.len()),
        term.context()
    );

    // `t` opens a tab with the address line open; Escape closes it, `J` and `K` switch.
    term.type_bytes(&kitty_keys("t"));
    let start = Instant::now();
    while term.tabs() != (2, 2) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "`t` opened no tab\n{}",
            term.context()
        );
    }
    for (keys, active) in [("\x1bJ", 1), ("K", 2)] {
        term.type_bytes(&kitty_keys(keys));
        let start = Instant::now();
        while term.tabs().0 != active {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{keys:?} did not switch to tab {active}\n{}",
                term.context()
            );
        }
    }

    term.type_bytes(&kitty_keys("q"));
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
}

/// Escape enters INSERT MODE (`q` reaches the page). Escape leaves it, and then `q` quits.
#[test]
fn escape_enters_insert_mode_where_q_does_not_quit() {
    let mut term = Terminal::start_with("insert-mode", &["demo://pattern"], KITTY_REPLIES);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440) && has(got, b"[1/1]")
    });
    assert!(shown, "the page did not show\n{}", term.context());

    let since = term.got.len();
    term.type_bytes(&kitty_keys("\x1b"));
    let insert = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "INSERT MODE")
    });
    assert!(
        insert,
        "Escape did not show INSERT MODE\n{}",
        term.context()
    );
    // The report says what the top line does (`report_until` drops the whitespace).
    let indicator = |report: &str| report_value(report, "keymap_indicator");
    let report = term.report_until(|r| indicator(r) == "\"INSERTMODE\"");
    assert_eq!(indicator(&report), "\"INSERTMODE\"", "{report}");

    // In INSERT MODE `q` is not a binding: the page takes the letter, and sig stays up.
    let since = term.got.len();
    term.type_bytes(&kitty_keys("q"));
    // Spaces the diff left blank are `CSI C`, so the status is read with `shows`.
    let typed = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Typed into the page: q") && shows(got, "INSERT MODE")
    });
    assert!(
        typed,
        "q in INSERT MODE did not reach the page\n{}",
        term.context()
    );
    assert!(
        term.child.try_wait().unwrap().is_none(),
        "q quit in INSERT MODE\n{}",
        term.context()
    );

    let since = term.got.len();
    term.type_bytes(&kitty_keys("\x1b"));
    let left = term.wait(since, Duration::from_secs(10), insert_mode_left);
    assert!(left, "Escape did not leave INSERT MODE\n{}", term.context());
    let report = term.report_until(|r| indicator(r) == "null");
    assert_eq!(indicator(&report), "null", "{report}");

    term.type_bytes(&kitty_keys("q"));
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
}

/// `y y` copies the selected tab's address as OSC 52.
#[test]
fn y_y_writes_the_address_with_osc_52() {
    let mut term = Terminal::start("yank", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440) && has(got, b"demo://pattern")
    });
    assert!(shown, "the page did not show\n{}", term.context());
    // `Url` keeps `demo://pattern` as given: `demo` is not a special scheme, so no slash is added.
    let url = "demo://pattern";
    let since = term.got.len();
    term.type_bytes(b"yy");
    let copied = term.wait(since, Duration::from_secs(10), |got| {
        has(got, &osc52(url.as_bytes()))
    });
    assert!(
        copied,
        "y y did not write OSC 52 of {url} ({})\n{}",
        base64(url.as_bytes()),
        term.context()
    );
}

/// Ctrl+V in the address line queries OSC 52 and inserts the terminal's answer.
#[test]
fn ctrl_v_in_the_line_pastes_the_terminals_answer() {
    let mut term = Terminal::start("paste", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());

    let since = term.got.len();
    term.type_bytes(b"o");
    let editing = term.wait(since, Duration::from_secs(10), |got| {
        has(got, b"\x1b[24H") && has(got, b"demo://pattern")
    });
    assert!(
        editing,
        "`o` did not open the address line\n{}",
        term.context()
    );

    let since = term.got.len();
    // Ctrl+V, as a legacy terminal sends it.
    term.type_bytes(b"\x16");
    let asked = term.wait(since, Duration::from_secs(5), |got| has(got, OSC52_QUERY));
    assert!(
        asked,
        "Ctrl+V did not query the clipboard\n{}",
        term.context()
    );

    let pasted = "clip-text";
    term.type_bytes(&osc52(pasted.as_bytes()));
    let since = term.got.len();
    let inserted = term.wait(since, Duration::from_secs(5), |got| shows(got, pasted));
    assert!(
        inserted,
        "the line did not show {pasted:?}\n{}",
        term.context()
    );
    assert!(
        !shows(&term.got[since..], "demo://patternclip-text"),
        "the paste was appended to the address\n{}",
        term.context()
    );
}

/// A terminal that ignores an OSC 52 read (tmux with `set-clipboard off`, Alacritty) does not
/// stop the next Ctrl+V from asking again, and tmux's reply, which names no selection, answers it.
#[test]
fn ctrl_v_asks_again_after_an_unanswered_read() {
    let mut term = Terminal::start("paste-again", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"o");
    let editing = term.wait(since, Duration::from_secs(10), |got| {
        has(got, b"\x1b[24H") && has(got, b"demo://pattern")
    });
    assert!(
        editing,
        "`o` did not open the address line\n{}",
        term.context()
    );

    let since = term.got.len();
    term.type_bytes(b"\x16");
    let asked = term.wait(since, Duration::from_secs(5), |got| has(got, OSC52_QUERY));
    assert!(
        asked,
        "Ctrl+V did not query the clipboard\n{}",
        term.context()
    );
    // No answer. After the read's second, Ctrl+V asks again.
    term.wait(term.got.len(), Duration::from_millis(1500), |_| false);
    let since = term.got.len();
    term.type_bytes(b"\x16");
    let again = term.wait(since, Duration::from_secs(5), |got| has(got, OSC52_QUERY));
    assert!(again, "the second Ctrl+V did not ask\n{}", term.context());

    let pasted = "tmux-text";
    let mut reply = b"\x1b]52;;".to_vec();
    reply.extend(base64(pasted.as_bytes()).into_bytes());
    reply.extend_from_slice(b"\x1b\\");
    let since = term.got.len();
    term.type_bytes(&reply);
    let inserted = term.wait(since, Duration::from_secs(5), |got| shows(got, pasted));
    assert!(
        inserted,
        "the line did not show {pasted:?}\n{}",
        term.context()
    );
}

/// A key typed before the terminal answered the startup probe is not lost: `q` quits.
#[test]
fn a_key_typed_during_the_probe_is_kept() {
    let mut term = Terminal::start_with("early-key", &["demo://pattern"], Q_THEN_REPLIES);
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
}

/// Ctrl+C in the address line copies the line with OSC 52, and does not quit.
/// Typing in the address line completes from the history file: the rest of the address shows
/// after the cursor, Tab makes the line that exact address (Ctrl+C copies it to show it), and
/// Enter opens it.
#[test]
fn the_address_line_completes_from_the_history() {
    const URL: &str = "https://www.example.com/page";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let history = format!(
        "version = 1\n[[visits]]\nurl = \"{URL}\"\ntitle = \"Example\"\nlast = {now}\ncount = 2\n"
    );
    let mut term = Terminal::start_seeded(
        "history",
        &["demo://pattern"],
        REPLIES,
        "",
        Seed::File(&history),
    );
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"o");
    let editing = term.wait(since, Duration::from_secs(10), |got| {
        has(got, b"demo://pattern")
    });
    assert!(
        editing,
        "`o` did not open the address line\n{}",
        term.context()
    );
    let since = term.got.len();
    term.type_bytes(b"exa");
    let completed = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "mple.com/page")
    });
    assert!(
        completed,
        "no completion after the cursor\n{}",
        term.context()
    );
    let since = term.got.len();
    // Tab, then Ctrl+C, as a legacy terminal sends them.
    term.type_bytes(b"\t\x03");
    let taken = term.wait(since, Duration::from_secs(10), |got| {
        has(got, &osc52(URL.as_bytes()))
    });
    assert!(taken, "Tab did not make the line {URL}\n{}", term.context());
    term.type_bytes(b"\r");
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(500) {
        term.wait(term.got.len(), Duration::from_millis(20), |_| false);
    }
    term.type_bytes(b"q");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
    // Only http and https pages are visits; the demo engine's are not, and the file stays.
    assert_eq!(term.counter("visits_recorded"), 0);
    let kept = fs::read_to_string(term.dir.join("state/sighurt/history.toml")).unwrap();
    assert!(kept.contains(URL), "{kept}");
}

#[test]
fn ctrl_c_in_the_line_copies_it() {
    let mut term = Terminal::start("copy-line", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());

    let since = term.got.len();
    term.type_bytes(b"o");
    let editing = term.wait(since, Duration::from_secs(10), |got| {
        has(got, b"\x1b[24H") && has(got, b"demo://pattern")
    });
    assert!(
        editing,
        "`o` did not open the address line\n{}",
        term.context()
    );

    let text = "copied-line";
    let since = term.got.len();
    term.type_bytes(text.as_bytes());
    let typed = term.wait(since, Duration::from_secs(10), |got| shows(got, text));
    assert!(typed, "the line did not show {text:?}\n{}", term.context());

    let since = term.got.len();
    // Ctrl+C, as a legacy terminal sends it. On the page this quits; in the line it copies.
    term.type_bytes(b"\x03");
    let copied = term.wait(since, Duration::from_secs(10), |got| {
        has(got, &osc52(text.as_bytes()))
    });
    assert!(copied, "Ctrl+C did not copy the line\n{}", term.context());
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        term.wait(term.got.len(), Duration::from_millis(20), |_| false);
        assert!(
            term.child.try_wait().unwrap().is_none(),
            "Ctrl+C quit from the address line\n{}",
            term.context()
        );
    }
}

#[test]
fn a_kitty_address_typed_in_one_write_opens_google() {
    // This build has no servo, so the navigation fails. The failure names the address that was
    // submitted: `https://oogle.com` would mean the `g` reached the page instead of the line.
    let mut term = Terminal::start_with("kitty-google", &["demo://pattern"], KITTY_REPLIES);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440) && has(got, b"[1/1]")
    });
    assert!(shown, "the page did not show\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(&kitty_keys("ogoogle.com\r"));
    let opened = term.wait(since, Duration::from_secs(10), |got| {
        has(got, b"https://google.com")
    });
    assert!(opened, "google.com was not opened\n{}", term.context());
    let tail = &term.got[since..];
    assert!(
        !has(tail, b"https://oogle.com"),
        "the g was lost\n{}",
        term.context()
    );
}

#[test]
fn a_legacy_terminal_types_an_address_in_one_write() {
    let mut term = Terminal::start("legacy-keys", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    let pattern = images(&term.got).last().unwrap().to_vec();
    let since = term.got.len();
    term.type_bytes(b"odemo://resize\r");
    let navigated = term.wait(since, Duration::from_secs(10), |got| {
        images(got).iter().any(|pixels| *pixels != pattern)
    });
    assert!(navigated, "the address was not opened\n{}", term.context());
    term.type_bytes(b"q");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
}

#[test]
fn text_mode_shows_the_page_as_text_and_tab_enter_follow_a_link_and_fill_a_field() {
    let mut term = Terminal::start_in("text", &["demo://pattern"], REPLIES, "mode = \"text\"");
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        shows(got, "Demo: pattern") && shows(got, "Animation page[1]") && shows(got, "TEXT")
    });
    assert!(shown, "no page text\n{}", term.context());
    assert!(
        images(&term.got).is_empty(),
        "text mode sent an image\n{}",
        term.context()
    );

    // Tab focuses the first link, Enter follows it.
    let since = term.got.len();
    term.type_bytes(b"\t\r");
    let followed = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Demo: animation")
    });
    assert!(
        followed,
        "Tab, Enter did not follow the link\n{}",
        term.context()
    );

    // Three more Tabs reach the field (link, link, field). In NORMAL, Enter opens no editor:
    // a notice says the mode types nothing into the page, and a key closes it.
    let since = term.got.len();
    term.type_bytes(b"\t\t\t\r");
    let refused = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "This keymap mode doesn't type into the page")
    });
    assert!(
        refused,
        "no notice for the field in NORMAL\n{}",
        term.context()
    );
    term.type_bytes(b" ");
    // In INSERT, Enter edits it, `q` is typed into it, not run, and Enter sets it.
    let since = term.got.len();
    term.type_bytes(b"\x1b");
    let insert = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "INSERT MODE")
    });
    assert!(
        insert,
        "Escape did not show INSERT MODE\n{}",
        term.context()
    );
    let since = term.got.len();
    term.type_bytes(b"\r");
    let editing = term.wait(since, Duration::from_secs(10), |got| shows(got, "Edit:"));
    assert!(editing, "no field editor\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"zq\r");
    let set = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Typed into the page: zq")
    });
    assert!(
        set,
        "the field's value did not reach the page\n{}",
        term.context()
    );

    // Back in NORMAL, `q` quits.
    let since = term.got.len();
    term.type_bytes(b"\x1b");
    let left = term.wait(since, Duration::from_secs(10), |got| {
        !got.is_empty() && !shows(got, "INSERT MODE")
    });
    assert!(left, "Escape did not leave INSERT MODE\n{}", term.context());
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
}

/// With a field focused in graphics, NORMAL gives the page no key and no paste; INSERT gives it
/// both, `q` and Ctrl-C included; back in NORMAL, Ctrl-C quits.
#[test]
fn graphics_mode_gives_a_focused_field_keys_and_pastes_only_in_insert() {
    let mut term = Terminal::start_in(
        "text-field",
        &["demo://pattern"],
        REPLIES,
        "mode = \"text\"",
    );
    let shown = term.wait(0, Duration::from_secs(15), |got| shows(got, "TEXT"));
    assert!(shown, "no text mode\n{}", term.context());
    term.type_bytes(b"\t\r");
    let since = term.got.len();
    let followed = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Demo: animation")
    });
    assert!(followed, "link not followed\n{}", term.context());
    // The field's editor opens in INSERT.
    term.type_bytes(b"\t\t\t");
    let since = term.got.len();
    term.type_bytes(b"\x1b");
    let insert = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "INSERT MODE")
    });
    assert!(
        insert,
        "Escape did not show INSERT MODE\n{}",
        term.context()
    );
    term.type_bytes(b"\r");
    let since = term.got.len();
    let editing = term.wait(since, Duration::from_secs(10), |got| shows(got, "Edit:"));
    assert!(editing, "no field editor\n{}", term.context());
    term.type_bytes(b"zq\r");
    let since = term.got.len();
    let set = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Typed into the page: zq")
    });
    assert!(set, "field not set\n{}", term.context());

    // Back in NORMAL, and in graphics with the field focused: a letter no binding has and a
    // paste reach nothing.
    term.type_bytes(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    let since = term.got.len();
    term.type_bytes(b"m");
    let graphics = term.wait(since, Duration::from_secs(10), |got| shows(got, "GFX"));
    assert!(graphics, "`m` did not show graphics\n{}", term.context());
    term.type_bytes(b"z\x1b[200~normal\x1b[201~");
    thread::sleep(Duration::from_millis(300));
    // INSERT: a paste and `q` go to the page's field, and `q` does not quit: sig is still up a
    // second on. What reached the page shows that NORMAL's letter and paste did not.
    let since = term.got.len();
    term.type_bytes(b"\x1b");
    let insert = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "INSERT MODE")
    });
    assert!(
        insert,
        "Escape did not show INSERT MODE\n{}",
        term.context()
    );
    term.type_bytes(b"\x1b[200~P\x1b[201~q");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        term.wait(term.got.len(), Duration::from_millis(20), |_| false);
        assert!(
            term.child.try_wait().unwrap().is_none(),
            "q quit in INSERT\n{}",
            term.context()
        );
    }
    // Ctrl-C is the page's in INSERT too; in NORMAL it quits.
    term.type_bytes(b"\x1b");
    thread::sleep(Duration::from_millis(200));
    term.type_bytes(b"\x03");
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
    // The page's title ends with what it was given (the status line is redrawn as a diff, so
    // the saved session says it): INSERT's paste and q, and nothing of NORMAL's.
    let session = fs::read_to_string(term.dir.join("state/sighurt/session.toml"));
    assert!(
        session.as_deref().is_ok_and(|s| s.contains("zqPq\"")),
        "INSERT's paste and q did not reach the page, or NORMAL's did: {session:?}\n{}",
        term.context()
    );
}

/// Decision 105: text mode gives the engine a real page area and takes its frames, but never
/// encodes or sends one. (The demo engine paints only into a view with room and a permit, so a
/// presented frame shows both; it cannot model a page that has text only after a frame.)
#[test]
fn text_mode_takes_the_engines_frames_and_sends_none() {
    let started = Instant::now();
    let mut term = Terminal::start_in(
        "text-frames",
        &["demo://animation"],
        REPLIES,
        "mode = \"text\"",
    );
    let shown = term.wait(0, Duration::from_secs(15), |got| shows(got, "TEXT"));
    assert!(shown, "no text mode\n{}", term.context());
    std::thread::sleep(Duration::from_secs(2));
    let _ = term.tabs();
    let frames = term.counter("page_frames_presented");
    // Text mode takes at most 2 frames a second (`TEXT_FPS`), from process start through the
    // report. Four seconds covers a quiet startup plus this wait; a loaded machine stretches
    // the startup, and the same cap follows the clock instead of failing a longer run.
    let seconds = started.elapsed().as_secs() + 1;
    assert!(
        frames > 0 && frames <= 2 * seconds.max(4),
        "text mode presented {frames} frames in {seconds} s\n{}",
        term.context()
    );
    assert_eq!(term.counter("encoded_bytes"), 0, "{}", term.context());
    assert!(images(&term.got).is_empty(), "{}", term.context());
}

#[test]
fn a_click_follows_a_link_in_text_mode() {
    let mut term = Terminal::start_in(
        "text-click",
        &["demo://pattern"],
        REPLIES,
        "mode = \"text\"",
    );
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        shows(got, "Animation page[1]")
    });
    assert!(shown, "no page text\n{}", term.context());
    // The link is the sixth line of the page, which starts on the second row.
    let since = term.got.len();
    term.type_bytes(b"\x1b[<0;3;7M\x1b[<0;3;7m");
    let followed = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Demo: animation")
    });
    assert!(
        followed,
        "the click did not follow the link\n{}",
        term.context()
    );
}

/// The value of the first `"key":` in a report, as JSON. An object (flat: its values have no
/// commas) comes with its fields sorted: serde_json writes them in the order the struct declares
/// them when a dependency turns on its `preserve_order` (Servo's script crate does, so a `sig`
/// with Servo does), and sorted otherwise.
fn report_value(report: &str, key: &str) -> String {
    let name = format!("\"{key}\":");
    let Some(at) = report.find(&name) else {
        return String::new();
    };
    let rest = &report[at + name.len()..];
    match rest.find('}') {
        Some(end) if rest.starts_with('{') => {
            let mut fields: Vec<&str> = rest[1..end].split(',').collect();
            fields.sort_unstable();
            format!("{{{}}}", fields.join(","))
        }
        _ => rest.split([',', '}']).next().unwrap_or("").to_owned(),
    }
}

/// The first tab's search in a report, as JSON: `null` with none.
fn tab_find(report: &str) -> String {
    report_value(report, "find")
}

/// `/` opens the find field in text mode; typing searches the page's text, Enter keeps the
/// search for `n` and `N`, which wrap around, and Escape in the field ends it.
#[test]
fn text_mode_finds_in_the_pages_text_with_the_find_field() {
    let mut term = Terminal::start_in("find-text", &["demo://pattern"], REPLIES, "mode = \"text\"");
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        shows(got, "Animation page[1]") && shows(got, "TEXT")
    });
    assert!(shown, "no page text\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"/page");
    let counted = term.wait(since, Duration::from_secs(10), |got| shows(got, " of "));
    assert!(counted, "no count on the bottom line\n{}", term.context());
    let at = |current: u32| {
        format!("{{\"current\":{current},\"found\":true,\"supported\":true,\"total\":3}}")
    };
    let report = term.report_until(|r| tab_find(r) == at(1));
    assert_eq!(tab_find(&report), at(1), "{}", term.context());
    // Enter keeps the search; `n` and `N` walk it, wrapping.
    term.type_bytes(b"\r");
    term.type_bytes(b"n");
    let report = term.report_until(|r| tab_find(r) == at(2));
    assert_eq!(tab_find(&report), at(2), "{}", term.context());
    term.type_bytes(b"NN");
    let report = term.report_until(|r| tab_find(r) == at(3));
    assert_eq!(tab_find(&report), at(3), "{}", term.context());
    // Upper case: case-sensitive, no match.
    term.type_bytes(b"/Page\r");
    let none = "{\"current\":null,\"found\":false,\"supported\":true,\"total\":0}";
    let report = term.report_until(|r| tab_find(r) == none);
    assert_eq!(tab_find(&report), none, "{}", term.context());
    // Escape in the field ends the search.
    term.type_bytes(b"/");
    term.wait(term.got.len(), Duration::from_millis(300), |_| false);
    term.type_bytes(b"\x1b");
    let report = term.report_until(|r| tab_find(r) == "null");
    assert_eq!(tab_find(&report), "null", "{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"n");
    let told = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Nothing to find")
    });
    assert!(told, "n without a search says so\n{}", term.context());
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
}

/// In graphics mode the engine searches; switching to text ends the search and says so.
#[test]
fn graphics_mode_finds_through_the_engine_and_m_ends_the_search() {
    let mut term = Terminal::start("find-gfx", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440) && shows(got, "GFX")
    });
    assert!(shown, "no image\n{}", term.context());
    term.type_bytes(b":page.find page\r");
    let at = |current: u32| {
        format!("{{\"current\":{current},\"found\":true,\"supported\":true,\"total\":3}}")
    };
    let report = term.report_until(|r| tab_find(r) == at(1));
    assert_eq!(tab_find(&report), at(1), "{}", term.context());
    term.type_bytes(b"n");
    let report = term.report_until(|r| tab_find(r) == at(2));
    assert_eq!(tab_find(&report), at(2), "{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"m");
    let told = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Searches in pages ended")
    });
    assert!(told, "no notice\n{}", term.context());
    let report = term.report_until(|r| tab_find(r) == "null");
    assert_eq!(tab_find(&report), "null", "{}", term.context());
}

#[test]
fn m_switches_between_graphics_and_text() {
    let mut term = Terminal::start("toggle", &["demo://pattern"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440) && shows(got, "GFX")
    });
    assert!(shown, "no image\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"m");
    let text = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Animation page[1]") && shows(got, "TEXT")
    });
    assert!(text, "`m` did not show text\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(b"m");
    let graphics = term.wait(since, Duration::from_secs(10), |got| {
        image(got, 80, 22, 800, 440) && shows(got, "GFX")
    });
    assert!(
        graphics,
        "`m` did not show graphics again\n{}",
        term.context()
    );
}

#[test]
fn sixel_is_sent_when_the_terminal_has_sixel_and_no_kitty_graphics() {
    let mut term = Terminal::start_with("sixel", &["demo://pattern"], SIXEL_REPLIES);
    // 80×22 cells of 10×20 pixels is 800×440, and the sixel height rounds down to a band.
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        sixel_image(got) && has(got, b"\"1;1;800;438") && shows(got, "GFX")
    });
    assert!(shown, "no sixel image\n{}", term.context());
    assert!(
        !has(&term.got, b"_Ga=T"),
        "a kitty image was sent\n{}",
        term.context()
    );
    // tmux 3.8 writes a sixel through at once but redraws a synchronized pane's lines when
    // the update ends, over the image. No sixel starts inside a synchronized update.
    let mut synced = false;
    for (at, window) in term.got.windows(8).enumerate() {
        if window == b"\x1b[?2026h" {
            synced = true;
        } else if window == b"\x1b[?2026l" {
            synced = false;
        } else if window.starts_with(b"\x1bP0;2q") {
            assert!(
                !synced,
                "a sixel at byte {at} is inside a synchronized update"
            );
        }
    }
    // Every register is defined: tmux re-encodes an undefined one as `#n;0;0;0;0`, and Rio
    // drops an image that has one.
    assert!(
        has(&term.got, b"#0;2;0;0;0#1;2;0;0;20#"),
        "{}",
        term.context()
    );
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
    let json = fs::read_to_string(term.dir.join("diag.json")).unwrap_or_default();
    assert!(
        json.contains("graphics=sixel"),
        "capabilities did not say sixel: {json}\n{}",
        term.context()
    );
    assert!(term.counter("encoded_bytes") > 0, "{}", term.context());
    term.assert_restored();
}

#[test]
fn graphics_none_sends_no_sixel() {
    let mut term = Terminal::start_in(
        "sixel-none",
        &["demo://pattern"],
        SIXEL_REPLIES,
        "graphics = \"none\"",
    );
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        shows(got, "TEXT") && shows(got, "Demo: pattern")
    });
    assert!(shown, "no text page\n{}", term.context());
    assert!(
        !sixel_image(&term.got),
        "graphics none sent a sixel\n{}",
        term.context()
    );
    assert!(
        !has(&term.got, b"_Ga=T"),
        "graphics none sent a kitty image\n{}",
        term.context()
    );
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
    assert_eq!(term.counter("encoded_bytes"), 0, "{}", term.context());
    term.assert_restored();
}

/// Where each finished sixel image is in `bytes`: from its `ESC P 0;2 q` to after its `ESC \`.
fn sixel_spans(bytes: &[u8]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(start) = bytes[at..]
        .windows(6)
        .position(|window| window == b"\x1bP0;2q")
    {
        let start = at + start;
        let Some(end) = bytes[start..].windows(2).position(|w| w == b"\x1b\\") else {
            break;
        };
        let end = start + end + 2;
        spans.push((start, end));
        at = end;
    }
    spans
}

/// The first page row written with spaces, as the blank before a sixel writes it, for a
/// terminal `cols` wide.
fn blank_row(cols: usize) -> Vec<u8> {
    let mut row = b"\x1b[2;1H".to_vec();
    row.extend(std::iter::repeat_n(b' ', cols));
    row
}

/// `[tui]` lines for the sixel tests: F2 is `tab.next`, F5 `page.reload`, and nothing else is
/// bound (the example keymap's modes are not these tests' business).
const SIXEL_KEYS: &str =
    "[input]\nkeymap = \"bindings\"\n[keys]\n\"f2\" = \"tab.next\"\n\"f5\" = \"page.reload\"\n\"q\" = \"browser.quit\"";
/// The end of a synchronized update, then the cursor saved and moved to the page's first row
/// as a sixel frame begins: a blank right before the image.
const SYNCED_THEN_SIXEL: &[u8] = b"\x1b[?2026l\x1b[s\x1b[2;1H";
/// F2 and F5 as a legacy terminal sends them.
const F2: &[u8] = b"\x1bOQ";
const F5: &[u8] = b"\x1b[15~";

/// tmux frees the sixel it stores when a cell is written over it, then redraws the pane: the
/// rows show black until the next image has arrived. So only the first image of a layout is
/// preceded by the blank (spaces, in a synchronized update that ends before the image); the
/// next ones go over it as they are. A resize is a new layout: blank again.
#[test]
fn sixel_frames_go_over_each_other_and_only_a_new_layout_is_blanked_first() {
    let mut term = Terminal::start_in(
        "sixel-over",
        &["demo://animation"],
        SIXEL_REPLIES,
        SIXEL_KEYS,
    );
    // More frames than tmux's stack may hold: outside tmux none of them is blanked.
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        sixel_spans(got).len() >= 12
    });
    assert!(shown, "fewer than 12 sixel frames\n{}", term.context());
    let spans = sixel_spans(&term.got);
    let blank = blank_row(80);
    let before = &term.got[..spans[0].0];
    assert!(
        has(before, &blank) && before.ends_with(SYNCED_THEN_SIXEL),
        "the first sixel is not preceded by the blank in a synchronized update\n{}",
        term.context()
    );
    for pair in spans.windows(2) {
        let between = &term.got[pair[0].1..pair[1].0];
        assert!(
            !has(between, &blank) && !between.ends_with(SYNCED_THEN_SIXEL),
            "a sixel for the same layout was blanked first: {:?}\n{}",
            printable(between),
            term.context()
        );
    }
    // 100×30 cells: a new layout, blanked once, then frames over each other again.
    let resized = term.got.len();
    term.resize(100, 30);
    let shown = term.wait(resized, Duration::from_secs(15), |got| {
        has(got, b"\"1;1;1000;558") && sixel_spans(got).len() >= 3
    });
    assert!(
        shown,
        "no sixel frames after the resize\n{}",
        term.context()
    );
    let after = &term.got[resized..];
    let spans = sixel_spans(after);
    let blank = blank_row(100);
    assert!(
        has(&after[..spans[0].0], b"\x1b[2J") || has(&after[..spans[0].0], &blank),
        "the first sixel after the resize is not preceded by a blank\n{}",
        term.context()
    );
    for pair in spans.windows(2) {
        assert!(
            !has(&after[pair[0].1..pair[1].0], &blank),
            "a sixel after the resize was blanked first\n{}",
            term.context()
        );
    }
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
}

/// A frame whose sixel is the picture already on the page rows is not written again.
#[test]
fn a_sixel_frame_that_is_the_picture_on_the_rows_is_not_written() {
    let mut term = Terminal::start_in("sixel-same", &["demo://pattern"], SIXEL_REPLIES, SIXEL_KEYS);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        !sixel_spans(got).is_empty()
    });
    assert!(shown, "no sixel\n{}", term.context());
    let presented = |report: &str| {
        let at = report.find("\"page_frames_presented\":").map(|at| at + 24);
        at.and_then(|at| {
            report[at..]
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse::<u64>()
                .ok()
        })
        .unwrap_or(0)
    };
    // Settled: the page drew once for the layout (and once more when it took the keyboard).
    thread::sleep(Duration::from_millis(500));
    let report = term.report_until(|_| true);
    let before = presented(&report);
    let from = term.got.len();
    // The demo engine paints the pattern again, pixel for pixel.
    term.type_bytes(F5);
    let report = term.report_until(|r| presented(r) > before);
    assert!(
        presented(&report) > before,
        "the reload presented no frame\n{report}\n{}",
        term.context()
    );
    term.wait(term.got.len(), Duration::from_millis(500), |_| false);
    assert!(
        sixel_spans(&term.got[from..]).is_empty() && !has(&term.got[from..], &blank_row(80)),
        "the same picture was written again\n{}",
        term.context()
    );
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
}

/// A tab switch writes the shown tab's frame over the hidden tab's sixel, with no blank in
/// between. Switches faster than the sixel frame interval (200 ms) are paced: no image, and no
/// blank of one, comes sooner than that after the image before (250 ms for a hidden tab's: Rio
/// lost its GPU device while tabs were switched at key repeat, each image blanked a moment
/// after it came).
#[test]
fn a_tab_switch_writes_over_the_hidden_tabs_sixel_and_a_storm_of_them_is_paced() {
    let mut term = Terminal::start_in(
        "sixel-tabs",
        &["demo://animation", "demo://pattern", "demo://resize"],
        SIXEL_REPLIES,
        SIXEL_KEYS,
    );
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        !sixel_spans(got).is_empty()
    });
    assert!(shown, "no sixel\n{}", term.context());
    let (active, tabs) = term.tabs();
    assert_eq!((active, tabs), (1, 3), "{}", term.context());
    // Each tab is shown once: a hidden tab is given no frames, so its first one comes after
    // the blank. Then from the resize page (still) back to the animation, which has a frame
    // for this layout: it goes over the resize page's sixel in the same write.
    for tab in [2, 3, 1] {
        term.wait(term.got.len(), Duration::from_millis(400), |_| false);
        let from = term.got.len();
        term.type_bytes(F2);
        let switched = term.wait(from, Duration::from_secs(5), |got| {
            !sixel_spans(got).is_empty()
        });
        assert!(
            switched,
            "no sixel after the switch to tab {tab}\n{}",
            term.context()
        );
        assert_eq!(term.tabs().0, tab, "{}", term.context());
        if tab == 1 {
            let after = &term.got[from..];
            let first = sixel_spans(after)[0];
            assert!(
                !has(&after[..first.0], &blank_row(80)),
                "the switch blanked the rows before the shown tab's frame\n{}",
                term.context()
            );
        }
    }
    // A storm: 40 switches 25 ms apart, a second in all.
    let storm = term.got.len();
    let started = Instant::now();
    for _ in 0..40 {
        term.type_bytes(F2);
        term.wait(term.got.len(), Duration::from_millis(25), |_| false);
    }
    let lasted = started.elapsed();
    let during = sixel_spans(&term.got[storm..]).len();
    let most = (lasted.as_millis() / 200 + 2) as usize;
    assert!(
        during <= most,
        "{during} sixels in {lasted:?} of tab switches, more than {most}\n{}",
        term.context()
    );
    // Every tab has a frame for this layout: each goes over the one before, never a blank.
    assert!(
        !has(&term.got[storm..], &blank_row(80)),
        "the rows were blanked between tabs that all have frames\n{}",
        term.context()
    );
    // Settled: the shown tab's frame is the last thing on the rows, after any blank.
    let blank = blank_row(80);
    let shown = term.wait(storm, Duration::from_secs(5), |got| {
        let last_blank = got
            .windows(blank.len())
            .rposition(|w| w == blank.as_slice());
        sixel_spans(got)
            .last()
            .is_some_and(|&(start, _)| last_blank.is_none_or(|at| at < start))
    });
    let tail = &term.got[storm..];
    let last_blank = tail
        .windows(blank.len())
        .rposition(|w| w == blank.as_slice());
    let spans = sixel_spans(tail);
    assert!(
        shown,
        "no sixel once the switches stopped: last blank at {last_blank:?}, sixels at {:?}, {} bytes\n{}",
        spans,
        tail.len(),
        printable(&tail[last_blank.unwrap_or(0)..])
    );
    assert!(term.running(), "{}", term.context());
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
}

/// Inside tmux (TERM) too, a page that keeps changing has its sixels written over each other
/// with no blank: a blank every few frames would be a blink each time, and spaces over a sixel
/// Rio shows are what loses Rio its GPU device (decision 133).
#[test]
fn inside_tmux_a_changing_page_is_not_blanked_either() {
    let mut term = Terminal::start_in_tmux(
        "sixel-stack",
        &["demo://animation"],
        SIXEL_REPLIES,
        SIXEL_KEYS,
    );
    let shown = term.wait(0, Duration::from_secs(20), |got| {
        sixel_spans(got).len() >= 12
    });
    assert!(shown, "fewer than 12 sixel frames\n{}", term.context());
    let spans = sixel_spans(&term.got);
    let blank = blank_row(80);
    for pair in spans.windows(2) {
        assert!(
            !has(&term.got[pair[0].1..pair[1].0], &blank),
            "a changing page was blanked inside tmux\n{}",
            term.context()
        );
    }
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
}

/// Where `bytes` writes a cell or erases on rows `first..=last` (1-based): the offsets. Escape
/// strings (sixel, OSC, APC) and cursor moves are not writes. Follows the cursor through `CUP`,
/// `VPA`, `CUU`/`CUD`/`CNL`/`CPL`, save and restore, line feeds and reverse index.
fn page_row_writes(bytes: &[u8], first: u32, last: u32) -> Vec<usize> {
    let (mut row, mut saved, mut at, mut found) = (1u32, 1u32, 0usize, Vec::new());
    let on_page = |row: u32| (first..=last).contains(&row);
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == 0x1b {
            match bytes.get(at + 1) {
                Some(b'[') => {
                    let start = at + 2;
                    let mut end = start;
                    while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                        end += 1;
                    }
                    let Some(&final_byte) = bytes.get(end) else {
                        break;
                    };
                    let params = String::from_utf8_lossy(&bytes[start..end]);
                    let first_param = |default: u32| {
                        params
                            .trim_start_matches('?')
                            .split(';')
                            .next()
                            .and_then(|p| p.parse::<u32>().ok())
                            .filter(|&n| n > 0)
                            .unwrap_or(default)
                    };
                    match final_byte {
                        b'H' | b'f' | b'd' => row = first_param(1),
                        b'A' | b'F' => row = row.saturating_sub(first_param(1)).max(1),
                        b'B' | b'E' | b'e' => row += first_param(1),
                        b's' => saved = row,
                        b'u' => row = saved,
                        b'K' | b'X' | b'@' | b'P' | b'L' | b'M' if on_page(row) => found.push(at),
                        b'J' if !params.starts_with('?') => found.push(at),
                        _ => {}
                    }
                    at = end + 1;
                }
                Some(b'P' | b']' | b'_') => {
                    let mut end = at + 2;
                    while end < bytes.len()
                        && bytes[end] != 0x07
                        && bytes.get(end..end + 2) != Some(b"\x1b\\")
                    {
                        end += 1;
                    }
                    at = end + if bytes.get(end) == Some(&0x07) { 1 } else { 2 };
                }
                // `ESC (` and kin take one more byte, a charset.
                Some(b'(' | b')' | b'*' | b'+' | b'#' | b'%') => at += 3,
                Some(b'7') => {
                    saved = row;
                    at += 2;
                }
                Some(b'8') => {
                    row = saved;
                    at += 2;
                }
                Some(b'M') => {
                    row = row.saturating_sub(1).max(1);
                    at += 2;
                }
                _ => at += 2,
            }
            continue;
        }
        match byte {
            b'\n' | 0x0b | 0x0c => row += 1,
            b' '..=0x7e | 0x80.. if on_page(row) => found.push(at),
            _ => {}
        }
        at += 1;
    }
    found
}

/// While a page's picture is on the rows and the layout stays, nothing writes a cell or an
/// erase on the page rows, inside tmux or not: typing into the page, reloads, a navigation in
/// the tab, the address line and the find field opened and closed, tab switches, a notice.
/// Every frame goes over the picture as it is, and a view with no frame yet (a new document,
/// a tab not shown before) leaves the old picture until its first one comes (decision 133).
#[test]
fn nothing_is_written_on_the_page_rows_under_a_picture() {
    for tmux in [true, false] {
        let name = if tmux {
            "sixel-still-tmux"
        } else {
            "sixel-still"
        };
        // F4 opens the address line, F8 the find field.
        let keys = format!("{SIXEL_KEYS}\n\"f4\" = \"location.focus\"\n\"f8\" = \"page.find\"");
        // A still page first: its frames come only from what the steps do (the focus square).
        let args = ["demo://pattern", "demo://animation"];
        let mut term = match tmux {
            true => Terminal::start_in_tmux(name, &args, SIXEL_REPLIES, &keys),
            false => Terminal::start_in(name, &args, SIXEL_REPLIES, &keys),
        };
        let shown = term.wait(0, Duration::from_secs(15), |got| {
            !sixel_spans(got).is_empty()
        });
        assert!(shown, "no sixel\n{}", term.context());
        // Tab 2 once, so both tabs have a frame, then back; from here on a picture is placed.
        term.type_bytes(F2);
        term.wait(term.got.len(), Duration::from_millis(800), |_| false);
        term.type_bytes(F2);
        term.wait(term.got.len(), Duration::from_millis(800), |_| false);
        let from = term.got.len();
        let pause = |term: &mut Terminal, ms| {
            term.wait(term.got.len(), Duration::from_millis(ms), |_| false);
        };
        let steps: [(&str, &[u8]); 10] = [
            ("typing into the page", b"jjjkkkabc"),
            ("a reload", F5),
            ("another reload", F5),
            ("the address line", b"\x1bOS"),
            ("typing in the address line", b"demo://res"),
            ("Escape", b"\x1b"),
            ("the find field and Escape", b"\x1b[19~ani\x1b"),
            ("a tab switch and back", b"\x1bOQ"),
            ("a notice", b"\x1bOS:chrome.nothing\r"),
            ("a navigation in the tab", b"\x1bOSdemo://resize\r"),
        ];
        for (what, bytes) in steps {
            let at = term.got.len();
            term.type_bytes(bytes);
            pause(
                &mut term,
                if what == "a tab switch and back" {
                    400
                } else {
                    700
                },
            );
            if what == "a tab switch and back" {
                term.type_bytes(F2);
                pause(&mut term, 700);
            }
            let writes = page_row_writes(&term.got[at..], 2, 23);
            if let Some(&first) = writes.first() {
                let start = at + first.saturating_sub(40);
                let end = (start + 160).min(term.got.len());
                panic!(
                    "{what} wrote on the page rows (tmux {tmux}), {} times: {}\n{}",
                    writes.len(),
                    printable(&term.got[start..end]),
                    term.context()
                );
            }
        }
        assert!(
            sixel_spans(&term.got[from..]).len() >= 2,
            "the steps drew too few frames\n{}",
            term.context()
        );
        term.type_bytes(b"q");
        assert!(term.exit().success(), "{}", term.context());
    }
}

#[test]
fn a_drag_that_leaves_the_page_still_reaches_the_page() {
    let mut term = Terminal::start("drag", &["demo://resize"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "no image\n{}", term.context());
    // Press at cell (5, 5) of the screen, then move onto the top line of the chrome with the
    // button held: the page gets the position above its top edge (one row of 20 pixels).
    // Presses, and motion while a button is held (1002), in SGR form.
    assert!(
        has(&term.got, b"\x1b[?1000h\x1b[?1002h\x1b[?1006h"),
        "button-event mouse tracking was not asked for\n{}",
        term.context()
    );
    let since = term.got.len();
    term.type_bytes(b"\x1b[<0;6;6M\x1b[<32;6;1M");
    let moved = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "50.0, -20.0")
    });
    assert!(moved, "the page lost the drag\n{}", term.context());
    // Released over the chrome: the capture ends, so the next motion up there is nobody's.
    let since = term.got.len();
    term.type_bytes(b"\x1b[<0;6;1m\x1b[<35;11;1M");
    let still = term.wait(since, Duration::from_millis(500), |got| {
        shows(got, "100.0, -20.0")
    });
    assert!(!still, "motion after the release reached the page");
}

/// `sig --dump ARGS` with the demo engine's configuration, without a terminal.
fn dump(name: &str, args: &[&str]) -> (ExitStatus, String, String) {
    let dir = std::env::temp_dir().join(format!("sig-dump-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("config/sighurt")).unwrap();
    fs::write(
        dir.join("config/sighurt/config.toml"),
        "version = 1\n[engines]\ndefault = \"demo\"\n[routing.demo]\nschemes = [\"demo\"]\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_sig"))
        .arg("--dump")
        .args(args)
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(&dir);
    (
        output.status,
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn dump_prints_the_page_as_text_to_a_pipe() {
    let (status, out, err) = dump("ok", &["demo://pattern", "--width", "40"]);
    assert!(status.success(), "{status:?}\n{err}");
    assert!(out.starts_with("Demo: pattern\n"), "{out}");
    assert!(out.contains("Animation page[1]"), "{out}");
    assert!(
        out.contains("References\n\n1. demo:animation\n2. demo:resize\n"),
        "{out}"
    );
    assert!(out.lines().all(|line| line.chars().count() <= 40), "{out}");
    assert!(!out.contains('\x1b'), "escape sequences in the dump");
}

#[test]
fn dump_fails_with_a_message_and_a_nonzero_exit() {
    let (status, out, err) = dump("fail", &["demo://nope"]);
    assert_eq!(status.code(), Some(1), "{err}");
    assert!(out.is_empty(), "{out}");
    assert!(
        err.starts_with("sig: ") && err.lines().count() == 1,
        "{err}"
    );

    // An engine with no page text says so.
    if cfg!(feature = "engine-wasm") {
        let (status, out, err) = dump("no-text", &["demo://pattern", "--engine", "wasm"]);
        assert_eq!(status.code(), Some(1), "{err}");
        assert!(out.is_empty() && err.contains("no page text"), "{out}{err}");
    }
}

#[test]
fn dump_errors_never_carry_escape_sequences() {
    let (status, out, err) = dump("hostile", &["demo://\x1b]0;pwned\x07\u{202e}nope"]);
    assert_eq!(status.code(), Some(1), "{err}");
    assert!(out.is_empty(), "{out}");
    let bad = |c: char| (c.is_control() && c != '\n') || c == '\u{202e}';
    assert!(!err.chars().any(bad), "{err:?}");
}

/// A local page whose script spins after load. `--dump --engine servo` must still exit
/// inside its timeout plus the shutdown bound. SpiderMonkey often kills that spin, and
/// Servo then shuts down by itself; a spin that survives is what the bound is for, and
/// the runtime test is what proves the `_exit`. This test is off unless `sig` is built
/// with `engine-servo` (`cargo test -p sighurt-browser --all-features --test tui`).
#[cfg(feature = "engine-servo")]
#[test]
fn dump_of_a_busy_script_exits_within_its_deadline() {
    const PAGE: &str = r#"<!DOCTYPE html>
<title>Busy</title>
<h1>Busy page</h1>
<p>alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango uniform victor whiskey xray yankee zulu</p>
<script>
addEventListener("load", function () {
  setTimeout(function spin() {
    var end = performance.now() + 60000;
    while (performance.now() < end) {}
    setTimeout(spin, 0);
  }, 300);
});
</script>
"#;
    let dir = std::env::temp_dir().join(format!("sig-dump-busy-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("config/sighurt")).unwrap();
    fs::write(dir.join("config/sighurt/config.toml"), "version = 1\n").unwrap();
    let page = dir.join("busy.html");
    fs::write(&page, PAGE).unwrap();
    let stdout = fs::File::create(dir.join("stdout")).unwrap();
    let stderr = fs::File::create(dir.join("stderr")).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_sig"))
        .arg("--dump")
        .arg(&page)
        .args(["--engine", "servo", "--timeout", "8"])
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .unwrap();
    let started = Instant::now();
    // `--timeout 8` plus dump's 5 s shutdown bound, and time for Servo to start.
    let limit = Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                let err = fs::read_to_string(dir.join("stderr")).unwrap_or_default();
                panic!("still running after {limit:?}\n{err}");
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => panic!("{error}"),
        }
    };
    let err = fs::read_to_string(dir.join("stderr")).unwrap_or_default();
    let _ = fs::remove_dir_all(&dir);
    assert!(
        matches!(status.code(), Some(0 | 2)),
        "exit {status:?} after {:?}\n{err}",
        started.elapsed()
    );
}

// ── Faults: what a terminal or its user can throw at `sig` ────────────────────────────────

impl Terminal {
    /// Types `bytes` in chunks, reading what `sig` writes between them, so neither side of the
    /// pty fills up while the other waits.
    fn flood(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(4096) {
            let mut rest = chunk;
            let mut progress = Instant::now();
            while !rest.is_empty() {
                match rustix::io::write(&self.master, rest) {
                    Ok(n) => {
                        rest = &rest[n..];
                        progress = Instant::now();
                    }
                    Err(Errno::AGAIN) | Err(Errno::INTR) => {
                        self.assert_taking(progress);
                        self.wait(self.got.len(), Duration::from_millis(1), |_| false);
                    }
                    Err(error) => panic!("writing to the pty: {error}"),
                }
            }
            self.wait(self.got.len(), Duration::ZERO, |_| false);
        }
    }

    /// Whether `sig` is still running.
    fn running(&mut self) -> bool {
        self.child.try_wait().unwrap().is_none()
    }
}

/// A small xorshift generator: the tests' random bytes are the same on every run.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// About `size` bytes of what a terminal sends, at random: printable text, UTF-8, controls,
/// CSI, SS3, SGR mouse reports, bracketed pastes, OSC and DCS strings, cut-off sequences and
/// stray bytes. Never Ctrl+C in any encoding (0x03, `CSI 99;5u`, `CSI 27;5;99~`) and never `q`.
fn random_input(seed: u64, size: usize) -> Vec<u8> {
    let mut random = Random(seed);
    let mut out = Vec::with_capacity(size + 64);
    while out.len() < size {
        match random.below(10) {
            0..=2 => {
                for _ in 0..random.below(12) {
                    let byte = 0x20 + random.below(0x5f) as u8;
                    out.push(if byte == b'q' { b'p' } else { byte });
                }
            }
            3 => out.extend_from_slice(
                ["å", "ä", "ö", "→", "😀", "\u{202e}"][random.below(6) as usize].as_bytes(),
            ),
            4 => {
                let control = random.below(32) as u8;
                out.push(if control == 0x03 { 0x04 } else { control });
            }
            5 => {
                let params: Vec<String> = (0..random.below(4))
                    .map(|_| random.below(300).to_string())
                    .collect();
                let mut params = params.join(";");
                let finals = b"ABCDEFGHJKPQRSZ~mnhlu";
                let fin = finals[random.below(finals.len() as u64) as usize];
                if matches!(fin, b'u' | b'~')
                    && (params.starts_with("99") || params.starts_with("27"))
                {
                    params.insert(0, '1');
                }
                out.extend_from_slice(format!("\x1b[{params}").as_bytes());
                out.push(fin);
            }
            6 => out.extend_from_slice(
                format!(
                    "\x1b[<{};{};{}{}",
                    random.below(70),
                    random.below(120),
                    random.below(40),
                    if random.below(2) == 0 { 'M' } else { 'm' }
                )
                .as_bytes(),
            ),
            7 => out.extend_from_slice(
                [
                    "\x1bOP",
                    "\x1bOA",
                    "\x1b",
                    "\x1b[",
                    "\x1b[<",
                    "\x1b]52;c;",
                    "\x1bP",
                ][random.below(7) as usize]
                    .as_bytes(),
            ),
            8 => {
                let body: Vec<u8> = (0..random.below(64))
                    .map(|_| 0x20 + random.below(0x5f) as u8)
                    .collect();
                let (start, end): (&[u8], &[u8]) = match random.below(4) {
                    0 => (b"\x1b]0;", b"\x07"),
                    1 => (b"\x1bP1+r", b"\x1b\\"),
                    2 => (b"\x1b[200~", b"\x1b[201~"),
                    _ => (b"\x1b_G", b"\x1b\\"),
                };
                out.extend_from_slice(start);
                out.extend_from_slice(&body);
                out.extend_from_slice(end);
            }
            _ => out.push(0x80 + random.below(0x80) as u8),
        }
    }
    out
}

/// 200 resizes in about a second: `sig` keeps up, ends at the last size, and still quits.
#[test]
fn a_flood_of_resizes_ends_at_the_last_size() {
    let started = Instant::now();
    let mut term = Terminal::start("winch-flood", &["demo://resize"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    let flood = Instant::now();
    for i in 0..200u16 {
        term.resize(60 + i % 41, 16 + i % 17);
        thread::sleep(Duration::from_millis(4));
        term.wait(term.got.len(), Duration::ZERO, |_| false);
    }
    let flooded = flood.elapsed();
    let since = term.got.len();
    term.resize(100, 30);
    let resized = term.wait(since, Duration::from_secs(10), |got| {
        image(got, 100, 28, 1000, 560)
    });
    assert!(
        resized,
        "no image of the last size after {flooded:?} of resizes\n{}",
        term.context()
    );
    assert_eq!(term.tabs(), (1, 1), "{}", term.context());
    term.type_bytes(b"q");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
    let frames = term.counter("page_frames_presented");
    let seconds = started.elapsed().as_secs() + 1;
    assert!(
        frames <= FPS * seconds,
        "{frames} frames in {seconds} s, above {FPS} a second"
    );
}

/// A terminal that never answers the probe: `sig` gives up on it after its deadline and runs.
#[test]
fn a_terminal_that_never_answers_the_probe_still_gets_the_browser() {
    let mut term = Terminal::start_with("silent", &["demo://pattern"], b"");
    let shown = term.wait(0, Duration::from_secs(15), |got| has(got, b"[1/1]"));
    assert!(shown, "no tab line\n{}", term.context());
    term.type_bytes(b"q");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
}

/// Malformed answers to the probe: overlong and empty parameters, unknown and cut-off
/// sequences, strings that aren't UTF-8, an interrupted CSI. `sig` starts, keeps the
/// terminal's graphics off, and stays responsive.
const GARBAGE_REPLIES: &[u8] =
    b"\x1b[?;;;;99999999999999999999999u\x1b_G\x1b\\\x1b_Gi=;\xff\xfe\x1b\\\
\x1bP1+r\x1b\\\x1bP0+r4142\x1b\\\x1b]11;rgb:zzzz/\x07\x1b]10;\x1b\\\x1b[6;0;0t\x1b[6;-5;99999t\
\x1b[4;0;0t\x1b[4;99999999999;99999999999t\x1b[?62;\x1b[?\x1b[c";

#[test]
fn garbage_answers_to_the_probe_are_survived() {
    let mut term = Terminal::start_with("garbage", &["demo://pattern"], GARBAGE_REPLIES);
    let shown = term.wait(0, Duration::from_secs(15), |got| has(got, b"[1/1]"));
    assert!(shown, "no tab line\n{}", term.context());
    assert_eq!(term.tabs(), (1, 1), "{}", term.context());
    term.type_bytes(b"q");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
}

/// A megabyte of random terminal input as the answer to the probe: `sig` starts, answers its
/// diagnostics signal, and SIGTERM still quits it and restores the terminal.
#[test]
fn a_megabyte_of_garbage_as_the_probes_answer_is_survived() {
    let replies: &'static [u8] = Box::leak(random_input(0x5eed, 1 << 20).into_boxed_slice());
    let mut term = Terminal::start_with("garbage-1mb", &["demo://pattern"], replies);
    let shown = term.wait(0, Duration::from_secs(20), |got| has(got, b"[1/"));
    assert!(shown, "no tab line\n{}", term.context());
    let (_, tabs) = term.tabs();
    assert!(tabs >= 1, "{}", term.context());
    assert!(term.running(), "sig quit\n{}", term.context());
    // SAFETY: kill(2) on the child.
    unsafe { libc::kill(term.child.id() as i32, libc::SIGTERM) };
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
}

/// A megabyte of random input while `sig` runs, with a keymap whose one binding is Ctrl+C:
/// none of it is Ctrl+C, so nothing quits; the browser still answers, and Ctrl+C then quits.
#[test]
fn a_megabyte_of_random_input_runs_no_command_the_keymap_did_not_bind() {
    let keys = "[input]\nkeymap = \"bindings\"\n[keys]\n\"ctrl-c\" = \"browser.quit\"";
    let mut term = Terminal::start_in("stdin-flood", &["demo://pattern"], REPLIES, keys);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    let input = random_input(0xf100d, 1 << 20);
    let flood = Instant::now();
    term.flood(&input);
    let flooded = flood.elapsed();
    // The report comes once the loop has read up to it.
    let answered = Instant::now();
    let _ = term.tabs();
    let answered = answered.elapsed();
    assert!(
        term.running(),
        "random input without Ctrl+C quit sig\n{}",
        term.context()
    );
    // An Escape closes an address line a click may have opened; Ctrl+C then quits.
    term.type_bytes(b"\x1b");
    thread::sleep(Duration::from_millis(100));
    term.type_bytes(b"\x03");
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?} (flood {flooded:?}, report after {answered:?})\n{}",
        term.context()
    );
    term.assert_restored();
}

/// The terminal goes away (its master side closed) while `sig` runs: `sig` exits with a
/// failure, saving the session, instead of spinning on a dead terminal.
#[test]
fn closing_the_terminal_ends_sig_with_a_failure() {
    let mut term = Terminal::start("hangup", &["demo://animation"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    // Close the master but keep its descriptor number: /dev/null takes its place.
    let null = fs::File::open("/dev/null").unwrap();
    // SAFETY: dup2(2) onto a descriptor this test owns.
    assert!(
        unsafe {
            libc::dup2(
                std::os::fd::AsRawFd::as_raw_fd(&null),
                std::os::fd::AsRawFd::as_raw_fd(&term.master),
            )
        } >= 0
    );
    let closed = Instant::now();
    let status = loop {
        if let Some(status) = term.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            closed.elapsed() < Duration::from_secs(10),
            "sig still runs 10 s after its terminal went away\n{}",
            term.context()
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        !status.success(),
        "sig exited with {status:?} after {:?}\n{}",
        closed.elapsed(),
        term.context()
    );
    let session = fs::read_to_string(term.dir.join("state/sighurt/session.toml"));
    assert!(
        session
            .as_deref()
            .is_ok_and(|s| s.contains("demo://animation")),
        "the session was not saved: {session:?}\n{}",
        term.context()
    );
}

/// Opens a second tab, `demo://pattern`, with `:tab.new`, and waits until `sig` reports it.
fn open_second_tab(term: &mut Terminal) {
    term.type_bytes(b":tab.new demo://pattern\r");
    let start = Instant::now();
    while term.tabs().1 != 2 {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "no second tab\n{}",
            term.context()
        );
    }
}

/// The session file holds the second tab, and no temporary file is left beside it.
fn assert_saved_whole(term: &Terminal) {
    let state = term.dir.join("state/sighurt");
    let session = fs::read_to_string(state.join("session.toml"));
    assert!(
        session
            .as_deref()
            .is_ok_and(|s| s.contains("demo://animation") && s.contains("demo://pattern")),
        "the latest tabs were not saved: {session:?}\n{}",
        term.context()
    );
    let left: Vec<String> = fs::read_dir(&state)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(left.is_empty(), "left behind: {left:?}");
}

/// `SIGHUP` quits like `SIGTERM`: the terminal restored, status 0, and the tab opened a moment
/// before (inside the session's one-second save delay) saved. It used to restore the terminal
/// from the handler and die of the signal, without the last save.
#[test]
fn sighup_quits_saving_the_latest_tabs() {
    let mut term = Terminal::start("sighup", &["demo://animation"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    open_second_tab(&mut term);
    // SAFETY: kill(2) on the child.
    unsafe { libc::kill(term.child.id() as i32, libc::SIGHUP) };
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
    assert_saved_whole(&term);
}

/// The terminal emulator goes away while `sig` runs in it: `sig`'s controlling terminal hangs
/// up, the kernel sends it `SIGHUP` and its reads and writes fail. `sig` exits by itself
/// (status 0 for the signal, or the failure of the terminal read if the loop sees that first),
/// never killed by the signal, with the latest tabs saved and nothing half-written left.
#[test]
fn the_controlling_terminal_closing_saves_the_latest_tabs() {
    let mut term = Terminal::start_controlling("ctty-hangup", &["demo://animation"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    open_second_tab(&mut term);
    let null = fs::File::open("/dev/null").unwrap();
    // SAFETY: dup2(2) onto a descriptor this test owns: the master closes.
    assert!(
        unsafe {
            libc::dup2(
                std::os::fd::AsRawFd::as_raw_fd(&null),
                std::os::fd::AsRawFd::as_raw_fd(&term.master),
            )
        } >= 0
    );
    let closed = Instant::now();
    let status = loop {
        if let Some(status) = term.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            closed.elapsed() < Duration::from_secs(10),
            "sig still runs 10 s after its terminal went away\n{}",
            term.context()
        );
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.code().is_some(),
        "sig was killed: {status:?}\n{}",
        term.context()
    );
    assert_saved_whole(&term);
}

/// `--dump` of the slow page: done within the settle cap (3 s after its first snapshot) and the
/// thin page's grace (6 s after the load), not the 30 s timeout.
#[test]
fn dump_of_the_slow_page_ends_within_its_caps() {
    let started = Instant::now();
    let (status, out, err) = dump("slow", &["demo://slow"]);
    let took = started.elapsed();
    assert!(status.success(), "{status:?}\n{err}");
    assert!(out.starts_with("Demo: slow"), "{out}");
    assert!(took < Duration::from_secs(12), "{took:?}");
}

/// The terminal has no window fullscreen: `browser.fullscreen` says so. A page's element
/// fullscreen follows the window's policy (a click lets it, a notice says so, Escape leaves it)
/// and the page keeps its rows; without a click it is refused.
#[test]
fn fullscreen_in_the_terminal_is_a_notice() {
    let mut term = Terminal::start("fullscreen", &["demo://fullscreen"]);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "no image\n{}", term.context());
    let page = |held: bool| format!("{{\"page\":{held},\"wanted\":false,\"window\":false}}");
    let fullscreen = |report: &str| report_value(report, "fullscreen");
    let since = term.got.len();
    term.type_bytes(b"\x1b[<0;6;6M\x1b[<0;6;6m");
    let told = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "The page is fullscreen. Escape leaves it.")
    });
    assert!(
        told,
        "no notice of the page's fullscreen\n{}",
        term.context()
    );
    let report = term.report_until(|r| fullscreen(r) == page(true));
    assert_eq!(
        fullscreen(&report),
        page(true),
        "{report}\n{}",
        term.context()
    );
    term.type_bytes(b"\x1b");
    // Left, the notice that said so is taken back: the bottom line says nothing.
    let left = |r: &str| fullscreen(r) == page(false) && r.contains("\"status\":null");
    let report = term.report_until(left);
    assert!(left(&report), "{report}\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(b":browser.fullscreen\r");
    let told = term.wait(since, Duration::from_secs(10), |got| {
        // The grid's diff skips cells the last notice left as they are: match what differs.
        shows(got, "browser.fullscreen is not available in")
    });
    assert!(told, "no notice\n{}", term.context());
    // A page asking by itself is refused.
    term.type_bytes(b":page.open demo://fullscreen-unasked\r");
    let opened = term.wait(term.got.len(), Duration::from_secs(10), |got| {
        shows(got, "unask")
    });
    assert!(opened, "the page did not open\n{}", term.context());
    let report = term.report_until(|_| false);
    assert_eq!(fullscreen(&report), page(false), "{}", term.context());
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
}

/// No graphics: DA1 without sixel, and no kitty reply.
const NO_GRAPHICS_REPLIES: &[u8] = b"\x1b[6;20;10t\x1b[?62c";

/// sig's own pages are drawn and have no text: without graphics, the page rows say so instead
/// of suggesting text mode, and in text mode they say graphics shows them. Needs the Wasm
/// engine: run with `--features frontend-tui,engine-demo,engine-wasm`.
#[cfg(feature = "engine-wasm")]
#[test]
fn sig_pages_need_graphics_and_say_so() {
    let mut term = Terminal::start_with("pages-text", &["sig://help"], NO_GRAPHICS_REPLIES);
    let said = term.wait(0, Duration::from_secs(20), |got| {
        shows(got, "need graphics")
    });
    assert!(
        said,
        "no notice for a sig page without graphics\n{}",
        term.context()
    );
    assert!(
        !shows(&term.got, "chrome.text-mode"),
        "it suggests text mode\n{}",
        term.context()
    );
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());

    let mut term = Terminal::start_in("pages-mode", &["sig://help"], REPLIES, "mode = \"text\"");
    let said = term.wait(0, Duration::from_secs(20), |got| shows(got, "have no text"));
    assert!(
        said,
        "no notice for a sig page in text mode\n{}",
        term.context()
    );
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
}

/// sig://terminal in the terminal frontend with graphics: the page shows, Escape and Enter start
/// the shell, typed keys reach it, Escape in INSERT is the shell's, the example configuration's
/// `ctrl-escape` (kitty's `CSI 27;5u`) and `f12` (`CSI 24~`) leave INSERT, and `q` quits sig
/// with the shell ended. Needs the Wasm engine: run with `--features frontend-tui,engine-demo,engine-wasm`.
#[cfg(feature = "engine-wasm")]
#[test]
fn sig_terminal_runs_a_shell_in_the_terminal_frontend() {
    let pages = "[pages.terminal]\nenabled = true\ncommand = \"/bin/sh\"";
    let mut term = Terminal::start_in("terminal-page", &["sig://terminal"], KITTY_REPLIES, pages);
    let home = term.dir.join("home");
    let shown = term.wait(0, Duration::from_secs(20), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    term.type_bytes(&kitty_keys("\x1b"));
    term.wait(term.got.len(), Duration::from_secs(5), |got| {
        shows(got, "INSERT MODE")
    });
    term.type_bytes(&kitty_keys("\r"));
    thread::sleep(Duration::from_secs(2));
    term.type_bytes(&kitty_keys("echo $$ > ~/pid; echo hi > ~/hi.txt\r"));
    let start = Instant::now();
    while fs::read_to_string(home.join("hi.txt")).ok().as_deref() != Some("hi\n") {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "the shell did not get the keys\n{}",
            term.context()
        );
        term.wait(term.got.len(), Duration::from_millis(100), |_| false);
    }
    let pid = fs::read_to_string(home.join("pid")).unwrap();
    let shell = Path::new("/proc").join(pid.trim());
    assert!(shell.exists(), "the shell is not running");
    // Escape is the shell's: INSERT stays, and the shell still reads the next line once
    // Ctrl+C has dropped what the Escape began.
    term.type_bytes(&kitty_keys("\x1b"));
    thread::sleep(Duration::from_millis(300));
    term.type_bytes(b"\x1b[99;5u\x1b[99;5:3u");
    thread::sleep(Duration::from_secs(1));
    term.type_bytes(&kitty_keys("echo esc > ~/esc.txt\r"));
    let start = Instant::now();
    while fs::read_to_string(home.join("esc.txt")).ok().as_deref() != Some("esc\n") {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "after Escape, INSERT no longer gave the shell the keys\n{}",
            term.context()
        );
        term.wait(term.got.len(), Duration::from_millis(100), |_| false);
    }
    // ctrl-escape leaves INSERT; Escape enters it again, and f12 leaves it.
    term.type_bytes(b"\x1b[27;5u\x1b[27;5:3u");
    let left = term.wait(term.got.len(), Duration::from_secs(10), insert_mode_left);
    assert!(left, "ctrl-escape did not leave INSERT\n{}", term.context());
    let since = term.got.len();
    term.type_bytes(&kitty_keys("\x1b"));
    let insert = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "INSERT MODE")
    });
    assert!(
        insert,
        "Escape did not enter INSERT again\n{}",
        term.context()
    );
    term.type_bytes(b"\x1b[24~\x1b[24;1:3~");
    let left = term.wait(term.got.len(), Duration::from_secs(10), insert_mode_left);
    assert!(left, "f12 did not leave INSERT\n{}", term.context());
    // Then q in NORMAL.
    term.type_bytes(&kitty_keys("q"));
    let status = term.exit();
    assert!(
        status.success(),
        "sig exited with {status:?}\n{}",
        term.context()
    );
    term.assert_restored();
    let start = Instant::now();
    while shell.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "the shell outlived sig"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// sig://config in the terminal: drawn as graphics, Enter in INSERT MODE runs the editor on the
/// configuration (here `[pages.config] editor`, a script that appends a line to the file it is
/// given), and the file changes. Needs the Wasm engine: run with
/// `--features frontend-tui,engine-demo,engine-wasm`.
#[cfg(feature = "engine-wasm")]
#[test]
fn sig_config_runs_the_editor_in_the_terminal() {
    use std::os::unix::fs::PermissionsExt;
    let scripts = std::env::temp_dir().join(format!("sig-tui-editor-{}", std::process::id()));
    fs::create_dir_all(&scripts).unwrap();
    let editor = scripts.join("editor");
    fs::write(
        &editor,
        "#!/bin/sh\necho '# edited in sig://config' >> \"$1\"\n",
    )
    .unwrap();
    fs::set_permissions(&editor, fs::Permissions::from_mode(0o755)).unwrap();
    let pages = format!(
        "[pages.config]\neditor = {:?}",
        editor.display().to_string()
    );
    let mut term = Terminal::start_in("config-page", &["sig://config"], KITTY_REPLIES, &pages);
    let path = term.dir.join("config/sighurt/config.toml");
    let shown = term.wait(0, Duration::from_secs(20), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "sig://config did not show\n{}", term.context());
    assert!(
        !fs::read_to_string(&path).unwrap().contains("# edited"),
        "nothing runs before Enter"
    );
    let since = term.got.len();
    term.type_bytes(&kitty_keys("\x1b"));
    let insert = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "INSERT MODE")
    });
    assert!(
        insert,
        "Escape did not show INSERT MODE\n{}",
        term.context()
    );
    term.type_bytes(&kitty_keys("\r"));
    let deadline = Instant::now() + Duration::from_secs(15);
    while !fs::read_to_string(&path)
        .unwrap()
        .contains("# edited in sig://config")
    {
        assert!(
            Instant::now() < deadline,
            "the editor did not run\n{}",
            term.context()
        );
        term.wait(term.got.len(), Duration::from_millis(100), |_| false);
    }
    // On sig://config INSERT gives Escape to the page; the example configuration's
    // ctrl-escape leaves it.
    let since = term.got.len();
    term.type_bytes(b"\x1b[27;5u\x1b[27;5:3u");
    let left = term.wait(since, Duration::from_secs(10), insert_mode_left);
    assert!(
        left,
        "ctrl-escape did not leave INSERT MODE\n{}",
        term.context()
    );
    term.type_bytes(&kitty_keys("q"));
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
    let _ = fs::remove_dir_all(&scripts);
}

/// A directory of scripts for a test, made anew.
fn scripts(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sig-tui-{name}-scripts-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Writes the executable script `name` in `dir` and returns its path.
fn script(dir: &Path, name: &str, text: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join(name);
    fs::write(&path, text).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Reads `sig`'s output until `path` exists or `timeout`. Returns whether it does.
fn wait_for_file(term: &mut Terminal, path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while !path.exists() && Instant::now() < deadline {
        term.wait(term.got.len(), Duration::from_millis(100), |_| false);
    }
    path.exists()
}

/// The legacy keys of f12, which leaves INSERT on sig://config.
const F12: &[u8] = b"\x1b[24~";
/// A bracketed paste of "pasted".
const PASTE: &[u8] = b"\x1b[200~pasted\x1b[201~";

/// sig://config where the terminal does not draw it, with no graphics and in text mode: the
/// page rows say how to see it (in text mode the key from the keymap), and Escape, Enter,
/// typing, a paste and a click start nothing and leave the file as it was. With graphics (`m`)
/// Enter starts the editor and typing reaches it; back in text mode typing does not, while it
/// keeps running, and with graphics again it does. Needs the Wasm engine: run with
/// `--features frontend-tui,engine-demo,engine-wasm`.
#[cfg(feature = "engine-wasm")]
#[test]
fn sig_config_gets_no_input_where_it_is_not_drawn() {
    let dir = scripts("config-undrawn");
    let (started, input) = (dir.join("started"), dir.join("input"));
    // The editor says it started, then puts what it reads, byte by byte, in `input`.
    let editor = script(
        &dir,
        "editor",
        &format!("#!/bin/sh\necho $$ > {started:?}\nstty raw -echo\nexec cat >> {input:?}\n"),
    );
    let pages = format!(
        "[pages.config]\neditor = {:?}",
        editor.display().to_string()
    );
    // What the page gets while it is not drawn: INSERT, Enter, typing, a paste, a click.
    let try_everything = |term: &mut Terminal| {
        term.type_bytes(b"\x1b");
        let insert = term.wait(term.got.len(), Duration::from_secs(5), |got| {
            shows(got, "INSERT MODE")
        });
        assert!(insert, "Escape did not enter INSERT\n{}", term.context());
        term.type_bytes(b"\r");
        thread::sleep(Duration::from_millis(300));
        term.type_bytes(b"abc\r");
        term.type_bytes(PASTE);
        term.type_bytes(&left_click(40, 12));
        term.type_bytes(b"\r");
        term.wait(term.got.len(), Duration::from_secs(2), |_| false);
        term.type_bytes(F12);
        let left = term.wait(term.got.len(), Duration::from_secs(5), insert_mode_left);
        assert!(left, "f12 did not leave INSERT\n{}", term.context());
    };

    // No graphics.
    let mut term = Terminal::start_in(
        "config-nographics",
        &["sig://config"],
        NO_GRAPHICS_REPLIES,
        &pages,
    );
    let config = term.dir.join("config/sighurt/config.toml");
    let before = fs::read_to_string(&config).unwrap();
    let said = term.wait(0, Duration::from_secs(20), |got| {
        shows(got, "need graphics")
    });
    assert!(said, "no message\n{}", term.context());
    assert!(shows(&term.got, "get no input here"), "{}", term.context());
    try_everything(&mut term);
    assert!(!started.exists(), "the editor started\n{}", term.context());
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        before,
        "the file changed"
    );
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());

    // Text mode, then graphics.
    let tui = format!("mode = \"text\"\n{pages}");
    let mut term = Terminal::start_in("config-text", &["sig://config"], REPLIES, &tui);
    let config = term.dir.join("config/sighurt/config.toml");
    let before = fs::read_to_string(&config).unwrap();
    let said = term.wait(0, Duration::from_secs(20), |got| {
        screen(got, 80, 24)
            .iter()
            .any(|row| row.contains("have no text and get no input here: m shows graphics"))
    });
    assert!(said, "no message naming m\n{}", term.context());
    try_everything(&mut term);
    assert!(!started.exists(), "the editor started\n{}", term.context());
    assert_eq!(
        fs::read_to_string(&config).unwrap(),
        before,
        "the file changed"
    );
    // With graphics, Enter starts it and typing reaches it.
    let since = term.got.len();
    term.type_bytes(b"m");
    let drawn = term.wait(since, Duration::from_secs(10), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(drawn, "m did not show graphics\n{}", term.context());
    term.type_bytes(b"\x1b");
    thread::sleep(Duration::from_millis(300));
    term.type_bytes(b"\r");
    assert!(
        wait_for_file(&mut term, &started, Duration::from_secs(10)),
        "with graphics Enter did not start the editor\n{}",
        term.context()
    );
    let pid = fs::read_to_string(&started).unwrap();
    let program = Path::new("/proc").join(pid.trim());
    // Typed until it reads them: stty may not have made it raw yet.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fs::read_to_string(&input)
        .unwrap_or_default()
        .contains("xyz")
    {
        assert!(
            Instant::now() < deadline,
            "the editor did not get the keys\n{}",
            term.context()
        );
        term.type_bytes(b"xyz");
        term.wait(term.got.len(), Duration::from_millis(500), |_| false);
    }
    // Text mode again: the editor runs on and gets nothing.
    term.type_bytes(F12);
    term.wait(term.got.len(), Duration::from_secs(5), insert_mode_left);
    let since = term.got.len();
    term.type_bytes(b"m");
    let text = term.wait(since, Duration::from_secs(10), |got| shows(got, "TEXT"));
    assert!(text, "m did not show text\n{}", term.context());
    term.type_bytes(b"\x1b");
    term.wait(term.got.len(), Duration::from_millis(500), |_| false);
    term.type_bytes(b"QQQ");
    term.type_bytes(PASTE);
    term.wait(term.got.len(), Duration::from_secs(2), |_| false);
    let got = fs::read_to_string(&input).unwrap_or_default();
    assert!(
        !got.contains('Q') && !got.contains("pasted"),
        "typed into it while not drawn: {got:?}"
    );
    assert!(program.exists(), "the editor stopped running");
    // Graphics again: it gets the keys.
    term.type_bytes(F12);
    term.wait(term.got.len(), Duration::from_secs(5), insert_mode_left);
    let since = term.got.len();
    term.type_bytes(b"m");
    let drawn = term.wait(since, Duration::from_secs(10), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(drawn, "m did not show graphics again\n{}", term.context());
    term.type_bytes(b"\x1b");
    term.wait(term.got.len(), Duration::from_millis(500), |_| false);
    term.type_bytes(b"ok");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !fs::read_to_string(&input)
        .unwrap_or_default()
        .contains("ok")
    {
        assert!(
            Instant::now() < deadline,
            "the editor did not get keys again\n{}",
            term.context()
        );
        term.wait(term.got.len(), Duration::from_millis(100), |_| false);
    }
    assert!(!fs::read_to_string(&input).unwrap().contains('Q'));
    term.type_bytes(F12);
    term.wait(term.got.len(), Duration::from_secs(5), insert_mode_left);
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
    let _ = fs::remove_dir_all(&dir);
}

/// sig://config's editor ending reads a linked configuration again, which the watch can't see,
/// with no further input: the editor writes the file when it is told to hang up (its tab closed
/// from another), ignores it, and is killed 2 s later, when nothing else is due; a single report
/// some seconds after that has the reload. Needs the Wasm engine: run with
/// `--features frontend-tui,engine-demo,engine-wasm`.
#[cfg(feature = "engine-wasm")]
#[test]
fn an_editors_end_reads_a_linked_configuration_with_no_more_input() {
    let dir = scripts("config-linked");
    let started = dir.join("started");
    let editor = script(
        &dir,
        "editor",
        &format!(
            "#!/bin/sh\ntrap 'printf \"[commands]\\nmarked = \\\"tab.new\\\"\\n\" >> \"$1\"' HUP\n\
             echo $$ > {started:?}\nwhile :; do sleep 0.2; done\n"
        ),
    );
    let pages = format!(
        "[pages.config]\neditor = {:?}",
        editor.display().to_string()
    );
    let mut term = Terminal::start_in("config-linked", &["sig://config"], KITTY_REPLIES, &pages);
    let shown = term.wait(0, Duration::from_secs(20), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "sig://config did not show\n{}", term.context());
    // The file becomes a link to one elsewhere, with the same text.
    let link = term.dir.join("config/sighurt/config.toml");
    let target = term.dir.join("dotfiles/sig.toml");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::copy(&link, &target).unwrap();
    let new = link.with_extension("new");
    std::os::unix::fs::symlink(&target, &new).unwrap();
    fs::rename(&new, &link).unwrap();
    term.wait(term.got.len(), Duration::from_secs(1), |_| false);
    let reloads = |report: &str| {
        report_value(report, "config_reloads")
            .parse::<u64>()
            .unwrap()
    };
    let before = reloads(&term.report_until(|r| r.contains("config_reloads")));
    term.type_bytes(&kitty_keys("\x1b"));
    term.wait(term.got.len(), Duration::from_secs(5), |got| {
        shows(got, "INSERT MODE")
    });
    term.type_bytes(&kitty_keys("\r"));
    assert!(
        wait_for_file(&mut term, &started, Duration::from_secs(10)),
        "the editor did not start\n{}",
        term.context()
    );
    let pid = fs::read_to_string(&started).unwrap();
    let program = Path::new("/proc").join(pid.trim());
    term.type_bytes(b"\x1b[27;5u\x1b[27;5:3u");
    let left = term.wait(term.got.len(), Duration::from_secs(10), insert_mode_left);
    assert!(left, "ctrl-escape did not leave INSERT\n{}", term.context());
    // Another tab, home, and the page's tab closed from it: no view is left to wake sig.
    let since = term.got.len();
    term.type_bytes(&kitty_keys("t"));
    let home = term.wait(since, Duration::from_secs(10), |got| shows(got, "Sighurt"));
    assert!(home, "home did not show\n{}", term.context());
    term.type_bytes(&kitty_keys("\x1b"));
    term.wait(term.got.len(), Duration::from_millis(300), |_| false);
    term.type_bytes(&kitty_keys(":tab.close 1\r"));
    term.wait(term.got.len(), Duration::from_millis(300), |_| false);
    assert_eq!(term.tabs(), (1, 1), "{}", term.context());
    // No more input: the editor is killed 2 s after the hang-up, and then nothing is due.
    let deadline = Instant::now() + Duration::from_secs(8);
    while program.exists() {
        assert!(Instant::now() < deadline, "the editor was not ended");
        term.wait(term.got.len(), Duration::from_millis(100), |_| false);
    }
    term.wait(term.got.len(), Duration::from_secs(2), |_| false);
    assert!(
        fs::read_to_string(&target)
            .unwrap()
            .contains("marked = \"tab.new\""),
        "the editor did not write where the link leads"
    );
    // One report: asking for it wakes the loop, so a second one would see a reload that only
    // that wake did.
    let report = term.report_until(|r| r.contains("config_reloads"));
    assert!(
        reloads(&report) > before,
        "the editor ended but the file was not read again ({before} reloads before)\n{}",
        term.context()
    );
    term.type_bytes(&kitty_keys("q"));
    assert!(term.exit().success(), "{}", term.context());
    let _ = fs::remove_dir_all(&dir);
}

/// sig://home drawn before the history file has been read: once it has, with no input, home
/// lists the visits. The file is a named pipe the test writes only after home is drawn.
#[test]
fn home_lists_the_visits_read_after_it_was_drawn() {
    let mut term = Terminal::start_seeded(
        "home-history",
        &["sig://home"],
        KITTY_REPLIES,
        "",
        Seed::Pipe,
    );
    term.resize(80, 40);
    let listed = |got: &[u8], text: &str| screen(got, 80, 40).iter().any(|row| row.contains(text));
    let shown = term.wait(0, Duration::from_secs(15), |got| listed(got, "KEYS"));
    assert!(shown, "home is not shown\n{}", term.context());
    term.wait(term.got.len(), Duration::from_millis(500), |_| false);
    assert!(!listed(&term.got, "RECENTLY VISITED"), "{}", term.context());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    term.feed_history(&format!(
        "version = 1\n[[visits]]\nurl = \"https://www.example.com/seen\"\ntitle = \"Seen\"\n\
         last = {now}\ncount = 1\n"
    ));
    let visits = term.wait(0, Duration::from_secs(5), |got| {
        listed(got, "RECENTLY VISITED")
    });
    assert!(
        visits,
        "home did not list the visits read\n{}",
        term.context()
    );
    // Then idle again.
    term.wait(term.got.len(), Duration::from_millis(800), |_| false);
    let idle = term.got.len();
    term.wait(idle, Duration::from_millis(800), |_| false);
    assert_eq!(term.got.len(), idle, "an idle home writes");
    term.type_bytes(&kitty_keys("q"));
    assert!(term.exit().success(), "{}", term.context());
}

/// The screen a terminal of `cols`×`rows` shows after `bytes`, as lines of text.
fn screen(bytes: &[u8], cols: u16, rows: u16) -> Vec<String> {
    let mut parser = vt100::Parser::new(rows, cols, 0);
    parser.process(bytes);
    parser.screen().rows(0, cols).collect()
}

/// The 1-based column and row of the first `text` on `screen`.
fn find_on(screen: &[String], text: &str) -> Option<(usize, usize)> {
    screen.iter().enumerate().find_map(|(row, line)| {
        line.find(text)
            .map(|at| (line[..at].chars().count() + 1, row + 1))
    })
}

/// A left click (press and release) at a 1-based cell, as SGR mouse reports.
fn left_click(column: usize, row: usize) -> Vec<u8> {
    format!("\x1b[<0;{column};{row}M\x1b[<0;{column};{row}m").into_bytes()
}

/// sig://home is the chrome's own page: its heading and sections are text in the page rows,
/// with graphics and without, and an idle home builds nothing again.
#[test]
fn home_is_text_in_the_page_rows_with_graphics_and_without() {
    for (name, replies) in [
        ("home-kitty", KITTY_REPLIES),
        ("home-sixel", SIXEL_REPLIES),
        ("home-none", NO_GRAPHICS_REPLIES),
    ] {
        let mut term = Terminal::start_with(name, &["sig://home"], replies);
        let shown = term.wait(0, Duration::from_secs(15), |got| {
            let rows = screen(got, 80, 24);
            find_on(&rows, "Sighurt ").is_some()
                && find_on(&rows, "KEYS").is_some()
                && find_on(&rows, "Address or search").is_some()
        });
        assert!(shown, "{name}: home is not shown\n{}", term.context());
        let rows = screen(&term.got, 80, 24);
        let (_, heading) = find_on(&rows, "Sighurt ").unwrap();
        assert_eq!(
            heading, 2,
            "{name}: home starts on the first page row\n{rows:#?}"
        );
        if name == "home-kitty" {
            eprintln!("sig --tui sig://home at 80x24:\n{}", rows.join("\n"));
        }
        // Idle: nothing is built or drawn again.
        term.wait(term.got.len(), Duration::from_millis(800), |_| false);
        let idle = term.got.len();
        term.wait(idle, Duration::from_millis(800), |_| false);
        assert_eq!(term.got.len(), idle, "{name}: an idle home writes");
        term.type_bytes(b"q");
        assert!(term.exit().success(), "{}", term.context());
        assert!(
            term.counter("home_builds") <= 2,
            "{name}: home was built again"
        );
    }
}

/// A new tab opened over a picture shows home: the picture goes (kitty deletes it, a sixel is
/// blanked) before home's text is written, and going back shows the picture again.
#[test]
fn a_new_tab_over_a_picture_shows_home_and_the_picture_comes_back() {
    // Kitty: `t` opens the new tab (the example configuration), J goes back to the first.
    let mut term = Terminal::start_with("home-over-kitty", &["demo://pattern"], KITTY_REPLIES);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(shown, "the page did not show\n{}", term.context());
    term.wait(term.got.len(), Duration::from_millis(300), |_| false);
    let since = term.got.len();
    term.type_bytes(&kitty_keys("t"));
    let home = term.wait(since, Duration::from_secs(10), |got| shows(got, "Sighurt"));
    assert!(home, "the new tab does not show home\n{}", term.context());
    let after = &term.got[since..];
    let delete = after.windows(7).position(|w| w == b"\x1b_Ga=d,");
    let text = after
        .windows(b"Sighurt".len())
        .position(|w| w == b"Sighurt");
    assert!(
        delete.is_some_and(|delete| text.is_some_and(|text| delete < text)),
        "the picture was not deleted before home's text ({delete:?}, {text:?})\n{}",
        term.context()
    );
    // Escape leaves the address line tab.new opened; K is tab.previous.
    term.type_bytes(&kitty_keys("\x1b"));
    term.wait(term.got.len(), Duration::from_millis(300), |_| false);
    let since = term.got.len();
    term.type_bytes(&kitty_keys("K"));
    let back = term.wait(since, Duration::from_secs(10), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(back, "the picture did not come back\n{}", term.context());
    term.type_bytes(&kitty_keys("q"));
    assert!(term.exit().success(), "{}", term.context());

    // Sixel: F3 opens a tab, F2 cycles them.
    let keys = format!("{SIXEL_KEYS}\n\"f3\" = \"tab.new\"");
    let mut term = Terminal::start_in("home-over-sixel", &["demo://pattern"], SIXEL_REPLIES, &keys);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        !sixel_spans(got).is_empty()
    });
    assert!(shown, "no sixel\n{}", term.context());
    let blank = blank_row(80);
    for round in 0..2 {
        term.wait(term.got.len(), Duration::from_millis(400), |_| false);
        let since = term.got.len();
        term.type_bytes(if round == 0 { b"\x1bOR" } else { F2 });
        let home = term.wait(since, Duration::from_secs(10), |got| shows(got, "Sighurt"));
        assert!(home, "round {round}: no home\n{}", term.context());
        let after = &term.got[since..];
        let blanked = after
            .windows(blank.len())
            .position(|w| w == blank.as_slice());
        let text = after
            .windows(b"Sighurt".len())
            .position(|w| w == b"Sighurt");
        assert!(
            blanked.is_some_and(|blanked| text.is_some_and(|text| blanked < text)),
            "round {round}: the sixel was not blanked before home's text\n{}",
            term.context()
        );
        assert!(
            sixel_spans(after).is_empty(),
            "round {round}: a sixel over home\n{}",
            term.context()
        );
        if round == 0 {
            // The address line tab.new opened is closed by Escape (the line's own key).
            term.type_bytes(b"\x1b");
            term.wait(term.got.len(), Duration::from_millis(300), |_| false);
        }
        term.wait(term.got.len(), Duration::from_millis(400), |_| false);
        let since = term.got.len();
        term.type_bytes(F2);
        let back = term.wait(since, Duration::from_secs(10), |got| {
            !sixel_spans(got).is_empty()
        });
        assert!(
            back,
            "round {round}: the sixel did not come back\n{}",
            term.context()
        );
    }
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
}

/// Home's links: a left click on one, and Tab then Enter (the example configuration sends them
/// to the page with `page.send-key`), open sig://help in the tab. Needs the Wasm engine: run
/// with `--features frontend-tui,engine-demo,engine-wasm`.
#[cfg(feature = "engine-wasm")]
#[test]
fn home_links_open_by_click_and_by_tab_and_enter() {
    let mut term = Terminal::start_with("home-click", &["sig://home"], KITTY_REPLIES);
    let shown = term.wait(0, Duration::from_secs(15), |got| {
        find_on(&screen(got, 80, 24), "Help[1]").is_some()
    });
    assert!(shown, "no Help link on home\n{}", term.context());
    let (column, row) = find_on(&screen(&term.got, 80, 24), "Help[1]").unwrap();
    let since = term.got.len();
    term.type_bytes(&left_click(column + 1, row));
    let opened = term.wait(since, Duration::from_secs(20), |got| {
        image(got, 80, 22, 800, 440)
    });
    assert!(
        opened,
        "the click did not open sig://help\n{}",
        term.context()
    );
    assert_eq!(term.tabs(), (1, 1), "{}", term.context());
    term.type_bytes(&kitty_keys("q"));
    assert!(term.exit().success(), "{}", term.context());

    let mut term = Terminal::start_with("home-keys", &["sig://home"], NO_GRAPHICS_REPLIES);
    let shown = term.wait(0, Duration::from_secs(15), |got| shows(got, "Help[1]"));
    assert!(shown, "no Help link on home\n{}", term.context());
    let since = term.got.len();
    // Tab and Enter as a legacy terminal sends them.
    term.type_bytes(b"\t");
    term.wait(since, Duration::from_millis(500), |_| false);
    term.type_bytes(b"\r");
    let opened = term.wait(since, Duration::from_secs(20), |got| {
        shows(got, "need graphics")
    });
    assert!(
        opened,
        "Tab and Enter did not open sig://help\n{}",
        term.context()
    );
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
}
