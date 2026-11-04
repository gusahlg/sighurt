//! The CPU backend: renders a [`Scene`] into pixel memory the host lends, with tiny-skia for
//! shapes and the shared glyph rasterizer ([`crate::glyph`]) for text. Always built; `[ui]
//! renderer = "cpu"`.
//!
//! Every frame paints the whole buffer, which may hold an older frame. A render whose scene and
//! metrics are those of the surface's last frame paints nothing and says
//! [`RenderOutcome::Unchanged`]: the surface still shows that frame, so the frontend commits
//! nothing. [`Backend::unbind`] forgets the last frame, for a surface the host hid (its buffer
//! is detached then) or retired. Glyph images are cached across frames and surfaces up to a
//! budget, and the cache starts over when the scale changes.
//!
//! # Pixel format
//!
//! The buffer is Wayland's `WL_SHM_FORMAT_ARGB8888`: 32-bit pixels `0xAARRGGBB`, stored
//! little-endian, so the bytes of each pixel are B, G, R, A, with premultiplied alpha. tiny-skia
//! paints premultiplied R, G, B, A bytes; the backend swaps red and blue once, after drawing, in
//! place when rows are packed and through a reused scratch pixmap when the stride has padding.
//!
//! Must not own: buffers or their lifetime (the host's SHM presenter), what is drawn (the
//! chrome), page pixels, or a native surface (it binds none).

use std::collections::BTreeMap;
use std::time::Instant;

use sighurt_api::geometry::{Metrics, Scale};
use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, PixmapMut, Rect, Transform};

use crate::backend::{
    Backend, BackendStats, NativeWindow, Output, PixelBuffer, Presents, RenderError, RenderOutcome,
    SurfaceKey,
};
use crate::color::Rgba;
use crate::glyph::{GlyphCache, SwashContent};
use crate::scene::{glyph_physical, Kind, Mapping, PixelRect, Scene};
use crate::shape;
use crate::text::FontSystemHandle;

/// The CPU backend. One serves every surface of a window.
pub struct CpuBackend {
    fonts: FontSystemHandle,
    glyphs: GlyphCache,
    /// The scale the cached glyphs were rasterized at.
    scale: Option<Scale>,
    /// Where to draw when the target's rows aren't packed.
    scratch: Option<Pixmap>,
    /// Each surface's last frame: its metrics and scene.
    last: BTreeMap<SurfaceKey, (Metrics, Scene)>,
    stats: BackendStats,
}

impl CpuBackend {
    /// A backend rasterizing glyphs from `fonts`, the font system the chrome shapes with.
    pub fn new(fonts: FontSystemHandle) -> Self {
        Self {
            fonts,
            glyphs: GlyphCache::new(),
            scale: None,
            scratch: None,
            last: BTreeMap::new(),
            stats: BackendStats::default(),
        }
    }

    /// The glyph cache for `scale`: kept from earlier frames at the same scale, and bounded
    /// by its own admission rule ([`GlyphCache`]).
    fn glyph_cache(&mut self, scale: Scale) -> &mut GlyphCache {
        if self.scale != Some(scale) {
            self.scale = Some(scale);
            self.glyphs.clear();
        }
        &mut self.glyphs
    }
}

