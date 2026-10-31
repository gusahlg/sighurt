//! The CPU rasterizer: paints a display list into a buffer the host lends for one frame.
//!
//! The buffer is wl_shm ARGB8888: premultiplied alpha, little-endian, so each pixel is B, G, R,
//! A in memory. tiny-skia paints premultiplied R, G, B, A; instead of converting the finished
//! page (a pass over every pixel, every frame), everything is painted with red and blue swapped
//! (colors, image pixels, glyph colors), which leaves exactly ARGB8888 in memory. Source-over
//! blending treats the channels alike, so the swap is exact.
//!
//! Clips are axis-aligned rectangles, painted as layers: what a clip holds is painted into a
//! pixmap the size of the clip and composited when the clip ends. That costs memory and time
//! in proportion to the clip, not to the page, and gives the same result as painting in place,
//! because source-over is associative. Clip edges are snapped to whole pixels.
//!
//! One frame does at most [`MAX_OVERDRAW`] pages' worth of work; ops beyond it are not painted,
//! so no list keeps the owner thread painting for long. An op costs the pixels it can cover
//! within the clip, and text also what shaping it costs; ops that can't reach the clip are
//! skipped, and so are glyphs outside it. The overlay (the browser's permission prompt) has a
//! budget of its own, so no list can hide it. Images are converted for painting when the list
//! is made, on the guest's worker ([`convert_images`]), not here.
//!
//! A large frame is painted in horizontal bands at once, straight into the lent buffer ([`paint`]),
//! a [`Frame`](sighurt_api::present::Frame).
//! Which ops fit the budget is decided first, once for the whole frame ([`select`]); each band
//! then paints the part of those ops that reaches its rows. So the same ops are painted
//! whatever the number of bands, and the bands together do the frame's work, not more. Shapes
//! are placed relative to each band, so an anti-aliased edge pixel may be shaded a little
//! differently than in one piece (`bands_paint_what_one_piece_does`).
//!
//! When the caller names a few changed rectangles ([`paint_regions`]), each one is painted on
//! this thread into a pixmap of that rectangle and copied over the buffer. The copy replaces
//! those pixels. The rest of the buffer is left as it was, which is only correct when it still
//! holds a frame the unchanged pixels match. The same ops are selected as for the whole frame,
//! so a rectangle shows what the full paint would have put there.
//!
//! Owns: turning ops into pixels. Must not own: what the ops are (see [`crate::draw`]) or when a
//! frame is painted (the host grants a frame permit).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use sighurt_api::geometry::BufferSize;
pub use sighurt_api::present::Frame;
use tiny_skia::{
    BlendMode, Color, FillRule, FilterQuality, Paint, PathBuilder, Pattern, Pixmap, PixmapMut,
    PixmapPaint, Rect, SpreadMode, Stroke, Transform,
};

use crate::capabilities::DecodedImage;
use crate::draw::{DisplayList, DrawOp};

/// Most work in one frame, in pages' worth of pixels, for the list and again for the overlay.
/// The example guests use 1.1 to 5.7 pages (gradient-demo the most); a frame that uses up the
/// budget takes about 16 times as long to paint as a page of translucent fill.
pub const MAX_OVERDRAW: f64 = 16.0;
/// What shaping text costs, in pixels of work: for each text op, and for each byte of it. A
/// byte of text took as long as 136 to 140 pixels of translucent fill on the machine this was
/// written on (`text_costs_what_shaping_it_does`).
const TEXT_OP_COST: f64 = 2_000.0;
const TEXT_BYTE_COST: f64 = 150.0;
/// Largest text painted, in buffer pixels: past it the glyphs of one line take hundreds of
/// megabytes, and such text is not painted.
const MAX_TEXT_PX: f32 = 2048.0;

/// What to paint: a guest's display list and an overlay on top of it (the permission prompt).
pub struct Scene<'a> {
    pub list: Option<&'a DisplayList>,
    /// Buffer pixels per canvas pixel, horizontally and vertically: the display scale times the
    /// page zoom.
    pub list_scale: (f32, f32),
    /// Drawn after the list, in logical pixels. It has no images.
    pub overlay: &'a [DrawOp],
    /// Buffer pixels per logical pixel.
    pub overlay_scale: (f32, f32),
}

/// `images` converted for painting, reusing the conversions of `previous`, the list before.
/// Runs where the list is made, on the guest's worker.
pub fn convert_images(
    images: &[Arc<DecodedImage>],
    previous: Option<&DisplayList>,
) -> Vec<Option<Arc<Pixmap>>> {
    let known: HashMap<*const DecodedImage, &Option<Arc<Pixmap>>> = previous
        .map(|list| {
            list.images
                .iter()
                .map(Arc::as_ptr)
                .zip(&list.pixmaps)
                .collect()
        })
        .unwrap_or_default();
    images
        .iter()
        .map(|image| match known.get(&Arc::as_ptr(image)) {
            Some(&converted) => converted.clone(),
            None => pixmap(image).map(Arc::new),
        })
        .collect()
}

/// `image` premultiplied, with red and blue swapped.
fn pixmap(image: &DecodedImage) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(image.width, image.height)?;
    if image.pixels.len() != pixmap.data().len() {
        return None;
    }
    let premul = |c: u8, a: u8| ((u32::from(c) * u32::from(a) + 127) / 255) as u8;
    let pixels = image.pixels.as_chunks::<4>().0;
    for (dst, &[r, g, b, a]) in pixmap
        .data_mut()
        .as_chunks_mut::<4>()
        .0
        .iter_mut()
        .zip(pixels)
    {
        *dst = [premul(b, a), premul(g, a), premul(r, a), a];
    }
    Some(pixmap)
}

