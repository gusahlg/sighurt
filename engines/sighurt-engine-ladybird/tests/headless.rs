//! Ladybird in a test process with no display server: pages load from local files, the events come
//! in the contract's order, frames land in an in-memory target, resizing and history work, and an
//! idle page leaves the owner loop alone.
//!
//! Needs the shim (`nix/build-ladybird-shim.sh`) and Ladybird's helper processes; without the shim
//! the test says so and passes. Ladybird is one per process, so everything is one test.
//!
//!   tools/heavy cargo test -p sighurt-engine-ladybird --test headless -- --nocapture

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{BufferSize, Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Modifiers, PointerButton, PointerEvent,
};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_engine_ladybird::{ffi, LadybirdConfig, LadybirdRuntime};

struct Permit {
    epoch: LayoutEpoch,
}

#[derive(Default)]
struct Shot {
    size: Option<BufferSize>,
    pixels: Vec<u8>,
    frames: u32,
}

struct Mem {
    metrics: Rc<RefCell<Metrics>>,
    shot: Rc<RefCell<Shot>>,
}

impl CpuTarget for Mem {
    type Permit = Permit;

    fn metrics(&self) -> Metrics {
        *self.metrics.borrow()
    }

    fn permit_epoch(permit: &Permit) -> LayoutEpoch {
        permit.epoch
    }

