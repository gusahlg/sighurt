//! A persistent cookie survives SIGKILL; a session cookie never reaches the profile.
//!
//! The child is this test again (`SIGHURT_COOKIE_CHILD`). It loads a page from a local HTTP
//! server with Servo offscreen and then loops, so the profile writer — not Servo's exit path —
//! is what commits the jar. The parent waits until `cookie_jar.json` contains only the
//! persistent cookie, SIGKILLs that child, and checks that the next process sends only that
//! cookie. `file://` is not used: cookie storage follows an HTTP response.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PumpStatus, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_engine_servo::{Offscreen, ServoConfig, ServoRuntime};

const PERSIST: &str = "persist_value_9f3a";
const SESSION: &str = "session_value_9f3a";
const PAGE: &str = "<!DOCTYPE html><title>cookies</title><p>ok</p>";
const WAIT: Duration = Duration::from_secs(150);
const LOAD_WAIT: Duration = Duration::from_secs(90);

struct Mem {
    metrics: Metrics,
}

struct Permit {
    epoch: LayoutEpoch,
}

impl CpuTarget for Mem {
    type Permit = Permit;

    fn metrics(&self) -> Metrics {
        self.metrics
    }

    fn permit_epoch(permit: &Self::Permit) -> LayoutEpoch {
        permit.epoch
    }

    fn submit(
        &mut self,
        _permit: Self::Permit,
        _paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        Err("this test does not present".to_owned())
    }
}

#[test]
fn persistent_cookie_survives_sigkill() {
    match std::env::var("SIGHURT_COOKIE_CHILD").ok().as_deref() {
        Some("set") | Some("read") => child(),
        Some(role) => panic!("unknown cookie child role {role}"),
        None => parent(),
    }
}

fn child() {
    let profile = PathBuf::from(std::env::var("SIGHURT_COOKIE_PROFILE").expect("profile"));
    let url = std::env::var("SIGHURT_COOKIE_URL").expect("url");
    std::env::remove_var("WAYLAND_DISPLAY");
    std::env::remove_var("DISPLAY");
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut runtime = ServoRuntime::<Offscreen<Mem>>::new(
        ServoConfig {
            profile: Some(profile),
            ..Default::default()
        },
        EngineWaker::new(|| {}),
    );
    runtime.setup_logging();

    let metrics = Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width: 320,
            height: 180,
        },
        Scale::ONE,
    );
    let view = ViewId::next();
    if let Err(error) = runtime.open_view(view, ViewOptions::default(), Offscreen(Mem { metrics }))
    {
        fail(format!("open_view: {error:?}"));
    }
    let navigation = NavigationId::next();
    if let Err(error) = runtime.command(
        view,
        EngineCommand::Navigate {
            navigation,
            url: url.clone(),
        },
    ) {
        fail(format!("navigate: {error:?}"));
    }
    eprintln!("cookie child: loading {url}");

    let start = Instant::now();
    let mut committed = false;
    let mut loading = None;
    loop {
        let mut events = Vec::new();
        let status = runtime.pump(&mut events);
        for event in events {
            match event {
                EngineEvent::NavigationCommitted { .. } => committed = true,
                EngineEvent::LoadingChanged { loading: value, .. } => loading = Some(value),
                EngineEvent::NavigationFailed { error, .. } => {
                    fail(format!("navigation failed: {error:?}"));
                }
                EngineEvent::ViewFailed { message, .. } => fail(format!("view failed: {message}")),
                EngineEvent::RuntimeFailed { .. } => fail("runtime failed"),
                _ => {}
            }
        }
        if committed && loading == Some(false) {
            break;
        }
        if start.elapsed() > LOAD_WAIT {
            fail("page did not finish loading");
        }
        if !matches!(status, PumpStatus::Pending { deadline: None }) {
            thread::sleep(Duration::from_millis(10));
        }
    }
    eprintln!("cookie child: loaded; leaving Servo running for the periodic commit");
    // Must not return: returning drops Servo, and Exit would commit the jar itself.
    loop {
        let mut events = Vec::new();
        let _ = runtime.pump(&mut events);
        thread::sleep(Duration::from_millis(50));
    }
}

