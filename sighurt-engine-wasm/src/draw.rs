//! The display list: what a guest's canvas looks like, as the rasterizer paints it.
//!
//! [`display_list`] turns a frame's canvas commands into [`DrawOp`]s with [`convert`], bounds
//! them with [`bound`] and keeps the result only when it differs from the previous frame's, so a
//! guest that redraws the same picture every frame asks for no new presentation. Coordinates
//! stay in canvas pixels: zoom and the display scale are the rasterizer's business.
//!
//! [`convert`] bakes the translation and opacity into coordinates and colors, turns clips into
//! [`DrawOp::PushClip`] / [`DrawOp::PopClip`] pairs, and builds the shapes the list has no op
//! for from the ones it has. [`bound`] applies the limits the pre-rewrite browser applied to
//! every draw list an engine sent it (`gpui-baseline:sighurt-core/src/page.rs`).
//!
//! Owns: the draw-op vocabulary, which is this engine's private type. Must not own: pixels.

use std::sync::Arc;

use tiny_skia::Pixmap;

use crate::capabilities::{
    clamp, CanvasState, DecodedImage, DrawCommand, GradientStop, Rgba, MAX_IMAGE_SIDE,
};
use crate::raster;

/// How far, in pixels, a flattened curve may stray from the true one.
const TOLERANCE: f32 = 0.25;
/// Most operations in one list, and most path points; the rest is cut.
pub const MAX_OPS: usize = 200_000;
pub const MAX_POINTS: usize = 1_000_000;
/// Largest coordinate or size in a list, in canvas pixels.
pub const MAX_COORD: f32 = 1e6;
/// Largest font size in a list. Glyphs are rasterized at this size times the zoom and scale.
pub const MAX_FONT_SIZE: f32 = 512.0;
/// Deepest nesting of clips; deeper clips are dropped, with what they clip still drawn under
/// the clip around them.
pub const MAX_CLIP_DEPTH: usize = 16;

/// One step of a display list. Coordinates are canvas pixels from the page's top-left corner;
/// colors are `0xRRGGBBAA`, not premultiplied.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawOp {
    /// Fills the whole page (within the current clip).
    Clear {
        color: u32,
    },
    /// A filled rectangle. `radius` rounds its corners; a square with `radius = w / 2` is a
    /// circle.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: u32,
    },
    /// A polyline stroked `width` wide, or a filled polygon when `width` is 0.
    Path {
        points: Vec<(f32, f32)>,
        width: f32,
        color: u32,
    },
    /// One line of text. The line box is `size * 1.2` tall with its top at `y`; `x` is its left
    /// edge, centre or right edge for `align` 0, 1 or 2. An empty `family` means the default
    /// font; `weight` is CSS-style (400 normal, 700 bold).
    Text {
        x: f32,
        y: f32,
        size: f32,
        color: u32,
        family: String,
        weight: u16,
        italic: bool,
        align: u8,
        text: String,
    },
    /// Image `id` of the list's images, scaled into the rectangle.
    Image {
        id: u32,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    /// Clips the following operations to the rectangle, intersected with the current clip,
    /// until the matching [`DrawOp::PopClip`].
    PushClip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    PopClip,
}

/// A frame of a guest: its ops, the images they draw, and the canvas size it was drawn for.
#[derive(Debug, PartialEq)]
pub struct DisplayList {
    /// The canvas size, in canvas pixels, the guest saw when it drew this.
    pub size: (u32, u32),
    pub ops: Vec<DrawOp>,
    /// [`DrawOp::Image`] ids index this.
    pub images: Vec<Arc<DecodedImage>>,
    /// `images` converted for painting ([`raster::convert_images`]); `None` for one that can't
    /// be painted.
    pub pixmaps: Vec<Option<Arc<Pixmap>>>,
}