impl Backend for CpuBackend {
    fn name(&self) -> &'static str {
        "cpu"
    }

    fn presents(&self) -> Presents {
        Presents::Pixels
    }

    fn describe(&self) -> String {
        "tiny-skia 0.11 and swash, into ARGB8888 shared memory".into()
    }

    #[allow(unsafe_code)]
    unsafe fn bind(&mut self, _: SurfaceKey, _: &dyn NativeWindow) -> Result<(), RenderError> {
        Err(RenderError::Output(
            "the CPU backend renders into pixel memory and binds no window".into(),
        ))
    }

    fn unbind(&mut self, surface: SurfaceKey) {
        self.last.remove(&surface);
    }

    fn render(
        &mut self,
        surface: SurfaceKey,
        metrics: &Metrics,
        scene: &Scene,
        output: Output<'_>,
    ) -> Result<RenderOutcome, RenderError> {
        let Output::Pixels(buffer) = output else {
            return Err(RenderError::Output(
                "the CPU backend renders into pixel memory".into(),
            ));
        };
        let started = Instant::now();
        let PixelBuffer {
            data,
            width,
            height,
            stride,
        } = buffer;
        let expected = metrics.buffer();
        if (width, height) != (expected.width, expected.height) {
            return Err(RenderError::Output(format!(
                "buffer is {width}×{height}, the metrics ask for {}×{}",
                expected.width, expected.height
            )));
        }
        if metrics.is_empty() || width == 0 || height == 0 {
            return Ok(RenderOutcome::Submitted);
        }
        let row = width as usize * 4;
        let needed = stride
            .checked_mul(height as usize - 1)
            .and_then(|rows| rows.checked_add(row));
        if stride < row || needed.is_none_or(|needed| data.len() < needed) {
            return Err(RenderError::Output(
                "buffer stride or length too small for its size".into(),
            ));
        }
        if !scene.shaped_with(self.fonts.id()) {
            return Err(RenderError::Output(
                "the scene's text was shaped with another font system".into(),
            ));
        }
        let unchanged = self.last.get(&surface);
        if unchanged.is_some_and(|(last, drawn)| last == metrics && drawn == scene) {
            self.stats.unchanged += 1;
            return Ok(RenderOutcome::Unchanged);
        }

        let mapping = Mapping::new(metrics);
        let fonts = self.fonts.clone();
        let mut scratch = self.scratch.take();
        let glyphs = self.glyph_cache(metrics.scale());
        if stride == row {
            let bytes = &mut data[..row * height as usize];
            let pixmap = PixmapMut::from_bytes(bytes, width, height).expect("size checked");
            draw(pixmap, &mapping, scene, &fonts, glyphs);
            swap_red_blue(bytes);
        } else {
            let pixmap = match scratch.take() {
                Some(pixmap) if (pixmap.width(), pixmap.height()) == (width, height) => pixmap,
                _ => Pixmap::new(width, height).expect("non-empty size"),
            };
            let pixmap = scratch.insert(pixmap);
            draw(pixmap.as_mut(), &mapping, scene, &fonts, glyphs);
            for (source, target) in pixmap.data().chunks_exact(row).zip(data.chunks_mut(stride)) {
                target[..row].copy_from_slice(source);
                swap_red_blue(&mut target[..row]);
            }
        }
        self.scratch = scratch;
        self.last.insert(surface, (*metrics, scene.clone()));
        let took = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        self.stats.frames += 1;
        self.stats.max_render_us = self.stats.max_render_us.max(took);
        self.stats.last_render_us = took;
        Ok(RenderOutcome::Submitted)
    }

    fn stats(&self) -> BackendStats {
        BackendStats {
            glyphs: self.glyphs.len() as u64,
            glyph_bytes: self.glyphs.bytes() as u64,
            glyph_resets: self.glyphs.resets(),
            ..self.stats
        }
    }
}