fn fail(message: impl std::fmt::Display) -> ! {
    eprintln!("cookie child: {message}");
    // Skip destructors. Dropping Servo here can hang, and the parent is waiting for this process.
    std::process::exit(1);
}

#[derive(Debug)]
struct Hit {
    path: String,
    cookie: Option<String>,
}

struct Server {
    port: u16,
    hits: Arc<Mutex<Vec<Hit>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
        listener.set_nonblocking(true).expect("nonblocking");
        let port = listener.local_addr().expect("addr").port();
        let hits = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let hits_thread = Arc::clone(&hits);
        let stop_thread = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("cookie-server".to_owned())
            .spawn(move || serve(listener, hits_thread, stop_thread))
            .expect("server thread");
        Self {
            port,
            hits,
            stop,
            thread: Some(thread),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(listener: TcpListener, hits: Arc<Mutex<Vec<Hit>>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => handle(stream, &hits),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(20));
            }
            Err(_) if stop.load(Ordering::Relaxed) => break,
            Err(error) => {
                eprintln!("cookie server: {error}");
                break;
            }
        }
    }
}

fn handle(mut stream: TcpStream, hits: &Mutex<Vec<Hit>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let Some(head) = read_head(&mut stream) else {
        return;
    };
    let path = request_path(&head);
    let cookie = header_value(&head, "cookie");
    if path == "/set" || path == "/next" {
        hits.lock().expect("hits").push(Hit {
            path: path.clone(),
            cookie,
        });
    }
    let body = PAGE;
    let extra = if path == "/set" {
        format!(
            "Set-Cookie: persist={PERSIST}; Max-Age=86400; Path=/\r\n\
             Set-Cookie: session={SESSION}; Path=/\r\n"
        )
    } else {
        String::new()
    };
    let status = if path == "/set" || path == "/next" {
        "200 OK"
    } else {
        "404 Not Found"
    };
    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         Connection: close\r\n\
         {extra}\
         \r\n\
         {body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    while !buf.windows(4).any(|window| window == b"\r\n\r\n") {
        if buf.len() > 64 * 1024 {
            return None;
        }
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    buf.windows(4)
        .any(|window| window == b"\r\n\r\n")
        .then(|| String::from_utf8_lossy(&buf).into_owned())
}

fn request_path(head: &str) -> String {
    let line = head.lines().next().unwrap_or("");
    let target = line.split_whitespace().nth(1).unwrap_or("/");
    let path = if let Some(rest) = target
        .strip_prefix("http://")
        .or_else(|| target.strip_prefix("https://"))
    {
        match rest.find('/') {
            Some(index) => &rest[index..],
            None => "/",
        }
    } else {
        target
    };
    path.split('?').next().unwrap_or("/").to_owned()
}

fn header_value(head: &str, name: &str) -> Option<String> {
    for line in head.split("\r\n").skip(1) {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.eq_ignore_ascii_case(name) {
            return Some(value.trim().to_owned());
        }
    }
    None
}

struct Profile {
    path: PathBuf,
    keep: bool,
}

impl Profile {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "sighurt-servo-cookies-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir(&path).expect("profile dir");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o700);
        fs::set_permissions(&path, perms).expect("profile mode");
        eprintln!("cookie profile: {}", path.display());
        Self { path, keep: true }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

struct KillOnDrop(Option<Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(child) = self.0.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn parent() {
    let mut profile = Profile::new();
    let server = Server::start();
    let set_log = profile.path.join("child-set.log");
    let mut set_child = KillOnDrop(Some(spawn_child(
        "set",
        &profile.path,
        &server.url("/set"),
        &set_log,
    )));
    let jar = profile.path.join("cookie_jar.json");
    let text = poll_jar(&mut set_child, &jar, &set_log);
    assert!(
        set_child.0.as_mut().unwrap().try_wait().unwrap().is_none(),
        "the set child must still be running when the jar hits disk"
    );
    assert_eq!(mode(&jar), 0o600, "cookie file mode");
    assert!(
        !profile.path.join("auth_cache.json").exists(),
        "HTTP authentication must not be written"
    );
    assert!(
        !text.contains(SESSION),
        "session cookie reached disk: {text}"
    );
    // SIGKILL, so Servo's Exit path does not run.
    let child = set_child.0.take().unwrap();
    kill_child(child);
    let after = fs::read_to_string(&jar).expect("jar after kill");
    assert!(after.contains(PERSIST), "{after}");
    assert!(!after.contains(SESSION), "{after}");
    assert_eq!(mode(&jar), 0o600);

    let read_log = profile.path.join("child-read.log");
    let mut read_child = KillOnDrop(Some(spawn_child(
        "read",
        &profile.path,
        &server.url("/next"),
        &read_log,
    )));
    let cookie = poll_request(&mut read_child, &server, &read_log);
    assert!(
        cookie.contains(PERSIST),
        "second run did not send the persistent cookie: {cookie}"
    );
    assert!(
        !cookie.contains(SESSION),
        "second run sent the session cookie: {cookie}"
    );
    let child = read_child.0.take().unwrap();
    kill_child(child);
    profile.keep = false;
}

fn spawn_child(role: &str, profile: &Path, url: &str, log_path: &Path) -> Child {
    let log = File::create(log_path).expect("child log");
    Command::new(std::env::current_exe().expect("test executable"))
        .arg("--exact")
        .arg("persistent_cookie_survives_sigkill")
        .arg("--nocapture")
        .env("SIGHURT_COOKIE_CHILD", role)
        .env("SIGHURT_COOKIE_PROFILE", profile)
        .env("SIGHURT_COOKIE_URL", url)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .stdin(Stdio::null())
        .stdout(log.try_clone().expect("log"))
        .stderr(log)
        .spawn()
        .expect("spawn child")
}

fn kill_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn poll_jar(child: &mut KillOnDrop, jar: &Path, log_path: &Path) -> String {
    let start = Instant::now();
    loop {
        if let Some(status) = try_finished(child) {
            panic!(
                "set child exited ({status}) before the jar was committed\n{}",
                tail(log_path)
            );
        }
        if let Ok(text) = fs::read_to_string(jar) {
            if text.contains(PERSIST) {
                assert!(
                    !text.contains(SESSION),
                    "session cookie reached disk: {text}"
                );
                return text;
            }
        }
        if start.elapsed() > WAIT {
            panic!("cookie jar was not committed\n{}", tail(log_path));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn poll_request(child: &mut KillOnDrop, server: &Server, log_path: &Path) -> String {
    let start = Instant::now();
    loop {
        if let Some(status) = try_finished(child) {
            panic!(
                "read child exited ({status}) before /next\n{}",
                tail(log_path)
            );
        }
        let hits = server.hits.lock().expect("hits");
        if hits.iter().any(|hit| {
            hit.path == "/next"
                && hit
                    .cookie
                    .as_deref()
                    .is_some_and(|cookie| cookie.contains(SESSION))
        }) {
            panic!("second run sent the session cookie: {hits:?}");
        }
        if let Some(cookie) = hits.iter().find_map(|hit| {
            let cookie = hit.cookie.as_deref()?;
            (hit.path == "/next" && cookie.contains(PERSIST)).then(|| cookie.to_owned())
        }) {
            return cookie;
        }
        drop(hits);
        if start.elapsed() > WAIT {
            panic!(
                "second run did not send the persistent cookie\n{}",
                tail(log_path)
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn try_finished(child: &mut KillOnDrop) -> Option<String> {
    let proc = child.0.as_mut()?;
    match proc.try_wait() {
        Ok(Some(status)) => Some(status.to_string()),
        Ok(None) => None,
        Err(error) => Some(error.to_string()),
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).expect("metadata").permissions().mode() & 0o777
}

fn tail(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return format!("(no log at {})", path.display());
    };
    let start = text.len().saturating_sub(4000);
    let start = (start..=text.len())
        .find(|index| text.is_char_boundary(*index))
        .unwrap_or(text.len());
    text[start..].to_owned()
}
