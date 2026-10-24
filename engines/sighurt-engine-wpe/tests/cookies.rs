//! Persistent cookies in a WPE profile, with no display server.
//!
//! WebKit's UI-process state is once per process, so each runtime here is its own process:
//! this file's tests spawn themselves. Ignored so `cargo test` does not start WebKit.
//! `tests/cookies.sh` runs them on a private D-Bus session with no display.
//!
//! A local page sets a persistent cookie (`Max-Age`) and a session cookie. After the runtime
//! shuts down, or after it and its network process are killed, a second runtime's request
//! carries only the persistent cookie, and `cookies.sqlite` is in the profile.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_engine_wpe::{Offscreen, WpeConfig, WpeRuntime};

const PERSIST: &str = "sighurt_persist=kept";
const SESSION: &str = "sighurt_session=dropped";
const LOAD_WAIT: Duration = Duration::from_secs(45);
const STORE_WAIT: Duration = Duration::from_secs(20);

struct Permit {
    epoch: LayoutEpoch,
    size: BufferSize,
}

struct Mem {
    metrics: Metrics,
}

impl CpuTarget for Mem {
    type Permit = Permit;

    fn metrics(&self) -> Metrics {
        self.metrics
    }

    fn permit_epoch(permit: &Permit) -> LayoutEpoch {
        permit.epoch
    }

    fn submit(
        &mut self,
        permit: Permit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let stride = 4 * permit.size.width as usize;
        let mut pixels = vec![0u8; stride * permit.size.height as usize];
        paint(Frame {
            pixels: &mut pixels,
            size: permit.size,
            stride,
        })?;
        Ok(())
    }
}

struct Page {
    runtime: WpeRuntime<Offscreen<Mem>>,
    view: ViewId,
    epoch: LayoutEpoch,
    size: BufferSize,
    committed: Vec<String>,
    failed: Option<String>,
}

impl Page {
    fn open(profile: &Path) -> Self {
        let metrics = Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width: 640,
                height: 480,
            },
            Scale::ONE,
        );
        let epoch = metrics.epoch();
        let size = metrics.buffer();
        let mut runtime = WpeRuntime::<Offscreen<Mem>>::new(
            WpeConfig {
                profile: Some(profile.to_path_buf()),
                ..WpeConfig::default()
            },
            EngineWaker::new(|| {}),
        )
        .unwrap_or_else(|error| panic!("WpeRuntime::new: {error}"));
        let view = ViewId::next();
        runtime
            .open_view(view, ViewOptions::default(), Offscreen(Mem { metrics }))
            .unwrap_or_else(|error| panic!("open_view: {error}"));
        Self {
            runtime,
            view,
            epoch,
            size,
            committed: Vec::new(),
            failed: None,
        }
    }

    fn step(&mut self) {
        self.runtime
            .present(
                self.view,
                Permit {
                    epoch: self.epoch,
                    size: self.size,
                },
            )
            .unwrap_or_else(|error| panic!("present: {error}"));
        let mut events = Vec::new();
        self.runtime.pump(&mut events);
        for event in events {
            match event {
                EngineEvent::NavigationStarted { url, .. } => println!("started {url}"),
                EngineEvent::NavigationCommitted { url, .. } => {
                    println!("committed {url}");
                    self.committed.push(url);
                }
                EngineEvent::NavigationFailed { error, .. } => {
                    println!("failed {error}");
                    self.failed = Some(error.to_string());
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    fn navigate(&mut self, url: &str) {
        println!("load {url}");
        self.runtime
            .command(
                self.view,
                EngineCommand::Navigate {
                    navigation: NavigationId::next(),
                    url: url.to_owned(),
                },
            )
            .unwrap_or_else(|error| panic!("navigate: {error}"));
    }

    fn wait_commit(&mut self, needle: &str) {
        let start = Instant::now();
        while start.elapsed() < LOAD_WAIT {
            if self.committed.iter().any(|url| url.contains(needle)) {
                return;
            }
            if let Some(error) = &self.failed {
                panic!("navigation failed: {error}");
            }
            self.step();
        }
        panic!("no commit of {needle}; committed {:?}", self.committed);
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        self.runtime.shutdown();
        if std::thread::panicking() {
            reap();
            return;
        }
        wait_webkit_gone();
    }
}

#[derive(Clone, Debug)]
struct Hit {
    path: String,
    cookie: String,
}

fn serve() -> (String, Arc<Mutex<Vec<Hit>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|error| panic!("bind: {error}"));
    let addr = listener.local_addr().unwrap();
    let hits = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&hits);
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut stream) = conn else { continue };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut buf = Vec::new();
            let mut tmp = [0u8; 2048];
            loop {
                match stream.read(&mut tmp) {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 65536 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let text = String::from_utf8_lossy(&buf);
            let path = text.lines().next().unwrap_or("").to_owned();
            let cookie = text
                .lines()
                .find(|line| line.to_ascii_lowercase().starts_with("cookie:"))
                .unwrap_or("")
                .to_owned();
            let set = request_target(&path) == "/set";
            log.lock().expect("hits").push(Hit { path, cookie });
            let extra = if set {
                format!("Set-Cookie: {PERSIST}; Max-Age=86400; Path=/\r\nSet-Cookie: {SESSION}; Path=/\r\n")
            } else {
                String::new()
            };
            let body = "<!DOCTYPE html><title>cookies</title><p>ok</p>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    (format!("http://{addr}"), hits)
}

fn request_target(line: &str) -> &str {
    let target = line.split_whitespace().nth(1).unwrap_or("");
    target.split('?').next().unwrap_or(target)
}

fn snapshot(hits: &Mutex<Vec<Hit>>) -> Vec<Hit> {
    hits.lock().expect("hits").clone()
}

fn show(hits: &[Hit]) -> String {
    if hits.is_empty() {
        return "(no requests)".into();
    }
    hits.iter()
        .map(|hit| format!("{} | {}", hit.path, hit.cookie))
        .collect::<Vec<_>>()
        .join("\n")
}

fn cookie_on<'a>(hits: &'a [Hit], path: &str) -> Option<&'a str> {
    hits.iter()
        .rev()
        .find(|hit| request_target(&hit.path).ends_with(path))
        .map(|hit| hit.cookie.as_str())
}

