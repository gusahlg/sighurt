//! De-risking RENDER.md §14: wgpu 30 on Vulkan presenting into a host GL-mode target.
//!
//! Opens the host's window (sighurt-wayland), makes one chrome-like target with
//! `PresentMode::Gl` (an 800×60 strip at the top; the rest is the root's opaque background),
//! binds wgpu to the lease's raw handles and presents one cleared frame per host permit, the
//! colour changing every frame so a screenshot names the frame on screen. A fixed script then
//! hides and shows the strip (keeping the `wgpu::Surface`, then dropping and recreating it),
//! resizes it, presents bursts outside the host's pacing, and shuts down in the documented
//! order. Everything measured is printed: per-second permit/callback counts, the time spent in
//! `get_current_texture`, `submit` and `present`, RSS, the adapter and the surface's
//! capabilities.
//!
//! ```sh
//! tools/heavy tools/headless-sway --renderer gles2 -- target/debug/wgpu-subsurface --out DIR
//! tools/heavy tools/headless-sway --renderer gles2 -- target/debug/wgpu-subsurface --wrong-order
//! ```
//!
//! Must not own: anything sig uses. This is an experiment.

use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use sighurt_api::engine::PresentOutcome;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::WindowId;
use sighurt_wayland::{
    Host, HostEvent, InputRegion, Placement, PresentMode, PresentStats, TargetLease,
    TargetPlacement, WindowOptions,
};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// What the script does, each step a delay after the previous one ran (so a step that stalls
/// the loop does not make the later ones run at once).
#[derive(Clone, Debug)]
enum Step {
    Shot(&'static str),
    /// Presents `n` frames back to back without permits, while shown.
    Burst(usize),
    /// Unmaps the strip (an empty rectangle); `drop` also drops the wgpu Surface (unbind).
    Hide {
        drop: bool,
    },
    /// Presents `n` frames on the hidden, still bound surface.
    ProbeHidden(usize),
    /// Shows the strip at this logical size, binding first if unbound. `burst` frames follow
    /// the first permit's frame before the host can commit the root (synchronized subsurface).
    Show {
        w: u32,
        h: u32,
        burst: usize,
    },
    /// Resizes the shown strip.
    Resize {
        w: u32,
        h: u32,
    },
    /// Wrong-order run: retire the lease while the wgpu Surface lives.
    RetireLease,
    End,
}

fn script(wrong_order: bool, sync_burst: usize) -> VecDeque<(f64, Step)> {
    use Step::*;
    if wrong_order {
        return VecDeque::from([(1.5, RetireLease), (1.0, End)]);
    }
    VecDeque::from([
        (2.0, Shot("2s")),
        (1.0, Burst(10)),
        (0.5, Hide { drop: false }),
        (1.0, ProbeHidden(10)),
        (
            1.0,
            Show {
                w: 800,
                h: 60,
                burst: 0,
            },
        ),
        (0.5, Shot("6s-kept-800x60")),
        (0.5, Hide { drop: false }),
        (
            1.0,
            Show {
                w: 1000,
                h: 80,
                burst: 0,
            },
        ),
        (0.5, Shot("kept-1000x80")),
        (0.5, Hide { drop: true }),
        (
            1.0,
            Show {
                w: 800,
                h: 60,
                burst: 0,
            },
        ),
        (0.5, Shot("rebound-800x60")),
        (0.5, Hide { drop: true }),
        (
            0.5,
            Show {
                w: 600,
                h: 40,
                burst: 0,
            },
        ),
        (0.5, Shot("rebound-600x40")),
        (0.5, Resize { w: 1000, h: 80 }),
        (0.5, Shot("resized-1000x80")),
        (
            0.5,
            Show {
                w: 1000,
                h: 80,
                burst: 0,
            },
        ),
        (0.5, Hide { drop: false }),
        (
            0.5,
            Show {
                w: 800,
                h: 60,
                burst: sync_burst,
            },
        ),
        (1.0, Shot("after-sync-burst")),
        (1.0, End),
    ])
}

fn rss() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .map(|l| l["VmRSS:".len()..].trim().to_owned())
        })
        .unwrap_or_else(|| "?".into())
}

