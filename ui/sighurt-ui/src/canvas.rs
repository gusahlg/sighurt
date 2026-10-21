//! Drawing into buffer memory the host lends: [`Canvas`] checks a buffer against its target's
//! metrics and hands a [`Pen`] that fills rectangles and draws shaped text at the target's
//! scale, with a glyph cache kept from frame to frame. Part of the chrome API: the standard
//! chrome's painter ([`crate::paint_cpu`]) draws with it, and so can another chrome.
//!
//! A pen draws in window-logical coordinates. Rectangles are snapped to buffer pixels, so a
//! rectangle covers exactly the pixels whose centres lie inside it: controls stay crisp at
//! fractional scales, and a hit test against the same rectangles stays on what is drawn. Text
//! shaped by [`Fonts`] in logical pixels has its glyphs rasterized by swash at the target's
//! scale, hinted, into a glyph cache with a fixed budget that starts over when the scale
//! changes.
//!
//! # Pixel format
//!
//! The buffer is Wayland's `WL_SHM_FORMAT_ARGB8888`: 32-bit pixels `0xAARRGGBB`, stored
//! little-endian, so the bytes of each pixel are B, G, R, A, with premultiplied alpha. tiny-skia
//! paints premultiplied R, G, B, A bytes; the canvas swaps red and blue once, after drawing, in
//! place when rows are packed and through a reused scratch pixmap when the stride has padding.
//!
//! Must not own: buffers or their lifetime (the host's SHM presenter), what is drawn (the
//! chrome), or page pixels.

use std::fmt;

use cosmic_text::{SwashCache, SwashContent};
use sighurt_api::geometry::{BufferSize, Metrics, Scale};
use tiny_skia::{
    FillRule, LineCap, Paint, PathBuilder, Pixmap, PixmapMut, Rect, Stroke, Transform,
};

use crate::chrome::Rgba;
use crate::text::{Fonts, ShapedText};

/// Rasterized glyphs kept before the cache starts over. Chrome text uses a few hundred.
const MAX_GLYPHS: usize = 2048;

/// Memory in `WL_SHM_FORMAT_ARGB8888` (premultiplied, little-endian: bytes B, G, R, A) to paint
/// into.
pub struct PixelBuffer<'a> {
    /// The pixels, row by row.
    pub data: &'a mut [u8],
    /// Width in buffer pixels.
    pub width: u32,
    /// Height in buffer pixels.
    pub height: u32,
    /// Bytes from one row's start to the next's: at least `width * 4`.
    pub stride: usize,
}

/// Why a buffer couldn't be painted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaintError {
    /// The buffer isn't the size the metrics ask for.
    WrongSize {
        /// The metrics' buffer size.
        expected: BufferSize,
        /// The buffer's width.
        width: u32,
        /// The buffer's height.
        height: u32,
    },
    /// The stride is shorter than a row, or the memory is shorter than the rows.
    WrongLayout,
}

impl fmt::Display for PaintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongSize {
                expected,
                width,
                height,
            } => write!(
                f,
                "buffer is {width}×{height}, the metrics ask for {}×{}",
                expected.width, expected.height
            ),
            Self::WrongLayout => f.write_str("buffer stride or length too small for its size"),
        }
    }
}

impl std::error::Error for PaintError {}

/// What a chrome draws with: the glyph cache and scratch memory it keeps between frames. One
/// canvas serves every surface of a window.
#[derive(Default)]
pub struct Canvas {
    glyphs: Option<SwashCache>,
    /// The scale the cached glyphs were rasterized at.
    scale: Option<Scale>,
    /// Where to draw when the target's rows aren't packed.
    scratch: Option<Pixmap>,
}

