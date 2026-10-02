//! The Wasm runtime in process, driven the way the browser's owner loop drives it: commands in,
//! events out of `pump`, frames through `present`.
//!
//! Tests that use the example guests need them built first:
//! `tools/cargo build --target wasm32-unknown-unknown --release -p hello-sighurt -p raf-demo
//! -p gradient-demo -p typography-demo`.

mod common;

use std::time::{Duration, Instant};

use common::{committed_url, example, guest, is_frame_ready, metrics, Host};
use sighurt_api::engine::{EngineCommand, EngineRuntime, PresentOutcome};
use sighurt_api::events::{Cursor, EngineEvent, NavigationError, OpenDisposition};
use sighurt_api::geometry::SurfacePoint;
use sighurt_api::ids::{NavigationId, ViewId};
use sighurt_api::input::{
    ButtonState, InputKind, Key, KeyCode, KeyEvent, Modifiers, PointerButton, PointerEvent,
};
use sighurt_engine_wasm::SandboxPolicy;

const SECS: Duration = Duration::from_secs(1);

/// Loads `url` into a new 800 × 600 view at 100 % and waits until it has committed.
fn load(host: &mut Host, url: &str) -> (ViewId, common::Shown) {
    let (view, shown) = host.open(800, 600, 120);
    let navigation = host.navigate(view, url);
    host.until(20 * SECS, |event| {
        matches!(event, EngineEvent::NavigationCommitted { navigation: n, .. } if *n == navigation)
    });
    (view, shown)
}

fn failed(view: ViewId) -> impl Fn(&EngineEvent) -> bool {
    move |event| matches!(event, EngineEvent::ViewFailed { view: v, .. } if *v == view)
}

fn failure(events: &[EngineEvent]) -> &str {
    match events.last() {
        Some(EngineEvent::ViewFailed { message, .. }) => message,
        other => panic!("expected ViewFailed, got {other:?}"),
    }
}

fn press(key: Key, code: u32, text: Option<&str>, modifiers: Modifiers) -> InputKind {
    InputKind::Key(KeyEvent {
        code: KeyCode(code),
        key,
        state: ButtonState::Pressed,
        repeat: false,
        modifiers,
        consumed: Modifiers::default(),
        text: text.map(Into::into),
    })
}

fn pointer(event: PointerEvent) -> InputKind {
    InputKind::Pointer(event)
}

fn at(x: f64, y: f64) -> SurfacePoint {
    SurfacePoint { x, y }
}

fn click(host: &mut Host, view: ViewId, button: PointerButton, position: SurfacePoint) {
    for state in [ButtonState::Pressed, ButtonState::Released] {
        host.input(
            view,
            pointer(PointerEvent::Button {
                button,
                state,
                position,
                modifiers: Modifiers::default(),
            }),
        );
    }
}

/// `[b, g, r, a]` of an opaque `0xRRGGBB`.
fn bgra(rgb: u32) -> [u8; 4] {
    let [_, r, g, b] = rgb.to_be_bytes();
    [b, g, r, 255]
}

#[test]
fn hello_sighurt_draws() {
    let mut host = Host::new();
    let url = example("hello-sighurt");
    let (view, shown) = host.open(800, 600, 120);
    let navigation = host.navigate(view, &url);
    let events = host.until(20 * SECS, is_frame_ready(view));
    assert_eq!(events[0], EngineEvent::ViewReady { view });
    assert_eq!(
        events[1],
        EngineEvent::NavigationStarted {
            view,
            navigation,
            url: url.clone()
        }
    );
    assert!(events
        .iter()
        .any(|e| committed_url(e) == Some(url.as_str())));
    assert!(events.contains(&EngineEvent::HistoryChanged {
        view,
        entries: vec![url.clone()],
        current: 0
    }));

    // The guest draws in on_frame: let it run a few frames.
    host.run_for(SECS / 5);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(5, 5), bgra(0x322850), "the header");
    assert_eq!(shown.pixel(50, 120), bgra(0xf06e6e), "the red rectangle");
    assert_eq!(shown.pixel(195, 120), bgra(0x78dc8c), "the green circle");
    assert_eq!(shown.pixel(700, 580), bgra(0x1e1e2e), "the background");
    // Nothing changed since: nothing to paint.
    assert_eq!(host.present(view), PresentOutcome::Unchanged);

    // At 200 % the same picture has twice the pixels, and the guest's canvas keeps its size.
    host.command(view, EngineCommand::SetMetrics(metrics(800, 600, 240)));
    host.until(5 * SECS, is_frame_ready(view));
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixels().0.width, 1600);
    assert_eq!(shown.pixel(100, 240), bgra(0xf06e6e));
    assert_eq!(shown.pixel(1400, 1160), bgra(0x1e1e2e));

    // Page zoom makes the canvas smaller and its pixels bigger.
    host.command(view, EngineCommand::SetZoom(2.0));
    host.run_for(SECS / 5);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(200, 480), bgra(0xf06e6e));
}

/// Sets a timeout, listens for a custom event it emits and for resizes, and records each in its
/// history, where the browser sees them as same-document navigations.
const TIMERS_AND_EVENTS: &str = r##"
(module
  (import "oxide" "api_set_timeout" (func $timeout (param i32 i32) (result i32)))
  (import "oxide" "api_on_event" (func $on (param i32 i32 i32) (result i32)))
  (import "oxide" "api_emit_event" (func $emit (param i32 i32 i32 i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "ping")
  (data (i32.const 16) "resize")
  (data (i32.const 32) "#timer")
  (data (i32.const 48) "#ping")
  (data (i32.const 64) "#resize")
  (func $mark (param $at i32) (param $len i32)
    (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (local.get $at) (local.get $len)))
  (func (export "start_app")
    (drop (call $timeout (i32.const 1) (i32.const 30)))
    (drop (call $on (i32.const 0) (i32.const 4) (i32.const 2)))
    (drop (call $on (i32.const 16) (i32.const 6) (i32.const 3)))
    (call $emit (i32.const 0) (i32.const 4) (i32.const 0) (i32.const 0)))
  (func (export "on_frame") (param i32))
  (func (export "on_timer") (param $cb i32)
    (if (i32.eq (local.get $cb) (i32.const 1)) (then (call $mark (i32.const 32) (i32.const 6)))))
  (func (export "on_event") (param $cb i32)
    (if (i32.eq (local.get $cb) (i32.const 2)) (then (call $mark (i32.const 48) (i32.const 5))))
    (if (i32.eq (local.get $cb) (i32.const 3)) (then (call $mark (i32.const 64) (i32.const 7))))))
"##;

#[test]
fn timers_and_events_reach_the_guest() {
    let mut host = Host::new();
    let url = guest(host.dir(), "app", TIMERS_AND_EVENTS);
    let (view, _) = load(&mut host, &url);
    let ends = |suffix: &'static str| {
        move |event: &EngineEvent| committed_url(event).is_some_and(|url| url.ends_with(suffix))
    };
    let events = host.until(5 * SECS, |event| {
        matches!(event, EngineEvent::HistoryChanged { entries, current: 2, .. } if entries.len() == 3)
    });
    // The event emitted in start_app came first, at the first frame; the timer 30 ms later.
    let order: Vec<_> = events
        .iter()
        .filter_map(committed_url)
        .filter_map(|url| url.rsplit_once('#').map(|(_, fragment)| fragment))
        .collect();
    assert_eq!(order, ["ping", "timer"]);

    host.command(view, EngineCommand::SetMetrics(metrics(640, 480, 120)));
    host.until(5 * SECS, ends("#resize"));
}

#[test]
fn raf_demo_animates() {
    let mut host = Host::new();
    let (view, shown) = load(&mut host, &example("raf-demo"));
    let mut frames = Vec::new();
    while frames.len() < 3 {
        host.until(5 * SECS, is_frame_ready(view));
        if host.present(view) == PresentOutcome::Submitted {
            frames.push(shown.pixels().1);
        }
    }
    assert_ne!(frames[0], frames[1]);
    assert_ne!(frames[1], frames[2]);
}

#[test]
fn a_hidden_view_steps_once_a_second() {
    let mut host = Host::new();
    let (view, _) = load(&mut host, &example("raf-demo"));
    let visible = host.run_for(SECS / 2);
    let frames = |events: &[EngineEvent]| events.iter().filter(|e| is_frame_ready(view)(e)).count();
    // Each pump coalesces FrameReady, so this counts the pumps that had a new frame.
    assert!(frames(&visible) >= 10, "{} frames", frames(&visible));
    host.command(view, EngineCommand::SetVisibility(false));
    let hidden = host.run_for(3 * SECS / 2);
    assert!(
        frames(&hidden) <= 3,
        "{} frames while hidden",
        frames(&hidden)
    );
}

const LOOPS_IN_ON_FRAME: &str = r#"
(module
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (loop $l (br $l))))
"#;

