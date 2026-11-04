//! The runtime against a local HTTP server and a fake CPU target. NetSurf is one runtime per
//! process, so this binary has one test that walks through every scenario in order (find in
//! page among them).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, FindCommand, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::{EngineEvent, HistoryEffect, NavigationError, OpenDisposition};
use sighurt_api::geometry::{Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, RequestId, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers, PointerButton,
    PointerEvent, ScrollDelta, TextPurpose,
};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_api::services::{ClipboardData, Service, ServiceReply};
use sighurt_engine_netsurf::{NetSurfConfig, NetSurfRuntime};

// ---------------------------------------------------------------------------- the target

/// A painted frame: width, height, pixels.
type Picture = (u32, u32, Vec<u32>);

#[derive(Clone)]
struct Mem {
    metrics: Arc<Mutex<Metrics>>,
    last: Arc<Mutex<Option<Picture>>>,
}

struct Permit(LayoutEpoch);

impl CpuTarget for Mem {
    type Permit = Permit;

    fn metrics(&self) -> Metrics {
        *self.metrics.lock().unwrap()
    }

    fn permit_epoch(permit: &Permit) -> LayoutEpoch {
        permit.0
    }

    fn submit(
        &mut self,
        _: Permit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let size = self.metrics().buffer();
        let (w, h) = (size.width as usize, size.height as usize);
        let mut words = vec![0u32; w * h];
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), w * h * 4) };
        paint(Frame {
            pixels: bytes,
            size,
            stride: w * 4,
        })?;
        *self.last.lock().unwrap() = Some((size.width, size.height, words));
        Ok(())
    }
}

fn metrics(width: u32, height: u32, scale_120: u32) -> Metrics {
    Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width,
            height,
        },
        Scale::from_fractional(scale_120).unwrap(),
    )
}

// ---------------------------------------------------------------------------- the server

const HOME: &str = "<!doctype html><title>Smoke Page</title><body style='margin:0;font:20px sans-serif'>\
<h1 style='margin:0;height:60px'>Heading</h1>\
<a href='/second' style='position:absolute;left:0;top:100px;width:300px;height:60px;background:#cfc'>second page</a>\
<a href='/blank-target' target=_blank style='position:absolute;left:0;top:200px;width:300px;height:60px;background:#ccf'>new window</a>\
<a href='/file.bin' style='position:absolute;left:0;top:300px;width:300px;height:60px;background:#fcc'>download</a>\
<a href='mailto:me@example.org' style='position:absolute;left:0;top:470px;width:300px;height:40px;background:#ffc'>mail</a>\
<a href='mailto:you@example.org' target=_blank style='position:absolute;left:0;top:520px;width:300px;height:40px;background:#cff'>mail in a new window</a>\
<form method=post enctype='multipart/form-data' action='about:query/ssl'>\
<input type=hidden name=siteurl value='https://127.0.0.1:@TLS@/'><input type=hidden name=proceed value=1>\
<input type=submit value='proceed anyway' style='position:absolute;left:0;top:400px;width:300px;height:60px'></form>";

const FORM: &str = "<!doctype html><meta charset=utf-8><title>Form</title><body style='margin:0'>\
<form action='/echo' method=get><input type=text name=q style='position:absolute;left:0;top:0;width:300px;height:40px;font:20px sans-serif'></form>";

const FIELDS: &str =
    "<!doctype html><meta charset=utf-8><title>Fields</title><body style='margin:0'>\
<input type=password name=p style='position:absolute;left:0;top:0;width:300px;height:40px'>\
<textarea name=t style='position:absolute;left:0;top:100px;width:300px;height:60px'></textarea>\
<input type=text name=q style='position:absolute;left:0;top:200px;width:300px;height:40px'>";

/// A 1x1 GIF of two frames, black then white, 100 ms each, looping.
const ANIM_GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\
\x21\xff\x0bNETSCAPE2.0\x03\x01\x00\x00\x00\
\x21\xf9\x04\x00\x0a\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00\
\x21\xf9\x04\x00\x0a\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x4c\x01\x00\
\x3b";

/// A link longer than the 4 KiB the frontend's first buffer holds.
fn long_link_path() -> String {
    format!("/second?{}", "x".repeat(6000))
}

fn long_page() -> String {
    let mut page =
        String::from("<!doctype html><title>Long</title><body style='font:20px sans-serif'>");
    for i in 0..200 {
        page.push_str(&format!("<p>line number {i}</p>"));
    }
    page
}