    fn submit(
        &mut self,
        permit: Permit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let metrics = *self.metrics.borrow();
        assert_eq!(metrics.epoch(), permit.epoch);
        let size = metrics.buffer();
        let stride = 4 * size.width as usize;
        let mut pixels = vec![0u8; stride * size.height as usize];
        paint(Frame {
            pixels: &mut pixels,
            size,
            stride,
        })?;
        let mut shot = self.shot.borrow_mut();
        shot.size = Some(size);
        shot.pixels = pixels;
        shot.frames += 1;
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

/// Pumps until `done` says so on the events so far, or `timeout` passes. Sleeps on the waker, as
/// the owner loop does, plus the pump's own deadline.
struct Driver {
    runtime: LadybirdRuntime<Mem>,
    wakes: mpsc::Receiver<()>,
    waker: EngineWaker,
    seen: Vec<EngineEvent>,
    deadline: Option<Instant>,
    pumps: u64,
}

impl Driver {
    fn pump(&mut self) -> bool {
        // As the owner loop does, right before pumping: a wake during the pump notifies again.
        self.waker.take();
        let mut events = Vec::new();
        self.pumps += 1;
        let status = self.runtime.pump(&mut events);
        self.deadline = match status {
            PumpStatus::Pending { deadline } => Some(deadline.unwrap_or_else(Instant::now)),
            PumpStatus::Idle => None,
        };
        let any = !events.is_empty();
        self.seen.extend(events);
        any
    }

    fn until(&mut self, timeout: Duration, mut done: impl FnMut(&[EngineEvent]) -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
            self.pump();
            if done(&self.seen) {
                return true;
            }
            let now = Instant::now();
            if now >= end {
                return false;
            }
            let wait = self.deadline.map_or(end - now, |d| {
                d.saturating_duration_since(now).min(end - now)
            });
            let _ = self.wakes.recv_timeout(wait);
        }
    }

    /// Sleeps (as the owner loop would) for `duration`, pumping only when woken or due. Returns the
    /// number of pumps it took.
    fn idle(&mut self, duration: Duration) -> u64 {
        let before = self.pumps;
        let end = Instant::now() + duration;
        loop {
            let now = Instant::now();
            if now >= end {
                return self.pumps - before;
            }
            let wait = self.deadline.map_or(end - now, |d| {
                d.saturating_duration_since(now).min(end - now)
            });
            if self.wakes.recv_timeout(wait).is_ok()
                || self.deadline.is_some_and(|d| d <= Instant::now())
            {
                self.pump();
            }
        }
    }

    fn count(&self, matches: impl Fn(&EngineEvent) -> bool) -> usize {
        self.seen.iter().filter(|e| matches(e)).count()
    }
}

fn page(dir: &Path, name: &str, body: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    format!("file://{}", path.display())
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

fn pixel(shot: &Shot, x: usize, y: usize) -> [u8; 4] {
    let size = shot.size.expect("a frame");
    let i = 4 * (y * size.width as usize + x);
    [
        shot.pixels[i],
        shot.pixels[i + 1],
        shot.pixels[i + 2],
        shot.pixels[i + 3],
    ]
}

#[test]
fn pages_load_render_navigate_and_idle() {
    if ffi::load(None).is_err() {
        eprintln!("skipped: no Ladybird shim (nix/build-ladybird-shim.sh)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sighurt-ladybird-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let second = page(
        &dir,
        "two.html",
        "<!doctype html><title>Second</title><body style='margin:0;background:#0000ff'>two",
    );
    let first = page(
        &dir,
        "one.html",
        &format!(
            "<!doctype html><title>First</title><body style='margin:0;background:#ff0000'>\
             <a id=a href='#frag' style='display:block;height:100px'>fragment</a>\
             <a id=b href='{second}' style='display:block;height:100px'>second</a><div id=frag>x</div>"
        ),
    );

    // A child of this process that is not Ladybird's. It exits at once and stays a zombie until
    // we wait for it: Ladybird's monitor, which reaps with waitpid(-1), would take its status.
    let mut bystander = std::process::Command::new("sleep")
        .arg("0.2")
        .spawn()
        .unwrap();

    let (wake_tx, wakes) = mpsc::channel();
    let waker = EngineWaker::new(move || {
        let _ = wake_tx.send(());
    });
    let runtime = LadybirdRuntime::<Mem>::new(LadybirdConfig::default(), waker.clone())
        .unwrap_or_else(|e| panic!("LadybirdRuntime::new: {e}"));
    let mut d = Driver {
        runtime,
        wakes,
        waker,
        seen: Vec::new(),
        deadline: None,
        pumps: 0,
    };

    let view = ViewId::next();
    let metrics_cell = Rc::new(RefCell::new(metrics(640, 480)));
    let shot = Rc::new(RefCell::new(Shot::default()));
    d.runtime
        .open_view(
            view,
            ViewOptions::default(),
            Mem {
                metrics: Rc::clone(&metrics_cell),
                shot: Rc::clone(&shot),
            },
        )
        .unwrap();
    // A navigation straight after open: Ladybird drops one made before its blank page loaded, so
    // the shim holds it.
    let first_navigation = NavigationId::next();
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: first_navigation,
                url: first.clone(),
            },
        )
        .unwrap();

    // 1. Events, in the contract's order.
    assert!(
        d.until(Duration::from_secs(30), |e| {
            e.iter().any(|e| matches!(e, EngineEvent::FrameReady { .. }))
                && e.iter().any(|e| matches!(e, EngineEvent::TitleChanged { title: Some(t), .. } if t == "First"))
                && e.iter().any(|e| matches!(e, EngineEvent::LoadingChanged { loading: false, .. }))
        }),
        "no page: {:#?}",
        d.seen
    );
    let position = |pred: &dyn Fn(&EngineEvent) -> bool| d.seen.iter().position(pred);
    let ready = position(&|e| matches!(e, EngineEvent::ViewReady { .. })).expect("ViewReady");
    let started = position(&|e| matches!(e, EngineEvent::NavigationStarted { navigation, .. } if *navigation == first_navigation)).expect("NavigationStarted");
    let committed = position(&|e| matches!(e, EngineEvent::NavigationCommitted { navigation, url, .. } if *navigation == first_navigation && *url == first)).expect("NavigationCommitted");
    let finished = position(&|e| matches!(e, EngineEvent::LoadingChanged { loading: false, .. }))
        .expect("loading stopped");
    assert!(
        ready < started && started < committed && committed < finished,
        "{:#?}",
        d.seen
    );
    assert_eq!(
        d.count(|e| matches!(e, EngineEvent::NavigationCommitted { .. })),
        1
    );

    // 2. A frame, with the page's colour in it. (The first frames can be of the blank page.)
    let mut red = false;
    for _ in 0..50 {
        d.pump();
        let permit = Permit {
            epoch: metrics_cell.borrow().epoch(),
        };
        if d.runtime.present(view, permit).unwrap() == PresentOutcome::Submitted
            && pixel(&shot.borrow(), 600, 400) == [0, 0, 255, 255]
        {
            red = true;
            break;
        }
        let _ = d.wakes.recv_timeout(Duration::from_millis(100));
    }
    assert!(red, "the page's background");
    assert_eq!(
        shot.borrow().size,
        Some(BufferSize {
            width: 640,
            height: 480
        })
    );
    // Nothing new: nothing submitted.
    let again = d
        .runtime
        .present(
            view,
            Permit {
                epoch: metrics_cell.borrow().epoch(),
            },
        )
        .unwrap();
    assert_eq!(again, PresentOutcome::Unchanged);

    // 3. The page's own fragment navigation stays in the document and adds a history entry.
    let documents_before = d
        .seen
        .iter()
        .rev()
        .find_map(|e| match e {
            EngineEvent::NavigationCommitted { document, .. } => Some(*document),
            _ => None,
        })
        .unwrap();
    let marker = d.seen.len();
    let fragment = NavigationId::next();
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: fragment,
                url: format!("{first}#frag"),
            },
        )
        .unwrap();
    assert!(d.until(Duration::from_secs(10), |e| e[marker..].iter().any(|e| matches!(e, EngineEvent::NavigationCommitted { navigation, .. } if *navigation == fragment))));
    let same = d.seen[marker..]
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationCommitted { document, url, .. } => {
                Some((*document, url.clone()))
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(same.0, documents_before, "a fragment is the same document");
    assert!(same.1.ends_with("#frag"));

    // 4. Another document, then history back and forward through the three entries.
    let marker = d.seen.len();
    let two = NavigationId::next();
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: two,
                url: second.clone(),
            },
        )
        .unwrap();
    assert!(d.until(Duration::from_secs(20), |e| {
        e[marker..]
            .iter()
            .any(|e| matches!(e, EngineEvent::TitleChanged { title: Some(t), .. } if t == "Second"))
            && e[marker..]
                .iter()
                .any(|e| matches!(e, EngineEvent::LoadingChanged { loading: false, .. }))
    }));
    let new_document = d.seen[marker..]
        .iter()
        .find_map(|e| match e {
            EngineEvent::NavigationCommitted {
                document,
                navigation,
                ..
            } if *navigation == two => Some(*document),
            _ => None,
        })
        .expect("commit of the second page");
    assert_ne!(new_document, documents_before, "a new document");
    let history = d
        .seen
        .iter()
        .rev()
        .find_map(|e| match e {
            EngineEvent::HistoryChanged {
                entries, current, ..
            } => Some((entries.clone(), *current)),
            _ => None,
        })
        .expect("HistoryChanged");
    assert_eq!(history.0.len(), 3, "{history:?}");
    assert_eq!(history.1, 2);

    // The second page's frame is blue.
    let mut blue = false;
    for _ in 0..50 {
        d.pump();
        let permit = Permit {
            epoch: metrics_cell.borrow().epoch(),
        };
        if d.runtime.present(view, permit).unwrap() == PresentOutcome::Submitted
            && pixel(&shot.borrow(), 600, 400) == [255, 0, 0, 255]
        {
            blue = true;
            break;
        }
        let _ = d.wakes.recv_timeout(Duration::from_millis(100));
    }
    assert!(blue, "the second page's background");

    // 5. Back: to the fragment entry (the same document), then to the first page's start.
    let marker = d.seen.len();
    let back = NavigationId::next();
    d.runtime
        .command(
            view,
            EngineCommand::Traverse {
                navigation: back,
                delta: -2,
            },
        )
        .unwrap();
    assert!(d.until(Duration::from_secs(20), |e| e[marker..].iter().any(
        |e| matches!(e, EngineEvent::TitleChanged { title: Some(t), .. } if t == "First")
    )));
    let history = d
        .seen
        .iter()
        .rev()
        .find_map(|e| match e {
            EngineEvent::HistoryChanged {
                entries, current, ..
            } => Some((entries.clone(), *current)),
            _ => None,
        })
        .unwrap();
    assert_eq!(history.1, 0, "{history:?}");

    // 5b. A click on a link: the page starts its own navigation, with an id of the engine's own.
    {
        let _ = d.until(Duration::from_secs(5), |_| true);
        let marker = d.seen.len();
        let seat = sighurt_api::ids::SeatId::next();
        let position = SurfacePoint { x: 50.0, y: 150.0 };
        let click = |state| {
            EngineCommand::Input(InputEvent {
                seat,
                kind: InputKind::Pointer(PointerEvent::Button {
                    button: PointerButton::Left,
                    state,
                    position,
                    modifiers: Modifiers::default(),
                }),
            })
        };
        d.runtime
            .command(
                view,
                EngineCommand::Input(InputEvent {
                    seat,
                    kind: InputKind::Pointer(PointerEvent::Motion {
                        position,
                        modifiers: Modifiers::default(),
                    }),
                }),
            )
            .unwrap();
        assert!(d.until(Duration::from_secs(10), |e| e[marker..].iter().any(|e| matches!(e, EngineEvent::StatusTextChanged { text: Some(t), .. } if *t == second))), "link hover: {:#?}", &d.seen[marker..]);
        d.runtime
            .command(view, click(ButtonState::Pressed))
            .unwrap();
        d.runtime
            .command(view, click(ButtonState::Released))
            .unwrap();
        // The click is the user's: the engine asks before following it (tests/navigation.rs).
        assert!(
            d.until(Duration::from_secs(20), |e| e[marker..]
                .iter()
                .any(|e| matches!(e, EngineEvent::NavigationRequested { .. }))),
            "click: {:#?}",
            &d.seen[marker..]
        );
        let request = d.seen[marker..]
            .iter()
            .find_map(|e| match e {
                EngineEvent::NavigationRequested { request, .. } => Some(*request),
                _ => None,
            })
            .unwrap();
        d.runtime
            .command(
                view,
                EngineCommand::AnswerNavigation {
                    request,
                    allow: true,
                },
            )
            .unwrap();
        assert!(
            d.until(Duration::from_secs(20), |e| e[marker..].iter().any(
                |e| matches!(e, EngineEvent::NavigationCommitted { url, .. } if *url == second)
            )),
            "click: {:#?}",
            &d.seen[marker..]
        );
        let started = d.seen[marker..].iter().find_map(|e| match e {
            EngineEvent::NavigationStarted { navigation, .. } => Some(*navigation),
            _ => None,
        });
        assert!(
            started.is_some_and(|id| id != back && id != two),
            "the page's own navigation has its own id"
        );
    }

    // 6. A resize: Ladybird renders again at the new size; until then the host's permit is not met.
    *metrics_cell.borrow_mut() = metrics(800, 600);
    d.runtime
        .command(view, EngineCommand::SetMetrics(*metrics_cell.borrow()))
        .unwrap();
    let mut resized = false;
    for _ in 0..80 {
        d.pump();
        let permit = Permit {
            epoch: metrics_cell.borrow().epoch(),
        };
        match d.runtime.present(view, permit).unwrap() {
            PresentOutcome::Submitted => {
                resized = shot.borrow().size
                    == Some(BufferSize {
                        width: 800,
                        height: 600,
                    });
                if resized {
                    break;
                }
            }
            PresentOutcome::NotReady | PresentOutcome::Unchanged => {}
        }
        let _ = d.wakes.recv_timeout(Duration::from_millis(100));
    }
    assert!(resized, "a frame at 800x600");
    assert_eq!(pixel(&shot.borrow(), 790, 590), [0, 0, 255, 255]);

    // 7. A hidden view stops presenting; showing it brings the frame back.
    d.runtime
        .command(view, EngineCommand::SetVisibility(false))
        .unwrap();
    d.idle(Duration::from_millis(500));
    d.runtime
        .command(view, EngineCommand::SetVisibility(true))
        .unwrap();

    // 8. Idle: with a static page, the owner loop is woken only by Ladybird's own low-rate
    // housekeeping, never in a loop. (Measured, not zero: see docs/rewrite/LADYBIRD.md.)
    d.idle(Duration::from_millis(1500));
    let wakes_before = d.runtime.wake_count();
    let pumps = d.idle(Duration::from_secs(6));
    let woken = d.runtime.wake_count() - wakes_before;
    eprintln!("idle 6 s: {pumps} pumps, {woken} loop wakes");
    assert!(pumps <= 40, "{pumps} pumps in 6 s of idle");
    assert!(woken <= 40, "{woken} wakes in 6 s of idle");

    // 9. A helper that dies is reaped by Ladybird (by its pid), and nothing else is: the process
    // killed here is gone from the process table, the bystander is still ours to wait for.
    let web_content = child_pids()
        .into_iter()
        .find(|pid| {
            std::fs::read(format!("/proc/{pid}/cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains("WebContent"))
        })
        .expect("a WebContent process");
    let marker = d.seen.len();
    unsafe { libc::kill(web_content, libc::SIGKILL) };
    assert!(
        d.until(Duration::from_secs(10), |e| e[marker..]
            .iter()
            .any(|e| matches!(e, EngineEvent::ViewFailed { .. }))),
        "the crash is reported"
    );
    let mut reaped = false;
    for _ in 0..50 {
        d.pump();
        if !Path::new(&format!("/proc/{web_content}")).exists() {
            reaped = true;
            break;
        }
        let _ = d.wakes.recv_timeout(Duration::from_millis(100));
    }
    assert!(reaped, "Ladybird reaps the helper that died");
    assert!(
        matches!(bystander.try_wait(), Ok(Some(status)) if status.success()),
        "a child that is not Ladybird's is still ours to wait for"
    );

    // 10. Closing reports ViewClosed, and shutdown leaves no helper processes of ours behind.
    d.runtime.close_view(view);
    d.pump();
    assert!(d
        .seen
        .iter()
        .any(|e| matches!(e, EngineEvent::ViewClosed { view: v } if *v == view)));
    d.runtime.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
