//! Chromium presenting into an in-memory [`CpuTarget`], through the real `EngineRuntime`.
//!
//! One test: CEF starts once per process. Ignored so `cargo test` does not start Chromium;
//! `tests/offscreen.sh` runs it with no display connection (ozone headless). It checks the
//! pages' pixels (ARGB top-down), navigation events in order with
//! history, a link the user clicks being held and answered, a popup becoming `OpenRequested`,
//! typing, the text-input state, a paste through the clipboard service (a multi-line one as
//! text, never Enter), a popup nobody clicked for refused, a page closing itself (asked, then
//! failed and closed), resizing, idling without wakeups, and a clean shutdown.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, ViewOptions};
use sighurt_api::events::{EngineEvent, HistoryEffect, OpenDisposition};
use sighurt_api::geometry::{BufferSize, Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Key, KeyCode, KeyEvent, Modifiers, PointerButton,
    PointerEvent,
};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_api::services::{ClipboardData, Service, ServiceReply};
use sighurt_engine_cef::{CefConfig, CefRuntime};

/// Header `#e67e22` as B, G, R, A.
const ORANGE: [u8; 4] = [0x22, 0x7e, 0xe6, 0xff];
/// Body `#1b4f72`.
const BLUE: [u8; 4] = [0x72, 0x4f, 0x1b, 0xff];
/// The box `#00ff00`.
const GREEN: [u8; 4] = [0x00, 0xff, 0x00, 0xff];
/// The corner mark `#ff00ff`.
const MAGENTA: [u8; 4] = [0xff, 0x00, 0xff, 0xff];

const WAIT: Duration = Duration::from_secs(40);

struct Stored {
    size: BufferSize,
    stride: usize,
    pixels: Vec<u8>,
}

struct Mem {
    metrics: Metrics,
    frames: Arc<Mutex<Vec<Stored>>>,
    /// A buffer that keeps its content between frames, like a swapchain's: the engine copies
    /// only what it lacks into it. Every frame is also painted into a fresh buffer, and the two
    /// must be identical.
    persistent: Vec<u8>,
}

struct Permit {
    epoch: LayoutEpoch,
    size: BufferSize,
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
        // Padding past each row: a copy that ignores the stride writes into it.
        let stride = 4 * permit.size.width as usize + 16;
        let len = stride * permit.size.height as usize;
        if self.persistent.len() != len {
            self.persistent = vec![0u8; len];
        }
        paint(Frame {
            pixels: &mut self.persistent,
            size: permit.size,
            stride,
        })?;
        let mut pixels = vec![0u8; len];
        paint(Frame {
            pixels: &mut pixels,
            size: permit.size,
            stride,
        })?;
        let row = 4 * permit.size.width as usize;
        assert!(
            (0..permit.size.height as usize).all(|y| pixels[y * stride + row..(y + 1) * stride]
                .iter()
                .all(|b| *b == 0)),
            "the copy wrote past the row"
        );
        for y in 0..permit.size.height as usize {
            assert!(
                self.persistent[y * stride..y * stride + row]
                    == pixels[y * stride..y * stride + row],
                "the partial copy into the kept buffer differs from a whole copy at row {y}"
            );
        }
        self.frames.lock().expect("frames").push(Stored {
            size: permit.size,
            stride,
            pixels,
        });
        Ok(())
    }
}

fn metrics(width: u32, height: u32) -> Metrics {
    Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width,
            height,
        },
        Scale::ONE,
    )
}

fn pixel(frame: &Stored, x: u32, y: u32) -> [u8; 4] {
    let i = y as usize * frame.stride + x as usize * 4;
    frame.pixels[i..i + 4].try_into().expect("pixel")
}

struct Harness {
    rt: CefRuntime<Mem>,
    log: Vec<EngineEvent>,
    wakes: Arc<AtomicUsize>,
    frames: Arc<Mutex<Vec<Stored>>>,
    view: ViewId,
    metrics: Metrics,
}

impl Harness {
    fn pump(&mut self) -> usize {
        let mut events = Vec::new();
        self.rt.pump(&mut events);
        let n = events.len();
        self.log.extend(events);
        n
    }

