//! Rendering offscreen and reading the pixels back, for tests only (feature `testing`): the
//! comparison of this backend's drawing with the CPU backend's. Never on the display path; an error
//! part-way leaks the temporary image and buffer (tests only).

use ash::vk;
use sighurt_api::geometry::Metrics;
use sighurt_render::gpu;
use sighurt_render::{FontSystemHandle, RenderError, Scene};

use crate::device::{device_error, FORMAT};
use crate::VulkanBackend;

impl VulkanBackend {
    /// A backend with a device and no surface, on the first graphics device (a CPU one too):
    /// for [`render_to_image`](Self::render_to_image). `Err` without a loader or a device.
    pub fn headless(fonts: FontSystemHandle) -> Result<Self, String> {
        let mut backend = Self::new(fonts, true)?;
        backend.start(None).map_err(|e| e.to_string())?;
        Ok(backend)
    }

    /// `scene` rendered for `metrics` into an image of their buffer's size, read back as rows
    /// of RGBA bytes. Shares the atlas and the frame slot with the surfaces' renders.
    pub fn render_to_image(
        &mut self,
        metrics: &Metrics,
        scene: &Scene,
    ) -> Result<Vec<u8>, RenderError> {
        let gpu = self
            .gpu
            .as_mut()
            .ok_or_else(|| RenderError::Device("no device".into()))?;
        let prepared = gpu::prepare(scene, metrics, &mut self.atlas, &self.fonts)
            .map_err(|full| RenderError::Device(full.to_string()))?;
        let (width, height) = prepared.size;
        if width == 0 || height == 0 {
            return Ok(Vec::new());
        }
        gpu.wait()?;
        let d = gpu.device.clone();
        let err = |what| move |e| device_error(what, e);
        // The target image, its memory, view and framebuffer; a host-visible buffer to read.
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(FORMAT)
            .extent(vk::Extent3D {
                width,
                height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let instance = &self.vulkan.as_ref().expect("an instance").instance;
        let memory_types = unsafe { instance.get_physical_device_memory_properties(gpu.physical) };
        let find = |bits: u32, flags: vk::MemoryPropertyFlags| {
            (0..memory_types.memory_type_count).find(|&i| {
                bits & (1 << i) != 0
                    && memory_types.memory_types[i as usize]
                        .property_flags
                        .contains(flags)
            })
        };
        unsafe {
            let image = d.create_image(&info, None).map_err(err("vkCreateImage"))?;
            let needs = d.get_image_memory_requirements(image);
            let index = find(
                needs.memory_type_bits,
                vk::MemoryPropertyFlags::DEVICE_LOCAL,
            )
            .ok_or_else(|| RenderError::Device("no device-local memory".into()))?;
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(needs.size)
                .memory_type_index(index);
            let image_memory = d
                .allocate_memory(&alloc, None)
                .map_err(err("vkAllocateMemory"))?;
            d.bind_image_memory(image, image_memory, 0)
                .map_err(err("vkBindImageMemory"))?;
            let view_info = vk::ImageViewCreateInfo::default()
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
            let view = d
                .create_image_view(&view_info, None)
                .map_err(err("vkCreateImageView"))?;
            let attachments = [view];
            let fb_info = vk::FramebufferCreateInfo::default()
                .render_pass(gpu.offscreen_pass)
                .attachments(&attachments)
                .width(width)
                .height(height)
                .layers(1);
            let framebuffer = d
                .create_framebuffer(&fb_info, None)
                .map_err(err("vkCreateFramebuffer"))?;
            let bytes = u64::from(width) * u64::from(height) * 4;
            let buffer_info = vk::BufferCreateInfo::default()
                .size(bytes)
                .usage(vk::BufferUsageFlags::TRANSFER_DST)
                .sharing_mode(vk::SharingMode::EXCLUSIVE);
            let buffer = d
                .create_buffer(&buffer_info, None)
                .map_err(err("vkCreateBuffer"))?;
            let needs = d.get_buffer_memory_requirements(buffer);
            let flags =
                vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
            let index = find(needs.memory_type_bits, flags)
                .ok_or_else(|| RenderError::Device("no host-visible memory".into()))?;
            let alloc = vk::MemoryAllocateInfo::default()
                .allocation_size(needs.size)
                .memory_type_index(index);
            let buffer_memory = d
                .allocate_memory(&alloc, None)
                .map_err(err("vkAllocateMemory"))?;
            d.bind_buffer_memory(buffer, buffer_memory, 0)
                .map_err(err("vkBindBufferMemory"))?;

            gpu.record(gpu.offscreen_pass, framebuffer, &prepared)?;
            // The pass left the image in TRANSFER_SRC_OPTIMAL.
            let region = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D::default(),
                image_extent: vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                },
            };
            let layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
            d.cmd_copy_image_to_buffer(gpu.command, image, layout, buffer, &[region]);
            let to_host = vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::HOST_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(buffer)
                .offset(0)
                .size(vk::WHOLE_SIZE);
            d.cmd_pipeline_barrier(
                gpu.command,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::HOST,
                vk::DependencyFlags::empty(),
                &[],
                &[to_host],
                &[],
            );
            gpu.submit(None, None)?;
            gpu.wait()?;
            let mapped = d
                .map_memory(buffer_memory, 0, bytes, vk::MemoryMapFlags::empty())
                .map_err(err("vkMapMemory"))?;
            let pixels = std::slice::from_raw_parts(mapped.cast::<u8>(), bytes as usize);
            // B8G8R8A8 to RGBA.
            let rgba = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|&[b, g, r, a]| [r, g, b, a])
                .collect();
            d.unmap_memory(buffer_memory);
            d.destroy_buffer(buffer, None);
            d.free_memory(buffer_memory, None);
            d.destroy_framebuffer(framebuffer, None);
            d.destroy_image_view(view, None);
            d.destroy_image(image, None);
            d.free_memory(image_memory, None);
            Ok(rgba)
        }
    }
}
