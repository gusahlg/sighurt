//! The backend's device and what draws on it: the logical device and its queue, the pipeline
//! made from the shared program's SPIR-V, the atlas images, the host-visible vertex, index,
//! uniform and staging buffers, and the one frame slot (a command buffer, its fence and the
//! acquire semaphore). One frame is in flight per backend: [`Gpu::wait`] waits for the last
//! one before anything it used (the atlas images, the descriptor set, the buffers) is written
//! again, so no two frames ever share them.
//!
//! Must not own: surfaces and swapchains (`crate::target`), what a frame holds
//! (`sighurt_render::gpu`), or choosing the device (`crate`).

use std::ffi::CStr;
use std::time::Duration;

use ash::vk;
use sighurt_render::gpu::{
    AttributeFormat, PageKind, Prepared, FRAGMENT_ENTRY, UNIFORM_SIZE, VERTEX_ATTRIBUTES,
    VERTEX_ENTRY, VERTEX_STRIDE,
};
use sighurt_render::RenderError;

/// The format of every chrome target: opaque, not sRGB (RENDER.md §14.12).
pub const FORMAT: vk::Format = vk::Format::B8G8R8A8_UNORM;
/// The longest a frame may take before the device is given up on.
const FRAME_TIMEOUT: Duration = Duration::from_secs(2);

/// A Vulkan error as the frontend acts on it: device loss and anything unexpected are the
/// device's; `what` says which call.
pub fn device_error(what: &str, result: vk::Result) -> RenderError {
    RenderError::Device(format!("{what}: {result}"))
}

/// A buffer in host-visible memory, mapped for as long as it lives.
struct Buffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: u64,
    mapped: *mut u8,
    /// Whether writes need a flush (memory without `HOST_COHERENT`).
    flush: bool,
}

/// An atlas page as an image.
struct Image {
    image: vk::Image,
    memory: vk::DeviceMemory,
    view: vk::ImageView,
    size: u32,
    /// Never written: its first frame clears it (its layout is undefined).
    fresh: bool,
}

/// The device, its queue, and everything that draws a prepared frame.
pub struct Gpu {
    pub physical: vk::PhysicalDevice,
    pub family: u32,
    pub device: ash::Device,
    pub queue: vk::Queue,
    pub swapchain: ash::khr::swapchain::Device,
    pub describe: String,
    pub max_extent: u32,
    memory: vk::PhysicalDeviceMemoryProperties,
    /// Render passes ending in `PRESENT_SRC_KHR` (swapchains) and `TRANSFER_SRC_OPTIMAL`
    /// (offscreen, for tests); the pipeline is compatible with both.
    pub present_pass: vk::RenderPass,
    pub offscreen_pass: vk::RenderPass,
    set_layout: vk::DescriptorSetLayout,
    pipeline_layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    pool: vk::DescriptorPool,
    set: vk::DescriptorSet,
    commands: vk::CommandPool,
    pub command: vk::CommandBuffer,
    pub fence: vk::Fence,
    pub acquired: vk::Semaphore,
    uniform: Buffer,
    vertices: Option<Buffer>,
    indices: Option<Buffer>,
    staging: Option<Buffer>,
    mask: Option<Image>,
    color: Option<Image>,
    /// Bytes of device memory allocated, swapchains not included.
    pub bytes: u64,
}

