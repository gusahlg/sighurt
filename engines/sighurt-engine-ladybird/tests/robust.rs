//! What a hostile or sloppy page and a busy host must not be able to break: a flood of title changes,
//! a window.open, a huge download, a page closing its own window, keys held when the focus goes, a
//! drag, a helper reaped by someone else, a resize that races a frame, and quitting in the middle of
//! a download. One process, one Ladybird (see tests/headless.rs).
//!
//!   tools/heavy cargo test -p sighurt-engine-ladybird --test robust -- --nocapture

use std::cell::RefCell;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PumpStatus, ViewOptions};
use sighurt_api::events::{EngineEvent, OpenDisposition};
use sighurt_api::geometry::{Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers, PointerButton,
    PointerEvent,
};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_api::services::Service;
use sighurt_engine_ladybird::{ffi, LadybirdConfig, LadybirdRuntime};

struct Mem(Rc<RefCell<Metrics>>);

impl CpuTarget for Mem {
    type Permit = LayoutEpoch;
    fn metrics(&self) -> Metrics {
        *self.0.borrow()
    }
    fn permit_epoch(permit: &LayoutEpoch) -> LayoutEpoch {
        *permit
    }
    fn submit(
        &mut self,
        _permit: LayoutEpoch,
        _paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        Ok(())
    }
}

struct Driver {
    runtime: LadybirdRuntime<Mem>,
    wakes: mpsc::Receiver<()>,
    waker: EngineWaker,
    seen: Vec<EngineEvent>,
    cursor: usize,
    deadline: Option<Instant>,
    view: ViewId,
    seat: SeatId,
}

impl Driver {
    fn pump(&mut self) {
        self.waker.take();
        let mut events = Vec::new();
        self.deadline = match self.runtime.pump(&mut events) {
            PumpStatus::Pending { deadline } => Some(deadline.unwrap_or_else(Instant::now)),
            PumpStatus::Idle => None,
        };
        self.seen.extend(events);
    }

    /// Waits for an event after the last one taken that `pick` accepts; returns what it picked.
    fn next<T>(
        &mut self,
        timeout: Duration,
        what: &str,
        mut pick: impl FnMut(&EngineEvent) -> Option<T>,
    ) -> T {
        let end = Instant::now() + timeout;
        loop {
            while self.cursor < self.seen.len() {
                let event = &self.seen[self.cursor];
                self.cursor += 1;
                if let Some(found) = pick(event) {
                    return found;
                }
            }
            let now = Instant::now();
            assert!(
                now < end,
                "timed out waiting for {what}; saw {:#?}",
                &self.seen[self.cursor.saturating_sub(10)..]
            );
            let wait = self.deadline.map_or(end - now, |d| {
                d.saturating_duration_since(now).min(end - now)
            });
            let _ = self.wakes.recv_timeout(wait);
            self.pump();
        }
    }

    fn input(&mut self, kind: InputKind) {
        let event = InputEvent {
            seat: self.seat,
            kind,
        };
        self.runtime
            .command(self.view, EngineCommand::Input(event))
            .unwrap();
    }

    fn click(&mut self, x: f64, y: f64) {
        let position = SurfacePoint { x, y };
        let modifiers = Modifiers::default();
        self.input(InputKind::Pointer(PointerEvent::Motion {
            position,
            modifiers,
        }));
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.input(InputKind::Pointer(PointerEvent::Button {
                button: PointerButton::Left,
                state,
                position,
                modifiers,
            }));
        }
    }
}

impl Driver {
    fn title(&mut self, wanted: &str) {
        let wanted = wanted.to_owned();
        self.next(
            Duration::from_secs(15),
            &format!("title {wanted}"),
            |e| match e {
                EngineEvent::TitleChanged { title: Some(t), .. } if *t == wanted => Some(()),
                _ => None,
            },
        );
    }
}

impl Driver {
    /// Pumps as the owner loop would for `duration`; returns the pumps it took.
    fn idle(&mut self, duration: Duration) -> u64 {
        let end = Instant::now() + duration;
        let mut pumps = 0;
        loop {
            let now = Instant::now();
            if now >= end {
                return pumps;
            }
            let wait = self.deadline.map_or(end - now, |d| {
                d.saturating_duration_since(now).min(end - now)
            });
            if self.wakes.recv_timeout(wait).is_ok()
                || self.deadline.is_some_and(|d| d <= Instant::now())
            {
                self.pump();
                pumps += 1;
            }
        }
    }
}

/// An HTTP server that serves `bytes` bytes of a download, `chunk` bytes every `pause`.
/// What the test sees of a served download: bytes written so far, and whether a write failed (the
/// client closed the connection).
#[derive(Clone)]
struct Served {
    sent: Arc<AtomicU64>,
    broken: Arc<AtomicBool>,
}

