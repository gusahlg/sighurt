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

/// The last frame a target was given: its size and ARGB8888 pixels (B, G, R, A in memory),
/// how long painting it took, and the buffer age and damage of that submit.
#[derive(Clone, Default)]
pub struct Shown {
    frame: Arc<Mutex<Option<Painted>>>,
    painting: Arc<Mutex<Duration>>,
    /// How many submits ago the buffer's pixels were a submitted frame. 0 is a new, abandoned
    /// or resized buffer.
    age: Arc<Mutex<u32>>,
    /// The submit damaged every pixel: age 0, an unknown age, or a diff that is the whole buffer.
    whole: Arc<Mutex<bool>>,
}

type Painted = (BufferSize, Vec<u8>);

impl Shown {
    /// How long the runtime took to paint the last frame into the buffer it was lent: the
    /// owner thread's time, without what this in-memory target spends allocating buffers.
    pub fn painting(&self) -> Duration {
        *self.painting.lock().unwrap()
    }

    /// The age the last submit's buffer reported. 0 means the painter had to cover every pixel.
    pub fn age(&self) -> u32 {
        *self.age.lock().unwrap()
    }

    /// Whether the last submit damaged the whole buffer.
    pub fn whole(&self) -> bool {
        *self.whole.lock().unwrap()
    }

    /// The `[b, g, r, a]` bytes of pixel (`x`, `y`) of the last frame.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let shown = self.frame.lock().unwrap();
        let (size, pixels) = shown.as_ref().expect("nothing was presented");
        let i = 4 * (y * size.width + x) as usize;
        pixels[i..i + 4].try_into().unwrap()
    }

    pub fn pixels(&self) -> Painted {
        self.frame
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

/// One SHM slot. `content` is the swapchain epoch of the frame these pixels show, or `None`
/// when they are not a submitted frame (new, or abandoned mid-paint).
struct Slot {
    pixels: Vec<u8>,
    content: Option<u64>,
    /// Attached until a later submit releases it, the way the compositor holds the latest two.
    submitted: bool,
}

/// A presentation target in memory. It follows the host swapchain's age rules
/// ([`sighurt_wayland`]'s `Swapchain`): at most three buffers, a new or abandoned one is age 0
/// and zeros, a resize starts over, and a reused buffer's age is how many submits ago its
/// pixels were submitted. The painter is told that age, so a partial repaint is checked against
/// the pixels the buffer actually still holds. Two submitted buffers stay held; the older one
/// is released when a third submit arrives, which is age 3 on the way back.
pub struct TestTarget {
    metrics: Metrics,
    shown: Shown,
    slots: Vec<Slot>,
    size: BufferSize,
    /// Submits at the current size.
    epoch: u64,
}

/// How many submits ago `content` was. 0 when the pixels are not a submitted frame.
fn age_of(epoch: u64, content: Option<u64>) -> u32 {
    match content {
        Some(content) => epoch
            .saturating_sub(content)
            .saturating_add(1)
            .min(u32::MAX as u64) as u32,
        None => 0,
    }
}

/// Clears the thread-local paint if a paint panics, so the next test on this thread does not
/// inherit an age.
struct EndPaint {
    done: bool,
}

impl EndPaint {
    fn finish(&mut self) -> Option<Vec<sighurt_wayland::DamageRect>> {
        self.done = true;
        sighurt_wayland::end_paint()
    }
}

impl Drop for EndPaint {
    fn drop(&mut self) {
        if !self.done {
            let _ = sighurt_wayland::end_paint();
        }
    }
}

pub struct Permit {
    epoch: LayoutEpoch,
    size: BufferSize,
}

impl TestTarget {
    /// A free slot of `size`, newly zeroed when it holds no submitted frame.
    fn acquire(&mut self, size: BufferSize) -> usize {
        let len = 4 * (size.width as usize) * (size.height as usize);
        if let Some(index) = self.slots.iter().position(|slot| !slot.submitted) {
            let slot = &mut self.slots[index];
            if slot.pixels.len() != len {
                slot.pixels.resize(len, 0);
            }
            if slot.content.is_none() {
                slot.pixels.fill(0);
            }
            return index;
        }
        assert!(self.slots.len() < 3, "every buffer is still submitted");
        self.slots.push(Slot {
            pixels: vec![0; len],
            content: None,
            submitted: false,
        });
        self.slots.len() - 1
    }

    /// Keeps the two newest submitted buffers. The one released still shows the frame it was
    /// given, and reports that age the next time it is drawn into.
    fn release_oldest(&mut self) {
        let held = self.slots.iter().filter(|slot| slot.submitted).count();
        if held <= 2 {
            return;
        }
        let oldest = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.submitted)
            .min_by_key(|(_, slot)| slot.content.unwrap_or(0))
            .map(|(index, _)| index);
        if let Some(index) = oldest {
            self.slots[index].submitted = false;
        }
    }
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
        let size = permit.size;
        if self.size != size {
            // A new size has no frames. Ages belong to this size only, as on a resize.
            self.slots.clear();
            self.epoch = 0;
            self.size = size;
        }
        let index = self.acquire(size);
        let age = age_of(self.epoch, self.slots[index].content);
        sighurt_wayland::begin_paint(age);
        let mut end = EndPaint { done: false };
        let started = Instant::now();
        let painted = {
            let slot = &mut self.slots[index];
            paint(Frame {
                pixels: &mut slot.pixels,
                size,
                stride: 4 * size.width as usize,
            })
        };
        let damage = end.finish();
        *self.shown.painting.lock().unwrap() = started.elapsed();
        if let Err(error) = painted {
            // Not a frame: the next lend is age 0 and zeros, whatever was half-painted.
            let slot = &mut self.slots[index];
            slot.content = None;
            slot.submitted = false;
            slot.pixels.fill(0);
            return Err(error);
        }
        let epoch = self.epoch.saturating_add(1);
        self.slots[index].content = Some(epoch);
        self.slots[index].submitted = true;
        self.epoch = epoch;
        self.release_oldest();
        // No report, or an empty one, damages the whole buffer. Age 0 must take that path.
        let whole = match &damage {
            None => true,
            Some(rects) => rects.is_empty(),
        };
        assert!(
            age != 0 || whole,
            "age 0 damages the whole buffer, got {damage:?}"
        );
        let pixels = self.slots[index].pixels.clone();
        *self.shown.frame.lock().unwrap() = Some((size, pixels));
        *self.shown.age.lock().unwrap() = age;
        *self.shown.whole.lock().unwrap() = whole;
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

    /// A host with these internal pages (`sig://<name>`).
    pub fn with_pages(pages: Vec<sighurt_engine_wasm::pages::InternalPage>) -> Self {
        Self::build(
            SandboxPolicy::default(),
            tempfile::tempdir().unwrap(),
            pages,
        )
    }

    /// A host whose key-value store lives in `dir`.
    pub fn in_dir(policy: SandboxPolicy, dir: tempfile::TempDir) -> Self {
        Self::build(policy, dir, Vec::new())
    }

    fn build(
        policy: SandboxPolicy,
        dir: tempfile::TempDir,
        pages: Vec<sighurt_engine_wasm::pages::InternalPage>,
    ) -> Self {
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
            pages,
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
            slots: Vec::new(),
            size: BufferSize::default(),
            epoch: 0,
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
    // Honour the build's target dir. The crate lives in engines/, so the default is two up.
    let mut path = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            [env!("CARGO_MANIFEST_DIR"), "..", "..", "target"]
                .iter()
                .collect()
        });
    path.push("wasm32-unknown-unknown");
    path.push("release");
    path.push(&file);
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
