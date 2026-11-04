//! The backend's device objects: the pipeline that draws a prepared frame, the atlas
//! textures, the vertex, index and uniform buffers, and encoding one frame's pass.
//!
//! Must not own: surfaces or presenting (`crate`'s), or what a frame holds
//! (`sighurt_render::gpu`).

use sighurt_render::gpu::{
    self, AttributeFormat, PageKind, Prepared, FRAGMENT_ENTRY, VERTEX_ATTRIBUTES, VERTEX_ENTRY,
    VERTEX_STRIDE,
};

/// The format of every chrome target: opaque, not sRGB (RENDER.md §14.12).
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Bgra8Unorm;

/// A buffer that grows by doubling.
struct Growing {
    buffer: wgpu::Buffer,
    capacity: u64,
}

/// The atlas textures, at the plan's sizes, and the bind group that names them.
struct Pages {
    mask_size: u32,
    color_size: u32,
    bind_group: wgpu::BindGroup,
}

/// What draws prepared frames on one device.
pub struct Device {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    pages: Option<Pages>,
    mask: Option<wgpu::Texture>,
    color: Option<wgpu::Texture>,
    vertices: Option<Growing>,
    indices: Option<Growing>,
}

impl Device {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("chrome"),
            source: wgpu::ShaderSource::Wgsl(gpu::SHADER.into()),
        });
        let texture = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("chrome"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                texture(1),
                texture(2),
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("chrome"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let attributes = VERTEX_ATTRIBUTES.map(|a| wgpu::VertexAttribute {
            format: match a.format {
                AttributeFormat::Float32x2 => wgpu::VertexFormat::Float32x2,
                AttributeFormat::Float32x4 => wgpu::VertexFormat::Float32x4,
                AttributeFormat::Uint32 => wgpu::VertexFormat::Uint32,
            },
            offset: a.offset,
            shader_location: a.location,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("chrome"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some(VERTEX_ENTRY),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: VERTEX_STRIDE,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &attributes,
                })],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some(FRAGMENT_ENTRY),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: FORMAT,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chrome viewport"),
            size: gpu::UNIFORM_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            device,
            queue,
            layout,
            pipeline,
            uniform,
            pages: None,
            mask: None,
            color: None,
            vertices: None,
            indices: None,
        }
    }

    /// Bytes of the resources it made: the atlas textures and the buffers.
    pub fn bytes(&self) -> u64 {
        let pages = self.pages.as_ref().map_or(0, |p| {
            u64::from(p.mask_size).pow(2) + u64::from(p.color_size).pow(2) * 4
        });
        let buffers = [&self.vertices, &self.indices]
            .into_iter()
            .flatten()
            .map(|b| b.capacity)
            .sum::<u64>();
        pages + buffers + gpu::UNIFORM_SIZE
    }

    /// Records `prepared` drawn on `view` (`prepared.size`, [`FORMAT`]): the atlas uploads and
    /// the buffers are written to the queue, so they precede this command buffer's work.
    pub fn encode(&mut self, view: &wgpu::TextureView, prepared: &Prepared) -> wgpu::CommandBuffer {
        self.ensure_pages(prepared.mask_size, prepared.color_size);
        for upload in &prepared.uploads {
            let texture = match upload.page {
                PageKind::Mask => self.mask.as_ref(),
                PageKind::Color => self.color.as_ref(),
            }
            .expect("the pages exist");
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d {
                        x: upload.x,
                        y: upload.y,
                        z: 0,
                    },
                    aspect: wgpu::TextureAspect::All,
                },
                &upload.data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(upload.width * upload.page.bytes_per_texel()),
                    rows_per_image: Some(upload.height),
                },
                wgpu::Extent3d {
                    width: upload.width,
                    height: upload.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        let (width, height) = prepared.size;
        self.queue
            .write_buffer(&self.uniform, 0, &gpu::uniform(width, height));
        let drawing = !prepared.indices.is_empty();
        if drawing {
            let usage = wgpu::BufferUsages::VERTEX;
            write(
                &self.device,
                &self.queue,
                &mut self.vertices,
                usage,
                prepared.vertex_bytes(),
            );
            let usage = wgpu::BufferUsages::INDEX;
            write(
                &self.device,
                &self.queue,
                &mut self.indices,
                usage,
                prepared.index_bytes(),
            );
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("chrome"),
            });
        {
            let [r, g, b, a] = prepared.clear.map(f64::from);
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("chrome"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            if drawing {
                let pages = self.pages.as_ref().expect("the pages exist");
                let vertices = &self.vertices.as_ref().expect("written").buffer;
                let indices = &self.indices.as_ref().expect("written").buffer;
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &pages.bind_group, &[]);
                pass.set_vertex_buffer(0, vertices.slice(..));
                pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
                for batch in &prepared.batches {
                    let [x, y, w, h] = batch.scissor();
                    pass.set_scissor_rect(x, y, w, h);
                    pass.draw_indexed(batch.indices.clone(), 0, 0..1);
                }
            }
        }
        encoder.finish()
    }

    /// Textures at the plan's page sizes: a page that changed size is a new texture.
    fn ensure_pages(&mut self, mask_size: u32, color_size: u32) {
        if self
            .pages
            .as_ref()
            .is_some_and(|p| (p.mask_size, p.color_size) == (mask_size, color_size))
        {
            return;
        }
        let texture = |size, format, label| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d {
                    width: size,
                    height: size,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let keep = |old: Option<wgpu::Texture>, size| old.filter(|t| t.width() == size);
        let mask = keep(self.mask.take(), mask_size)
            .unwrap_or_else(|| texture(mask_size, wgpu::TextureFormat::R8Unorm, "chrome masks"));
        let color = keep(self.color.take(), color_size).unwrap_or_else(|| {
            texture(color_size, wgpu::TextureFormat::Rgba8Unorm, "chrome colors")
        });
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        let (mask_view, color_view) = (view(&mask), view(&color));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("chrome"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&mask_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&color_view),
                },
            ],
        });
        self.mask = Some(mask);
        self.color = Some(color);
        self.pages = Some(Pages {
            mask_size,
            color_size,
            bind_group,
        });
    }
}

/// Writes `data` to `slot`'s buffer, a new one twice the size when it doesn't fit.
fn write(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    slot: &mut Option<Growing>,
    usage: wgpu::BufferUsages,
    data: &[u8],
) {
    let needed = data.len() as u64;
    if slot.as_ref().is_none_or(|b| b.capacity < needed) {
        let capacity = needed.next_power_of_two().max(4096);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("chrome mesh"),
            size: capacity,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        *slot = Some(Growing { buffer, capacity });
    }
    let buffer = &slot.as_ref().expect("made above").buffer;
    queue.write_buffer(buffer, 0, data);
}
