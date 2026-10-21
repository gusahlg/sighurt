//! Servo presenting into an in-memory [`CpuTarget`] with no display server.
//!
//! One test: Servo initializes once per process. It checks that frames arrive only for a
//! current permit, that the pixels are ARGB8888 top-down, that `SetMetrics` changes the frame
//! size, that each readback is counted, and that shutdown does not leak Servo.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_engine_servo::{Offscreen, ServoConfig, ServoRuntime};

/// Header `#e67e22` as B, G, R, A.
const ORANGE: [u8; 4] = [0x22, 0x7e, 0xe6, 0xff];
/// Body `#1b4f72`.
const BLUE: [u8; 4] = [0x72, 0x4f, 0x1b, 0xff];
/// The box `#00ff00`.
const GREEN: [u8; 4] = [0x00, 0xff, 0x00, 0xff];
/// The corner mark `#ff00ff`.
const MAGENTA: [u8; 4] = [0xff, 0x00, 0xff, 0xff];

const LOAD_WAIT: Duration = Duration::from_secs(90);
const COLOUR_WAIT: Duration = Duration::from_secs(30);

struct Stored {
    epoch: LayoutEpoch,
    size: BufferSize,
    stride: usize,
    pixels: Vec<u8>,
}

struct Mem {
    metrics: Metrics,
    frames: Arc<Mutex<Vec<Stored>>>,
}

struct Permit {
    epoch: LayoutEpoch,
    size: BufferSize,
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
        let stride = padded_stride(permit.size.width);
        let mut pixels = vec![0u8; stride * permit.size.height as usize];
        paint(Frame {
            pixels: &mut pixels,
            size: permit.size,
            stride,
        })?;
        self.frames.lock().expect("frames").push(Stored {
            epoch: permit.epoch,
            size: permit.size,
            stride,
            pixels,
        });
        Ok(())
    }
}

