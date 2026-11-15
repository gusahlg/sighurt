//! How fast WebKit runs `requestAnimationFrame` on the output the window is on, and what a page
//! sees in `screen.*`: one animating page, its callback intervals measured in the page with
//! `performance.now()`, then a page that animates nothing, to show that the platform's vblank
//! thread is stopped while no page asks for frames.
//!
//! Needs a compositor and is `#[ignore]`d. Run it in a private sway (see `tests/headless.sh`
//! for the steps; `swaymsg output HEADLESS-1 mode 1280x800@120Hz` first picks the output's
//! rate). `SIG_RAF_EXPECT_HZ=<n>` asserts the median interval is within 20 % of 1000/n ms.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{Driver, Layout};
use sighurt_engine_wpe::WpeConfig;

const WAIT: Duration = Duration::from_secs(20);

fn page(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pages")
        .join(name);
    url::Url::from_file_path(path).unwrap().to_string()
}

#[test]
#[ignore = "needs a compositor: run in a private sway"]
fn animation_frames_follow_the_output() {
    let mut d = Driver::new(WpeConfig::default(), Layout::Stack);
    let a = d.state.open(&page("raf.html"), true);
    assert!(
        d.run_until(WAIT, |s| s.submitted(a) > 0),
        "the page presents"
    );
    d.run_for(Duration::from_secs(2));
    println!("SCREEN {}", d.eval(a, "window.screenInfo()").unwrap());
    d.eval(a, "window.measure(true)").unwrap();
    d.run_for(Duration::from_secs(5));
    let report = d.eval(a, "window.report()").unwrap();
    println!("RAF {report}");
    println!("DIAG {:?}", d.state.runtime.diagnostics().vblank);
    if let Ok(hz) = std::env::var("SIG_RAF_EXPECT_HZ") {
        let hz: f64 = hz.parse().unwrap();
        let median: f64 = report
            .split("\"median\":")
            .nth(1)
            .and_then(|s| s.split(',').next())
            .and_then(|s| s.parse().ok())
            .expect("a median");
        let want = 1000.0 / hz;
        assert!(
            (median - want).abs() < want * 0.2,
            "median {median} ms, expected about {want} ms"
        );
    }

    // No animation: the page stops asking for frames, and nothing ticks.
    d.eval(a, "window.stop()").unwrap();
    d.run_for(Duration::from_secs(2));
    let before = d.state.runtime.diagnostics().vblank;
    d.run_for(Duration::from_secs(3));
    let after = d.state.runtime.diagnostics().vblank;
    println!("IDLE before {before:?} after {after:?}");
    assert_eq!(before.ticks, after.ticks, "an idle page gets no ticks");
    assert!(!after.running, "the vblank thread is stopped when idle");
    d.shutdown();
}