/// Paints all of `scene` on `pixmap`.
fn draw(
    mut pixmap: PixmapMut<'_>,
    mapping: &Mapping,
    scene: &Scene,
    fonts: &FontSystemHandle,
    glyphs: &mut GlyphCache,
) {
    let Rgba { r, g, b, a } = scene.clear;
    pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, a));
    let transform = shape::transform(mapping);
    for item in &scene.items {
        if let Kind::Text { run, color } = item.kind {
            let clip = match item.clip {
                Some(clip) => mapping.snap(clip),
                None => {
                    PixelRect::from_ltrb(0.0, 0.0, pixmap.width() as f32, pixmap.height() as f32)
                }
            };
            if let Some(clip) = clip {
                text(&mut pixmap, mapping, scene, run, color, clip, fonts, glyphs);
            }
            continue;
        }
        // Shapes are cut to their clip with a mask; the chromes' shapes have none.
        let mask = match item.clip {
            Some(clip) => match mapping.snap(clip).and_then(|clip| clip_mask(&pixmap, clip)) {
                Some(mask) => Some(mask),
                None => continue,
            },
            None => None,
        };
        let mask = mask.as_ref();
        match item.kind {
            Kind::Rect { rect, color } => {
                if let Some(rect) = mapping.snap(rect).and_then(skia_rect) {
                    pixmap.fill_rect(rect, &paint(color), Transform::identity(), mask);
                }
            }
            Kind::RoundedRect {
                rect,
                radius,
                color,
            } => {
                if let Some(rect) = mapping.snap(rect).and_then(skia_rect) {
                    fill_rounded(&mut pixmap, rect, radius * mapping.scale(), color, mask);
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
                if let Some(outer) = skia_rect(bordered.outer) {
                    fill_rounded(&mut pixmap, outer, bordered.outer_radius, border, mask);
                }
                if let Some((inner, radius)) = bordered.inner {
                    if let Some(inner) = skia_rect(inner) {
                        fill_rounded(&mut pixmap, inner, radius, fill, mask);
                    }
                }
            }
            Kind::Path {
                path,
                color,
                stroke,
            } => {
                if let Some(path) = shape::skia_path(scene.path(path)) {
                    shape::paint_path(&mut pixmap, &path, color, stroke, transform, mask);
                }
            }
            Kind::Text { .. } => {}
        }
    }
}

/// Draws glyph run `run` of `scene` in `color`, cut to `clip` and the buffer.
#[allow(clippy::too_many_arguments)]
fn text(
    pixmap: &mut PixmapMut<'_>,
    mapping: &Mapping,
    scene: &Scene,
    run: crate::scene::RunId,
    color: Rgba,
    clip: PixelRect,
    fonts: &FontSystemHandle,
    glyphs: &mut GlyphCache,
) {
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    let clip = (
        (clip.left as i32).max(0),
        (clip.top as i32).max(0),
        (clip.right as i32).min(width),
        (clip.bottom as i32).min(height),
    );
    let run = scene.run(run);
    let data = pixmap.data_mut();
    for glyph in &run.glyphs {
        let (x, y, key) = glyph_physical(mapping, run, glyph);
        let Some(image) = glyphs.image(fonts, key) else {
            continue;
        };
        let (w, h) = (image.placement.width as i32, image.placement.height as i32);
        let x0 = x + image.placement.left;
        let y0 = y - image.placement.top;
        for row in (y0.max(clip.1))..((y0 + h).min(clip.3)) {
            for column in (x0.max(clip.0))..((x0 + w).min(clip.2)) {
                let source = ((row - y0) * w + (column - x0)) as usize;
                let target = ((row * width + column) * 4) as usize;
                let pixel = &mut data[target..target + 4];
                match image.content {
                    SwashContent::Mask => blend(pixel, color, image.data[source]),
                    SwashContent::Color => {
                        let [r, g, b, a] = image.data[source * 4..source * 4 + 4] else {
                            continue;
                        };
                        blend(pixel, Rgba { r, g, b, a }, 255);
                    }
                    SwashContent::SubpixelMask => {}
                }
            }
        }
    }
}

/// A mask of `pixmap`'s size that lets through `clip`. `None` if it has no pixels.
fn clip_mask(pixmap: &PixmapMut<'_>, clip: PixelRect) -> Option<Mask> {
    let mut mask = Mask::new(pixmap.width(), pixmap.height())?;
    let path = PathBuilder::from_rect(skia_rect(clip)?);
    mask.fill_path(&path, FillRule::Winding, false, Transform::identity());
    Some(mask)
}

fn skia_rect(rect: PixelRect) -> Option<Rect> {
    Rect::from_ltrb(rect.left, rect.top, rect.right, rect.bottom)
}

