//! Scenes for comparing renderers: a seeded random scene generator (every item kind, clips,
//! fractional edges, odd sizes and scales) and the fixed "every kind alone under every clip
//! case" set. Built only through the public `Pen` and `Fonts` API, so any backend can render
//! them. Included by path, by the GPU crates' comparison tests (`compare.rs`) and by
//! `gpu/tests.rs`. The including module declares `use <the render crate> as render;` and a
//! sibling `fuzz` (tools/fuzz/mod.rs).
//!
//! Must not own: how a scene is rendered or compared (the including tests'), or any
//! non-public renderer detail.

#![allow(dead_code)]

use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::LayoutEpoch;

use super::fuzz::Rng;
use super::render::scene::PathBuilder;
use super::render::{Fonts, Pen, Rgba, Scene, TextStyle};

/// The scales random scenes use, as numerators over 120.
pub const RANDOM_SCALES: [u32; 8] = [120, 126, 140, 150, 168, 180, 210, 240];
/// The scales the fixed scenes cover: 100 %, 125 %, 150 % and 200 %.
pub const FIXED_SCALES: [u32; 4] = [120, 150, 180, 240];

/// Metrics for `rect` at `scale` (over 120).
pub fn metrics(rect: WindowRect, scale: u32) -> Metrics {
    Metrics::new(
        LayoutEpoch::next(),
        rect,
        Scale::from_fractional(scale).expect("a valid scale"),
    )
}

const ALPHAS: [u8; 7] = [0, 1, 64, 128, 200, 254, 255];

const TEXTS: [&str; 14] = [
    "Sighurt",
    "Hello, world",
    "fjord 0123456789",
    "Grüße räksmörgås",
    "e\u{301}a\u{308}o\u{302}",
    "中文字符 日本語",
    "😀🎉 emoji ✓",
    "ÅÄÖ åäö ÉÈ",
    "WMQ@#%&",
    "iiiillll....",
    "a\u{200d}b",
    "— – … «»",
    "x",
    "Blåbärssylt och kanelbullar från Göteborg",
];

fn f(rng: &mut Rng, lo: f32, hi: f32) -> f32 {
    lo + rng.below(100_001) as f32 / 100_000.0 * (hi - lo)
}

fn color(rng: &mut Rng) -> Rgba {
    let rgb = rng.next_u64() as u32 & 0x00ff_ffff;
    let alpha = *rng.pick(&ALPHAS);
    Rgba::rgb_alpha(rgb << 8 | u32::from(alpha))
}

/// The surface a scene is drawn on, in window-logical pixels.
#[derive(Clone, Copy)]
struct Area {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Area {
    /// A rectangle near the surface: mostly on it, partly or wholly off it.
    fn rect(&self, rng: &mut Rng) -> [f32; 4] {
        let (w, h) = (self.w, self.h);
        match rng.below(12) {
            // Wholly outside, on a random side.
            0 => {
                let (rw, rh) = (f(rng, 1.0, 60.0), f(rng, 1.0, 40.0));
                match rng.below(4) {
                    0 => [
                        self.x - rw - f(rng, 0.0, 30.0),
                        self.y + f(rng, 0.0, h),
                        rw,
                        rh,
                    ],
                    1 => [
                        self.x + w + f(rng, 0.0, 30.0),
                        self.y + f(rng, 0.0, h),
                        rw,
                        rh,
                    ],
                    2 => [
                        self.x + f(rng, 0.0, w),
                        self.y - rh - f(rng, 0.0, 30.0),
                        rw,
                        rh,
                    ],
                    _ => [
                        self.x + f(rng, 0.0, w),
                        self.y + h + f(rng, 0.0, 30.0),
                        rw,
                        rh,
                    ],
                }
            }
            // Zero size.
            1 => [
                self.x + f(rng, 0.0, w),
                self.y + f(rng, 0.0, h),
                if rng.one_in(2) {
                    0.0
                } else {
                    f(rng, 1.0, 30.0)
                },
                if rng.one_in(2) {
                    0.0
                } else {
                    f(rng, 1.0, 30.0)
                },
            ],
            // Negative size.
            2 => [
                self.x + f(rng, 0.0, w),
                self.y + f(rng, 0.0, h),
                -f(rng, 0.0, 30.0),
                f(rng, -10.0, 30.0),
            ],
            // Covering the surface and more.
            3 => [
                self.x - f(rng, 0.0, 10.0),
                self.y - f(rng, 0.0, 10.0),
                w + 20.0,
                h + 20.0,
            ],
            // Small, on the surface.
            4..=7 => [
                self.x + f(rng, 0.0, w),
                self.y + f(rng, 0.0, h),
                f(rng, 0.5, 40.0),
                f(rng, 0.5, 30.0),
            ],
            // Anywhere, any size, edges across the surface's.
            _ => [
                self.x + f(rng, -20.0, w),
                self.y + f(rng, -20.0, h),
                f(rng, 1.0, w + 20.0),
                f(rng, 1.0, h + 20.0),
            ],
        }
    }