fn cookie_db(profile: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    for name in ["cookies.sqlite", "cookies.sqlite-wal"] {
        if let Ok(more) = std::fs::read(profile.join(name)) {
            bytes.extend(more);
        }
    }
    bytes
}

fn has_bytes(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

/// The persistent cookie is in the database and the session cookie is not.
fn cookie_db_ok(profile: &Path) -> bool {
    let bytes = cookie_db(profile);
    has_bytes(&bytes, "sighurt_persist") && !has_bytes(&bytes, "sighurt_session")
}

struct TempProfile(PathBuf);

impl Drop for TempProfile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp_profile(test: &str) -> TempProfile {
    let path =
        std::env::temp_dir().join(format!("sighurt-wpe-cookies-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir(&path).unwrap_or_else(|error| panic!("create {}: {error}", path.display()));
    // Wider than 0700, so the runtime has to tighten it before WebKit writes.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    TempProfile(path)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn spawn_phase(test: &str, phase: &str, hold: bool, url: &str, profile: &Path) -> Child {
    let mut cmd = Command::new(std::env::current_exe().unwrap());
    cmd.env("SIGHURT_WPE_COOKIE_PHASE", phase)
        .env("SIGHURT_WPE_COOKIE_URL", url)
        .env("SIGHURT_WPE_COOKIE_PROFILE", profile)
        .env("SIGHURT_WPE_COOKIE_HOLD", if hold { "1" } else { "0" })
        .args([
            "--exact",
            test,
            "--include-ignored",
            "--nocapture",
            "--test-threads",
            "1",
        ]);
    // SAFETY: setpgid is async-signal-safe. This runs in the forked child, before exec, so
    // WebKit's network and web processes share the child's process group.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    cmd.spawn()
        .unwrap_or_else(|error| panic!("spawn {phase}: {error}"))
}

fn proc_parents() -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(rest) = stat.rsplit_once(')').map(|(_, rest)| rest) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let _state = fields.next();
        let Some(ppid) = fields.next().and_then(|s| s.parse().ok()) else {
            continue;
        };
        out.push((pid, ppid));
    }
    out
}

/// Whether `pid` was started for this cookie test. WebKit's children inherit the variable,
/// so a network process that called `setsid` is still ours and not another browser's.
fn marked(pid: u32) -> bool {
    let Ok(env) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return false;
    };
    const NEEDLE: &[u8] = b"SIGHURT_WPE_COOKIE_PHASE=";
    env.windows(NEEDLE.len()).any(|window| window == NEEDLE)
}