/// Paints `scene` into `frame`, replacing what it held. Fails, painting nothing, for a buffer
/// whose rows are padded or that is smaller than its size says.
///
/// A large frame is painted in horizontal bands at once, one on the calling thread and the
/// others on threads of their own, each straight into its rows of the buffer. Which ops the
/// budget allows is decided first, for the whole frame; then every band paints the part of
/// those ops that reaches its rows. So the caller waits for the slowest band rather than for
/// the whole frame, and nothing is copied.
pub fn paint(frame: Frame<'_>, scene: &Scene<'_>) -> Result<(), String> {
    let count = bands(frame.size.width, frame.size.height);
    paint_in_bands(frame, scene, count)
}

/// [`paint`] in `count` bands.
fn paint_in_bands(frame: Frame<'_>, scene: &Scene<'_>, count: u32) -> Result<(), String> {
    let BufferSize { width, height } = frame.size;
    let len = frame.stride * height as usize;
    if frame.stride != 4 * width as usize || frame.pixels.len() < len || len == 0 {
        return Err(format!(
            "unsupported buffer: {width}x{height}, stride {}, {} bytes",
            frame.stride,
            frame.pixels.len()
        ));
    }
    let page = [0.0, 0.0, width as f32, height as f32];
    let budget = MAX_OVERDRAW * f64::from(width) * f64::from(height);
    let work = Work {
        scene,
        list: scene
            .list
            .map(|list| select(&list.ops, scene.list_scale, page, budget))
            .unwrap_or_default(),
        overlay: select(scene.overlay, scene.overlay_scale, page, budget),
        width,
    };
    let band_rows = height.div_ceil(count.clamp(1, height));
    // Each band waits in a slot of its own, so a band whose thread can't be started is still
    // there to paint on this one.
    let bands: Vec<(Mutex<Option<&mut [u8]>>, u32)> = frame.pixels[..len]
        .chunks_mut(frame.stride * band_rows as usize)
        .zip((0..).step_by(band_rows as usize))
        .map(|(rows, top)| (Mutex::new(Some(rows)), top))
        .collect();
    let paint = |(rows, top): &(Mutex<Option<&mut [u8]>>, u32)| {
        let rows = rows.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(rows) = rows {
            paint_band(rows, *top, &work);
        }
    };
    std::thread::scope(|scope| {
        for band in &bands[1..] {
            let spawned = std::thread::Builder::new()
                .name("sighurt-wasm-band".into())
                .spawn_scoped(scope, move || paint(band));
            if spawned.is_err() {
                paint(band);
            }
        }
        paint(&bands[0]);
    });
    Ok(())
}

/// Paints `scene` over `rects` of `frame` and leaves every other pixel as the buffer held it.
/// `rects` are buffer pixels. Fails, changing nothing it has not already copied, for a buffer
/// whose rows are padded or that is smaller than its size says.
pub fn paint_regions(
    frame: Frame<'_>,
    scene: &Scene<'_>,
    rects: &[sighurt_wayland::DamageRect],
) -> Result<(), String> {
    let BufferSize { width, height } = frame.size;
    let len = frame.stride * height as usize;
    if frame.stride != 4 * width as usize || frame.pixels.len() < len || len == 0 {
        return Err(format!(
            "unsupported buffer: {width}x{height}, stride {}, {} bytes",
            frame.stride,
            frame.pixels.len()
        ));
    }
    let page = [0.0, 0.0, width as f32, height as f32];
    let budget = MAX_OVERDRAW * f64::from(width) * f64::from(height);
    let work = Work {
        scene,
        list: scene
            .list
            .map(|list| select(&list.ops, scene.list_scale, page, budget))
            .unwrap_or_default(),
        overlay: select(scene.overlay, scene.overlay_scale, page, budget),
        width,
    };
    let replace = PixmapPaint {
        blend_mode: BlendMode::Source,
        ..PixmapPaint::default()
    };
    for rect in rects {
        let Some((x, y, rw, rh)) = rect_in_buffer(*rect, width, height) else {
            continue;
        };
        let mut pixmap = Pixmap::new(rw, rh).ok_or_else(|| format!("a {rw}x{rh} region"))?;
        paint_into(pixmap.data_mut(), (rw, rh), (x, y), &work);
        let mut dest = PixmapMut::from_bytes(&mut frame.pixels[..len], width, height)
            .ok_or_else(|| format!("a {width}x{height} buffer"))?;
        dest.draw_pixmap(x, y, pixmap.as_ref(), &replace, Transform::identity(), None);
    }
    Ok(())
}

