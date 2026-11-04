//! The wgpu chrome renderer (`[ui] renderer = "wgpu"`): wgpu 30 on Vulkan, presenting each
//! chrome surface through a swapchain on the host's native target (docs/rewrite/RENDER.md §7,
//! as amended by §14 and §15).
//!
//! - **Vulkan only**, never wgpu's GL backend: that would open the EGL display of the
//!   `wl_display`, which is Servo's (§14.1). The GPU-selection environment is never written.
//!   Validation only with `SIG_VULKAN_VALIDATION=1`, whatever the build profile.
//! - **Construction is provisional**: [`WgpuBackend::new`] makes the instance and nothing else.
//!   The first [`Backend::bind`] picks an adapter that can present to that surface (low power
//!   first; a software adapter only when `[ui] renderer = "wgpu"` was named, with a notice),
//!   creates the device with an uncaptured-error handler and a device-lost callback, and checks
//!   that the surface offers MAILBOX, `Bgra8Unorm` and opaque composition; otherwise it fails
//!   and the frontend falls back to the CPU backend.
//! - **Swapchains are configured lazily** by the first render, at the permit's metrics (a
//!   target is born with empty metrics, and a 0×0 configure is an error), again when the size
//!   changes, and once after an acquire reports `Suboptimal` (NVIDIA does after every map).
//! - **One render per permit, never on its own clock.** An acquire that times out or finds the
//!   surface occluded is [`RenderOutcome::Deferred`]: the frontend invalidates the target and
//!   the host issues a new permit when it may; presenting outside the host's pacing would block
//!   1 s in a synchronized subsurface (§15). `Outdated` or `Lost` reconfigures once, then is
//!   [`RenderError::Surface`]; validation errors, device loss and an atlas that overflows are
//!   [`RenderError::Device`].
//! - **Presenting commits the `wl_surface`** with the frame callback the host requested at the
//!   permit (measured on NVIDIA, §15): the frontend then finishes the permit as submitted.
//! - **No readback on the display path**: [`Output::Pixels`] is refused. With the `testing`
//!   feature, [`WgpuBackend::render_to_image`] renders offscreen and reads it back, for the
//!   comparison with the CPU backend.
//! - **Lifetimes**: a surface's swapchain lives from `bind` to `unbind` (kept across a hide);
//!   when the last surface is unbound the device, adapter and instance are dropped too, so a
//!   backend that presents nothing holds no GPU memory. Dropping the backend drops every surface
//!   first, then the device, then the instance.
//!
//! Must not own: what is drawn (the chrome's), meshes, atlas planning and shaders
//! (`sighurt_render::gpu`), or target lifetimes (the frontend's and the host's).

mod device;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, WindowHandle,
};
use sighurt_api::geometry::Metrics;
use sighurt_render::gpu::{self, Atlas};
use sighurt_render::{
    AtlasStats, Backend, BackendStats, FontSystemHandle, NativeWindow, Output, Presents,
    RenderError, RenderOutcome, Scene, SurfaceKey,
};

use crate::device::{Device, FORMAT};

/// Frames a swapchain may queue ahead: wgpu then asks for one image more.
const FRAME_LATENCY: u32 = 2;

/// Why the device can't go on, as its callbacks report it (they may run on wgpu's threads).
type Fault = Arc<Mutex<Option<String>>>;

/// The adapter and the device made at the first bind.
struct Gpu {
    adapter: wgpu::Adapter,
    device: Device,
    describe: String,
}

/// One bound surface.
struct Bound {
    surface: wgpu::Surface<'static>,
    /// The size its swapchain is configured at.
    configured: Option<(u32, u32)>,
    /// An acquire said `Suboptimal`: configure again before the next frame.
    reconfigure: bool,
    /// The reconfigure after a `Suboptimal` happened; not again until an acquire succeeds.
    reconfigured: bool,
    /// The metrics and scene of the last frame presented.
    last: Option<(Metrics, Scene)>,
}

/// The wgpu backend. One serves every chrome surface of a window.
pub struct WgpuBackend {
    fonts: FontSystemHandle,
    /// `[ui] renderer = "wgpu"` named it: a software adapter may be used, with a notice.
    explicit: bool,
    instance: Option<wgpu::Instance>,
    surfaces: BTreeMap<SurfaceKey, Bound>,
    gpu: Option<Gpu>,
    atlas: Atlas,
    fault: Fault,
    notices: Vec<String>,
    stats: BackendStats,
}

