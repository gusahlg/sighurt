//! Bytes copied and time spent in `present` for a small blinking square.
//!
//! Ignored: it starts Chromium. One process is one repetition. Prints
//! `cef-copy: bytes_median=N time_us_median=N samples=N full=N`.
//! The square sits off the present-buffer probe pixels, so a trusted buffer
//! is asked only for the damaged rectangle.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_engine_cef::{CefConfig, CefRuntime};

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;
/// Frames measured after the first paints have filled the buffer.
const SAMPLES: usize = 30;
const HTML: &str = r#"<!doctype html>
<meta charset="utf-8">
<title>blink</title>
<style>
  html,body { margin:0; background:#fff; }
  /* A 24px square, off the present-buffer probe pixels, blinking. A transform
     spinner often dirties a whole layer; a colour change stays in the box. */
  #s {
    position:absolute; left:48px; top:16px; width:24px; height:24px;
    background:#c0392b;
    animation: blink 0.2s steps(2, end) infinite;
  }
  @keyframes blink { 50% { background:#2980b9; } }
</style>
<div id="s"></div>
"#;

struct Permit {
    epoch: LayoutEpoch,
    size: BufferSize,
}

struct Mem {
    metrics: Metrics,
    buffer: Vec<u8>,
}

impl CpuTarget for Mem {
    type Permit = Permit;

    fn metrics(&self) -> Metrics {
        self.metrics
    }

    fn permit_epoch(permit: &Permit) -> LayoutEpoch {
        permit.epoch
    }

    fn submit(
        &mut self,
        permit: Permit,
        paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        let stride = 4 * permit.size.width as usize;
        let len = stride * permit.size.height as usize;
        if self.buffer.len() != len {
            self.buffer = vec![0u8; len];
        }
        paint(Frame {
            pixels: &mut self.buffer,
            size: permit.size,
            stride,
        })
    }
}

fn metrics() -> Metrics {
    Metrics::new(
        LayoutEpoch::next(),
        WindowRect {
            x: 0,
            y: 0,
            width: WIDTH,
            height: HEIGHT,
        },
        Scale::ONE,
    )
}

fn median(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    let mid = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[mid - 1] + values[mid]) / 2
    } else {
        values[mid]
    }
}

#[test]
#[ignore = "starts Chromium; one repetition of the present-copy measurement"]
fn spinner_present_copy() {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cef-copybench");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).expect("scratch");
    let page = scratch.join("spin.html");
    std::fs::write(&page, HTML).expect("page");
    let mut config = CefConfig::installed().expect("CEF is installed");
    config.scratch = Some(scratch);
    let waker = EngineWaker::new(|| {});
    let mut rt = CefRuntime::<Mem>::new(config, waker).expect("CEF starts");
    let view = ViewId::next();
    let geometry = metrics();
    rt.open_view(
        view,
        ViewOptions::default(),
        Mem {
            metrics: geometry,
            buffer: Vec::new(),
        },
    )
    .expect("open_view");
    let mut events = Vec::new();
    let end = Instant::now() + Duration::from_secs(30);
    loop {
        rt.pump(&mut events);
        if events
            .iter()
            .any(|e| matches!(e, EngineEvent::ViewReady { .. }))
        {
            break;
        }
        assert!(Instant::now() < end, "no ViewReady");
        std::thread::sleep(Duration::from_millis(5));
    }
    rt.command(
        view,
        EngineCommand::Navigate {
            navigation: NavigationId::next(),
            url: format!("file://{}", page.display()),
        },
    )
    .expect("navigate");
    let permit = || Permit {
        epoch: geometry.epoch(),
        size: geometry.buffer(),
    };
    let mut bytes = Vec::new();
    let mut times = Vec::new();
    let mut seen = 0u32;
    let deadline = Instant::now() + Duration::from_secs(20);
    while bytes.len() < SAMPLES && Instant::now() < deadline {
        rt.pump(&mut events);
        let before = rt.diagnostics().presented_bytes;
        let started = Instant::now();
        let outcome = rt.present(view, permit()).expect("present");
        let elapsed = started.elapsed().as_micros();
        if matches!(outcome, PresentOutcome::Submitted) {
            seen += 1;
            // The first paints fill the buffer. Both trees copy those whole.
            if seen <= 5 {
                continue;
            }
            bytes.push(u128::from(rt.diagnostics().presented_bytes - before));
            times.push(elapsed);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert!(
        bytes.len() >= SAMPLES,
        "only {} animation frames",
        bytes.len()
    );
    let full = u128::from(WIDTH) * u128::from(HEIGHT) * 4;
    eprintln!(
        "cef-copy: bytes_median={} time_us_median={} samples={} full={}",
        median(&mut bytes),
        median(&mut times),
        bytes.len(),
        full
    );
    rt.close_view(view);
    let end = Instant::now() + Duration::from_secs(8);
    while Instant::now() < end {
        events.clear();
        rt.pump(&mut events);
        if events
            .iter()
            .any(|e| matches!(e, EngineEvent::ViewClosed { .. }))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    rt.shutdown();
}