/// `rect` clipped to the buffer: its origin and its size. `None` when nothing remains.
fn rect_in_buffer(
    rect: sighurt_wayland::DamageRect,
    width: u32,
    height: u32,
) -> Option<(i32, i32, u32, u32)> {
    if rect.width <= 0 || rect.height <= 0 {
        return None;
    }
    let (width, height) = (width as i32, height as i32);
    let x0 = rect.x.max(0);
    let y0 = rect.y.max(0);
    let x1 = rect.x.saturating_add(rect.width).min(width);
    let y1 = rect.y.saturating_add(rect.height).min(height);
    (x1 > x0 && y1 > y0).then_some((x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
}

/// Most threads that paint one frame, the caller's included.
const MAX_BANDS: u32 = 8;
/// Pixels a band is worth a thread for.
const MIN_BAND_PIXELS: u64 = 256 * 1024;

/// How many bands to paint a `width` × `height` frame in.
fn bands(width: u32, height: u32) -> u32 {
    static CORES: OnceLock<u32> = OnceLock::new();
    let cores = *CORES.get_or_init(|| {
        std::thread::available_parallelism().map_or(1, |n| n.get().min(MAX_BANDS as usize) as u32)
    });
    let worth = (u64::from(width) * u64::from(height) / MIN_BAND_PIXELS) as u32;
    worth.clamp(1, cores).min(height)
}

/// What every band of a frame paints.
struct Work<'a> {
    scene: &'a Scene<'a>,
    list: Selection,
    overlay: Selection,
    /// The buffer's width in pixels.
    width: u32,
}

/// The ops of a list that a frame paints, decided once for the whole frame.
#[derive(Default)]
struct Selection {
    /// Ops from here on are not painted: the first of them doesn't fit the budget.
    end: usize,
    /// The `PushClip`s before `end` that the budget had no room for, by index, ascending:
    /// nothing inside them shows.
    refused: Vec<usize>,
}

/// Which of `ops`, in units of `scale` buffer pixels, a frame of `page` paints within
/// `budget`: each op is charged what it costs within its clip over the whole frame, and each
/// clip its layer, up to the first op the budget has no room for. It paints nothing.
fn select(ops: &[DrawOp], scale: (f32, f32), page: Bounds, mut budget: f64) -> Selection {
    let mut clips: Vec<Bounds> = Vec::new();
    let mut refused = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let clip = clips.last().copied().unwrap_or(page);
        match *op {
            DrawOp::PushClip { x, y, w, h } => {
                let layer = snap(intersect(scaled([x, y, x + w, y + h], scale), clip));
                let cost = 2.0 * area(layer);
                if cost <= budget {
                    budget -= cost;
                    clips.push(layer);
                } else {
                    refused.push(i);
                    clips.push([0.0; 4]);
                }
            }
            DrawOp::PopClip => {
                clips.pop();
            }
            _ => match cost(op, scale, clip) {
                Some(cost) if cost > budget => return Selection { end: i, refused },
                Some(cost) => budget -= cost,
                None => {}
            },
        }
    }
    Selection {
        end: ops.len(),
        refused,
    }
}

/// Paints `work` into `rows`, the buffer's rows from `top` down.
fn paint_band(rows: &mut [u8], top: u32, work: &Work<'_>) {
    let width = work.width;
    let height = (rows.len() / (4 * width as usize)) as u32;
    paint_into(rows, (width, height), (0, top as i32), work);
}

/// Paints `work` into `pixels`, a pixmap of `size` whose top-left is `origin` in the buffer.
fn paint_into(pixels: &mut [u8], (width, height): (u32, u32), origin: (i32, i32), work: &Work<'_>) {
    let Some(mut page) = PixmapMut::from_bytes(pixels, width, height) else {
        return;
    };
    let scene = work.scene;
    // A list that starts by painting all of it opaque replaces what the pixmap held anyway.
    let covered = scene
        .list
        .and_then(|list| list.ops.first())
        .is_some_and(|op| matches!(op, DrawOp::Clear { color } if color & 0xff == 0xff));
    if !covered {
        page.fill(Color::TRANSPARENT);
    }
    let mut painter = Painter {
        page,
        origin,
        layers: Vec::new(),
    };
    if let Some(list) = scene.list {
        painter.ops(&list.ops, &work.list, scene.list_scale, &list.pixmaps);
    }
    painter.ops(scene.overlay, &work.overlay, scene.overlay_scale, &[]);
}

struct Painter<'a> {
    /// The pixmap this painter paints: a band, a damage rectangle, or the whole buffer.
    page: PixmapMut<'a>,
    /// Where that pixmap's top-left sits in the buffer. Everything else is in buffer pixels.
    origin: (i32, i32),
    /// Open clips, innermost last.
    layers: Vec<Layer>,
}

/// An open clip: what is painted inside it, and where that goes in the buffer.
struct Layer {
    /// `None` when the clip is empty: nothing inside it shows.
    pixmap: Option<Pixmap>,
    /// The layer's top-left corner in the buffer, in buffer pixels.
    x: i32,
    y: i32,
}

/// A rectangle in buffer pixels: left, top, right, bottom.
type Bounds = [f32; 4];