/// Calls the host in a loop: cheap in fuel, expensive in time.
const CALLS_THE_HOST_IN_A_LOOP: &str = r#"
(module
  (import "oxide" "api_time_now_ms" (func $now (result i64)))
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (loop $l (drop (call $now)) (br $l))))
"#;

#[test]
fn a_looping_guest_hits_the_fuel_limit_without_blocking_the_owner() {
    let mut host = Host::new();
    let (animated, _) = load(&mut host, &example("raf-demo"));
    let url = guest(host.dir(), "loop", LOOPS_IN_ON_FRAME);
    let (view, _) = load(&mut host, &url);
    let started = Instant::now();
    let events = host.until(10 * SECS, failed(view));
    let message = failure(&events);
    assert!(
        message.contains("on_frame halted: fuel limit exceeded"),
        "{message}"
    );
    eprintln!(
        "looping guest stopped after {:?}; longest pump {:?}",
        started.elapsed(),
        host.longest_pump
    );
    assert!(started.elapsed() < 5 * SECS, "{:?}", started.elapsed());
    // The owner loop never waited for the guest, and the other view kept animating.
    assert!(
        host.longest_pump < Duration::from_millis(50),
        "{:?}",
        host.longest_pump
    );
    let frames = events
        .iter()
        .filter(|e| is_frame_ready(animated)(e))
        .count();
    assert!(frames >= 1, "{events:#?}");
    // Nothing more comes from the failed guest: no more steps, no more frames.
    let later = host.run_for(SECS / 2);
    assert!(!later.iter().any(|e| e.view() == Some(view)), "{later:#?}");
}

#[test]
fn a_guest_calling_the_host_in_a_loop_hits_the_time_limit() {
    let mut host = Host::with_policy(SandboxPolicy {
        frame_fuel: u64::MAX,
        time_limit: Duration::from_millis(200),
        ..SandboxPolicy::default()
    });
    let url = guest(host.dir(), "calls", CALLS_THE_HOST_IN_A_LOOP);
    let (view, _) = load(&mut host, &url);
    let started = Instant::now();
    let events = host.until(10 * SECS, failed(view));
    let message = failure(&events);
    assert!(
        message.contains("on_frame halted: time limit exceeded"),
        "{message}"
    );
    eprintln!("host-calling guest stopped after {:?}", started.elapsed());
    assert!(started.elapsed() < 3 * SECS, "{:?}", started.elapsed());
    assert!(
        host.longest_pump < Duration::from_millis(50),
        "{:?}",
        host.longest_pump
    );
}

/// Sets every timer it may at once, each of which does a little work; draws a new color every
/// frame; records when the last timer fired.
const TIMER_FLOOD: &str = r##"
(module
  (import "oxide" "api_set_timeout" (func $timeout (param i32 i32) (result i32)))
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#all-fired")
  (global $fired (mut i32) (i32.const 0))
  (global $frames (mut i32) (i32.const 0))
  (func (export "start_app") (local $i i32)
    (loop $l
      (drop (call $timeout (i32.const 1) (i32.const 0)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (i32.const 10000)))))
  (func (export "on_timer") (param i32) (local $i i32)
    (loop $l
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (i32.const 20000))))
    (global.set $fired (i32.add (global.get $fired) (i32.const 1)))
    (if (i32.eq (global.get $fired) (i32.const 4096))
      (then (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 10)))))
  (func (export "on_frame") (param i32)
    (global.set $frames (i32.add (global.get $frames) (i32.const 1)))
    (call $clear (global.get $frames) (i32.const 0) (i32.const 0) (i32.const 255))))
"##;

#[test]
fn a_flood_of_timers_is_spread_over_steps() {
    let mut host = Host::new();
    let url = guest(host.dir(), "flood", TIMER_FLOOD);
    let (view, _) = load(&mut host, &url);
    let events = host.until(20 * SECS, |event| {
        committed_url(event).is_some_and(|url| url.ends_with("#all-fired"))
    });
    // 4096 timers (the most a guest may have), at most 1000 a step: several frames came
    // between them, and the owner never waited.
    let frames = events.iter().filter(|e| is_frame_ready(view)(e)).count();
    assert!(frames >= 3, "{frames} frames");
    assert!(
        host.longest_pump < Duration::from_millis(50),
        "{:?}",
        host.longest_pump
    );
}

