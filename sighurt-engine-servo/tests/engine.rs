//! The Servo adapter against a real compositor and a real Servo: navigation and history (with
//! same-document entries), failed navigations and hidden views each have their own outcome
//! (plan §25, with `crash.rs` and `unavailable_context.rs`), the display path reads nothing back,
//! and Servo shuts down under its profile lock.
//!
//! Servo initializes once per process, so this is one test, one scenario after another. It
//! needs a Wayland compositor and is `#[ignore]`d: `tests/headless.sh` runs it in a private
//! headless sway.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Driver, Layout, State};
use sighurt_api::engine::EngineCommand;
use sighurt_api::events::{EngineEvent, HistoryEffect, NavigationError, OpenDisposition};
use sighurt_api::ids::{DocumentGeneration, NavigationId, ViewId};
use sighurt_engine_servo::ServoConfig;

const WAIT: Duration = Duration::from_secs(20);
/// How long a view waits for a load Servo may have dropped (`view::LOAD_WAIT`).
const LOAD_WAIT: Duration = Duration::from_secs(5);

fn page(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pages")
        .join(name);
    url::Url::from_file_path(path).unwrap().to_string()
}

/// The latest commit of `view`: navigation, URL and document.
fn commit(state: &State, view: ViewId) -> Option<(NavigationId, String, DocumentGeneration)> {
    state.events_of(view).rev().find_map(|event| match event {
        EngineEvent::NavigationCommitted {
            navigation,
            url,
            document,
            ..
        } => Some((*navigation, url.clone(), *document)),
        _ => None,
    })
}

fn committed(state: &State, view: ViewId, navigation: NavigationId) -> bool {
    state.events_of(view).any(
        |e| matches!(e, EngineEvent::NavigationCommitted { navigation: n, .. } if *n == navigation),
    )
}

/// The latest history of `view`.
fn history(state: &State, view: ViewId) -> Option<(Vec<String>, usize)> {
    state.events_of(view).rev().find_map(|event| match event {
        EngineEvent::HistoryChanged {
            entries, current, ..
        } => Some((entries.clone(), *current)),
        _ => None,
    })
}

/// Whether `view` last said it was loading.
fn loading(state: &State, view: ViewId) -> Option<bool> {
    state.events_of(view).rev().find_map(|event| match event {
        EngineEvent::LoadingChanged { loading, .. } => Some(*loading),
        _ => None,
    })
}

fn count(state: &State, view: ViewId, what: fn(&EngineEvent) -> bool) -> usize {
    state.events_of(view).filter(|e| what(e)).count()
}

fn cancelled(state: &State, view: ViewId, navigation: NavigationId) -> bool {
    state.events_of(view).any(|e| {
        matches!(e, EngineEvent::NavigationFailed { navigation: n, error: NavigationError::Cancelled, .. }
            if *n == navigation)
    })
}

/// The first commit of `view` to `to`: navigation, document and history effect.
fn commit_to(
    state: &State,
    view: ViewId,
    to: &str,
) -> Option<(NavigationId, DocumentGeneration, Option<HistoryEffect>)> {
    state.events_of(view).find_map(|event| match event {
        EngineEvent::NavigationCommitted {
            navigation,
            url,
            document,
            history,
            ..
        } if url == to => Some((*navigation, *document, *history)),
        _ => None,
    })
}

