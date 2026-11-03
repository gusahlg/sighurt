//! The host against a real compositor: a private headless sway, driven through Wayland
//! protocols only (wtype, wlrctl, swaymsg, grim, wl-clipboard, and two small drivers below for
//! what those tools can't do: a pointer held down across surfaces, and an input method).
//!
//! Every test is `#[ignore]`d, so a plain `cargo test` lists them as ignored; run them with
//! `frontends/sighurt-wayland/tests/headless.sh`, which includes them. Outside that script's compositor a
//! test refuses to start: they must never type into, or click on, a real desktop.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use calloop::EventLoop;
use sighurt_api::geometry::{
    BufferSize, LogicalSize, Metrics, SurfacePoint, SurfaceRect, WindowRect,
};
use sighurt_api::ids::{RequestId, TargetId, WindowId};
use sighurt_api::input::{
    ButtonState, FocusEvent, InputKind, Key, KeyEvent, PointerButton, PointerEvent,
    SurroundingText, TextInputEvent, TextInputState, TextPurpose,
};
use sighurt_api::services::{ClipboardData, Selection};
use sighurt_wayland::{
    FramePermit, Host, HostEvent, InputRegion, Placement, PresentMode, TargetDiagnostics,
    TargetLease, TargetPlacement, WindowOptions, RESIZE_GRACE,
};

const CHROME_HEIGHT: u32 = 40;
const CHROME: [u8; 3] = [0x20, 0x40, 0xa0];
const PAGE: [u8; 3] = [0x20, 0xa0, 0x40];
const BACKGROUND: [u8; 3] = [0x80, 0x10, 0x10];

/// Why every test here is ignored by default.
const HEADLESS_ONLY: &str =
    "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh";

/// Whether this process runs inside tools/headless-sway, as the test script starts it.
fn headless() -> bool {
    let private = std::env::var("XDG_RUNTIME_DIR").is_ok_and(|d| d.starts_with("/tmp/sighl."));
    std::env::var_os("SIGHURT_HEADLESS").is_some() && private
}

/// The application side: a window with a chrome strip and a page, painted in solid colors.
struct App {
    host: Host,
    window: WindowId,
    chrome: TargetLease,
    page: TargetLease,
    size: Option<LogicalSize>,
    events: Vec<HostEvent>,
    /// Every frame submitted: target and buffer size.
    frames: Vec<(TargetId, BufferSize)>,
    permits: usize,
    page_visible: bool,
    chrome_height: u32,
    /// Keep the page's permits instead of presenting them: a slow page.
    hold_page: bool,
    held: Option<FramePermit>,
    /// The page's presenter when it is a GL target.
    gl: Option<gl::Presenter>,
}

impl App {
    fn place(&mut self, size: LogicalSize) {
        let chrome_height = self.chrome_height.min(size.height);
        let entry = |target: TargetId, rect, visible| TargetPlacement {
            target,
            rect,
            visible,
            input: InputRegion::Whole,
        };
        let placement = Placement {
            window: self.window,
            size,
            targets: vec![
                entry(
                    self.chrome.id(),
                    WindowRect {
                        x: 0,
                        y: 0,
                        width: size.width,
                        height: chrome_height,
                    },
                    true,
                ),
                entry(
                    self.page.id(),
                    WindowRect {
                        x: 0,
                        y: chrome_height as i32,
                        width: size.width,
                        height: size.height - chrome_height,
                    },
                    self.page_visible,
                ),
            ],
        };
        self.host
            .apply_placement(placement)
            .expect("a valid placement");
    }

    /// One loop iteration's work, as the executable would do it.
    fn process(&mut self) {
        while let Some(event) = self.host.next_event() {
            if let HostEvent::WindowConfigured { size, .. } = event {
                self.size = Some(size);
                self.place(size);
            }
            self.events.push(event);
        }
        for permit in self.host.permits() {
            self.permits += 1;
            if permit.target() == self.page.id() && self.hold_page {
                self.held = Some(permit);
            } else {
                self.present(permit);
            }
        }
        self.host.flush();
    }

    /// Paints `permit`'s target in its color and submits the frame.
    fn present(&mut self, permit: FramePermit) {
        let target = permit.target();
        if target == self.page.id() && self.page.mode() == PresentMode::Native {
            let presenter = self
                .gl
                .get_or_insert_with(|| gl::Presenter::new(&self.page));
            self.frames.push((target, permit.metrics().buffer()));
            presenter.present(permit, PAGE);
            return;
        }
        let [r, g, b] = if target == self.chrome.id() {
            CHROME
        } else {
            PAGE
        };
        let mut frame = permit.shm_frame().expect("an SHM buffer");
        let size = frame.size();
        let pixels = frame.pixels();
        // A painter may ask its lease and the host anything while it holds the pixels.
        let lease = if target == self.chrome.id() {
            &self.chrome
        } else {
            &self.page
        };
        assert_eq!(lease.metrics().buffer(), size);
        assert!(!self.host.diagnostics().targets.is_empty());
        for pixel in pixels.as_chunks_mut::<4>().0 {
            *pixel = [b, g, r, 0xff];
        }
        frame.submit();
        self.frames.push((target, size));
    }

    fn diagnostics(&self, target: TargetId) -> TargetDiagnostics {
        self.host
            .diagnostics()
            .targets
            .into_iter()
            .find(|t| t.target == target)
            .expect("a live target")
    }

    fn metrics(&self, target: TargetId) -> Option<Metrics> {
        self.host
            .diagnostics()
            .targets
            .into_iter()
            .find(|t| t.target == target)
            .map(|t| t.metrics)
    }

    /// Whether both targets are mapped with a frame for their current metrics.
    fn shown(&self) -> bool {
        let diagnostics = self.host.diagnostics();
        [self.chrome.id(), self.page.id()].iter().all(|id| {
            let t = diagnostics
                .targets
                .iter()
                .find(|t| t.target == *id)
                .expect("a live target");
            t.mapped
                && self
                    .frames
                    .iter()
                    .any(|(f, size)| f == id && *size == t.metrics.buffer())
        })
    }