/// The display list of `canvas`, ready to paint, or `None` when it is the same as `previous`.
pub fn display_list(canvas: &CanvasState, previous: Option<&DisplayList>) -> Option<DisplayList> {
    let mut ops = Vec::new();
    let images = canvas.images.len();
    convert(
        &canvas.commands,
        |index| (index < images).then_some(index as u32),
        &mut ops,
    );
    bound(&mut ops, &canvas.images);
    let size = (canvas.width, canvas.height);
    let unchanged = previous.is_some_and(|previous| {
        previous.size == size
            && previous.ops == ops
            && previous.images.len() == images
            && previous
                .images
                .iter()
                .zip(&canvas.images)
                .all(|(a, b)| Arc::ptr_eq(a, b))
    });
    (!unchanged).then(|| DisplayList {
        size,
        ops,
        images: canvas.images.clone(),
        pixmaps: raster::convert_images(&canvas.images, previous),
    })
}

/// The transform, opacity and clip depth that apply to a command.
#[derive(Clone, Copy)]
struct State {
    dx: f32,
    dy: f32,
    opacity: f32,
    clips: usize,
}

impl State {
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        (x + self.dx, y + self.dy)
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
        let (x, y) = self.point(x, y);
        [x, y, w, h]
    }

    fn color(&self, [r, g, b, a]: Rgba) -> u32 {
        let a = (f32::from(a) * self.opacity).round().clamp(0.0, 255.0) as u8;
        u32::from_be_bytes([r, g, b, a])
    }
}

