//! Several views of the one Ladybird runtime, one after another and at once: each takes its
//! navigation and shows its page, also when the first is hidden, closed, or the next one starts with
//! no room (an empty target, as `sig` opens a tab's view before it is laid out). One process, one
//! Ladybird (see tests/headless.rs).
//!
//!   tools/heavy cargo test -p sighurt-engine-ladybird --test views -- --nocapture

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PumpStatus, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
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
}

impl Driver {
    /// Waits for a title event of `view`.
    fn title_of(&mut self, view: ViewId, wanted: &str) {
        let wanted = wanted.to_owned();
        self.next(
            Duration::from_secs(25),
            &format!("title {wanted} of {view:?}"),
            |e| match e {
                EngineEvent::TitleChanged {
                    view: v,
                    title: Some(t),
                } if *v == view && *t == wanted => Some(()),
                _ => None,
            },
        );
    }
}

fn page(dir: &std::path::Path, name: &str, title: &str) -> String {
    let path = dir.join(name);
    std::fs::write(
        &path,
        format!("<!doctype html><title>{title}</title>{title}"),
    )
    .unwrap();
    format!("file://{}", path.display())
}

fn metrics(width: u32, height: u32) -> Rc<RefCell<Metrics>> {
    Rc::new(RefCell::new(Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width,
            height,
        },
        Scale::ONE,
    )))
}

#[test]
fn every_view_gets_its_page() {
    if ffi::load(None).is_err() {
        eprintln!("skipped: no Ladybird shim (nix/build-ladybird-shim.sh)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sighurt-ladybird-views-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (wake_tx, wakes) = mpsc::channel();
    let waker = EngineWaker::new(move || {
        let _ = wake_tx.send(());
    });
    let runtime = LadybirdRuntime::<Mem>::new(LadybirdConfig::default(), waker.clone())
        .unwrap_or_else(|e| panic!("LadybirdRuntime::new: {e}"));
    let first = ViewId::next();
    let mut d = Driver {
        runtime,
        wakes,
        waker,
        seen: Vec::new(),
        cursor: 0,
        deadline: None,
    };

    // One view.
    d.runtime
        .open_view(first, ViewOptions::default(), Mem(metrics(640, 480)))
        .unwrap();
    d.runtime
        .command(
            first,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: page(&dir, "a.html", "A"),
            },
        )
        .unwrap();
    d.title_of(first, "A");

    // Time passes: Ladybird has started a spare WebContent process, which loads its blank page before
    // any view exists. The next view takes it over and is never told of that load.
    let wait_until = Instant::now() + Duration::from_secs(4);
    while Instant::now() < wait_until {
        let _ = d.wakes.recv_timeout(Duration::from_millis(100));
        d.pump();
    }

    // The first is hidden (a tab left behind); the second starts with no room and hidden, as a
    // tab's view does before the host has laid it out, then is shown at a size.
    d.runtime
        .command(first, EngineCommand::SetVisibility(false))
        .unwrap();
    let second = ViewId::next();
    let second_metrics = metrics(0, 0);
    d.runtime
        .open_view(
            second,
            ViewOptions {
                visible: false,
                ..ViewOptions::default()
            },
            Mem(Rc::clone(&second_metrics)),
        )
        .unwrap();
    d.runtime
        .command(
            second,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: page(&dir, "b.html", "B"),
            },
        )
        .unwrap();
    // As the browser does: it shows the view once the engine says it is ready, not before.
    d.next(
        Duration::from_secs(20),
        "ViewReady of the second, still hidden",
        |e| match e {
            EngineEvent::ViewReady { view } if *view == second => Some(()),
            _ => None,
        },
    );
    *second_metrics.borrow_mut() = Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width: 800,
            height: 600,
        },
        Scale::ONE,
    );
    d.runtime
        .command(second, EngineCommand::SetMetrics(*second_metrics.borrow()))
        .unwrap();
    d.runtime
        .command(second, EngineCommand::SetVisibility(true))
        .unwrap();
    d.title_of(second, "B");
    assert!(
        d.seen
            .iter()
            .any(|e| matches!(e, EngineEvent::ViewReady { view } if *view == second)),
        "ViewReady of the second"
    );

    // The first closes; a third opens afterwards and loads.
    d.runtime.close_view(first);
    let third = ViewId::next();
    d.runtime
        .open_view(third, ViewOptions::default(), Mem(metrics(640, 480)))
        .unwrap();
    d.runtime
        .command(
            third,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: page(&dir, "c.html", "C"),
            },
        )
        .unwrap();
    d.title_of(third, "C");

    // Every view that is closed and opened again, in sequence, keeps loading.
    for round in 0..3 {
        d.runtime.close_view(third);
        let next = ViewId::next();
        d.runtime
            .open_view(next, ViewOptions::default(), Mem(metrics(640, 480)))
            .unwrap();
        let name = format!("r{round}");
        d.runtime
            .command(
                next,
                EngineCommand::Navigate {
                    navigation: NavigationId::next(),
                    url: page(&dir, &format!("{name}.html"), &name),
                },
            )
            .unwrap();
        d.title_of(next, &name);
        let _ = next;
        d.runtime.close_view(next);
    }
    d.runtime.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
