//! Rendering offscreen and reading the pixels back, for tests only (feature `testing`): the
//! comparison of this backend's drawing with the CPU backend's. Never on the display path.

use sighurt_api::geometry::Metrics;
use sighurt_render::gpu;
use sighurt_render::{FontSystemHandle, RenderError, Scene};

use crate::device::{Device, FORMAT};
use crate::{block_on, check, instance, Gpu, WgpuBackend};

impl WgpuBackend {
    /// A backend with a device and no surface, on the first adapter (a software one too):
    /// for [`render_to_image`](Self::render_to_image). `Err` when there is no Vulkan adapter.
    pub fn headless(fonts: FontSystemHandle) -> Result<Self, String> {
        let mut backend = Self::new(fonts, true)?;
        let instance = backend.instance.get_or_insert_with(instance);
        let adapters = block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN));
        let adapter = adapters
            .into_iter()
            .min_by_key(|a| a.get_info().device_type == wgpu::DeviceType::Cpu)
            .ok_or("no Vulkan adapter")?;
        let info = adapter.get_info();
        let (device, queue) = block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .map_err(|e| e.to_string())?;
        let fault = std::sync::Arc::clone(&backend.fault);
        device.on_uncaptured_error(std::sync::Arc::new(move |error: wgpu::Error| {
            let mut fault = fault.lock().unwrap_or_else(|e| e.into_inner());
            fault.get_or_insert_with(|| format!("wgpu: {error}"));
        }));
        backend.gpu = Some(Gpu {
            adapter,
            device: Device::new(device, queue),
            describe: format!("wgpu 30 on Vulkan: {} ({:?})", info.name, info.device_type),
        });
        Ok(backend)
    }

    /// `scene` rendered for `metrics` into a texture of their buffer's size, read back as
    /// rows of RGBA bytes. Shares the atlas with the surfaces' renders.
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
        let device = gpu.device.device.clone();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("chrome readback"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chrome readback"),
            size: u64::from(row) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let draw = gpu.device.encode(&view, &prepared);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        gpu.device.queue.submit([draw, encoder.finish()]);
        buffer.map_async(wgpu::MapMode::Read, .., |_| {});
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| RenderError::Device(e.to_string()))?;
        check(&self.fault)?;
        let mapped = buffer
            .get_mapped_range(..)
            .map_err(|e| RenderError::Device(e.to_string()))?;
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for line in mapped.chunks_exact(row as usize) {
            for px in line[..(width * 4) as usize].as_chunks::<4>().0 {
                // Bgra8Unorm.
                rgba.extend([px[2], px[1], px[0], px[3]]);
            }
        }
        Ok(rgba)
    }
}
