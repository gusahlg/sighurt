//! WebKit presenting into an in-memory [`CpuTarget`] with no display server.
//!
//! One test: WebKit's UI-process state is once per process. Ignored so `cargo test` does not
//! start WebKit; `tests/offscreen.sh` runs it on a private D-Bus session with no display.
//!
//! The page animates a square, so WebKit always has a next frame. It checks that a frame is
//! copied only for a current permit, that the pixels are ARGB8888 top-down, that `SetMetrics`
//! changes the frame size, that withholding permits leaves at most one frame unacknowledged,
//! how fast frames come when permits are not withheld, that the web process does not inherit
//! `LIBGL_ALWAYS_SOFTWARE` or `DRI_PRIME` when they are unset here, and that shutdown leaves no
//! web or network process behind.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, ViewOptions};
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_api::present::{CpuTarget, Frame};
use sighurt_engine_wpe::{Diagnostics, Offscreen, Transport, WpeConfig, WpeRuntime};

/// Header `#e67e22` as B, G, R, A.
const ORANGE: [u8; 4] = [0x22, 0x7e, 0xe6, 0xff];
/// Body `#1b4f72`.
const BLUE: [u8; 4] = [0x72, 0x4f, 0x1b, 0xff];
/// The box `#00ff00`.
const GREEN: [u8; 4] = [0x00, 0xff, 0x00, 0xff];
/// The corner mark `#ff00ff`.
const MAGENTA: [u8; 4] = [0xff, 0x00, 0xff, 0xff];

const COLOUR_WAIT: Duration = Duration::from_secs(40);
const HOLD: Duration = Duration::from_millis(1500);

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
        assert!(
            padding_untouched(&pixels, permit.size, stride),
            "the copy wrote past the row"
        );
        self.frames.lock().expect("frames").push(Stored {
            epoch: permit.epoch,
            size: permit.size,
            stride,
            pixels,
        });
        Ok(())
    }
}

/// At least 16 bytes of padding past a tightly packed row.
fn padded_stride(width: u32) -> usize {
    let row = 4 * width as usize;
    let aligned = row.div_ceil(16) * 16;
    if aligned == row {
        aligned + 16
    } else {
        aligned
    }
}

fn padding_untouched(pixels: &[u8], size: BufferSize, stride: usize) -> bool {
    let row = 4 * size.width as usize;
    (0..size.height as usize).all(|y| {
        pixels[y * stride + row..(y + 1) * stride]
            .iter()
            .all(|b| *b == 0)
    })
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
    let i = y as usize * frame.stride + x as usize * 4;
    frame.pixels[i..i + 4].try_into().expect("pixel")
}

/// Orange header, blue body, green box, magenta corner. The top row is the header, so a
/// bottom-up buffer fails here.
fn page_colours(frame: &Stored) -> Result<(), String> {
    let (w, h) = (frame.size.width, frame.size.height);
    let checks = [
        ((4, 0), ORANGE, "top row"),
        ((4, 8), ORANGE, "header"),
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
                "{name} at ({x},{y}) on {w}×{h}: got {got:02x?}, expected {expected:02x?}",
            ));
        }
    }
    Ok(())
}

fn present_at(
    runtime: &mut WpeRuntime<Offscreen<Mem>>,
    view: ViewId,
    epoch: LayoutEpoch,
    size: BufferSize,
) -> PresentOutcome {
    runtime
        .present(view, Permit { epoch, size })
        .unwrap_or_else(|error| panic!("present: {error}"))
}

