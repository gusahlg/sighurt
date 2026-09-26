//! The CPU painter: draws the chrome into buffer memory the host lends it.
//!
//! Given a target's [`Metrics`], it paints whatever the chrome shows inside the target's
//! rectangle: the tab strip and toolbar for the chrome surface, the status strip, or a page the
//! chrome draws itself. Every paint covers the whole buffer, so a buffer that last held an
//! older frame (or garbage) comes out complete.
//!
//! Rectangles are snapped to buffer pixels from the same layout rectangles the hit test uses,
//! so controls stay crisp at fractional scales and stay under the pointer. Shapes and icons are
//! drawn with tiny-skia; text is shaped by cosmic-text in logical pixels and its glyphs
//! rasterized by swash at the target's scale, hinted, into a glyph cache with a fixed budget
//! that starts over when the scale changes.
//!
//! # Pixel format
//!
//! The buffer is Wayland's `WL_SHM_FORMAT_ARGB8888`: 32-bit pixels `0xAARRGGBB`, stored
//! little-endian, so the bytes of each pixel are B, G, R, A, with premultiplied alpha. tiny-skia
//! paints premultiplied R, G, B, A bytes; the painter swaps red and blue once, after painting,
//! in place when rows are packed and through a reused scratch pixmap when the stride has
//! padding. The chrome is opaque, so the buffer also works as `XRGB8888`.
//!
//! Must not own: buffers or their lifetime (the host's SHM presenter), or page pixels.

use std::fmt;

use cosmic_text::{SwashCache, SwashContent};
use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
use tiny_skia::{
    FillRule, LineCap, Paint, PathBuilder, Pixmap, PixmapMut, Rect, Stroke, Transform,
};

use crate::chrome::{Chrome, Colors, Rgba, Tone, CARET_WIDTH};
use crate::hit_test::Target;
use crate::text::{ShapedText, TextStyle};

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

/// Paints the chrome on the CPU. One painter serves every chrome surface of a window.
#[derive(Default)]
pub struct CpuPainter {
    glyphs: Option<SwashCache>,
    /// The scale the cached glyphs were rasterized at.
    scale: Option<Scale>,
    /// Where to paint when the target's rows aren't packed.
    scratch: Option<Pixmap>,
}

impl CpuPainter {
    /// A painter with empty caches.
    pub fn new() -> Self {
        Self::default()
    }

    /// Paints what `chrome` shows inside `metrics.rect()` into `buffer`, which must be
    /// `metrics.buffer()` in size. An empty rectangle paints nothing.
    pub fn paint(
        &mut self,
        chrome: &mut Chrome,
        metrics: &Metrics,
        buffer: PixelBuffer<'_>,
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
            draw(chrome, metrics, glyphs, pixmap);
            swap_red_blue(bytes);
        } else {
            let scratch = match self.scratch.take() {
                Some(pixmap) if (pixmap.width(), pixmap.height()) == (width, height) => pixmap,
                _ => Pixmap::new(width, height).expect("non-empty size"),
            };
            let scratch = self.scratch.insert(scratch);
            draw(chrome, metrics, glyphs, scratch.as_mut());
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

/// A buffer being painted, with the mapping from window-logical coordinates to its pixels.
struct Canvas<'a> {
    pixmap: PixmapMut<'a>,
    /// The target's window-logical origin.
    origin: (f32, f32),
    /// Buffer pixels per logical pixel, from the actual buffer size.
    ratio: (f32, f32),
    /// The nominal scale, for text and stroke sizes.
    scale: f32,
}

impl Canvas<'_> {
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

    fn fill(&mut self, rect: [f32; 4], color: Rgba) {
        if let Some(rect) = self.snap(rect) {
            self.pixmap
                .fill_rect(rect, &paint(color), Transform::identity(), None);
        }
    }

    /// A rectangle with rounded corners, `radius` logical pixels.
    fn fill_rounded(&mut self, rect: [f32; 4], radius: f32, color: Rgba) {
        if let Some(rect) = self.snap(rect) {
            self.fill_rounded_pixels(rect, radius * self.scale, color);
        }
    }