impl Gpu {
    /// The device on `physical` with a queue of `family` and `VK_KHR_swapchain`, and the
    /// pipeline and frame slot.
    ///
    /// # Safety
    ///
    /// `instance` is alive, and outlives the returned `Gpu` (destroyed with [`Gpu::destroy`]).
    pub unsafe fn new(
        instance: &ash::Instance,
        physical: vk::PhysicalDevice,
        family: u32,
        describe: String,
    ) -> Result<Self, RenderError> {
        let priorities = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(family)
            .queue_priorities(&priorities)];
        let extensions = [ash::khr::swapchain::NAME.as_ptr()];
        let info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&extensions);
        let device = unsafe { instance.create_device(physical, &info, None) }
            .map_err(|e| device_error("vkCreateDevice", e))?;
        let queue = unsafe { device.get_device_queue(family, 0) };
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let limits = unsafe { instance.get_physical_device_properties(physical) }.limits;
        let swapchain = ash::khr::swapchain::Device::new(instance, &device);
        let mut gpu = Self {
            physical,
            family,
            device,
            queue,
            swapchain,
            describe,
            max_extent: limits.max_image_dimension2_d,
            memory,
            present_pass: vk::RenderPass::null(),
            offscreen_pass: vk::RenderPass::null(),
            set_layout: vk::DescriptorSetLayout::null(),
            pipeline_layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            pool: vk::DescriptorPool::null(),
            set: vk::DescriptorSet::null(),
            commands: vk::CommandPool::null(),
            command: vk::CommandBuffer::null(),
            fence: vk::Fence::null(),
            acquired: vk::Semaphore::null(),
            uniform: Buffer {
                buffer: vk::Buffer::null(),
                memory: vk::DeviceMemory::null(),
                size: 0,
                mapped: std::ptr::null_mut(),
                flush: false,
            },
            vertices: None,
            indices: None,
            staging: None,
            mask: None,
            color: None,
            bytes: 0,
        };
        // On failure, what was made so far is destroyed (null handles are skipped).
        if let Err(error) = unsafe { gpu.build() } {
            unsafe { gpu.destroy() };
            return Err(error);
        }
        Ok(gpu)
    }

    /// Makes the render passes, the pipeline, the descriptor set, the frame slot and the
    /// uniform buffer.
    unsafe fn build(&mut self) -> Result<(), RenderError> {
        let d = &self.device;
        let err = |what| move |e| device_error(what, e);
        self.present_pass = unsafe { render_pass(d, vk::ImageLayout::PRESENT_SRC_KHR) }?;
        self.offscreen_pass = unsafe { render_pass(d, vk::ImageLayout::TRANSFER_SRC_OPTIMAL) }?;
        let bindings = [
            vk::DescriptorSetLayoutBinding::default()
                .binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::VERTEX),
            vk::DescriptorSetLayoutBinding::default()
                .binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
            vk::DescriptorSetLayoutBinding::default()
                .binding(2)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::FRAGMENT),
        ];
        let info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
        self.set_layout = unsafe { d.create_descriptor_set_layout(&info, None) }
            .map_err(err("vkCreateDescriptorSetLayout"))?;
        let layouts = [self.set_layout];
        let info = vk::PipelineLayoutCreateInfo::default().set_layouts(&layouts);
        self.pipeline_layout = unsafe { d.create_pipeline_layout(&info, None) }
            .map_err(err("vkCreatePipelineLayout"))?;
        self.pipeline = unsafe { pipeline(d, self.pipeline_layout, self.present_pass) }?;
        let sizes = [
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::UNIFORM_BUFFER,
                descriptor_count: 1,
            },
            vk::DescriptorPoolSize {
                ty: vk::DescriptorType::SAMPLED_IMAGE,
                descriptor_count: 2,
            },
        ];
        let info = vk::DescriptorPoolCreateInfo::default()
            .max_sets(1)
            .pool_sizes(&sizes);
        self.pool = unsafe { d.create_descriptor_pool(&info, None) }
            .map_err(err("vkCreateDescriptorPool"))?;
        let info = vk::DescriptorSetAllocateInfo::default()
            .descriptor_pool(self.pool)
            .set_layouts(&layouts);
        self.set = unsafe { d.allocate_descriptor_sets(&info) }
            .map_err(err("vkAllocateDescriptorSets"))?[0];
        let info = vk::CommandPoolCreateInfo::default()
            .queue_family_index(self.family)
            .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
        self.commands =
            unsafe { d.create_command_pool(&info, None) }.map_err(err("vkCreateCommandPool"))?;
        let info = vk::CommandBufferAllocateInfo::default()
            .command_pool(self.commands)
            .level(vk::CommandBufferLevel::PRIMARY)
            .command_buffer_count(1);
        self.command = unsafe { d.allocate_command_buffers(&info) }
            .map_err(err("vkAllocateCommandBuffers"))?[0];
        let info = vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED);
        self.fence = unsafe { d.create_fence(&info, None) }.map_err(err("vkCreateFence"))?;
        self.acquired = unsafe { d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }
            .map_err(err("vkCreateSemaphore"))?;
        self.uniform = unsafe { self.buffer(UNIFORM_SIZE, vk::BufferUsageFlags::UNIFORM_BUFFER) }?;
        let buffers = [vk::DescriptorBufferInfo {
            buffer: self.uniform.buffer,
            offset: 0,
            range: UNIFORM_SIZE,
        }];
        let write = vk::WriteDescriptorSet::default()
            .dst_set(self.set)
            .dst_binding(0)
            .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
            .buffer_info(&buffers);
        unsafe { self.device.update_descriptor_sets(&[write], &[]) };
        Ok(())
    }

    /// Waits for the frame in flight, if any (bounded: a frame that takes longer is the
    /// device's failure).
    pub fn wait(&self) -> Result<(), RenderError> {
        let timeout = FRAME_TIMEOUT.as_nanos() as u64;
        match unsafe { self.device.wait_for_fences(&[self.fence], true, timeout) } {
            Ok(()) => Ok(()),
            Err(vk::Result::TIMEOUT) => Err(RenderError::Device(
                "the last chrome frame did not finish in 2 s".into(),
            )),
            Err(e) => Err(device_error("vkWaitForFences", e)),
        }
    }

    /// Waits for the device to be idle. False if it failed for another reason than device
    /// loss (after which nothing runs any more): then work may still be in flight, and what
    /// it uses must not be destroyed.
    ///
    /// The presentation engine's own use of swapchain images and semaphores is not covered by
    /// this (it is by `VK_EXT_swapchain_maintenance1`'s present fences, not used yet): like
    /// wgpu-hal, the backend destroys a swapchain after the device is idle.
    pub fn idle(&self) -> bool {
        match unsafe { self.device.device_wait_idle() } {
            Ok(()) | Err(vk::Result::ERROR_DEVICE_LOST) => true,
            Err(_) => false,
        }
    }

    /// After a failure between an acquire and its submission: an empty submission that waits
    /// on the acquire semaphore, so its signal is consumed. Best effort; the device is being
    /// given up on anyway.
    pub fn drain(&self) {
        let d = &self.device;
        unsafe {
            let _ = d.reset_command_buffer(self.command, vk::CommandBufferResetFlags::empty());
            let begin = vk::CommandBufferBeginInfo::default();
            if d.begin_command_buffer(self.command, &begin).is_ok() {
                let _ = self.submit(Some(self.acquired), None);
                let _ = self.wait();
            }
        }
    }

    /// Records `prepared` drawn into `framebuffer` (`prepared.size`, [`FORMAT`], begun with
    /// `pass`) in the slot's command buffer: the atlas uploads first, then one pass with a
    /// scissor per batch. Call after [`Gpu::wait`].
    pub fn record(
        &mut self,
        pass: vk::RenderPass,
        framebuffer: vk::Framebuffer,
        prepared: &Prepared,
    ) -> Result<(), RenderError> {
        unsafe { self.ensure_pages(prepared.mask_size, prepared.color_size) }?;
        let (width, height) = prepared.size;
        unsafe {
            write(
                &self.device,
                &self.uniform,
                &sighurt_render::gpu::uniform(width, height),
            )
        }?;
        let drawing = !prepared.indices.is_empty();
        if drawing {
            let usage = vk::BufferUsageFlags::VERTEX_BUFFER;
            unsafe { self.write_growing(Slot::Vertices, usage, prepared.vertex_bytes()) }?;
            let usage = vk::BufferUsageFlags::INDEX_BUFFER;
            unsafe { self.write_growing(Slot::Indices, usage, prepared.index_bytes()) }?;
        }
        // The uploads, each at a 4-byte aligned offset in the staging buffer.
        let mut staged = Vec::new();
        let mut bytes = Vec::new();
        for upload in &prepared.uploads {
            let offset = bytes.len().next_multiple_of(4);
            bytes.resize(offset, 0);
            bytes.extend_from_slice(&upload.data);
            staged.push((upload, offset as u64));
        }
        if !bytes.is_empty() {
            let usage = vk::BufferUsageFlags::TRANSFER_SRC;
            unsafe { self.write_growing(Slot::Staging, usage, &bytes) }?;
        }
        let d = &self.device;
        let cmd = self.command;
        let err = |what| move |e| device_error(what, e);
        unsafe {
            d.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())
                .map_err(err("vkResetCommandBuffer"))?;
            let begin = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            d.begin_command_buffer(cmd, &begin)
                .map_err(err("vkBeginCommandBuffer"))?;
        }
        for (kind, image) in [
            (PageKind::Mask, &mut self.mask),
            (PageKind::Color, &mut self.color),
        ] {
            let image = image.as_mut().expect("ensured");
            let regions: Vec<vk::BufferImageCopy> = staged
                .iter()
                .filter(|(upload, _)| upload.page == kind)
                .map(|(upload, offset)| vk::BufferImageCopy {
                    buffer_offset: *offset,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: COLOR_LAYER,
                    image_offset: vk::Offset3D {
                        x: upload.x as i32,
                        y: upload.y as i32,
                        z: 0,
                    },
                    image_extent: vk::Extent3D {
                        width: upload.width,
                        height: upload.height,
                        depth: 1,
                    },
                })
                .collect();
            if regions.is_empty() && !image.fresh {
                continue;
            }
            let from = if image.fresh {
                vk::ImageLayout::UNDEFINED
            } else {
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
            };
            unsafe {
                barrier(
                    d,
                    cmd,
                    image.image,
                    (from, vk::ImageLayout::TRANSFER_DST_OPTIMAL),
                    (
                        vk::AccessFlags::SHADER_READ,
                        vk::AccessFlags::TRANSFER_WRITE,
                    ),
                    (
                        vk::PipelineStageFlags::FRAGMENT_SHADER,
                        vk::PipelineStageFlags::TRANSFER,
                    ),
                );
                if image.fresh {
                    let zero = vk::ClearColorValue::default();
                    let range = [COLOR_RANGE];
                    let layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
                    d.cmd_clear_color_image(cmd, image.image, layout, &zero, &range);
                    if !regions.is_empty() {
                        // The clear before the copies.
                        barrier(
                            d,
                            cmd,
                            image.image,
                            (layout, layout),
                            (
                                vk::AccessFlags::TRANSFER_WRITE,
                                vk::AccessFlags::TRANSFER_WRITE,
                            ),
                            (
                                vk::PipelineStageFlags::TRANSFER,
                                vk::PipelineStageFlags::TRANSFER,
                            ),
                        );
                    }
                }
                if !regions.is_empty() {
                    let staging = self.staging.as_ref().expect("written").buffer;
                    let layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
                    d.cmd_copy_buffer_to_image(cmd, staging, image.image, layout, &regions);
                }
                barrier(
                    d,
                    cmd,
                    image.image,
                    (
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    ),
                    (
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::SHADER_READ,
                    ),
                    (
                        vk::PipelineStageFlags::TRANSFER,
                        vk::PipelineStageFlags::FRAGMENT_SHADER,
                    ),
                );
            }
            image.fresh = false;
        }
        let [r, g, b, a] = prepared.clear;
        let clear = [vk::ClearValue {
            color: vk::ClearColorValue {
                float32: [r, g, b, a],
            },
        }];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D { width, height },
        };
        let begin = vk::RenderPassBeginInfo::default()
            .render_pass(pass)
            .framebuffer(framebuffer)
            .render_area(area)
            .clear_values(&clear);
        unsafe {
            d.cmd_begin_render_pass(cmd, &begin, vk::SubpassContents::INLINE);
            if drawing {
                d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.pipeline);
                let viewport = vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: width as f32,
                    height: height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                };
                d.cmd_set_viewport(cmd, 0, &[viewport]);
                d.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipeline_layout,
                    0,
                    &[self.set],
                    &[],
                );
                let vertices = self.vertices.as_ref().expect("written").buffer;
                let indices = self.indices.as_ref().expect("written").buffer;
                d.cmd_bind_vertex_buffers(cmd, 0, &[vertices], &[0]);
                d.cmd_bind_index_buffer(cmd, indices, 0, vk::IndexType::UINT32);
                for batch in &prepared.batches {
                    let [x, y, w, h] = batch.scissor();
                    let scissor = vk::Rect2D {
                        offset: vk::Offset2D {
                            x: x as i32,
                            y: y as i32,
                        },
                        extent: vk::Extent2D {
                            width: w,
                            height: h,
                        },
                    };
                    d.cmd_set_scissor(cmd, 0, &[scissor]);
                    let count = batch.indices.end - batch.indices.start;
                    d.cmd_draw_indexed(cmd, count, 1, batch.indices.start, 0, 0);
                }
            }
            d.cmd_end_render_pass(cmd);
        }
        Ok(())
    }

    /// Ends the command buffer, resets the fence (a submission follows) and submits, waiting
    /// on `wait` (at color output) and signalling `signal`, if given.
    pub fn submit(
        &self,
        wait: Option<vk::Semaphore>,
        signal: Option<vk::Semaphore>,
    ) -> Result<(), RenderError> {
        let d = &self.device;
        unsafe {
            d.end_command_buffer(self.command)
                .map_err(|e| device_error("vkEndCommandBuffer", e))?;
            d.reset_fences(&[self.fence])
                .map_err(|e| device_error("vkResetFences", e))?;
        }
        let waits: Vec<_> = wait.into_iter().collect();
        let stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
        let signals: Vec<_> = signal.into_iter().collect();
        let commands = [self.command];
        let info = vk::SubmitInfo::default()
            .wait_semaphores(&waits)
            .wait_dst_stage_mask(&stages[..waits.len()])
            .command_buffers(&commands)
            .signal_semaphores(&signals);
        unsafe { d.queue_submit(self.queue, &[info], self.fence) }
            .map_err(|e| device_error("vkQueueSubmit", e))
    }

    /// Atlas images at the plan's page sizes; a page that changed size is a new, cleared
    /// image, and the descriptor set names the current ones. Call after [`Gpu::wait`].
    unsafe fn ensure_pages(&mut self, mask: u32, color: u32) -> Result<(), RenderError> {
        let mut changed = false;
        for (kind, size) in [(PageKind::Mask, mask), (PageKind::Color, color)] {
            let current = match kind {
                PageKind::Mask => self.mask.as_ref(),
                PageKind::Color => self.color.as_ref(),
            };
            if current.is_some_and(|image| image.size == size) {
                continue;
            }
            let format = match kind {
                PageKind::Mask => vk::Format::R8_UNORM,
                PageKind::Color => vk::Format::R8G8B8A8_UNORM,
            };
            let image = unsafe { self.image(size, format) }?;
            let old = match kind {
                PageKind::Mask => self.mask.replace(image),
                PageKind::Color => self.color.replace(image),
            };
            if let Some(old) = old {
                unsafe { self.destroy_image(old) };
            }
            changed = true;
        }
        if changed {
            let mask = [vk::DescriptorImageInfo {
                sampler: vk::Sampler::null(),
                image_view: self.mask.as_ref().expect("made").view,
                image_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            }];
            let color = [vk::DescriptorImageInfo {
                image_view: self.color.as_ref().expect("made").view,
                ..mask[0]
            }];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(self.set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&mask),
                vk::WriteDescriptorSet::default()
                    .dst_set(self.set)
                    .dst_binding(2)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&color),
            ];
            unsafe { self.device.update_descriptor_sets(&writes, &[]) };
        }
        Ok(())
    }

    /// Writes `data` into the buffer of `slot`, a new one twice the size when it doesn't fit.
    unsafe fn write_growing(
        &mut self,
        slot: Slot,
        usage: vk::BufferUsageFlags,
        data: &[u8],
    ) -> Result<(), RenderError> {
        let needed = data.len() as u64;
        let current = match slot {
            Slot::Vertices => &self.vertices,
            Slot::Indices => &self.indices,
            Slot::Staging => &self.staging,
        };
        if current.as_ref().is_none_or(|b| b.size < needed) {
            let size = needed.next_power_of_two().max(4096);
            let buffer = unsafe { self.buffer(size, usage) }?;
            let old = match slot {
                Slot::Vertices => self.vertices.replace(buffer),
                Slot::Indices => self.indices.replace(buffer),
                Slot::Staging => self.staging.replace(buffer),
            };
            if let Some(old) = old {
                unsafe { self.destroy_buffer(old) };
            }
        }
        let buffer = match slot {
            Slot::Vertices => &self.vertices,
            Slot::Indices => &self.indices,
            Slot::Staging => &self.staging,
        };
        unsafe { write(&self.device, buffer.as_ref().expect("made"), data) }
    }

    /// A host-visible buffer of `size` bytes, mapped; coherent memory if there is any.
    unsafe fn buffer(
        &mut self,
        size: u64,
        usage: vk::BufferUsageFlags,
    ) -> Result<Buffer, RenderError> {
        let d = &self.device.clone();
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE);
        let buffer = unsafe { d.create_buffer(&info, None) }
            .map_err(|e| device_error("vkCreateBuffer", e))?;
        let needs = unsafe { d.get_buffer_memory_requirements(buffer) };
        let visible = vk::MemoryPropertyFlags::HOST_VISIBLE;
        let coherent = visible | vk::MemoryPropertyFlags::HOST_COHERENT;
        let (memory, flush) = match self.allocate(needs, coherent) {
            Ok(memory) => (memory, false),
            Err(_) => match self.allocate(needs, visible) {
                Ok(memory) => (memory, true),
                Err(error) => {
                    unsafe { d.destroy_buffer(buffer, None) };
                    return Err(error);
                }
            },
        };
        let mapped = unsafe {
            d.bind_buffer_memory(buffer, memory, 0)
                .and_then(|()| d.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty()))
        };
        match mapped {
            Ok(mapped) => Ok(Buffer {
                buffer,
                memory,
                size,
                mapped: mapped.cast(),
                flush,
            }),
            Err(e) => {
                unsafe {
                    d.destroy_buffer(buffer, None);
                    d.free_memory(memory, None);
                }
                self.bytes -= needs.size;
                Err(device_error("vkMapMemory", e))
            }
        }
    }

    /// A `size`² image of `format` in device-local memory, to sample and copy into.
    unsafe fn image(&mut self, size: u32, format: vk::Format) -> Result<Image, RenderError> {
        let d = &self.device.clone();
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: size,
                height: size,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image =
            unsafe { d.create_image(&info, None) }.map_err(|e| device_error("vkCreateImage", e))?;
        let needs = unsafe { d.get_image_memory_requirements(image) };
        let memory = match self.allocate(needs, vk::MemoryPropertyFlags::DEVICE_LOCAL) {
            Ok(memory) => memory,
            Err(error) => {
                unsafe { d.destroy_image(image, None) };
                return Err(error);
            }
        };
        let view = unsafe {
            d.bind_image_memory(image, memory, 0).and_then(|()| {
                let info = vk::ImageViewCreateInfo::default()
                    .image(image)
                    .view_type(vk::ImageViewType::TYPE_2D)
                    .format(format)
                    .subresource_range(COLOR_RANGE);
                d.create_image_view(&info, None)
            })
        };
        match view {
            Ok(view) => Ok(Image {
                image,
                memory,
                view,
                size,
                fresh: true,
            }),
            Err(e) => {
                unsafe {
                    d.destroy_image(image, None);
                    d.free_memory(memory, None);
                }
                self.bytes -= needs.size;
                Err(device_error("vkCreateImageView", e))
            }
        }
    }

    /// Memory for `needs` of a type with `flags`.
    fn allocate(
        &mut self,
        needs: vk::MemoryRequirements,
        flags: vk::MemoryPropertyFlags,
    ) -> Result<vk::DeviceMemory, RenderError> {
        let types = &self.memory.memory_types[..self.memory.memory_type_count as usize];
        let index = types
            .iter()
            .enumerate()
            .position(|(i, t)| {
                needs.memory_type_bits & (1 << i) != 0 && t.property_flags.contains(flags)
            })
            .ok_or_else(|| RenderError::Device(format!("no memory type with {flags:?}")))?;
        let info = vk::MemoryAllocateInfo::default()
            .allocation_size(needs.size)
            .memory_type_index(index as u32);
        let memory = unsafe { self.device.allocate_memory(&info, None) }
            .map_err(|e| device_error("vkAllocateMemory", e))?;
        self.bytes += needs.size;
        Ok(memory)
    }

    unsafe fn destroy_buffer(&mut self, buffer: Buffer) {
        let needs = unsafe { self.device.get_buffer_memory_requirements(buffer.buffer) };
        unsafe {
            self.device.destroy_buffer(buffer.buffer, None);
            self.device.free_memory(buffer.memory, None);
        }
        self.bytes = self.bytes.saturating_sub(needs.size);
    }

    unsafe fn destroy_image(&mut self, image: Image) {
        let needs = unsafe { self.device.get_image_memory_requirements(image.image) };
        unsafe {
            self.device.destroy_image_view(image.view, None);
            self.device.destroy_image(image.image, None);
            self.device.free_memory(image.memory, None);
        }
        self.bytes = self.bytes.saturating_sub(needs.size);
    }

    /// Destroys everything, the device last. Null handles (a `Gpu` that failed to build) are
    /// skipped.
    ///
    /// # Safety
    ///
    /// The device is idle ([`Gpu::idle`]) and no swapchain made on it is left.
    pub unsafe fn destroy(&mut self) {
        let d = self.device.clone();
        unsafe {
            for slot in [
                self.vertices.take(),
                self.indices.take(),
                self.staging.take(),
            ]
            .into_iter()
            .flatten()
            {
                self.destroy_buffer(slot);
            }
            for image in [self.mask.take(), self.color.take()].into_iter().flatten() {
                self.destroy_image(image);
            }
            if self.uniform.buffer != vk::Buffer::null() {
                d.destroy_buffer(self.uniform.buffer, None);
                d.free_memory(self.uniform.memory, None);
            }
            d.destroy_semaphore(self.acquired, None);
            d.destroy_fence(self.fence, None);
            d.destroy_command_pool(self.commands, None);
            d.destroy_descriptor_pool(self.pool, None);
            d.destroy_pipeline(self.pipeline, None);
            d.destroy_pipeline_layout(self.pipeline_layout, None);
            d.destroy_descriptor_set_layout(self.set_layout, None);
            d.destroy_render_pass(self.present_pass, None);
            d.destroy_render_pass(self.offscreen_pass, None);
            d.destroy_device(None);
        }
    }
}

