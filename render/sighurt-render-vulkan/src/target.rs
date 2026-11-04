//! One bound chrome surface: its `VkSurfaceKHR` and the swapchain on it, with an image view, a
//! framebuffer and a render-finished semaphore per image (the presentation engine waits on an
//! image's semaphore, which no fence tracks, so each image has its own).
//!
//! Must not own: the device or the frame slot (`crate::device`), or when to render (`crate`).

use ash::vk;
use sighurt_api::geometry::Metrics;
use sighurt_render::{RenderError, Scene};

use crate::device::{device_error, Gpu, FORMAT};

/// A swapchain and what each of its images needs.
pub struct Chain {
    pub swapchain: vk::SwapchainKHR,
    pub size: (u32, u32),
    pub framebuffers: Vec<vk::Framebuffer>,
    pub finished: Vec<vk::Semaphore>,
    views: Vec<vk::ImageView>,
}

/// A bound surface.
pub struct Target {
    pub surface: vk::SurfaceKHR,
    pub chain: Option<Chain>,
    /// Make the swapchain again before the next frame (`SUBOPTIMAL`, an outdated present).
    pub recreate: bool,
    /// The recreate after a `SUBOPTIMAL` happened; not again until an acquire is optimal.
    pub recreated: bool,
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
        };
        if let Err(error) = unsafe { chain.fill(gpu) } {
            unsafe { chain.destroy(gpu) };
            return Err(error);
        }
        self.chain = Some(chain);
        self.recreate = false;
        Ok(())
    }

    /// Destroys the swapchain and the surface.
    ///
    /// # Safety
    ///
    /// The device is idle (nothing of this surface's is in flight).
    pub unsafe fn destroy(mut self, gpu: Option<&Gpu>, surfaces: &ash::khr::surface::Instance) {
        if let (Some(chain), Some(gpu)) = (self.chain.take(), gpu) {
            unsafe { chain.destroy(gpu) };
        }
        unsafe { surfaces.destroy_surface(self.surface, None) };
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