fn serve_download(bytes: u64, chunk: usize, pause: Duration) -> (String, Served) {
    let served = Served {
        sent: Arc::new(AtomicU64::new(0)),
        broken: Arc::new(AtomicBool::new(false)),
    };
    let handle = served.clone();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let served = served.clone();
            std::thread::spawn(move || {
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"big.bin\"\r\nContent-Length: {bytes}\r\nConnection: close\r\n\r\n"
                );
                if stream.write_all(header.as_bytes()).is_err() {
                    served.broken.store(true, Ordering::SeqCst);
                    return;
                }
                let block = vec![7u8; chunk];
                let mut sent = 0u64;
                while sent < bytes {
                    let n = chunk.min((bytes - sent) as usize);
                    if stream.write_all(&block[..n]).is_err() {
                        served.broken.store(true, Ordering::SeqCst);
                        return;
                    }
                    sent += n as u64;
                    served.sent.store(sent, Ordering::SeqCst);
                    std::thread::sleep(pause);
                }
            });
        }
    });
    (format!("http://127.0.0.1:{port}/big.bin"), handle)
}

fn staged_files(profile: &Path) -> usize {
    std::fs::read_dir(profile.join("downloads-staging"))
        .map(|dir| dir.count())
        .unwrap_or(0)
}

