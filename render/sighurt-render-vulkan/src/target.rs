//! One bound chrome surface: its `VkSurfaceKHR` and the swapchain on it, with an image view, a
//! framebuffer and a render-finished semaphore per image (the presentation engine waits on an
//! image's semaphore, which no queue fence tracks, so each image has its own).
//!
//! Where the device has `VK_EXT_swapchain_maintenance1`, each present signals a fence when the
//! presentation engine is done with it (its semaphore wait included). The fences come from a
//! small pool per swapchain and are taken back once they are seen signalled, without waiting,
//! so the hot path never blocks on one; at most [`PENDING_PRESENTS`] are pending before the
//! oldest is waited for. (An A/B on louise-pc, 6 runs each, found no render-time cost of the
//! fences against the same build without them: bench/results/RENDER.md.) A swapchain, retired by a
//! new one or destroyed, is destroyed only after its pending present fences have signalled, so the presentation engine never uses a freed
//! semaphore or image; without the extension, after the device is idle only (RENDER.md §13's
//! known gap, as in wgpu-hal). A wait that does not end within [`FRAME_TIMEOUT`] leaks the
//! swapchain instead of destroying what may still be in use.
//!
//! Must not own: the device or the frame slot (`crate::device`), or when to render (`crate`).

use ash::vk;
use sighurt_api::geometry::Metrics;
use sighurt_render::{RenderError, Scene};

use crate::device::{device_error, Gpu, FORMAT, FRAME_TIMEOUT};

/// Present fences a swapchain may have pending before a present waits for the oldest.
pub const PENDING_PRESENTS: usize = 8;

/// A swapchain and what each of its images needs.
pub struct Chain {
    pub swapchain: vk::SwapchainKHR,
    pub size: (u32, u32),
    pub framebuffers: Vec<vk::Framebuffer>,
    pub finished: Vec<vk::Semaphore>,
    views: Vec<vk::ImageView>,
    /// With present fences: those of presents queued and not yet seen signalled, oldest
    /// first, and those free (unsignalled). Both empty without them.
    pending: Vec<vk::Fence>,
    spare: Vec<vk::Fence>,
}

/// A bound surface.
pub struct Target {
    pub surface: vk::SurfaceKHR,
    pub chain: Option<Chain>,
    /// Make the swapchain again before the next frame (`SUBOPTIMAL`, an outdated present).
    pub recreate: bool,
    /// The recreate after a `SUBOPTIMAL` happened; not again until a frame is optimal.
    pub recreated: bool,
    /// The last present was out of date (the frame was put off and the swapchain is made
    /// again): a second one in a row is the surface's failure.
    pub present_outdated: bool,
    /// The metrics and scene of the last frame presented.
    pub last: Option<(Metrics, Scene)>,
}

/// Why a surface can't be presented on as the chrome needs: no MAILBOX, no `B8G8R8A8_UNORM`,
/// no opaque composition. `None` when it can.
pub fn refusal(
    surfaces: &ash::khr::surface::Instance,
    physical: vk::PhysicalDevice,
    surface: vk::SurfaceKHR,
) -> Result<Option<String>, RenderError> {
    let err = |what| move |e| device_error(what, e);
    let modes = unsafe { surfaces.get_physical_device_surface_present_modes(physical, surface) }
        .map_err(err("vkGetPhysicalDeviceSurfacePresentModesKHR"))?;
    let formats = unsafe { surfaces.get_physical_device_surface_formats(physical, surface) }
        .map_err(err("vkGetPhysicalDeviceSurfaceFormatsKHR"))?;
    let caps = unsafe { surfaces.get_physical_device_surface_capabilities(physical, surface) }
        .map_err(err("vkGetPhysicalDeviceSurfaceCapabilitiesKHR"))?;
    let mut missing = Vec::new();
    if !modes.contains(&vk::PresentModeKHR::MAILBOX) {
        missing.push("MAILBOX");
    }
    if !formats
        .iter()
        .any(|f| f.format == FORMAT && f.color_space == vk::ColorSpaceKHR::SRGB_NONLINEAR)
    {
        missing.push("B8G8R8A8_UNORM");
    }
    if !caps
        .supported_composite_alpha
        .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
    {
        missing.push("opaque composition");
    }
    Ok((!missing.is_empty()).then(|| format!("it offers no {}", missing.join(", "))))
}

