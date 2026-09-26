//! Turns a guest's canvas into [`sighurt_ipc`] draw lists.
//!
//! [`convert`] maps canvas commands to [`DrawOp`]s: the translation, opacity and page zoom are
//! baked into coordinates and colors, clips become [`DrawOp::PushClip`] / [`DrawOp::PopClip`]
//! pairs, and the shapes the protocol has no op for are built from the ones it has. [`Presenter`]
//! sends a page's draw lists and images to the browser, and only what changed.

use std::sync::Arc;

use sighurt_engine_wasm::capabilities::{
    CanvasState, DecodedImage, DrawCommand, GradientStop, Rgba,
};
use sighurt_ipc::{DrawOp, FromEngine};

/// How far, in pixels, a flattened curve may stray from the true one.
const TOLERANCE: f32 = 0.25;

/// Sends a page's draw lists, uploading each image once.
#[derive(Default)]
pub struct Presenter {
    /// The draw list last sent.
    ops: Vec<DrawOp>,
    /// Where the next list is built, to reuse its allocation.
    scratch: Vec<DrawOp>,
    /// What `ops` was built from: the canvas commands, zoom and overlay. Most frames of a page
    /// that isn't animating draw exactly what the one before drew; comparing these finds that
    /// without building the list. The commands are only kept once a frame repeated the list
    /// before it, so that an animating page doesn't pay for copying them every frame.
    commands: Option<Vec<DrawCommand>>,
    zoom: f32,
    overlay: Vec<DrawOp>,
    /// The images of the canvas generation last presented, by canvas index. Kept so that
    /// images a cleared canvas draws again keep their ids instead of being sent again.
    images: Vec<Image>,
    generation: Option<u64>,
    next_id: u32,
}

/// An image the browser has.
struct Image {
    id: u32,
    image: Arc<DecodedImage>,
}

impl Presenter {
    /// Forgets which canvas was presented, for a new document. Its images are reused or dropped
    /// at the next [`present`](Self::present).
    pub fn reset(&mut self) {
        self.generation = None;
    }

    /// Sends `canvas`, zoomed by `zoom`, followed by `overlay` (unzoomed): first the images the
    /// browser doesn't have yet, then the draw list if it changed, then drops for the images it
    /// no longer uses.
    pub fn present(
        &mut self,
        canvas: &CanvasState,
        zoom: f32,
        overlay: &[DrawOp],
        send: &mut impl FnMut(&FromEngine),
    ) {
        let (retired, remapped) = self.sync_images(canvas, send);
        let unchanged = !remapped
            && zoom == self.zoom
            && overlay == self.overlay
            && self.commands.as_ref() == Some(&canvas.commands);
        if !unchanged {
            let mut ops = std::mem::take(&mut self.scratch);
            ops.clear();
            let image_id = |index: usize| self.images.get(index).map(|image| image.id);
            convert(&canvas.commands, image_id, zoom, &mut ops);
            ops.extend_from_slice(overlay);
            if ops == self.ops {
                self.scratch = ops;
                self.commands = Some(canvas.commands.clone());
            } else {
                let msg = FromEngine::Draw { ops };
                send(&msg);
                if let FromEngine::Draw { ops } = msg {
                    self.scratch = std::mem::replace(&mut self.ops, ops);
                }
                self.commands = None;
            }
            self.zoom = zoom;
            self.overlay.clear();
            self.overlay.extend_from_slice(overlay);
        }
        for id in retired {
            send(&FromEngine::DropImage { id });
        }
    }