impl Canvas {
    /// A canvas with empty caches.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lets `draw` paint `buffer`, which must be `metrics.buffer()` in size, with a pen for the
    /// target's rectangle, `metrics.rect()`; then turns the pixels into ARGB8888. `draw` must
    /// cover the whole buffer ([`Pen::clear`] first), since the buffer may hold an older frame.
    /// An empty rectangle draws nothing.
    pub fn paint(
        &mut self,
        metrics: &Metrics,
        buffer: PixelBuffer<'_>,
        draw: impl FnOnce(&mut Pen<'_>),
    ) -> Result<(), PaintError> {
        let PixelBuffer {
            data,
            width,
            height,
            stride,
        } = buffer;
        let expected = metrics.buffer();
        if (width, height) != (expected.width, expected.height) {
            return Err(PaintError::WrongSize {
                expected,
                width,
                height,
            });
        }
        if metrics.is_empty() || width == 0 || height == 0 {
            return Ok(());
        }
        let row = width as usize * 4;
        let needed = stride
            .checked_mul(height as usize - 1)
            .and_then(|rows| rows.checked_add(row));
        if stride < row || needed.is_none_or(|needed| data.len() < needed) {
            return Err(PaintError::WrongLayout);
        }

        if self.scale != Some(metrics.scale()) {
            self.scale = Some(metrics.scale());
            self.glyphs = None;
        }
        let glyphs = self.glyphs.get_or_insert_with(SwashCache::new);
        if glyphs.image_cache.len() > MAX_GLYPHS {
            glyphs.image_cache.clear();
        }

        if stride == row {
            let bytes = &mut data[..row * height as usize];
            let pixmap = PixmapMut::from_bytes(bytes, width, height).expect("size checked");
            draw(&mut Pen::new(pixmap, metrics, glyphs));
            swap_red_blue(bytes);
        } else {
            let scratch = match self.scratch.take() {
                Some(pixmap) if (pixmap.width(), pixmap.height()) == (width, height) => pixmap,
                _ => Pixmap::new(width, height).expect("non-empty size"),
            };
            let scratch = self.scratch.insert(scratch);
            draw(&mut Pen::new(scratch.as_mut(), metrics, glyphs));
            for (source, target) in scratch
                .data()
                .chunks_exact(row)
                .zip(data.chunks_mut(stride))
            {
                target[..row].copy_from_slice(source);
                swap_red_blue(&mut target[..row]);
            }
        }
        Ok(())
    }
}

/// Turns premultiplied RGBA bytes into premultiplied BGRA (ARGB8888 little-endian), in place.
fn swap_red_blue(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
}

/// Draws on one buffer, in window-logical coordinates: a rectangle is `[x, y, width, height]`.
/// What falls outside the target's rectangle is cut off.
pub struct Pen<'a> {
    pixmap: PixmapMut<'a>,
    glyphs: &'a mut SwashCache,
    /// The target's window-logical origin.
    origin: (f32, f32),
    /// Buffer pixels per logical pixel, from the actual buffer size.
    ratio: (f32, f32),
    /// The nominal scale, for text and stroke sizes.
    scale: f32,
}

impl<'a> Pen<'a> {
    fn new(pixmap: PixmapMut<'a>, metrics: &Metrics, glyphs: &'a mut SwashCache) -> Self {
        let target = metrics.rect();
        let buffer = metrics.buffer();
        Self {
            pixmap,
            glyphs,
            origin: (target.x as f32, target.y as f32),
            ratio: (
                buffer.width as f32 / target.width as f32,
                buffer.height as f32 / target.height as f32,
            ),
            scale: metrics.scale().factor() as f32,
        }
    }

    /// Fills the whole buffer with `color`.
    pub fn clear(&mut self, color: Rgba) {
        let Rgba { r, g, b, a } = color;
        self.pixmap.fill(tiny_skia::Color::from_rgba8(r, g, b, a));
    }

    /// Fills `rect`, its edges snapped to the buffer's pixels.
    pub fn fill(&mut self, rect: [f32; 4], color: Rgba) {
        if let Some(rect) = self.snap(rect) {
            self.pixmap
                .fill_rect(rect, &paint(color), Transform::identity(), None);
        }
    }