impl Target {
    /// Makes the swapchain at `size`, replacing the one it has (retired through
    /// `oldSwapchain`, destroyed after the device is idle). A size beyond the surface's or the
    /// device's limits is a [`RenderError::Surface`].
    pub fn configure(
        &mut self,
        gpu: &Gpu,
        surfaces: &ash::khr::surface::Instance,
        size: (u32, u32),
    ) -> Result<(), RenderError> {
        let caps = unsafe {
            surfaces.get_physical_device_surface_capabilities(gpu.physical, self.surface)
        }
        .map_err(|e| surface_error("vkGetPhysicalDeviceSurfaceCapabilitiesKHR", e))?;
        let (min, max) = (caps.min_image_extent, caps.max_image_extent);
        let fits = (min.width..=max.width).contains(&size.0)
            && (min.height..=max.height).contains(&size.1);
        if !fits || size.0 > gpu.max_extent || size.1 > gpu.max_extent {
            return Err(RenderError::Surface(format!(
                "{}×{} is beyond the surface's or the device's limits",
                size.0, size.1
            )));
        }
        // One image more than the minimum, as MAILBOX needs to never wait.
        let mut count = caps.min_image_count + 1;
        if caps.max_image_count != 0 {
            count = count.min(caps.max_image_count);
        }
        if self.chain.is_some() && !gpu.idle() {
            // Nothing of ours may still use the old images or their semaphores.
            return Err(RenderError::Device("the device didn't go idle".into()));
        }
        if self.chain.as_ref().is_some_and(|chain| !chain.settled(gpu)) {
            // The presentation engine may still use them: keep the old swapchain alive (it
            // stays this target's) rather than retire it.
            return Err(RenderError::Device(
                "the presentation engine didn't release the swapchain".into(),
            ));
        }
        let old = self.chain.take();
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(FORMAT)
            .image_color_space(vk::ColorSpaceKHR::SRGB_NONLINEAR)
            .image_extent(vk::Extent2D {
                width: size.0,
                height: size.1,
            })
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(vk::PresentModeKHR::MAILBOX)
            .clipped(true)
            .old_swapchain(
                old.as_ref()
                    .map_or(vk::SwapchainKHR::null(), |c| c.swapchain),
            );
        let made = unsafe { gpu.swapchain.create_swapchain(&info, None) };
        if let Some(old) = old {
            unsafe { old.destroy(gpu) };
        }
        let swapchain = made.map_err(|e| surface_error("vkCreateSwapchainKHR", e))?;
        let mut chain = Chain {
            swapchain,
            size,
            framebuffers: Vec::new(),
            finished: Vec::new(),
            views: Vec::new(),
            pending: Vec::new(),
            spare: Vec::new(),
        };
        if let Err(error) = unsafe { chain.fill(gpu) } {
            unsafe { chain.destroy(gpu) };
            return Err(error);
        }
        self.chain = Some(chain);
        self.recreate = false;
        Ok(())
    }

    /// Destroys the swapchain and the surface. False if the presentation engine did not
    /// release the swapchain in time: then both are left alive, and so must be the device and
    /// the instance.
    ///
    /// # Safety
    ///
    /// The device is idle (nothing of this surface's is in flight).
    #[must_use]
    pub unsafe fn destroy(
        mut self,
        gpu: Option<&Gpu>,
        surfaces: &ash::khr::surface::Instance,
    ) -> bool {
        if let (Some(chain), Some(gpu)) = (self.chain.take(), gpu) {
            if !chain.settled(gpu) {
                log::error!(
                    "the presentation engine didn't release a chrome swapchain; it and its \
                     surface are left alive"
                );
                std::mem::forget(chain);
                return false;
            }
            unsafe { chain.destroy(gpu) };
        }
        unsafe { surfaces.destroy_surface(self.surface, None) };
        true
    }
}