    fn keys(&self, target: TargetId) -> Vec<&KeyEvent> {
        self.events
            .iter()
            .filter_map(|e| match e {
                HostEvent::Input {
                    target: Some(t),
                    event,
                    ..
                } if *t == target => match &event.kind {
                    InputKind::Key(key) => Some(key),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }

    fn pointer(&self) -> Vec<(Option<TargetId>, &PointerEvent)> {
        self.events
            .iter()
            .filter_map(|e| match e {
                HostEvent::Input { target, event, .. } => match &event.kind {
                    InputKind::Pointer(p) => Some((*target, p)),
                    _ => None,
                },
                _ => None,
            })
            .collect()
    }
}

struct Harness {
    event_loop: EventLoop<'static, App>,
    app: App,
}

impl Harness {
    /// A mapped window with SHM chrome and page, in the headless compositor.
    fn new() -> Self {
        Self::with_page(PresentMode::Shm)
    }

    fn with_page(mode: PresentMode) -> Self {
        assert!(
            headless(),
            "refusing to run outside tools/headless-sway; {HEADLESS_ONLY}"
        );
        sway(&["output HEADLESS-1 resolution 1280x800 scale 1"]);
        let event_loop: EventLoop<'static, App> = EventLoop::try_new().unwrap();
        let mut host =
            Host::new(&event_loop.handle(), |app: &mut App| &mut app.host).expect("a host");
        let window = WindowId::next();
        let options = WindowOptions {
            background: BACKGROUND,
            ..WindowOptions::default()
        };
        host.create_window(window, options);
        let chrome = host.create_target(window, PresentMode::Shm, None).unwrap();
        let page = host.create_target(window, mode, None).unwrap();
        host.set_keyboard_focus(window, Some(page.id()));
        let app = App {
            host,
            window,
            chrome,
            page,
            size: None,
            events: Vec::new(),
            frames: Vec::new(),
            permits: 0,
            page_visible: true,
            chrome_height: CHROME_HEIGHT,
            hold_page: false,
            held: None,
            gl: None,
        };
        let mut harness = Self { event_loop, app };
        harness.wait("the window to map", |app| app.size.is_some() && app.shown());
        harness.pump_for(Duration::from_millis(100));
        harness.app.events.clear();
        harness
    }

    /// One dispatch, waiting at most `timeout`, then the application's work. Returns whether
    /// the loop woke before the timeout.
    fn pump(&mut self, timeout: Duration) -> bool {
        let start = Instant::now();
        self.event_loop
            .dispatch(Some(timeout), &mut self.app)
            .expect("dispatch");
        self.app.process();
        start.elapsed() < timeout
    }

    fn pump_for(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        while let Some(left) = end.checked_duration_since(Instant::now()) {
            self.pump(left);
        }
    }

    fn wait(&mut self, what: &str, done: impl Fn(&App) -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done(&self.app) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            self.pump(Duration::from_millis(20));
        }
    }

    /// Runs `program` to completion while the loop keeps running, and returns its output.
    fn run(&mut self, program: &str, args: &[&str]) -> String {
        let mut child = Command::new(program)
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("{program}: {e}"));
        let end = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(Instant::now() < end, "{program} {args:?} did not finish");
            self.pump(Duration::from_millis(5));
        };
        assert!(status.success(), "{program} {args:?}: {status}");
        let mut output = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut output)
            .unwrap();
        output
    }
}

fn sway(commands: &[&str]) {
    let status = Command::new("swaymsg")
        .args(commands)
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "swaymsg {commands:?}");
}

/// A screenshot of the output, as RGB rows.
struct Shot {
    width: usize,
    rgb: Vec<u8>,
}

impl Shot {
    fn take() -> Self {
        let out = Command::new("grim")
            .args(["-t", "ppm", "-"])
            .output()
            .unwrap();
        assert!(out.status.success());
        // P6\n<width> <height>\n255\n<rgb...>
        let mut fields = out.stdout.splitn(5, |b| b.is_ascii_whitespace());
        let _magic = fields.next();
        let width: usize = std::str::from_utf8(fields.next().unwrap())
            .unwrap()
            .parse()
            .unwrap();
        let _height = fields.next();
        let _max = fields.next();
        Self {
            width,
            rgb: fields.next().unwrap().to_vec(),
        }
    }

