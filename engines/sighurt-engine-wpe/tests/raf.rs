//! How fast WebKit runs `requestAnimationFrame` on the output the window is on, and what a page
//! sees in `screen.*`: one animating page, its callback intervals measured in the page with
//! `performance.now()`; in a sway (`SWAYSOCK` set), the output's rate changed to 120 Hz and to
//! 60 Hz while the page asks for frames and paints nothing, so no frame is presented, to show
//! that the vblank observer follows the output without one, and then painting again, to show
//! that the page's animation frames follow it; then a page that animates nothing, to show
//! that the platform's vblank observer is stopped while no page asks for frames; then animating
//! pages opened and closed again and again, the last one each time (WebKit's web context, its
//! process pool and its display links go with it), while their callbacks tick.
//!
//! Needs a compositor and is `#[ignore]`d. Run it in a private sway (see `tests/headless.sh`
//! for the steps; `swaymsg output HEADLESS-1 mode 1280x800@120Hz` first picks the output's
//! rate). `SIG_RAF_EXPECT_HZ=<n>` asserts the median interval is within 20 % of 1000/n ms.

mod common;

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use common::{Driver, Layout};
use sighurt_engine_wpe::WpeConfig;

const WAIT: Duration = Duration::from_secs(20);

/// The median of a `window.report()`.
fn median(report: &str) -> f64 {
    report
        .split("\"median\":")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.parse().ok())
        .expect("a median")
}

/// `swaymsg` with `args`; its output.
fn swaymsg(args: &[&str]) -> String {
    let out = Command::new("swaymsg")
        .args(args)
        .output()
        .expect("swaymsg");
    assert!(out.status.success(), "swaymsg {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The number after `"key": ` in `json`, from `at` on.
fn number(json: &str, key: &str) -> u32 {
    let at = json.find(&format!("\"{key}\": ")).expect(key) + key.len() + 4;
    json[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()
        .and_then(|n| n.parse().ok())
        .expect(key)
}

/// The first output's name and current size (`WxH`), from `swaymsg -t get_outputs` (JSON
/// when its output is not a terminal).
fn output() -> (String, String) {
    let json = swaymsg(&["-t", "get_outputs", "-r"]);
    let name = json
        .split("\"name\": \"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .expect("an output")
        .to_owned();
    let mode = &json[json.find("\"current_mode\"").expect("a mode")..];
    let size = format!("{}x{}", number(mode, "width"), number(mode, "height"));
    (name, size)
}

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
        let median = median(&report);
        let want = 1000.0 / hz;
        assert!(
            (median - want).abs() < want * 0.2,
            "median {median} ms, expected about {want} ms"
        );
    }

    // The output's rate changes while the page asks for frames and paints nothing: no frame
    // reaches the runtime's present, and the observer ticks at the new rate all the same (the
    // runtime reads the lease's rate at every pump). Then the page paints again, and its
    // animation frames come at the new rate: WebKit is told 60 Hz whatever the rate, so every
    // tick is a rendering update (`crate::screen`).
    if std::env::var_os("SWAYSOCK").is_some() {
        let (name, size) = output();
        for hz in [120u32, 60] {
            d.eval(a, "window.still(true)").unwrap();
            d.run_for(Duration::from_secs(1));
            let submitted = d.state.submitted(a);
            // One argument: swaymsg would take `--custom` for an option of its own.
            swaymsg(&[&format!("output {name} mode --custom {size}@{hz}Hz")]);
            d.run_for(Duration::from_secs(1));
            let diag = d.state.runtime.diagnostics();
            let painted = d.state.submitted(a) - submitted;
            println!(
                "OUTPUT {hz} Hz: observer at {:?} mHz, {:?}, {painted} frames presented",
                diag.refresh_mhz, diag.vblank
            );
            assert_eq!(painted, 0, "the page painted nothing");
            assert_eq!(
                diag.refresh_mhz,
                Some(hz * 1000),
                "the observer follows the output"
            );
            d.eval(a, "window.measure(true)").unwrap();
            d.run_for(Duration::from_secs(2));
            let still = d.eval(a, "window.report()").unwrap();
            d.eval(a, "window.still(false)").unwrap();
            d.eval(a, "window.measure(true)").unwrap();
            d.run_for(Duration::from_secs(3));
            let report = d.eval(a, "window.report()").unwrap();
            println!("OUTPUT {hz} Hz: RAF still {still}, painting {report}");
            let want = 1000.0 / f64::from(hz);
            let median = median(&report);
            assert!(
                (median - want).abs() < want * 0.2,
                "at {hz} Hz: median {median} ms, expected about {want} ms"
            );
        }
    }

    // No animation: the page stops asking for frames, and nothing ticks.
    d.eval(a, "window.stop()").unwrap();
    d.run_for(Duration::from_secs(2));
    let before = d.state.runtime.diagnostics().vblank;
    d.run_for(Duration::from_secs(3));
    let after = d.state.runtime.diagnostics().vblank;
    println!("IDLE before {before:?} after {after:?}");
    assert_eq!(before.ticks, after.ticks, "an idle page gets no ticks");
    assert!(!after.running, "the vblank observer is stopped when idle");

    // The last view closes while it animates, and another opens: WebKit destroys the closed
    // context's display links (and their monitors, whose callbacks the observer runs) on its
    // main thread, where the ticks run too, and the next view's display link starts the
    // observer again.
    d.state.close(a);
    for round in 0..8 {
        let before = d.state.runtime.diagnostics().vblank;
        let b = d.state.open(&page("raf.html"), true);
        assert!(
            d.run_until(WAIT, |s| s.submitted(b) > 0),
            "round {round}: the page presents"
        );
        d.run_for(Duration::from_millis(300));
        let during = d.state.runtime.diagnostics().vblank;
        assert!(during.ticks > before.ticks, "round {round}: it ticks");
        d.state.close(b);
        // Teardown and ticks interleave in the owner's GLib dispatch from here.
        d.run_for(Duration::from_millis(round * 60));
    }
    d.run_for(Duration::from_secs(2));
    let end = d.state.runtime.diagnostics().vblank;
    println!("CHURN {end:?}");
    assert!(!end.running, "no page left, no ticks");
    d.shutdown();
}