/// Appends the draw ops for `commands` to `ops`. `image_id` maps canvas image indices to the
/// list's image ids.
///
/// Transforms apply only their translation, and clips stay axis-aligned rectangles.
pub fn convert(
    commands: &[DrawCommand],
    image_id: impl Fn(usize) -> Option<u32>,
    ops: &mut Vec<DrawOp>,
) {
    let mut state = State {
        dx: 0.0,
        dy: 0.0,
        opacity: 1.0,
        clips: 0,
    };
    let mut saved = Vec::new();
    for command in commands {
        match *command {
            DrawCommand::Save => saved.push(state),
            DrawCommand::Restore => {
                if let Some(prev) = saved.pop() {
                    ops.extend(std::iter::repeat_n(
                        DrawOp::PopClip,
                        state.clips - prev.clips,
                    ));
                    state = prev;
                }
            }
            DrawCommand::Transform { tx, ty, .. } => {
                state.dx += tx;
                state.dy += ty;
            }
            DrawCommand::Clip { x, y, w, h } => {
                let [x, y, w, h] = state.rect(x, y, w, h);
                ops.push(DrawOp::PushClip { x, y, w, h });
                state.clips += 1;
            }
            DrawCommand::Opacity { alpha } => state.opacity *= alpha,
            DrawCommand::Clear { color } => ops.push(DrawOp::Clear {
                color: state.color(color),
            }),
            DrawCommand::Rect { x, y, w, h, color } => {
                let [x, y, w, h] = state.rect(x, y, w, h);
                ops.push(DrawOp::Rect {
                    x,
                    y,
                    w,
                    h,
                    radius: 0.0,
                    color: state.color(color),
                });
            }
            DrawCommand::RoundedRect {
                x,
                y,
                w,
                h,
                radius,
                color,
            } => {
                let [x, y, w, h] = state.rect(x, y, w, h);
                ops.push(DrawOp::Rect {
                    x,
                    y,
                    w,
                    h,
                    radius: radius.min(w.min(h) / 2.0).max(0.0),
                    color: state.color(color),
                });
            }
            DrawCommand::Circle {
                cx,
                cy,
                radius,
                color,
            } => {
                let (cx, cy) = state.point(cx, cy);
                let radius = radius.abs();
                ops.push(DrawOp::Rect {
                    x: cx - radius,
                    y: cy - radius,
                    w: 2.0 * radius,
                    h: 2.0 * radius,
                    radius,
                    color: state.color(color),
                });
            }
            DrawCommand::Line {
                x1,
                y1,
                x2,
                y2,
                color,
                thickness,
            } => ops.push(DrawOp::Path {
                points: vec![state.point(x1, y1), state.point(x2, y2)],
                width: thickness,
                color: state.color(color),
            }),
            DrawCommand::Arc {
                cx,
                cy,
                radius,
                start_angle,
                end_angle,
                color,
                thickness,
            } => {
                let (cx, cy) = state.point(cx, cy);
                let sweep = end_angle - start_angle;
                // A chord spanning angle θ strays r·θ²/8 from its arc.
                let n = segments(sweep.abs() * (radius.abs() / (8.0 * TOLERANCE)).sqrt());
                let points = (0..=n)
                    .map(|i| {
                        let t = start_angle + sweep * i as f32 / n as f32;
                        (cx + radius * t.cos(), cy + radius * t.sin())
                    })
                    .collect();
                ops.push(DrawOp::Path {
                    points,
                    width: thickness,
                    color: state.color(color),
                });
            }
            DrawCommand::Bezier {
                x1,
                y1,
                cp1x,
                cp1y,
                cp2x,
                cp2y,
                x2,
                y2,
                color,
                thickness,
            } => {
                let p = [
                    state.point(x1, y1),
                    state.point(cp1x, cp1y),
                    state.point(cp2x, cp2y),
                    state.point(x2, y2),
                ];
                // Wang's formula: enough segments to keep the cubic within the tolerance.
                let bend = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
                    (a.0 - 2.0 * b.0 + c.0).hypot(a.1 - 2.0 * b.1 + c.1)
                };
                let m = bend(p[0], p[1], p[2]).max(bend(p[1], p[2], p[3]));
                let n = segments((0.75 * m / TOLERANCE).sqrt());
                let points = (0..=n)
                    .map(|i| {
                        let t = i as f32 / n as f32;
                        let u = 1.0 - t;
                        let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
                        let x = (0..4).map(|k| w[k] * p[k].0).sum();
                        let y = (0..4).map(|k| w[k] * p[k].1).sum();
                        (x, y)
                    })
                    .collect();
                ops.push(DrawOp::Path {
                    points,
                    width: thickness,
                    color: state.color(color),
                });
            }
            DrawCommand::Image {
                x,
                y,
                w,
                h,
                image_id: index,
            } => {
                if let Some(id) = image_id(index) {
                    let [x, y, w, h] = state.rect(x, y, w, h);
                    ops.push(DrawOp::Image { id, x, y, w, h });
                }
            }
            DrawCommand::Text {
                x,
                y,
                size,
                color,
                ref text,
            } => {
                let (x, y) = state.point(x, y);
                ops.push(DrawOp::Text {
                    x,
                    y,
                    size,
                    color: state.color(color),
                    family: String::new(),
                    weight: 400,
                    italic: false,
                    align: 0,
                    text: text.clone(),
                });
            }
            DrawCommand::TextEx {
                x,
                y,
                size,
                color,
                ref family,
                weight,
                style,
                align,
                ref text,
            } => {
                let (x, y) = state.point(x, y);
                ops.push(DrawOp::Text {
                    x,
                    y,
                    size,
                    color: state.color(color),
                    family: family.clone(),
                    weight,
                    // The protocol has no oblique; it looks like italic.
                    italic: style != 0,
                    align,
                    text: text.clone(),
                });
            }
            DrawCommand::Gradient {
                x,
                y,
                w,
                h,
                kind,
                ax,
                ay,
                bx,
                by,
                ref stops,
            } => {
                if stops.is_empty() {
                    continue;
                }
                let rect = state.rect(x, y, w, h);
                let color = |t: f32| state.color(sample(stops, t));
                let from = state.point(ax, ay);
                if kind == 1 {
                    radial_gradient(ops, rect, from, by.abs(), color);
                } else {
                    linear_gradient(ops, rect, from, state.point(bx, by), color);
                }
            }
        }
    }
    ops.extend(std::iter::repeat_n(DrawOp::PopClip, state.clips));
}

/// Segments for a flattened curve: `n` rounded up, within sane bounds.
fn segments(n: f32) -> usize {
    (n.ceil() as usize).clamp(1, 256)
}

/// Bands for a gradient `len` pixels long: about one per 4 pixels, at most 48, which keeps
/// neighbouring bands a shade or two apart.
fn bands(len: f32) -> usize {
    ((len / 4.0).ceil() as usize).clamp(2, 48)
}

