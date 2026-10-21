//! A page crash fails the view's document, not the view or the runtime (plan §25): the view
//! loads again, in a new WebView, and the other views go on. On Servo 0.5, afterwards Servo
//! could not shut down (its constellation waited forever for the dead script thread's
//! pipelines), so the runtime leaves it running and says so. That workaround stays until this
//! test shows 0.7 finishes shutting down (see `SERVO-0.7.md`).
//!
//! A process of its own with its own `main` (`harness = false`): with Servo left running, the
//! process has to end without exit handlers (`_exit`), as `sig` must after `leaked`; the
//! driver's exit handlers would otherwise tear down GL state Servo still uses (M1.md,
//! "Teardown"). Needs a compositor and Servo built with `testbinding` (a dev-dependency) for
//! `ServoTestUtils.panic()`: `tests/headless.sh` runs it; elsewhere it says it was skipped.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Driver, Layout, State};
use sighurt_api::events::EngineEvent;
use sighurt_api::ids::{NavigationId, ViewId};
use sighurt_engine_servo::ServoConfig;

const WAIT: Duration = Duration::from_secs(20);

fn page(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pages")
        .join(name);
    url::Url::from_file_path(path).unwrap().to_string()
}

fn committed(state: &State, view: ViewId, navigation: NavigationId) -> bool {
    state.events_of(view).any(
        |e| matches!(e, EngineEvent::NavigationCommitted { navigation: n, .. } if *n == navigation),
    )
}

fn count(state: &State, view: ViewId, what: fn(&EngineEvent) -> bool) -> usize {
    state.events_of(view).filter(|e| what(e)).count()
}

fn main() {
    if std::env::var_os("SIGHURT_SERVO_HEADLESS").is_none() {
        println!("crash: skipped (needs a compositor: run tests/headless.sh)");
        return;
    }
    a_crashed_page_fails_its_document_and_the_view_loads_again();
    println!("crash: a_crashed_page_fails_its_document_and_the_view_loads_again ... ok");
    // SAFETY: ends the process at once, as `sig` does after a leaked Servo: nothing runs after.
    unsafe { libc::_exit(0) }
}

fn a_crashed_page_fails_its_document_and_the_view_loads_again() {
    let mut d = Driver::new(ServoConfig::default(), Layout::Split);
    // `ServoTestUtils` exists only with `testbinding` and this preference.
    let mut prefs = servo::prefs::get().clone();
    prefs.dom_servo_helpers_enabled = true;
    servo::prefs::set(prefs);

    let a = d.state.open(&page("second.html"), true);
    let b = d.state.open(&page("second.html"), true);
    let loaded = |s: &State, view| {
        s.events_of(view)
            .any(|e| matches!(e, EngineEvent::NavigationCommitted { .. }))
    };
    assert!(d.run_until(WAIT, |s| loaded(s, a) && loaded(s, b)));

    let crash = d.state.navigate(b, &page("crash.html"));
    assert!(d.run_until(WAIT, |s| committed(s, b, crash)));
    let failed = |e: &EngineEvent| matches!(e, EngineEvent::ViewFailed { .. });
    assert!(
        d.run_until(WAIT, |s| s.events_of(b).any(failed)),
        "the crash is reported"
    );

    // The next navigation loads in a new WebView of the same view.
    let again = d.state.navigate(b, &page("second.html"));
    assert!(
        d.run_until(WAIT, |s| committed(s, b, again)),
        "the view loads again"
    );
    let frames = d.state.submitted(b);
    d.state
        .command(b, sighurt_api::engine::EngineCommand::SetZoom(1.5));
    assert!(
        d.run_until(WAIT, |s| s.submitted(b) > frames),
        "and presents"
    );
    let ready = |e: &EngineEvent| matches!(e, EngineEvent::ViewReady { .. });
    let closed = |e: &EngineEvent| matches!(e, EngineEvent::ViewClosed { .. });
    assert_eq!(count(&d.state, b, ready), 1, "still the same view");
    assert_eq!(count(&d.state, b, closed), 0, "the view stays open");
    // Nothing happened to the other view, or to the runtime.
    let failures = |e: &EngineEvent| {
        matches!(
            e,
            EngineEvent::ViewFailed { .. } | EngineEvent::ViewClosed { .. }
        )
    };
    assert_eq!(count(&d.state, a, failures), 0);
    assert!(!d
        .state
        .events
        .iter()
        .any(|(_, e)| matches!(e, EngineEvent::RuntimeFailed { .. })));
    let reload = d.state.navigate(a, &page("history.html"));
    assert!(
        d.run_until(WAIT, |s| committed(s, a, reload)),
        "a still loads"
    );

    // Shutting down returns, with Servo left running.
    let state = d.shutdown();
    assert!(state.runtime.diagnostics().leaked);
    assert!(!state.host.is_lost());
}
