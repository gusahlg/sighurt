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
/// Kitty graphics, cells of 10×20 pixels, and DA1. No kitty keyboard: keys arrive as a legacy
/// terminal sends them.
const REPLIES: &[u8] = b"\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c";
/// The same with the kitty keyboard flags `sig` pushes (31), as kitty and Rio answer.
const KITTY_REPLIES: &[u8] = b"\x1b[?31u\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c";

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
        let dir = std::env::temp_dir().join(format!("sig-tui-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config/sighurt")).unwrap();
        fs::write(
            dir.join("config/sighurt/config.toml"),
            format!(
                "version = 1\n[engines]\ndefault = \"demo\"\n[routing.demo]\nschemes = \
                 [\"demo\"]\n[tui]\nfps = {FPS}\n"
            ),
        )
        .unwrap();
        let (master, slave) = open_pty().unwrap();
        let flags = rustix::fs::fcntl_getfl(&master).unwrap();
        rustix::fs::fcntl_setfl(&master, flags | OFlags::NONBLOCK).unwrap();
        tcsetwinsize(&slave, winsize(80, 24)).unwrap();
        let original = tcgetattr(&slave).unwrap();
        let stderr = fs::File::create(dir.join("stderr")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_sig"))
            .arg("--tui")
            .arg("--diagnostics")
            .arg(dir.join("diag.json"))
            .args(args)
            .env("XDG_CONFIG_HOME", dir.join("config"))
            .env("XDG_STATE_HOME", dir.join("state"))
            .env("XDG_DATA_HOME", dir.join("data"))
            .env("XDG_CACHE_HOME", dir.join("cache"))
            .env("TERM", "xterm-kitty")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("DISPLAY")
            .env_remove("SIG_DIAG")
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(stderr)
            .process_group(0)
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
    /// from `since` on, or `timeout`.
    fn wait(&mut self, since: usize, timeout: Duration, done: impl Fn(&[u8]) -> bool) -> bool {
        let start = Instant::now();
        let mut buf = vec![0u8; 1 << 16];
        loop {
            match rustix::io::read(&self.master, &mut buf) {
                Ok(n) if n > 0 => {
                    self.got.extend_from_slice(&buf[..n]);
                    if !self.answered && has(&self.got, DA1) {
                        self.answered = true;
                        self.type_bytes(self.replies);
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
            if start.elapsed() > timeout {
                return false;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn type_bytes(&self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            match rustix::io::write(&self.master, bytes) {
                Ok(n) => bytes = &bytes[n..],
                Err(Errno::AGAIN) | Err(Errno::INTR) => thread::sleep(Duration::from_millis(1)),
                Err(error) => panic!("writing to the pty: {error}"),
            }
        }
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
                Some(b']' | b'_') => {
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

/// A kitty image transmission placed over `columns`×`rows` cells, `width`×`height` pixels.
fn image(bytes: &[u8], columns: u16, rows: u16, width: u32, height: u32) -> bool {
    let size = format!("s={width},v={height},");
    let cells = format!("c={columns},r={rows},");
    bytes.split(|&byte| byte == 0x1b).any(|apc| {
        apc.starts_with(b"_Ga=T,") && has(apc, size.as_bytes()) && has(apc, cells.as_bytes())
    })
}

/// The start of each kitty image's pixels (its first chunk), in the order they were sent.
fn images(bytes: &[u8]) -> Vec<&[u8]> {
    bytes
        .split(|&byte| byte == 0x1b)
        .filter(|apc| apc.starts_with(b"_Ga=T,"))
        .filter_map(|apc| apc.iter().position(|&b| b == b';').map(|at| &apc[at + 1..]))
        .collect()
}

/// The printable text of `bytes`, image payloads left out, for messages.
fn printable(bytes: &[u8]) -> String {
    let mut out = String::new();
    for apc in bytes.split(|&byte| byte == 0x1b) {
        let apc = match apc.starts_with(b"_G") {
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
        "the edited address was not demo://pattern\n{}",
        term.context()
    );

    // `t` opens a tab with the address line open; Escape closes it, `J` and `K` switch.
    let since = term.got.len();
    term.type_bytes(&kitty_keys("t"));
    let opened = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "sig: pages aren't shown in the terminal yet")
    });
    assert!(opened, "`t` opened no tab\n{}", term.context());
    assert_eq!(term.tabs(), (2, 2));
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
