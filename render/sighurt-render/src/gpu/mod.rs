//! What every GPU backend prepares the same way (feature `gpu`): a [`Scene`] as one mesh of
//! quads in buffer pixels, snapped with the shared [`Mapping`]; the [`Atlas`] that places
//! glyph and icon images in two textures; and the one program both draw with ([`SHADER`]).
//! A backend only creates its device objects, uploads what [`prepare`] hands it, and draws.
//!
//! # Coordinates
//!
//! Positions are buffer pixels, origin at the top-left, y down; the vertex shader maps them
//! with an orthographic projection from the configured size (the viewport uniform). A quad's
//! corners are whole pixels: rectangles are snapped with [`Mapping::snap`], glyphs placed with
//! [`glyph_physical`], icons rasterized at the [`Mapping::path_transform`] into tiles offset by
//! whole pixels ([`crate::shape`]). So every primitive covers the pixels the CPU backend
//! covers; only antialiased rounded corners differ (an analytic signed distance here, a
//! cubic-cornered path there).
//!
//! # Vertex layout
//!
//! One vertex buffer, [`Vertex`], 68 bytes a vertex ([`VERTEX_STRIDE`]), every attribute
//! 4-byte aligned, per vertex (no instancing), and `u32` indices, six a quad:
//!
//! | location | field | format | offset |
//! |---|---|---|---|
//! | 0 | `position` | `Float32x2`, buffer pixels | 0 |
//! | 1 | `uv` | `Float32x2`, atlas texels (MASK, COLOR) | 8 |
//! | 2 | `color` | `Float32x4`, premultiplied 0–1 | 16 |
//! | 3 | `rect` | `Float32x4`, ROUNDED: left, top, right, bottom in pixels | 32 |
//! | 4 | `params` | `Float32x4`, ROUNDED: `x` the corner radius in pixels | 48 |
//! | 5 | `kind` | `Uint32`: [`SOLID`], [`ROUNDED`], [`MASK`], [`COLOR`] | 64 |
//!
//! `rect` and `params` are the primitive's two parameter vectors (RENDER.md §14.11); they are
//! flat-interpolated. A test checks this table against the WGSL's vertex input.
//!
//! # Bindings
//!
//! Group 0: binding 0 a uniform buffer of [`UNIFORM_SIZE`] bytes ([`uniform`]: the viewport's
//! width and height as `f32`, then 8 bytes unused), visible to the vertex stage; binding 1 the
//! mask page (`R8Unorm`, `texture_2d<f32>`), binding 2 the color page (`Rgba8Unorm`,
//! premultiplied), both read with `textureLoad` at whole texels in the fragment stage (no
//! sampler, no filtering: a tile maps one texel to one pixel).
//!
//! # Drawing
//!
//! One pipeline, triangle list, no culling, no depth; blending premultiplied source-over (`One`,
//! `OneMinusSrcAlpha`, for color and alpha) into a `Bgra8Unorm` target (not sRGB: blending in
//! encoded space, as tiny-skia does), cleared to [`Prepared::clear`] (opaque). The
//! [`Prepared::batches`] are drawn in order, each with its scissor rectangle (its items' clip,
//! clamped to the buffer). naga parses [`SHADER`] with its default WGSL options; for SPIR-V
//! or MSL the entry points are [`VERTEX_ENTRY`] and [`FRAGMENT_ENTRY`].
//!
//! Must not own: device objects, uploads or presentation (the backends'), what is drawn (the
//! chrome's), or the snapping and placement rules (`crate::scene`'s, shared with the CPU
//! backend).

mod atlas;

use std::ops::Range;

pub use atlas::{Atlas, AtlasFull, PageKind, Tile, Upload, INITIAL_SIZE, MAX_SIZE};
use sighurt_api::geometry::Metrics;

use crate::color::Rgba;
use crate::scene::{glyph_physical, Kind, Mapping, PixelRect, Scene};
use crate::shape;
use crate::text::FontSystemHandle;

/// The program: one vertex and one fragment shader in WGSL.
pub const SHADER: &str = include_str!("shaders.wgsl");
/// The vertex shader's entry point.
pub const VERTEX_ENTRY: &str = "vs_main";
/// The fragment shader's entry point.
pub const FRAGMENT_ENTRY: &str = "fs_main";