#[test]
fn hostile_pages_and_a_busy_host() {
    if ffi::load(None).is_err() {
        eprintln!("skipped: no Ladybird shim (nix/build-ladybird-shim.sh)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sighurt-ladybird-robust-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let profile = dir.join("profile");
    // About 50 MiB/s: the limit (64 MiB) is passed after a second or so, the whole file would take four.
    // (At loopback speed the 200 MiB arrive in a quarter of a second, before any 250 ms poll could stop them.)
    const BIG: u64 = 200 * 1024 * 1024;
    let (big, big_served) = serve_download(BIG, 1024 * 1024, Duration::from_millis(20));
    let (slow, _) = serve_download(50 * 1024 * 1024, 64 * 1024, Duration::from_millis(5));
    let two = dir.join("two.html");
    std::fs::write(&two, "<!doctype html><title>two</title>two").unwrap();
    let two_url = format!("file://{}", two.display());
    let page = dir.join("page.html");
    std::fs::write(
        &page,
        format!(
            "<!doctype html><title>start</title><body style='margin:0'>\
             <a id=blank target=_blank href='{two_url}' style='position:absolute;left:0;top:0;width:200px;height:30px;display:block'>popup</a>\
             <a id=big href='{big}' download style='position:absolute;left:0;top:40px;width:200px;height:30px;display:block'>big</a>\
             <a id=slow href='{slow}' download style='position:absolute;left:0;top:80px;width:200px;height:30px;display:block'>slow</a>\
             <a id=flood href='#' style='position:absolute;left:0;top:120px;width:200px;height:30px;display:block' \
                onclick='for (let i = 0; i < 200000; i++) document.title = \"t\" + i; document.title = \"flooded\"; return false;'>flood</a>"
        ),
    )
    .unwrap();

    let (wake_tx, wakes) = mpsc::channel();
    let waker = EngineWaker::new(move || {
        let _ = wake_tx.send(());
    });
    let config = LadybirdConfig {
        profile: Some(profile.clone()),
        ..LadybirdConfig::default()
    };
    let runtime = LadybirdRuntime::<Mem>::new(config, waker.clone())
        .unwrap_or_else(|e| panic!("LadybirdRuntime::new: {e}"));
    let view = ViewId::next();
    let metrics = Rc::new(RefCell::new(Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width: 640,
            height: 480,
        },
        Scale::ONE,
    )));
    let mut d = Driver {
        runtime,
        wakes,
        waker,
        seen: Vec::new(),
        cursor: 0,
        deadline: None,
        view,
        seat: SeatId::next(),
    };
    d.runtime
        .open_view(view, ViewOptions::default(), Mem(Rc::clone(&metrics)))
        .unwrap();
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: format!("file://{}", page.display()),
            },
        )
        .unwrap();
    d.title("start");

    // A flood of title changes arrives as a handful of events, the last one the page's last.
    let marker = d.seen.len();
    let flood_start = Instant::now();
    d.click(20.0, 135.0);
    d.title("flooded");
    let elapsed = flood_start.elapsed();
    let titles = d.seen[marker..]
        .iter()
        .filter(|e| matches!(e, EngineEvent::TitleChanged { .. }))
        .count();
    // About one every 50 ms at the most, not one per change (the loop is 200000 changes).
    assert!(
        titles as u128 <= elapsed.as_millis() / 40 + 5,
        "{titles} title events in {elapsed:?} for a loop of 200000 changes"
    );

    // A resize asks for a frame at the new size: a FrameReady follows it, whatever came before.
    let marker = d.seen.len();
    *metrics.borrow_mut() = Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width: 700,
            height: 500,
        },
        Scale::ONE,
    );
    d.runtime
        .command(view, EngineCommand::SetMetrics(*metrics.borrow()))
        .unwrap();
    d.next(
        Duration::from_secs(15),
        "FrameReady after a resize",
        |e| match e {
            EngineEvent::FrameReady { .. } => Some(()),
            _ => None,
        },
    );
    assert!(d.seen.len() > marker);

    // A window the page opens: its address goes to the browser as OpenRequested for the opener (the
    // popup never loads it), and the opener's own document is untouched.
    let commits = d
        .seen
        .iter()
        .filter(|e| matches!(e, EngineEvent::NavigationCommitted { .. }))
        .count();
    d.click(20.0, 15.0);
    let (opener, url, disposition) =
        d.next(Duration::from_secs(20), "OpenRequested", |e| match e {
            EngineEvent::OpenRequested {
                view,
                url,
                disposition,
                ..
            } => Some((*view, url.clone(), *disposition)),
            _ => None,
        });
    assert_eq!(
        (opener, url.as_str(), disposition),
        (view, two_url.as_str(), OpenDisposition::NewView)
    );
    assert_eq!(
        d.seen
            .iter()
            .filter(|e| matches!(e, EngineEvent::NavigationCommitted { .. }))
            .count(),
        commits
    );

    // A download over the limit is stopped while it runs: nothing is handed over, and the staging
    // directory empties.
    let before_big = d.seen.len();
    d.click(20.0, 55.0);
    // The transfer starts, and is then stopped before the file is over: the server sees the
    // connection closed (or the client stop reading) well short of 200 MiB. (Sampling the staging
    // directory for this is a race: the file lives only from the first bytes to the next 250 ms
    // poll after the limit.)
    let limit_end = Instant::now() + Duration::from_secs(40);
    let mut last = (0u64, Instant::now());
    loop {
        d.idle(Duration::from_millis(200));
        let sent = big_served.sent.load(Ordering::SeqCst);
        if sent != last.0 {
            last = (sent, Instant::now());
        }
        let stopped = big_served.broken.load(Ordering::SeqCst)
            || (sent > 0 && last.1.elapsed() > Duration::from_secs(2));
        if stopped {
            break;
        }
        assert!(
            Instant::now() < limit_end,
            "the oversized download was not stopped (the server sent {sent} of {BIG} bytes)"
        );
    }
    let sent = big_served.sent.load(Ordering::SeqCst);
    assert!(sent > 0, "the download never started");
    assert!(
        sent < BIG * 3 / 4,
        "the transfer was not stopped in time: {sent} of {BIG} bytes fetched"
    );
    // And what was staged is gone.
    let gone = Instant::now() + Duration::from_secs(5);
    while staged_files(&profile) > 0 && Instant::now() < gone {
        d.idle(Duration::from_millis(200));
    }
    assert_eq!(
        staged_files(&profile),
        0,
        "a partial download was left staged"
    );
    assert!(
        !d.seen.iter().any(|e| matches!(e, EngineEvent::ServiceRequested(r) if matches!(r.service, Service::Download { .. }))),
        "an oversized download was handed over"
    );
    // Cancelling it must not take the page's process down (upstream cancels the stream outside an
    // execution context and aborts WebContent; patch 0002 of nix/ladybird/ fixes that).
    assert!(
        !d.seen[before_big..]
            .iter()
            .any(|e| matches!(e, EngineEvent::ViewFailed { .. })),
        "cancelling a download crashed the page: {:#?}",
        &d.seen[before_big..]
    );

    // Ladybird's own pages (about:settings can open a file manager) are never shown, whoever asks.
    let marker = d.seen.len();
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: "about:settings".into(),
            },
        )
        .unwrap();
    d.idle(Duration::from_secs(4));
    assert!(
        !d.seen[marker..].iter().any(|e| matches!(e, EngineEvent::NavigationCommitted { url, .. } if url.starts_with("about:settings"))),
        "about:settings was shown: {:#?}",
        &d.seen[marker..]
    );

    // A second view, loaded once (so its page may close it). Keys its page holds are released when
    // it loses the focus, a drag shows the held button, and window.close() ends the view: the
    // browser hears the request, then that it failed and closed, and nothing of it after that.
    let closer = dir.join("closer.html");
    std::fs::write(
        &closer,
        "<!doctype html><title>closer</title>\
         <body style='margin:0;height:300px' onkeyup='document.title=\"up:\"+event.code'>\
         <div style='position:absolute;left:0;top:0;width:200px;height:30px' onclick='window.close()'>close</div>\
         <div style='position:absolute;left:0;top:100px;width:300px;height:100px' \
              onmousemove='if (event.buttons) document.title=\"buttons:\"+event.buttons'>drag</div>",
    )
    .unwrap();
    let main_view = d.view;
    let second = ViewId::next();
    d.runtime
        .open_view(second, ViewOptions::default(), Mem(Rc::clone(&metrics)))
        .unwrap();
    d.view = second;
    d.runtime
        .command(
            second,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: format!("file://{}", closer.display()),
            },
        )
        .unwrap();
    d.title("closer");
    d.runtime
        .command(second, EngineCommand::SetFocus(true))
        .unwrap();
    d.click(250.0, 250.0);
    d.input(InputKind::Key(KeyEvent {
        code: KeyCode(30),
        key: Key::Character("a".into()),
        state: ButtonState::Pressed,
        repeat: false,
        modifiers: Modifiers::default(),
        consumed: Modifiers::default(),
        text: Some("a".into()),
    }));
    d.runtime
        .command(second, EngineCommand::SetFocus(false))
        .unwrap();
    d.title("up:KeyA");
    let modifiers = Modifiers::default();
    let at = |x| SurfacePoint { x, y: 150.0 };
    d.input(InputKind::Pointer(PointerEvent::Motion {
        position: at(20.0),
        modifiers,
    }));
    d.input(InputKind::Pointer(PointerEvent::Button {
        button: PointerButton::Left,
        state: ButtonState::Pressed,
        position: at(20.0),
        modifiers,
    }));
    d.input(InputKind::Pointer(PointerEvent::Motion {
        position: at(60.0),
        modifiers,
    }));
    d.title("buttons:1");
    d.input(InputKind::Pointer(PointerEvent::Button {
        button: PointerButton::Left,
        state: ButtonState::Released,
        position: at(60.0),
        modifiers,
    }));
    d.click(20.0, 15.0);
    d.next(Duration::from_secs(15), "CloseRequested", |e| match e {
        EngineEvent::CloseRequested { view, .. } if *view == second => Some(()),
        _ => None,
    });
    d.next(Duration::from_secs(1), "ViewFailed", |e| match e {
        EngineEvent::ViewFailed { view, .. } if *view == second => Some(()),
        _ => None,
    });
    d.next(Duration::from_secs(1), "ViewClosed", |e| match e {
        EngineEvent::ViewClosed { view } if *view == second => Some(()),
        _ => None,
    });
    let after = d.seen.len();
    d.idle(Duration::from_secs(1));
    assert!(
        !d.seen[after..].iter().any(|e| e.view() == Some(second)),
        "events of a closed view: {:#?}",
        &d.seen[after..]
    );
    // The browser's own close of it afterwards does nothing; Ladybird goes on.
    d.runtime.close_view(second);
    assert!(d
        .runtime
        .command(second, EngineCommand::SetFocus(true))
        .is_err());
    d.view = main_view;

    // A helper reaped by someone else (here: this process) is treated as exited and the loop does not
    // spin on its pidfd afterwards.
    let web_content = child_pids()
        .into_iter()
        .find(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains("WebContent"))
        })
        .expect("a WebContent process");
    let marker = d.seen.len();
    unsafe {
        libc::kill(web_content, libc::SIGKILL);
        let mut status = 0;
        libc::waitpid(web_content, &mut status, 0);
    }
    d.next(Duration::from_secs(15), "the crash", |e| match e {
        EngineEvent::ViewFailed { .. } => Some(()),
        _ => None,
    });
    let _ = marker;
    // Ladybird recovers the crashed page in the first moments; after that the loop is quiet. A
    // pidfd nobody drains would keep it readable and make every pump ask for the next at once.
    d.idle(Duration::from_secs(3));
    let pumps = d.idle(Duration::from_secs(3));
    assert!(
        pumps <= 30,
        "{pumps} pumps in 3 s, 3 s after a helper was reaped elsewhere"
    );

    // Quitting in the middle of a download: no abort, and nothing staged is left.
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: format!("file://{}", page.display()),
            },
        )
        .unwrap();
    d.title("start");
    d.click(20.0, 95.0);
    let begin = Instant::now();
    while staged_files(&profile) == 0 && begin.elapsed() < Duration::from_secs(15) {
        d.idle(Duration::from_millis(100));
    }
    assert!(
        staged_files(&profile) > 0,
        "the slow download did not start"
    );
    d.runtime.shutdown();
    assert_eq!(staged_files(&profile), 0, "a staged file was left");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The direct children of this process.
fn child_pids() -> Vec<i32> {
    let mut pids = Vec::new();
    for task in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
        if let Ok(text) = std::fs::read_to_string(task.path().join("children")) {
            pids.extend(
                text.split_whitespace()
                    .filter_map(|p| p.parse::<i32>().ok()),
            );
        }
    }
    pids
}