fn pump(runtime: &mut WpeRuntime<Offscreen<Mem>>, for_how_long: Duration) -> Vec<String> {
    let start = Instant::now();
    let mut recent = Vec::new();
    while start.elapsed() < for_how_long {
        let mut events = Vec::new();
        runtime.pump(&mut events);
        for event in events {
            recent.push(format!("{event:?}"));
            if recent.len() > 12 {
                recent.remove(0);
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    recent
}

fn present_until_colours(
    runtime: &mut WpeRuntime<Offscreen<Mem>>,
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
            assert_eq!(frame.size, size, "submitted frame size");
            assert_eq!(frame.epoch, epoch, "submitted frame epoch");
            match page_colours(frame) {
                Ok(()) => return,
                Err(error) => last = error,
            }
        }
        if start.elapsed() > COLOUR_WAIT {
            let diagnostics = runtime.diagnostics();
            panic!(
                "page colours did not arrive ({last}); last outcome {outcome:?}; \
                 produced={} copies={} stale={} transport={:?}\n{:?}",
                diagnostics.produced,
                diagnostics.present.shm_copies,
                diagnostics.stale_frames,
                diagnostics.transport,
                frames
                    .lock()
                    .expect("frames")
                    .last()
                    .map(|frame| sample(frame, 4, 0))
            );
        }
        let mut events = Vec::new();
        runtime.pump(&mut events);
        let _ = events;
        std::thread::sleep(Duration::from_millis(10));
    }
}

struct Proc {
    pid: u32,
    ppid: u32,
    comm: String,
    cmdline: String,
    state: char,
}

fn processes() -> Vec<Proc> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let Some((ppid, state)) = stat_ppid_state(&stat) else {
            continue;
        };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = String::from_utf8_lossy(
            &std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default(),
        )
        .replace('\0', " ");
        out.push(Proc {
            pid,
            ppid,
            comm,
            cmdline: cmdline.trim().to_string(),
            state,
        });
    }
    out
}

fn stat_ppid_state(stat: &str) -> Option<(u32, char)> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    let mut parts = rest.split_whitespace();
    let state = parts.next()?.chars().next()?;
    let ppid = parts.next()?.parse().ok()?;
    Some((ppid, state))
}

fn is_webkit(proc: &Proc) -> bool {
    let text = format!("{} {}", proc.comm, proc.cmdline);
    text.contains("WPEWebProcess") || text.contains("WPENetworkProcess")
}

/// WebKit processes this test started: descendants, and any new process whose environ this
/// user can read. `before` is every pid that existed before the runtime started, so another
/// WebKit on the machine is not this test's.
fn webkit_procs(before: &[u32]) -> Vec<Proc> {
    let all = processes();
    let root = std::process::id();
    let mut family = vec![root];
    let mut index = 0;
    while index < family.len() {
        let parent = family[index];
        for proc in &all {
            if proc.ppid == parent && !family.contains(&proc.pid) {
                family.push(proc.pid);
            }
        }
        index += 1;
    }
    all.into_iter()
        .filter(|proc| proc.state != 'Z' && is_webkit(proc))
        .filter(|proc| family.contains(&proc.pid) || !before.contains(&proc.pid))
        .collect()
}

fn env_has(pid: u32, key: &str) -> Result<bool, String> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ"))
        .map_err(|error| format!("/proc/{pid}/environ: {error}"))?;
    let needle = format!("{key}=");
    Ok(bytes
        .split(|byte| *byte == 0)
        .any(|entry| entry.starts_with(needle.as_bytes())))
}

extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

fn reap() {
    loop {
        // WNOHANG: collect children that have already exited, including WebKit's.
        let status = unsafe { waitpid(-1, std::ptr::null_mut(), 1) };
        if status <= 0 {
            break;
        }
    }
}

/// Frames WebKit handed over that were neither copied nor discarded: at most the one held.
fn unacknowledged(diagnostics: &Diagnostics) -> u64 {
    diagnostics.produced - diagnostics.present.shm_copies - diagnostics.present.dropped
}

fn print_diagnostics(label: &str, runtime: &WpeRuntime<Offscreen<Mem>>) {
    let diagnostics = runtime.diagnostics();
    let copies = diagnostics.present.shm_copies.max(1);
    println!(
        "{label}: produced={} copies={} bytes={} stale={} offscreen={} transport={:?} \
         libgl={:?} dri={:?} max_copy_ms={:.3} mean_copy_ms={:.3} max_present_ms={:.3} \
         total_present_ms={:.3} renderer={:?}",
        diagnostics.produced,
        diagnostics.present.shm_copies,
        diagnostics.present.shm_bytes,
        diagnostics.stale_frames,
        diagnostics.offscreen,
        diagnostics.transport,
        diagnostics.libgl_always_software,
        diagnostics.dri_prime,
        diagnostics.max_copy.as_secs_f64() * 1e3,
        diagnostics.total_copy.as_secs_f64() * 1e3 / f64::from(copies as u32),
        diagnostics.max_present.as_secs_f64() * 1e3,
        diagnostics.total_present.as_secs_f64() * 1e3,
        diagnostics.renderer(),
    );
}

