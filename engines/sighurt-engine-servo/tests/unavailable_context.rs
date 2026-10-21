//! A view whose GL context can't be made fails and closes; the runtime goes on (plan §25).
//!
//! `tests/headless.sh` runs this with an EGL vendor list that names no driver, so libglvnd
//! gives surfman no display; Servo itself needs no GL until a view has a context. Like
//! `engine.rs`, it needs a compositor and is `#[ignore]`d.

mod common;

use std::time::Duration;

use common::{Driver, Layout};
use sighurt_api::engine::{EngineError, EngineRuntime, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::ids::ViewId;
use sighurt_engine_servo::ServoConfig;
use sighurt_wayland::PresentMode;

#[test]
#[ignore = "needs a compositor and no EGL driver: run tests/headless.sh"]
fn a_view_without_a_gl_context_fails_and_closes_alone() {
    let mut d = Driver::new(ServoConfig::default(), Layout::Stack);
    let view = d.state.open("data:text/html,<p>never shown</p>", true);
    let ended = |s: &common::State| {
        s.events_of(view)
            .any(|e| *e == EngineEvent::ViewClosed { view })
    };
    assert!(d.run_until(Duration::from_secs(20), ended));
    let events: Vec<_> = d.state.events_of(view).cloned().collect();
    let failed = events
        .iter()
        .position(|e| matches!(e, EngineEvent::ViewFailed { .. }))
        .expect("the view fails first");
    let closed = events
        .iter()
        .position(|e| *e == EngineEvent::ViewClosed { view });
    assert!(Some(failed) < closed, "failed, then closed: {events:?}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, EngineEvent::ViewReady { .. })),
        "never ready"
    );
    assert!(
        !d.state
            .events
            .iter()
            .any(|(_, e)| matches!(e, EngineEvent::RuntimeFailed { .. })),
        "the runtime goes on"
    );
    assert_eq!(d.state.runtime.diagnostics().views, 0);

    // A target without the GL path is refused at once, and the target given back.
    let other = ViewId::next();
    let window = d.state.window;
    let lease = d
        .state
        .host
        .create_target(window, PresentMode::Shm, Some(other))
        .unwrap();
    let refused = d
        .state
        .runtime
        .open_view(other, ViewOptions::default(), lease);
    assert!(
        matches!(refused, Err(EngineError::Unsupported(_))),
        "{refused:?}"
    );
    d.run_for(Duration::from_millis(100));

    let state = d.shutdown();
    assert!(!state.host.is_lost());
}
