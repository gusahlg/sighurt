//! A navigation the user starts in a page (a click on a link) is asked of the browser first
//! (`NavigationRequested`) and held until `AnswerNavigation`; one the page starts alone, and the
//! browser's own, are not. Needs the patched Ladybird (nix/ladybird.nix): with the unpatched one the
//! shim does not build. One process, one Ladybird (see tests/headless.rs).
//!
//!   tools/heavy cargo test -p sighurt-engine-ladybird --test navigation -- --nocapture

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PumpStatus, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{Metrics, Scale, SurfacePoint, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, RequestId, SeatId, ViewId};
use sighurt_api::input::{
    ButtonState, InputEvent, InputKind, Modifiers, PointerButton, PointerEvent,
};
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

    fn requested(&mut self, timeout: Duration) -> Option<(RequestId, String)> {
        let end = Instant::now() + timeout;
        loop {
            while self.cursor < self.seen.len() {
                let event = &self.seen[self.cursor];
                self.cursor += 1;
                if let EngineEvent::NavigationRequested { request, url, .. } = event {
                    return Some((*request, url.clone()));
                }
            }
            let now = Instant::now();
            if now >= end {
                return None;
            }
            let wait = self.deadline.map_or(end - now, |d| {
                d.saturating_duration_since(now).min(end - now)
            });
            let _ = self.wakes.recv_timeout(wait);
            self.pump();
        }
    }

    fn committed_url_within(&mut self, url: &str, timeout: Duration) -> bool {
        let wanted = url.to_owned();
        let end = Instant::now() + timeout;
        loop {
            if self.seen.iter().any(
                |e| matches!(e, EngineEvent::NavigationCommitted { url, .. } if *url == wanted),
            ) {
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
            self.pump();
        }
    }
}

#[test]
fn a_user_clicked_link_waits_for_the_browser() {
    if ffi::load(None).is_err() {
        eprintln!("skipped: no Ladybird shim (nix/build-ladybird-shim.sh)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sighurt-ladybird-nav-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let two = dir.join("two.html");
    std::fs::write(&two, "<!doctype html><title>two</title>two").unwrap();
    let two_url = format!("file://{}", two.display());
    let page = dir.join("one.html");
    std::fs::write(
        &page,
        format!(
            "<!doctype html><title>one</title><body style='margin:0'>\
             <a href='{two_url}' style='position:absolute;left:10px;top:10px;width:300px;height:40px;display:block'>go</a>\
             <a href='#here' style='position:absolute;left:10px;top:70px;width:300px;height:40px;display:block'>fragment</a>"
        ),
    )
    .unwrap();
    let one_url = format!("file://{}", page.display());

    let (wake_tx, wakes) = mpsc::channel();
    let waker = EngineWaker::new(move || {
        let _ = wake_tx.send(());
    });
    let runtime = LadybirdRuntime::<Mem>::new(LadybirdConfig::default(), waker.clone())
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
                url: one_url.clone(),
            },
        )
        .unwrap();
    d.title("one");
    // The browser's own navigation was not asked about.
    assert!(d
        .seen
        .iter()
        .all(|e| !matches!(e, EngineEvent::NavigationRequested { .. })));

    // A click on a link is asked about, and nothing happens until the answer: denied, it never does.
    d.click(20.0, 25.0);
    let (request, url) = d.requested(Duration::from_secs(15)).expect("a request");
    assert_eq!(url, two_url);
    assert!(
        !d.committed_url_within(&two_url, Duration::from_millis(1500)),
        "held, not performed"
    );
    d.runtime
        .command(
            view,
            EngineCommand::AnswerNavigation {
                request,
                allow: false,
            },
        )
        .unwrap();
    assert!(
        !d.committed_url_within(&two_url, Duration::from_millis(1500)),
        "denied"
    );
    // An answer again for the same request does nothing.
    d.runtime
        .command(
            view,
            EngineCommand::AnswerNavigation {
                request,
                allow: true,
            },
        )
        .unwrap();
    assert!(!d.committed_url_within(&two_url, Duration::from_millis(500)));

    // The same click, allowed: it goes on.
    d.click(20.0, 25.0);
    let (request, _) = d
        .requested(Duration::from_secs(15))
        .expect("a second request");
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
        d.committed_url_within(&two_url, Duration::from_secs(20)),
        "allowed, it commits"
    );
    d.title("two");

    // Back to the first page; a fragment link of the same document is not asked about.
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: one_url.clone(),
            },
        )
        .unwrap();
    d.title("one");
    let marker = d.seen.len();
    d.click(20.0, 85.0);
    assert!(
        d.committed_url_within(&format!("{one_url}#here"), Duration::from_secs(10)),
        "the fragment goes on"
    );
    assert!(d.seen[marker..]
        .iter()
        .all(|e| !matches!(e, EngineEvent::NavigationRequested { .. })));

    // A request held while the document is replaced is dropped: its late answer does nothing.
    d.click(20.0, 25.0);
    let (late, _) = d
        .requested(Duration::from_secs(15))
        .expect("a request to leave open");
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: two_url.clone(),
            },
        )
        .unwrap();
    d.title("two");
    d.runtime
        .command(
            view,
            EngineCommand::AnswerNavigation {
                request: late,
                allow: true,
            },
        )
        .unwrap();
    d.runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: one_url.clone(),
            },
        )
        .unwrap();
    d.title("one");

    // At most 16 are held for a view: the rest are denied without being asked.
    let marker = d.seen.len();
    for _ in 0..24 {
        d.click(20.0, 25.0);
        let _ = d.requested(Duration::from_millis(300));
    }
    let asked = d.seen[marker..]
        .iter()
        .filter(|e| matches!(e, EngineEvent::NavigationRequested { .. }))
        .count();
    assert!(asked <= 16, "{asked} requests held at once");
    assert!(asked >= 2, "{asked}");

    // Closing the view with requests held, and shutting down, leave nothing running.
    d.runtime.close_view(view);
    d.runtime.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