/// At least 16 bytes of padding past a tightly packed row, so `PACK_ROW_LENGTH` is not zero.
fn padded_stride(width: u32) -> usize {
    let row = 4 * width as usize;
    let aligned = row.div_ceil(16) * 16;
    if aligned == row {
        aligned + 16
    } else {
        aligned
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

fn sample(frame: &Stored, x: u32, y: u32) -> [u8; 4] {
    assert!(
        x < frame.size.width && y < frame.size.height,
        "({x},{y}) outside {}×{}",
        frame.size.width,
        frame.size.height
    );
    let i = y as usize * frame.stride + x as usize * 4;
    frame.pixels[i..i + 4].try_into().expect("pixel")
}

/// The static page: orange header, blue body, green box, magenta corner. The top row is the
/// header, so a bottom-up framebuffer fails here.
fn page_colours(frame: &Stored) -> Result<(), String> {
    let (w, h) = (frame.size.width, frame.size.height);
    let checks = [
        ((4, 0), ORANGE, "top row"),
        ((4, 4), ORANGE, "header"),
        ((w / 2, 8), ORANGE, "header middle"),
        ((w / 2, 70), BLUE, "body"),
        ((0, h - 1), BLUE, "bottom left"),
        ((100, 100), GREEN, "box"),
        ((w - 4, h - 4), MAGENTA, "corner"),
    ];
    for ((x, y), expected, name) in checks {
        let got = sample(frame, x, y);
        if got != expected {
            return Err(format!(
                "{name} at ({x},{y}) on {}×{}: got {got:02x?}, expected {expected:02x?}",
                w, h
            ));
        }
    }
    Ok(())
}

fn env_pair() -> (Option<OsString>, Option<OsString>) {
    (
        std::env::var_os("LIBGL_ALWAYS_SOFTWARE"),
        std::env::var_os("DRI_PRIME"),
    )
}

fn print_env(label: &str) {
    println!(
        "env {label}: LIBGL_ALWAYS_SOFTWARE={:?} DRI_PRIME={:?} WAYLAND_DISPLAY={:?} DISPLAY={:?}",
        std::env::var_os("LIBGL_ALWAYS_SOFTWARE"),
        std::env::var_os("DRI_PRIME"),
        std::env::var_os("WAYLAND_DISPLAY"),
        std::env::var_os("DISPLAY"),
    );
}

fn print_timing(label: &str, runtime: &ServoRuntime<Offscreen<Mem>>) {
    let d = runtime.diagnostics();
    let gl = d.gl.as_ref();
    println!(
        "{label}: frames={} read_to_image={} gl_reads={:?} adapter={:?} software={:?} \
         renderer={:?} egl={:?} paint_ms={:.3} readback_ms={:.3} max_paint_ms={:.3} \
         max_readback_ms={:.3}",
        d.frames,
        d.read_to_image,
        d.gl_reads,
        d.offscreen_adapter,
        gl.map(|info| info.software),
        gl.map(|info| info.renderer.as_str()),
        gl.map(|info| info.egl.as_str()),
        d.timing.paint.as_secs_f64() * 1e3,
        d.timing.swap.as_secs_f64() * 1e3,
        d.timing.max_paint.as_secs_f64() * 1e3,
        d.timing.max_swap.as_secs_f64() * 1e3,
    );
}

struct Seen {
    committed: bool,
    /// `None` until the view reports it.
    loading: Option<bool>,
    frame_ready: bool,
    recent: Vec<String>,
}

fn note(seen: &mut Seen, event: &EngineEvent) {
    match event {
        EngineEvent::NavigationCommitted { .. } => seen.committed = true,
        EngineEvent::LoadingChanged { loading, .. } => seen.loading = Some(*loading),
        EngineEvent::FrameReady { .. } => seen.frame_ready = true,
        _ => {}
    }
    seen.recent.push(format!("{event:?}"));
    if seen.recent.len() > 20 {
        seen.recent.remove(0);
    }
}

fn pump_until(
    runtime: &mut ServoRuntime<Offscreen<Mem>>,
    seen: &mut Seen,
    timeout: Duration,
    done: impl Fn(&Seen) -> bool,
) -> Result<(), String> {
    let start = Instant::now();
    while start.elapsed() < timeout {
        let mut events = Vec::new();
        let status = runtime.pump(&mut events);
        for event in &events {
            note(seen, event);
        }
        if done(seen) {
            return Ok(());
        }
        match status {
            PumpStatus::Idle => std::thread::sleep(Duration::from_millis(10)),
            PumpStatus::Pending { deadline: None } => {}
            PumpStatus::Pending {
                deadline: Some(deadline),
            } => {
                let wait = deadline.saturating_duration_since(Instant::now());
                std::thread::sleep(wait.min(Duration::from_millis(10)));
            }
        }
    }
    Err(format!("timed out; last events: {:?}", seen.recent))
}

fn present_at(
    runtime: &mut ServoRuntime<Offscreen<Mem>>,
    view: ViewId,
    epoch: LayoutEpoch,
    size: BufferSize,
) -> PresentOutcome {
    runtime
        .present(view, Permit { epoch, size })
        .unwrap_or_else(|error| panic!("present: {error:?}"))
}

/// Presents until the stored frame is `size` and the page's colours, pumping between attempts.
fn present_until_page(
    runtime: &mut ServoRuntime<Offscreen<Mem>>,
    seen: &mut Seen,
    view: ViewId,
    epoch: LayoutEpoch,
    size: BufferSize,
    frames: &Mutex<Vec<Stored>>,
) {
    let start = Instant::now();
    let mut last = String::from("no submitted frame");
    loop {
        let outcome = present_at(runtime, view, epoch, size);
        if outcome == PresentOutcome::Submitted {
            let stored = frames.lock().expect("frames");
            let frame = stored.last().expect("a submitted frame");
            assert_eq!(frame.epoch, epoch, "submitted frame epoch");
            assert_eq!(frame.size, size, "submitted frame size");
            assert!(frame.stride > 4 * size.width as usize, "padded stride");
            match page_colours(frame) {
                Ok(()) => return,
                Err(error) => last = error,
            }
        }
        if start.elapsed() > COLOUR_WAIT {
            panic!("page colours did not arrive ({last}); last outcome {outcome:?}");
        }
        let mut events = Vec::new();
        let status = runtime.pump(&mut events);
        for event in &events {
            note(seen, event);
        }
        if !matches!(status, PumpStatus::Pending { deadline: None }) {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn offscreen_view_presents_argb_frames_without_a_display() {
    std::env::remove_var("WAYLAND_DISPLAY");
    std::env::remove_var("DISPLAY");
    assert!(std::env::var_os("WAYLAND_DISPLAY").is_none());
    assert!(std::env::var_os("DISPLAY").is_none());
    let before = env_pair();
    print_env("before context");

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let mut runtime =
        ServoRuntime::<Offscreen<Mem>>::new(ServoConfig::default(), EngineWaker::new(|| {}));
    runtime.setup_logging();

    let frames = Arc::new(Mutex::new(Vec::new()));
    let open_metrics = metrics(320, 180);
    let epoch = open_metrics.epoch();
    let size = open_metrics.buffer();
    let view = ViewId::next();
    runtime
        .open_view(
            view,
            ViewOptions::default(),
            Offscreen(Mem {
                metrics: open_metrics,
                frames: Arc::clone(&frames),
            }),
        )
        .unwrap_or_else(|error| panic!("open_view: {error:?}"));
    print_env("after context");
    assert_eq!(
        env_pair(),
        before,
        "context creation restored the environment"
    );

    assert_eq!(
        present_at(&mut runtime, view, epoch, size),
        PresentOutcome::NotReady,
        "no webview yet"
    );
    assert!(
        frames.lock().expect("frames").is_empty(),
        "NotReady submits nothing"
    );

    let page = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pages/offscreen.html");
    let url = url::Url::from_file_path(&page).expect("file url");
    let navigation = NavigationId::next();
    runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation,
                url: url.to_string(),
            },
        )
        .unwrap_or_else(|error| panic!("navigate: {error:?}"));

    let mut seen = Seen {
        committed: false,
        loading: None,
        frame_ready: false,
        recent: Vec::new(),
    };
    pump_until(&mut runtime, &mut seen, LOAD_WAIT, |seen| {
        seen.committed && seen.loading == Some(false) && seen.frame_ready
    })
    .unwrap_or_else(|error| panic!("{error}"));

    let stale = LayoutEpoch::next();
    assert_eq!(
        present_at(&mut runtime, view, stale, size),
        PresentOutcome::NotReady,
        "a permit for another epoch"
    );
    assert!(
        frames.lock().expect("frames").is_empty(),
        "a stale permit submits nothing"
    );

    present_until_page(&mut runtime, &mut seen, view, epoch, size, &frames);
    print_timing("first page", &runtime);
    let submitted = frames.lock().expect("frames").len();
    let mut settled = false;
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        let before_len = frames.lock().expect("frames").len();
        let outcome = present_at(&mut runtime, view, epoch, size);
        outcomes.push(outcome);
        if outcome == PresentOutcome::Unchanged {
            assert_eq!(
                frames.lock().expect("frames").len(),
                before_len,
                "Unchanged submits nothing"
            );
            settled = true;
            break;
        }
        assert_eq!(outcome, PresentOutcome::Submitted, "{outcomes:?}");
    }
    assert!(
        settled,
        "a static page settles to Unchanged, got {outcomes:?}"
    );

    let resized = metrics(400, 240);
    let new_epoch = resized.epoch();
    let new_size = resized.buffer();
    runtime
        .command(view, EngineCommand::SetMetrics(resized))
        .unwrap_or_else(|error| panic!("set metrics: {error:?}"));
    let before_resize = frames.lock().expect("frames").len();
    assert_eq!(
        present_at(&mut runtime, view, epoch, size),
        PresentOutcome::NotReady,
        "the old epoch after SetMetrics"
    );
    assert_eq!(
        frames.lock().expect("frames").len(),
        before_resize,
        "the old epoch submits nothing"
    );
    present_until_page(&mut runtime, &mut seen, view, new_epoch, new_size, &frames);
    print_timing("after resize", &runtime);

    let diagnostics = runtime.diagnostics();
    let stored = frames.lock().expect("frames").len() as u64;
    assert_eq!(diagnostics.read_to_image, diagnostics.frames);
    assert_eq!(
        diagnostics.frames, stored,
        "one counted readback per submitted frame"
    );
    assert!(
        stored > submitted as u64,
        "the resize submitted another frame"
    );
    let gl = diagnostics.gl.expect("GL info");
    assert!(!gl.renderer.is_empty(), "renderer string");
    assert!(
        diagnostics.offscreen_adapter.is_some(),
        "offscreen adapter was recorded"
    );
    assert!(
        diagnostics.gl_reads.is_none(),
        "audit is off, so GL reads are not counted"
    );
    print_env("after presents");
    assert_eq!(env_pair(), before, "presents left the environment restored");

    runtime.shutdown();
    assert!(
        !runtime.diagnostics().leaked,
        "a clean page shuts Servo down"
    );
}