/// Kills `root`, every descendant (WebKit's network and web processes included), the
/// process group `root` leads, and any process still carrying this test's phase variable.
fn kill_tree(root: u32) {
    let procs = proc_parents();
    let mut pending = vec![root];
    let mut order = Vec::new();
    while let Some(pid) = pending.pop() {
        order.push(pid);
        for &(child, ppid) in &procs {
            if ppid == pid {
                pending.push(child);
            }
        }
    }
    for pid in order.into_iter().rev() {
        let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
    }
    let _ = unsafe { libc::kill(-(root as i32), libc::SIGKILL) };
    let self_pid = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if pid != self_pid && marked(pid) {
            let _ = unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
    }
}

fn reap() {
    loop {
        let status = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if status <= 0 {
            break;
        }
    }
}

fn is_webkit(pid: u32) -> bool {
    let Ok(cmd) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let text = String::from_utf8_lossy(&cmd);
    text.contains("WPENetworkProcess") || text.contains("WPEWebProcess")
}

fn webkit_ours() -> Vec<u32> {
    let procs = proc_parents();
    let mut family = vec![std::process::id()];
    let mut index = 0;
    while index < family.len() {
        let parent = family[index];
        index += 1;
        for &(pid, ppid) in &procs {
            if ppid == parent && !family.contains(&pid) {
                family.push(pid);
            }
        }
    }
    let mut ids: Vec<u32> = family.into_iter().filter(|&pid| is_webkit(pid)).collect();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return ids;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        if !ids.contains(&pid) && marked(pid) && is_webkit(pid) {
            ids.push(pid);
        }
    }
    ids
}

fn wait_webkit_gone() {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        reap();
        if webkit_ours().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("WebKit processes still running: {:?}", webkit_ours());
}

fn profile_files(profile: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(profile) else {
        return "(unreadable)".into();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    if names.is_empty() {
        "(empty)".into()
    } else {
        names.join(", ")
    }
}

fn prepare() {
    assert!(
        std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none(),
        "this test refuses a display; run tests/cookies.sh"
    );
    assert!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
        "this test needs a private session bus; run tests/cookies.sh"
    );
    if std::env::var_os("JSC_SIGNAL_FOR_GC").is_none() {
        // SAFETY: set before WebKit starts, as the other WPE tests do.
        unsafe { std::env::set_var("JSC_SIGNAL_FOR_GC", "12") };
    }
    unsafe {
        std::env::remove_var("LIBGL_ALWAYS_SOFTWARE");
        std::env::remove_var("DRI_PRIME");
    }
}

fn run_phase() {
    prepare();
    let phase = std::env::var("SIGHURT_WPE_COOKIE_PHASE").unwrap();
    let url = std::env::var("SIGHURT_WPE_COOKIE_URL").unwrap();
    let profile = PathBuf::from(std::env::var("SIGHURT_WPE_COOKIE_PROFILE").unwrap());
    let hold = std::env::var("SIGHURT_WPE_COOKIE_HOLD").ok().as_deref() == Some("1");
    match phase.as_str() {
        "set" => phase_set(&url, &profile, hold),
        "read" => phase_read(&url, &profile),
        other => panic!("unknown cookie phase {other}"),
    }
}