/// Draws and asks for everything wrong: NaN and huge geometry, pointers outside its memory,
/// broken images, more draw commands, links and timers than a frame may have. Then it
/// records that it is still running.
const BAD_REQUESTS: &str = r##"
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
  (import "oxide" "api_canvas_rounded_rect" (func $rrect (param f32 f32 f32 f32 f32 i32 i32 i32 i32)))
  (import "oxide" "api_canvas_text" (func $text (param f32 f32 f32 i32 i32 i32 i32 i32 i32)))
  (import "oxide" "api_canvas_gradient"
    (func $gradient (param f32 f32 f32 f32 i32 f32 f32 f32 f32 i32 i32)))
  (import "oxide" "api_canvas_image" (func $image (param f32 f32 f32 f32 i32 i32)))
  (import "oxide" "api_canvas_clip" (func $clip (param f32 f32 f32 f32)))
  (import "oxide" "api_register_hyperlink" (func $link (param f32 f32 f32 f32 i32 i32) (result i32)))
  (import "oxide" "api_log" (func $log (param i32 i32)))
  (import "oxide" "api_kv_store_set" (func $kv (param i32 i32 i32 i32) (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#alive")
  (data (i32.const 16) "not an image")
  (data (i32.const 32) "x.wasm")
  (func (export "start_app") (local $i i32)
    (call $clear (i32.const 0) (i32.const 0) (i32.const 64) (i32.const 255))
    (call $rect (f32.const nan) (f32.const 0) (f32.const 10) (f32.const 10)
                (i32.const 255) (i32.const 0) (i32.const 0) (i32.const 255))
    (call $rect (f32.const inf) (f32.const -inf) (f32.const 3e38) (f32.const 3e38)
                (i32.const 255) (i32.const 0) (i32.const 0) (i32.const 255))
    (call $rrect (f32.const 0) (f32.const 0) (f32.const -5) (f32.const 10) (f32.const nan)
                 (i32.const 255) (i32.const 0) (i32.const 0) (i32.const 255))
    (call $text (f32.const 10) (f32.const 10) (f32.const 1e9) (i32.const 255) (i32.const 255)
                (i32.const 255) (i32.const 255) (i32.const 0) (i32.const 5))
    (call $text (f32.const 10) (f32.const 10) (f32.const 12) (i32.const 255) (i32.const 255)
                (i32.const 255) (i32.const 255) (i32.const 0xfffffff0) (i32.const 100))
    (call $gradient (f32.const 0) (f32.const 0) (f32.const 10) (f32.const 10) (i32.const 1)
                    (f32.const 0) (f32.const 0) (f32.const nan) (f32.const 1e30)
                    (i32.const 0xffffff00) (i32.const 0x7fffffff))
    (call $image (f32.const 0) (f32.const 0) (f32.const 10) (f32.const 10)
                 (i32.const 16) (i32.const 12))
    (call $log (i32.const -1) (i32.const -1))
    (drop (call $kv (i32.const -16) (i32.const 64) (i32.const 0) (i32.const 1)))
    (loop $l
      (call $clip (f32.const 1) (f32.const 1) (f32.const 700) (f32.const 500))
      (call $rect (f32.const 0) (f32.const 0) (f32.const 800) (f32.const 600)
                  (i32.const 0) (i32.const 128) (i32.const 0) (i32.const 255))
      (drop (call $link (f32.const 0) (f32.const 0) (f32.const 1) (f32.const 1)
                        (i32.const 32) (i32.const 6)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (i32.const 120000))))
    (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 6))))
"##;

#[test]
fn invalid_drawing_and_resource_requests_fail_safely() {
    let mut host = Host::new();
    let url = guest(host.dir(), "bad", BAD_REQUESTS);
    let (view, shown) = host.open(800, 600, 120);
    host.navigate(view, &url);
    let events = host.until(20 * SECS, |event| {
        committed_url(event).is_some_and(|url| url.ends_with("#alive"))
    });
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::ViewFailed { .. })),
        "{events:#?}"
    );
    // 240,000 commands were asked for; the frame kept 200,000, each clipped, all in budget.
    let started = Instant::now();
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    let painting = started.elapsed();
    assert!(painting < SECS, "painting took {painting:?}");
    assert_eq!(
        shown.pixel(0, 0),
        bgra(0x000040),
        "the clear, outside every clip"
    );
    assert_eq!(
        shown.pixel(400, 300),
        bgra(0x008000),
        "the green fills, inside the clips"
    );
    // Still alive: it presents again at another zoom.
    host.command(view, EngineCommand::SetZoom(1.5));
    host.run_for(SECS / 5);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
}

/// Spends a while in `start_app`, then paints the whole canvas red, or in `on_frame` for the
/// live variant. Each loop iteration costs 8 instructions of fuel, which stays within the
/// budgets of `start_app` (500M) and `on_frame` (50M).
fn slow_red(live: bool) -> String {
    let iterations = if live { 5_000_000 } else { 50_000_000 };
    let work = format!(
        r#"(local $i i32)
      (loop $l
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br_if $l (i32.lt_u (local.get $i) (i32.const {iterations}))))
      (call $clear (i32.const 255) (i32.const 0) (i32.const 0) (i32.const 255))"#
    );
    let (start, frame) = if live {
        ("", work.as_str())
    } else {
        (work.as_str(), "")
    };
    format!(
        r#"(module
          (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
          (func (export "start_app") {start})
          (func (export "on_frame") (param i32) {frame}))"#
    )
}

const GREEN: &str = r#"
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (func (export "start_app") (call $clear (i32.const 0) (i32.const 255) (i32.const 0) (i32.const 255))))
"#;

#[test]
fn a_superseded_load_is_discarded() {
    let mut host = Host::new();
    let slow = guest(host.dir(), "slow", &slow_red(false));
    let green = guest(host.dir(), "green", GREEN);
    let (view, shown) = host.open(800, 600, 120);
    let first = host.navigate(view, &slow);
    // Let the slow load get into start_app, then navigate elsewhere.
    host.run_for(Duration::from_millis(30));
    let second = host.navigate(view, &green);
    let mut events = host.until(10 * SECS, |event| {
        matches!(event, EngineEvent::NavigationCommitted { navigation, .. } if *navigation == second)
    });
    events.extend(host.run_for(SECS));
    let mentions_first = |event: &EngineEvent| match event {
        EngineEvent::NavigationCommitted { navigation, .. }
        | EngineEvent::NavigationFailed { navigation, .. } => *navigation == first,
        _ => false,
    };
    assert!(!events.iter().any(mentions_first), "{events:#?}");
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(10, 10), bgra(0x00ff00));
}

#[test]
fn a_step_that_finishes_after_navigation_is_discarded() {
    let mut host = Host::new();
    let slow = guest(host.dir(), "slow", &slow_red(true));
    let green = guest(host.dir(), "green", GREEN);
    let (view, shown) = load(&mut host, &slow);
    // The slow document's first step is running now; replace the document meanwhile.
    host.run_for(Duration::from_millis(20));
    let navigation = host.navigate(view, &green);
    host.until(10 * SECS, |event| {
        matches!(event, EngineEvent::NavigationCommitted { navigation: n, .. } if *n == navigation)
    });
    // The old document's step finishes later; it must not show its red frame.
    host.run_for(SECS);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(10, 10), bgra(0x00ff00));
}

