//! The Vulkan chrome renderer (`[ui] renderer = "vulkan"`): ash 0.38 straight on Vulkan,
//! presenting each chrome surface through a swapchain on the host's native target
//! (docs/rewrite/RENDER.md §8, as amended by §14.13 and §15). Opt-in (`render-vulkan`), never
//! chosen by `auto`. It draws what every GPU backend draws, the same way: `sighurt_render::gpu`
//! prepares the frame and plans the atlas, and the shared WGSL program is compiled to SPIR-V
//! by `build.rs` with naga.
//!
//! - **The loader is opened at run time** ([`VulkanBackend::new`]: `ash::Entry::load`, then an
//!   instance with `VK_KHR_surface` and `VK_KHR_wayland_surface`, and the Khronos validation
//!   layer when `SIG_VULKAN_VALIDATION=1` and it is installed). Without a loader the backend
//!   can't start and the frontend says so.
//! - **The first bind chooses the device**: `vkCreateWaylandSurfaceKHR` from the raw display
//!   and surface, then a physical device with a queue family that does graphics and presents
//!   to that surface and has `VK_KHR_swapchain` (low power first; a CPU device only when the
//!   renderer was named), and the device. A surface without MAILBOX, `B8G8R8A8_UNORM` or opaque
//!   composition is refused ([`RenderError::Surface`], and the frontend falls back).
//! - **Swapchains are made lazily** by the first render at the permit's metrics, again on a new
//!   size and after `SUBOPTIMAL` (once) or `OUT_OF_DATE`. One image more than the surface's
//!   minimum, MAILBOX.
//! - **One frame in flight per backend**: a render first waits for the last frame's fence, so
//!   the shared atlas images, descriptor set, uniform, vertex, index and staging buffers are
//!   never used by two frames. The acquire has a zero timeout: `NOT_READY` or `TIMEOUT` is
//!   [`RenderOutcome::Deferred`], and the fence is reset only when a submission follows. A
//!   render-finished semaphore per swapchain image; the acquire semaphore and fence are the
//!   frame slot's.
//! - **One render per permit, never on its own clock**; presenting (`vkQueuePresentKHR`)
//!   commits the `wl_surface` with the frame callback the host requested at the permit.
//! - **The viewport is a uniform buffer**, not a push constant: the shared program declares it
//!   so (`sighurt_render::gpu`), and every backend uses the program unchanged.
//! - **No readback on the display path**: [`Output::Pixels`] is refused. With `testing`,
//!   [`VulkanBackend::render_to_image`] renders offscreen and reads it back.
//! - **Lifetimes**: `unbind` waits for the device to be idle, then destroys the swapchain and
//!   the surface; after the last unbind the device and the instance are destroyed too.
//!   Dropping the backend unbinds everything first.
//!
//! Must not own: what is drawn (the chrome's), meshes, atlas planning and the shader source
//! (`sighurt_render::gpu`), or target lifetimes (the frontend's and the host's).

mod device;
mod spirv;
mod target;

use std::collections::BTreeMap;
use std::ffi::{c_char, CStr};
use std::time::{Duration, Instant};

use ash::vk;
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};
use sighurt_api::geometry::Metrics;
use sighurt_render::gpu::{self, Atlas};
use sighurt_render::{
    Backend, BackendStats, FontSystemHandle, NativeWindow, Output, Presents, RenderError,
    RenderOutcome, Scene, SurfaceKey,
};

use crate::device::{device_error, Gpu};
use crate::target::{refusal, surface_error, Target};

const VALIDATION: &CStr = c"VK_LAYER_KHRONOS_validation";

/// The instance and its surface functions.
struct Vulkan {
    instance: ash::Instance,
    surfaces: ash::khr::surface::Instance,
    wayland: ash::khr::wayland_surface::Instance,
    api: u32,
    /// The Khronos validation layer is on.
    validation: bool,
}