    /// Draws `text`, shaped by `fonts`, with its top-left corner at (`x`, `y`), cut to `clip`.
    /// Baselines fall on whole buffer pixels, so text stays sharp vertically; horizontally,
    /// glyphs keep their subpixel positions.
    pub fn text(
        &mut self,
        fonts: &mut Fonts,
        text: &ShapedText,
        (x, y): (f32, f32),
        color: Rgba,
        clip: [f32; 4],
    ) {
        let Some(clip) = self.snap(clip) else {
            return;
        };
        let (width, height) = (self.pixmap.width() as i32, self.pixmap.height() as i32);
        let clip = (
            (clip.left() as i32).max(0),
            (clip.top() as i32).max(0),
            (clip.right() as i32).min(width),
            (clip.bottom() as i32).min(height),
        );
        let left = (x - self.origin.0) * self.ratio.0;
        let top = (y - self.origin.1) * self.ratio.1;
        let fonts = fonts.font_system();
        let data = self.pixmap.data_mut();
        for run in text.runs() {
            let baseline = (top + run.line_y * self.scale).round() as i32;
            for glyph in run.glyphs {
                let physical = glyph.physical((left, 0.0), self.scale);
                let Some(image) = self.glyphs.get_image(fonts, physical.cache_key) else {
                    continue;
                };
                let (w, h) = (image.placement.width as i32, image.placement.height as i32);
                let x0 = physical.x + image.placement.left;
                let y0 = baseline + physical.y - image.placement.top;
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
    }

    /// Window-logical `x` in buffer pixels, snapped to the pixel grid so that a rectangle covers
    /// exactly the pixels whose centres lie inside it: the pixels whose centres hit it.
    fn x(&self, x: f32) -> f32 {
        snap((x - self.origin.0) * self.ratio.0)
    }

    fn y(&self, y: f32) -> f32 {
        snap((y - self.origin.1) * self.ratio.1)
    }

    /// A logical rectangle as buffer pixels, with each edge snapped.
    fn snap(&self, [x, y, width, height]: [f32; 4]) -> Option<Rect> {
        Rect::from_ltrb(self.x(x), self.y(y), self.x(x + width), self.y(y + height))
    }

    /// Maps logical paths onto the buffer, unsnapped.
    fn transform(&self) -> Transform {
        Transform::from_row(
            self.ratio.0,
            0.0,
            0.0,
            self.ratio.1,
            -self.origin.0 * self.ratio.0,
            -self.origin.1 * self.ratio.1,
        )
    }

    /// A rectangle with rounded corners, `radius` logical pixels.
    pub(crate) fn fill_rounded(&mut self, rect: [f32; 4], radius: f32, color: Rgba) {
        if let Some(rect) = self.snap(rect) {
            self.fill_rounded_pixels(rect, radius * self.scale, color);
        }
    }

    /// A rounded rectangle with a one-pixel border: `border` around a `fill` inside.
    pub(crate) fn bordered(&mut self, rect: [f32; 4], radius: f32, fill: Rgba, border: Rgba) {
        let Some(outer) = self.snap(rect) else {
            return;
        };
        let line = self.scale.round().max(1.0);
        let radius = radius * self.scale;
        self.fill_rounded_pixels(outer, radius, border);
        let inner = Rect::from_ltrb(
            outer.left() + line,
            outer.top() + line,
            outer.right() - line,
            outer.bottom() - line,
        );
        if let Some(inner) = inner {
            self.fill_rounded_pixels(inner, radius - line, fill);
        }
    }

    fn fill_rounded_pixels(&mut self, rect: Rect, radius: f32, color: Rgba) {
        if let Some(path) = rounded_rect(rect, radius) {
            let transform = Transform::identity();
            self.pixmap
                .fill_path(&path, &paint(color), FillRule::Winding, transform, None);
        }
    }

    /// Strokes the lines of `build` (in logical coordinates) `width` logical pixels wide.
    pub(crate) fn stroke(&mut self, width: f32, color: Rgba, build: impl FnOnce(&mut PathBuilder)) {
        let mut builder = PathBuilder::new();
        build(&mut builder);
        if let Some(path) = builder.finish() {
            let stroke = Stroke {
                width,
                line_cap: LineCap::Round,
                ..Stroke::default()
            };
            let transform = self.transform();
            self.pixmap
                .stroke_path(&path, &paint(color), &stroke, transform, None);
        }
    }

    /// Fills the shape `build` makes (in logical coordinates).
    pub(crate) fn shape(&mut self, color: Rgba, build: impl FnOnce(&mut PathBuilder)) {
        let mut builder = PathBuilder::new();
        build(&mut builder);
        if let Some(path) = builder.finish() {
            let transform = self.transform();
            self.pixmap
                .fill_path(&path, &paint(color), FillRule::Winding, transform, None);
        }
    }
}

/// The pixel edge for buffer position `v`: the first pixel whose centre is at or after it.
fn snap(v: f32) -> f32 {
    (v - 0.5).ceil()
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