/// wgpu's futures resolve at once on native backends.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(v) = future.as_mut().poll(&mut cx) {
            return v;
        }
        std::thread::yield_now();
    }
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

/// The wall clock libwayland's WAYLAND_DEBUG prints (seconds mod 1000, ms), to line up our
/// present calls with its `wl_surface.commit` lines.
fn wall_ms() -> f64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    (d.as_secs() % 1000) as f64 * 1e3 + f64::from(d.subsec_micros()) / 1e3
}

#[derive(Default)]
struct Acc {
    n: u32,
    sum: Duration,
    max: Duration,
}

impl Acc {
    fn add(&mut self, d: Duration) {
        self.n += 1;
        self.sum += d;
        self.max = self.max.max(d);
    }
    fn show(&self) -> String {
        if self.n == 0 {
            return "-".into();
        }
        format!(
            "avg {:.3} max {:.3}",
            ms(self.sum) / f64::from(self.n),
            ms(self.max)
        )
    }
}

struct Second {
    start: Instant,
    acquire: Acc,
    submit: Acc,
    present: Acc,
    configure: Acc,
    failed: u32,
    suboptimal: u32,
    host: PresentStats,
}

struct Gpu {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
}

struct Bound {
    surface: wgpu::Surface<'static>,
    configured: Option<(u32, u32)>,
}

struct Opts {
    out: PathBuf,
    wrong_order: bool,
    present_after_retire: bool,
    latency: u32,
    trace: bool,
    sync_burst: usize,
    reconfigure_suboptimal: u32,
    validation: bool,
}

struct App {
    host: Host,
    window: WindowId,
    lease: Option<TargetLease>,
    window_size: Option<LogicalSize>,
    strip: (u32, u32),
    shown: bool,
    gpu: Option<Gpu>,
    bound: Option<Bound>,
    frames: u64,
    images: HashSet<String>,
    t0: Option<Instant>,
    script: VecDeque<(f64, Step)>,
    /// When the last step ran.
    last_step: Option<Instant>,
    second: Second,
    pending_burst: usize,
    suboptimal_total: u64,
    reconfigured_suboptimal: u32,
    running: bool,
    failed: bool,
    shots: Vec<Child>,
    first_frame_rss: bool,
    last_color: [u8; 3],
    opts: Opts,
}

/// The colour of frame `n`, as the bytes a screenshot shows.
fn color_of(n: u64) -> [u8; 3] {
    [((n * 37) % 256) as u8, ((n * 101) % 256) as u8, 0x80]
}

impl App {
    fn t(&self) -> f64 {
        self.t0.map_or(0.0, |t| t.elapsed().as_secs_f64())
    }

    fn place(&mut self) {
        let (Some(size), Some(lease)) = (self.window_size, self.lease.as_ref()) else {
            return;
        };
        let rect = if self.shown {
            WindowRect {
                x: 0,
                y: 0,
                width: self.strip.0.min(size.width),
                height: self.strip.1.min(size.height),
            }
        } else {
            WindowRect::default()
        };
        let placement = Placement {
            window: self.window,
            size,
            targets: vec![TargetPlacement {
                target: lease.id(),
                rect,
                visible: true,
                input: InputRegion::Whole,
            }],
        };
        let started = Instant::now();
        if let Err(error) = self.host.apply_placement(placement) {
            eprintln!("placement refused: {error}");
        }
        println!(
            "[{:.3}] placement {}×{} applied in {:.3} ms",
            self.t(),
            rect.width,
            rect.height,
            ms(started.elapsed())
        );
    }