struct Server {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start(tls_port: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let log = Arc::clone(&log);
                std::thread::spawn(move || {
                    let mut buf = [0u8; 4096];
                    let n = stream.read(&mut buf).unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    log.lock().unwrap().push(path.clone());
                    let (kind, extra, body): (&str, &str, Vec<u8>) =
                        match path.split('?').next().unwrap() {
                            "/" => (
                                "text/html",
                                "",
                                HOME.replace("@TLS@", &tls_port.to_string()).into(),
                            ),
                            "/second" => (
                                "text/html",
                                "",
                                b"<!doctype html><title>Second</title><p>two".to_vec(),
                            ),
                            "/find" => (
                                "text/html",
                                "",
                                b"<!doctype html><title>Find</title><p>apple one<p>Apple two\
                                  <p>apple three<p style='margin-top: 1500px'>apple four"
                                    .to_vec(),
                            ),
                            "/filemeta" => (
                            "text/html",
                            "",
                            b"<!doctype html><title>Meta</title><meta http-equiv=refresh content='0;url=file:///etc/hostname'><p>x".to_vec(),
                        ),
                        "/hostile" => (
                            "text/html",
                            "",
                            b"<!doctype html><title>Hostile</title><body style='margin:0'>\
<input type=radio style='width:200000px;height:200000px'>\
<div style='width:2000000px;border-top:1000px dashed #000'></div>\
<div style='width:3000px;height:3000px;border:500px dotted red'></div>".to_vec(),
                        ),
                        "/framehost" => (
                            "text/html",
                            "",
                            b"<!doctype html><title>Host</title><body style='margin:0'>\
<iframe src='/frame' style='position:absolute;left:0;top:0;width:400px;height:200px;border:0'></iframe>".to_vec(),
                        ),
                        "/frame" => (
                            "text/html",
                            "",
                            b"<!doctype html><meta http-equiv=refresh content=1><body style='margin:0'>\
<input type=file style='position:absolute;left:0;top:0;width:300px;height:60px'>".to_vec(),
                        ),
                        "/fileroot" => (
                            "text/html",
                            "",
                            b"<!doctype html><title>File</title><body style='margin:0'>\
<input type=file style='position:absolute;left:0;top:0;width:300px;height:60px'>".to_vec(),
                        ),
                        "/form" => ("text/html", "", FORM.into()),
                        "/fields" => ("text/html", "", FIELDS.into()),
                        "/anim" => (
                            "text/html",
                            "",
                            b"<!doctype html><title>Anim</title><body style='margin:0'>\
<img src='/anim.gif' style='width:100px;height:100px'>".to_vec(),
                        ),
                        "/anim.gif" => ("image/gif", "", ANIM_GIF.to_vec()),
                        "/longlink" => (
                            "text/html",
                            "",
                            format!(
                                "<!doctype html><title>Long link</title><body style='margin:0'>\
<a href='{}' style='position:absolute;left:0;top:0;width:300px;height:60px;background:#cfc'>long</a>",
                                long_link_path()
                            )
                            .into_bytes(),
                        ),
                            "/long" => ("text/html", "", long_page().into_bytes()),
                            "/blank-target" => (
                                "text/html",
                                "",
                                b"<!doctype html><title>Blank</title>".to_vec(),
                            ),
                            "/echo" => (
                                "text/html",
                                "",
                                b"<!doctype html><title>Echo</title><p>echo".to_vec(),
                            ),
                            "/slow" => {
                                std::thread::sleep(Duration::from_secs(2));
                                (
                                    "text/html",
                                    "",
                                    b"<!doctype html><title>Slow</title>".to_vec(),
                                )
                            }
                            "/file.bin" => (
                                "application/octet-stream",
                                "Content-Disposition: attachment; filename=\"data.bin\"\r\n",
                                b"0123456789".to_vec(),
                            ),
                            _ => ("text/plain", "", b"not found".to_vec()),
                        };
                    let charset = if kind == "text/html" {
                        "; charset=utf-8"
                    } else {
                        ""
                    };
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: {kind}{charset}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(head.as_bytes());
                    let _ = stream.write_all(&body);
                });
            }
        });
        Self { port, requests }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn saw(&self, needle: &str) -> bool {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains(needle))
    }
}

// ---------------------------------------------------------------------------- the harness

struct Harness {
    runtime: NetSurfRuntime<Mem>,
    waker: EngineWaker,
    wakes: Arc<AtomicUsize>,
    pumps: usize,
    next_deadline: Option<Instant>,
    mem: Mem,
    view: ViewId,
    epoch: LayoutEpoch,
}