impl WgpuBackend {
    /// A backend that will rasterize glyphs from `fonts`: the instance only (Vulkan), the rest
    /// at the first bind. `explicit` says `[ui] renderer = "wgpu"` named it.
    pub fn new(fonts: FontSystemHandle, explicit: bool) -> Result<Self, String> {
        Ok(Self {
            fonts,
            explicit,
            instance: Some(instance()),
            surfaces: BTreeMap::new(),
            gpu: None,
            atlas: Atlas::new(),
            fault: Arc::default(),
            notices: Vec::new(),
            stats: BackendStats::default(),
        })
    }

    /// The adapter and device, made for `surface` on the first bind.
    fn start(&mut self, surface: &wgpu::Surface<'_>) -> Result<(), RenderError> {
        let instance = self.instance.as_ref().expect("made before");
        let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        let presenting: Vec<_> = adapters
            .into_iter()
            .filter(|adapter| adapter.is_surface_supported(surface))
            .collect();
        let rank = |adapter: &wgpu::Adapter| match adapter.get_info().device_type {
            // Low power first: the chrome is light, the discrete GPU is Servo's.
            wgpu::DeviceType::IntegratedGpu => 0,
            wgpu::DeviceType::DiscreteGpu => 1,
            wgpu::DeviceType::VirtualGpu => 2,
            wgpu::DeviceType::Other => 3,
            wgpu::DeviceType::Cpu => 4,
        };
        let adapter = presenting
            .into_iter()
            .filter(|adapter| self.explicit || rank(adapter) < 4)
            .min_by_key(rank)
            .ok_or_else(|| {
                RenderError::Device(
                    "no Vulkan adapter can present to the chrome's surface (software adapters \
                     are used only when [ui] renderer = \"wgpu\" names this renderer)"
                        .into(),
                )
            })?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            self.notices.push(format!(
                "The chrome is drawn by wgpu on a software adapter ({}): slower than the cpu \
                 renderer.",
                info.name
            ));
        }
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("sighurt chrome"),
            // The chrome's resources are small: prefer memory to speed.
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            ..Default::default()
        }))
        .map_err(|e| RenderError::Device(format!("no device: {e}")))?;
        let fault = Arc::clone(&self.fault);
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            let mut fault = fault.lock().unwrap_or_else(|e| e.into_inner());
            fault.get_or_insert_with(|| format!("wgpu: {error}"));
        }));
        let fault = Arc::clone(&self.fault);
        device.set_device_lost_callback(move |reason, message| {
            let mut fault = fault.lock().unwrap_or_else(|e| e.into_inner());
            fault.get_or_insert_with(|| format!("the device was lost ({reason:?}): {message}"));
        });
        let describe = format!(
            "wgpu 30 on Vulkan: {} ({:?}), driver {} {}",
            info.name, info.device_type, info.driver, info.driver_info
        );
        self.gpu = Some(Gpu {
            adapter,
            device: Device::new(device, queue),
            describe,
        });
        Ok(())
    }

    /// Configures `bound`'s swapchain at `size`. A size beyond the device's limit or a
    /// configuration the device refuses is a [`RenderError::Surface`], and the swapchain stays
    /// unconfigured (acquiring from it would panic), so the next render configures again.
    fn configure(
        gpu: &Gpu,
        bound: &mut Bound,
        size: (u32, u32),
        stats: &mut BackendStats,
    ) -> Result<(), RenderError> {
        bound.configured = None;
        let limit = gpu.device.device.limits().max_texture_dimension_2d;
        if size.0 > limit || size.1 > limit {
            return Err(RenderError::Surface(format!(
                "{}×{} is beyond the device's {limit}-pixel limit",
                size.0, size.1
            )));
        }
        let started = Instant::now();
        let device = &gpu.device.device;
        let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        bound.surface.configure(
            device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: FORMAT,
                color_space: Default::default(),
                width: size.0,
                height: size.1,
                present_mode: wgpu::PresentMode::Mailbox,
                desired_maximum_frame_latency: FRAME_LATENCY,
                alpha_mode: wgpu::CompositeAlphaMode::Opaque,
                view_formats: vec![],
            },
        );
        let refused = block_on(validation.pop()).or(block_on(memory.pop()));
        stats.configures += 1;
        let took = micros(started.elapsed());
        stats.max_configure_us = Some(stats.max_configure_us.unwrap_or(0).max(took));
        if let Some(error) = refused {
            return Err(RenderError::Surface(format!(
                "configuring the swapchain: {error}"
            )));
        }
        bound.configured = Some(size);
        bound.reconfigure = false;
        Ok(())
    }

    /// Drops the device, the adapter and the instance, and the atlas planned for them.
    fn stop(&mut self) {
        self.gpu = None;
        self.instance = None;
        self.atlas = Atlas::new();
        *self.fault.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl Backend for WgpuBackend {
    fn name(&self) -> &'static str {
        "wgpu"
    }

    fn presents(&self) -> Presents {
        Presents::Native
    }

    fn describe(&self) -> String {
        match &self.gpu {
            Some(gpu) => gpu.describe.clone(),
            None => "wgpu 30 on Vulkan: the adapter is chosen at the first surface".into(),
        }
    }

    #[allow(unsafe_code)]
    unsafe fn bind(
        &mut self,
        surface: SurfaceKey,
        window: &dyn NativeWindow,
    ) -> Result<(), RenderError> {
        let started = Instant::now();
        self.unbind(surface);
        let instance = self.instance.get_or_insert_with(instance);
        let handles = Handles(window);
        // SAFETY: the caller keeps the window and its display alive until `unbind`
        // (`Backend::bind`'s contract), which drops this surface.
        let target =
            unsafe { wgpu::SurfaceTargetUnsafe::from_display_and_window(&handles, &handles) }
                .map_err(|e| RenderError::Surface(format!("the window's handles: {e}")))?;
        // SAFETY: as above.
        let created = unsafe { instance.create_surface_unsafe(target) }
            .map_err(|e| RenderError::Surface(format!("no Vulkan surface: {e}")));
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                if self.surfaces.is_empty() {
                    self.stop();
                }
                return Err(error);
            }
        };
        if self.gpu.is_none() {
            if let Err(error) = self.start(&created) {
                drop(created);
                self.stop();
                return Err(error);
            }
        }
        let gpu = self.gpu.as_ref().expect("started");
        let refused = if !gpu.adapter.is_surface_supported(&created) {
            Some("the adapter can't present to it".to_owned())
        } else {
            let caps = created.get_capabilities(&gpu.adapter);
            let missing = [
                (
                    caps.present_modes.contains(&wgpu::PresentMode::Mailbox),
                    "MAILBOX",
                ),
                (caps.formats.contains(&FORMAT), "Bgra8Unorm"),
                (
                    caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque),
                    "opaque composition",
                ),
            ];
            let missing: Vec<_> = missing
                .iter()
                .filter(|(ok, _)| !ok)
                .map(|(_, n)| *n)
                .collect();
            (!missing.is_empty()).then(|| format!("it offers no {}", missing.join(", ")))
        };
        if let Some(why) = refused {
            drop(created);
            if self.surfaces.is_empty() {
                self.stop();
            }
            return Err(RenderError::Surface(format!("the chrome's surface: {why}")));
        }
        self.surfaces.insert(
            surface,
            Bound {
                surface: created,
                configured: None,
                reconfigure: false,
                reconfigured: false,
                last: None,
            },
        );
        let took = micros(started.elapsed());
        self.stats.max_bind_us = Some(self.stats.max_bind_us.unwrap_or(0).max(took));
        Ok(())
    }

    fn unbind(&mut self, surface: SurfaceKey) {
        let Some(bound) = self.surfaces.remove(&surface) else {
            return;
        };
        let started = Instant::now();
        drop(bound);
        if self.surfaces.is_empty() {
            self.stop();
        }
        let took = micros(started.elapsed());
        self.stats.max_unbind_us = Some(self.stats.max_unbind_us.unwrap_or(0).max(took));
    }

    fn render(
        &mut self,
        surface: SurfaceKey,
        metrics: &Metrics,
        scene: &Scene,
        output: Output<'_>,
    ) -> Result<RenderOutcome, RenderError> {
        let Output::Native = output else {
            return Err(RenderError::Output(
                "the wgpu renderer presents on native surfaces; it never fills pixel memory".into(),
            ));
        };
        if !scene.shaped_with(self.fonts.id()) {
            return Err(RenderError::Output(
                "the scene's text was shaped with another font system".into(),
            ));
        }
        check(&self.fault)?;
        let started = Instant::now();
        let buffer = metrics.buffer();
        let size = (buffer.width, buffer.height);
        let Some(bound) = self.surfaces.get_mut(&surface) else {
            return Err(RenderError::Output("the surface isn't bound".into()));
        };
        if metrics.is_empty() || size.0 == 0 || size.1 == 0 {
            // No room: nothing to configure or present (a 0×0 swapchain is an error).
            self.stats.unchanged += 1;
            return Ok(RenderOutcome::Unchanged);
        }
        if bound
            .last
            .as_ref()
            .is_some_and(|(last, drawn)| last == metrics && drawn == scene)
        {
            self.stats.unchanged += 1;
            return Ok(RenderOutcome::Unchanged);
        }
        let gpu = self.gpu.as_mut().expect("bound surfaces have a device");
        if bound.configured != Some(size) || bound.reconfigure {
            Self::configure(gpu, bound, size, &mut self.stats)?;
        }
        let mut retried = false;
        let texture = loop {
            match bound.surface.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(texture) => {
                    bound.reconfigured = false;
                    break texture;
                }
                wgpu::CurrentSurfaceTexture::Suboptimal(texture) => {
                    // Render this one; configure once before the next (§15).
                    if !bound.reconfigured {
                        bound.reconfigure = true;
                        bound.reconfigured = true;
                    }
                    break texture;
                }
                wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                    self.stats.deferred += 1;
                    return Ok(RenderOutcome::Deferred);
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost
                    if !retried =>
                {
                    retried = true;
                    Self::configure(gpu, bound, size, &mut self.stats)?;
                }
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    bound.last = None;
                    return Err(RenderError::Surface(
                        "the swapchain is outdated or lost after a reconfigure".into(),
                    ));
                }
                wgpu::CurrentSurfaceTexture::Validation => {
                    check(&self.fault)?;
                    return Err(RenderError::Device(
                        "a validation error acquiring the swapchain image".into(),
                    ));
                }
            }
        };
        // Only now that the frame will be drawn: the atlas's uploads must reach the texture
        // with this frame, so a put-off frame must not have planned any.
        let prepared = gpu::prepare(scene, metrics, &mut self.atlas, &self.fonts)
            .map_err(|full| RenderError::Device(full.to_string()))?;
        let view = texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let commands = gpu.device.encode(&view, &prepared);
        gpu.device.queue.submit([commands]);
        gpu.device.queue.present(texture);
        bound.last = Some((*metrics, scene.clone()));
        check(&self.fault)?;
        let took = micros(started.elapsed());
        self.stats.frames += 1;
        self.stats.max_render_us = self.stats.max_render_us.max(took);
        self.stats.last_render_us = took;
        Ok(RenderOutcome::Submitted)
    }

    fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }

    fn stats(&self) -> BackendStats {
        let atlas: AtlasStats = self.atlas.stats();
        BackendStats {
            glyphs: self.atlas.glyphs() as u64,
            glyph_resets: atlas.resets,
            atlas: Some(atlas),
            surfaces: self.surfaces.len() as u64,
            device_bytes: self.gpu.as_ref().map(|gpu| gpu.device.bytes()),
            ..self.stats
        }
    }
}