    /// Uploads the images of `canvas` the browser doesn't have. Returns the ids of those it no
    /// longer needs, to drop once a draw list without them is out, and whether a canvas image
    /// index now stands for a different id.
    fn sync_images(
        &mut self,
        canvas: &CanvasState,
        send: &mut impl FnMut(&FromEngine),
    ) -> (Vec<u32>, bool) {
        if self.generation == Some(canvas.generation) {
            // Images are only appended within a generation.
            for image in canvas.images.iter().skip(self.images.len()) {
                let uploaded = self.upload(image, send);
                self.images.push(uploaded);
            }
            return (Vec::new(), false);
        }
        // The canvas was cleared: images it draws again keep their ids, the rest are retired.
        self.generation = Some(canvas.generation);
        let mut old: Vec<Option<Image>> = self.images.drain(..).map(Some).collect();
        let mut remapped = false;
        for (index, image) in canvas.images.iter().enumerate() {
            let kept = old.iter_mut().enumerate().find(|(_, old)| {
                old.as_ref()
                    .is_some_and(|old| Arc::ptr_eq(&old.image, image) || *old.image == **image)
            });
            let image = match kept {
                Some((old_index, kept)) => {
                    remapped |= old_index != index;
                    kept.take().unwrap()
                }
                None => {
                    remapped = true;
                    self.upload(image, send)
                }
            };
            self.images.push(image);
        }
        (
            old.into_iter().flatten().map(|image| image.id).collect(),
            remapped,
        )
    }

    fn upload(&mut self, image: &Arc<DecodedImage>, send: &mut impl FnMut(&FromEngine)) -> Image {
        self.next_id = self.next_id.wrapping_add(1);
        send(&FromEngine::Image {
            id: self.next_id,
            width: image.width,
            height: image.height,
            rgba: image.pixels.clone(),
        });
        Image {
            id: self.next_id,
            image: image.clone(),
        }
    }
}

/// The transform, opacity and clip depth that apply to a command.
#[derive(Clone, Copy)]
struct State {
    dx: f32,
    dy: f32,
    opacity: f32,
    clips: usize,
    zoom: f32,
}

impl State {
    fn point(&self, x: f32, y: f32) -> (f32, f32) {
        ((x + self.dx) * self.zoom, (y + self.dy) * self.zoom)
    }

    fn rect(&self, x: f32, y: f32, w: f32, h: f32) -> [f32; 4] {
        let (x, y) = self.point(x, y);
        [x, y, w * self.zoom, h * self.zoom]
    }

    fn color(&self, [r, g, b, a]: Rgba) -> u32 {
        let a = (f32::from(a) * self.opacity).round().clamp(0.0, 255.0) as u8;
        u32::from_be_bytes([r, g, b, a])
    }
}