fn phase_set(base: &str, profile: &Path, hold: bool) {
    let mut page = Page::open(profile);
    page.navigate(&format!("{base}/set"));
    page.wait_commit("/set");
    let deadline = Instant::now() + STORE_WAIT;
    loop {
        if cookie_db_ok(profile) {
            // Seen twice, with a beat between, so the name is a committed write.
            page.step();
            std::thread::sleep(Duration::from_millis(200));
            if cookie_db_ok(profile) {
                break;
            }
        }
        if Instant::now() > deadline {
            panic!(
                "cookies.sqlite did not store only the persistent cookie in {} ({})",
                profile.display(),
                profile_files(profile)
            );
        }
        page.step();
    }
    page.navigate(&format!("{base}/probe"));
    page.wait_commit("/probe");
    if hold {
        loop {
            page.step();
        }
    }
}

fn phase_read(base: &str, profile: &Path) {
    let mut page = Page::open(profile);
    page.navigate(&format!("{base}/read"));
    page.wait_commit("/read");
}

fn require_cookie(hits: &[Hit], path: &str, persistent: bool, session: bool) {
    let Some(cookie) = cookie_on(hits, path) else {
        panic!("no request for {path}\n{}", show(hits));
    };
    assert_eq!(
        cookie.contains(PERSIST),
        persistent,
        "{path} persistent cookie: {cookie}\n{}",
        show(hits)
    );
    assert_eq!(
        cookie.contains("sighurt_session"),
        session,
        "{path} session cookie: {cookie}\n{}",
        show(hits)
    );
}

fn orchestrate(test: &str, kill: bool) {
    prepare();
    let (base, hits) = serve();
    let profile = temp_profile(test);
    println!("profile {} server {base} kill={kill}", profile.0.display());
    let mut child = spawn_phase(test, "set", kill, &base, &profile.0);
    if kill {
        let deadline = Instant::now() + LOAD_WAIT + STORE_WAIT + LOAD_WAIT;
        loop {
            let seen = snapshot(&hits);
            if cookie_db_ok(&profile.0)
                && cookie_on(&seen, "/probe").is_some_and(|cookie| {
                    cookie.contains(PERSIST) && cookie.contains("sighurt_session")
                })
            {
                // The probe saw both cookies. Pause so the jar's write is committed, then kill.
                std::thread::sleep(Duration::from_millis(400));
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => panic!(
                    "set phase exited before it could be killed: {status}\n{}",
                    show(&seen)
                ),
                Ok(None) => {}
                Err(error) => panic!("set phase: {error}"),
            }
            if Instant::now() > deadline {
                kill_tree(child.id());
                let _ = child.wait();
                panic!(
                    "cookies were not stored before the timeout\n{}",
                    show(&snapshot(&hits))
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        kill_tree(child.id());
        let status = child.wait().expect("reap set phase");
        println!("killed set phase: {status}");
        assert!(
            !status.success(),
            "SIGKILL should not look like a clean exit"
        );
    } else {
        let status = child.wait().expect("reap set phase");
        assert!(status.success(), "set phase exited {status}");
    }
    assert!(
        profile.0.join("cookies.sqlite").is_file(),
        "cookies.sqlite missing in {}",
        profile.0.display()
    );
    assert!(
        cookie_db_ok(&profile.0),
        "cookies.sqlite should hold the persistent cookie and not the session cookie"
    );
    assert_eq!(mode(&profile.0), 0o700, "profile directory");
    require_cookie(&snapshot(&hits), "/probe", true, true);
    let mut reader = spawn_phase(test, "read", false, &base, &profile.0);
    let status = reader.wait().expect("reap read phase");
    let seen = snapshot(&hits);
    assert!(
        status.success(),
        "read phase exited {status}\n{}",
        show(&seen)
    );
    require_cookie(&seen, "/read", true, false);
    println!("requests:\n{}", show(&seen));
}

#[test]
#[ignore = "starts WebKit; run tests/cookies.sh"]
fn persistent_cookies_survive_shutdown() {
    if std::env::var_os("SIGHURT_WPE_COOKIE_PHASE").is_some() {
        run_phase();
        return;
    }
    orchestrate("persistent_cookies_survive_shutdown", false);
}

#[test]
#[ignore = "starts WebKit; run tests/cookies.sh"]
fn persistent_cookies_survive_sigkill() {
    if std::env::var_os("SIGHURT_WPE_COOKIE_PHASE").is_some() {
        run_phase();
        return;
    }
    orchestrate("persistent_cookies_survive_sigkill", true);
}