/// The Vulkan backend. One serves every chrome surface of a window.
pub struct VulkanBackend {
    fonts: FontSystemHandle,
    /// `[ui] renderer = "vulkan"` named it: a CPU device may be used, with a notice.
    explicit: bool,
    entry: ash::Entry,
    vulkan: Option<Vulkan>,
    gpu: Option<Gpu>,
    targets: BTreeMap<SurfaceKey, Target>,
    atlas: Atlas,
    /// The device was lost: nothing renders again.
    lost: Option<String>,
    notices: Vec<String>,
    swapchain_images: Option<u32>,
    stats: BackendStats,
}

impl VulkanBackend {
    /// Opens the Vulkan loader and makes an instance; the device waits for the first bind.
    /// `Err` when there is no loader or it lacks the Wayland surface extension.
    pub fn new(fonts: FontSystemHandle, explicit: bool) -> Result<Self, String> {
        // SAFETY: loading the system's Vulkan loader runs its initialization; nothing else
        // in the process depends on it not being loaded.
        let entry = unsafe { ash::Entry::load() }.map_err(|e| format!("no Vulkan loader: {e}"))?;
        let mut backend = Self {
            fonts,
            explicit,
            entry,
            vulkan: None,
            gpu: None,
            targets: BTreeMap::new(),
            atlas: Atlas::new(),
            lost: None,
            notices: Vec::new(),
            swapchain_images: None,
            stats: BackendStats::default(),
        };
        backend.vulkan = Some(backend.instance()?);
        Ok(backend)
    }

    /// An instance with the surface extensions, and the validation layer if asked for.
    fn instance(&mut self) -> Result<Vulkan, String> {
        let entry = &self.entry;
        let api = match unsafe { entry.try_enumerate_instance_version() } {
            Ok(Some(version)) => version.min(vk::API_VERSION_1_3),
            _ => vk::API_VERSION_1_0,
        };
        let available = unsafe { entry.enumerate_instance_extension_properties(None) }
            .map_err(|e| format!("vkEnumerateInstanceExtensionProperties: {e}"))?;
        let has = |name: &CStr| {
            available
                .iter()
                .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
        };
        let wanted = [ash::khr::surface::NAME, ash::khr::wayland_surface::NAME];
        if let Some(missing) = wanted.iter().find(|name| !has(name)) {
            return Err(format!(
                "the Vulkan loader lacks {}",
                missing.to_string_lossy()
            ));
        }
        let mut layers: Vec<*const c_char> = Vec::new();
        if std::env::var_os("SIG_VULKAN_VALIDATION").is_some_and(|v| v == "1") {
            let installed = unsafe { entry.enumerate_instance_layer_properties() }
                .unwrap_or_default()
                .iter()
                .any(|l| l.layer_name_as_c_str().is_ok_and(|n| n == VALIDATION));
            if installed {
                layers.push(VALIDATION.as_ptr());
            } else {
                self.notices.push(
                    "SIG_VULKAN_VALIDATION=1, but the Khronos validation layer isn't installed"
                        .into(),
                );
            }
        }
        let app = vk::ApplicationInfo::default()
            .application_name(c"sighurt")
            .engine_name(c"sighurt-render-vulkan")
            .api_version(api);
        let extensions = wanted.map(CStr::as_ptr);
        let info = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_extension_names(&extensions)
            .enabled_layer_names(&layers);
        let instance = unsafe { entry.create_instance(&info, None) }
            .map_err(|e| format!("vkCreateInstance: {e}"))?;
        Ok(Vulkan {
            surfaces: ash::khr::surface::Instance::new(entry, &instance),
            wayland: ash::khr::wayland_surface::Instance::new(entry, &instance),
            instance,
            api,
            validation: !layers.is_empty(),
        })
    }