impl Harness {
    fn pump(&mut self) -> Vec<EngineEvent> {
        let mut events = Vec::new();
        self.pumps += 1;
        match self.runtime.pump(&mut events) {
            PumpStatus::Idle => self.next_deadline = None,
            PumpStatus::Pending { deadline } => {
                self.next_deadline = Some(deadline.unwrap_or_else(Instant::now))
            }
        }
        events
    }

    /// Pumps like the owner loop does: when woken or when the deadline passes, and sleeps
    /// otherwise. Returns every event until `done` accepts the events so far.
    fn until(
        &mut self,
        what: &str,
        mut done: impl FnMut(&[EngineEvent]) -> bool,
    ) -> Vec<EngineEvent> {
        let end = Instant::now() + Duration::from_secs(15);
        let mut all = Vec::new();
        loop {
            all.extend(self.pump());
            if done(&all) {
                return all;
            }
            assert!(
                Instant::now() < end,
                "timed out waiting for {what}; events: {all:#?}"
            );
            // Sleep until the deadline or a wake.
            let until = self
                .next_deadline
                .unwrap_or_else(|| Instant::now() + Duration::from_millis(50));
            while Instant::now() < until && !self.waker.take() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    }

    /// The owner loop for `span` with no input: pumps only when woken or when the engine's
    /// deadline passes, as the real loop does.
    fn idle_for(&mut self, span: Duration) -> Vec<EngineEvent> {
        let end = Instant::now() + span;
        let mut all = Vec::new();
        while Instant::now() < end {
            all.extend(self.pump());
            let until = self.next_deadline.map_or(end, |d| d.min(end));
            while Instant::now() < until && !self.waker.take() {
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        all
    }

    fn command(&mut self, command: EngineCommand) {
        self.runtime.command(self.view, command).unwrap();
    }

    fn navigate(&mut self, url: &str) -> NavigationId {
        let navigation = NavigationId::next();
        self.command(EngineCommand::Navigate {
            navigation,
            url: url.into(),
        });
        navigation
    }

    fn load(&mut self, url: &str) -> Vec<EngineEvent> {
        self.navigate(url);
        let mut committed = false;
        self.until(url, |events| {
            committed |= events
                .iter()
                .any(|e| matches!(e, EngineEvent::NavigationCommitted { .. }));
            committed
                && matches!(
                    events.iter().rev().find_map(|e| match e {
                        EngineEvent::LoadingChanged { loading, .. } => Some(*loading),
                        _ => None,
                    }),
                    Some(false)
                )
        })
    }

    fn set_metrics(&mut self, m: Metrics) {
        *self.mem.metrics.lock().unwrap() = m;
        self.epoch = m.epoch();
        self.command(EngineCommand::SetMetrics(m));
    }

    fn frame(&mut self) -> Picture {
        let _ = self.pump();
        let outcome = self.runtime.present(self.view, Permit(self.epoch)).unwrap();
        // Unchanged: nothing moved since the last frame, which is still the picture.
        assert!(
            matches!(
                outcome,
                PresentOutcome::Submitted | PresentOutcome::Unchanged
            ),
            "{outcome:?}"
        );
        self.mem.last.lock().unwrap().clone().unwrap()
    }

    fn pointer(&mut self, event: PointerEvent) {
        self.command(EngineCommand::Input(InputEvent {
            seat: SeatId::next(),
            kind: InputKind::Pointer(event),
        }));
    }

    fn click(&mut self, x: f64, y: f64) {
        let position = SurfacePoint { x, y };
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.pointer(PointerEvent::Button {
                button: PointerButton::Left,
                state,
                position,
                modifiers: Modifiers::default(),
            });
        }
    }

    fn key(&mut self, key: Key, text: Option<&str>, modifiers: Modifiers) {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.command(EngineCommand::Input(InputEvent {
                seat: SeatId::next(),
                kind: InputKind::Key(KeyEvent {
                    code: KeyCode(0),
                    key: key.clone(),
                    state,
                    repeat: false,
                    modifiers,
                    consumed: Modifiers::default(),
                    text: text.map(str::to_owned),
                }),
            }));
        }
    }
}

fn dark_box(frame: &Picture, rows: std::ops::Range<usize>) -> (usize, usize) {
    let (w, _, px) = frame;
    let (mut top, mut bottom) = (usize::MAX, 0);
    for y in rows {
        let dark = (0..*w as usize).any(|x| {
            let v = px[y * *w as usize + x];
            (v & 0xff) < 0x60 && (v >> 8 & 0xff) < 0x60 && (v >> 16 & 0xff) < 0x60
        });
        if dark {
            top = top.min(y);
            bottom = bottom.max(y);
        }
    }
    (top, bottom)
}

fn non_white(frame: &Picture) -> usize {
    frame.2.iter().filter(|&&p| p != 0xffff_ffff).count()
}

/// An HTTPS server with a self-signed certificate (`openssl s_server`), killed on drop.
struct TlsServer {
    child: std::process::Child,
    port: u16,
}

impl TlsServer {
    fn start() -> Self {
        let dir = std::env::temp_dir().join(format!("sighurt-ns-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (key, cert) = (dir.join("key.pem"), dir.join("cert.pem"));
        let made = std::process::Command::new("openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            ])
            .args([
                "-subj",
                "/CN=127.0.0.1",
                "-addext",
                "subjectAltName=IP:127.0.0.1",
            ])
            .arg("-keyout")
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .output()
            .expect("the openssl command (the repository's nix-shell has it)");
        assert!(made.status.success(), "openssl req: {made:?}");
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let child = std::process::Command::new("openssl")
            .args(["s_server", "-www", "-quiet", "-accept", &port.to_string()])
            .arg("-cert")
            .arg(&cert)
            .arg("-key")
            .arg(&key)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // Owned from here on: it is killed and waited for when this drops, even on a panic.
        let server = Self { child, port };
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return server;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("openssl s_server did not start");
    }
}

impl Drop for TlsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn has(events: &[EngineEvent], f: impl Fn(&EngineEvent) -> bool) -> bool {
    events.iter().any(f)
}

#[test]
fn the_runtime_walks_through_every_scenario() {
    let tls = TlsServer::start();
    let server = Server::start(tls.port);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    let waker = EngineWaker::new(move || {
        counted.fetch_add(1, Ordering::SeqCst);
    });
    let runtime =
        NetSurfRuntime::<Mem>::new(NetSurfConfig::default(), waker.clone()).expect("start");
    let m = metrics(800, 600, 120);
    let mem = Mem {
        metrics: Arc::new(Mutex::new(m)),
        last: Arc::new(Mutex::new(None)),
    };
    let view = ViewId::next();
    let mut h = Harness {
        runtime,
        waker,
        wakes,
        pumps: 0,
        next_deadline: None,
        mem: mem.clone(),
        view,
        epoch: m.epoch(),
    };
    h.runtime
        .open_view(view, ViewOptions::default(), mem)
        .unwrap();
    let events = h.pump();
    assert_eq!(events, vec![EngineEvent::ViewReady { view }]);

    // --- step 4: open, navigate, events in the contract's order, present.
    let events = h.load(&server.url("/"));
    let order: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            EngineEvent::NavigationStarted { .. } => Some("started"),
            EngineEvent::LoadingChanged { loading: true, .. } => Some("loading"),
            EngineEvent::NavigationCommitted { .. } => Some("committed"),
            EngineEvent::TitleChanged { .. } => Some("title"),
            EngineEvent::LoadingChanged { loading: false, .. } => Some("loaded"),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        ["started", "loading", "committed", "title", "loaded"],
        "{events:#?}"
    );
    assert!(has(
        &events,
        |e| matches!(e, EngineEvent::TitleChanged { title: Some(t), .. } if t == "Smoke Page")
    ));
    assert!(has(
        &events,
        |e| matches!(e, EngineEvent::NavigationCommitted { url, history: Some(HistoryEffect::Push), .. } if *url == server.url("/"))
    ));
    let first = h.frame();
    assert_eq!((first.0, first.1), (800, 600));
    assert!(non_white(&first) > 5000, "the page drew something");
    // The permit of an epoch the core has not seen is not ready.
    assert_eq!(
        h.runtime
            .present(view, Permit(LayoutEpoch::next()))
            .unwrap(),
        PresentOutcome::NotReady
    );

    // --- TLS fails closed. A bad certificate is a failed load; a page cannot grant itself an
    // exception through NetSurf's own about:query form, and the form does nothing.
    let secure = format!("https://127.0.0.1:{}/", tls.port);
    let failed = h.navigate(&secure);
    let events = h.until("the bad certificate", |ev| {
        has(ev, |e| matches!(e, EngineEvent::NavigationFailed { navigation, .. } if *navigation == failed))
    });
    assert!(
        !has(&events, |e| matches!(
            e,
            EngineEvent::NavigationCommitted { .. }
        )),
        "{events:#?}"
    );
    let _ = h.load(&server.url("/"));
    h.click(50.0, 430.0);
    let events = h.idle_for(Duration::from_millis(500));
    assert!(
        !has(&events, |e| matches!(
            e,
            EngineEvent::NavigationCommitted { .. } | EngineEvent::NavigationStarted { .. }
        )),
        "the about:query form did something: {events:#?}"
    );
    let again = h.navigate(&secure);
    let events = h.until("the bad certificate again", |ev| {
        has(ev, |e| matches!(e, EngineEvent::NavigationFailed { navigation, .. } if *navigation == again))
    });
    assert!(
        !has(&events, |e| matches!(
            e,
            EngineEvent::NavigationCommitted { .. }
        )),
        "{events:#?}"
    );
    let _ = h.load(&server.url("/"));

    // --- a scheme NetSurf cannot fetch is an external URL of the view the click was in (also
    // from a target=_blank link, whose spawned window must not linger).
    let _ = h.load(&server.url("/"));
    for y in [490.0, 540.0] {
        h.click(50.0, y);
        let request = h
            .pump()
            .iter()
            .find_map(|e| match e {
                EngineEvent::NavigationRequested { request, .. } => Some(*request),
                _ => None,
            })
            .expect("a mailto link asks first");
        h.command(EngineCommand::AnswerNavigation {
            request,
            allow: true,
        });
        let events = h.pump();
        assert!(
            has(
                &events,
                |e| matches!(e, EngineEvent::ServiceRequested(r) if r.view == view && matches!(&r.service, Service::OpenExternal { url } if url.starts_with("mailto:")))
            ),
            "{events:#?}"
        );
    }

    // --- a web page cannot send the tab to a file: URL (a meta refresh), though the browser can.
    let _ = h.load(&server.url("/filemeta"));
    let events = h.idle_for(Duration::from_millis(1500));
    assert!(
        !has(
            &events,
            |e| matches!(e, EngineEvent::NavigationCommitted { url, .. } if url.starts_with("file:"))
        ),
        "{events:#?}"
    );
    let _ = h.load("file:///etc/hostname");

    // --- a page cannot make a paint expensive: a huge radio button, a huge dashed border.
    let _ = h.load(&server.url("/hostile"));
    let began = Instant::now();
    let _ = h.frame();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "the hostile page took {:?} to paint",
        began.elapsed()
    );
    let _ = h.load(&server.url("/"));

    // --- a file input in a frame is refused (its gadget dies with the frame's document); one in
    // the page asks, and answering after the page went away does nothing.
    let _ = h.load(&server.url("/framehost"));
    h.click(50.0, 30.0);
    let events = h.idle_for(Duration::from_millis(300));
    assert!(
        !has(
            &events,
            |e| matches!(e, EngineEvent::ServiceRequested(r) if matches!(r.service, Service::PickFiles { .. }))
        ),
        "{events:#?}"
    );
    let _ = h.load(&server.url("/fileroot"));
    h.click(50.0, 30.0);
    let events = h.pump();
    let pick = events.iter().find_map(|e| match e {
        EngineEvent::ServiceRequested(r) if matches!(r.service, Service::PickFiles { .. }) => {
            Some(r.id)
        }
        _ => None,
    });
    let pick = pick.expect("a file input in the page asks");
    let _ = h.load(&server.url("/second"));
    h.command(EngineCommand::ResolveServiceRequest {
        request: pick,
        result: Ok(ServiceReply::Files(vec!["/etc/hostname".into()])),
    });
    let _ = h.pump();
    let _ = h.load(&server.url("/"));

    // --- idle: nothing ticks. Two seconds of owner loop with no input.
    let pumps_before = h.pumps;
    let _ = h.idle_for(Duration::from_secs(2));
    assert!(
        h.pumps - pumps_before < 8,
        "the idle loop pumped {} times",
        h.pumps - pumps_before
    );

    // --- a second navigation supersedes the first.
    let slow = h.navigate(&server.url("/slow"));
    let second = h.navigate(&server.url("/second"));
    let events = h.until("the second page", |ev| has(ev, |e| matches!(e, EngineEvent::NavigationCommitted { navigation, .. } if *navigation == second)));
    assert!(has(
        &events,
        |e| matches!(e, EngineEvent::NavigationFailed { navigation, error: NavigationError::Cancelled, .. } if *navigation == slow)
    ));

    // --- step 5: scale, resize, hidden, empty.
    let _ = h.load(&server.url("/"));
    let one = h.frame();
    let heading_one = dark_box(&one, 0..120);
    h.set_metrics(metrics(800, 600, 240));
    let two = h.frame();
    assert_eq!((two.0, two.1), (1600, 1200));
    let heading_two = dark_box(&two, 0..240);
    let (a, b) = (
        heading_one.1 - heading_one.0 + 1,
        heading_two.1 - heading_two.0 + 1,
    );
    assert!(
        (b as f64 / a as f64 - 2.0).abs() < 0.2,
        "the heading is {a} px at scale 1 and {b} at 2"
    );
    h.set_metrics(metrics(500, 400, 120));
    let small = h.frame();
    assert_eq!((small.0, small.1), (500, 400));
    // No room: the window keeps its last size, and presents nothing for the empty epoch.
    let empty = metrics(0, 0, 120);
    h.set_metrics(empty);
    let _ = h.pump();
    assert_eq!(
        h.runtime.present(view, Permit(empty.epoch())).unwrap(),
        PresentOutcome::NotReady
    );
    h.set_metrics(metrics(800, 600, 120));
    assert_eq!(h.frame().0, 800);
    h.command(EngineCommand::SetVisibility(false));
    let _ = h.pump();
    assert_eq!(
        h.runtime.present(view, Permit(h.epoch)).unwrap(),
        PresentOutcome::NotReady
    );
    h.command(EngineCommand::SetVisibility(true));
    let _ = h.frame();
    h.command(EngineCommand::SetZoom(2.0));
    let zoomed = h.frame();
    let heading_zoomed = dark_box(&zoomed, 0..240);
    assert!(
        heading_zoomed.1 - heading_zoomed.0 > 3 * (a / 2),
        "zoom enlarges the page"
    );
    h.command(EngineCommand::SetZoom(1.0));

    // --- step 6/8: a link click is held for the browser; deny drops it, allow follows it.
    let _ = h.frame();
    h.click(50.0, 130.0);
    let events = h.pump();
    let (request, url) = events
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationRequested { request, url, .. } => Some((*request, url.clone())),
            _ => None,
        })
        .expect("a click on a link asks first");
    assert_eq!(url, server.url("/second"));
    h.command(EngineCommand::AnswerNavigation {
        request,
        allow: false,
    });
    let events = h.idle_for(Duration::from_millis(500));
    assert!(
        !has(&events, |e| matches!(
            e,
            EngineEvent::NavigationCommitted { .. }
        )),
        "a denied click navigated"
    );
    h.click(50.0, 130.0);
    let events = h.pump();
    let request = events
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationRequested { request, .. } => Some(*request),
            _ => None,
        })
        .unwrap();
    h.command(EngineCommand::AnswerNavigation {
        request,
        allow: true,
    });
    let events = h.until("the followed link", |ev| has(ev, |e| matches!(e, EngineEvent::NavigationCommitted { url, .. } if *url == server.url("/second"))));
    assert!(
        has(&events, |e| matches!(
            e,
            EngineEvent::NavigationStarted { .. }
        )),
        "a page-started navigation is announced"
    );

    // --- target=_blank opens a new view through the browser; a download is a service request.
    let _ = h.load(&server.url("/"));
    h.click(50.0, 230.0);
    let request = h
        .pump()
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationRequested { request, .. } => Some(*request),
            _ => None,
        })
        .unwrap();
    h.command(EngineCommand::AnswerNavigation {
        request,
        allow: true,
    });
    let events = h.until("an open request", |ev| {
        has(ev, |e| matches!(e, EngineEvent::OpenRequested { .. }))
    });
    assert!(
        has(
            &events,
            |e| matches!(e, EngineEvent::OpenRequested { url, disposition: OpenDisposition::NewView, .. } if *url == server.url("/blank-target"))
        ),
        "{events:#?}"
    );
    h.click(50.0, 330.0);
    let request = h
        .pump()
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationRequested { request, .. } => Some(*request),
            _ => None,
        })
        .unwrap();
    h.command(EngineCommand::AnswerNavigation {
        request,
        allow: true,
    });
    let events = h.until("a download", |ev| {
        has(ev, |e| matches!(e, EngineEvent::ServiceRequested(_)))
    });
    assert!(
        has(
            &events,
            |e| matches!(e, EngineEvent::ServiceRequested(r) if matches!(&r.service, Service::Download { suggested_name: Some(n), .. } if n == "data.bin"))
        ),
        "{events:#?}"
    );

    // --- typing: click a field, type accents, submit; the server sees UTF-8.
    let _ = h.load(&server.url("/form"));
    let before = h.frame();
    h.click(50.0, 20.0);
    let events = h.pump();
    assert!(
        has(&events, |e| matches!(
            e,
            EngineEvent::TextInputChanged { state: Some(_), .. }
        )),
        "a focused field is reported: {events:#?}"
    );
    h.key(
        Key::Character("h".into()),
        Some("héllo"),
        Modifiers::default(),
    );
    let typed = h.frame();
    assert!(
        non_white(&typed) > non_white(&before),
        "the typed text drew"
    );
    // Typing repainted only the field's rectangle: the picture is what a whole repaint makes
    // (hiding drops the frontend's picture, so showing paints all of it).
    h.command(EngineCommand::SetVisibility(false));
    let _ = h.pump();
    h.command(EngineCommand::SetVisibility(true));
    let whole = h.frame();
    assert!(
        typed.2 == whole.2,
        "a partial repaint differs from a whole one"
    );
    // Copy writes the clipboard through the service.
    let ctrl = Modifiers {
        control: true,
        ..Modifiers::default()
    };
    h.key(Key::Character("a".into()), None, ctrl);
    h.key(Key::Character("c".into()), None, ctrl);
    let events = h.pump();
    assert!(
        has(
            &events,
            |e| matches!(e, EngineEvent::ServiceRequested(r) if matches!(&r.service, Service::ClipboardWrite { items, .. } if items.iter().any(|i| i.data == "héllo".as_bytes())))
        ),
        "{events:#?}"
    );
    // End of line drops the selection, so the paste appends.
    h.key(Key::Named("End".into()), None, Modifiers::default());
    // Paste waits for the clipboard.
    h.key(Key::Character("v".into()), None, ctrl);
    let events = h.pump();
    let request = events
        .iter()
        .find_map(|e| match e {
            EngineEvent::ServiceRequested(r)
                if matches!(r.service, Service::ClipboardRead { .. }) =>
            {
                Some(r.id)
            }
            _ => None,
        })
        .expect("ctrl+v asks for the clipboard");
    h.command(EngineCommand::ResolveServiceRequest {
        request,
        result: Ok(ServiceReply::Clipboard(ClipboardData {
            mime_type: "text/plain".into(),
            data: b"+pasted".to_vec(),
        })),
    });
    h.key(Key::Named("Enter".into()), Some("\r"), Modifiers::default());
    // A GET form the user submits is asked about like a link.
    let events = h.pump();
    let request = events
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationRequested { request, url, .. } if url.contains("/echo") => {
                Some(*request)
            }
            _ => None,
        })
        .expect("a form submit asks first");
    assert!(
        !server.saw("/echo"),
        "the form went to the server before the answer"
    );
    h.command(EngineCommand::AnswerNavigation {
        request,
        allow: true,
    });
    let events = h.until("the form submit", |ev| {
        has(
            ev,
            |e| matches!(e, EngineEvent::NavigationCommitted { url, .. } if url.contains("/echo")),
        )
    });
    assert!(has(&events, |e| matches!(
        e,
        EngineEvent::NavigationStarted { .. }
    )));
    assert!(
        server.saw("q=h%C3%A9llo%2Bpasted") || server.saw("q=h%C3%A9llo+pasted"),
        "the server saw {:?}",
        server.requests.lock().unwrap()
    );

    // --- scroll: the wheel moves a long page.
    let _ = h.load(&server.url("/long"));
    let top = h.frame();
    assert!(
        non_white(&top) > 1000,
        "the long page drew {} non-white pixels",
        non_white(&top)
    );
    h.pointer(PointerEvent::Scroll {
        delta: ScrollDelta::Wheel { x: 0, y: 360 },
        position: SurfacePoint { x: 100.0, y: 100.0 },
        modifiers: Modifiers::default(),
    });
    let ev = h.pump();
    eprintln!("after scroll: {ev:?}");
    let scrolled = h.frame();
    assert_ne!(top.2, scrolled.2, "scrolling changed the frame");

    // --- text fields: a password is sensitive for the input method, a textarea is multiline,
    // a text field is neither.
    let _ = h.load(&server.url("/fields"));
    for (y, purpose, multiline) in [
        (20.0, TextPurpose::Password, false),
        (130.0, TextPurpose::Normal, true),
        (220.0, TextPurpose::Normal, false),
    ] {
        h.click(50.0, y);
        let events = h.pump();
        let state = events
            .iter()
            .rev()
            .find_map(|e| match e {
                EngineEvent::TextInputChanged {
                    state: Some(state), ..
                } => Some(state.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no field reported at y {y}: {events:#?}"));
        assert_eq!(
            (state.purpose, state.multiline),
            (purpose, multiline),
            "the field at y {y}"
        );
    }

    // --- a link longer than the first buffer is asked about whole, never shortened.
    let _ = h.load(&server.url("/longlink"));
    h.click(50.0, 30.0);
    let url = h
        .pump()
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationRequested { url, .. } => Some(url.clone()),
            _ => None,
        })
        .expect("a click on the long link asks first");
    assert_eq!(url, server.url(&long_link_path()));

    // --- an animated image wakes the owner while its view is shown, and not while it is hidden.
    let _ = h.load(&server.url("/anim"));
    let _ = h.frame();
    let pumps_before = h.pumps;
    let _ = h.idle_for(Duration::from_secs(1));
    let shown = h.pumps - pumps_before;
    assert!(
        shown >= 5,
        "the animation woke the owner {shown} times in a second"
    );
    h.command(EngineCommand::SetVisibility(false));
    let _ = h.idle_for(Duration::from_millis(300));
    let pumps_before = h.pumps;
    let _ = h.idle_for(Duration::from_secs(2));
    let hidden = h.pumps - pumps_before;
    assert!(
        hidden < 4,
        "a hidden animation woke the owner {hidden} times in two seconds"
    );
    h.command(EngineCommand::SetVisibility(true));
    let _ = h.frame();
    let pumps_before = h.pumps;
    let _ = h.idle_for(Duration::from_secs(1));
    let again = h.pumps - pumps_before;
    assert!(
        again >= 5,
        "the animation resumed with {again} wakes in a second"
    );

    // --- find in page: NetSurf says whether there is a match, synchronously; moves wrap; case
    // is as asked; moves and stops of a search the view no longer has, and a search of a gone
    // document, do nothing; a new document ends the search.
    let events = h.load(&server.url("/find"));
    let document = events
        .iter()
        .rev()
        .find_map(|e| match e {
            EngineEvent::NavigationCommitted { document, .. } => Some(*document),
            _ => None,
        })
        .expect("committed");
    let find = |h: &mut Harness, command: FindCommand| -> Vec<(RequestId, bool)> {
        h.command(EngineCommand::Find(command));
        let results = h.pump().into_iter().filter_map(|e| match e {
            EngineEvent::FindResult {
                view: v,
                document: d,
                search,
                found,
                current: None,
                total: None,
            } if v == view && d == document => Some((search, found)),
            EngineEvent::FindResult { .. } => panic!("{e:?}"),
            _ => None,
        });
        results.collect()
    };
    let search = |text: &str, case_sensitive: bool| {
        let search = RequestId::next();
        let command = FindCommand::Search {
            search,
            document,
            text: text.into(),
            case_sensitive,
        };
        (search, command)
    };
    let (apple, command) = search("apple", false);
    assert_eq!(find(&mut h, command), [(apple, true)]);
    // Forwards through all four and around; backwards from the first to the last.
    for _ in 0..4 {
        assert_eq!(
            find(&mut h, FindCommand::Next { search: apple }),
            [(apple, true)]
        );
    }
    for _ in 0..2 {
        let previous = FindCommand::Previous { search: apple };
        assert_eq!(find(&mut h, previous), [(apple, true)]);
    }
    let (sensitive, command) = search("APPLE", true);
    assert_eq!(find(&mut h, command), [(sensitive, false)], "case as asked");
    let (insensitive, command) = search("APPLE", false);
    assert_eq!(find(&mut h, command), [(insensitive, true)]);
    let (pear, command) = search("pear", false);
    assert_eq!(find(&mut h, command), [(pear, false)]);
    assert_eq!(
        find(&mut h, FindCommand::Next { search: pear }),
        [(pear, false)]
    );
    let gone = FindCommand::Search {
        search: RequestId::next(),
        document: DocumentGeneration::next(),
        text: "apple".into(),
        case_sensitive: false,
    };
    for command in [
        FindCommand::Next { search: apple },
        FindCommand::Stop { search: apple },
        gone,
    ] {
        assert_eq!(find(&mut h, command), [], "nothing answered");
    }
    let (again, command) = search("apple", false);
    assert_eq!(find(&mut h, command), [(again, true)]);
    assert_eq!(find(&mut h, FindCommand::Stop { search: again }), []);
    assert_eq!(
        find(&mut h, FindCommand::Next { search: again }),
        [],
        "stopped"
    );
    let (last, command) = search("apple", false);
    assert_eq!(find(&mut h, command), [(last, true)]);
    h.load(&server.url("/second"));
    assert_eq!(
        find(&mut h, FindCommand::Next { search: last }),
        [],
        "the search ended with its document"
    );

    // --- close and shut down.
    h.runtime.close_view(view);
    let events = h.pump();
    assert!(has(
        &events,
        |e| matches!(e, EngineEvent::ViewClosed { view: v } if *v == view)
    ));
    h.runtime.shutdown();
    assert_eq!(h.runtime.pump(&mut Vec::new()), PumpStatus::Idle);
    let _ = h.wakes.load(Ordering::SeqCst);
}