impl Drop for WgpuBackend {
    fn drop(&mut self) {
        // Surfaces before the device, the device before the instance.
        self.surfaces.clear();
        self.gpu = None;
        self.instance = None;
    }
}

/// The instance: Vulkan only, never a backend wgpu's environment names. Validation and debug
/// labels only with `SIG_VULKAN_VALIDATION=1`, in debug and release builds alike: wgpu's own
/// default turns them on in every debug build, where the Vulkan loader's messages (ICDs that
/// fail to load, say) are then logged as errors at every start. wgpu's `WGPU_*` flag
/// variables still apply on top.
fn instance() -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let validation = std::env::var_os("SIG_VULKAN_VALIDATION").is_some_and(|v| v == "1");
    desc.flags = match validation {
        true => wgpu::InstanceFlags::debugging(),
        false => wgpu::InstanceFlags::empty(),
    }
    .with_env();
    wgpu::Instance::new(desc)
}

/// A `NativeWindow` as the sized handle types wgpu takes.
struct Handles<'a>(&'a dyn NativeWindow);

impl HasDisplayHandle for Handles<'_> {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        self.0.display_handle()
    }
}

impl HasWindowHandle for Handles<'_> {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        self.0.window_handle()
    }
}

/// wgpu's futures on native backends resolve when first polled.
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

/// The fault the device's callbacks reported, if any.
fn check(fault: &Fault) -> Result<(), RenderError> {
    let fault = fault.lock().unwrap_or_else(|e| e.into_inner());
    match &*fault {
        Some(why) => Err(RenderError::Device(why.clone())),
        None => Ok(()),
    }
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(feature = "testing")]
mod testing;
