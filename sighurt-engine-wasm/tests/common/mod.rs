//! A test host for the Wasm runtime: an in-memory CPU target and an owner loop that pumps the
//! runtime when its waker fires, the way `sig` does.

#![allow(dead_code)] // Each test binary uses its own part of this.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, SeatId, ViewId};
use sighurt_api::input::{InputEvent, InputKind};
use sighurt_engine_wasm::{CpuTarget, Frame, SandboxPolicy, WasmConfig, WasmRuntime};

/// The last frame a target was given: its size and ARGB8888 pixels (B, G, R, A in memory), and
/// how long painting it took.
#[derive(Clone, Default)]
pub struct Shown(Arc<Mutex<Option<Painted>>>, Arc<Mutex<Duration>>);

type Painted = (BufferSize, Vec<u8>);

impl Shown {
    /// How long the runtime took to paint the last frame into the buffer it was lent: the
    /// owner thread's time, without what this in-memory target spends allocating buffers.
    pub fn painting(&self) -> Duration {
        *self.1.lock().unwrap()
    }

    /// The `[b, g, r, a]` bytes of pixel (`x`, `y`) of the last frame.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let shown = self.0.lock().unwrap();
        let (size, pixels) = shown.as_ref().expect("nothing was presented");
        let i = 4 * (y * size.width + x) as usize;
        pixels[i..i + 4].try_into().unwrap()
    }

    pub fn pixels(&self) -> Painted {
        self.0
            .lock()
            .unwrap()
            .clone()
            .expect("nothing was presented")
    }

    /// Writes the last frame as a PNG (straight RGBA) to `path`.
    pub fn save_png(&self, path: &Path) {
        let (size, pixels) = self.pixels();
        let rgba: Vec<u8> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[b, g, r, a]| {
                let unpremul = |c: u8| match a {
                    0 => 0,
                    a => ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8,
                };
                [unpremul(r), unpremul(g), unpremul(b), a]
            })
            .collect();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbaImage::from_raw(size.width, size.height, rgba)
            .unwrap()
            .save(path)
            .unwrap();
    }
}

/// A presentation target in memory, standing in for the Wayland host's SHM lease.
pub struct TestTarget {
    metrics: Metrics,
    shown: Shown,
}

pub struct Permit {
    epoch: LayoutEpoch,
    size: BufferSize,
}

impl CpuTarget for TestTarget {
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
        let BufferSize { width, height } = permit.size;
        let mut pixels = vec![0x55; 4 * (width * height) as usize];
        let started = Instant::now();
        paint(Frame {
            pixels: &mut pixels,
            size: permit.size,
            stride: 4 * width as usize,
        })?;
        *self.shown.1.lock().unwrap() = started.elapsed();
        *self.shown.0.lock().unwrap() = Some((permit.size, pixels));
        Ok(())
    }
}

/// `width` × `height` logical pixels at `scale` / 120, in a new layout epoch.
pub fn metrics(width: u32, height: u32, scale: u32) -> Metrics {
    let rect = WindowRect {
        x: 0,
        y: 64,
        width,
        height,
    };
    Metrics::new(
        LayoutEpoch::next(),
        rect,
        Scale::from_fractional(scale).unwrap(),
    )
}

/// The owner loop of one runtime, with everything it has been told so far.
pub struct Host {
    pub runtime: WasmRuntime<TestTarget>,
    waker: EngineWaker,
    woken: Arc<(Mutex<bool>, Condvar)>,
    /// Events pumped but not yet looked at by `until`.
    backlog: Vec<EngineEvent>,
    /// The longest a pump took.
    pub longest_pump: Duration,
    /// How often the runtime woke the owner loop.
    pub wakes: Arc<AtomicUsize>,
    pub metrics: std::collections::HashMap<ViewId, Metrics>,
    dir: tempfile::TempDir,
}

impl Host {
    pub fn new() -> Self {
        Self::with_policy(SandboxPolicy::default())
    }