/// A flat color.
pub const SOLID: u32 = 0;
/// A rectangle with rounded corners: `rect` and `params.x`.
pub const ROUNDED: u32 = 1;
/// The color times the mask page's coverage at `uv`.
pub const MASK: u32 = 2;
/// The color page's premultiplied texel at `uv`.
pub const COLOR: u32 = 3;

/// One vertex; see the module's table.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub struct Vertex {
    /// Buffer pixels.
    pub position: [f32; 2],
    /// Atlas texels.
    pub uv: [f32; 2],
    /// Premultiplied.
    pub color: [f32; 4],
    /// ROUNDED: left, top, right, bottom.
    pub rect: [f32; 4],
    /// ROUNDED: `[radius, 0, 0, 0]`.
    pub params: [f32; 4],
    /// [`SOLID`], [`ROUNDED`], [`MASK`] or [`COLOR`].
    pub kind: u32,
}

/// Bytes from one vertex to the next.
pub const VERTEX_STRIDE: u64 = std::mem::size_of::<Vertex>() as u64;

/// A vertex attribute's format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeFormat {
    /// Two `f32`.
    Float32x2,
    /// Four `f32`.
    Float32x4,
    /// One `u32`.
    Uint32,
}

/// One vertex attribute: its shader location, format and offset in [`Vertex`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attribute {
    /// `@location`.
    pub location: u32,
    /// Format.
    pub format: AttributeFormat,
    /// Byte offset.
    pub offset: u64,
}

/// [`Vertex`]'s attributes, as a backend declares them.
pub const VERTEX_ATTRIBUTES: [Attribute; 6] = [
    attribute(
        0,
        AttributeFormat::Float32x2,
        std::mem::offset_of!(Vertex, position),
    ),
    attribute(
        1,
        AttributeFormat::Float32x2,
        std::mem::offset_of!(Vertex, uv),
    ),
    attribute(
        2,
        AttributeFormat::Float32x4,
        std::mem::offset_of!(Vertex, color),
    ),
    attribute(
        3,
        AttributeFormat::Float32x4,
        std::mem::offset_of!(Vertex, rect),
    ),
    attribute(
        4,
        AttributeFormat::Float32x4,
        std::mem::offset_of!(Vertex, params),
    ),
    attribute(
        5,
        AttributeFormat::Uint32,
        std::mem::offset_of!(Vertex, kind),
    ),
];

const fn attribute(location: u32, format: AttributeFormat, offset: usize) -> Attribute {
    Attribute {
        location,
        format,
        offset: offset as u64,
    }
}

/// Bytes of the uniform buffer at binding 0.
pub const UNIFORM_SIZE: u64 = 16;

/// The uniform buffer's contents for a target `width` × `height` pixels.
pub fn uniform(width: u32, height: u32) -> [u8; UNIFORM_SIZE as usize] {
    let values = [width as f32, height as f32, 0.0, 0.0];
    bytemuck::cast(values)
}

/// A run of indices drawn with one scissor rectangle.
#[derive(Clone, Debug, PartialEq)]
pub struct Batch {
    /// The scissor: the items' clip in buffer pixels, whole, inside the buffer, not empty.
    pub clip: PixelRect,
    /// Its indices in [`Prepared::indices`].
    pub indices: Range<u32>,
}

impl Batch {
    /// The scissor as `x, y, width, height`.
    pub fn scissor(&self) -> [u32; 4] {
        let PixelRect {
            left,
            top,
            right,
            bottom,
        } = self.clip;
        [
            left as u32,
            top as u32,
            (right - left) as u32,
            (bottom - top) as u32,
        ]
    }
}

/// A frame ready to draw: what [`prepare`] made of a scene.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prepared {
    /// The buffer's size in pixels.
    pub size: (u32, u32),
    /// The clear color, opaque, 0–1.
    pub clear: [f32; 4],
    /// The vertices, four a quad.
    pub vertices: Vec<Vertex>,
    /// The indices, six a quad.
    pub indices: Vec<u32>,
    /// What to draw, in order.
    pub batches: Vec<Batch>,
    /// Texels to copy into the atlas pages before drawing.
    pub uploads: Vec<Upload>,
    /// The pages' sides: the backend's textures must have them (a page that grew is a new
    /// texture).
    pub mask_size: u32,
    /// See `mask_size`.
    pub color_size: u32,
}