#[test]
#[ignore = "starts WebKit; run tests/offscreen.sh"]
fn offscreen_view_copies_argb_frames_without_a_display() {
    assert!(
        std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none(),
        "this test refuses a display; run tests/offscreen.sh"
    );
    assert!(
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some(),
        "this test needs a private session bus; run tests/offscreen.sh"
    );
    if std::env::var_os("JSC_SIGNAL_FOR_GC").is_none() {
        // SAFETY: the test process has no other threads yet that read this variable.
        unsafe { std::env::set_var("JSC_SIGNAL_FOR_GC", "12") };
    }
    unsafe {
        std::env::remove_var("LIBGL_ALWAYS_SOFTWARE");
        std::env::remove_var("DRI_PRIME");
    }
    let before: Vec<u32> = processes().into_iter().map(|proc| proc.pid).collect();

    let mut runtime =
        WpeRuntime::<Offscreen<Mem>>::new(WpeConfig::default(), EngineWaker::new(|| {}))
            .unwrap_or_else(|error| panic!("WpeRuntime::new: {error}"));
    let frames = Arc::new(Mutex::new(Vec::new()));
    let open_metrics = metrics(640, 480);
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
        .unwrap_or_else(|error| panic!("open_view: {error}"));

    let page = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pages/colours.html");
    let url = format!("file://{}", page.display());
    runtime
        .command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url,
            },
        )
        .unwrap_or_else(|error| panic!("navigate: {error}"));

    let _ = pump(&mut runtime, HOLD);
    let held = runtime.diagnostics();
    assert!(
        frames.lock().expect("frames").is_empty(),
        "no permit, so nothing is copied"
    );
    assert!(
        held.produced <= 1,
        "WebKit produced {} frames before any permit",
        held.produced
    );
    println!("withheld before first permit: produced={}", held.produced);

    let stale = LayoutEpoch::next();
    assert_eq!(
        present_at(&mut runtime, view, stale, size),
        PresentOutcome::NotReady,
        "a permit for another epoch"
    );
    assert!(
        frames.lock().expect("frames").is_empty(),
        "a stale permit copies nothing"
    );

    present_until_colours(&mut runtime, view, epoch, size, &frames);
    print_diagnostics("first page 640x480", &runtime);
    let copies = runtime.diagnostics().present.shm_copies;
    let produced_at_hold = runtime.diagnostics().produced;
    let _during = pump(&mut runtime, HOLD);
    let after_hold = runtime.diagnostics();
    println!(
        "withheld after {} copies: produced {} -> {}",
        copies, produced_at_hold, after_hold.produced
    );
    assert!(
        unacknowledged(&after_hold) <= 1,
        "{} frames unacknowledged with permits withheld",
        unacknowledged(&after_hold)
    );
    assert!(
        after_hold.produced - produced_at_hold <= 1,
        "produced grew by {} while permits were withheld",
        after_hold.produced - produced_at_hold
    );
    assert_eq!(
        frames.lock().expect("frames").len() as u64,
        copies,
        "a withheld permit copies nothing more"
    );

    let resized = metrics(400, 300);
    let new_epoch = resized.epoch();
    let new_size = resized.buffer();
    runtime
        .command(view, EngineCommand::SetMetrics(resized))
        .unwrap_or_else(|error| panic!("set metrics: {error}"));
    assert_eq!(
        present_at(&mut runtime, view, epoch, size),
        PresentOutcome::NotReady,
        "the old epoch after SetMetrics"
    );
    present_until_colours(&mut runtime, view, new_epoch, new_size, &frames);
    print_diagnostics("after resize 400x300", &runtime);
    let diagnostics = runtime.diagnostics();
    assert!(diagnostics.offscreen, "CPU target");
    assert_eq!(diagnostics.transport, Some(Transport::Shm));
    assert!(diagnostics.libgl_always_software.is_none());
    assert!(diagnostics.dri_prime.is_none());
    assert_eq!(
        diagnostics.present.shm_copies,
        frames.lock().expect("frames").len() as u64,
        "one counted copy per submitted frame"
    );
    assert!(
        diagnostics.present.shm_copies > copies,
        "the resize submitted another frame"
    );

    let web = webkit_procs(&before);
    let webs: Vec<_> = web
        .iter()
        .filter(|proc| proc.comm.contains("Web") || proc.cmdline.contains("WPEWebProcess"))
        .collect();
    assert!(
        !webs.is_empty(),
        "no readable WPEWebProcess in {}",
        format_procs(&web)
    );
    for proc in &webs {
        let libgl = env_has(proc.pid, "LIBGL_ALWAYS_SOFTWARE").expect("environ");
        let dri = env_has(proc.pid, "DRI_PRIME").expect("environ");
        println!(
            "web process pid={} comm={} LIBGL_ALWAYS_SOFTWARE={libgl} DRI_PRIME={dri}",
            proc.pid, proc.comm
        );
        assert!(!libgl, "web process inherited LIBGL_ALWAYS_SOFTWARE");
        assert!(!dri, "web process inherited DRI_PRIME");
    }

    // Permits as fast as WebKit renders, for 2 s: its rate with this owner loop, and the copies.
    let rate_from = runtime.diagnostics();
    let rate_start = Instant::now();
    while rate_start.elapsed() < Duration::from_secs(2) {
        present_at(&mut runtime, view, new_epoch, new_size);
        let mut events = Vec::new();
        runtime.pump(&mut events);
        std::thread::sleep(Duration::from_millis(1));
    }
    let rate_to = runtime.diagnostics();
    let copied = rate_to.present.shm_copies - rate_from.present.shm_copies;
    let seconds = rate_start.elapsed().as_secs_f64();
    println!(
        "unthrottled 400x300: {copied} frames in {seconds:.3} s, {:.1} frames/s, \
         copy mean {:.3} ms, max {:.3} ms",
        copied as f64 / seconds,
        (rate_to.total_copy - rate_from.total_copy).as_secs_f64() * 1e3 / copied.max(1) as f64,
        rate_to.max_copy.as_secs_f64() * 1e3,
    );
    assert!(
        copied > 10,
        "the animation gave only {copied} frames in 2 s"
    );

    let before_hide = runtime.diagnostics().produced;
    runtime
        .command(view, EngineCommand::SetVisibility(false))
        .unwrap_or_else(|error| panic!("hide: {error}"));
    let _ = pump(&mut runtime, Duration::from_secs(1));
    let hidden = runtime.diagnostics();
    println!(
        "hidden: produced {} -> {} copies={} dropped={}",
        before_hide, hidden.produced, hidden.present.shm_copies, hidden.present.dropped
    );
    assert!(
        hidden.produced - before_hide <= 1 && unacknowledged(&hidden) <= 1,
        "hidden: produced {} -> {}, {} unacknowledged",
        before_hide,
        hidden.produced,
        unacknowledged(&hidden)
    );

    let shutdown_start = Instant::now();
    runtime.shutdown();
    println!(
        "shutdown took {:.3} s",
        shutdown_start.elapsed().as_secs_f64()
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    let mut left = webkit_procs(&before);
    while Instant::now() < deadline {
        reap();
        left = webkit_procs(&before);
        if left.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    reap();
    assert!(
        left.is_empty(),
        "web processes still running: {}",
        format_procs(&left)
    );
    println!(
        "shutdown: web and network processes gone {:.3} s after it began",
        shutdown_start.elapsed().as_secs_f64()
    );
}

fn format_procs(procs: &[Proc]) -> String {
    if procs.is_empty() {
        return "none".into();
    }
    procs
        .iter()
        .map(|proc| format!("{}:{}:{}", proc.pid, proc.state, proc.comm))
        .collect::<Vec<_>>()
        .join(",")
}