    /// A corner radius: mostly small, sometimes over half the size (clamped to it), sometimes
    /// negative. Without `hard`, at most 12 and only on small rectangles: see
    /// [`random_scene`].
    fn radius(&self, rng: &mut Rng, rect: [f32; 4], hard: bool) -> f32 {
        let longest = if hard { 24.0 } else { 12.0 };
        let side = rect[2].abs().min(rect[3].abs());
        match rng.below(10) {
            0 => 0.0,
            1 => -f(rng, 0.0, 8.0),
            2 if hard || side <= 24.0 => side * f(rng, 0.5, 1.5),
            3 => f(rng, 0.0, 2.0),
            _ => f(rng, 0.0, longest),
        }
    }

    /// A point for an icon of at most 48 px across, near the surface.
    fn spot(&self, rng: &mut Rng) -> (f32, f32) {
        (
            self.x + f(rng, -10.0, self.w + 10.0),
            self.y + f(rng, -10.0, self.h + 10.0),
        )
    }
}

fn icon_point(rng: &mut Rng, (ox, oy): (f32, f32), span: f32) -> (f32, f32) {
    (ox + f(rng, 0.0, span), oy + f(rng, 0.0, span))
}

/// A path of at most `span` px across at `origin`: a polyline, quads, cubics, a circle or a
/// dot, sometimes closed, sometimes with a second subpath.
fn path(rng: &mut Rng, origin: (f32, f32), span: f32, p: &mut PathBuilder) {
    let (x, y) = icon_point(rng, origin, span);
    p.move_to(x, y);
    match rng.below(6) {
        0 => {
            let r = f(rng, 0.5, span / 2.0);
            p.push_circle(origin.0 + span / 2.0, origin.1 + span / 2.0, r);
        }
        1 => {
            for _ in 0..rng.range(1, 3) {
                let (cx, cy) = icon_point(rng, origin, span);
                let (ex, ey) = icon_point(rng, origin, span);
                p.quad_to(cx, cy, ex, ey);
            }
        }
        2 => {
            for _ in 0..rng.range(1, 3) {
                let (c1x, c1y) = icon_point(rng, origin, span);
                let (c2x, c2y) = icon_point(rng, origin, span);
                let (ex, ey) = icon_point(rng, origin, span);
                p.cubic_to(c1x, c1y, c2x, c2y, ex, ey);
            }
        }
        3 if rng.one_in(3) => {
            // A dot: a zero-length line.
            p.line_to(x, y);
        }
        _ => {
            for _ in 0..rng.range(1, 6) {
                let (px, py) = icon_point(rng, origin, span);
                p.line_to(px, py);
            }
        }
    }
    if rng.one_in(3) {
        p.close();
    }
    if rng.one_in(6) {
        let (x, y) = icon_point(rng, origin, span);
        p.move_to(x, y);
        let (x, y) = icon_point(rng, origin, span);
        p.line_to(x, y);
    }
}

/// A shape to fill, of at most `span` px across at `origin`: a circle, a convex polygon, a
/// lens of two quads or a closed cubic.
fn shape_path(rng: &mut Rng, origin: (f32, f32), span: f32, p: &mut PathBuilder) {
    let (cx, cy, r) = (origin.0 + span / 2.0, origin.1 + span / 2.0, span / 2.0);
    match rng.below(4) {
        0 => p.push_circle(cx, cy, f(rng, 0.5, r)),
        1 => {
            // Points on a circle, in angle order: convex.
            let mut angles: Vec<f32> = (0..rng.range(3, 7))
                .map(|_| f(rng, 0.0, std::f32::consts::TAU))
                .collect();
            angles.sort_by(f32::total_cmp);
            for (i, a) in angles.iter().enumerate() {
                let (x, y) = (cx + r * a.cos(), cy + r * a.sin());
                if i == 0 {
                    p.move_to(x, y);
                } else {
                    p.line_to(x, y);
                }
            }
            p.close();
        }
        2 => {
            p.move_to(cx - r, cy);
            p.quad_to(cx, cy - f(rng, 0.5, r), cx + r, cy);
            p.quad_to(cx, cy + f(rng, 0.5, r), cx - r, cy);
            p.close();
        }
        _ => {
            p.move_to(cx - r, cy);
            p.cubic_to(cx - r, cy - r, cx + r, cy - r, cx + r, cy);
            p.close();
        }
    }
}

/// Where a text box sits and the clip argument for it.
fn text_clip(rng: &mut Rng, area: &Area, at: (f32, f32), w: f32, h: f32) -> [f32; 4] {
    match rng.below(8) {
        0 => [
            at.0 + f(rng, 0.0, w),
            at.1 + f(rng, 0.0, h),
            f(rng, 0.5, 3.0),
            h,
        ],
        1 => [at.0, at.1 + f(rng, 0.0, h), w, f(rng, 0.3, 3.0)],
        2 => [at.0, at.1, 0.0, 0.0],
        3 => [area.x + area.w + 5.0, area.y + area.h + 5.0, w, h],
        4 => [
            at.0 + f(rng, 0.0, w / 2.0),
            at.1 + f(rng, 0.0, 2.0),
            f(rng, 1.0, w),
            h,
        ],
        _ => [at.0, at.1, w, h],
    }
}

fn style(rng: &mut Rng) -> TextStyle {
    match rng.below(5) {
        0 => TextStyle::XS,
        1 => TextStyle::HEADING,
        2 => TextStyle::SM.at(rng.range(9, 40) as u16),
        3 => TextStyle::XS.at(rng.range(9, 40) as u16),
        _ => TextStyle::SM,
    }
}

/// A piece of text from the pool, at most 20 characters unless it is the long one.
fn text(rng: &mut Rng) -> String {
    let text = rng.pick(&TEXTS);
    if rng.one_in(4) {
        text.chars().take(rng.range(1, 12)).collect()
    } else {
        (*text).to_owned()
    }
}

struct Budget {
    items: usize,
    texts: usize,
    /// Big translucent fills left: each blends with 8-bit rounding on the CPU and in float on
    /// a GPU, so the two drift apart by a level a layer where many stack over one area; a
    /// scene gets one such wash, and its other big fills are opaque.
    washes: usize,
    /// Whether the scene may hold what the renderers antialias differently (see [`random_scene`]).
    hard: bool,
}

/// A color for a fill of `rect`: translucent only if it is small or the scene's one wash.
fn fill_color(rng: &mut Rng, budget: &mut Budget, rect: [f32; 4]) -> Rgba {
    let color = color(rng);
    if rect[2].abs() * rect[3].abs() < 1500.0 || color.a == 255 || color.a == 0 {
        return color;
    }
    if budget.washes > 0 {
        budget.washes -= 1;
        return color;
    }
    Rgba { a: 255, ..color }
}

fn item(rng: &mut Rng, fonts: &mut Fonts, pen: &mut Pen, area: &Area, budget: &mut Budget) {
    budget.items -= 1;
    let kind = rng.below(10);
    match kind {
        0 | 1 => {
            let rect = area.rect(rng);
            pen.fill(rect, fill_color(rng, budget, rect));
        }
        2 | 3 => {
            let rect = area.rect(rng);
            let radius = area.radius(rng, rect, budget.hard);
            pen.fill_rounded(rect, radius, fill_color(rng, budget, rect));
        }
        4 => {
            let rect = area.rect(rng);
            // Radius below the line width too.
            let radius = if rng.one_in(4) {
                f(rng, 0.0, 1.5)
            } else {
                area.radius(rng, rect, budget.hard)
            };
            let fill = fill_color(rng, budget, rect);
            pen.bordered(rect, radius, fill, color(rng));
        }
        5 | 6 => {
            let span = f(rng, 2.0, 48.0);
            let origin = area.spot(rng);
            // Strokes under a buffer pixel are tiny-skia's hairlines, blended segment by
            // segment, which differ by more than the path tolerance where one overlaps
            // itself (seen: 17): only the hard scenes have them.
            let thinnest = if budget.hard { 0.5 } else { 1.0 };
            let width = f(rng, thinnest, 4.0);
            pen.stroke(width, color(rng), |p| path(rng, origin, span, p));
        }
        7 => {
            let span = f(rng, 2.0, 48.0);
            let origin = area.spot(rng);
            pen.shape(color(rng), |p| shape_path(rng, origin, span, p));
        }
        _ => {
            if budget.texts == 0 {
                return;
            }
            budget.texts -= 1;
            let style = style(rng);
            let content = text(rng);
            let shaped = match rng.below(3) {
                0 => fonts.wrapped(&content, style, f(rng, 20.0, 200.0)),
                1 => fonts.fitted(&content, style, f(rng, 10.0, 200.0)),
                _ => fonts.line(&content, style),
            };
            let at = (
                area.x + f(rng, -10.0, area.w),
                area.y + f(rng, -10.0, area.h),
            );
            let clip = text_clip(rng, area, at, shaped.width.max(1.0), shaped.height.max(1.0));
            pen.text(&shaped, at, color(rng), clip);
        }
    }
}

fn group(
    rng: &mut Rng,
    fonts: &mut Fonts,
    pen: &mut Pen,
    area: &Area,
    budget: &mut Budget,
    depth: usize,
) {
    let count = rng.range(0, 8).min(budget.items);
    for _ in 0..count {
        if budget.items == 0 {
            return;
        }
        if depth < 3 && rng.one_in(5) {
            let clip = match rng.below(6) {
                // Empty.
                0 => [area.x + f(rng, 0.0, area.w), area.y, 0.0, area.h],
                // Wholly outside the surface.
                1 => [area.x + area.w + 3.0, area.y - 40.0, 30.0, 30.0],
                _ => area.rect(rng),
            };
            pen.clip(clip, |pen| group(rng, fonts, pen, area, budget, depth + 1));
        } else {
            item(rng, fonts, pen, area, budget);
        }
    }
}

/// A random scene and the metrics to render it with: a surface at a random window position,
/// 1..=900 by 1..=240 logical pixels, at one of [`RANDOM_SCALES`], with 0..=40 items of
/// every kind and nested clips. Left out, because the CPU backend and the GPU preparation
/// differ there by more than their tolerances (reproduced by an `#[ignore]`d test, and
/// generated by [`random_hard_scene`]): corner radii over 12 logical pixels (an analytic
/// corner against tiny-skia's 4× supersampled one: 12 to 18 levels off at radius 36 buffer
/// pixels, and 33 where the corner also crosses the surface's edge, which tiny-skia clips).
/// And strokes thinner than a pixel that overlap themselves (tiny-skia's hairlines, up to 17
/// levels off). Other paths are in, within the comparisons' path tolerance.
pub fn random_scene(rng: &mut Rng, fonts: &mut Fonts) -> (Metrics, Scene) {
    random_scene_with(rng, fonts, false)
}

/// [`random_scene`] with long corner arcs as well.
pub fn random_hard_scene(rng: &mut Rng, fonts: &mut Fonts) -> (Metrics, Scene) {
    random_scene_with(rng, fonts, true)
}

fn random_scene_with(rng: &mut Rng, fonts: &mut Fonts, hard: bool) -> (Metrics, Scene) {
    let (mut width, mut height) = match rng.below(10) {
        0 => (1, 1),
        1 => (rng.range(1, 6), rng.range(1, 6)),
        _ => (rng.range(1, 900), rng.range(1, 240)),
    };
    if rng.one_in(3) {
        width |= 1;
        height |= 1;
    }
    let rect = WindowRect {
        x: rng.range(0, 2000) as i32,
        y: rng.range(0, 2000) as i32,
        width: width as u32,
        height: height as u32,
    };
    let metrics = metrics(rect, *rng.pick(&RANDOM_SCALES));
    let area = Area {
        x: rect.x as f32,
        y: rect.y as f32,
        w: rect.width as f32,
        h: rect.height as f32,
    };
    let mut pen = Pen::new(rect);
    if !rng.one_in(8) {
        pen.clear(color(rng));
    }
    let mut budget = Budget {
        items: rng.range(0, 40),
        texts: 10,
        washes: 1,
        hard,
    };
    while budget.items > 0 {
        group(rng, fonts, &mut pen, &area, &mut budget, 0);
    }
    (metrics, pen.finish())
}

/// The item kinds of the fixed scenes.
pub const KINDS: [&str; 7] = [
    "rect",
    "rounded",
    "bordered",
    "stroke",
    "shape",
    "text",
    "clipped-text",
];

/// The clip cases of the fixed scenes, around the item's box.
pub const CLIPS: [&str; 5] = ["none", "inside", "fractional", "outside", "nested"];

/// The fixed surface of the fixed scenes: odd-sized, at an odd position.
pub const FIXED_RECT: WindowRect = WindowRect {
    x: 13,
    y: 21,
    width: 121,
    height: 61,
};

/// Every item kind alone, under each clip case, at each of [`FIXED_SCALES`]: `(name, metrics,
/// scene)`.
pub fn kind_scenes(fonts: &mut Fonts) -> Vec<(String, Metrics, Scene)> {
    let mut scenes = Vec::new();
    let bg = Rgba::rgb(0x1b1b20);
    let ink = Rgba::rgb_alpha(0xe8a33dd0);
    let edge = Rgba::rgb(0x60a5fa);
    // The item's box, window-logical.
    let (bx, by, bw, bh) = (33.3_f32, 31.6_f32, 50.4_f32, 30.2_f32);
    let text = fonts.line("Grüße fjord 0123", TextStyle::SM);
    let clipped = fonts.line("Blåbärssylt och kanelbullar", TextStyle::HEADING);
    for kind in KINDS {
        for clip in CLIPS {
            for scale in FIXED_SCALES {
                let mut pen = Pen::new(FIXED_RECT);
                pen.clear(bg);
                let draw = |pen: &mut Pen| match kind {
                    "rect" => pen.fill([bx, by, bw, bh], ink),
                    "rounded" => pen.fill_rounded([bx, by, bw, bh], 7.0, ink),
                    "bordered" => pen.bordered([bx, by, bw, bh], 6.0, ink, edge),
                    "stroke" => pen.stroke(1.7, ink, |p| {
                        p.move_to(bx, by + bh);
                        p.quad_to(bx + 10.0, by - 5.0, bx + 25.0, by + 12.0);
                        p.cubic_to(bx + 30.0, by + 30.0, bx + 40.0, by, bx + 48.0, by + 20.0);
                    }),
                    "shape" => pen.shape(ink, |p| {
                        p.push_circle(bx + 20.0, by + 15.0, 13.5);
                        p.move_to(bx + 30.0, by + 4.0);
                        p.line_to(bx + 48.0, by + 10.0);
                        p.line_to(bx + 38.0, by + 28.0);
                        p.close();
                    }),
                    "text" => pen.text(&text, (bx, by), ink, [bx, by, bw + 20.0, bh]),
                    _ => pen.text(&clipped, (bx, by - 4.0), ink, [bx + 3.5, by, 31.3, 12.4]),
                };
                match clip {
                    "none" => draw(&mut pen),
                    "inside" => pen.clip([bx + 6.0, by + 5.0, 20.0, 12.0], draw),
                    "fractional" => pen.clip([bx + 8.4, by + 3.3, bw - 14.7, bh - 7.1], draw),
                    "outside" => pen.clip([bx + bw + 4.0, by + bh + 4.0, 20.0, 10.0], draw),
                    _ => pen.clip([bx + 2.5, by + 2.5, bw, bh], |pen| {
                        pen.clip([bx - 5.0, by + 6.25, bw - 9.5, bh], |pen| {
                            pen.clip([bx + 4.0, by - 3.0, 30.5, 20.7], draw);
                        });
                    }),
                }
                let name = format!("{kind} under clip {clip} at {scale}");
                scenes.push((name, metrics(FIXED_RECT, scale), pen.finish()));
            }
        }
    }
    scenes
}