/// A linear gradient from `a` to `b` over `rect`, as strips across the axis clipped to the
/// rectangle. The first and last strips run on to the rectangle's edges.
fn linear_gradient(
    ops: &mut Vec<DrawOp>,
    [x, y, w, h]: [f32; 4],
    a: (f32, f32),
    b: (f32, f32),
    color: impl Fn(f32) -> u32,
) {
    let d = (b.0 - a.0, b.1 - a.1);
    let len2 = d.0 * d.0 + d.1 * d.1;
    if len2 == 0.0 || len2.is_nan() {
        // No axis: the whole rectangle takes the last color.
        ops.push(DrawOp::Rect {
            x,
            y,
            w,
            h,
            radius: 0.0,
            color: color(1.0),
        });
        return;
    }
    // Position along the axis: 0 at `a`, 1 at `b`.
    let t = |p: (f32, f32)| ((p.0 - a.0) * d.0 + (p.1 - a.1) * d.1) / len2;
    let corners = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    let n = bands(len2.sqrt());
    let band_color = |i: usize| color((i as f32 + 0.5) / n as f32);
    for i in 0..n {
        let lo = if i == 0 {
            f32::NEG_INFINITY
        } else {
            i as f32 / n as f32
        };
        let hi = if i + 1 == n {
            f32::INFINITY
        } else if band_color(i + 1) & 0xff == 0xff {
            // Reach a pixel under the next strip, which covers it, so no seam shows where
            // the anti-aliased edges of the two meet.
            (i + 1) as f32 / n as f32 + 1.0 / len2.sqrt()
        } else {
            (i + 1) as f32 / n as f32
        };
        let strip = clip(&clip(&corners, |p| t(p) - lo), |p| hi - t(p));
        if strip.len() < 3 {
            continue;
        }
        let color = band_color(i);
        ops.push(if d.0 == 0.0 || d.1 == 0.0 {
            // Strips across a horizontal or vertical axis are rectangles.
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for &(px, py) in &strip {
                (x0, y0, x1, y1) = (x0.min(px), y0.min(py), x1.max(px), y1.max(py));
            }
            DrawOp::Rect {
                x: x0,
                y: y0,
                w: x1 - x0,
                h: y1 - y0,
                radius: 0.0,
                color,
            }
        } else {
            DrawOp::Path {
                points: strip,
                width: 0.0,
                color,
            }
        });
    }
}

/// A radial gradient around `center` over `rect`, as circles from the outside in, clipped to
/// the rectangle.
fn radial_gradient(
    ops: &mut Vec<DrawOp>,
    [x, y, w, h]: [f32; 4],
    (cx, cy): (f32, f32),
    radius: f32,
    color: impl Fn(f32) -> u32,
) {
    ops.push(DrawOp::PushClip { x, y, w, h });
    ops.push(DrawOp::Rect {
        x,
        y,
        w,
        h,
        radius: 0.0,
        color: color(1.0),
    });
    let n = bands(radius);
    for i in (1..=n).rev() {
        let r = radius * i as f32 / n as f32;
        ops.push(DrawOp::Rect {
            x: cx - r,
            y: cy - r,
            w: 2.0 * r,
            h: 2.0 * r,
            radius: r,
            color: color((i as f32 - 0.5) / n as f32),
        });
    }
    ops.push(DrawOp::PopClip);
}

/// The part of the convex polygon `poly` where the affine function `f` is not negative
/// (Sutherland–Hodgman).
fn clip(poly: &[(f32, f32)], f: impl Fn((f32, f32)) -> f32) -> Vec<(f32, f32)> {
    let mut out = Vec::with_capacity(poly.len() + 1);
    for (i, &p) in poly.iter().enumerate() {
        let q = poly[(i + 1) % poly.len()];
        let (fp, fq) = (f(p), f(q));
        if fp >= 0.0 {
            out.push(p);
        }
        if (fp >= 0.0) != (fq >= 0.0) {
            let s = fp / (fp - fq);
            out.push((p.0 + (q.0 - p.0) * s, p.1 + (q.1 - p.1) * s));
        }
    }
    out
}