    /// Chooses the physical device and queue family for `surface` (or any graphics device
    /// without one), and makes the device.
    fn start(&mut self, surface: Option<vk::SurfaceKHR>) -> Result<(), RenderError> {
        let vulkan = self.vulkan.as_ref().expect("an instance");
        let instance = &vulkan.instance;
        let physicals = unsafe { instance.enumerate_physical_devices() }
            .map_err(|e| device_error("vkEnumeratePhysicalDevices", e))?;
        let rank = |kind: vk::PhysicalDeviceType| match kind {
            vk::PhysicalDeviceType::INTEGRATED_GPU => 0,
            vk::PhysicalDeviceType::DISCRETE_GPU => 1,
            vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
            vk::PhysicalDeviceType::CPU => 4,
            _ => 3,
        };
        let mut best: Option<(u32, vk::PhysicalDevice, u32)> = None;
        for physical in physicals {
            let properties = unsafe { instance.get_physical_device_properties(physical) };
            let kind = rank(properties.device_type);
            if kind == 4 && !self.explicit {
                continue;
            }
            let extensions = unsafe { instance.enumerate_device_extension_properties(physical) }
                .unwrap_or_default();
            let swapchain = extensions.iter().any(|e| {
                e.extension_name_as_c_str()
                    .is_ok_and(|n| n == ash::khr::swapchain::NAME)
            });
            if !swapchain {
                continue;
            }
            let families =
                unsafe { instance.get_physical_device_queue_family_properties(physical) };
            let family = (0..families.len() as u32).find(|&i| {
                let graphics = families[i as usize]
                    .queue_flags
                    .contains(vk::QueueFlags::GRAPHICS);
                let presents = surface.is_none_or(|surface| unsafe {
                    vulkan
                        .surfaces
                        .get_physical_device_surface_support(physical, i, surface)
                        .unwrap_or(false)
                });
                graphics && presents
            });
            if let Some(family) = family {
                if best.is_none_or(|(r, ..)| kind < r) {
                    best = Some((kind, physical, family));
                }
            }
        }
        let Some((kind, physical, family)) = best else {
            return Err(RenderError::Device(
                "no Vulkan device can present to the chrome's surface (CPU devices are used \
                 only when [ui] renderer = \"vulkan\" names this renderer)"
                    .into(),
            ));
        };
        let describe = describe(vulkan, physical);
        if kind == 4 {
            self.notices.push(format!(
                "The chrome is drawn by Vulkan on a CPU device ({describe}): slower than the \
                 cpu renderer."
            ));
        }
        // SAFETY: the instance outlives the device (`stop` destroys the device first).
        let gpu = unsafe { Gpu::new(instance, physical, family, describe) }?;
        self.gpu = Some(gpu);
        Ok(())
    }

    /// Destroys every target, the device and the instance, and the atlas planned for them.
    fn stop(&mut self) {
        let surfaces = self.vulkan.as_ref().map(|v| v.surfaces.clone());
        if self.gpu.as_ref().is_some_and(|gpu| !gpu.idle()) {
            // Something may still be in flight: leak rather than free what it uses.
            log::error!("the Vulkan device didn't go idle; its objects are left alive");
            std::mem::forget(std::mem::take(&mut self.targets));
            std::mem::forget(self.gpu.take());
            // The instance is left alive too (dropping it destroys nothing).
            self.vulkan = None;
            return;
        }
        for (_, target) in std::mem::take(&mut self.targets) {
            if let Some(surfaces) = &surfaces {
                // SAFETY: the device is idle.
                unsafe { target.destroy(self.gpu.as_ref(), surfaces) };
            }
        }
        if let Some(mut gpu) = self.gpu.take() {
            // SAFETY: every swapchain is gone.
            unsafe { gpu.destroy() };
        }
        if let Some(vulkan) = self.vulkan.take() {
            unsafe { vulkan.instance.destroy_instance(None) };
        }
        self.atlas = Atlas::new();
        self.lost = None;
    }