/// Turns premultiplied RGBA bytes into premultiplied BGRA (ARGB8888 little-endian), in place.
fn swap_red_blue(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
}

fn fill_rounded(
    pixmap: &mut PixmapMut<'_>,
    rect: Rect,
    radius: f32,
    color: Rgba,
    mask: Option<&Mask>,
) {
    if let Some(path) = rounded_rect(rect, radius) {
        let transform = Transform::identity();
        pixmap.fill_path(&path, &paint(color), FillRule::Winding, transform, mask);
    }
}

fn paint(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    paint
}

/// A rounded rectangle path, with cubic corners.
fn rounded_rect(rect: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let radius = radius
        .min(rect.width() / 2.0)
        .min(rect.height() / 2.0)
        .max(0.0);
    // How far a corner's control points sit from the corner, for a circular arc.
    let k = radius * (1.0 - 0.552_284_8);
    let (l, t, r, b) = (rect.left(), rect.top(), rect.right(), rect.bottom());
    let mut path = PathBuilder::new();
    path.move_to(l + radius, t);
    path.line_to(r - radius, t);
    path.cubic_to(r - k, t, r, t + k, r, t + radius);
    path.line_to(r, b - radius);
    path.cubic_to(r, b - k, r - k, b, r - radius, b);
    path.line_to(l + radius, b);
    path.cubic_to(l + k, b, l, b - k, l, b - radius);
    path.line_to(l, t + radius);
    path.cubic_to(l, t + k, l + k, t, l + radius, t);
    path.close();
    path.finish()
}

/// Blends `color` at `coverage` over a premultiplied RGBA pixel.
fn blend(pixel: &mut [u8], color: Rgba, coverage: u8) {
    let alpha = mul(color.a, coverage);
    if alpha == 0 {
        return;
    }
    let keep = 255 - alpha;
    let source = [
        mul(color.r, alpha),
        mul(color.g, alpha),
        mul(color.b, alpha),
        alpha,
    ];
    for (target, source) in pixel.iter_mut().zip(source) {
        *target = source.saturating_add(mul(*target, keep));
    }
}

/// `a × b / 255`, rounded.
fn mul(a: u8, b: u8) -> u8 {
    ((u16::from(a) * u16::from(b) + 127) / 255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blending_is_premultiplied_source_over() {
        // Opaque over anything replaces it.
        let mut pixel = [10, 20, 30, 255];
        blend(&mut pixel, Rgba::rgb(0xff8000), 255);
        assert_eq!(pixel, [255, 128, 0, 255]);
        // Half coverage of white over opaque black is mid grey, still opaque.
        let mut pixel = [0, 0, 0, 255];
        blend(&mut pixel, Rgba::rgb(0xffffff), 128);
        assert_eq!(pixel, [128, 128, 128, 255]);
        // A translucent color over transparent stays premultiplied.
        let mut pixel = [0, 0, 0, 0];
        blend(&mut pixel, Rgba::rgb_alpha(0xff000080), 255);
        assert_eq!(pixel, [128, 0, 0, 128]);
        // No coverage changes nothing.
        let mut pixel = [1, 2, 3, 4];
        blend(&mut pixel, Rgba::rgb(0xffffff), 0);
        assert_eq!(pixel, [1, 2, 3, 4]);
    }

    #[test]
    fn rgba_becomes_argb8888_little_endian() {
        // tiny-skia's premultiplied RGBA bytes...
        let mut pixels = [0x11, 0x22, 0x33, 0xff, 0x80, 0x00, 0x00, 0x80];
        swap_red_blue(&mut pixels);
        // ...read back as the 32-bit ARGB values wl_shm expects.
        let first = u32::from_le_bytes(pixels[..4].try_into().unwrap());
        let second = u32::from_le_bytes(pixels[4..].try_into().unwrap());
        assert_eq!(first, 0xff11_2233);
        assert_eq!(second, 0x8080_0000, "half-transparent red, premultiplied");
    }
}