impl Painter<'_> {
    /// Paints the `selection` of `ops`, whose units are `scale` buffer pixels, that reaches
    /// these rows, and closes the clips they leave open.
    fn ops(
        &mut self,
        ops: &[DrawOp],
        selection: &Selection,
        scale: (f32, f32),
        images: &[Option<Arc<Pixmap>>],
    ) {
        let mut refused = selection.refused.iter().copied().peekable();
        for (i, op) in ops[..selection.end].iter().enumerate() {
            match *op {
                DrawOp::PushClip { x, y, w, h } => {
                    let refused = refused.next_if_eq(&i).is_some();
                    self.push_clip(scaled([x, y, x + w, y + h], scale), refused);
                }
                DrawOp::PopClip => self.pop_clip(),
                _ => {
                    if cost(op, scale, self.clip()).is_some() {
                        self.draw(op, scale, images);
                    }
                }
            }
        }
        while !self.layers.is_empty() {
            self.pop_clip();
        }
    }

    /// Where painting goes now: the innermost clip's pixmap or the page, with the offset from
    /// buffer pixels to that pixmap's. `None` inside an empty clip.
    fn target(&mut self) -> Option<(PixmapMut<'_>, (f32, f32))> {
        match self.layers.last_mut() {
            Some(layer) => {
                let offset = (-layer.x as f32, -layer.y as f32);
                Some((layer.pixmap.as_mut()?.as_mut(), offset))
            }
            None => {
                let (width, height) = (self.page.width(), self.page.height());
                let page = PixmapMut::from_bytes(self.page.data_mut(), width, height)?;
                Some((page, (-self.origin.0 as f32, -self.origin.1 as f32)))
            }
        }
    }

    /// The area painting can reach now, in buffer pixels.
    fn clip(&self) -> Bounds {
        match self.layers.last() {
            Some(Layer {
                pixmap: Some(pixmap),
                x,
                y,
            }) => {
                let (x, y) = (*x as f32, *y as f32);
                [x, y, x + pixmap.width() as f32, y + pixmap.height() as f32]
            }
            Some(_) => [0.0; 4],
            None => {
                let (x, y) = (self.origin.0 as f32, self.origin.1 as f32);
                let (width, height) = (self.page.width() as f32, self.page.height() as f32);
                [x, y, x + width, y + height]
            }
        }
    }

    /// Opens a clip of `bounds`; a `refused` one shows nothing.
    fn push_clip(&mut self, bounds: Bounds, refused: bool) {
        let [l, t, r, b] = snap(intersect(bounds, self.clip()));
        let pixmap = if refused {
            None
        } else {
            Pixmap::new((r - l) as u32, (b - t) as u32)
        };
        self.layers.push(Layer {
            pixmap,
            x: l as i32,
            y: t as i32,
        });
    }

    fn pop_clip(&mut self) {
        let Some(Layer {
            pixmap: Some(pixmap),
            x,
            y,
        }) = self.layers.pop()
        else {
            return;
        };
        if let Some((mut target, (dx, dy))) = self.target() {
            let (x, y) = (x + dx as i32, y + dy as i32);
            target.draw_pixmap(
                x,
                y,
                pixmap.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
        }
    }

    fn draw(&mut self, op: &DrawOp, (sx, sy): (f32, f32), images: &[Option<Arc<Pixmap>>]) {
        let Some((mut target, (dx, dy))) = self.target() else {
            return;
        };
        let transform = Transform::from_row(sx, 0.0, 0.0, sy, dx, dy);
        match *op {
            // Opaque: the same as filling, which is a plain store of the color.
            DrawOp::Clear { color } if color & 0xff == 0xff => {
                let [r, g, b, _] = color.to_be_bytes();
                target.fill(Color::from_rgba8(b, g, r, 0xff));
            }
            DrawOp::Clear { color } => {
                let (w, h) = (target.width() as f32, target.height() as f32);
                if let Some(rect) = Rect::from_xywh(0.0, 0.0, w, h) {
                    target.fill_rect(rect, &solid(color), Transform::identity(), None);
                }
            }
            DrawOp::Rect {
                x,
                y,
                w,
                h,
                radius,
                color,
            } => {
                let radius = radius.min(w / 2.0).min(h / 2.0).max(0.0);
                if radius == 0.0 {
                    if let Some(rect) = Rect::from_xywh(x, y, w, h) {
                        target.fill_rect(rect, &solid(color), transform, None);
                    }
                } else if let Some(path) = rounded_rect(x, y, w, h, radius) {
                    target.fill_path(&path, &solid(color), FillRule::Winding, transform, None);
                }
            }
            DrawOp::Path {
                ref points,
                width,
                color,
            } => {
                let fill = width == 0.0;
                if points.len() < if fill { 3 } else { 2 } || width < 0.0 {
                    return;
                }
                let mut path = PathBuilder::new();
                path.move_to(points[0].0, points[0].1);
                for &(x, y) in &points[1..] {
                    path.line_to(x, y);
                }
                if fill {
                    path.close();
                }
                let Some(path) = path.finish() else {
                    return;
                };
                if fill {
                    // GPUI, which painted these before, filled even-odd.
                    target.fill_path(&path, &solid(color), FillRule::EvenOdd, transform, None);
                } else {
                    let stroke = Stroke {
                        width,
                        ..Stroke::default()
                    };
                    target.stroke_path(&path, &solid(color), &stroke, transform, None);
                }
            }
            DrawOp::Text {
                x,
                y,
                size,
                color,
                ref family,
                weight,
                italic,
                align,
                ref text,
            } => {
                let (left, top, px) = (x * sx + dx, y * sy + dy, size * sy);
                let place = |[width, ascent, descent]: [f32; 3]| {
                    let left = match align {
                        1 => left - width / 2.0,
                        2 => left - width,
                        _ => left,
                    };
                    // Centred in a line box 1.2 times the font size, as GPUI laid it out.
                    (left, top + (px * 1.2 - ascent - descent) / 2.0 + ascent)
                };
                let color = color.to_be_bytes();
                let area = [0.0, 0.0, target.width() as f32, target.height() as f32];
                let line = crate::text::Line {
                    text,
                    size: px,
                    family,
                    weight,
                    italic,
                };
                crate::text::draw(line, place, area, |image, gx, gy| {
                    blend_glyph(&mut target, image, gx, gy, color);
                });
            }
            DrawOp::Image { id, x, y, w, h } => {
                let Some(Some(image)) = images.get(id as usize) else {
                    return;
                };
                let (Some(rect), true) = (Rect::from_xywh(x, y, w, h), w > 0.0 && h > 0.0) else {
                    return;
                };
                let (iw, ih) = (image.width() as f32, image.height() as f32);
                let placed = Transform::from_row(w / iw, 0.0, 0.0, h / ih, x, y);
                let paint = Paint {
                    shader: Pattern::new(
                        image.as_ref().as_ref(),
                        SpreadMode::Pad,
                        FilterQuality::Bilinear,
                        1.0,
                        placed,
                    ),
                    ..Paint::default()
                };
                target.fill_rect(rect, &paint, transform, None);
            }
            DrawOp::PushClip { .. } | DrawOp::PopClip => {}
        }
    }
}