    /// Starts the backend if needed (instance, this first surface, an adapter compatible with
    /// it, a device) and creates the wgpu Surface on the lease's handles.
    fn bind(&mut self) -> Result<(), String> {
        let lease = self.lease.as_ref().ok_or("no lease")?;
        let started = Instant::now();
        // SAFETY: the Surface is dropped before the lease (shutdown and `Hide { drop }`), except
        // in the deliberate wrong-order run.
        let handles = unsafe { lease.gl_handles() }.ok_or("not a GL target")?;
        if let Some(gpu) = self.gpu.as_ref() {
            let target =
                unsafe { wgpu::SurfaceTargetUnsafe::from_display_and_window(&handles, &handles) }
                    .map_err(|e| format!("handles: {e}"))?;
            let surface = unsafe { gpu.instance.create_surface_unsafe(target) }
                .map_err(|e| format!("create_surface_unsafe: {e}"))?;
            if !gpu.adapter.is_surface_supported(&surface) {
                return Err("the adapter cannot present to the new surface".into());
            }
            let caps = surface.get_capabilities(&gpu.adapter);
            if !caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                return Err("MAILBOX not offered on the new surface".into());
            }
            self.bound = Some(Bound {
                surface,
                configured: None,
            });
        } else {
            println!("RSS before the instance: {}", rss());
            let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
            desc.backends = wgpu::Backends::VULKAN;
            if self.opts.validation {
                desc.flags |= wgpu::InstanceFlags::VALIDATION | wgpu::InstanceFlags::DEBUG;
            }
            println!("instance flags: {:?}", desc.flags);
            let t = Instant::now();
            let instance = wgpu::Instance::new(desc);
            println!("instance: {:.3} ms", ms(t.elapsed()));
            // SAFETY: as above.
            let target =
                unsafe { wgpu::SurfaceTargetUnsafe::from_display_and_window(&handles, &handles) }
                    .map_err(|e| format!("handles: {e}"))?;
            let surface = unsafe { instance.create_surface_unsafe(target) }
                .map_err(|e| format!("create_surface_unsafe: {e}"))?;
            let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
            let mut chosen = None;
            for adapter in adapters {
                let info = adapter.get_info();
                let supported = adapter.is_surface_supported(&surface);
                println!(
                    "adapter: {:?} type {:?} driver {:?} ({:?}) vendor {:#06x} device {:#06x}; presents to the surface: {supported}",
                    info.name, info.device_type, info.driver, info.driver_info, info.vendor, info.device
                );
                if chosen.is_none() && supported && info.device_type != wgpu::DeviceType::Cpu {
                    chosen = Some(adapter);
                }
            }
            let adapter = chosen.ok_or(
                "no hardware Vulkan adapter can present to this surface (software adapters are refused)",
            )?;
            println!("chosen: {:?}", adapter.get_info().name);
            let t = Instant::now();
            let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("chrome"),
                ..Default::default()
            }))
            .map_err(|e| format!("request_device: {e}"))?;
            println!(
                "device: {:.3} ms; RSS after the device: {}",
                ms(t.elapsed()),
                rss()
            );
            device.on_uncaptured_error(Arc::new(|e: wgpu::Error| {
                eprintln!("wgpu uncaptured error: {e}");
            }));
            device.set_device_lost_callback(|reason, msg| {
                eprintln!("wgpu device lost: {reason:?} {msg}");
            });
            print_caps(&adapter, &surface);
            // Question 7: a configure before any size.
            let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
            surface.configure(&device, &config(0, 0, self.opts.latency));
            match block_on(scope.pop()) {
                Some(e) => println!("configure 0×0: validation error: {e}"),
                None => println!("configure 0×0: accepted (no error)"),
            }
            let caps = surface.get_capabilities(&adapter);
            if !caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
                return Err(format!(
                    "MAILBOX is not offered ({:?}); refusing to run",
                    caps.present_modes
                ));
            }
            self.gpu = Some(Gpu {
                instance,
                adapter,
                device,
                queue,
            });
            self.bound = Some(Bound {
                surface,
                configured: None,
            });
        }
        println!("[{:.3}] bound in {:.3} ms", self.t(), ms(started.elapsed()));
        Ok(())
    }

    fn unbind(&mut self) {
        if let Some(bound) = self.bound.take() {
            let t = Instant::now();
            drop(bound);
            println!(
                "[{:.3}] Surface dropped in {:.3} ms",
                self.t(),
                ms(t.elapsed())
            );
        }
    }

    /// One frame at `size`: lazy (re)configure, acquire, clear, submit, present.
    fn draw(&mut self, size: (u32, u32), tag: &str) -> Result<(), String> {
        let gpu = self.gpu.as_ref().ok_or("not started")?;
        let bound = self.bound.as_mut().ok_or("not bound")?;
        if bound.configured != Some(size) {
            let t = Instant::now();
            bound
                .surface
                .configure(&gpu.device, &config(size.0, size.1, self.opts.latency));
            let d = t.elapsed();
            self.second.configure.add(d);
            println!(
                "[{:.3}] configured {}×{} MAILBOX in {:.3} ms",
                self.t0.map_or(0.0, |t| t.elapsed().as_secs_f64()),
                size.0,
                size.1,
                ms(d)
            );
            bound.configured = Some(size);
        }
        let t = Instant::now();
        let acquired = bound.surface.get_current_texture();
        let acquire = t.elapsed();
        self.second.acquire.add(acquire);
        let texture = match acquired {
            wgpu::CurrentSurfaceTexture::Success(t) => t,
            wgpu::CurrentSurfaceTexture::Suboptimal(t) => {
                self.second.suboptimal += 1;
                self.suboptimal_total += 1;
                if self.reconfigured_suboptimal < self.opts.reconfigure_suboptimal {
                    // Reconfigure before the next acquire (after this frame).
                    self.reconfigured_suboptimal += 1;
                    bound.configured = None;
                    println!("  suboptimal: reconfiguring before the next frame");
                }
                t
            }
            other => {
                self.second.failed += 1;
                return Err(format!(
                    "get_current_texture: {other:?} after {:.3} ms",
                    ms(acquire)
                ));
            }
        };
        // SAFETY: only read to name the swapchain image.
        if let Some(raw) = unsafe { texture.texture.as_hal::<wgpu::hal::api::Vulkan>() } {
            self.images
                .insert(format!("{:?}", unsafe { raw.raw_handle() }));
        }
        self.frames += 1;
        let color = color_of(self.frames);
        self.last_color = color;
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let c = |v: u8| f64::from(v) / 255.0;
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: c(color[0]),
                            g: c(color[1]),
                            b: c(color[2]),
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        let t = Instant::now();
        gpu.queue.submit([encoder.finish()]);
        let submit = t.elapsed();
        self.second.submit.add(submit);
        let before = wall_ms();
        let t = Instant::now();
        gpu.queue.present(texture);
        let present = t.elapsed();
        self.second.present.add(present);
        if self.opts.trace {
            println!(
                "trace {tag} frame {} present wall {before:.3}..{:.3} ms",
                self.frames,
                wall_ms()
            );
        }
        if !tag.is_empty() {
            println!(
                "  {tag} frame {}: acquire {:.3} submit {:.3} present {:.3} ms",
                self.frames,
                ms(acquire),
                ms(submit),
                ms(present)
            );
        }
        Ok(())
    }

    fn present(&mut self) {
        for permit in self.host.permits() {
            let b = permit.metrics().buffer();
            if b.width == 0 || b.height == 0 || self.bound.is_none() {
                continue;
            }
            match self.draw((b.width, b.height), "") {
                Ok(()) => {
                    permit.finish(PresentOutcome::Submitted);
                    if self.t0.is_none() {
                        self.t0 = Some(Instant::now());
                        self.second.start = Instant::now();
                    }
                    if !self.first_frame_rss {
                        self.first_frame_rss = true;
                        println!("RSS after the first frame: {}", rss());
                    }
                }
                Err(e) => {
                    eprintln!("[{:.3}] frame failed: {e}", self.t());
                    permit.finish(PresentOutcome::NotReady);
                }
            }
            if self.pending_burst > 0 {
                let n = std::mem::take(&mut self.pending_burst);
                println!("[{:.3}] burst of {n} right after the first permit after show (root not yet committed):", self.t());
                self.burst(n, (b.width, b.height), "sync-burst");
            }
            // Continuous animation: the next permit follows the frame callback.
            if let Some(lease) = &self.lease {
                self.host.invalidate(lease.id());
            }
        }
    }

    fn burst(&mut self, n: usize, size: (u32, u32), tag: &str) {
        let started = Instant::now();
        for i in 0..n {
            if let Err(e) = self.draw(size, &format!("{tag}[{i}]")) {
                println!("  {tag}[{i}]: {e}");
            }
        }
        println!("  {tag}: {n} frames in {:.3} ms", ms(started.elapsed()));
    }

    fn report_second(&mut self) {
        let Some(t0) = self.t0 else { return };
        if self.second.start.elapsed() < Duration::from_secs(1) {
            return;
        }
        let stats = self
            .host
            .diagnostics()
            .targets
            .first()
            .map(|t| (t.stats, t.mapped, t.awaiting_callback));
        let (s, mapped, awaiting) = stats.unwrap_or_default();
        let p = self.second.host;
        println!(
            "[{:5.2}s] permits {:3} submissions {:3} callbacks {:3} presented {:3} | acquire {} | submit {} | present {} | configure {} | failed {} suboptimal {} | mapped {mapped} awaiting_cb {awaiting} | images seen {}",
            t0.elapsed().as_secs_f64(),
            s.permits.saturating_sub(p.permits),
            s.submissions.saturating_sub(p.submissions),
            s.frame_callbacks.saturating_sub(p.frame_callbacks),
            s.presented.saturating_sub(p.presented),
            self.second.acquire.show(),
            self.second.submit.show(),
            self.second.present.show(),
            self.second.configure.show(),
            self.second.failed,
            self.second.suboptimal,
            self.images.len()
        );
        self.second = Second {
            start: Instant::now(),
            acquire: Acc::default(),
            submit: Acc::default(),
            present: Acc::default(),
            configure: Acc::default(),
            failed: 0,
            suboptimal: 0,
            host: s,
        };
    }

    fn run_script(&mut self) {
        let Some(t0) = self.t0 else { return };
        loop {
            let since = self.last_step.unwrap_or(t0);
            if !self
                .script
                .front()
                .is_some_and(|(delay, _)| since.elapsed().as_secs_f64() >= *delay)
            {
                break;
            }
            let (_, step) = self.script.pop_front().expect("checked");
            self.last_step = Some(Instant::now());
            println!("[{:.3}] step: {step:?}", self.t());
            match step {
                Step::Shot(name) => {
                    let path = self.opts.out.join(format!("shot-{name}.png"));
                    println!(
                        "  screenshot {} (last frame {} colour rgb({},{},{}))",
                        path.display(),
                        self.frames,
                        self.last_color[0],
                        self.last_color[1],
                        self.last_color[2]
                    );
                    match Command::new("grim").arg(&path).spawn() {
                        Ok(child) => self.shots.push(child),
                        Err(e) => eprintln!("grim: {e}"),
                    }
                }
                Step::Burst(n) => {
                    let size = self.bound.as_ref().and_then(|b| b.configured);
                    if let Some(size) = size {
                        self.burst(n, size, "burst");
                    }
                }
                Step::Hide { drop } => {
                    self.shown = false;
                    if drop {
                        self.unbind();
                    }
                    self.place();
                }
                Step::ProbeHidden(n) => {
                    let size = self.bound.as_ref().and_then(|b| b.configured);
                    if let Some(size) = size {
                        self.burst(n, size, "hidden");
                    }
                }
                Step::Show { w, h, burst } => {
                    if self.bound.is_none() {
                        if let Err(e) = self.bind() {
                            eprintln!("bind failed: {e}");
                            self.failed = true;
                            self.running = false;
                        }
                    }
                    self.shown = true;
                    self.strip = (w, h);
                    self.pending_burst = burst;
                    self.place();
                }
                Step::Resize { w, h } => {
                    self.strip = (w, h);
                    self.place();
                }
                Step::RetireLease => {
                    println!("  WRONG ORDER: retiring the lease while the wgpu Surface is alive");
                    if let Some(lease) = self.lease.take() {
                        lease.retire();
                    }
                    self.host.flush();
                    println!(
                        "  lease retired, host flushed (the host destroys the wl_surface now)"
                    );
                    if self.opts.present_after_retire {
                        let size = self.bound.as_ref().and_then(|b| b.configured);
                        if let Some(size) = size {
                            println!("  presenting on the destroyed wl_surface:");
                            self.burst(1, size, "after-retire");
                        }
                    }
                }
                Step::End => self.running = false,
            }
        }
    }

    fn handle(&mut self, event: HostEvent) {
        match event {
            HostEvent::WindowConfigured { size, scale, .. } => {
                println!(
                    "window configured {}×{} at {}/120",
                    size.width,
                    size.height,
                    scale.numerator()
                );
                self.window_size = Some(size);
                self.place();
            }
            HostEvent::MetricsChanged { metrics, .. } => {
                let b = metrics.buffer();
                println!(
                    "[{:.3}] strip metrics: buffer {}×{}, {:?}",
                    self.t(),
                    b.width,
                    b.height,
                    metrics.epoch()
                );
            }
            HostEvent::CloseRequested { .. } => self.running = false,
            HostEvent::ConnectionLost { error } => {
                eprintln!("[{:.3}] connection lost: {error}", self.t());
                self.running = false;
                self.failed = true;
            }
            _ => {}
        }
    }
}