    pub fn with_policy(policy: SandboxPolicy) -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::in_dir(policy, dir)
    }

    /// A host whose key-value store lives in `dir`.
    pub fn in_dir(policy: SandboxPolicy, dir: tempfile::TempDir) -> Self {
        let woken = Arc::new((Mutex::new(false), Condvar::new()));
        let wakes = Arc::new(AtomicUsize::new(0));
        let (notify, count) = (woken.clone(), wakes.clone());
        let waker = EngineWaker::new(move || {
            count.fetch_add(1, Ordering::Relaxed);
            *notify.0.lock().unwrap() = true;
            notify.1.notify_all();
        });
        let config = WasmConfig {
            policy,
            kv_root: dir.path().join("kv"),
            aot_cache: None,
        };
        Self {
            runtime: WasmRuntime::new(config, waker.clone()).unwrap(),
            waker,
            woken,
            backlog: Vec::new(),
            longest_pump: Duration::ZERO,
            wakes,
            metrics: Default::default(),
            dir,
        }
    }

    pub fn dir(&self) -> &Path {
        self.dir.path()
    }

    /// Opens a visible view of `width` × `height` logical pixels at `scale` / 120.
    pub fn open(&mut self, width: u32, height: u32, scale: u32) -> (ViewId, Shown) {
        let view = ViewId::next();
        let shown = Shown::default();
        let metrics = metrics(width, height, scale);
        let target = TestTarget {
            metrics,
            shown: shown.clone(),
        };
        self.runtime
            .open_view(view, ViewOptions::default(), target)
            .unwrap();
        self.metrics.insert(view, metrics);
        (view, shown)
    }

    pub fn command(&mut self, view: ViewId, command: EngineCommand) {
        if let EngineCommand::SetMetrics(metrics) = command {
            self.metrics.insert(view, metrics);
        }
        self.runtime.command(view, command).unwrap();
    }

    pub fn navigate(&mut self, view: ViewId, url: &str) -> NavigationId {
        let navigation = NavigationId::next();
        let url = url.to_string();
        self.command(view, EngineCommand::Navigate { navigation, url });
        navigation
    }

    pub fn input(&mut self, view: ViewId, kind: InputKind) {
        let seat = SeatId::next();
        self.command(view, EngineCommand::Input(InputEvent { seat, kind }));
    }

    /// Asks `view` for a frame at its current metrics.
    pub fn present(&mut self, view: ViewId) -> PresentOutcome {
        let metrics = self.metrics[&view];
        let permit = Permit {
            epoch: metrics.epoch(),
            size: metrics.buffer(),
        };
        self.runtime.present(view, permit).unwrap()
    }

    /// One pump, timed.
    pub fn pump(&mut self) -> PumpStatus {
        self.waker.take();
        let started = Instant::now();
        let mut events = Vec::new();
        let status = self.runtime.pump(&mut events);
        self.longest_pump = self.longest_pump.max(started.elapsed());
        self.backlog.extend(events);
        status
    }

    /// The events pumped and not looked at yet.
    pub fn take(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.backlog)
    }

    /// Runs the owner loop until an event satisfies `done`, and returns every event up to and
    /// including it. Panics after `timeout`.
    pub fn until(
        &mut self,
        timeout: Duration,
        mut done: impl FnMut(&EngineEvent) -> bool,
    ) -> Vec<EngineEvent> {
        let deadline = Instant::now() + timeout;
        let mut seen = Vec::new();
        loop {
            if let Some(i) = self.backlog.iter().position(&mut done) {
                seen.extend(self.backlog.drain(..=i));
                return seen;
            }
            seen.append(&mut self.backlog);
            let status = self.pump();
            if !self.backlog.is_empty() {
                continue;
            }
            let now = Instant::now();
            assert!(now < deadline, "timed out; saw {seen:#?}");
            let wake_at = match status {
                PumpStatus::Pending { deadline: None } => continue,
                PumpStatus::Pending { deadline: Some(at) } => at.min(deadline),
                PumpStatus::Idle => deadline,
            };
            self.sleep_until(wake_at);
        }
    }

    /// Runs the owner loop for `time` and returns the events it got.
    pub fn run_for(&mut self, time: Duration) -> Vec<EngineEvent> {
        let end = Instant::now() + time;
        let mut seen = std::mem::take(&mut self.backlog);
        while Instant::now() < end {
            let status = self.pump();
            seen.append(&mut self.backlog);
            let wake_at = match status {
                PumpStatus::Pending { deadline: None } => continue,
                PumpStatus::Pending { deadline: Some(at) } => at.min(end),
                PumpStatus::Idle => end,
            };
            self.sleep_until(wake_at);
        }
        seen
    }

    /// Sleeps until `at` or until the runtime's waker fires.
    fn sleep_until(&self, at: Instant) {
        let (woken, wake) = &*self.woken;
        let mut woken = woken.lock().unwrap();
        while !*woken {
            let now = Instant::now();
            if now >= at {
                break;
            }
            woken = wake.wait_timeout(woken, at - now).unwrap().0;
        }
        *woken = false;
    }
}

/// A `file://` URL for `path`.
pub fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path).unwrap().to_string()
}

/// Writes the guest in WebAssembly text `wat` to `dir/{name}.wasm` and returns its URL.
pub fn guest(dir: &Path, name: &str, wat: &str) -> String {
    let path = dir.join(format!("{name}.wasm"));
    std::fs::write(&path, wat::parse_str(wat).unwrap()).unwrap();
    file_url(&path)
}

/// The URL of an example guest, built with
/// `tools/cargo build --target wasm32-unknown-unknown --release -p <example>`.
pub fn example(crate_name: &str) -> String {
    let file = format!("{}.wasm", crate_name.replace('-', "_"));
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "..",
        "target",
        "wasm32-unknown-unknown",
        "release",
        &file,
    ]
    .iter()
    .collect();
    assert!(
        path.is_file(),
        "{} is missing: build the example guests first with \
         `tools/cargo build --target wasm32-unknown-unknown --release -p {crate_name}`",
        path.display()
    );
    file_url(&path.canonicalize().unwrap())
}

pub fn committed_url(event: &EngineEvent) -> Option<&str> {
    match event {
        EngineEvent::NavigationCommitted { url, .. } => Some(url),
        _ => None,
    }
}

pub fn is_frame_ready(view: ViewId) -> impl Fn(&EngineEvent) -> bool {
    move |event| *event == EngineEvent::FrameReady { view }
}