/// `0xRRGGBBAA` as an anti-aliased solid paint, red and blue swapped for ARGB8888 memory.
fn solid(color: u32) -> Paint<'static> {
    let [r, g, b, a] = color.to_be_bytes();
    let mut paint = Paint::default();
    paint.set_color_rgba8(b, g, r, a);
    paint.anti_alias = true;
    paint
}

/// A rectangle with corners rounded by `r`, at most half its shorter side.
fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    // Control points of a cubic that approximates a quarter circle.
    let k = r * (1.0 - 0.552_284_8);
    let (right, bottom) = (x + w, y + h);
    let mut path = PathBuilder::new();
    path.move_to(x + r, y);
    path.line_to(right - r, y);
    path.cubic_to(right - k, y, right, y + k, right, y + r);
    path.line_to(right, bottom - r);
    path.cubic_to(right, bottom - k, right - k, bottom, right - r, bottom);
    path.line_to(x + r, bottom);
    path.cubic_to(x + k, bottom, x, bottom - k, x, bottom - r);
    path.line_to(x, y + r);
    path.cubic_to(x, y + k, x + k, y, x + r, y);
    path.close();
    path.finish()
}

/// Blends a glyph's coverage (or colour) image, top-left at (`x`, `y`), over `target` in
/// `color` (`[r, g, b, a]`, not premultiplied).
fn blend_glyph(
    target: &mut PixmapMut<'_>,
    image: &cosmic_text::SwashImage,
    x: i32,
    y: i32,
    [r, g, b, a]: [u8; 4],
) {
    use cosmic_text::SwashContent;
    let (tw, th) = (target.width() as i32, target.height() as i32);
    let (w, h) = (image.placement.width as i32, image.placement.height as i32);
    let mul = |c: u32, d: u32| (c * d + 127) / 255;
    let data = target.data_mut();
    for row in 0..h {
        let ty = y + row;
        if !(0..th).contains(&ty) {
            continue;
        }
        for col in 0..w {
            let tx = x + col;
            if !(0..tw).contains(&tx) {
                continue;
            }
            let i = (row * w + col) as usize;
            // The glyph's colour in memory order (blue first), and its alpha here.
            let (color, alpha) = match image.content {
                SwashContent::Mask => ([b, g, r], mul(u32::from(image.data[i]), u32::from(a))),
                SwashContent::Color => {
                    let p = &image.data[4 * i..4 * i + 4];
                    ([p[2], p[1], p[0]], mul(u32::from(p[3]), u32::from(a)))
                }
                SwashContent::SubpixelMask => continue,
            };
            let src = color.map(|c| mul(u32::from(c), alpha));
            let dst = &mut data[4 * (ty * tw + tx) as usize..][..4];
            let keep = 255 - alpha;
            for (d, s) in dst.iter_mut().zip(src.into_iter().chain([alpha])) {
                *d = (s + mul(u32::from(*d), keep)).min(255) as u8;
            }
        }
    }
}

/// `bounds` in units of `scale` buffer pixels, in buffer pixels.
fn scaled([l, t, r, b]: Bounds, (sx, sy): (f32, f32)) -> Bounds {
    [l * sx, t * sy, r * sx, b * sy]
}

fn intersect(a: Bounds, b: Bounds) -> Bounds {
    let (l, t) = (a[0].max(b[0]), a[1].max(b[1]));
    [l, t, a[2].min(b[2]).max(l), a[3].min(b[3]).max(t)]
}

/// `bounds` grown to whole pixels.
fn snap([l, t, r, b]: Bounds) -> Bounds {
    [l.floor(), t.floor(), r.ceil(), b.ceil()]
}

/// The pixels in `bounds`.
fn area([l, t, r, b]: Bounds) -> f64 {
    f64::from((r - l).max(0.0)) * f64::from((b - t).max(0.0))
}