/// Appends the draw ops for `commands`, scaled by `zoom`, to `ops`. `image_id` maps canvas
/// image indices to protocol image ids.
///
/// Transforms apply only their translation, and clips stay axis-aligned rectangles.
pub fn convert(
    commands: &[DrawCommand],
    image_id: impl Fn(usize) -> Option<u32>,
    zoom: f32,
    ops: &mut Vec<DrawOp>,
) {
    let mut state = State {
        dx: 0.0,
        dy: 0.0,
        opacity: 1.0,
        clips: 0,
        zoom,
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
                    radius: (radius * zoom).min(w.min(h) / 2.0).max(0.0),
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
                let radius = (radius * zoom).abs();
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
                width: thickness * zoom,
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
                let radius = radius * zoom;
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
                    width: thickness * zoom,
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
                    width: thickness * zoom,
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
                    size: size * zoom,
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
                    size: size * zoom,
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
                    radial_gradient(ops, rect, from, (by * zoom).abs(), color);
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
            // Reach half a pixel under the next strip, which covers it, so no seam shows.
            (i + 1) as f32 / n as f32 + 0.5 / len2.sqrt()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ops(commands: &[DrawCommand], zoom: f32) -> Vec<DrawOp> {
        let mut ops = Vec::new();
        convert(commands, |i| Some(i as u32 + 100), zoom, &mut ops);
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
    fn translation_opacity_and_zoom_are_baked_in() {
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
            ops(&commands, 2.0),
            [
                DrawOp::Clear { color: 0x112233ff },
                DrawOp::Rect {
                    x: 20.0,
                    y: 10.0,
                    w: 20.0,
                    h: 20.0,
                    radius: 0.0,
                    color: red(128),
                },
                DrawOp::Rect {
                    x: 2.0,
                    y: 2.0,
                    w: 4.0,
                    h: 4.0,
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
        let kinds: Vec<_> = ops(&commands, 1.0)
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
            ops(&commands, 2.0),
            [
                DrawOp::Rect {
                    x: 10.0,
                    y: 30.0,
                    w: 20.0,
                    h: 20.0,
                    radius: 10.0,
                    color: 0xff,
                },
                // The radius is clamped to half the shorter side.
                DrawOp::Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 20.0,
                    h: 8.0,
                    radius: 4.0,
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
        let ops = ops(&commands, 1.0);
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
        let bands = ops(&[gradient(10.0, 220.0)], 1.0);
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
        for band in ops(&[gradient(110.0, 220.0)], 1.0) {
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
        let ops = ops(&commands, 1.0);
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
            ops(&commands, 1.5),
            [DrawOp::Text {
                x: 150.0,
                y: 15.0,
                size: 18.0,
                color: 0x010203ff,
                family: "Georgia".into(),
                weight: 700,
                italic: true,
                align: 2,
                text: "Hi".into(),
            }]
        );
    }

    fn canvas(generation: u64, images: Vec<Arc<DecodedImage>>) -> CanvasState {
        let mut canvas = CanvasState::default();
        canvas.generation = generation;
        for image in images {
            canvas.push_image(image, 0.0, 0.0, 1.0, 1.0);
        }
        canvas
    }

    fn image(v: u8) -> Arc<DecodedImage> {
        Arc::new(DecodedImage {
            width: 1,
            height: 1,
            pixels: vec![v; 4],
        })
    }

    /// The messages one present sends, as short descriptions.
    fn present(presenter: &mut Presenter, canvas: &CanvasState) -> Vec<String> {
        let mut sent = Vec::new();
        presenter.present(canvas, 1.0, &[], &mut |msg| {
            sent.push(match msg {
                FromEngine::Image { id, .. } => format!("image {id}"),
                FromEngine::DropImage { id } => format!("drop {id}"),
                FromEngine::Draw { ops } => {
                    let ids: Vec<_> = ops
                        .iter()
                        .map(|op| match op {
                            DrawOp::Image { id, .. } => id.to_string(),
                            _ => "?".into(),
                        })
                        .collect();
                    format!("draw {}", ids.join(" "))
                }
                other => format!("{other:?}"),
            })
        });
        sent
    }

    #[test]
    fn images_are_uploaded_once_and_dropped_when_unused() {
        let mut presenter = Presenter::default();
        assert_eq!(
            present(&mut presenter, &canvas(1, vec![image(1)])),
            ["image 1", "draw 1"]
        );
        // Nothing changed.
        assert!(present(&mut presenter, &canvas(1, vec![image(1)])).is_empty());
        // An image appended within the generation.
        assert_eq!(
            present(&mut presenter, &canvas(1, vec![image(1), image(2)])),
            ["image 2", "draw 1 2"]
        );
        // Cleared and redrawn with the same pixels (in another order): nothing to send.
        assert_eq!(
            present(&mut presenter, &canvas(2, vec![image(2), image(1)])),
            ["draw 2 1"]
        );
        // Cleared and redrawn with a new image: the unused one is dropped after the draw list.
        assert_eq!(
            present(&mut presenter, &canvas(3, vec![image(1), image(3)])),
            ["image 3", "draw 1 3", "drop 2"]
        );
        // A new document whose canvas starts at the same generation number.
        presenter.reset();
        assert_eq!(
            present(&mut presenter, &canvas(3, vec![image(4)])),
            ["image 4", "draw 4", "drop 1", "drop 3"]
        );
    }

    #[test]
    fn the_list_is_sent_when_its_commands_zoom_or_overlay_change() {
        let mut presenter = Presenter::default();
        let mut canvas = CanvasState::default();
        canvas.push(rect(0.0, 0.0, 1.0, 1.0, 255));
        let mut sent = 0;
        let mut present = |presenter: &mut Presenter, canvas: &CanvasState, zoom, overlay| {
            presenter.present(canvas, zoom, overlay, &mut |_| sent += 1);
        };
        present(&mut presenter, &canvas, 1.0, &[]);
        present(&mut presenter, &canvas, 1.0, &[]);
        present(&mut presenter, &canvas, 2.0, &[]);
        present(&mut presenter, &canvas, 2.0, &[DrawOp::PopClip]);
        canvas.push(rect(1.0, 0.0, 1.0, 1.0, 255));
        present(&mut presenter, &canvas, 2.0, &[DrawOp::PopClip]);
        present(&mut presenter, &canvas, 2.0, &[DrawOp::PopClip]);
        assert_eq!(sent, 4);
    }
}