#[test]
fn nothing_arrives_for_a_closed_view() {
    let mut host = Host::new();
    let slow = guest(host.dir(), "slow", &slow_red(true));
    // One view closed while its first step runs, one while it loads.
    let (stepping, _) = load(&mut host, &slow);
    let (loading, _) = host.open(800, 600, 120);
    host.navigate(loading, &slow);
    host.run_for(Duration::from_millis(20));
    for view in [stepping, loading] {
        host.runtime.close_view(view);
    }
    let events = host.run_for(3 * SECS / 2);
    let closed: Vec<_> = events
        .iter()
        .filter(|e| e.view() == Some(stepping) || e.view() == Some(loading))
        .collect();
    assert_eq!(
        closed,
        [
            &EngineEvent::ViewClosed { view: stepping },
            &EngineEvent::ViewClosed { view: loading }
        ]
    );
    // Commands for a closed view are refused.
    assert!(host.runtime.command(stepping, EngineCommand::Stop).is_err());
}

const TRAPS_IN_ON_FRAME: &str = r#"
(module (func (export "start_app")) (func (export "on_frame") (param i32) unreachable))
"#;

const TRAPS_IN_START_APP: &str = r#"
(module (func (export "start_app") unreachable))
"#;

#[test]
fn a_guest_trap_fails_its_view_not_the_runtime() {
    let mut host = Host::new();
    let (animated, _) = load(&mut host, &example("raf-demo"));
    let trapping = guest(host.dir(), "trap", TRAPS_IN_ON_FRAME);
    let (view, shown) = load(&mut host, &trapping);
    let events = host.until(5 * SECS, failed(view));
    assert!(failure(&events).contains("on_frame trapped"), "{events:#?}");
    // The other view goes on.
    host.until(5 * SECS, is_frame_ready(animated));

    // A trap in start_app fails the navigation; the view stays usable.
    let broken = guest(host.dir(), "broken", TRAPS_IN_START_APP);
    let navigation = host.navigate(view, &broken);
    let events = host.until(5 * SECS, |event| {
        matches!(event, EngineEvent::NavigationFailed { navigation: n, .. } if *n == navigation)
    });
    match events.last() {
        Some(EngineEvent::NavigationFailed {
            error: NavigationError::Load(message),
            ..
        }) => assert!(message.contains("start_app trapped"), "{message}"),
        other => panic!("{other:?}"),
    }
    let green = guest(host.dir(), "green", GREEN);
    let navigation = host.navigate(view, &green);
    host.until(5 * SECS, |event| {
        matches!(event, EngineEvent::NavigationCommitted { navigation: n, .. } if *n == navigation)
    });
    host.run_for(SECS / 10);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(10, 10), bgra(0x00ff00));
}