fn config(width: u32, height: u32, latency: u32) -> wgpu::SurfaceConfiguration {
    wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: FORMAT,
        color_space: Default::default(),
        width,
        height,
        present_mode: wgpu::PresentMode::Mailbox,
        desired_maximum_frame_latency: latency,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
    }
}

fn print_caps(adapter: &wgpu::Adapter, surface: &wgpu::Surface<'_>) {
    let caps = surface.get_capabilities(adapter);
    println!("surface formats: {:?}", caps.formats);
    println!("surface present modes: {:?}", caps.present_modes);
    println!("surface alpha modes: {:?}", caps.alpha_modes);
    println!("surface usages: {:?}", caps.usages);
    // The image counts wgpu does not expose: wgpu-hal's capabilities carry
    // maximum_frame_latency = (minImageCount - 1)..=(maxImageCount - 1).
    use wgpu::hal::Adapter as _;
    // SAFETY: read-only queries on live objects.
    let hal = unsafe {
        match (
            adapter.as_hal::<wgpu::hal::api::Vulkan>(),
            surface.as_hal::<wgpu::hal::api::Vulkan>(),
        ) {
            (Some(a), Some(s)) => a.surface_capabilities(&s),
            _ => None,
        }
    };
    match hal {
        Some(c) => {
            let max = c.maximum_frame_latency.end().saturating_add(1);
            println!(
                "Vulkan surface: minImageCount {} maxImageCount {} (u32::MAX = unlimited), current extent {:?}",
                c.maximum_frame_latency.start() + 1,
                if max == 0 { u32::MAX } else { max },
                c.current_extent
            );
        }
        None => println!("Vulkan surface capabilities: unavailable"),
    }
}

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::max_level()
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!("log {} {}: {}", r.level(), r.target(), r.args());
        }
    }
    fn flush(&self) {}
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let opts = Opts {
        out: PathBuf::from(
            value("--out").unwrap_or_else(|| "/data/sig-builds/test-out/wgpuexp".into()),
        ),
        wrong_order: args.iter().any(|a| a == "--wrong-order"),
        present_after_retire: args.iter().any(|a| a == "--present-after-retire"),
        latency: value("--latency").and_then(|v| v.parse().ok()).unwrap_or(2),
        trace: args.iter().any(|a| a == "--trace"),
        sync_burst: value("--sync-burst")
            .and_then(|v| v.parse().ok())
            .unwrap_or(4),
        validation: args.iter().any(|a| a == "--validation"),
        reconfigure_suboptimal: value("--reconfigure-suboptimal")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    };
    let _ = std::fs::create_dir_all(&opts.out);
    let _ = log::set_logger(&Logger);
    log::set_max_level(match std::env::var("WGPU_EXP_LOG").as_deref() {
        Ok("info") => log::LevelFilter::Info,
        Ok("debug") => log::LevelFilter::Debug,
        _ => log::LevelFilter::Warn,
    });
    println!("RSS at start: {}", rss());
    println!(
        "compositor renderer: {}",
        std::env::var("SIG_SWAY_RENDERER").unwrap_or_else(|_| "?".into())
    );

    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().expect("an event loop");
    let handle = event_loop.handle();
    let mut host = match Host::new(&handle, |app: &mut App| &mut app.host) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    };
    println!("host capabilities: {:?}", host.capabilities());
    let window = WindowId::next();
    host.create_window(
        window,
        WindowOptions {
            title: "wgpu-subsurface".into(),
            app_id: "sighurt-wgpu-subsurface".into(),
            background: [0x20, 0x20, 0x20],
            ..WindowOptions::default()
        },
    );
    let lease = host
        .create_target(window, PresentMode::Gl, None)
        .expect("the window exists");
    // The script's clock: the loop wakes every 50 ms, so hidden phases still advance.
    handle
        .insert_source(
            Timer::from_duration(Duration::from_millis(50)),
            |_, _, _| TimeoutAction::ToDuration(Duration::from_millis(50)),
        )
        .expect("a timer");
    let mut app = App {
        host,
        window,
        lease: Some(lease),
        window_size: None,
        strip: (800, 60),
        shown: true,
        gpu: None,
        bound: None,
        frames: 0,
        images: HashSet::new(),
        t0: None,
        script: script(opts.wrong_order, opts.sync_burst),
        last_step: None,
        second: Second {
            start: Instant::now(),
            acquire: Acc::default(),
            submit: Acc::default(),
            present: Acc::default(),
            configure: Acc::default(),
            failed: 0,
            suboptimal: 0,
            host: PresentStats::default(),
        },
        pending_burst: 0,
        suboptimal_total: 0,
        reconfigured_suboptimal: 0,
        running: true,
        failed: false,
        shots: Vec::new(),
        first_frame_rss: false,
        last_color: [0; 3],
        opts,
    };
    // Bind at creation, while the target still has empty metrics (as a chrome target is born):
    // the swapchain is configured lazily at the first permit.
    if let Err(e) = app.bind() {
        eprintln!("REFUSED: {e}");
        app.lease.take();
        drop(app);
        std::process::exit(2);
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let signal = event_loop.get_signal();
    event_loop
        .run(None, &mut app, |app| {
            while let Some(event) = app.host.next_event() {
                app.handle(event);
            }
            app.present();
            app.run_script();
            app.report_second();
            app.host.flush();
            if !app.running || Instant::now() > deadline {
                signal.stop();
            }
        })
        .expect("the event loop");

    println!(
        "frames {} (suboptimal {}) distinct swapchain images seen {}: {:?}",
        app.frames,
        app.suboptimal_total,
        app.images.len(),
        app.images
    );
    for mut child in std::mem::take(&mut app.shots) {
        let _ = child.wait();
    }
    // Shutdown in the documented order: the Surface, then the device and instance, then the
    // lease, then the host.
    let App {
        host,
        lease,
        gpu,
        bound,
        ..
    } = app;
    let t = Instant::now();
    drop(bound);
    println!("shutdown: Surface dropped in {:.3} ms", ms(t.elapsed()));
    if let Some(Gpu {
        instance,
        adapter,
        device,
        queue,
    }) = gpu
    {
        let t = Instant::now();
        drop(queue);
        drop(device);
        println!(
            "shutdown: queue and device dropped in {:.3} ms",
            ms(t.elapsed())
        );
        let t = Instant::now();
        drop(adapter);
        drop(instance);
        println!(
            "shutdown: adapter and instance dropped in {:.3} ms",
            ms(t.elapsed())
        );
    }
    drop(lease);
    let t = Instant::now();
    let mut host = host;
    host.flush();
    drop(host);
    println!(
        "shutdown: lease and host dropped in {:.3} ms; clean exit",
        ms(t.elapsed())
    );
}