/// What painting `op`, in units of `(sx, sy)` buffer pixels, costs: the pixels it can cover
/// within `clip`, and for text what
/// shaping it costs too. `None` when it can't reach the clip, so painting it would change
/// nothing.
fn cost(op: &DrawOp, (sx, sy): (f32, f32), clip: Bounds) -> Option<f64> {
    let within = |bounds: Bounds| area(intersect(scaled(bounds, (sx, sy)), clip));
    let reached = |pixels: f64| (pixels > 0.0).then_some(pixels);
    match *op {
        DrawOp::Clear { .. } => reached(area(clip)),
        DrawOp::Rect { x, y, w, h, .. } | DrawOp::Image { x, y, w, h, .. } => {
            reached(within([x, y, x + w, y + h]))
        }
        DrawOp::Path {
            ref points, width, ..
        } => {
            let (mut l, mut t, mut r, mut b) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for &(x, y) in points {
                (l, t, r, b) = (l.min(x), t.min(y), r.max(x), b.max(y));
            }
            let half = width / 2.0;
            reached(within([l - half, t - half, r + half, b + half]))
        }
        DrawOp::Text {
            x,
            y,
            size,
            ref text,
            ..
        } => {
            if size * sy > MAX_TEXT_PX {
                return None;
            }
            // Glyphs are about a font size wide (and a character takes at least a byte);
            // the text may sit left or right of `x`. Where they may reach is wider: some
            // glyphs are wider than that, and marks go above and below the line.
            let wide = size * text.len() as f32;
            let reach = [x - 2.0 * wide, y - size, x + 2.0 * wide, y + 2.2 * size];
            reached(within(reach))?;
            let covered = within([x - wide, y, x + wide, y + size * 1.2]);
            Some(covered + TEXT_OP_COST + TEXT_BYTE_COST * text.len() as f64)
        }
        DrawOp::PushClip { .. } | DrawOp::PopClip => Some(0.0),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// Paints `ops` into a `width` × `height` buffer at `scale` and returns it.
    fn paint_ops(ops: Vec<DrawOp>, width: u32, height: u32, scale: f32) -> Vec<u8> {
        let mut pixels = vec![0xaa; (4 * width * height) as usize];
        let list = DisplayList {
            size: (width, height),
            ops,
            images: Vec::new(),
            pixmaps: Vec::new(),
        };
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize { width, height },
            stride: 4 * width as usize,
        };
        let scene = Scene {
            list: Some(&list),
            list_scale: (scale, scale),
            overlay: &[],
            overlay_scale: (1.0, 1.0),
        };
        paint(frame, &scene).unwrap();
        pixels
    }

    fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = 4 * (y * width + x) as usize;
        pixels[i..i + 4].try_into().unwrap()
    }

    fn rect(x: f32, y: f32, w: f32, h: f32, color: u32) -> DrawOp {
        DrawOp::Rect {
            x,
            y,
            w,
            h,
            radius: 0.0,
            color,
        }
    }

    #[test]
    fn pixels_are_premultiplied_argb8888() {
        let pixels = paint_ops(vec![rect(0.0, 0.0, 2.0, 2.0, 0xff000080)], 4, 4, 1.0);
        // Half-transparent red: B, G, R, A in memory, red premultiplied.
        assert_eq!(pixel(&pixels, 4, 0, 0), [0, 0, 128, 128]);
        // The rest was cleared, not left as the buffer held it.
        assert_eq!(pixel(&pixels, 4, 3, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn the_scale_maps_canvas_pixels_to_buffer_pixels() {
        let pixels = paint_ops(vec![rect(1.0, 1.0, 1.0, 1.0, 0x00ff00ff)], 8, 8, 2.0);
        assert_eq!(pixel(&pixels, 8, 1, 1), [0; 4]);
        assert_eq!(pixel(&pixels, 8, 2, 2), [0, 255, 0, 255]);
        assert_eq!(pixel(&pixels, 8, 3, 3), [0, 255, 0, 255]);
        assert_eq!(pixel(&pixels, 8, 4, 4), [0; 4]);
    }

    /// Painting the changed rectangles of frame B over a copy of frame A is frame B.
    #[test]
    fn a_partial_repaint_matches_the_full_frame() {
        let clear = DrawOp::Clear { color: 0x203040ff };
        let before = vec![clear.clone(), rect(10.0, 10.0, 16.0, 12.0, 0xff0000ff)];
        let after = vec![clear, rect(40.0, 28.0, 16.0, 12.0, 0x00ff00ff)];
        let (width, height) = (80, 60);
        let full_before = paint_ops(before.clone(), width, height, 1.0);
        let full_after = paint_ops(after.clone(), width, height, 1.0);
        let region = crate::damage::diff(
            Some(&crate::damage::Snap {
                ops: &before,
                images: &[],
                scale: (1.0, 1.0),
                overlay: &[],
                overlay_scale: (1.0, 1.0),
                buffer: (width, height),
                canvas: (width, height),
            }),
            &crate::damage::Snap {
                ops: &after,
                images: &[],
                scale: (1.0, 1.0),
                overlay: &[],
                overlay_scale: (1.0, 1.0),
                buffer: (width, height),
                canvas: (width, height),
            },
        );
        let rects = match region {
            crate::damage::Region::Rects(rects) => rects,
            crate::damage::Region::Full => panic!("the moved square should not be the whole frame"),
        };
        let mut pixels = full_before;
        let list = DisplayList {
            size: (width, height),
            ops: after,
            images: Vec::new(),
            pixmaps: Vec::new(),
        };
        let scene = Scene {
            list: Some(&list),
            list_scale: (1.0, 1.0),
            overlay: &[],
            overlay_scale: (1.0, 1.0),
        };
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize { width, height },
            stride: 4 * width as usize,
        };
        paint_regions(frame, &scene, &rects).unwrap();
        assert_eq!(pixels, full_after);
    }

    #[test]
    fn clips_confine_what_they_hold() {
        let ops = vec![
            DrawOp::PushClip {
                x: 2.0,
                y: 2.0,
                w: 2.0,
                h: 2.0,
            },
            DrawOp::Clear { color: 0x0000ffff },
            DrawOp::PopClip,
            rect(0.0, 0.0, 1.0, 1.0, 0xffffffff),
        ];
        let pixels = paint_ops(ops, 8, 8, 1.0);
        assert_eq!(pixel(&pixels, 8, 2, 2), [255, 0, 0, 255]);
        assert_eq!(pixel(&pixels, 8, 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&pixels, 8, 4, 4), [0; 4]);
        assert_eq!(pixel(&pixels, 8, 0, 0), [255; 4]);
        // A clip the list leaves open is closed at its end.
        let pixels = paint_ops(
            vec![
                DrawOp::PushClip {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                DrawOp::Clear { color: 0xff },
            ],
            4,
            4,
            1.0,
        );
        assert_eq!(pixel(&pixels, 4, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&pixels, 4, 1, 1), [0; 4]);
    }

    /// A frame painted in bands is the frame painted in one piece: clips, text, shapes and
    /// fills that cross the bands' edges included.
    #[test]
    fn bands_paint_what_one_piece_does() {
        let (width, height) = (1024, 1024);
        let cores = std::thread::available_parallelism().unwrap().get();
        assert!(bands(width, height) > 1 || cores == 1);
        let text = |y: f32| DrawOp::Text {
            x: 10.0,
            y,
            size: 40.0,
            color: 0xffffffcc,
            family: String::new(),
            weight: 400,
            italic: true,
            align: 0,
            text: "Across the bands".into(),
        };
        let ops = vec![
            DrawOp::Clear { color: 0x203040ff },
            DrawOp::PushClip {
                x: 20.5,
                y: 100.25,
                w: 300.0,
                h: 250.0,
            },
            rect(0.0, 0.0, 512.0, 512.0, 0xff800080),
            text(110.0),
            DrawOp::PopClip,
            DrawOp::Rect {
                x: 100.0,
                y: 200.0,
                w: 200.0,
                h: 200.0,
                radius: 100.0,
                color: 0x00ff0099,
            },
            DrawOp::Path {
                points: vec![(0.0, 0.0), (500.0, 511.0), (20.0, 400.0)],
                width: 3.5,
                color: 0xffffffff,
            },
            text(248.0),
        ];
        let banded = paint_ops(ops.clone(), width, height, 2.0);
        let list = DisplayList {
            size: (width, height),
            ops,
            images: Vec::new(),
            pixmaps: Vec::new(),
        };
        let scene = Scene {
            list: Some(&list),
            list_scale: (2.0, 2.0),
            overlay: &[],
            overlay_scale: (1.0, 1.0),
        };
        let whole = paint_counted(&scene, width, height, 1);
        // Shapes are placed relative to each band, so rounding can shade an anti-aliased edge
        // pixel a little differently; nothing else may differ.
        let differ: Vec<u8> = banded
            .as_chunks::<4>()
            .0
            .iter()
            .zip(whole.as_chunks::<4>().0)
            .filter(|(a, b)| a != b)
            .map(|(a, b)| (0..4).map(|i| a[i].abs_diff(b[i])).max().unwrap())
            .collect();
        let most = differ.iter().copied().max().unwrap_or(0);
        eprintln!("{} pixels differ, by at most {most}", differ.len());
        assert!(differ.len() < 1000, "{} pixels differ", differ.len());
        assert!(most <= 40, "a pixel differs by {most}");
    }

    /// Paints `scene` into a `width` × `height` buffer in `count` bands and returns it.
    fn paint_counted(scene: &Scene<'_>, width: u32, height: u32, count: u32) -> Vec<u8> {
        let mut pixels = vec![0xaa; (4 * width * height) as usize];
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize { width, height },
            stride: 4 * width as usize,
        };
        paint_in_bands(frame, scene, count).unwrap();
        pixels
    }

    /// The budget is the frame's, not each band's: a list whose work sits in a few bands is
    /// painted as far as it is in one piece, whatever the number of bands.
    #[test]
    fn the_budget_picks_the_same_ops_for_any_number_of_bands() {
        let (width, height) = (1024, 1024);
        let list = |rects: usize| {
            let mut ops = vec![DrawOp::Clear { color: 0x000000ff }];
            // Full-width strips over the top 100 rows, each in its own shade of blue.
            ops.extend(
                (0..rects).map(|i| rect(0.0, 0.0, 1024.0, 100.0, (i % 256) as u32 * 256 + 0xff)),
            );
            // And one at the bottom, after them.
            ops.push(rect(0.0, 900.0, 1024.0, 100.0, 0xffffffff));
            DisplayList {
                size: (width, height),
                ops,
                images: Vec::new(),
                pixmaps: Vec::new(),
            }
        };
        let scene = |list| Scene {
            list: Some(list),
            list_scale: (1.0, 1.0),
            overlay: &[],
            overlay_scale: (1.0, 1.0),
        };
        // 60 strips are 6 M pixels of work, well within the frame's 16.8 M, but more than an
        // eighth of it: all are painted, in one band or in eight.
        let fits = list(60);
        let one = paint_counted(&scene(&fits), width, height, 1);
        assert_eq!(pixel(&one, width, 0, 0), [59, 0, 0, 255]);
        assert_eq!(pixel(&one, width, 0, 950), [255; 4]);
        assert!(one == paint_counted(&scene(&fits), width, height, 8));
        // 300 strips are more than the frame's budget: after the page's clear, 153 fit, and
        // painting stops there (the strip at the bottom is not painted either).
        let over = list(300);
        let one = paint_counted(&scene(&over), width, height, 1);
        assert_eq!(pixel(&one, width, 0, 0), [152, 0, 0, 255]);
        assert_eq!(pixel(&one, width, 0, 950), [0, 0, 0, 255]);
        for count in [2, 3, 8] {
            assert!(
                one == paint_counted(&scene(&over), width, height, count),
                "{count} bands"
            );
        }
    }

    #[test]
    fn a_frame_does_bounded_work() {
        // Far more full-page fills than the budget allows: the last ones are not painted.
        let mut ops = vec![rect(0.0, 0.0, 4.0, 4.0, 0x000000ff); 2 * MAX_OVERDRAW as usize];
        ops.push(rect(0.0, 0.0, 4.0, 4.0, 0xffffffff));
        let pixels = paint_ops(ops, 4, 4, 1.0);
        assert_eq!(pixel(&pixels, 4, 0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn text_is_painted_in_its_color() {
        let text = DrawOp::Text {
            x: 2.0,
            y: 2.0,
            size: 20.0,
            color: 0xff0000ff,
            family: String::new(),
            weight: 700,
            italic: false,
            align: 0,
            text: "Hello".into(),
        };
        let pixels = paint_ops(vec![text], 80, 30, 1.0);
        let painted: Vec<_> = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0)
            .collect();
        assert!(painted.len() > 50, "{} pixels", painted.len());
        // Red only, premultiplied: no blue or green anywhere.
        assert!(painted
            .iter()
            .all(|p| p[0] == 0 && p[1] == 0 && p[2] == p[3]));
    }

    #[test]
    fn images_are_scaled_into_their_rectangle() {
        let image = Arc::new(DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![10, 20, 30, 255],
        });
        let list = DisplayList {
            size: (4, 4),
            ops: vec![DrawOp::Image {
                id: 0,
                x: 1.0,
                y: 1.0,
                w: 2.0,
                h: 2.0,
            }],
            pixmaps: convert_images(std::slice::from_ref(&image), None),
            images: vec![image],
        };
        let mut pixels = vec![0; 64];
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize {
                width: 4,
                height: 4,
            },
            stride: 16,
        };
        let scene = Scene {
            list: Some(&list),
            list_scale: (1.0, 1.0),
            overlay: &[],
            overlay_scale: (1.0, 1.0),
        };
        paint(frame, &scene).unwrap();
        assert_eq!(pixel(&pixels, 4, 1, 1), [30, 20, 10, 255]);
        assert_eq!(pixel(&pixels, 4, 2, 2), [30, 20, 10, 255]);
        assert_eq!(pixel(&pixels, 4, 0, 0), [0; 4]);
    }

    #[test]
    fn padded_rows_are_refused() {
        let mut pixels = vec![0; 64];
        let frame = Frame {
            pixels: &mut pixels,
            size: BufferSize {
                width: 3,
                height: 4,
            },
            stride: 16,
        };
        let scene = Scene {
            list: None,
            list_scale: (1.0, 1.0),
            overlay: &[],
            overlay_scale: (1.0, 1.0),
        };
        assert!(paint(frame, &scene).is_err());
    }

    /// Off-screen text is neither shaped nor charged, so a list full of it paints in no time;
    /// on-screen text is charged for its shaping, so a list full of it paints in bounded time.
    #[test]
    fn a_frame_of_text_does_bounded_work() {
        let text = |y: f32| DrawOp::Text {
            x: 0.0,
            y,
            size: 12.0,
            color: 0xffffffff,
            family: String::new(),
            weight: 400,
            italic: false,
            align: 0,
            text: "Lorem ipsum dolor sit amet. ".repeat(585),
        };
        let paint_list = |ops: Vec<DrawOp>| {
            let started = std::time::Instant::now();
            let pixels = paint_ops(ops, 800, 600, 1.0);
            (started.elapsed(), pixels)
        };
        let mut ops = vec![text(-1e5); 4000];
        ops.push(rect(0.0, 0.0, 1.0, 1.0, 0xff0000ff));
        let (took, pixels) = paint_list(ops);
        assert_eq!(
            pixel(&pixels, 800, 0, 0),
            [0, 0, 255, 255],
            "painted to the end"
        );
        assert!(
            took < Duration::from_millis(500),
            "off-screen text took {took:?}"
        );
        // On screen: the budget runs out after a few ops, and the rest isn't shaped.
        let (took, _) = paint_list(vec![text(10.0); 4000]);
        eprintln!("4000 on-screen lines of 16 KiB: {took:?}");
        assert!(
            took < Duration::from_secs(2),
            "on-screen text took {took:?}"
        );
    }

    /// Measures what filling pixels and shaping text cost here, for `TEXT_BYTE_COST`:
    /// `cargo test -p sighurt-engine-wasm text_costs -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement"]
    fn text_costs_what_shaping_it_does() {
        let time = |ops: Vec<DrawOp>, n: u32| {
            let _ = paint_ops(ops.clone(), 1920, 1080, 1.0);
            let started = std::time::Instant::now();
            for _ in 0..n {
                let _ = paint_ops(ops.clone(), 1920, 1080, 1.0);
            }
            started.elapsed() / n
        };
        let empty = time(vec![], 20);
        let fills = time(vec![rect(0.0, 0.0, 1920.0, 1080.0, 0xff000080); 20], 5);
        let per_pixel = (fills - empty).as_secs_f64() / (20.0 * 1920.0 * 1080.0);
        let line = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(280);
        let texts: Vec<_> = (0..16)
            .map(|i| DrawOp::Text {
                x: 0.0,
                y: i as f32,
                size: 1.0 + i as f32 * 0.01,
                color: 0xffffffff,
                family: String::new(),
                weight: 400,
                italic: false,
                align: 0,
                text: line.clone(),
            })
            .collect();
        let full = time(vec![rect(0.0, 0.0, 1920.0, 1080.0, 0xff000080); 200], 2);
        eprintln!("a frame that uses up the budget: {full:?}");
        let bytes = 16.0 * line.len() as f64;
        let shaping = time(texts, 3);
        let per_byte = (shaping - empty).as_secs_f64() / bytes;
        eprintln!(
            "a pixel of translucent fill: {:.2} ns; a byte of text: {:.1} ns = {:.0} pixels",
            per_pixel * 1e9,
            per_byte * 1e9,
            per_byte / per_pixel
        );
    }
}