    /// A rounded rectangle with a one-pixel border: `border` around a `fill` inside.
    fn bordered(&mut self, rect: [f32; 4], radius: f32, fill: Rgba, border: Rgba) {
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
    fn stroke(&mut self, width: f32, color: Rgba, build: impl FnOnce(&mut PathBuilder)) {
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
    fn shape(&mut self, color: Rgba, build: impl FnOnce(&mut PathBuilder)) {
        let mut builder = PathBuilder::new();
        build(&mut builder);
        if let Some(path) = builder.finish() {
            let transform = self.transform();
            self.pixmap
                .fill_path(&path, &paint(color), FillRule::Winding, transform, None);
        }
    }

    /// Draws `text` with its top-left corner at logical (`x`, `y`), clipped to logical `clip`.
    /// Baselines fall on whole buffer pixels, so text stays sharp vertically; horizontally,
    /// glyphs keep their subpixel positions.
    fn text(
        &mut self,
        fonts: &mut cosmic_text::FontSystem,
        glyphs: &mut SwashCache,
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
        let data = self.pixmap.data_mut();
        for run in text.runs() {
            let baseline = (top + run.line_y * self.scale).round() as i32;
            for glyph in run.glyphs {
                let physical = glyph.physical((left, 0.0), self.scale);
                let Some(image) = glyphs.get_image(fonts, physical.cache_key) else {
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

fn rect_f(rect: WindowRect) -> [f32; 4] {
    [
        rect.x as f32,
        rect.y as f32,
        rect.width as f32,
        rect.height as f32,
    ]
}

fn intersects(a: WindowRect, b: WindowRect) -> bool {
    !crate::layout::intersect(a, b).size().is_empty()
}

fn tone(colors: &Colors, tone: Tone) -> Rgba {
    match tone {
        Tone::Text => colors.text,
        Tone::Muted => colors.text_muted,
        Tone::Dim => colors.text_dim,
        Tone::Error => colors.error,
    }
}

/// Paints everything of `chrome` inside `metrics.rect()` onto `pixmap`, which is the target's
/// buffer.
fn draw(chrome: &mut Chrome, metrics: &Metrics, glyphs: &mut SwashCache, pixmap: PixmapMut<'_>) {
    let target = metrics.rect();
    let buffer = metrics.buffer();
    let mut canvas = Canvas {
        pixmap,
        origin: (target.x as f32, target.y as f32),
        ratio: (
            buffer.width as f32 / target.width as f32,
            buffer.height as f32 / target.height as f32,
        ),
        scale: metrics.scale().factor() as f32,
    };
    let colors = *chrome.colors();
    let background = colors.bg;
    canvas.pixmap.fill(tiny_skia::Color::from_rgba8(
        background.r,
        background.g,
        background.b,
        255,
    ));
    let layout = chrome.layout().clone();
    if intersects(target, layout.tab_strip) {
        draw_tabs(&mut canvas, chrome, glyphs, &colors);
    }
    if intersects(target, layout.toolbar) {
        draw_toolbar(&mut canvas, chrome, glyphs, &colors);
    }
    if intersects(target, layout.status) {
        draw_status(&mut canvas, chrome, glyphs, &colors);
    }
    if intersects(target, layout.page) && chrome.draws_page() {
        draw_page(&mut canvas, chrome, glyphs, &colors);
    }
}

fn draw_tabs(canvas: &mut Canvas, chrome: &mut Chrome, glyphs: &mut SwashCache, colors: &Colors) {
    let layout = chrome.layout().clone();
    let strip = rect_f(layout.tab_strip);
    canvas.fill(
        [strip[0], strip[1] + strip[3] - 1.0, strip[2], 1.0],
        colors.border,
    );
    let hover = chrome.state().hover;
    for (tab, place) in chrome.snapshot().tabs.clone().iter().zip(&layout.tabs) {
        if place.rect.size().is_empty() {
            continue;
        }
        let rect = rect_f(place.rect);
        if tab.active {
            canvas.fill_rounded(rect, 6.0, colors.tab);
        }
        let mut left = rect[0] + 12.0;
        let center_y = rect[1] + rect[3] / 2.0;
        if tab.loading {
            let color = colors.text_muted;
            canvas.shape(color, |path| path.push_circle(left + 3.0, center_y, 3.0));
            left += 12.0;
        }
        let right = match place.close {
            Some(close) => {
                let [x, y, w, h] = rect_f(close);
                let color = match hover == Some(Target::CloseTab(tab.id)) {
                    true => colors.text,
                    false => colors.text_dim,
                };
                cross(canvas, (x + w / 2.0, y + h / 2.0), 3.5, color);
                x - 4.0
            }
            None => rect[0] + rect[2] - 12.0,
        };
        let style = TextStyle::SM;
        let text = chrome
            .fonts()
            .fitted(&tab.display_title(), style, (right - left).max(0.0));
        let top = center_y - f32::from(style.line_height) / 2.0;
        let color = if tab.active {
            colors.text
        } else {
            colors.text_muted
        };
        let clip = [left, rect[1], (right - left).max(0.0), rect[3]];
        let fonts = chrome.fonts().font_system();
        canvas.text(fonts, glyphs, &text, (left, top), color, clip);
    }
    let [x, y, w, h] = rect_f(layout.new_tab);
    if w > 0.0 {
        let color = match hover == Some(Target::NewTab) {
            true => colors.text,
            false => colors.text_muted,
        };
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        canvas.stroke(1.5, color, |path| {
            path.move_to(cx - 5.0, cy);
            path.line_to(cx + 5.0, cy);
            path.move_to(cx, cy - 5.0);
            path.line_to(cx, cy + 5.0);
        });
    }
}

/// An × of two strokes, `size` logical pixels from the centre to each end.
fn cross(canvas: &mut Canvas, (cx, cy): (f32, f32), size: f32, color: Rgba) {
    canvas.stroke(1.5, color, |path| {
        path.move_to(cx - size, cy - size);
        path.line_to(cx + size, cy + size);
        path.move_to(cx + size, cy - size);
        path.line_to(cx - size, cy + size);
    });
}

fn draw_toolbar(
    canvas: &mut Canvas,
    chrome: &mut Chrome,
    glyphs: &mut SwashCache,
    colors: &Colors,
) {
    let layout = chrome.layout().clone();
    let bar = rect_f(layout.toolbar);
    canvas.fill([bar[0], bar[1] + bar[3] - 1.0, bar[2], 1.0], colors.border);
    let snapshot = chrome.snapshot();
    let loading = snapshot.active().is_some_and(|tab| tab.loading);
    let buttons = [
        (layout.back, Target::Back, snapshot.can_go_back),
        (layout.forward, Target::Forward, snapshot.can_go_forward),
        (layout.reload, Target::ReloadOrStop, true),
    ];
    let hover = chrome.state().hover;
    for (place, target, enabled) in buttons {
        if place.size().is_empty() {
            continue;
        }
        let [x, y, w, h] = rect_f(place);
        if enabled && hover == Some(target) {
            canvas.fill_rounded([x, y, w, h], 6.0, colors.hover);
        }
        let color = if enabled {
            colors.text_muted
        } else {
            colors.text_dim
        };
        let (cx, cy) = (x + w / 2.0, y + h / 2.0);
        match target {
            // Filled triangles, as the old toolbar's ◀ and ▶.
            Target::Back => canvas.shape(color, |path| {
                path.move_to(cx + 3.5, cy - 5.0);
                path.line_to(cx + 3.5, cy + 5.0);
                path.line_to(cx - 4.5, cy);
                path.close();
            }),
            Target::Forward => canvas.shape(color, |path| {
                path.move_to(cx - 3.5, cy - 5.0);
                path.line_to(cx - 3.5, cy + 5.0);
                path.line_to(cx + 4.5, cy);
                path.close();
            }),
            _ if loading => cross(canvas, (cx, cy), 4.0, color),
            _ => reload_icon(canvas, (cx, cy), color),
        }
    }
    draw_address(canvas, chrome, glyphs, colors);
}

/// ↻: three quarters of a circle, clockwise from the right to the top, with an arrowhead.
fn reload_icon(canvas: &mut Canvas, (cx, cy): (f32, f32), color: Rgba) {
    let radius = 5.5;
    // Control-point distance for a quarter circle.
    let k = radius * 0.552_284_8;
    canvas.stroke(1.5, color, |path| {
        path.move_to(cx + radius, cy);
        path.cubic_to(cx + radius, cy + k, cx + k, cy + radius, cx, cy + radius);
        path.cubic_to(cx - k, cy + radius, cx - radius, cy + k, cx - radius, cy);
        path.cubic_to(cx - radius, cy - k, cx - k, cy - radius, cx, cy - radius);
    });
    canvas.shape(color, |path| {
        path.move_to(cx + 3.5, cy - radius);
        path.line_to(cx - 0.5, cy - radius - 3.5);
        path.line_to(cx - 0.5, cy - radius + 3.5);
        path.close();
    });
}

fn draw_address(
    canvas: &mut Canvas,
    chrome: &mut Chrome,
    glyphs: &mut SwashCache,
    colors: &Colors,
) {
    let field = chrome.layout().address;
    if field.size().is_empty() {
        return;
    }
    let state = chrome.state();
    let focused = state.focused;
    let active = focused && state.window_active;
    let border = match (&state.command_error, focused) {
        (Some(_), _) => colors.error,
        (None, true) => colors.focus,
        (None, false) => colors.border,
    };
    canvas.bordered(rect_f(field), 6.0, colors.surface, border);

    let view = chrome.address_view();
    let clip = [
        view.clip.0,
        field.y as f32,
        view.clip.1 - view.clip.0,
        field.height as f32,
    ];
    let line = f32::from(TextStyle::SM.line_height);
    let clipped = |(a, b): (f32, f32)| {
        let (a, b) = (a.max(view.clip.0), b.min(view.clip.1));
        (a < b).then_some([a, view.y, b - a, line])
    };
    if active {
        if let Some(selection) = view.selection.and_then(clipped) {
            canvas.fill(selection, colors.selection);
        }
    }
    let fonts = chrome.fonts().font_system();
    canvas.text(
        fonts,
        glyphs,
        &view.shaped,
        (view.x, view.y),
        colors.text,
        clip,
    );
    if let Some(preedit) = view.preedit.and_then(clipped) {
        canvas.fill(
            [preedit[0], view.y + line - 2.0, preedit[2], 1.0],
            colors.text,
        );
    }
    let caret_shown = active && view.selection.is_none();
    if let Some(caret) = view.caret.filter(|_| caret_shown) {
        if caret >= view.clip.0 - 1.0 && caret <= view.clip.1 + 1.0 {
            canvas.fill([caret, view.y, CARET_WIDTH, line], colors.text);
        }
    }
}

fn draw_status(canvas: &mut Canvas, chrome: &mut Chrome, glyphs: &mut SwashCache, colors: &Colors) {
    let strip = rect_f(chrome.layout().status);
    canvas.fill([strip[0], strip[1], strip[2], 1.0], colors.border);
    let Some((line, shade)) = chrome.status_line() else {
        return;
    };
    let style = TextStyle::XS;
    let width = (strip[2] - 16.0).max(0.0);
    let text = chrome.fonts().fitted(&line, style, width);
    let top = strip[1] + (strip[3] - f32::from(style.line_height)) / 2.0;
    let clip = [strip[0] + 8.0, strip[1], width, strip[3]];
    let fonts = chrome.fonts().font_system();
    let color = tone(colors, shade);
    canvas.text(fonts, glyphs, &text, (strip[0] + 8.0, top), color, clip);
}

fn draw_page(canvas: &mut Canvas, chrome: &mut Chrome, glyphs: &mut SwashCache, colors: &Colors) {
    let Some(page) = chrome.page_layout() else {
        return;
    };
    canvas.bordered(page.card, 8.0, colors.surface, colors.border);
    let clip = rect_f(chrome.layout().page);
    let fonts = chrome.fonts().font_system();
    for text in &page.texts {
        let color = tone(colors, text.tone);
        canvas.text(fonts, glyphs, &text.shaped, (text.x, text.y), color, clip);
    }
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