impl Prepared {
    /// The vertices as bytes.
    pub fn vertex_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.vertices)
    }

    /// The indices as bytes.
    pub fn index_bytes(&self) -> &[u8] {
        bytemuck::cast_slice(&self.indices)
    }

    /// Quads drawn.
    pub fn quads(&self) -> usize {
        self.indices.len() / 6
    }
}

/// Prepares `scene` for a target with `metrics`: places its glyphs and this frame's icons in
/// `atlas` (rasterizing new ones from `fonts`, the font system the scene was shaped with), then
/// builds the mesh, so no texture coordinate is computed before its tile is placed. A page that
/// fills grows or starts over and the frame's placement restarts (RENDER.md §14.10); a frame
/// that still doesn't fit is [`AtlasFull`]. Empty metrics prepare nothing.
pub fn prepare(
    scene: &Scene,
    metrics: &Metrics,
    atlas: &mut Atlas,
    fonts: &FontSystemHandle,
) -> Result<Prepared, AtlasFull> {
    let buffer = metrics.buffer();
    let Rgba { r, g, b, .. } = scene.clear;
    let mut prepared = Prepared {
        size: (buffer.width, buffer.height),
        clear: [
            f32::from(r) / 255.0,
            f32::from(g) / 255.0,
            f32::from(b) / 255.0,
            1.0,
        ],
        mask_size: atlas.size(PageKind::Mask),
        color_size: atlas.size(PageKind::Color),
        ..Prepared::default()
    };
    if metrics.is_empty() || buffer.width == 0 || buffer.height == 0 {
        return Ok(prepared);
    }
    let mapping = Mapping::new(metrics);
    atlas.begin_frame(metrics.scale());
    let icons = loop {
        match place(scene, &mapping, atlas, fonts) {
            Ok(icons) => break icons,
            Err(page) => atlas.make_room(page)?,
        }
    };
    let surface = PixelRect::from_ltrb(0.0, 0.0, buffer.width as f32, buffer.height as f32)
        .expect("a non-empty buffer");
    let mut mesh = Mesh {
        prepared: &mut prepared,
    };
    let mut icons = icons.into_iter();
    for item in &scene.items {
        let clip = match item.clip {
            None => Some(surface),
            Some(clip) => mapping.snap(clip).and_then(|clip| intersect(clip, surface)),
        };
        // A path's icon was placed in order, clip or not.
        let icon = matches!(item.kind, Kind::Path { .. }).then(|| icons.next().flatten());
        let Some(clip) = clip else {
            continue;
        };
        match item.kind {
            Kind::Rect { rect, color } => {
                if let Some(rect) = mapping.snap(rect) {
                    mesh.quad(clip, rect, color, SOLID, [0.0; 4], [0.0; 4], [0.0; 4]);
                }
            }
            Kind::RoundedRect {
                rect,
                radius,
                color,
            } => {
                if let Some(rect) = mapping.snap(rect) {
                    mesh.rounded(clip, rect, radius * mapping.scale(), color);
                }
            }
            Kind::Bordered {
                rect,
                radius,
                fill,
                border,
            } => {
                let Some(bordered) = mapping.bordered(rect, radius) else {
                    continue;
                };
                mesh.rounded(clip, bordered.outer, bordered.outer_radius, border);
                if let Some((inner, radius)) = bordered.inner {
                    mesh.rounded(clip, inner, radius, fill);
                }
            }
            Kind::Path { color, .. } => {
                if let Some(Some((bounds, tile))) = icon {
                    let rect = PixelRect::from_ltrb(
                        bounds.x as f32,
                        bounds.y as f32,
                        (bounds.x + bounds.width as i32) as f32,
                        (bounds.y + bounds.height as i32) as f32,
                    )
                    .expect("whole pixels");
                    mesh.tile(clip, rect, &tile, color, MASK);
                }
            }
            Kind::Text { run, color } => {
                let run = scene.run(run);
                for glyph in &run.glyphs {
                    let (x, y, key) = glyph_physical(&mapping, run, glyph);
                    let Some(tile) = atlas.placed(key) else {
                        continue;
                    };
                    let left = (x + tile.left) as f32;
                    let top = (y - tile.top) as f32;
                    let rect = PixelRect::from_ltrb(
                        left,
                        top,
                        left + tile.width as f32,
                        top + tile.height as f32,
                    )
                    .expect("whole pixels");
                    let kind = match tile.page {
                        PageKind::Mask => MASK,
                        PageKind::Color => COLOR,
                    };
                    mesh.tile(clip, rect, &tile, color, kind);
                }
            }
        }
    }
    prepared.uploads = atlas.take_uploads();
    prepared.mask_size = atlas.size(PageKind::Mask);
    prepared.color_size = atlas.size(PageKind::Color);
    Ok(prepared)
}