/// The gradient's color at `t` (0–1), interpolated between the stops around it.
fn sample(stops: &[GradientStop], t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    let (lo, hi) = stops
        .windows(2)
        .map(|pair| (&pair[0], &pair[1]))
        .find(|(lo, hi)| t >= lo.offset && t <= hi.offset)
        .unwrap_or((&stops[0], &stops[stops.len() - 1]));
    let range = hi.offset - lo.offset;
    let frac = if range > 0.0 {
        (t - lo.offset) / range
    } else {
        0.0
    };
    let (lo, hi) = (lo.color, hi.color);
    std::array::from_fn(|i| {
        (f32::from(lo[i]) + (f32::from(hi[i]) - f32::from(lo[i])) * frac).round() as u8
    })
}

/// Cuts a list to [`MAX_OPS`] operations and [`MAX_POINTS`] path points, so that one list
/// cannot keep the rasterizer busy for arbitrarily long, and cuts text to
/// [`MAX_TEXT`](crate::capabilities::MAX_TEXT) bytes. Drops the operations with a coordinate
/// beyond [`MAX_COORD`] (NaN and infinities included), text that can't be painted, images that
/// aren't in `images` or have a side of 0 or over [`MAX_IMAGE_SIDE`], and clips nested deeper
/// than [`MAX_CLIP_DEPTH`], together with their [`DrawOp::PopClip`]. Runs once per list, on the
/// worker that built it, so painting it needs no checks.
pub fn bound(ops: &mut Vec<DrawOp>, images: &[Arc<DecodedImage>]) {
    let mut points = 0;
    let too_many = ops.iter().take(MAX_OPS).position(|op| {
        if let DrawOp::Path { points: p, .. } = op {
            points += p.len();
        }
        points > MAX_POINTS
    });
    ops.truncate(too_many.unwrap_or(MAX_OPS));
    // Whether each open clip was kept, innermost last, and how many were.
    let mut clips: Vec<bool> = Vec::new();
    let mut depth = 0;
    ops.retain_mut(|op| {
        if let DrawOp::Text { text, .. } = op {
            *text = clamp(std::mem::take(text));
        }
        match op {
            DrawOp::PushClip { .. } => {
                let keep = in_range(op) && depth < MAX_CLIP_DEPTH;
                depth += usize::from(keep);
                clips.push(keep);
                keep
            }
            DrawOp::PopClip => {
                let kept = clips.pop().unwrap_or(false);
                depth -= usize::from(kept);
                kept
            }
            DrawOp::Image { id, .. } => {
                let side = 1..=MAX_IMAGE_SIDE;
                let exists = images.get(*id as usize).is_some_and(|image| {
                    side.contains(&image.width) && side.contains(&image.height)
                });
                exists && in_range(op)
            }
            _ => in_range(op),
        }
    });
}