    /// `result` with device loss remembered: nothing renders after it.
    fn lose(&mut self, error: RenderError) -> RenderError {
        if let RenderError::Device(why) = &error {
            self.lost.get_or_insert_with(|| why.clone());
        }
        error
    }
}

impl Backend for VulkanBackend {
    fn name(&self) -> &'static str {
        "vulkan"
    }

    fn presents(&self) -> Presents {
        Presents::Native
    }

    fn describe(&self) -> String {
        match &self.gpu {
            Some(gpu) => gpu.describe.clone(),
            None => "Vulkan (ash 0.38): the device is chosen at the first surface".into(),
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
        if self.vulkan.is_none() {
            self.vulkan = Some(self.instance().map_err(RenderError::Device)?);
        }
        let display = window
            .display_handle()
            .map_err(|e| RenderError::Surface(format!("the window's display: {e}")))?;
        let handle = window
            .window_handle()
            .map_err(|e| RenderError::Surface(format!("the window: {e}")))?;
        let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(handle)) =
            (display.as_raw(), handle.as_raw())
        else {
            return Err(RenderError::Surface("not a Wayland window".into()));
        };
        let info = vk::WaylandSurfaceCreateInfoKHR::default()
            .display(display.display.as_ptr())
            .surface(handle.surface.as_ptr());
        let vulkan = self.vulkan.as_ref().expect("made above");
        // SAFETY: the caller keeps the wl_display and wl_surface alive until `unbind`
        // (`Backend::bind`'s contract), which destroys this surface.
        let created = unsafe { vulkan.wayland.create_wayland_surface(&info, None) }
            .map_err(|e| surface_error("vkCreateWaylandSurfaceKHR", e));
        let created = match created {
            Ok(created) => created,
            Err(error) => {
                if self.targets.is_empty() {
                    self.stop();
                }
                return Err(error);
            }
        };
        let surfaces = vulkan.surfaces.clone();
        let refused = (|| {
            if self.gpu.is_none() {
                self.start(Some(created))?;
            }
            let gpu = self.gpu.as_ref().expect("started");
            let supported = unsafe {
                surfaces.get_physical_device_surface_support(gpu.physical, gpu.family, created)
            }
            .map_err(|e| surface_error("vkGetPhysicalDeviceSurfaceSupportKHR", e))?;
            if !supported {
                return Err(RenderError::Surface(
                    "the chrome's surface: the device's queue can't present to it".into(),
                ));
            }
            match refusal(&surfaces, gpu.physical, created)? {
                Some(why) => Err(RenderError::Surface(format!("the chrome's surface: {why}"))),
                None => Ok(()),
            }
        })();
        if let Err(error) = refused {
            unsafe { surfaces.destroy_surface(created, None) };
            if self.targets.is_empty() {
                self.stop();
            }
            return Err(error);
        }
        self.targets.insert(
            surface,
            Target {
                surface: created,
                chain: None,
                recreate: false,
                recreated: false,
                last: None,
            },
        );
        let took = micros(started.elapsed());
        self.stats.max_bind_us = Some(self.stats.max_bind_us.unwrap_or(0).max(took));
        Ok(())
    }

    fn forget_frame(&mut self, surface: SurfaceKey) {
        // The binding and the swapchain stay (RENDER.md §15); only the frame the surface last
        // showed is forgotten, so the next render after a hide is not found unchanged.
        if let Some(target) = self.targets.get_mut(&surface) {
            target.last = None;
        }
    }

    fn unbind(&mut self, surface: SurfaceKey) {
        let Some(target) = self.targets.remove(&surface) else {
            return;
        };
        let started = Instant::now();
        if self.gpu.as_ref().is_some_and(|gpu| !gpu.idle()) {
            log::error!("the Vulkan device didn't go idle; the surface's objects are left alive");
            std::mem::forget(target);
            return;
        }
        if let Some(vulkan) = &self.vulkan {
            // SAFETY: the device is idle.
            unsafe { target.destroy(self.gpu.as_ref(), &vulkan.surfaces) };
        }
        if self.targets.is_empty() {
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
                "the vulkan renderer presents on native surfaces; it never fills pixel memory"
                    .into(),
            ));
        };
        if !scene.shaped_with(self.fonts.id()) {
            return Err(RenderError::Output(
                "the scene's text was shaped with another font system".into(),
            ));
        }
        if let Some(why) = &self.lost {
            return Err(RenderError::Device(why.clone()));
        }
        let started = Instant::now();
        let buffer = metrics.buffer();
        let size = (buffer.width, buffer.height);
        let Some(target) = self.targets.get(&surface) else {
            return Err(RenderError::Output("the surface isn't bound".into()));
        };
        if metrics.is_empty() || size.0 == 0 || size.1 == 0 {
            self.stats.unchanged += 1;
            return Ok(RenderOutcome::Unchanged);
        }
        if target
            .last
            .as_ref()
            .is_some_and(|(last, drawn)| last == metrics && drawn == scene)
        {
            self.stats.unchanged += 1;
            return Ok(RenderOutcome::Unchanged);
        }
        match self.frame(surface, metrics, scene, size) {
            Ok(RenderOutcome::Submitted) => {
                let took = micros(started.elapsed());
                self.stats.frames += 1;
                self.stats.max_render_us = self.stats.max_render_us.max(took);
                self.stats.last_render_us = took;
                Ok(RenderOutcome::Submitted)
            }
            Ok(outcome) => Ok(outcome),
            Err(error) => Err(self.lose(error)),
        }
    }

    fn stats(&self) -> BackendStats {
        let atlas = self.atlas.stats();
        BackendStats {
            glyphs: self.atlas.glyphs() as u64,
            glyph_resets: atlas.resets,
            atlas: Some(atlas),
            surfaces: self.targets.len() as u64,
            swapchain_images: self.swapchain_images,
            device_bytes: self.gpu.as_ref().map(|gpu| gpu.bytes),
            ..self.stats
        }
    }

    fn take_notices(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notices)
    }
}