/// Which growing buffer.
#[derive(Clone, Copy)]
enum Slot {
    Vertices,
    Indices,
    Staging,
}

const COLOR_RANGE: vk::ImageSubresourceRange = vk::ImageSubresourceRange {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    base_mip_level: 0,
    level_count: 1,
    base_array_layer: 0,
    layer_count: 1,
};

const COLOR_LAYER: vk::ImageSubresourceLayers = vk::ImageSubresourceLayers {
    aspect_mask: vk::ImageAspectFlags::COLOR,
    mip_level: 0,
    base_array_layer: 0,
    layer_count: 1,
};

/// Copies `data` to the start of `buffer`'s mapping, flushed when its memory isn't coherent.
unsafe fn write(device: &ash::Device, buffer: &Buffer, data: &[u8]) -> Result<(), RenderError> {
    assert!(data.len() as u64 <= buffer.size);
    // SAFETY: the mapping is `buffer.size` bytes and no frame in flight reads it (the caller
    // waited for the last one).
    unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), buffer.mapped, data.len()) };
    if buffer.flush {
        let range = vk::MappedMemoryRange::default()
            .memory(buffer.memory)
            .offset(0)
            .size(vk::WHOLE_SIZE);
        unsafe { device.flush_mapped_memory_ranges(&[range]) }
            .map_err(|e| device_error("vkFlushMappedMemoryRanges", e))?;
    }
    Ok(())
}