impl Chain {
    /// Views, framebuffers and semaphores for every image of the swapchain.
    unsafe fn fill(&mut self, gpu: &Gpu) -> Result<(), RenderError> {
        let d = &gpu.device;
        let images = unsafe { gpu.swapchain.get_swapchain_images(self.swapchain) }
            .map_err(|e| device_error("vkGetSwapchainImagesKHR", e))?;
        for image in images {
            let info = vk::ImageViewCreateInfo::default()
                .image(image)
                .view_type(vk::ImageViewType::TYPE_2D)
                .format(FORMAT)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: 0,
                    layer_count: 1,
                });
            let view = unsafe { d.create_image_view(&info, None) }
                .map_err(|e| device_error("vkCreateImageView", e))?;
            self.views.push(view);
            let attachments = [view];
            let info = vk::FramebufferCreateInfo::default()
                .render_pass(gpu.present_pass)
                .attachments(&attachments)
                .width(self.size.0)
                .height(self.size.1)
                .layers(1);
            let framebuffer = unsafe { d.create_framebuffer(&info, None) }
                .map_err(|e| device_error("vkCreateFramebuffer", e))?;
            self.framebuffers.push(framebuffer);
            let semaphore =
                unsafe { d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
                    .map_err(|e| device_error("vkCreateSemaphore", e))?;
            self.finished.push(semaphore);
        }
        Ok(())
    }

    /// An unsignalled fence for the next present, from now on pending; `None` without present
    /// fences. Pending fences seen signalled are taken back first, without waiting; only when
    /// [`PENDING_PRESENTS`] are still pending is the oldest waited for (bounded).
    pub fn present_fence(&mut self, gpu: &Gpu) -> Result<Option<vk::Fence>, RenderError> {
        if !gpu.present_fences {
            return Ok(None);
        }
        let d = &gpu.device;
        if self.pending.len() >= PENDING_PRESENTS {
            let timeout = FRAME_TIMEOUT.as_nanos() as u64;
            unsafe { d.wait_for_fences(&self.pending[..1], true, timeout) }
                .map_err(|e| device_error("vkWaitForFences (a present)", e))?;
        }
        let mut index = 0;
        while index < self.pending.len() {
            let fence = self.pending[index];
            match unsafe { d.get_fence_status(fence) } {
                Ok(true) => {
                    unsafe { d.reset_fences(&[fence]) }
                        .map_err(|e| device_error("vkResetFences", e))?;
                    self.spare.push(self.pending.remove(index));
                }
                Ok(false) => index += 1,
                Err(e) => return Err(device_error("vkGetFenceStatus", e)),
            }
        }
        let fence = match self.spare.pop() {
            Some(fence) => fence,
            None => unsafe { d.create_fence(&vk::FenceCreateInfo::default(), None) }
                .map_err(|e| device_error("vkCreateFence", e))?,
        };
        self.pending.push(fence);
        Ok(Some(fence))
    }

    /// After a present that was refused before anything was queued: its fence, the last one
    /// taken, is not pending (and still unsignalled).
    pub fn unpend(&mut self) {
        if let Some(fence) = self.pending.pop() {
            self.spare.push(fence);
        }
    }

    /// Whether the presentation engine is done with every present of this swapchain that has
    /// a fence (bounded by [`FRAME_TIMEOUT`]; device loss counts as done, since nothing runs
    /// after it). Always true without present fences.
    fn settled(&self, gpu: &Gpu) -> bool {
        if self.pending.is_empty() {
            return true;
        }
        let timeout = FRAME_TIMEOUT.as_nanos() as u64;
        matches!(
            unsafe { gpu.device.wait_for_fences(&self.pending, true, timeout) },
            Ok(()) | Err(vk::Result::ERROR_DEVICE_LOST)
        )
    }

    /// The number of images the swapchain has: what the driver made, not what was asked.
    pub fn images(&self) -> usize {
        self.views.len()
    }

    /// # Safety
    ///
    /// Nothing in flight uses it (the device is idle).
    unsafe fn destroy(self, gpu: &Gpu) {
        let d = &gpu.device;
        unsafe {
            for semaphore in self.finished {
                d.destroy_semaphore(semaphore, None);
            }
            for framebuffer in self.framebuffers {
                d.destroy_framebuffer(framebuffer, None);
            }
            for view in self.views {
                d.destroy_image_view(view, None);
            }
            for fence in self.pending.into_iter().chain(self.spare) {
                d.destroy_fence(fence, None);
            }
            gpu.swapchain.destroy_swapchain(self.swapchain, None);
        }
    }
}

/// A surface call's failure: device loss is the device's, the rest the surface's.
pub fn surface_error(what: &str, result: vk::Result) -> RenderError {
    match result {
        vk::Result::ERROR_DEVICE_LOST => device_error(what, result),
        _ => RenderError::Surface(format!("{what}: {result}")),
    }
}