impl VulkanBackend {
    /// One frame on `surface`: the swapchain made if needed, the previous frame waited for, an
    /// image acquired without waiting, the frame prepared, recorded, submitted and presented.
    fn frame(
        &mut self,
        surface: SurfaceKey,
        metrics: &Metrics,
        scene: &Scene,
        size: (u32, u32),
    ) -> Result<RenderOutcome, RenderError> {
        let surfaces = self.vulkan.as_ref().expect("bound").surfaces.clone();
        let gpu = self.gpu.as_mut().expect("bound surfaces have a device");
        let target = self.targets.get_mut(&surface).expect("checked");
        gpu.wait()?;
        let stale = target.chain.as_ref().is_none_or(|c| c.size != size);
        if stale || target.recreate {
            let configured = Instant::now();
            target.configure(gpu, &surfaces, size)?;
            self.stats.configures += 1;
            let took = micros(configured.elapsed());
            self.stats.max_configure_us = Some(self.stats.max_configure_us.unwrap_or(0).max(took));
            self.swapchain_images = target.chain.as_ref().map(|c| c.images() as u32);
        }
        let mut retried = false;
        let (index, suboptimal) = loop {
            let chain = target.chain.as_ref().expect("configured");
            let acquired = unsafe {
                gpu.swapchain.acquire_next_image(
                    chain.swapchain,
                    0,
                    gpu.acquired,
                    vk::Fence::null(),
                )
            };
            match acquired {
                Ok(acquired) => break acquired,
                Err(vk::Result::NOT_READY | vk::Result::TIMEOUT) => {
                    self.stats.deferred += 1;
                    return Ok(RenderOutcome::Deferred);
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) if !retried => {
                    retried = true;
                    target.configure(gpu, &surfaces, size)?;
                    self.stats.configures += 1;
                }
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                    target.last = None;
                    return Err(RenderError::Surface(
                        "the swapchain is out of date after it was made again".into(),
                    ));
                }
                Err(e) => return Err(surface_error("vkAcquireNextImageKHR", e)),
            }
        };
        if suboptimal {
            // Draw this one; make the swapchain again once before the next (§15).
            if !target.recreated {
                target.recreate = true;
                target.recreated = true;
            }
        } else {
            target.recreated = false;
        }
        // The image is acquired: from here on a submission follows, so the acquire semaphore
        // is always waited on.
        let chain = target.chain.as_ref().expect("configured");
        let framebuffer = chain.framebuffers[index as usize];
        let finished = chain.finished[index as usize];
        let drawn = gpu::prepare(scene, metrics, &mut self.atlas, &self.fonts)
            .map_err(|full| RenderError::Device(full.to_string()))
            .and_then(|prepared| gpu.record(gpu.present_pass, framebuffer, &prepared))
            .and_then(|()| gpu.submit(Some(gpu.acquired), Some(finished)));
        if let Err(error) = drawn {
            // The acquire's signal must still be waited on before the semaphore is destroyed
            // or used again: an empty submission waits on it.
            gpu.drain();
            return Err(error);
        }
        let waits = [finished];
        let swapchains = [chain.swapchain];
        let indices = [index];
        let info = vk::PresentInfoKHR::default()
            .wait_semaphores(&waits)
            .swapchains(&swapchains)
            .image_indices(&indices);
        match unsafe { gpu.swapchain.queue_present(gpu.queue, &info) } {
            Ok(false) => {}
            Ok(true) => {
                // Shown, but suboptimal: once, as on acquire.
                if !target.recreated {
                    target.recreate = true;
                    target.recreated = true;
                }
            }
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                // Not shown, so no commit carried the host's frame request: put off, with the
                // swapchain made again before the retry.
                target.recreate = true;
                target.last = None;
                self.stats.deferred += 1;
                return Ok(RenderOutcome::Deferred);
            }
            Err(e) => return Err(surface_error("vkQueuePresentKHR", e)),
        }
        target.last = Some((*metrics, scene.clone()));
        Ok(RenderOutcome::Submitted)
    }
}