/// An image layout transition with its access and stage masks.
unsafe fn barrier(
    device: &ash::Device,
    cmd: vk::CommandBuffer,
    image: vk::Image,
    (old, new): (vk::ImageLayout, vk::ImageLayout),
    (src_access, dst_access): (vk::AccessFlags, vk::AccessFlags),
    (src_stage, dst_stage): (vk::PipelineStageFlags, vk::PipelineStageFlags),
) {
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(old)
        .new_layout(new)
        .src_access_mask(src_access)
        .dst_access_mask(dst_access)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(COLOR_RANGE);
    unsafe {
        device.cmd_pipeline_barrier(
            cmd,
            src_stage,
            dst_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        )
    };
}

/// A render pass of one [`FORMAT`] attachment, cleared, stored, ending in `final_layout`.
unsafe fn render_pass(
    device: &ash::Device,
    final_layout: vk::ImageLayout,
) -> Result<vk::RenderPass, RenderError> {
    let attachments = [vk::AttachmentDescription::default()
        .format(FORMAT)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(final_layout)];
    let color = [vk::AttachmentReference {
        attachment: 0,
        layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
    }];
    let subpasses = [vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color)];
    // The acquire semaphore is waited at color output; the layout transition follows it.
    let incoming = vk::SubpassDependency {
        src_subpass: vk::SUBPASS_EXTERNAL,
        dst_subpass: 0,
        src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        dst_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        src_access_mask: vk::AccessFlags::empty(),
        dst_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        dependency_flags: vk::DependencyFlags::empty(),
    };
    // Offscreen, a copy reads the image next: the writes before it.
    let outgoing = vk::SubpassDependency {
        src_subpass: 0,
        dst_subpass: vk::SUBPASS_EXTERNAL,
        src_stage_mask: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
        dst_stage_mask: vk::PipelineStageFlags::TRANSFER,
        src_access_mask: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
        dst_access_mask: vk::AccessFlags::TRANSFER_READ,
        dependency_flags: vk::DependencyFlags::empty(),
    };
    let both = [incoming, outgoing];
    let dependencies = match final_layout {
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL => &both[..],
        _ => &both[..1],
    };
    let info = vk::RenderPassCreateInfo::default()
        .attachments(&attachments)
        .subpasses(&subpasses)
        .dependencies(dependencies);
    unsafe { device.create_render_pass(&info, None) }
        .map_err(|e| device_error("vkCreateRenderPass", e))
}