/// What Servo 0.5 drops or doesn't attribute by itself (the M4 review): a navigation after a
/// 204 response, one asked for while the page's own navigation waits for its response, two
/// quick traversals, a page's own traversal, Stop.
fn navigations_servo_would_drop_or_misattribute(d: &mut Driver, a: ViewId) {
    let server = common::serve();
    let url = |path: &str| format!("{server}{path}");
    let started = Instant::now();

    // A 204 response ends its navigation without a word from Servo; the next navigation still
    // loads, at most `LOAD_WAIT` (5 s) after it, and the 204's is given up.
    let no_content = d.state.navigate(a, &url("/204"));
    d.run_for(Duration::from_millis(500));
    let after = d.state.navigate(a, &url("/after-204.html"));
    assert!(
        d.run_until(WAIT, |s| committed(s, a, after)),
        "a navigation after a 204 response loads"
    );
    println!(
        "[{:8.3}] after the 204: {:?}",
        d.state.secs(),
        started.elapsed()
    );
    assert!(started.elapsed() < Duration::from_secs(7));
    assert!(cancelled(&d.state, a, no_content), "the 204's is given up");
    let (_, _, effect) = commit_to(&d.state, a, &url("/after-204.html")).unwrap();
    assert_eq!(effect, Some(HistoryEffect::Push));

    // The page navigates itself to a slow page; the browser's navigation asked for while that
    // one waits for its response is held until the page's commits, then loads with its id.
    let own = d.state.navigate(a, &url("/self.html?go=/slow/own.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, own)));
    d.run_for(Duration::from_millis(1000));
    let typed = d.state.navigate(a, &url("/typed.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, typed)), "typed loads");
    let (page, ..) = commit_to(&d.state, a, &url("/slow/own.html")).expect("the page's commits");
    assert!(page != typed && page != own, "the page's commit is its own");
    assert!(commit(&d.state, a).unwrap().1.ends_with("/typed.html"));

    // Two quick backs, both asked for before either commits, keep their ids.
    for name in ["/one.html", "/two.html"] {
        let navigation = d.state.navigate(a, &url(name));
        assert!(d.run_until(WAIT, |s| committed(s, a, navigation)));
    }
    let (_, two, _) = commit_to(&d.state, a, &url("/two.html")).unwrap();
    let length = history(&d.state, a).unwrap().0.len();
    let back_to_one = d.state.traverse(a, -1);
    let back_to_typed = d.state.traverse(a, -1);
    assert!(d.run_until(WAIT, |s| committed(s, a, back_to_typed)));
    assert!(committed(&d.state, a, back_to_one));
    let traversals: Vec<_> = d
        .state
        .events_of(a)
        .filter_map(|e| match e {
            EngineEvent::NavigationCommitted {
                navigation,
                url,
                document,
                history,
                ..
            } if [back_to_one, back_to_typed].contains(navigation) => {
                Some((*navigation, url.clone(), *document, *history))
            }
            _ => None,
        })
        .collect();
    assert_eq!(traversals.len(), 2, "{traversals:?}");
    assert_eq!(traversals[0].0, back_to_one);
    assert!(traversals[0].1.ends_with("/one.html"), "{traversals:?}");
    assert!(traversals[1].1.ends_with("/typed.html"), "{traversals:?}");
    assert_ne!(traversals[0].2, two);
    assert!(traversals
        .iter()
        .all(|t| t.3 == Some(HistoryEffect::Traverse)));
    assert_eq!(history(&d.state, a).unwrap().0.len(), length);

    // The page's own `history.back()`: its own id, a traversal.
    let goes_back = d.state.navigate(a, &url("/goes-back.html?back"));
    assert!(d.run_until(WAIT, |s| committed(s, a, goes_back)));
    let went_back = |s: &State| {
        s.events_of(a).rev().find_map(|e| match e {
            EngineEvent::NavigationCommitted {
                navigation,
                url,
                history,
                ..
            } if url.ends_with("/typed.html") && ![typed, back_to_typed].contains(navigation) => {
                Some((*navigation, *history))
            }
            _ => None,
        })
    };
    assert!(
        d.run_until(WAIT, |s| went_back(s).is_some()),
        "the page goes back"
    );
    let (own_back, effect) = went_back(&d.state).unwrap();
    assert_ne!(own_back, goes_back);
    assert_eq!(effect, Some(HistoryEffect::Traverse));

    // A back that finds no entry by the time Servo gets it (the first took the last one) is
    // cancelled.
    let c = d.state.open(&url("/c1.html"), false);
    assert!(d.run_until(WAIT, |s| commit(s, c).is_some()));
    let to_c2 = d.state.navigate(c, &url("/c2.html"));
    assert!(d.run_until(WAIT, |s| committed(s, c, to_c2)));
    let (back, too_far) = (d.state.traverse(c, -1), d.state.traverse(c, -1));
    assert!(d.run_until(WAIT, |s| committed(s, c, back) && cancelled(s, c, too_far)));
    d.state.close(c);
    assert!(d.run_until(WAIT, |s| s
        .events_of(c)
        .any(|e| matches!(e, EngineEvent::ViewClosed { .. }))));

    // A page's pop-up asks the browser for a new view on behalf of the document that opened it.
    let opener = d.state.navigate(a, &url("/opener.html?open=/opened.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, opener)));
    let (_, opener_doc, _) =
        commit_to(&d.state, a, &url("/opener.html?open=/opened.html")).unwrap();
    let opened = |s: &State| {
        s.events_of(a).find_map(|e| match e {
            EngineEvent::OpenRequested {
                document,
                url,
                disposition: OpenDisposition::NewView,
                ..
            } if url.ends_with("/opened.html") => Some(*document),
            _ => None,
        })
    };
    assert!(
        d.run_until(WAIT, |s| opened(s).is_some()),
        "the pop-up asks"
    );
    assert_eq!(opened(&d.state), Some(opener_doc));

    // Stop stops the document's own loading: an image that never comes keeps it loading until
    // then (`window.stop()`).
    let held = d.state.navigate(a, &url("/loading.html?img=/held"));
    assert!(d.run_until(WAIT, |s| committed(s, a, held)));
    d.run_for(Duration::from_millis(1000));
    assert_eq!(
        loading(&d.state, a),
        Some(true),
        "the image keeps it loading"
    );
    d.state.command(a, EngineCommand::Stop);
    assert!(
        d.run_until(Duration::from_secs(2), |s| loading(s, a) == Some(false)),
        "Stop stops the document's loading"
    );

    // Stop can't stop a navigation waiting for its response (Servo 0.5): it commits, with its
    // id. After a 204, though, the navigation Servo dropped is given up.
    let slow = d.state.navigate(a, &url("/slow/stopped.html"));
    d.run_for(Duration::from_millis(1000));
    d.state.command(a, EngineCommand::Stop);
    assert!(
        d.run_until(WAIT, |s| committed(s, a, slow)),
        "it still commits"
    );
    assert!(!cancelled(&d.state, a, slow));
    let dropped = d.state.navigate(a, &url("/204"));
    d.run_for(LOAD_WAIT + Duration::from_millis(500));
    assert!(!cancelled(&d.state, a, dropped));
    d.state.command(a, EngineCommand::Stop);
    d.run_for(Duration::from_millis(200));
    assert!(cancelled(&d.state, a, dropped), "Stop gives it up");

    let timing = d.state.runtime.diagnostics().timing;
    println!("[{:8.3}] {timing:?}", d.state.secs());
    assert!(timing.total_paint > Duration::ZERO && timing.max_swap > Duration::ZERO);
    assert!(
        timing.first_frame.is_some(),
        "navigate to first frame is timed"
    );
}

#[test]
#[ignore = "needs a compositor: run tests/headless.sh"]
fn servo_views_navigate_present_and_fail_distinctly() {
    let profile = std::env::temp_dir().join(format!("sighurt-m4-profile-{}", std::process::id()));
    let config = ServoConfig {
        profile: Some(profile.clone()),
        audit_gl: true,
    };
    let mut d = Driver::new(config, Layout::Stack);
    assert_eq!(
        d.state.runtime.diagnostics().profile.as_deref(),
        Some(&*profile)
    );

    // A page loads, commits and presents.
    let a = d.state.open(&page("history.html"), true);
    assert!(
        d.run_until(WAIT, |s| s.submitted(a) > 0 && commit(s, a).is_some()),
        "the first page loads and presents"
    );
    assert!(d
        .state
        .events_of(a)
        .any(|e| *e == EngineEvent::ViewReady { view: a }));
    let (first, url, doc) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("history.html"), "{url}");
    assert!(
        d.state
            .events_of(a)
            .any(|e| matches!(e, EngineEvent::NavigationStarted { navigation, .. } if *navigation == first)),
        "the commit carries the browser's navigation id"
    );

    // Same-document entries: a fragment and pushState, in the same document, the page's ids.
    assert!(
        d.run_until(WAIT, |s| history(s, a).is_some_and(|(e, _)| e.len() == 3)),
        "the page adds two same-document entries"
    );
    let (entries, current) = history(&d.state, a).unwrap();
    assert_eq!(current, 2);
    // Servo 0.5 names only the current entry exactly: its projection gives an entry before a
    // `pushState` the URL after it (`notify_history_changed` doesn't resolve pipeline diffs).
    assert!(entries[2].ends_with("#two"), "{entries:?}");
    let (own, url, same) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("#two"));
    assert_eq!(same, doc, "pushState keeps the document");
    assert_ne!(own, first, "the page's own navigation has its own id");

    // A cross-document navigation: a new document and a fourth entry.
    let to_second = d.state.navigate(a, &page("second.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, to_second)));
    let (_, _, second_doc) = commit(&d.state, a).unwrap();
    assert_ne!(second_doc, doc);
    assert!(d.run_until(WAIT, |s| history(s, a)
        .is_some_and(|(e, c)| e.len() == 4 && c == 3)));

    // Back into the first document, at #two: another document than the second page, shown
    // again with a new generation.
    let back = d.state.traverse(a, -1);
    assert!(d.run_until(WAIT, |s| committed(s, a, back)), "back commits");
    let (_, url, back_doc) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("#two"), "{url}");
    assert_ne!(back_doc, second_doc);
    // Back again, to #one: the same document.
    let back = d.state.traverse(a, -1);
    assert!(d.run_until(WAIT, |s| committed(s, a, back)));
    let (_, url, one_doc) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("#one"), "{url}");
    assert_eq!(
        one_doc, back_doc,
        "a same-document traversal keeps the document"
    );
    // Forward by two, to the second page: another document again.
    let forward = d.state.traverse(a, 2);
    assert!(d.run_until(WAIT, |s| committed(s, a, forward)));
    let (_, url, forward_doc) = commit(&d.state, a).unwrap();
    assert!(url.ends_with("second.html"), "{url}");
    assert_ne!(forward_doc, one_doc);
    // Beyond the end: cancelled, not sent to Servo.
    let beyond = d.state.traverse(a, 1);
    d.run_for(Duration::from_millis(200));
    assert!(d.state.events_of(a).any(|e| matches!(
        e,
        EngineEvent::NavigationFailed { navigation, error: NavigationError::Cancelled, .. }
            if *navigation == beyond
    )));

    // Reload: a new document in the same entry.
    let reload = NavigationId::next();
    d.state
        .command(a, EngineCommand::Reload { navigation: reload });
    assert!(
        d.run_until(WAIT, |s| committed(s, a, reload)),
        "reload commits"
    );
    assert_ne!(commit(&d.state, a).unwrap().2, forward_doc);
    assert_eq!(
        history(&d.state, a).unwrap().0.len(),
        4,
        "a reload adds no entry"
    );

    // Two navigations in a row: Servo 0.5 drops a load asked for while another is in flight,
    // so the second waits for the first's document, then loads and commits with its own id.
    let first_of_two = d.state.navigate(a, &page("history.html"));
    let second_of_two = d.state.navigate(a, &page("second.html"));
    assert!(d.run_until(WAIT, |s| committed(s, a, second_of_two)));
    assert!(
        committed(&d.state, a, first_of_two),
        "the first commits first"
    );
    assert!(commit(&d.state, a).unwrap().1.ends_with("second.html"));

    navigations_servo_would_drop_or_misattribute(&mut d, a);

    // Failed navigations. An address that isn't one fails the navigation, nothing else.
    let invalid = d.state.navigate(a, "http://[");
    d.run_for(Duration::from_millis(200));
    assert!(d.state.events_of(a).any(|e| matches!(
        e,
        EngineEvent::NavigationFailed { navigation, error: NavigationError::Load(_), .. }
            if *navigation == invalid
    )));
    // One that can't be reached commits Servo's error page: a failed load, not a failed view.
    let unreachable = d.state.navigate(a, "http://127.0.0.1:9/");
    assert!(d.run_until(WAIT, |s| committed(s, a, unreachable)));
    let failures = |e: &EngineEvent| {
        matches!(
            e,
            EngineEvent::ViewFailed { .. }
                | EngineEvent::ViewClosed { .. }
                | EngineEvent::RuntimeFailed { .. }
        )
    };
    assert_eq!(count(&d.state, a, failures), 0);

    // A hidden view (a background tab) loads but presents nothing.
    let b = d.state.open(&page("second.html"), false);
    assert!(
        d.run_until(WAIT, |s| commit(s, b).is_some()),
        "a hidden view loads"
    );
    d.run_for(Duration::from_millis(500));
    assert_eq!(d.state.submitted(b), 0, "a hidden view presents nothing");
    // Switching tabs: a is hidden and stops presenting, b shows at once.
    let a_frames = d.state.submitted(a);
    d.state.set_visible(a, false);
    d.state.set_visible(b, true);
    d.state.set_focus(Some(b));
    assert!(
        d.run_until(WAIT, |s| s.submitted(b) > 0),
        "a shown view presents"
    );
    d.run_for(Duration::from_millis(500));
    assert_eq!(
        d.state.submitted(a),
        a_frames,
        "a hidden view stops presenting"
    );
    // And back, several times: each show presents without waiting on a callback.
    for _ in 0..3 {
        let (a_before, b_before) = (d.state.submitted(a), d.state.submitted(b));
        d.state.set_visible(b, false);
        d.state.set_visible(a, true);
        assert!(d.run_until(WAIT, |s| s.submitted(a) > a_before));
        d.state.set_visible(a, false);
        d.state.set_visible(b, true);
        assert!(d.run_until(WAIT, |s| s.submitted(b) > b_before));
    }

    // Closing a view.
    let closed = |e: &EngineEvent| matches!(e, EngineEvent::ViewClosed { .. });
    d.state.close(b);
    assert!(d.run_until(WAIT, |s| s.events_of(b).any(closed)));
    assert!(d.state.runtime.diagnostics().views == 1);

    let diagnostics = d.state.runtime.diagnostics();
    println!("{diagnostics:?}");
    assert!(diagnostics.gl.is_some());
    assert_eq!(diagnostics.read_to_image, 0, "no page readback");
    assert_eq!(diagnostics.gl_reads, Some(0), "no GL read");
    let host = d.state.host.diagnostics();
    println!("{host:?}");

    // The runtime holds the profile's lock until Servo has shut down.
    let lock = std::fs::File::open(profile.join("lock")).unwrap();
    assert!(
        lock.try_lock().is_err(),
        "the profile is locked while Servo runs"
    );
    let state = d.shutdown();
    assert!(!state.host.is_lost(), "no protocol error");
    assert_eq!(state.runtime.diagnostics().views, 0);
    assert!(!state.runtime.diagnostics().leaked, "Servo shut down");
    lock.try_lock()
        .expect("the profile is free once Servo has shut down");
    std::fs::remove_dir_all(&profile).unwrap();
}
