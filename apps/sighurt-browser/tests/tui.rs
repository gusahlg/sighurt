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
/// Cells of 10×20 pixels and DA1 with sixel, and no kitty graphics reply.
const SIXEL_REPLIES: &[u8] = b"\x1b[6;20;10t\x1b[?62;4c";
/// [`REPLIES`] after a `q` the user typed while the terminal had not answered yet.
const Q_THEN_REPLIES: &[u8] = b"q\x1b_Gi=31;OK\x1b\\\x1b[6;20;10t\x1b[?62;4c";

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
        let dir = std::env::temp_dir().join(format!("sig-tui-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("config/sighurt")).unwrap();
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

    // Three more Tabs reach the field (link, link, field); Enter edits it, `q` is typed into
    // it, not run, and Enter sets it.
    let since = term.got.len();
    term.type_bytes(b"\t\t\t\r");
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

    // The field is focused, and text mode told the core the page is not typing: `q` quits.
    term.type_bytes(b"q");
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
}

/// With a field focused, graphics lets the page take letters: `q` types, and Ctrl-C still quits.
#[test]
fn graphics_mode_keeps_a_focused_fields_letters_for_the_page() {
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
    term.type_bytes(b"\t\t\t\r");
    let since = term.got.len();
    let editing = term.wait(since, Duration::from_secs(10), |got| shows(got, "Edit:"));
    assert!(editing, "no field editor\n{}", term.context());
    term.type_bytes(b"zq\r");
    let since = term.got.len();
    let set = term.wait(since, Duration::from_secs(10), |got| {
        shows(got, "Typed into the page: zq")
    });
    assert!(set, "field not set\n{}", term.context());

    let since = term.got.len();
    term.type_bytes(b"m");
    let graphics = term.wait(since, Duration::from_secs(10), |got| shows(got, "GFX"));
    assert!(graphics, "`m` did not show graphics\n{}", term.context());
    // `q` goes to the page's field and does not run its binding: sig is still up a second on.
    term.type_bytes(b"q");
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(1) {
        term.wait(term.got.len(), Duration::from_millis(20), |_| false);
        assert!(
            term.child.try_wait().unwrap().is_none(),
            "q quit while the field was focused in graphics\n{}",
            term.context()
        );
    }
    // Ctrl-C quits from anywhere, field or not.
    term.type_bytes(b"\x03");
    assert!(term.exit().success(), "{}", term.context());
    term.assert_restored();
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
            while !rest.is_empty() {
                match rustix::io::write(&self.master, rest) {
                    Ok(n) => rest = &rest[n..],
                    Err(Errno::AGAIN) | Err(Errno::INTR) => {
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