/// The one pipeline: the shared program, the vertex layout, premultiplied source-over,
/// dynamic viewport and scissor.
unsafe fn pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    pass: vk::RenderPass,
) -> Result<vk::Pipeline, RenderError> {
    let code = crate::spirv::words();
    let info = vk::ShaderModuleCreateInfo::default().code(&code);
    let module = unsafe { device.create_shader_module(&info, None) }
        .map_err(|e| device_error("vkCreateShaderModule", e))?;
    let entry = |name: &str| -> std::ffi::CString { std::ffi::CString::new(name).expect("no NUL") };
    let (vs, fs) = (entry(VERTEX_ENTRY), entry(FRAGMENT_ENTRY));
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(module)
            .name(&vs),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(module)
            .name(&fs),
    ];
    let bindings = [vk::VertexInputBindingDescription {
        binding: 0,
        stride: VERTEX_STRIDE as u32,
        input_rate: vk::VertexInputRate::VERTEX,
    }];
    let attributes = VERTEX_ATTRIBUTES.map(|a| vk::VertexInputAttributeDescription {
        location: a.location,
        binding: 0,
        format: match a.format {
            AttributeFormat::Float32x2 => vk::Format::R32G32_SFLOAT,
            AttributeFormat::Float32x4 => vk::Format::R32G32B32A32_SFLOAT,
            AttributeFormat::Uint32 => vk::Format::R32_UINT,
        },
        offset: a.offset as u32,
    });
    let vertex = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&bindings)
        .vertex_attribute_descriptions(&attributes);
    let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
    let viewport = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);
    let raster = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);
    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);
    let blend = [vk::PipelineColorBlendAttachmentState {
        blend_enable: vk::TRUE,
        src_color_blend_factor: vk::BlendFactor::ONE,
        dst_color_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        color_blend_op: vk::BlendOp::ADD,
        src_alpha_blend_factor: vk::BlendFactor::ONE,
        dst_alpha_blend_factor: vk::BlendFactor::ONE_MINUS_SRC_ALPHA,
        alpha_blend_op: vk::BlendOp::ADD,
        color_write_mask: vk::ColorComponentFlags::RGBA,
    }];
    let blending = vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend);
    let dynamic = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic);
    let info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex)
        .input_assembly_state(&assembly)
        .viewport_state(&viewport)
        .rasterization_state(&raster)
        .multisample_state(&multisample)
        .color_blend_state(&blending)
        .dynamic_state(&dynamic)
        .layout(layout)
        .render_pass(pass)
        .subpass(0);
    let made =
        unsafe { device.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) };
    unsafe { device.destroy_shader_module(module, None) };
    made.map(|pipelines| pipelines[0])
        .map_err(|(_, e)| device_error("vkCreateGraphicsPipelines", e))
}

/// A device's name as Vulkan reports it.
pub fn name(properties: &vk::PhysicalDeviceProperties) -> String {
    properties
        .device_name_as_c_str()
        .map(CStr::to_string_lossy)
        .unwrap_or_default()
        .into_owned()
}