/// Whether every coordinate and size in `op` is a number within [`MAX_COORD`], and text has
/// something to show at a font size within [`MAX_FONT_SIZE`].
fn in_range(op: &DrawOp) -> bool {
    let ok = |values: &[f32]| values.iter().all(|v| v.abs() <= MAX_COORD);
    match op {
        DrawOp::Clear { .. } | DrawOp::PopClip => true,
        DrawOp::Rect {
            x, y, w, h, radius, ..
        } => ok(&[*x, *y, *w, *h, *radius]),
        DrawOp::Path { points, width, .. } => {
            ok(&[*width]) && points.iter().all(|&(x, y)| ok(&[x, y]))
        }
        DrawOp::Text {
            x, y, size, text, ..
        } => ok(&[*x, *y]) && *size > 0.0 && *size <= MAX_FONT_SIZE && !text.is_empty(),
        DrawOp::Image { x, y, w, h, .. } | DrawOp::PushClip { x, y, w, h } => ok(&[*x, *y, *w, *h]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(commands: &[DrawCommand]) -> Vec<DrawOp> {
        let mut ops = Vec::new();
        convert(commands, |i| Some(i as u32 + 100), &mut ops);
        ops
    }

    fn rect(x: f32, y: f32, w: f32, h: f32, a: u8) -> DrawCommand {
        DrawCommand::Rect {
            x,
            y,
            w,
            h,
            color: [255, 0, 0, a],
        }
    }

    fn stop(offset: f32, v: u8) -> GradientStop {
        GradientStop {
            offset,
            color: [v, v, v, 255],
        }
    }

    #[test]
    fn translation_and_opacity_are_baked_in() {
        let commands = [
            DrawCommand::Clear {
                color: [0x11, 0x22, 0x33, 0xff],
            },
            DrawCommand::Save,
            DrawCommand::Transform {
                a: 1.0,
                b: 0.0,
                c: 0.0,
                d: 1.0,
                tx: 10.0,
                ty: 5.0,
            },
            DrawCommand::Opacity { alpha: 0.5 },
            rect(0.0, 0.0, 10.0, 10.0, 255),
            DrawCommand::Restore,
            rect(1.0, 1.0, 2.0, 2.0, 255),
        ];
        let red = |a: u32| 0xff00_0000 | a;
        assert_eq!(
            ops(&commands),
            [
                DrawOp::Clear { color: 0x112233ff },
                DrawOp::Rect {
                    x: 10.0,
                    y: 5.0,
                    w: 10.0,
                    h: 10.0,
                    radius: 0.0,
                    color: red(128),
                },
                DrawOp::Rect {
                    x: 1.0,
                    y: 1.0,
                    w: 2.0,
                    h: 2.0,
                    radius: 0.0,
                    color: red(255),
                },
            ]
        );
    }

    #[test]
    fn clips_are_popped_on_restore_and_at_the_end() {
        let clip = |x| DrawCommand::Clip {
            x,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        };
        let commands = [
            DrawCommand::Save,
            clip(1.0),
            clip(2.0),
            rect(0.0, 0.0, 1.0, 1.0, 255),
            DrawCommand::Restore,
            clip(3.0),
        ];
        let kinds: Vec<_> = ops(&commands)
            .into_iter()
            .map(|op| match op {
                DrawOp::PushClip { x, .. } => format!("push {x}"),
                DrawOp::PopClip => "pop".into(),
                _ => "rect".into(),
            })
            .collect();
        assert_eq!(
            kinds,
            ["push 1", "push 2", "rect", "pop", "pop", "push 3", "pop"]
        );
    }

    #[test]
    fn circles_and_rounded_rects_are_rounded_rects() {
        let commands = [
            DrawCommand::Circle {
                cx: 10.0,
                cy: 20.0,
                radius: 5.0,
                color: [0, 0, 0, 255],
            },
            DrawCommand::RoundedRect {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 4.0,
                radius: 8.0,
                color: [0, 0, 0, 255],
            },
        ];
        assert_eq!(
            ops(&commands),
            [
                DrawOp::Rect {
                    x: 5.0,
                    y: 15.0,
                    w: 10.0,
                    h: 10.0,
                    radius: 5.0,
                    color: 0xff,
                },
                // The radius is clamped to half the shorter side.
                DrawOp::Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 4.0,
                    radius: 2.0,
                    color: 0xff,
                },
            ]
        );
    }

    #[test]
    fn arcs_are_flattened_within_the_tolerance() {
        let commands = [DrawCommand::Arc {
            cx: 50.0,
            cy: 50.0,
            radius: 100.0,
            start_angle: 0.0,
            end_angle: std::f32::consts::FRAC_PI_2,
            color: [0, 0, 0, 255],
            thickness: 2.0,
        }];
        let ops = ops(&commands);
        let [DrawOp::Path { points, width, .. }] = &ops[..] else {
            panic!("expected one path");
        };
        assert_eq!(*width, 2.0);
        let close = |(x, y): (f32, f32), (ex, ey): (f32, f32)| {
            (x - ex).abs() < 1e-3 && (y - ey).abs() < 1e-3
        };
        assert!(close(points[0], (150.0, 50.0)));
        assert!(close(*points.last().unwrap(), (50.0, 150.0)));
        for pair in points.windows(2) {
            let mid = ((pair[0].0 + pair[1].0) / 2.0, (pair[0].1 + pair[1].1) / 2.0);
            let sagitta = 100.0 - (mid.0 - 50.0).hypot(mid.1 - 50.0);
            assert!(sagitta <= TOLERANCE, "{sagitta}");
        }
    }

    #[test]
    fn linear_gradients_become_bands_along_the_axis() {
        let gradient = |bx, by| DrawCommand::Gradient {
            x: 10.0,
            y: 20.0,
            w: 100.0,
            h: 200.0,
            kind: 0,
            ax: 10.0,
            ay: 20.0,
            bx,
            by,
            stops: vec![stop(0.0, 0), stop(1.0, 255)],
        };

        // Vertical: rectangles covering the whole rectangle, from black to white.
        let bands = ops(&[gradient(10.0, 220.0)]);
        assert_eq!(bands.len(), 48);
        let mut last_bottom = 20.0;
        let mut last_shade = 0;
        for band in &bands {
            let DrawOp::Rect {
                x, y, w, h, color, ..
            } = *band
            else {
                panic!("expected a rectangle, got {band:?}");
            };
            assert_eq!((x, w), (10.0, 100.0));
            assert!(y <= last_bottom && y + h > last_bottom);
            last_bottom = y + h;
            let shade = color >> 24;
            assert!(shade >= last_shade);
            last_shade = shade;
        }
        assert_eq!(last_bottom, 220.0);
        assert!(last_shade > 250);

        // Diagonal: polygons inside the rectangle.
        for band in ops(&[gradient(110.0, 220.0)]) {
            let DrawOp::Path { points, width, .. } = band else {
                panic!("expected a polygon, got {band:?}");
            };
            assert_eq!(width, 0.0);
            assert!(points.len() >= 3);
            for (x, y) in points {
                assert!((10.0 - 1e-3..=110.0 + 1e-3).contains(&x), "{x}");
                assert!((20.0 - 1e-3..=220.0 + 1e-3).contains(&y), "{y}");
            }
        }
    }

    #[test]
    fn radial_gradients_are_clipped_circles() {
        let commands = [DrawCommand::Gradient {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 20.0,
            kind: 1,
            ax: 20.0,
            ay: 10.0,
            bx: 0.0,
            by: 20.0,
            stops: vec![stop(0.0, 255), stop(1.0, 0)],
        }];
        let ops = ops(&commands);
        assert!(matches!(
            ops[0],
            DrawOp::PushClip {
                w: 40.0,
                h: 20.0,
                ..
            }
        ));
        assert!(matches!(
            ops[1],
            DrawOp::Rect {
                w: 40.0,
                color: 0xff,
                ..
            }
        ));
        let DrawOp::Rect { x, w, radius, .. } = ops[2] else {
            panic!("expected the outer circle");
        };
        assert_eq!((x, w, radius), (0.0, 40.0, 20.0));
        let DrawOp::Rect { color, .. } = ops[ops.len() - 2] else {
            panic!("expected the inner circle");
        };
        assert!(color >> 24 > 200, "the centre is near white");
        assert_eq!(ops.last(), Some(&DrawOp::PopClip));
    }

    #[test]
    fn text_keeps_its_font_and_alignment() {
        let commands = [DrawCommand::TextEx {
            x: 100.0,
            y: 10.0,
            size: 12.0,
            color: [1, 2, 3, 255],
            family: "Georgia".into(),
            weight: 700,
            style: 2,
            align: 2,
            text: "Hi".into(),
        }];
        assert_eq!(
            ops(&commands),
            [DrawOp::Text {
                x: 100.0,
                y: 10.0,
                size: 12.0,
                color: 0x010203ff,
                family: "Georgia".into(),
                weight: 700,
                italic: true,
                align: 2,
                text: "Hi".into(),
            }]
        );
    }

    fn image(v: u8, side: u32) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            width: side,
            height: 1,
            pixels: vec![v; 4 * side as usize],
        })
    }

    #[test]
    fn a_list_is_made_only_when_the_frame_changed() {
        let mut canvas = CanvasState::default();
        canvas.push(rect(0.0, 0.0, 1.0, 1.0, 255));
        let first = display_list(&canvas, None).unwrap();
        assert_eq!(first.size, (800, 600));
        // Cleared and drawn again the same way: nothing new.
        canvas.clear();
        canvas.push(rect(0.0, 0.0, 1.0, 1.0, 255));
        assert_eq!(display_list(&canvas, Some(&first)), None);
        // Another size, another image or another op is new.
        canvas.width = 400;
        let resized = display_list(&canvas, Some(&first)).unwrap();
        assert_eq!(resized.ops, first.ops);
        canvas.push_image(image(1, 1), 0.0, 0.0, 1.0, 1.0);
        let with_image = display_list(&canvas, Some(&resized)).unwrap();
        canvas.clear();
        canvas.push(rect(0.0, 0.0, 1.0, 1.0, 255));
        canvas.push_image(image(1, 1), 0.0, 0.0, 1.0, 1.0);
        assert!(
            display_list(&canvas, Some(&with_image)).is_some(),
            "a new image"
        );
    }

    fn text(size: f32, text: &str) -> DrawOp {
        DrawOp::Text {
            x: 0.0,
            y: 0.0,
            size,
            color: 0xff,
            family: String::new(),
            weight: 400,
            italic: false,
            align: 0,
            text: text.into(),
        }
    }

    fn plain_rect(x: f32) -> DrawOp {
        DrawOp::Rect {
            x,
            y: 0.0,
            w: 1.0,
            h: 1.0,
            radius: 0.0,
            color: 0xff,
        }
    }

    #[test]
    fn bound_drops_what_cannot_be_painted() {
        let clip = |x| DrawOp::PushClip {
            x,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        let draw_image = |id| DrawOp::Image {
            id,
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        let mut ops = vec![
            plain_rect(f32::NAN),
            plain_rect(f32::INFINITY),
            plain_rect(2e6),
            plain_rect(1.0),
            text(0.0, "a"),
            text(MAX_FONT_SIZE + 1.0, "a"),
            text(12.0, ""),
            text(12.0, &"x".repeat(crate::capabilities::MAX_TEXT + 10)),
            // A clip out of range goes with its pop; what it clipped stays.
            clip(f32::NAN),
            plain_rect(2.0),
            DrawOp::PopClip,
            DrawOp::PopClip,
            draw_image(0),
            draw_image(1),
            draw_image(2),
            DrawOp::Path {
                points: vec![(0.0, 0.0), (f32::NAN, 1.0)],
                width: 1.0,
                color: 0xff,
            },
        ];
        bound(&mut ops, &[image(1, 1), image(1, MAX_IMAGE_SIDE + 1)]);
        let DrawOp::Text { text: cut, .. } = &ops[1] else {
            panic!("expected the long text, got {:?}", ops[1]);
        };
        assert_eq!(cut.len(), crate::capabilities::MAX_TEXT);
        assert_eq!(ops[0], plain_rect(1.0));
        assert_eq!(ops[2], plain_rect(2.0));
        assert_eq!(ops[3], draw_image(0));
        assert_eq!(ops.len(), 4);
    }

    #[test]
    fn bound_cuts_long_lists_and_deep_clips() {
        let mut ops = vec![plain_rect(0.0); MAX_OPS + 5];
        bound(&mut ops, &[]);
        assert_eq!(ops.len(), MAX_OPS);

        let path = DrawOp::Path {
            points: vec![(0.0, 0.0); MAX_POINTS / 2],
            width: 1.0,
            color: 0xff,
        };
        let mut ops = vec![path.clone(), path.clone(), path];
        bound(&mut ops, &[]);
        assert_eq!(ops.len(), 2);

        let clip = DrawOp::PushClip {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        };
        let deep = MAX_CLIP_DEPTH + 3;
        let mut ops = vec![clip; deep];
        ops.push(plain_rect(0.0));
        ops.extend(std::iter::repeat_n(DrawOp::PopClip, deep));
        bound(&mut ops, &[]);
        let pushes = ops
            .iter()
            .filter(|op| matches!(op, DrawOp::PushClip { .. }))
            .count();
        let pops = ops.iter().filter(|op| **op == DrawOp::PopClip).count();
        assert_eq!((pushes, pops), (MAX_CLIP_DEPTH, MAX_CLIP_DEPTH));
        assert!(ops.contains(&plain_rect(0.0)));
    }
}