/// Records what is typed each frame in its URL's fragment, and Enter as "#enter".
const TYPING: &str = r##"
(module
  (import "oxide" "api_text_input" (func $text (param i32 i32) (result i32)))
  (import "oxide" "api_key_pressed" (func $pressed (param i32) (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#")
  (data (i32.const 100) "#enter")
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (local $n i32)
    (local.set $n (call $text (i32.const 1) (i32.const 64)))
    (if (local.get $n)
      (then (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                        (i32.const 0) (i32.add (local.get $n) (i32.const 1)))))
    (if (call $pressed (i32.const 36))
      (then (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                        (i32.const 100) (i32.const 6))))))
"##;

#[test]
fn typed_text_comes_from_the_key_events_text() {
    let mut host = Host::new();
    let url = guest(host.dir(), "typing", TYPING);
    let (view, _) = load(&mut host, &url);
    let none = Modifiers::default();
    let ctrl = Modifiers {
        control: true,
        ..none
    };
    let char = |s: &str| Key::Character(s.into());
    for key in [
        press(char("h"), 35, Some("h"), none),
        press(char(" "), 57, Some(" "), none),
        // A dead key types nothing; the key that completes it types "ä".
        press(Key::Dead(Some('¨')), 27, None, none),
        press(char("ä"), 30, Some("ä"), none),
        // Ctrl+C is a shortcut, not typing.
        press(char("c"), 46, None, ctrl),
        press(char("!"), 2, Some("!"), none),
    ] {
        host.input(view, key);
    }
    let mut typed = String::new();
    host.until(5 * SECS, |event| {
        if let Some(url) = committed_url(event) {
            let fragment = url.rsplit_once('#').map_or("", |(_, f)| f);
            typed += &percent_encoding::percent_decode_str(fragment).decode_utf8_lossy();
        }
        typed.chars().count() >= 4
    });
    assert_eq!(typed, "h ä!");
    host.input(view, press(Key::Named("Enter".into()), 28, None, none));
    host.until(5 * SECS, |event| {
        committed_url(event).is_some_and(|url| url.ends_with("#enter"))
    });
}

/// Registers a link over its top-left corner.
const LINK: &str = r#"
(module
  (import "oxide" "api_register_hyperlink" (func $link (param f32 f32 f32 f32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "other.wasm")
  (func (export "start_app")
    (drop (call $link (f32.const 10) (f32.const 10) (f32.const 100) (f32.const 20)
                      (i32.const 0) (i32.const 10)))))
"#;

#[test]
fn links_show_their_target_and_open_on_click() {
    let mut host = Host::new();
    let url = guest(host.dir(), "links", LINK);
    let target = url.replace("links.wasm", "other.wasm");
    let (view, _) = load(&mut host, &url);
    // At 200 % zoom the link covers logical (20, 20) to (220, 60).
    host.command(view, EngineCommand::SetZoom(2.0));
    let motion = |x, y| {
        pointer(PointerEvent::Motion {
            position: at(x, y),
            modifiers: Modifiers::default(),
        })
    };
    host.input(view, motion(100.0, 30.0));
    let events = host.until(SECS, |e| matches!(e, EngineEvent::CursorChanged { .. }));
    assert!(events.contains(&EngineEvent::StatusTextChanged {
        view,
        text: Some(target.clone())
    }));
    assert_eq!(
        events.last(),
        Some(&EngineEvent::CursorChanged {
            view,
            cursor: Cursor::Pointer
        })
    );

    let opened = |disposition| move |event: &EngineEvent| matches!(event, EngineEvent::OpenRequested { disposition: d, .. } if *d == disposition);
    click(&mut host, view, PointerButton::Left, at(100.0, 30.0));
    let events = host.until(SECS, opened(OpenDisposition::CurrentView));
    let Some(EngineEvent::OpenRequested { url, .. }) = events.last() else {
        unreachable!()
    };
    assert_eq!(url, &target);
    click(&mut host, view, PointerButton::Middle, at(100.0, 30.0));
    host.until(SECS, opened(OpenDisposition::NewView));

    host.input(view, motion(300.0, 300.0));
    let events = host.until(SECS, |e| matches!(e, EngineEvent::CursorChanged { .. }));
    assert!(events.contains(&EngineEvent::StatusTextChanged { view, text: None }));
}

/// Pushes two same-document entries in start_app.
const HISTORY: &str = r##"
(module
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#a")
  (data (i32.const 8) "#b")
  (func (export "start_app")
    (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 2))
    (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 8) (i32.const 2)))
  (func (export "on_frame") (param i32)))
"##;

#[test]
fn traversal_walks_same_document_history() {
    let mut host = Host::new();
    let url = guest(host.dir(), "history", HISTORY);
    let (view, _) = host.open(800, 600, 120);
    let navigation = host.navigate(view, &url);
    let events = host.until(5 * SECS, |e| {
        matches!(e, EngineEvent::HistoryChanged { .. })
    });
    let entries = vec![url.clone(), format!("{url}#a"), format!("{url}#b")];
    let Some(EngineEvent::NavigationCommitted {
        navigation: committed,
        url: last,
        document,
        ..
    }) = events.iter().rev().find(|e| committed_url(e).is_some())
    else {
        panic!("{events:#?}");
    };
    assert_eq!((*committed, last), (navigation, &entries[2]));
    let document = *document;
    assert_eq!(
        events.last(),
        Some(&EngineEvent::HistoryChanged {
            view,
            entries: entries.clone(),
            current: 2
        })
    );

    let traverse = |host: &mut Host, delta| {
        let navigation = NavigationId::next();
        host.command(view, EngineCommand::Traverse { navigation, delta });
        let events = host.until(SECS, |e| match e {
            EngineEvent::NavigationCommitted { navigation: n, .. }
            | EngineEvent::NavigationFailed { navigation: n, .. } => *n == navigation,
            _ => false,
        });
        events.last().cloned().unwrap()
    };
    match traverse(&mut host, -2) {
        EngineEvent::NavigationCommitted {
            url, document: d, ..
        } => assert_eq!((url, d), (entries[0].clone(), document), "same document"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        traverse(&mut host, -1),
        EngineEvent::NavigationFailed {
            error: NavigationError::Cancelled,
            ..
        }
    ));
    assert_eq!(
        committed_url(&traverse(&mut host, 1)),
        Some(entries[1].as_str())
    );
}

/// Asks for the camera every frame until it gets an answer, and records a refusal.
const CAMERA: &str = r##"
(module
  (import "oxide" "api_camera_open" (func $open (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#denied")
  (global $done (mut i32) (i32.const 0))
  (func (export "start_app"))
  (func (export "on_frame") (param i32)
    (if (i32.eqz (global.get $done)) (then
      (if (i32.eq (call $open) (i32.const -1)) (then
        (global.set $done (i32.const 1))
        (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 7))))))))
"##;

#[test]
fn the_permission_prompt_is_drawn_and_answered() {
    let mut host = Host::new();
    let url = guest(host.dir(), "camera", CAMERA);
    let (view, shown) = load(&mut host, &url);
    host.until(5 * SECS, is_frame_ready(view));
    host.run_for(SECS / 10);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    // The prompt's card, over the page's top-left corner.
    assert_eq!(shown.pixel(20, 60), bgra(0x18181b));
    // Escape blocks the request, and the guest's next try is refused. (Never allow it here,
    // which would open a real camera.)
    host.input(
        view,
        press(Key::Named("Escape".into()), 1, None, Modifiers::default()),
    );
    host.until(5 * SECS, |event| {
        committed_url(event).is_some_and(|url| url.ends_with("#denied"))
    });
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(20, 60), [0; 4], "the prompt is gone");
}

const KV_WRITER: &str = r##"
(module
  (import "oxide" "api_kv_store_set" (func $set (param i32 i32 i32 i32) (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "kone")
  (data (i32.const 16) "#stored")
  (func (export "start_app")
    (if (i32.eqz (call $set (i32.const 0) (i32.const 1) (i32.const 1) (i32.const 3)))
      (then (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 16) (i32.const 7)))))
  (func (export "on_frame") (param i32)))
"##;

const KV_READER: &str = r##"
(module
  (import "oxide" "api_kv_store_get" (func $get (param i32 i32 i32 i32) (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "k")
  (data (i32.const 16) "#missing")
  (data (i32.const 31) "#")
  (func (export "start_app") (local $n i32)
    (local.set $n (call $get (i32.const 0) (i32.const 1) (i32.const 32) (i32.const 16)))
    (if (i32.gt_s (local.get $n) (i32.const 0))
      (then (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                        (i32.const 31) (i32.add (local.get $n) (i32.const 1))))
      (else (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                        (i32.const 16) (i32.const 8))))))
"##;

/// Views of one origin share its persistent store, while both run.
#[test]
fn views_share_the_persistent_store() {
    let mut host = Host::new();
    let writer = guest(host.dir(), "writer", KV_WRITER);
    let reader = guest(host.dir(), "reader", KV_READER);
    let (_writer, _) = host.open(800, 600, 120);
    host.navigate(_writer, &writer);
    host.until(5 * SECS, |e| {
        committed_url(e).is_some_and(|u| u.ends_with("#stored"))
    });
    let (_reader, _) = host.open(800, 600, 120);
    host.navigate(_reader, &reader);
    host.until(5 * SECS, |e| {
        committed_url(e).is_some_and(|u| u.ends_with("#one"))
    });
}

#[test]
fn shutdown_closes_everything_and_refuses_more() {
    let mut host = Host::new();
    let (view, _) = load(&mut host, &example("raf-demo"));
    host.runtime.shutdown();
    let mut events = Vec::new();
    assert_eq!(
        host.runtime.pump(&mut events),
        sighurt_api::engine::PumpStatus::Idle
    );
    assert!(events.is_empty());
    assert!(host.runtime.command(view, EngineCommand::Stop).is_err());
    host.runtime.shutdown();
}

/// Spends 40 ms in every frame, calling the host in a loop.
const BUSY: &str = r#"
(module
  (import "oxide" "api_time_now_ms" (func $now (result i64)))
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (local $end i64)
    (local.set $end (i64.add (call $now) (i64.const 40)))
    (loop $l (br_if $l (i64.lt_u (call $now) (local.get $end))))))
"#;

/// Six guests that each take 40 ms a frame keep every worker thread busy all the time; a key
/// press still reaches another guest in its turn, and the owner loop never waits.
#[test]
fn input_reaches_a_guest_while_others_are_busy() {
    let mut host = Host::new();
    let busy = guest(host.dir(), "busy", BUSY);
    for _ in 0..6 {
        load(&mut host, &busy);
    }
    let typing = guest(host.dir(), "typing", TYPING);
    let (view, _) = load(&mut host, &typing);
    host.run_for(SECS / 2);
    host.longest_pump = Duration::ZERO;
    let started = Instant::now();
    host.input(
        view,
        press(
            Key::Character("x".into()),
            45,
            Some("x"),
            Modifiers::default(),
        ),
    );
    host.until(10 * SECS, |event| {
        committed_url(event).is_some_and(|url| url.ends_with("#x"))
    });
    let latency = started.elapsed();
    eprintln!(
        "key to guest with 6 busy guests: {latency:?}; longest pump {:?}",
        host.longest_pump
    );
    assert!(latency < 2 * SECS, "{latency:?}");
    assert!(
        host.longest_pump < Duration::from_millis(50),
        "{:?}",
        host.longest_pump
    );
}

/// A guest whose every frame fetches `URL`, a server that never answers: each `api_fetch`
/// waits for its 30 s timeout.
fn hanging_fetch(url: &str) -> String {
    format!(
        r##"
(module
  (import "oxide" "api_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i64)))
  (memory (export "memory") 1)
  (data (i32.const 0) "GET")
  (data (i32.const 16) "{url}")
  (func (export "start_app"))
  (func (export "on_frame") (param i32)
    (drop (call $fetch (i32.const 0) (i32.const 3) (i32.const 16) (i32.const {len})
                       (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                       (i32.const 1024) (i32.const 16)))))
"##,
        len = url.len()
    )
}

/// Six guests waiting on a server that never answers would hold every guest thread for the
/// fetch's 30 s timeout; while they wait, spare threads run the others, so a key press still
/// reaches another guest at once.
#[test]
fn guests_waiting_on_the_network_do_not_hold_up_the_others() {
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/", server.local_addr().unwrap());
    // Accepts every connection and answers none, until the test ends.
    std::thread::spawn(move || {
        let held: Vec<_> = server.incoming().collect();
        drop(held);
    });
    let mut host = Host::new();
    let hanging = guest(host.dir(), "hanging", &hanging_fetch(&url));
    for _ in 0..6 {
        load(&mut host, &hanging);
    }
    let typing = guest(host.dir(), "typing", TYPING);
    let (view, _) = load(&mut host, &typing);
    host.run_for(SECS / 2);
    let started = Instant::now();
    host.input(
        view,
        press(
            Key::Character("x".into()),
            45,
            Some("x"),
            Modifiers::default(),
        ),
    );
    host.until(10 * SECS, |event| {
        committed_url(event).is_some_and(|url| url.ends_with("#x"))
    });
    let latency = started.elapsed();
    eprintln!("key to guest with 6 guests waiting on the network: {latency:?}");
    assert!(latency < 2 * SECS, "{latency:?}");
}

/// Keeps calling `api_fetch` (a refused local port, so each call returns at once) until 2 s
/// have passed, then records that it got that far.
const FETCH_LOOP: &str = r##"
(module
  (import "oxide" "api_fetch" (func $fetch (param i32 i32 i32 i32 i32 i32 i32 i32 i32 i32) (result i64)))
  (import "oxide" "api_time_now_ms" (func $now (result i64)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "GET")
  (data (i32.const 16) "http://127.0.0.1:1/")
  (data (i32.const 48) "#survived")
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (local $end i64)
    (local.set $end (i64.add (call $now) (i64.const 2000)))
    (loop $l
      (drop (call $fetch (i32.const 0) (i32.const 3) (i32.const 16) (i32.const 19)
                         (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                         (i32.const 64) (i32.const 16)))
      (br_if $l (i64.lt_u (call $now) (local.get $end))))
    (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 48) (i32.const 9))))
"##;

/// Waiting in host functions isn't charged to the time limit, but there is a limit to it.
#[test]
fn a_guest_that_keeps_the_host_waiting_is_stopped() {
    let mut host = Host::with_policy(SandboxPolicy {
        time_limit: Duration::from_millis(200),
        max_wait: Duration::from_millis(300),
        ..SandboxPolicy::default()
    });
    let url = guest(host.dir(), "fetchloop", FETCH_LOOP);
    let (view, _) = load(&mut host, &url);
    let started = Instant::now();
    let events = host.until(10 * SECS, failed(view));
    let message = failure(&events);
    eprintln!(
        "fetching guest stopped after {:?}: {message}",
        started.elapsed()
    );
    assert!(
        message.contains("on_frame halted: host calls waited too long"),
        "{message}"
    );
    assert!(started.elapsed() < 3 * SECS, "{:?}", started.elapsed());
    let survived = |e: &EngineEvent| committed_url(e).is_some_and(|u| u.ends_with("#survived"));
    assert!(!events.iter().any(survived));
    assert!(host.longest_pump < Duration::from_millis(50));
}

/// Loops in the module's own `start` function, which runs as it is instantiated.
const LOOPS_IN_START_SECTION: &str = r#"
(module
  (import "oxide" "api_time_now_ms" (func $now (result i64)))
  (func $spin (loop $l (drop (call $now)) (br $l)))
  (start $spin)
  (func (export "start_app")))
"#;

const LOOPS_IN_START_APP: &str = r#"(module (func (export "start_app") (loop $l (br $l))))"#;

fn load_failure(events: &[EngineEvent]) -> &str {
    match events.last() {
        Some(EngineEvent::NavigationFailed {
            error: NavigationError::Load(message),
            ..
        }) => message,
        other => panic!("expected a failed load, got {other:?}"),
    }
}

#[test]
fn a_start_function_has_the_budget_of_start_app() {
    let mut host = Host::with_policy(SandboxPolicy {
        fuel_limit: u64::MAX,
        start_time_limit: Duration::from_millis(200),
        ..SandboxPolicy::default()
    });
    let url = guest(host.dir(), "section", LOOPS_IN_START_SECTION);
    let (view, _) = host.open(800, 600, 120);
    let started = Instant::now();
    let navigation = host.navigate(view, &url);
    let events = host.until(5 * SECS, |event| {
        matches!(event, EngineEvent::NavigationFailed { navigation: n, .. } if *n == navigation)
    });
    let message = load_failure(&events);
    assert!(
        message.contains("the module's start function halted: time limit exceeded"),
        "{message}"
    );
    assert!(started.elapsed() < 3 * SECS, "{:?}", started.elapsed());
}

/// Both loader threads run guests that would loop for a minute; stopping one load and closing
/// the other view frees them at once for the next load.
#[test]
fn stopping_or_closing_a_load_stops_its_guest() {
    let mut host = Host::with_policy(SandboxPolicy {
        fuel_limit: u64::MAX,
        start_time_limit: Duration::from_secs(60),
        ..SandboxPolicy::default()
    });
    let section = guest(host.dir(), "section", LOOPS_IN_START_SECTION);
    let start_app = guest(host.dir(), "start", LOOPS_IN_START_APP);
    let (stopped, _) = host.open(800, 600, 120);
    let stopped_load = host.navigate(stopped, &section);
    let (closed, _) = host.open(800, 600, 120);
    host.navigate(closed, &start_app);
    host.run_for(SECS / 3);
    host.command(stopped, EngineCommand::Stop);
    host.runtime.close_view(closed);
    let events = host.run_for(Duration::from_millis(50));
    assert!(events.contains(&EngineEvent::NavigationFailed {
        view: stopped,
        navigation: stopped_load,
        error: NavigationError::Cancelled
    }));

    let green = guest(host.dir(), "green", GREEN);
    let started = Instant::now();
    let (view, shown) = load(&mut host, &green);
    eprintln!("next load committed after {:?}", started.elapsed());
    assert!(started.elapsed() < 3 * SECS, "{:?}", started.elapsed());
    host.run_for(SECS / 10);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(10, 10), bgra(0x00ff00));
    // Nothing more of the stopped load or the closed view.
    let later = host.run_for(SECS / 2);
    assert!(
        !later
            .iter()
            .any(|e| e.view() == Some(stopped) || e.view() == Some(closed)),
        "{later:#?}"
    );
}

/// Paints over its whole canvas with more translucent rectangles than a frame's budget covers,
/// and asks for the location every frame; records a grant.
const HIDES_THE_PROMPT: &str = r##"
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
  (import "oxide" "api_get_location" (func $location (param i32 i32) (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#granted")
  (global $done (mut i32) (i32.const 0))
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (local $i i32)
    (call $clear (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 255))
    (loop $l
      (call $rect (f32.const 0) (f32.const 0) (f32.const 800) (f32.const 600)
                  (i32.const 255) (i32.const 255) (i32.const 255) (i32.const 1))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (i32.const 100))))
    (if (i32.eqz (global.get $done)) (then
      (if (i32.gt_s (call $location (i32.const 100) (i32.const 64)) (i32.const 0)) (then
        (global.set $done (i32.const 1))
        (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 8))))))))
"##;

#[test]
fn the_permission_prompt_cannot_be_hidden_or_answered_unseen() {
    let mut host = Host::new();
    let url = guest(host.dir(), "hide", HIDES_THE_PROMPT);
    let (view, shown) = load(&mut host, &url);
    let granted = |e: &EngineEvent| committed_url(e).is_some_and(|u| u.ends_with("#granted"));
    let enter = || press(Key::Named("Enter".into()), 28, None, Modifiers::default());
    // The guest has asked, but the prompt was never painted: Enter is not an answer.
    host.run_for(SECS / 3);
    host.input(view, enter());
    assert!(!host.run_for(SECS / 3).iter().any(granted));
    // However much the page paints, the prompt is painted over it.
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    assert_eq!(shown.pixel(20, 60), bgra(0x18181b), "the prompt's card");
    host.input(view, enter());
    host.until(5 * SECS, granted);
}

/// Measures a line of `len` bytes every frame.
fn measures(len: u32) -> String {
    format!(
        r##"
(module
  (import "oxide" "api_canvas_measure_text"
    (func $measure (param f32 i32 i32 i32 i32 i32 i32 i32) (result i32)))
  (memory (export "memory") {pages})
  (func (export "start_app")
    (memory.fill (i32.const 64) (i32.const 97) (i32.const {len})))
  (func (export "on_frame") (param i32)
    (drop (call $measure (f32.const 12) (i32.const 0) (i32.const 0) (i32.const 400) (i32.const 0)
                         (i32.const 64) (i32.const {len}) (i32.const 0)))))
"##,
        pages = len / 65536 + 2
    )
}

const HELLO: &str = r##"
(module
  (import "oxide" "api_canvas_text" (func $text (param f32 f32 f32 i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "Hello")
  (func (export "start_app")
    (call $text (f32.const 10) (f32.const 10) (f32.const 20) (i32.const 255) (i32.const 255)
                (i32.const 255) (i32.const 255) (i32.const 0) (i32.const 5))))
"##;

/// Another guest measuring megabytes of text doesn't slow painting down.
#[test]
fn measuring_text_does_not_hold_up_painting() {
    let mut host = Host::new();
    let hello = guest(host.dir(), "hello", HELLO);
    let (painter, _) = load(&mut host, &hello);
    let present = |host: &mut Host| {
        host.command(painter, EngineCommand::SetMetrics(metrics(800, 600, 120)));
        let started = Instant::now();
        assert_eq!(host.present(painter), PresentOutcome::Submitted);
        started.elapsed()
    };
    let quiet = (0..3).map(|_| present(&mut host)).max().unwrap();
    let busy = guest(host.dir(), "measure", &measures(8 * 1024 * 1024));
    load(&mut host, &busy);
    host.run_for(SECS / 2);
    let mut worst = Duration::ZERO;
    for _ in 0..5 {
        worst = worst.max(present(&mut host));
        host.run_for(SECS / 10);
    }
    eprintln!("a one-word page presents in {quiet:?}, and in {worst:?} while another guest measures 8 MiB a frame");
    assert!(worst < Duration::from_millis(500), "{worst:?}");
}

/// Pushes 64 entries with URLs of 1 MiB, then replaces the current one every frame.
fn big_history() -> String {
    let len = 1024 * 1024;
    format!(
        r##"
(module
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (import "oxide" "api_replace_state" (func $replace (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 20)
  (global $frame (mut i32) (i32.const 0))
  (func (export "start_app") (local $i i32)
    (i32.store8 (i32.const 64) (i32.const 35))
    (memory.fill (i32.const 65) (i32.const 97) (i32.const {len}))
    (loop $l
      (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 64) (i32.const {len}))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $l (i32.lt_u (local.get $i) (i32.const 64)))))
  (func (export "on_frame") (param i32)
    (global.set $frame (i32.add (global.get $frame) (i32.const 1)))
    (i32.store8 (i32.const 65) (i32.add (i32.const 97) (i32.rem_u (global.get $frame) (i32.const 26))))
    (call $replace (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 64) (i32.const {len}))))
"##
    )
}

/// What a guest can make the host keep of its history, and copy for the browser, is bounded.
#[test]
fn history_is_bounded() {
    let mut host = Host::new();
    let url = guest(host.dir(), "history", &big_history());
    let (view, _) = host.open(800, 600, 120);
    host.navigate(view, &url);
    let (mut reports, mut most) = (0, 0);
    let started = Instant::now();
    let mut pumping = Duration::ZERO;
    while started.elapsed() < 2 * SECS {
        let before = Instant::now();
        let status = host.pump();
        pumping += before.elapsed();
        for event in host.take() {
            if let EngineEvent::HistoryChanged { entries, .. } = event {
                reports += 1;
                most = most.max(entries.iter().map(String::len).sum::<usize>());
            }
        }
        if !matches!(
            status,
            sighurt_api::engine::PumpStatus::Pending { deadline: None }
        ) {
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    eprintln!(
        "{reports} history reports of at most {most} bytes; longest pump {:?}, {pumping:?} of 2 s pumping",
        host.longest_pump
    );
    assert!(reports > 10, "{reports} reports");
    assert!(
        most <= sighurt_engine_wasm::navigation::MAX_BYTES,
        "{most} bytes"
    );
    assert!(
        host.longest_pump < Duration::from_millis(50),
        "{:?}",
        host.longest_pump
    );
}

const RED: &str = r#"
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (func (export "start_app") (call $clear (i32.const 255) (i32.const 0) (i32.const 0) (i32.const 255))))
"#;

/// Back and forward move between documents of one view, loading each again.
#[test]
fn back_and_forward_cross_documents() {
    let mut host = Host::new();
    let green = guest(host.dir(), "green", GREEN);
    let red = guest(host.dir(), "red", RED);
    let other = guest(host.dir(), "other", GREEN);
    let (view, shown) = load(&mut host, &green);
    host.run_for(SECS / 10);
    let history = |host: &mut Host| {
        let events = host.until(5 * SECS, |e| {
            matches!(e, EngineEvent::HistoryChanged { .. })
        });
        let Some(EngineEvent::HistoryChanged {
            entries, current, ..
        }) = events.last().cloned()
        else {
            unreachable!()
        };
        let document = events.iter().rev().find_map(|e| match e {
            EngineEvent::NavigationCommitted { document, .. } => Some(*document),
            _ => None,
        });
        (entries, current, document)
    };
    let traverse = |host: &mut Host, delta| {
        let navigation = NavigationId::next();
        host.command(view, EngineCommand::Traverse { navigation, delta });
        navigation
    };
    let color = |host: &mut Host| {
        host.run_for(SECS / 20);
        assert_eq!(host.present(view), PresentOutcome::Submitted);
        shown.pixel(10, 10)
    };
    host.navigate(view, &red);
    let (entries, current, first) = history(&mut host);
    assert_eq!((entries, current), (vec![green.clone(), red.clone()], 1));

    traverse(&mut host, -1);
    let (entries, current, back) = history(&mut host);
    assert_eq!((entries, current), (vec![green.clone(), red.clone()], 0));
    assert_ne!(back, first, "a new document");
    assert_eq!(color(&mut host), bgra(0x00ff00));

    traverse(&mut host, 1);
    let (entries, current, _) = history(&mut host);
    assert_eq!((entries, current), (vec![green.clone(), red.clone()], 1));
    assert_eq!(color(&mut host), bgra(0xff0000));

    // A new navigation from the first entry drops the one after it.
    traverse(&mut host, -1);
    history(&mut host);
    host.navigate(view, &other);
    let (entries, current, _) = history(&mut host);
    assert_eq!((entries, current), (vec![green, other], 1));
    let navigation = traverse(&mut host, 1);
    host.until(5 * SECS, |e| {
        *e == EngineEvent::NavigationFailed {
            view,
            navigation,
            error: NavigationError::Cancelled,
        }
    });
}

/// Pushes another origin's URL, then one of its own.
const PUSHES_ANOTHER_ORIGIN: &str = r##"
(module
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "https://bank.example/login")
  (data (i32.const 32) "#ok")
  (func (export "start_app")
    (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 26))
    (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 32) (i32.const 3))))
"##;

#[test]
fn a_page_cannot_show_another_origins_url() {
    let mut host = Host::new();
    let url = guest(host.dir(), "spoof", PUSHES_ANOTHER_ORIGIN);
    let (view, _) = host.open(800, 600, 120);
    host.navigate(view, &url);
    let events = host.until(5 * SECS, |e| {
        matches!(e, EngineEvent::HistoryChanged { .. })
    });
    assert_eq!(
        events.last(),
        Some(&EngineEvent::HistoryChanged {
            view,
            entries: vec![url.clone(), format!("{url}#ok")],
            current: 1
        })
    );
    assert!(!format!("{events:?}").contains("bank.example"));
}

/// Spawns workers that loop in `start_app` until it is refused, then terminates them all and
/// spawns one more each frame until that works again.
fn spawns_workers(worker: &str) -> String {
    format!(
        r##"
(module
  (import "oxide" "api_spawn_worker" (func $spawn (param i32 i32) (result i32)))
  (import "oxide" "api_worker_terminate" (func $terminate (param i32) (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#sixteen")
  (data (i32.const 16) "#again")
  (data (i32.const 64) "{worker}")
  (global $again (mut i32) (i32.const 0))
  (func $spawn_one (result i32) (call $spawn (i32.const 64) (i32.const {len})))
  (func (export "start_app") (local $n i32) (local $h i32)
    (block $full
      (loop $l
        (br_if $full (i32.le_s (call $spawn_one) (i32.const 0)))
        (local.set $n (i32.add (local.get $n) (i32.const 1)))
        (br_if $l (i32.lt_u (local.get $n) (i32.const 100)))))
    (if (i32.eq (local.get $n) (i32.const 16)) (then
      (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 8))))
    (local.set $h (i32.const 1))
    (loop $t
      (drop (call $terminate (local.get $h)))
      (local.set $h (i32.add (local.get $h) (i32.const 1)))
      (br_if $t (i32.le_u (local.get $h) (i32.const 16)))))
  (func (export "on_frame") (param i32)
    (if (i32.eqz (global.get $again)) (then
      (if (i32.gt_s (call $spawn_one) (i32.const 0)) (then
        (global.set $again (i32.const 1))
        (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 16) (i32.const 6))))))))
"##,
        len = worker.len()
    )
}

/// A guest has at most 16 worker threads, terminated ones still running included, and
/// terminating a worker stops the guest code it runs.
#[test]
fn workers_are_bounded_and_terminating_stops_them() {
    let mut host = Host::with_policy(SandboxPolicy {
        fuel_limit: u64::MAX,
        start_time_limit: Duration::from_secs(60),
        ..SandboxPolicy::default()
    });
    let worker = guest(host.dir(), "spin", LOOPS_IN_START_APP);
    let url = guest(host.dir(), "workers", &spawns_workers(&worker));
    let (view, _) = host.open(800, 600, 120);
    host.navigate(view, &url);
    let ends = |suffix: &'static str| {
        move |event: &EngineEvent| committed_url(event).is_some_and(|url| url.ends_with(suffix))
    };
    host.until(10 * SECS, ends("#sixteen"));
    let started = Instant::now();
    host.until(10 * SECS, ends("#again"));
    eprintln!(
        "a worker could be spawned again {:?} after 16 were terminated",
        started.elapsed()
    );
    assert!(started.elapsed() < 3 * SECS, "{:?}", started.elapsed());
}

/// Draws the same frame every frame.
const SAME_FRAME: &str = r#"
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (import "oxide" "api_canvas_rect" (func $rect (param f32 f32 f32 f32 i32 i32 i32 i32)))
  (func (export "start_app"))
  (func (export "on_frame") (param i32)
    (call $clear (i32.const 30) (i32.const 30) (i32.const 46) (i32.const 255))
    (call $rect (f32.const 10) (f32.const 10) (f32.const 100) (f32.const 50)
                (i32.const 240) (i32.const 110) (i32.const 110) (i32.const 255))))
"#;

/// A page that shows the same frame every frame doesn't wake the owner loop, though its guest
/// runs `on_frame` sixty times a second.
#[test]
fn a_static_page_leaves_the_owner_asleep() {
    use std::sync::atomic::Ordering;
    let mut host = Host::new();
    let url = guest(host.dir(), "same", SAME_FRAME);
    let (view, _) = load(&mut host, &url);
    host.run_for(SECS / 2);
    assert_eq!(host.present(view), PresentOutcome::Submitted);
    host.wakes.store(0, Ordering::Relaxed);
    let events = host.run_for(SECS);
    let wakes = host.wakes.load(Ordering::Relaxed);
    eprintln!("a static page woke the owner {wakes} times in 1 s");
    assert_eq!(wakes, 0);
    assert!(events.is_empty(), "{events:#?}");
}