impl Drop for VulkanBackend {
    fn drop(&mut self) {
        // Surfaces and swapchains, then the device, then the instance.
        self.stop();
    }
}

/// What a device is, for `describe()`: its name and type, the Vulkan version, and the driver's
/// name and version where Vulkan 1.2 says them.
fn describe(vulkan: &Vulkan, physical: vk::PhysicalDevice) -> String {
    let instance = &vulkan.instance;
    let properties = unsafe { instance.get_physical_device_properties(physical) };
    let version = |v: u32| {
        format!(
            "{}.{}.{}",
            vk::api_version_major(v),
            vk::api_version_minor(v),
            vk::api_version_patch(v)
        )
    };
    let api = properties.api_version.min(vulkan.api);
    let mut driver = String::new();
    if api >= vk::API_VERSION_1_2 {
        let mut props = vk::PhysicalDeviceDriverProperties::default();
        let mut all = vk::PhysicalDeviceProperties2::default().push_next(&mut props);
        unsafe { instance.get_physical_device_properties2(physical, &mut all) };
        let text = |s: Result<&CStr, _>| {
            s.map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        driver = format!(
            ", driver {} {}",
            text(props.driver_name_as_c_str()),
            text(props.driver_info_as_c_str())
        );
    }
    let validation = if vulkan.validation {
        ", validation layer on"
    } else {
        ""
    };
    format!(
        "Vulkan {} (ash 0.38): {} ({:?}){driver}{validation}",
        version(api),
        device::name(&properties),
        properties.device_type
    )
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

#[cfg(feature = "testing")]
mod testing;