    fn at(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.width + x) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn chrome_and_page_are_placed_exactly() {
    let mut h = Harness::new();
    let size = h.app.size.unwrap();
    assert_eq!((size.width, size.height), (1280, 800));
    let chrome = h.app.metrics(h.app.chrome.id()).unwrap();
    let page = h.app.metrics(h.app.page.id()).unwrap();
    assert_eq!(
        chrome.rect(),
        WindowRect {
            x: 0,
            y: 0,
            width: 1280,
            height: 40
        }
    );
    assert_eq!(
        page.rect(),
        WindowRect {
            x: 0,
            y: 40,
            width: 1280,
            height: 760
        }
    );
    assert_eq!(
        page.buffer(),
        BufferSize {
            width: 1280,
            height: 760
        }
    );
    h.pump_for(Duration::from_millis(100));
    let shot = Shot::take();
    assert_eq!(shot.at(5, 5), CHROME);
    assert_eq!(shot.at(1279, 39), CHROME, "the chrome's last row");
    assert_eq!(shot.at(0, 40), PAGE, "the page's first row");
    assert_eq!(shot.at(1279, 799), PAGE);
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn swedish_letters_and_a_dead_key_arrive_as_text() {
    let mut h = Harness::new();
    let page = h.app.page.id();
    // The sleep gives the host time to take the new virtual keyboard before keys arrive.
    h.run(
        "wtype",
        &["-s", "300", "åäö", "-k", "dead_diaeresis", "-k", "a"],
    );
    h.wait("five key presses", |app| {
        app.keys(page)
            .iter()
            .filter(|k| k.state == ButtonState::Pressed)
            .count()
            >= 5
    });
    let presses: Vec<&KeyEvent> = h
        .app
        .keys(page)
        .into_iter()
        .filter(|k| k.state == ButtonState::Pressed)
        .collect();
    let texts: Vec<Option<&str>> = presses.iter().map(|k| k.text.as_deref()).collect();
    assert_eq!(texts, [Some("å"), Some("ä"), Some("ö"), None, Some("ä")]);
    assert_eq!(presses[3].key, Key::Dead(Some('¨')));
    assert_eq!(presses[4].key, Key::Character("ä".into()));
    assert!(presses.iter().all(|k| !k.repeat));
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn altgr_is_not_control_and_alt() {
    let mut h = Harness::new();
    let page = h.app.page.id();
    h.run(
        "wtype",
        &["-s", "300", "-M", "altgr", "-k", "2", "-m", "altgr"],
    );
    h.wait("the key", |app| {
        app.keys(page)
            .iter()
            .any(|k| k.key == Key::Character("2".into()))
    });
    let key = h
        .app
        .keys(page)
        .into_iter()
        .find(|k| k.key == Key::Character("2".into()))
        .unwrap();
    assert!(key.modifiers.alt_graph);
    assert!(!key.modifiers.control && !key.modifiers.alt);
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_held_key_repeats_at_the_compositors_rate() {
    let mut h = Harness::new();
    sway(&["input type:keyboard repeat_delay 200"]);
    sway(&["input type:keyboard repeat_rate 20"]);
    let page = h.app.page.id();
    // Held for 700 ms: repeats at 200, 250, ... 650 ms, about ten.
    h.run("wtype", &["-s", "300", "-P", "x", "-s", "700", "-p", "x"]);
    h.wait("the release", |app| {
        app.keys(page)
            .iter()
            .any(|k| k.state == ButtonState::Released)
    });
    let keys = h.app.keys(page);
    let repeats = keys.iter().filter(|k| k.repeat).count();
    assert!((6..=13).contains(&repeats), "{repeats} repeats");
    assert!(keys
        .iter()
        .filter(|k| k.repeat)
        .all(|k| k.text.as_deref() == Some("x")));
    let released = keys
        .iter()
        .position(|k| k.state == ButtonState::Released)
        .unwrap();
    h.pump_for(Duration::from_millis(200));
    assert_eq!(
        h.app.keys(page).len(),
        released + 1,
        "no repeat after the release"
    );
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn pointer_positions_are_local_to_the_surface_under_the_pointer() {
    let mut h = Harness::new();
    let (chrome, page) = (h.app.chrome.id(), h.app.page.id());
    let mut pointer = driver::Pointer::new((1280, 800));
    h.pump_for(Duration::from_millis(300));
    pointer.move_to(100, 20);
    h.wait("the chrome", |app| {
        app.pointer().iter().any(|(t, _)| *t == Some(chrome))
    });
    pointer.move_to(100, 39);
    pointer.move_to(100, 40);
    pointer.move_to(300, 250);
    h.wait("the page", |app| {
        app.pointer().iter().any(|(t, e)| {
            *t == Some(page)
                && matches!(e, PointerEvent::Motion { position, .. } if position.y == 210.0)
        })
    });
    let seen: Vec<(Option<TargetId>, PointerEvent)> = h
        .app
        .pointer()
        .into_iter()
        .map(|(t, e)| (t, e.clone()))
        .collect();
    let position = |e: &PointerEvent| match e {
        PointerEvent::Enter { position } | PointerEvent::Motion { position, .. } => Some(*position),
        _ => None,
    };
    let chrome_points: Vec<SurfacePoint> = seen
        .iter()
        .filter(|(t, _)| *t == Some(chrome))
        .filter_map(|(_, e)| position(e))
        .collect();
    assert!(
        chrome_points.contains(&SurfacePoint { x: 100.0, y: 20.0 }),
        "{chrome_points:?}"
    );
    assert!(
        chrome_points.contains(&SurfacePoint { x: 100.0, y: 39.0 }),
        "{chrome_points:?}"
    );
    let page_points: Vec<SurfacePoint> = seen
        .iter()
        .filter(|(t, _)| *t == Some(page))
        .filter_map(|(_, e)| position(e))
        .collect();
    assert_eq!(
        page_points.first(),
        Some(&SurfacePoint { x: 100.0, y: 0.0 }),
        "window y 40 is the page's first row"
    );
    assert!(page_points.contains(&SurfacePoint { x: 300.0, y: 210.0 }));
    // Scrolling goes to the page, where the pointer is.
    h.run("wlrctl", &["pointer", "scroll", "10", "0"]);
    h.wait("a scroll", |app| {
        app.pointer()
            .iter()
            .any(|(_, e)| matches!(e, PointerEvent::Scroll { .. }))
    });
    assert!(h
        .app
        .pointer()
        .iter()
        .all(|(t, e)| !matches!(e, PointerEvent::Scroll { .. }) || *t == Some(page)));
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_drag_from_the_page_into_the_chrome_stays_with_the_page() {
    let mut h = Harness::new();
    let (chrome, page) = (h.app.chrome.id(), h.app.page.id());
    let mut pointer = driver::Pointer::new((1280, 800));
    h.pump_for(Duration::from_millis(300));
    pointer.move_to(200, 300);
    h.wait("the page", |app| {
        app.pointer().iter().any(|(t, _)| *t == Some(page))
    });
    h.app.events.clear();
    pointer.button(driver::BTN_LEFT, true);
    pointer.move_to(200, 100);
    pointer.move_to(210, 10); // In the chrome now, button still down.
    pointer.button(driver::BTN_LEFT, false);
    h.wait("the chrome after the release", |app| {
        app.pointer().iter().any(|(t, _)| *t == Some(chrome))
    });
    let events: Vec<(Option<TargetId>, PointerEvent)> = h
        .app
        .pointer()
        .into_iter()
        .map(|(t, e)| (t, e.clone()))
        .collect();
    let release = events
        .iter()
        .position(|(_, e)| {
            matches!(
                e,
                PointerEvent::Button {
                    state: ButtonState::Released,
                    ..
                }
            )
        })
        .expect("a release");
    for (target, event) in &events[..=release] {
        assert_eq!(
            *target,
            Some(page),
            "{event:?} went elsewhere during the drag"
        );
    }
    match &events[release].1 {
        PointerEvent::Button {
            button, position, ..
        } => {
            assert_eq!(*button, PointerButton::Left);
            // Window (210, 10) in the page's coordinates.
            assert_eq!(*position, SurfacePoint { x: 210.0, y: -30.0 });
        }
        other => panic!("{other:?}"),
    }
    let after: Vec<_> = events[release + 1..]
        .iter()
        .map(|(t, e)| (*t, std::mem::discriminant(e)))
        .collect();
    assert_eq!(
        after.first(),
        Some(&(Some(page), std::mem::discriminant(&PointerEvent::Leave)))
    );
    assert!(after.iter().any(|(t, _)| *t == Some(chrome)));
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn resizes_and_scale_changes_reach_the_buffers() {
    let mut h = Harness::new();
    let page = h.app.page.id();
    sway(&["output HEADLESS-1 resolution 1000x700"]);
    h.wait("the new size", |app| {
        app.size.is_some_and(|s| s.width == 1000) && app.shown()
    });
    sway(&["output HEADLESS-1 scale 1.5"]);
    h.wait("the new scale", |app| {
        app.events.iter().any(
            |e| matches!(e, HostEvent::WindowConfigured { scale, .. } if scale.numerator() == 180),
        ) && app.shown()
    });
    let metrics = h.app.metrics(page).unwrap();
    let rect = metrics.rect();
    assert_eq!(metrics.scale().numerator(), 180);
    assert_eq!(metrics.buffer(), metrics.scale().buffer_size(rect.size()));
    assert!(h
        .app
        .frames
        .iter()
        .any(|(t, size)| *t == page && *size == metrics.buffer()));
    h.pump_for(Duration::from_millis(100));
    // The screenshot is in output pixels: the chrome's 40 logical rows are 60.
    let shot = Shot::take();
    assert_eq!(shot.at(10, 58), CHROME);
    assert_eq!(shot.at(10, 61), PAGE);
    assert_eq!(shot.at(990, 690), PAGE);
    // The same logical size at another scale still gets new buffers.
    let before = h.app.metrics(page).unwrap();
    sway(&["output HEADLESS-1 resolution 1500x1050 scale 2.25"]);
    h.wait("scale 2.25", |app| {
        app.metrics(page)
            .is_some_and(|m| m.scale().numerator() == 270)
            && app.shown()
    });
    let after = h.app.metrics(page).unwrap();
    assert_eq!(
        after.rect().size(),
        before.rect().size(),
        "1500 / 2.25 = 1000 / 1.5"
    );
    assert_ne!(after.epoch(), before.epoch());
    assert_eq!(
        after.buffer(),
        after.scale().buffer_size(after.rect().size())
    );
    assert!(
        after.buffer().width.abs_diff(1500) <= 2,
        "{:?}",
        after.buffer()
    );
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn an_idle_window_sleeps() {
    let mut h = Harness::new();
    h.pump_for(Duration::from_millis(300));
    let (permits, frames) = (h.app.permits, h.app.frames.len());
    let start = Instant::now();
    let mut wakeups = 0;
    while start.elapsed() < Duration::from_secs(2) {
        if h.pump(Duration::from_secs(2) - start.elapsed()) {
            wakeups += 1;
        }
    }
    eprintln!(
        "idle for 2 s: {wakeups} wakeups, {} permits",
        h.app.permits - permits
    );
    assert_eq!(h.app.permits, permits, "no permits while idle");
    assert_eq!(h.app.frames.len(), frames, "no frames while idle");
    assert!(wakeups <= 1, "{wakeups} wakeups in two idle seconds");
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_resize_storm_keeps_buffers_bounded() {
    let mut h = Harness::new();
    let mut worst = (0, 0);
    for i in 0..30 {
        let mode = format!(
            "output HEADLESS-1 resolution {}x{}",
            800 + i * 13,
            600 + i * 7
        );
        h.run("swaymsg", &[&mode]);
        for target in h.app.host.diagnostics().targets {
            worst.0 = worst.0.max(target.buffers.buffers);
            worst.1 = worst.1.max(target.buffers.retiring_generations);
        }
    }
    h.wait("the last size", |app| {
        app.size.is_some_and(|s| s.width == 800 + 29 * 13) && app.shown()
    });
    h.pump_for(Duration::from_millis(300));
    eprintln!(
        "resize storm: at most {} buffers and {} retiring generations per target",
        worst.0, worst.1
    );
    assert!(worst.0 <= 9, "{} buffers held at once", worst.0);
    assert!(worst.1 <= 2, "{} generations retiring at once", worst.1);
    for target in h.app.host.diagnostics().targets {
        assert!(target.buffers.buffers <= 3, "{:?}", target.buffers);
    }
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_second_placement_joins_the_layout_without_waiting_for_the_grace_period() {
    let mut h = Harness::new();
    let (chrome, page) = (h.app.chrome.id(), h.app.page.id());
    let size = h.app.size.unwrap();
    // A slow page holds the first layout open. The chrome's frame for it is in, a synchronized
    // commit the compositor keeps until the root commits.
    h.app.hold_page = true;
    h.app.chrome_height = 50;
    h.app.place(size);
    h.app.process();
    assert!(h.app.held.is_some(), "the page's permit is out");
    h.pump_for(Duration::from_millis(20));
    // The page gives its permit back unused, and a second placement arrives.
    h.app.held = None;
    h.app.hold_page = false;
    h.app.chrome_height = 60;
    let placed = Instant::now();
    h.app.place(size);
    h.app.process();
    eprintln!("second placement to both frames: {:?}", placed.elapsed());
    assert!(
        h.app.shown(),
        "both frames for the second layout were permitted in the same iteration"
    );
    h.pump_for(RESIZE_GRACE + Duration::from_millis(100));
    for target in [chrome, page] {
        let d = h.app.diagnostics(target);
        assert!(d.mapped, "{d:?}");
        assert_eq!(
            d.stats.late, 0,
            "the layout committed with every frame: {d:?}"
        );
    }
    let shot = Shot::take();
    assert_eq!(shot.at(5, 59), CHROME);
    assert_eq!(shot.at(5, 60), PAGE);
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_frame_after_the_grace_period_still_maps_its_page() {
    let mut h = Harness::new();
    let page = h.app.page.id();
    let size = h.app.size.unwrap();
    h.app.hold_page = true;
    h.app.chrome_height = 45;
    // One loop iteration, as the executable runs one: the placement, then its permits.
    h.app.place(size);
    h.app.process();
    assert!(h.app.held.is_some(), "the page's permit is out");
    h.pump_for(RESIZE_GRACE * 2);
    // The layout went ahead without the page, showing the background where it goes.
    let d = h.app.diagnostics(page);
    assert!(!d.mapped && d.shown, "{d:?}");
    assert_eq!(d.stats.late, 1);
    let shot = Shot::take();
    assert_eq!(shot.at(5, 44), CHROME);
    assert_eq!(shot.at(500, 400), BACKGROUND);
    // Its frame comes late, for the layout that is still current, and is what maps it.
    let permit = h.app.held.take().expect("the page's permit");
    assert!(permit.is_current());
    h.app.hold_page = false;
    let permits = h.app.permits;
    h.app.present(permit);
    h.app.host.flush();
    assert!(
        h.app.diagnostics(page).mapped,
        "mapped at the flush after its frame"
    );
    h.pump_for(Duration::from_millis(100));
    assert_eq!(h.app.permits, permits, "no other frame was needed");
    let shot = Shot::take();
    assert_eq!(shot.at(5, 44), CHROME);
    assert_eq!(shot.at(5, 45), PAGE);
    assert_eq!(shot.at(500, 400), PAGE);
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn the_clipboard_goes_both_ways() {
    let mut h = Harness::new();
    if Command::new("wl-paste").arg("--version").output().is_err() {
        panic!("wl-clipboard is not on PATH; headless.sh puts it there");
    }
    // Setting the selection needs a serial from real input.
    h.run("wtype", &["-s", "300", "-k", "Shift_L"]);
    h.wait("keyboard focus", |app| {
        app.events.iter().any(|e| matches!(e, HostEvent::Input { event, .. } if matches!(event.kind, InputKind::Focus(FocusEvent::Activated))))
    });
    let text = "räksmörgås från sighurt";
    let item = ClipboardData {
        mime_type: "text/plain;charset=utf-8".into(),
        data: text.as_bytes().to_vec(),
    };
    h.app
        .host
        .clipboard_write(Selection::Clipboard, vec![item])
        .expect("the clipboard is writable");
    h.pump_for(Duration::from_millis(50));
    assert_eq!(h.run("wl-paste", &["--no-newline"]), text);
    h.run("wl-copy", &["--", "hej från wl-copy"]);
    h.pump_for(Duration::from_millis(100));
    let request = RequestId::next();
    let types = ["text/plain;charset=utf-8".to_string()];
    h.app
        .host
        .clipboard_read(request, Selection::Clipboard, &types, 1024);
    h.wait("the read", |app| {
        app.events
            .iter()
            .any(|e| matches!(e, HostEvent::ClipboardRead { request: r, .. } if *r == request))
    });
    let result = h.app.events.iter().find_map(|e| match e {
        HostEvent::ClipboardRead { result, .. } => Some(result.clone()),
        _ => None,
    });
    assert_eq!(result.unwrap().unwrap().data, "hej från wl-copy".as_bytes());
    // A read over its limit is refused, not truncated.
    let request = RequestId::next();
    h.app
        .host
        .clipboard_read(request, Selection::Clipboard, &types, 4);
    h.wait("the small read", |app| {
        app.events
            .iter()
            .any(|e| matches!(e, HostEvent::ClipboardRead { request: r, .. } if *r == request))
    });
    let too_large = h.app.events.iter().any(|e| {
        matches!(e, HostEvent::ClipboardRead { request: r, result: Err(sighurt_api::services::ServiceError::TooLarge) } if *r == request)
    });
    assert!(too_large);
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn the_input_method_sees_passwords_as_passwords_and_its_text_arrives() {
    let mut h = Harness::new();
    assert!(h.app.host.capabilities().text_input);
    let mut ime = driver::InputMethod::new();
    h.pump_for(Duration::from_millis(200));
    let (window, page) = (h.app.window, h.app.page.id());
    let field = TextInputState {
        caret: SurfaceRect {
            x: 10,
            y: 20,
            width: 2,
            height: 18,
        },
        surrounding: None,
        purpose: TextPurpose::Password,
        multiline: false,
    };
    h.app.host.set_text_input(window, page, Some(field));
    h.pump_for(Duration::from_millis(100));
    let state = ime.poll();
    assert!(state.active, "the input method was activated");
    assert_eq!(state.purpose, Some(driver::PASSWORD));
    assert!(
        state.hint & driver::SENSITIVE_AND_HIDDEN == driver::SENSITIVE_AND_HIDDEN,
        "hint {:#x}",
        state.hint
    );
    ime.preedit("ö");
    h.wait("the preedit", |app| {
        text_inputs(app, page)
            .iter()
            .any(|e| e.preedit.as_ref().is_some_and(|p| p.text == "ö"))
    });
    ime.commit("ö");
    h.wait("the commit", |app| {
        text_inputs(app, page)
            .iter()
            .any(|e| e.commit.as_deref() == Some("ö"))
    });
    let last = text_inputs(&h.app, page).last().cloned().unwrap();
    assert_eq!(last.preedit, None, "the commit replaced the preedit");
    h.app.host.set_text_input(window, page, None);
    h.pump_for(Duration::from_millis(100));
    assert!(!ime.poll().active, "turned off with the field");
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_new_field_on_the_same_target_is_a_new_input_method_session() {
    let mut h = Harness::new();
    let mut ime = driver::InputMethod::new();
    h.pump_for(Duration::from_millis(200));
    let (window, page) = (h.app.window, h.app.page.id());
    let caret = SurfaceRect {
        x: 10,
        y: 20,
        width: 2,
        height: 18,
    };
    let username = TextInputState {
        caret,
        surrounding: Some(SurroundingText {
            text: "anna".into(),
            cursor: 4,
            anchor: 4,
        }),
        purpose: TextPurpose::Normal,
        multiline: false,
    };
    h.app.host.set_text_input(window, page, Some(username));
    h.pump_for(Duration::from_millis(100));
    let state = ime.poll();
    assert!(state.active);
    assert_eq!(state.surrounding.as_deref(), Some("anna"));
    // Focus moves to the password field below, in the same page.
    let password = TextInputState {
        caret: SurfaceRect { y: 60, ..caret },
        surrounding: None,
        purpose: TextPurpose::Password,
        multiline: false,
    };
    h.app.host.set_text_input(window, page, Some(password));
    h.pump_for(Duration::from_millis(100));
    let state = ime.poll();
    eprintln!(
        "the input method's surrounding text now: {:?}",
        state.surrounding
    );
    assert!(state.active);
    assert_eq!(state.purpose, Some(driver::PASSWORD));
    assert_ne!(
        state.surrounding.as_deref(),
        Some("anna"),
        "the input method started over: the username is not the password field's text"
    );
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_gl_presenter_owns_its_leaf_from_first_frame_to_teardown() {
    let mut h = Harness::with_page(PresentMode::Native);
    let page = h.app.page.id();
    assert!(h.app.gl.is_some(), "the page presented through EGL");
    h.pump_for(Duration::from_millis(100));
    let shot = Shot::take();
    assert_eq!(shot.at(5, 5), CHROME);
    assert_eq!(shot.at(5, 40), PAGE, "the EGL frame, placed by the host");
    assert_eq!(shot.at(1279, 799), PAGE);

    // A resize: new metrics, and the presenter's next swap brings the new size.
    sway(&["output HEADLESS-1 resolution 1000x700"]);
    h.wait("the resized page", |app| {
        app.size.is_some_and(|s| s.width == 1000) && app.shown()
    });
    h.pump_for(Duration::from_millis(100));
    let shot = Shot::take();
    assert_eq!(shot.at(999, 699), PAGE);
    assert_eq!(shot.at(5, 39), CHROME);

    // Hidden: unmapped by the host without touching the leaf's buffers; no permits meanwhile.
    h.app.page_visible = false;
    h.app.place(h.app.size.unwrap());
    h.app.host.invalidate(page);
    h.pump_for(Duration::from_millis(200));
    let hidden_frames = h.app.frames.iter().filter(|(t, _)| *t == page).count();
    assert_eq!(Shot::take().at(500, 400), BACKGROUND);
    h.app.host.invalidate(page);
    h.pump_for(Duration::from_millis(100));
    assert_eq!(
        h.app.frames.iter().filter(|(t, _)| *t == page).count(),
        hidden_frames
    );

    // Shown again: mapped with a new role and a bootstrapped frame, without waiting for the
    // frame callback that was outstanding when it was hidden.
    h.app.page_visible = true;
    h.app.place(h.app.size.unwrap());
    h.wait("the page again", |app| {
        app.frames.iter().filter(|(t, _)| *t == page).count() > hidden_frames && app.shown()
    });
    h.pump_for(Duration::from_millis(100));
    assert_eq!(Shot::take().at(500, 400), PAGE);

    // Teardown in the documented order: the presenter's EGL objects, then the lease; the host
    // destroys the surface afterwards, and the connection stays healthy.
    h.app.gl.take().unwrap().destroy();
    let replacement = h
        .app
        .host
        .create_target(h.app.window, PresentMode::Shm, None)
        .unwrap();
    drop(std::mem::replace(&mut h.app.page, replacement));
    h.app.place(h.app.size.unwrap());
    h.wait("the replacement page", |app| app.shown());
    h.pump_for(Duration::from_millis(200));
    assert!(!h.app.host.is_lost());
    assert!(!h
        .app
        .events
        .iter()
        .any(|e| matches!(e, HostEvent::ConnectionLost { .. })));
    assert!(
        h.app
            .host
            .diagnostics()
            .targets
            .iter()
            .all(|t| t.target != page),
        "the old target is gone"
    );
}

fn text_inputs(app: &App, target: TargetId) -> Vec<TextInputEvent> {
    app.events
        .iter()
        .filter_map(|e| match e {
            HostEvent::Input {
                target: Some(t),
                event,
                ..
            } if *t == target => match &event.kind {
                InputKind::TextInput(e) => Some(e.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
#[ignore = "needs tools/headless-sway: run frontends/sighurt-wayland/tests/headless.sh"]
fn a_close_request_goes_to_the_browser() {
    let mut h = Harness::new();
    sway(&["kill"]);
    h.wait("the close request", |app| {
        app.events
            .iter()
            .any(|e| matches!(e, HostEvent::CloseRequested { .. }))
    });
    assert!(h.app.shown(), "the host doesn't close on its own");
    // The browser closes it while presenters still hold their leases: nothing more is shown on
    // the targets, and dropping the leases afterwards is fine.
    let frames = h.app.frames.len();
    h.app.host.close_window(h.app.window);
    h.app.host.invalidate(h.app.page.id());
    h.pump_for(Duration::from_millis(200));
    assert_eq!(h.app.frames.len(), frames);
    let App { chrome, page, .. } = &mut h.app;
    let _ = (chrome.metrics(), page.metrics());
    h.app.host.flush();
    h.pump_for(Duration::from_millis(100));
    assert!(!h.app.host.is_lost(), "no protocol error");
}

/// Ends the compositor, so it runs last and alone (the script runs it with `--ignored`).
#[test]
#[ignore = "needs tools/headless-sway and stops it: headless.sh runs it last"]
fn losing_the_compositor_is_reported_once() {
    let mut h = Harness::new();
    let _ = Command::new("swaymsg").arg("exit").status();
    h.wait("the report", |app| {
        app.events
            .iter()
            .any(|e| matches!(e, HostEvent::ConnectionLost { .. }))
    });
    h.pump_for(Duration::from_millis(200));
    let reports = h
        .app
        .events
        .iter()
        .filter(|e| matches!(e, HostEvent::ConnectionLost { .. }))
        .count();
    assert_eq!(reports, 1);
    assert!(h.app.host.is_lost());
    // The loop keeps working, and the leases can still be dropped cleanly.
    let App { chrome, page, .. } = h.app;
    drop((chrome, page));
}

/// A minimal EGL presenter, as an engine adapter would make one from a GL lease: libEGL and
/// libwayland-egl loaded at run time, a GLES 2 context, swap interval 0, one clear per frame.
mod gl {
    use std::ffi::{c_char, c_void};
    use std::ptr::null_mut;

    use libloading::{Library, Symbol};
    use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
    use sighurt_api::engine::PresentOutcome;
    use sighurt_api::geometry::BufferSize;
    use sighurt_wayland::{FramePermit, TargetLease};

    type Ptr = *mut c_void;
    const EGL_PLATFORM_WAYLAND_KHR: u32 = 0x31D8;
    const EGL_OPENGL_ES_API: u32 = 0x30A0;
    const EGL_NONE: i32 = 0x3038;
    const GL_COLOR_BUFFER_BIT: u32 = 0x4000;

    pub struct Presenter {
        egl: Library,
        wl_egl: Library,
        display: Ptr,
        context: Ptr,
        surface: Ptr,
        window: Ptr,
        size: BufferSize,
    }

    /// `name` from `lib`, as the function type `T`.
    unsafe fn f<T: Copy>(lib: &Library, name: &[u8]) -> T {
        let symbol: Symbol<T> = unsafe { lib.get(name) }.unwrap_or_else(|e| panic!("{e}"));
        *symbol
    }

    impl Presenter {
        pub fn new(lease: &TargetLease) -> Self {
            // SAFETY: everything made from the handles is destroyed in `destroy`, which the test
            // calls before dropping the lease.
            let handles = unsafe { lease.gl_handles() }.expect("a GL target");
            let RawDisplayHandle::Wayland(display) = handles.display_handle().unwrap().as_raw()
            else {
                panic!()
            };
            let RawWindowHandle::Wayland(window) = handles.window_handle().unwrap().as_raw() else {
                panic!()
            };
            let size = lease.metrics().buffer();
            unsafe {
                let egl = Library::new("libEGL.so.1").unwrap();
                let wl_egl = Library::new("libwayland-egl.so.1").unwrap();
                let get_display: unsafe extern "C" fn(u32, Ptr, *const isize) -> Ptr =
                    f(&egl, b"eglGetPlatformDisplay\0");
                let initialize: unsafe extern "C" fn(Ptr, *mut i32, *mut i32) -> u32 =
                    f(&egl, b"eglInitialize\0");
                let bind_api: unsafe extern "C" fn(u32) -> u32 = f(&egl, b"eglBindAPI\0");
                let choose: unsafe extern "C" fn(Ptr, *const i32, *mut Ptr, i32, *mut i32) -> u32 =
                    f(&egl, b"eglChooseConfig\0");
                let create_context: unsafe extern "C" fn(Ptr, Ptr, Ptr, *const i32) -> Ptr =
                    f(&egl, b"eglCreateContext\0");
                let create_surface: unsafe extern "C" fn(Ptr, Ptr, Ptr, *const i32) -> Ptr =
                    f(&egl, b"eglCreateWindowSurface\0");
                let make_current: unsafe extern "C" fn(Ptr, Ptr, Ptr, Ptr) -> u32 =
                    f(&egl, b"eglMakeCurrent\0");
                let swap_interval: unsafe extern "C" fn(Ptr, i32) -> u32 =
                    f(&egl, b"eglSwapInterval\0");
                let window_create: unsafe extern "C" fn(Ptr, i32, i32) -> Ptr =
                    f(&wl_egl, b"wl_egl_window_create\0");

                let display = get_display(
                    EGL_PLATFORM_WAYLAND_KHR,
                    display.display.as_ptr(),
                    std::ptr::null(),
                );
                assert!(
                    !display.is_null() && initialize(display, null_mut(), null_mut()) == 1,
                    "EGL on Wayland"
                );
                assert_eq!(bind_api(EGL_OPENGL_ES_API), 1);
                // Window-renderable, GLES 2, 8-bit RGB.
                let attributes = [
                    0x3033, 0x0004, 0x3040, 0x0004, 0x3024, 8, 0x3023, 8, 0x3022, 8, EGL_NONE,
                ];
                let (mut config, mut count) = (null_mut(), 0);
                assert_eq!(
                    choose(display, attributes.as_ptr(), &mut config, 1, &mut count),
                    1
                );
                assert_eq!(count, 1);
                let context =
                    create_context(display, config, null_mut(), [0x3098, 2, EGL_NONE].as_ptr());
                let window = window_create(
                    window.surface.as_ptr(),
                    size.width as i32,
                    size.height as i32,
                );
                let surface = create_surface(display, config, window, [EGL_NONE].as_ptr());
                assert!(!context.is_null() && !window.is_null() && !surface.is_null());
                assert_eq!(make_current(display, surface, surface, context), 1);
                // The host paces frames; a swap must never wait for a callback.
                assert_eq!(swap_interval(display, 0), 1);
                Self {
                    egl,
                    wl_egl,
                    display,
                    context,
                    surface,
                    window,
                    size,
                }
            }
        }

        /// Renders one frame at the permit's size and swaps, which commits the leaf.
        pub fn present(&mut self, permit: FramePermit, [r, g, b]: [u8; 3]) {
            let size = permit.metrics().buffer();
            unsafe {
                if size != self.size {
                    let resize: unsafe extern "C" fn(Ptr, i32, i32, i32, i32) =
                        f(&self.wl_egl, b"wl_egl_window_resize\0");
                    resize(self.window, size.width as i32, size.height as i32, 0, 0);
                    self.size = size;
                }
                let proc_address: unsafe extern "C" fn(*const c_char) -> Ptr =
                    f(&self.egl, b"eglGetProcAddress\0");
                let viewport: unsafe extern "C" fn(i32, i32, i32, i32) =
                    std::mem::transmute(proc_address(c"glViewport".as_ptr()));
                let clear_color: unsafe extern "C" fn(f32, f32, f32, f32) =
                    std::mem::transmute(proc_address(c"glClearColor".as_ptr()));
                let clear: unsafe extern "C" fn(u32) =
                    std::mem::transmute(proc_address(c"glClear".as_ptr()));
                let swap: unsafe extern "C" fn(Ptr, Ptr) -> u32 = f(&self.egl, b"eglSwapBuffers\0");
                viewport(0, 0, size.width as i32, size.height as i32);
                let channel = |c: u8| f32::from(c) / 255.0;
                clear_color(channel(r), channel(g), channel(b), 1.0);
                clear(GL_COLOR_BUFFER_BIT);
                assert_eq!(swap(self.display, self.surface), 1);
            }
            permit.finish(PresentOutcome::Submitted);
        }

        /// Destroys everything made from the lease, before the lease goes.
        pub fn destroy(self) {
            unsafe {
                let make_current: unsafe extern "C" fn(Ptr, Ptr, Ptr, Ptr) -> u32 =
                    f(&self.egl, b"eglMakeCurrent\0");
                let destroy_surface: unsafe extern "C" fn(Ptr, Ptr) -> u32 =
                    f(&self.egl, b"eglDestroySurface\0");
                let destroy_context: unsafe extern "C" fn(Ptr, Ptr) -> u32 =
                    f(&self.egl, b"eglDestroyContext\0");
                let terminate: unsafe extern "C" fn(Ptr) -> u32 = f(&self.egl, b"eglTerminate\0");
                let window_destroy: unsafe extern "C" fn(Ptr) =
                    f(&self.wl_egl, b"wl_egl_window_destroy\0");
                make_current(self.display, null_mut(), null_mut(), null_mut());
                destroy_surface(self.display, self.surface);
                window_destroy(self.window);
                destroy_context(self.display, self.context);
                terminate(self.display);
            }
        }
    }
}

/// Drivers for what the command-line tools can't do. Test code: blocking roundtrips are fine.
mod driver {
    use smithay_client_toolkit::reexports::protocols_misc::zwp_input_method_v2::client::{
        zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
        zwp_input_method_v2::{self, ZwpInputMethodV2},
    };
    use smithay_client_toolkit::reexports::protocols_wlr::virtual_pointer::v1::client::{
        zwlr_virtual_pointer_manager_v1::ZwlrVirtualPointerManagerV1,
        zwlr_virtual_pointer_v1::ZwlrVirtualPointerV1,
    };
    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::wl_pointer::ButtonState;
    use wayland_client::protocol::wl_registry::WlRegistry;
    use wayland_client::protocol::wl_seat::WlSeat;
    use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum};

    pub const BTN_LEFT: u32 = 0x110;
    /// `zwp_text_input_v3.content_purpose.password`.
    pub const PASSWORD: u32 = 8;
    /// `zwp_text_input_v3.content_hint`: `hidden_text | sensitive_data`.
    pub const SENSITIVE_AND_HIDDEN: u32 = 0x40 | 0x80;

    #[derive(Default)]
    pub struct State {
        pub active: bool,
        pub hint: u32,
        pub purpose: Option<u32>,
        pub surrounding: Option<String>,
        dones: u32,
    }

    impl Dispatch<WlRegistry, GlobalListContents> for State {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: <WlRegistry as wayland_client::Proxy>::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    wayland_client::delegate_noop!(State: ignore WlSeat);
    wayland_client::delegate_noop!(State: ZwlrVirtualPointerManagerV1);
    wayland_client::delegate_noop!(State: ZwlrVirtualPointerV1);
    wayland_client::delegate_noop!(State: ZwpInputMethodManagerV2);

    impl Dispatch<ZwpInputMethodV2, ()> for State {
        fn event(
            state: &mut Self,
            _: &ZwpInputMethodV2,
            event: zwp_input_method_v2::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                zwp_input_method_v2::Event::Activate => state.active = true,
                zwp_input_method_v2::Event::Deactivate => state.active = false,
                zwp_input_method_v2::Event::ContentType { hint, purpose } => {
                    state.hint = match hint {
                        WEnum::Value(h) => h.bits(),
                        WEnum::Unknown(u) => u,
                    };
                    state.purpose = Some(match purpose {
                        WEnum::Value(p) => p as u32,
                        WEnum::Unknown(u) => u,
                    });
                }
                zwp_input_method_v2::Event::SurroundingText { text, .. } => {
                    state.surrounding = Some(text);
                }
                zwp_input_method_v2::Event::Done => state.dones += 1,
                _ => {}
            }
        }
    }

    fn connect() -> (
        Connection,
        EventQueue<State>,
        wayland_client::globals::GlobalList,
    ) {
        let conn = Connection::connect_to_env().unwrap();
        let (globals, queue) = registry_queue_init::<State>(&conn).unwrap();
        (conn, queue, globals)
    }

    /// A virtual pointer with absolute positions in the output's logical pixels.
    pub struct Pointer {
        _conn: Connection,
        queue: EventQueue<State>,
        pointer: ZwlrVirtualPointerV1,
        extent: (u32, u32),
        time: u32,
    }

    impl Pointer {
        pub fn new(extent: (u32, u32)) -> Self {
            let (conn, mut queue, globals) = connect();
            let qh = queue.handle();
            let seat: WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
            let manager: ZwlrVirtualPointerManagerV1 = globals.bind(&qh, 1..=1, ()).unwrap();
            let pointer = manager.create_virtual_pointer(Some(&seat), &qh, ());
            queue.roundtrip(&mut State::default()).unwrap();
            Self {
                _conn: conn,
                queue,
                pointer,
                extent,
                time: 0,
            }
        }

        fn frame(&mut self) {
            self.pointer.frame();
            self.queue.roundtrip(&mut State::default()).unwrap();
        }

        pub fn move_to(&mut self, x: u32, y: u32) {
            self.time += 10;
            self.pointer
                .motion_absolute(self.time, x, y, self.extent.0, self.extent.1);
            self.frame();
        }

        pub fn button(&mut self, button: u32, pressed: bool) {
            self.time += 10;
            let state = if pressed {
                ButtonState::Pressed
            } else {
                ButtonState::Released
            };
            self.pointer.button(self.time, button, state);
            self.frame();
        }
    }

    /// An input method for the seat.
    pub struct InputMethod {
        _conn: Connection,
        queue: EventQueue<State>,
        im: ZwpInputMethodV2,
        state: State,
    }

    impl InputMethod {
        pub fn new() -> Self {
            let (conn, mut queue, globals) = connect();
            let qh = queue.handle();
            let seat: WlSeat = globals.bind(&qh, 1..=1, ()).unwrap();
            let manager: ZwpInputMethodManagerV2 = globals.bind(&qh, 1..=1, ()).unwrap();
            let im = manager.get_input_method(&seat, &qh, ());
            let mut state = State::default();
            queue.roundtrip(&mut state).unwrap();
            Self {
                _conn: conn,
                queue,
                im,
                state,
            }
        }

        /// What the compositor told the input method so far.
        pub fn poll(&mut self) -> &State {
            self.queue.roundtrip(&mut self.state).unwrap();
            &self.state
        }

        pub fn preedit(&mut self, text: &str) {
            self.im
                .set_preedit_string(text.into(), 0, text.len() as i32);
            self.im.commit(self.state.dones);
            self.poll();
        }

        pub fn commit(&mut self, text: &str) {
            self.im.commit_string(text.into());
            self.im.commit(self.state.dones);
            self.poll();
        }
    }
}
