//! Find in page (shim ABI 3): a search gives the selected match and the total, moves wrap,
//! case is as asked, no match is said; an answer is never stamped with a later search; moves
//! and stops of a search the view no longer has and a search of a gone document do nothing; a
//! new document ends the search. Needs the shim (nix/build-ladybird-shim.sh); skipped without
//! it. One process, one Ladybird (see tests/headless.rs).
//!
//!   tools/heavy cargo test -p sighurt-engine-ladybird --test find -- --nocapture

use std::sync::mpsc;
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, FindCommand, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, NavigationId, RequestId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_engine_ladybird::{ffi, LadybirdConfig, LadybirdRuntime};

struct Mem(Metrics);

impl CpuTarget for Mem {
    type Permit = LayoutEpoch;
    fn metrics(&self) -> Metrics {
        self.0
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

/// Found, current and total.
type Found = (bool, Option<u32>, Option<u32>);

struct Driver {
    runtime: LadybirdRuntime<Mem>,
    wakes: mpsc::Receiver<()>,
    waker: EngineWaker,
    seen: Vec<EngineEvent>,
    deadline: Option<Instant>,
    view: ViewId,
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

    /// Runs the loop until `done` holds, at most `timeout`; whether it held.
    fn until(&mut self, timeout: Duration, done: impl Fn(&[EngineEvent]) -> bool) -> bool {
        let end = Instant::now() + timeout;
        loop {
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
            self.pump();
        }
    }

    fn command(&mut self, command: EngineCommand) {
        self.runtime.command(self.view, command).unwrap();
    }

    fn find(&mut self, command: FindCommand) {
        self.command(EngineCommand::Find(command));
    }

    /// Navigates and waits for the commit; the new document.
    fn open(&mut self, url: &str) -> DocumentGeneration {
        let navigation = NavigationId::next();
        self.command(EngineCommand::Navigate {
            navigation,
            url: url.into(),
        });
        let committed = |seen: &[EngineEvent]| {
            seen.iter().rev().find_map(|e| match e {
                EngineEvent::NavigationCommitted {
                    navigation: n,
                    document,
                    ..
                } if *n == navigation => Some(*document),
                _ => None,
            })
        };
        assert!(
            self.until(Duration::from_secs(15), |seen| committed(seen).is_some()),
            "{url} commits"
        );
        committed(&self.seen).unwrap()
    }

    fn search(&mut self, document: DocumentGeneration, text: &str, case: bool) -> RequestId {
        let search = RequestId::next();
        self.find(FindCommand::Search {
            search,
            document,
            text: text.into(),
            case_sensitive: case,
        });
        search
    }

    /// The results for `search` from event `from` on.
    fn found(&self, from: usize, search: RequestId) -> Vec<Found> {
        found(&self.seen[from..], search)
    }

    /// Waits for the next result for `search` after `from`.
    fn next_result(&mut self, from: usize, search: RequestId) -> Found {
        assert!(
            self.until(Duration::from_secs(10), |seen| !found(
                &seen[from..],
                search
            )
            .is_empty()),
            "a result for {search:?}"
        );
        self.found(from, search)[0]
    }

    /// Every find result from event `from` on.
    fn results(&self, from: usize) -> usize {
        let results = self.seen[from..].iter();
        results
            .filter(|e| matches!(e, EngineEvent::FindResult { .. }))
            .count()
    }

    fn idle(&mut self, time: Duration) {
        self.until(time, |_| false);
    }
}

fn found(seen: &[EngineEvent], search: RequestId) -> Vec<Found> {
    let results = seen.iter().filter_map(|e| match e {
        EngineEvent::FindResult {
            search: s,
            found,
            current,
            total,
            ..
        } if *s == search => Some((*found, *current, *total)),
        _ => None,
    });
    results.collect()
}

#[test]
fn find_in_page_counts_moves_and_stops() {
    if ffi::load(None).is_err() {
        eprintln!("skipped: no Ladybird shim (nix/build-ladybird-shim.sh)");
        return;
    }
    let dir = std::env::temp_dir().join(format!("sighurt-ladybird-find-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let page = dir.join("find.html");
    std::fs::write(
        &page,
        "<!doctype html><title>find</title><body style='font: 20px sans-serif'>\
         <p>apple one</p><p>Apple two</p><p>apple three</p>\
         <p style='margin-top: 1500px'>apple four</p>",
    )
    .unwrap();
    let url = format!("file://{}", page.display());

    let (wake_tx, wakes) = mpsc::channel();
    let waker = EngineWaker::new(move || {
        let _ = wake_tx.send(());
    });
    let runtime = LadybirdRuntime::<Mem>::new(LadybirdConfig::default(), waker.clone())
        .unwrap_or_else(|e| panic!("LadybirdRuntime::new: {e}"));
    let view = ViewId::next();
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
    let mut d = Driver {
        runtime,
        wakes,
        waker,
        seen: Vec::new(),
        deadline: None,
        view,
    };
    d.runtime
        .open_view(view, ViewOptions::default(), Mem(metrics))
        .unwrap();
    let document = d.open(&url);
    d.until(Duration::from_secs(10), |seen| {
        seen.iter()
            .any(|e| matches!(e, EngineEvent::TitleChanged { title: Some(t), .. } if t == "find"))
    });

    // A search: the first match of four; moves select the next and previous, wrapping.
    let from = d.seen.len();
    let apple = d.search(document, "apple", false);
    assert_eq!(d.next_result(from, apple), (true, Some(1), Some(4)));
    for (command, current) in [
        (FindCommand::Next { search: apple }, 2),
        (FindCommand::Previous { search: apple }, 1),
        (FindCommand::Previous { search: apple }, 4),
    ] {
        let from = d.seen.len();
        d.find(command);
        assert_eq!(d.next_result(from, apple), (true, Some(current), Some(4)));
    }
    let from = d.seen.len();
    let sensitive = d.search(document, "Apple", true);
    assert_eq!(d.next_result(from, sensitive), (true, Some(1), Some(1)));
    let from = d.seen.len();
    let pear = d.search(document, "pear", false);
    assert_eq!(d.next_result(from, pear), (false, None, Some(0)));

    // Two searches at once: the first's answer is not the second's.
    let from = d.seen.len();
    let replaced = d.search(document, "apple", false);
    let three = d.search(document, "three", false);
    assert_eq!(d.next_result(from, three), (true, Some(1), Some(1)));
    d.idle(Duration::from_millis(500));
    assert_eq!(d.found(from, replaced), []);
    assert_eq!(d.found(from, three), [(true, Some(1), Some(1))]);

    // Moves and stops of a search the view no longer has, a search of a gone document: nothing.
    let from = d.seen.len();
    d.find(FindCommand::Next { search: replaced });
    d.find(FindCommand::Previous { search: replaced });
    d.find(FindCommand::Stop { search: replaced });
    d.search(DocumentGeneration::next(), "apple", false);
    d.idle(Duration::from_millis(500));
    assert_eq!(d.results(from), 0, "nothing answered");
    // Stopped, the search doesn't move; a stop's own answer is nobody's.
    d.find(FindCommand::Stop { search: three });
    d.find(FindCommand::Next { search: three });
    d.idle(Duration::from_millis(500));
    assert_eq!(d.results(from), 0, "a stopped search doesn't move");
    // The search after the stop starts from the top again.
    let again = d.search(document, "apple", false);
    assert_eq!(d.next_result(from, again), (true, Some(1), Some(4)));

    // A new document ends the search.
    d.open(&format!("{url}?again"));
    let from = d.seen.len();
    d.find(FindCommand::Next { search: again });
    d.idle(Duration::from_millis(500));
    assert_eq!(d.results(from), 0, "the search ended with its document");

    d.runtime.close_view(view);
    d.runtime.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}