    /// Pumps until `pred` finds something new in the log (beyond `from`), or the wait ends.
    fn wait<R>(
        &mut self,
        from: usize,
        mut pred: impl FnMut(&EngineEvent) -> Option<R>,
    ) -> Option<R> {
        let end = Instant::now() + WAIT;
        let mut seen = from;
        while Instant::now() < end {
            self.pump();
            while seen < self.log.len() {
                if let Some(found) = pred(&self.log[seen]) {
                    return Some(found);
                }
                seen += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        None
    }

    fn send(&mut self, command: EngineCommand) {
        self.rt
            .command(self.view, command)
            .expect("command accepted");
    }

    fn navigate(&mut self, url: &str) -> NavigationId {
        let navigation = NavigationId::next();
        let from = self.log.len();
        self.send(EngineCommand::Navigate {
            navigation,
            url: url.into(),
        });
        let committed = self.wait(from, |e| match e {
            EngineEvent::NavigationCommitted {
                navigation: n,
                url: u,
                ..
            } if *n == navigation => Some(u.clone()),
            _ => None,
        });
        assert!(committed.is_some(), "{url} committed with the browser's id");
        navigation
    }

    fn title(&mut self, from: usize, wanted: &str) -> bool {
        self.wait(from, |e| match e {
            EngineEvent::TitleChanged { title: Some(t), .. } if t == wanted => Some(()),
            _ => None,
        })
        .is_some()
    }

    fn present(&mut self) -> Stored {
        let end = Instant::now() + WAIT;
        loop {
            self.pump();
            let permit = Permit {
                epoch: self.metrics.epoch(),
                size: self.metrics.buffer(),
            };
            match self.rt.present(self.view, permit).expect("present") {
                PresentOutcome::Submitted => {
                    return self.frames.lock().expect("frames").pop().expect("a frame");
                }
                _ => {
                    assert!(Instant::now() < end, "no frame to present");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// Presents until `check` accepts the frame (Chromium paints in steps).
    fn frame_where(&mut self, check: impl Fn(&Stored) -> bool) -> Stored {
        let end = Instant::now() + WAIT;
        loop {
            self.metrics = Metrics::new(
                LayoutEpoch::next(),
                self.metrics.rect(),
                self.metrics.scale(),
            );
            let frame = self.present();
            if check(&frame) {
                return frame;
            }
            assert!(Instant::now() < end, "the page never looked right");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn input(&mut self, kind: InputKind) {
        self.send(EngineCommand::Input(InputEvent {
            seat: sighurt_api::ids::SeatId::next(),
            kind,
        }));
    }

    fn click(&mut self, x: f64, y: f64) {
        let position = SurfacePoint { x, y };
        self.input(InputKind::Pointer(PointerEvent::Motion {
            position,
            modifiers: Modifiers::default(),
        }));
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.input(InputKind::Pointer(PointerEvent::Button {
                button: PointerButton::Left,
                state,
                position,
                modifiers: Modifiers::default(),
            }));
        }
    }

    fn type_key(&mut self, code: u32, text: &str, modifiers: Modifiers) {
        for state in [ButtonState::Pressed, ButtonState::Released] {
            self.input(InputKind::Key(KeyEvent {
                code: KeyCode(code),
                key: Key::Character(text.into()),
                state,
                repeat: false,
                modifiers,
                consumed: Modifiers::default(),
                text: (state == ButtonState::Pressed && !modifiers.control).then(|| text.into()),
            }));
        }
    }

    /// Index just after `navigation`'s commit. `navigate` returns as soon as that event is in
    /// the log, and the same pump can already hold the new document's focus report.
    fn after_commit(&self, navigation: NavigationId) -> usize {
        self.log
            .iter()
            .rposition(|e| {
                matches!(
                    e,
                    EngineEvent::NavigationCommitted { navigation: n, .. } if *n == navigation
                )
            })
            .map(|i| i + 1)
            .unwrap_or(self.log.len())
    }

    /// The input at (100, 35) of `input.html` has focus in the document `navigation` committed.
    ///
    /// A reload can restore the field before this runs, and a click on a field that already
    /// has focus does not report it again. A click during the swap can also hit the document
    /// that is leaving, so the click waits until the load has stopped.
    fn focus_field(&mut self, navigation: NavigationId, why: &str) {
        let from = self.after_commit(navigation);
        let focused = |e: &EngineEvent| {
            matches!(e, EngineEvent::TextInputChanged { state: Some(_), .. }).then_some(())
        };
        if self.log[from..].iter().any(|e| focused(e).is_some()) {
            return;
        }
        if self
            .wait(from, |e| match e {
                EngineEvent::LoadingChanged { loading: false, .. } => Some(()),
                _ => None,
            })
            .is_none()
        {
            panic!("{why}: the load never finished ({})", self.trace(from));
        }
        // Leave whatever the reload restored, then enter the field, so this document reports
        // it. The gap lets the blur land on its own: the two reports would otherwise collapse
        // into whichever arrived last.
        let mark = self.log.len();
        self.click(500.0, 500.0);
        let pause = Instant::now() + Duration::from_millis(300);
        while Instant::now() < pause {
            self.pump();
            if self.log[mark..]
                .iter()
                .any(|e| matches!(e, EngineEvent::TextInputChanged { .. }))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mark = self.log.len();
        self.click(100.0, 35.0);
        if self.wait(mark, focused).is_none() {
            panic!("{why} ({})", self.trace(from));
        }
    }

    fn trace(&self, from: usize) -> String {
        self.log[from..]
            .iter()
            .filter_map(|e| match e {
                EngineEvent::TextInputChanged { state, .. } => {
                    Some(format!("text-input:{}", state.is_some()))
                }
                EngineEvent::LoadingChanged { loading, .. } => Some(format!("loading:{loading}")),
                EngineEvent::NavigationCommitted { .. } => Some("commit".into()),
                EngineEvent::FrameReady { .. } => Some("frame".into()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Writes a frame as a PPM (BGRA to RGB) for looking at after a failure.
fn save(frame: &Stored, name: &str) {
    use std::io::Write;
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let mut out = std::fs::File::create(dir.join(name)).expect("ppm");
    write!(out, "P6\n{} {}\n255\n", frame.size.width, frame.size.height).expect("header");
    for y in 0..frame.size.height {
        for x in 0..frame.size.width {
            let p = pixel(frame, x, y);
            out.write_all(&[p[2], p[1], p[0]]).expect("pixel");
        }
    }
}

fn page(name: &str) -> String {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "tests", "pages", name]
        .iter()
        .collect();
    format!("file://{}", path.display())
}

fn count_children() -> usize {
    // Chromium's helper processes of this test, but the zygotes: they stay until the browser
    // process exits (offscreen.sh checks that they do).
    let out = std::process::Command::new("pgrep")
        .args(["-af", "^/[^ ]*/sig-cef-helper --type="])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| !line.contains("--type=zygote"))
        .count()
}

#[test]
#[ignore = "starts Chromium; run tests/offscreen.sh"]
fn chromium_in_the_engine_contract() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cef-offscreen");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    let mut config = CefConfig::installed().expect("CEF is installed (nix/build-cef.sh)");
    config.scratch = Some(scratch);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&wakes);
    let waker = EngineWaker::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    let started = Instant::now();
    let rt = CefRuntime::<Mem>::new(config, waker.clone()).expect("CEF starts");
    eprintln!(
        "cef-offscreen: runtime started in {} ms",
        started.elapsed().as_millis()
    );
    let frames = Arc::new(Mutex::new(Vec::new()));
    let view = ViewId::next();
    let size = metrics(800, 600);
    let mut h = Harness {
        rt,
        log: Vec::new(),
        wakes,
        frames: Arc::clone(&frames),
        view,
        metrics: size,
    };
    h.rt.open_view(
        view,
        ViewOptions::default(),
        Mem {
            metrics: size,
            frames,
            persistent: Vec::new(),
        },
    )
    .expect("open_view");
    assert!(
        h.wait(0, |e| matches!(e, EngineEvent::ViewReady { .. })
            .then_some(()))
            .is_some(),
        "ViewReady"
    );

    // A view closed the moment it is opened: the close meets the browser's creation. Chromium's
    // thread must neither deadlock nor lose the view's `ViewClosed`.
    for _ in 0..3 {
        let quick = ViewId::next();
        h.rt.open_view(
            quick,
            ViewOptions::default(),
            Mem {
                metrics: size,
                frames: Arc::new(Mutex::new(Vec::new())),
                persistent: Vec::new(),
            },
        )
        .expect("open_view");
        h.rt.close_view(quick);
        assert!(
            h.wait(0, |e| {
                matches!(e, EngineEvent::ViewClosed { view } if *view == quick).then_some(())
            })
            .is_some(),
            "a view closed during its creation is reported closed"
        );
    }

    // The page has the keyboard, as the host gives it to the selected view.
    h.send(EngineCommand::SetFocus(true));

    // 1. A page, its events and its pixels.
    let from = h.log.len();
    let first = h.navigate(&page("colours.html"));
    let order: Vec<&str> = h.log[from..]
        .iter()
        .filter_map(|e| match e {
            EngineEvent::NavigationStarted { navigation, .. } if *navigation == first => {
                Some("started")
            }
            EngineEvent::NavigationCommitted {
                navigation,
                history: Some(HistoryEffect::Push),
                ..
            } if *navigation == first => Some("committed"),
            _ => None,
        })
        .collect();
    assert_eq!(
        order,
        ["started", "committed"],
        "started, then committed, with a Push"
    );
    assert!(h.title(0, "Colours"), "the title arrives");
    let frame = h.frame_where(|f| pixel(f, 4, 8) == ORANGE);
    for ((x, y), expected, name) in [
        ((4, 0), ORANGE, "top row"),
        ((400, 8), ORANGE, "header middle"),
        ((400, 300), BLUE, "body"),
        ((0, 599), BLUE, "bottom left"),
        ((100, 100), GREEN, "box"),
        ((795, 595), MAGENTA, "corner"),
    ] {
        assert_eq!(pixel(&frame, x, y), expected, "{name} at ({x}, {y})");
    }
    let history = h.log.iter().rev().find_map(|e| match e {
        EngineEvent::HistoryChanged {
            entries, current, ..
        } => Some((entries.len(), *current)),
        _ => None,
    });
    assert_eq!(history, Some((1, 0)));
    // Nothing new to paint: the same permit shows nothing more.
    let again = h.rt.present(
        view,
        Permit {
            epoch: h.metrics.epoch(),
            size: h.metrics.buffer(),
        },
    );
    assert!(matches!(
        again,
        Ok(PresentOutcome::Unchanged) | Ok(PresentOutcome::Submitted)
    ));

    // 2. A link the user clicks is asked about, then followed when allowed.
    let from = h.log.len();
    h.click(220.0, 210.0);
    let (request, held_url) = h
        .wait(from, |e| match e {
            EngineEvent::NavigationRequested { request, url, .. } => Some((*request, url.clone())),
            _ => None,
        })
        .expect("NavigationRequested for a clicked link");
    assert!(held_url.ends_with("second.html"), "{held_url}");
    assert!(
        !h.log[from..]
            .iter()
            .any(|e| matches!(e, EngineEvent::NavigationStarted { .. })),
        "nothing started before the answer"
    );
    h.send(EngineCommand::AnswerNavigation {
        request,
        allow: true,
    });
    let committed = h.wait(from, |e| match e {
        EngineEvent::NavigationCommitted { url, history, .. } if url.ends_with("second.html") => {
            Some(*history)
        }
        _ => None,
    });
    assert_eq!(
        committed,
        Some(Some(HistoryEffect::Push)),
        "the allowed link commits"
    );

    // 3. Traversal.
    let from = h.log.len();
    h.send(EngineCommand::Traverse {
        navigation: NavigationId::next(),
        delta: -1,
    });
    let traverse = h.wait(from, |e| match e {
        EngineEvent::NavigationCommitted { url, history, .. } if url.ends_with("colours.html") => {
            Some(*history)
        }
        _ => None,
    });
    assert_eq!(traverse, Some(Some(HistoryEffect::Traverse)));
    let list = h.log.iter().rev().find_map(|e| match e {
        EngineEvent::HistoryChanged {
            entries, current, ..
        } => Some((entries.len(), *current)),
        _ => None,
    });
    assert_eq!(list, Some((2, 0)));
    let from = h.log.len();
    h.send(EngineCommand::Traverse {
        navigation: NavigationId::next(),
        delta: -1,
    });
    assert!(
        h.wait(from, |e| matches!(
            e,
            EngineEvent::NavigationFailed {
                error: sighurt_api::events::NavigationError::Cancelled,
                ..
            }
        )
        .then_some(()))
            .is_some(),
        "back beyond the list is cancelled"
    );

    // 4. Typing, the text-input state, a popup, a paste.
    h.navigate(&page("input.html"));
    let frame = h.frame_where(|f| pixel(f, 500, 500) == [0xff; 4]);
    save(&frame, "cef-input.ppm");
    let from = h.log.len();
    h.click(100.0, 35.0);
    let state = h.wait(from, |e| match e {
        EngineEvent::TextInputChanged {
            state: Some(state), ..
        } => Some(state.clone()),
        _ => None,
    });
    assert!(state.is_some(), "a focused field reports text input");
    let from = h.log.len();
    h.type_key(35, "h", Modifiers::default());
    h.type_key(23, "i", Modifiers::default());
    assert!(h.title(from, "typed:hi"), "typed text reaches the page");
    // Typing damages a few rows: a buffer that kept the last frame gets those, not the page.
    h.present();
    let before = h.rt.diagnostics().presented_bytes;
    h.type_key(30, "a", Modifiers::default());
    assert!(h.title(from, "typed:hia"), "more typing");
    h.present();
    let copied = h.rt.diagnostics().presented_bytes - before;
    let whole = 800 * 600 * 4;
    // Every frame is painted twice by the test target: whole into a fresh buffer (the check),
    // and into the kept buffer, where only the damage is copied.
    let kept = copied.saturating_sub(whole);
    eprintln!("cef-offscreen: a keystroke copied {kept} bytes into the kept buffer of a {whole}-byte frame");
    assert!(
        kept < whole / 4,
        "{kept} bytes into the kept buffer for one keystroke"
    );
    let back = h.log.len();
    h.type_key(14, "", Modifiers::default()); // Backspace: the field is as it was
    assert!(h.title(back, "typed:hi"), "backspace");
    let from = h.log.len();
    h.type_key(
        47,
        "v",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    let (id, service) = h
        .wait(from, |e| match e {
            EngineEvent::ServiceRequested(r) => Some((r.id, r.service.clone())),
            _ => None,
        })
        .expect("Ctrl+V asks the clipboard service");
    assert!(
        matches!(service, Service::ClipboardRead { .. }),
        "{service:?}"
    );
    h.send(EngineCommand::ResolveServiceRequest {
        request: id,
        result: Ok(ServiceReply::Clipboard(ClipboardData {
            mime_type: "text/plain".into(),
            data: b"PASTED".to_vec(),
        })),
    });
    let ok = h.title(from, "typed:hiPASTED");
    let titles: Vec<_> = h.log[from..]
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TitleChanged { title: Some(t), .. } => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(ok, "the pasted text reaches the field: {titles:?}");
    // An input method's commit reaches the field too.
    let from = h.log.len();
    h.input(InputKind::TextInput(sighurt_api::input::TextInputEvent {
        commit: Some("!".into()),
        ..Default::default()
    }));
    assert!(
        h.title(from, "typed:hiPASTED!"),
        "committed text reaches the field"
    );
    // A multi-line paste never presses Enter in a one-line field: its line break is a space
    // there (as Chromium makes it of a paste), where a typed '\r' would submit a form.
    let from = h.log.len();
    h.type_key(
        47,
        "v",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    let id = h
        .wait(from, |e| match e {
            EngineEvent::ServiceRequested(r)
                if matches!(r.service, Service::ClipboardRead { .. }) =>
            {
                Some(r.id)
            }
            _ => None,
        })
        .expect("the multi-line paste is asked for");
    h.send(EngineCommand::ResolveServiceRequest {
        request: id,
        result: Ok(ServiceReply::Clipboard(ClipboardData {
            mime_type: "text/plain".into(),
            data: b"A\nB".to_vec(),
        })),
    });
    let ok = h.title(from, "typed:hiPASTED!A B");
    let titles: Vec<_> = h.log[from..]
        .iter()
        .filter_map(|e| match e {
            EngineEvent::TitleChanged { title: Some(t), .. } => Some(t.clone()),
            _ => None,
        })
        .collect();
    assert!(ok, "a multi-line paste is inserted as text: {titles:?}");
    assert!(
        !titles.iter().any(|t| t.starts_with("enter:")),
        "a multi-line paste pressed Enter: {titles:?}"
    );
    let from = h.log.len();
    h.click(30.0, 110.0);
    let open = h
        .wait(from, |e| match e {
            EngineEvent::OpenRequested {
                url, disposition, ..
            } => Some((url.clone(), *disposition)),
            _ => None,
        })
        .expect("a popup is an OpenRequested");
    assert!(
        open.0.ends_with("second.html") && open.1 == OpenDisposition::NewView,
        "{open:?}"
    );
    // A popup the page opens on its own, with no click behind it, is refused.
    let from = h.log.len();
    h.navigate(&page("popup.html"));
    assert!(h.title(from, "opened"), "the popup page ran");
    let settle = Instant::now() + Duration::from_millis(500);
    while Instant::now() < settle {
        h.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !h.log[from..]
            .iter()
            .any(|e| matches!(e, EngineEvent::OpenRequested { .. })),
        "a popup without a user gesture became an OpenRequested"
    );

    // A paste belongs to the document that asked: answered after a navigation, it goes nowhere.
    let first = h.navigate(&page("input.html"));
    h.focus_field(first, "the field of the first document is focused");
    let from = h.log.len();
    h.type_key(
        47,
        "v",
        Modifiers {
            control: true,
            ..Modifiers::default()
        },
    );
    let late = h
        .wait(from, |e| match e {
            EngineEvent::ServiceRequested(r)
                if matches!(r.service, Service::ClipboardRead { .. }) =>
            {
                Some(r.id)
            }
            _ => None,
        })
        .expect("the paste is asked for");
    let second = h.navigate(&page("input.html"));
    h.focus_field(second, "the field of the second document is focused");
    h.send(EngineCommand::ResolveServiceRequest {
        request: late,
        result: Ok(ServiceReply::Clipboard(ClipboardData {
            mime_type: "text/plain".into(),
            data: b"LEAKED".to_vec(),
        })),
    });
    std::thread::sleep(Duration::from_millis(800));
    let from = h.log.len();
    h.type_key(44, "z", Modifiers::default());
    assert!(
        h.title(from, "typed:z"),
        "the late paste was not typed into the new document, and typing still works"
    );
    assert!(
        !h.log.iter().any(|e| matches!(e, EngineEvent::TitleChanged { title: Some(t), .. } if t.contains("LEAKED"))),
        "the late paste reached the page"
    );

    // A POST form the user submits is not held and asked about (only an address can be handed
    // to another engine): it goes ahead in Chromium with its body, and an attachment is a
    // download request for the browser's service, not a page.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let port = listener.local_addr().expect("addr").port();
    let (posted_tx, posted) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        while let Ok((mut stream, _)) = listener.accept() {
            let mut request = Vec::new();
            let mut chunk = [0u8; 2048];
            let (mut head_end, mut want) = (None, 0usize);
            while let Ok(n) = stream.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..n]);
                if head_end.is_none() {
                    head_end = request
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|p| p + 4);
                    if let Some(end) = head_end {
                        let head = String::from_utf8_lossy(&request[..end]).to_lowercase();
                        want = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse().ok())
                            .unwrap_or(0);
                    }
                }
                if head_end.is_some_and(|end| request.len() >= end + want) {
                    break;
                }
            }
            let text = String::from_utf8_lossy(&request).into_owned();
            let (content_type, extra, body) = if text.starts_with("GET /form") {
                (
                    "text/html",
                    String::new(),
                    "<!doctype html><title>form</title><form method=post action=/submit>\
                     <input type=hidden name=a value=b>\
                     <button style='position:absolute;left:20px;top:20px;width:120px;height:50px'>go\
                     </button></form>"
                        .to_owned(),
                )
            } else if text.starts_with("POST /submit") {
                let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
                let _ = posted_tx.send(body);
                (
                    "text/html",
                    String::new(),
                    "<!doctype html><title>posted</title>ok".to_owned(),
                )
            } else {
                (
                    "application/octet-stream",
                    "Content-Disposition: attachment; filename=\"a.txt\"\r\n".to_owned(),
                    "downloaded".to_owned(),
                )
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\n{extra}\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    h.navigate(&format!("http://127.0.0.1:{port}/form"));
    h.frame_where(|f| pixel(f, 60, 40) != [0xff; 4]);
    let from = h.log.len();
    h.click(60.0, 40.0);
    let body = posted
        .recv_timeout(Duration::from_secs(20))
        .expect("the POST reached the server");
    assert_eq!(body, "a=b", "the form's body arrived whole");
    assert!(
        h.wait(from, |e| {
            matches!(e, EngineEvent::NavigationCommitted { url, .. } if url.ends_with("/submit"))
                .then_some(())
        })
        .is_some(),
        "the POST's page committed"
    );
    assert!(
        !h.log[from..]
            .iter()
            .any(|e| matches!(e, EngineEvent::NavigationRequested { .. })),
        "a POST form was held and asked about"
    );
    let from = h.log.len();
    h.send(EngineCommand::Navigate {
        navigation: NavigationId::next(),
        url: format!("http://127.0.0.1:{port}/a.txt"),
    });
    let download = h
        .wait(from, |e| match e {
            EngineEvent::ServiceRequested(r) => match &r.service {
                Service::Download {
                    source,
                    suggested_name,
                } => Some((source.clone(), suggested_name.clone())),
                _ => None,
            },
            _ => None,
        })
        .expect("an attachment is a Download service request");
    assert!(
        matches!(&download.0, sighurt_api::services::DownloadSource::Url(u) if u.ends_with("/a.txt")),
        "{download:?}"
    );

    // 5. Resizing.
    let big = metrics(1000, 700);
    h.metrics = big;
    h.send(EngineCommand::SetMetrics(big));
    let frame = h.frame_where(|f| {
        f.size
            == BufferSize {
                width: 1000,
                height: 700,
            }
    });
    save(&frame, "cef-resized.ppm");
    for e in &h.log {
        if let EngineEvent::NavigationCommitted { url, history, .. } = e {
            eprintln!("cef-offscreen: committed {url} {history:?}");
        }
    }
    assert_eq!(
        pixel(&frame, 0, 0),
        [0xff, 0xff, 0xff, 0xff],
        "white page, opaque"
    );

    // A page alone in its history that closes itself: the browser is asked, and the view, whose
    // document is gone, fails and closes on its own.
    let closer = ViewId::next();
    h.rt.open_view(
        closer,
        ViewOptions::default(),
        Mem {
            metrics: size,
            frames: Arc::new(Mutex::new(Vec::new())),
            persistent: Vec::new(),
        },
    )
    .expect("open_view");
    let from = h.log.len();
    h.rt.command(
        closer,
        EngineCommand::Navigate {
            navigation: NavigationId::next(),
            url: page("close.html"),
        },
    )
    .expect("command accepted");
    h.wait(from, |e| {
        matches!(e, EngineEvent::ViewClosed { view } if *view == closer).then_some(())
    })
    .expect("a page that closed itself closes its view");
    let ends: Vec<&str> = h.log[from..]
        .iter()
        .filter_map(|e| match e {
            EngineEvent::CloseRequested { view, .. } if *view == closer => Some("asked"),
            EngineEvent::ViewFailed { view, .. } if *view == closer => Some("failed"),
            EngineEvent::ViewClosed { view } if *view == closer => Some("closed"),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends,
        ["asked", "failed", "closed"],
        "window.close() in order"
    );
    // The browser closing it afterwards, as when its rule allows the request, is harmless.
    h.rt.close_view(closer);

    // 6. Idle: nothing wakes the owner, and nothing is queued.
    h.frame_where(|_| true);
    std::thread::sleep(Duration::from_millis(500));
    h.pump();
    let before = h.wakes.load(Ordering::SeqCst);
    waker.take();
    std::thread::sleep(Duration::from_secs(3));
    let idle = h.wakes.load(Ordering::SeqCst) - before;
    eprintln!("cef-offscreen: owner wakeups in 3 s of idle: {idle}");
    assert!(
        idle <= 2,
        "an idle page wakes the owner {idle} times in 3 s"
    );

    // 7. Close and shut down; nothing of Chromium is left.
    h.rt.close_view(view);
    assert!(
        h.wait(0, |e| matches!(e, EngineEvent::ViewClosed { .. })
            .then_some(()))
            .is_some(),
        "ViewClosed"
    );
    h.rt.shutdown();
    let end = Instant::now() + Duration::from_secs(10);
    while count_children() > 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(100));
    }
    if count_children() > 0 {
        let left = std::process::Command::new("pgrep")
            .args(["-af", "^/[^ ]*/sig-cef-helper --type="])
            .output()
            .expect("pgrep");
        panic!(
            "helper processes left behind after shutdown:\n{}",
            String::from_utf8_lossy(&left.stdout)
        );
    }
}