/// The tiles of a scene, placed: each glyph's in the atlas, and each path item's icon (in
/// item order: `None` for a path that covers nothing). `Err` names the page that filled.
#[allow(clippy::type_complexity)]
fn place(
    scene: &Scene,
    mapping: &Mapping,
    atlas: &mut Atlas,
    fonts: &FontSystemHandle,
) -> Result<Vec<Option<(shape::PixelBounds, Tile)>>, PageKind> {
    let mut icons = Vec::new();
    for item in &scene.items {
        match item.kind {
            Kind::Text { run, .. } => {
                let run = scene.run(run);
                for glyph in &run.glyphs {
                    let (_, _, key) = glyph_physical(mapping, run, glyph);
                    atlas.glyph(fonts, key)?;
                }
            }
            Kind::Path { path, stroke, .. } => {
                let path = scene.path(path);
                let coverage = shape::ink_bounds(path, stroke, mapping)
                    .and_then(|bounds| shape::coverage(path, stroke, mapping, bounds));
                icons.push(match coverage {
                    Some(coverage) => {
                        let bounds = coverage.bounds;
                        Some((bounds, atlas.icon(coverage)?))
                    }
                    None => None,
                });
            }
            Kind::Rect { .. } | Kind::RoundedRect { .. } | Kind::Bordered { .. } => {}
        }
    }
    Ok(icons)
}

fn intersect(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let rect = PixelRect::from_ltrb(
        a.left.max(b.left),
        a.top.max(b.top),
        a.right.min(b.right),
        a.bottom.min(b.bottom),
    )?;
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

/// Premultiplied 0–1.
fn premultiplied(color: Rgba) -> [f32; 4] {
    let a = f32::from(color.a) / 255.0;
    let c = |v: u8| f32::from(v) / 255.0 * a;
    [c(color.r), c(color.g), c(color.b), a]
}

/// Appends quads and their batches.
struct Mesh<'a> {
    prepared: &'a mut Prepared,
}

impl Mesh<'_> {
    fn rounded(&mut self, clip: PixelRect, rect: PixelRect, radius: f32, color: Rgba) {
        let bounds = [rect.left, rect.top, rect.right, rect.bottom];
        let params = [radius.max(0.0), 0.0, 0.0, 0.0];
        self.quad(clip, rect, color, ROUNDED, [0.0; 4], bounds, params);
    }

    fn tile(&mut self, clip: PixelRect, rect: PixelRect, tile: &Tile, color: Rgba, kind: u32) {
        let (x, y) = (tile.x as f32, tile.y as f32);
        let uv = [x, y, x + tile.width as f32, y + tile.height as f32];
        self.quad(clip, rect, color, kind, uv, [0.0; 4], [0.0; 4]);
    }

    #[allow(clippy::too_many_arguments)]
    fn quad(
        &mut self,
        clip: PixelRect,
        rect: PixelRect,
        color: Rgba,
        kind: u32,
        uv: [f32; 4],
        bounds: [f32; 4],
        params: [f32; 4],
    ) {
        if intersect(rect, clip).is_none() {
            return;
        }
        let p = &mut *self.prepared;
        let base = p.vertices.len() as u32;
        let color = premultiplied(color);
        let corners = [
            ([rect.left, rect.top], [uv[0], uv[1]]),
            ([rect.right, rect.top], [uv[2], uv[1]]),
            ([rect.left, rect.bottom], [uv[0], uv[3]]),
            ([rect.right, rect.bottom], [uv[2], uv[3]]),
        ];
        for (position, uv) in corners {
            p.vertices.push(Vertex {
                position,
                uv,
                color,
                rect: bounds,
                params,
                kind,
            });
        }
        let start = p.indices.len() as u32;
        p.indices
            .extend([0, 1, 2, 2, 1, 3].map(|corner| base + corner));
        let end = p.indices.len() as u32;
        match p.batches.last_mut() {
            Some(batch) if batch.clip == clip && batch.indices.end == start => {
                batch.indices.end = end
            }
            _ => p.batches.push(Batch {
                clip,
                indices: start..end,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
