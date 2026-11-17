//! How paths are rasterized, once for every backend: a scene [`Path`] as tiny-skia's, filled
//! (non-zero) or stroked with round caps at a [`Mapping`]'s affine transform (the actual
//! ratio, strokes' widths included), antialiased. The CPU backend paints with
//! [`paint_path`] straight into the target; a GPU backend rasterizes the same path into a
//! small coverage mask ([`coverage`]) at the same transform, offset by whole pixels to the
//! [`ink_bounds`] it covers, so the two put the same coverage on the same pixels. A GPU backend
//! rasterizes only the part within the buffer ([`PixelBounds::within`]), as the CPU backend
//! paints only its buffer (tiny-skia's clipping to the pixmap changes the antialiasing, so the
//! cut must be the buffer's): a path that reaches far beyond the surface costs no more than
//! the surface.
//!
//! Must not own: what a path is (the scene's), where a mask goes (a backend's atlas), or
//! blending (each backend's).

use tiny_skia::{
    FillRule, LineCap, Mask, Paint, PathBuilder, Pixmap, PixmapMut, Stroke, Transform,
};

use crate::color::Rgba;
use crate::scene::{Mapping, Path, PathCommand};

/// A rectangle of whole buffer pixels: `x`, `y` its top-left corner, possibly negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PixelBounds {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width, at least 1.
    pub width: u32,
    /// Height, at least 1.
    pub height: u32,
}

impl PixelBounds {
    /// The part of these bounds inside `rect` (whole pixels, as [`Mapping::snap`] makes them),
    /// if any.
    pub fn within(self, rect: crate::scene::PixelRect) -> Option<PixelBounds> {
        let left = i64::from(self.x).max(rect.left as i64);
        let top = i64::from(self.y).max(rect.top as i64);
        let right = (i64::from(self.x) + i64::from(self.width)).min(rect.right as i64);
        let bottom = (i64::from(self.y) + i64::from(self.height)).min(rect.bottom as i64);
        (right > left && bottom > top).then(|| PixelBounds {
            x: left as i32,
            y: top as i32,
            width: (right - left) as u32,
            height: (bottom - top) as u32,
        })
    }
}

/// A coverage mask: one byte per pixel, rows packed, `bounds.width` wide.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// Where it goes, in buffer pixels.
    pub bounds: PixelBounds,
    /// 0 (none) to 255 (all), row by row.
    pub data: Vec<u8>,
}

/// `path` as tiny-skia's, in window-logical coordinates; `None` if it draws nothing.
pub(crate) fn skia_path(path: &Path) -> Option<tiny_skia::Path> {
    let mut builder = PathBuilder::new();
    for command in &path.commands {
        match *command {
            PathCommand::MoveTo(x, y) => builder.move_to(x, y),
            PathCommand::LineTo(x, y) => builder.line_to(x, y),
            PathCommand::QuadTo(x1, y1, x, y) => builder.quad_to(x1, y1, x, y),
            PathCommand::CubicTo(x1, y1, x2, y2, x, y) => builder.cubic_to(x1, y1, x2, y2, x, y),
            PathCommand::Close => builder.close(),
        }
    }
    builder.finish()
}

/// The mapping's path transform as tiny-skia's.
pub(crate) fn transform(mapping: &Mapping) -> Transform {
    let [sx, ky, kx, sy, tx, ty] = mapping.path_transform();
    Transform::from_row(sx, ky, kx, sy, tx, ty)
}

/// Fills `path`, or strokes it `stroke` logical pixels wide with round caps, in `color` on
/// `pixmap` at `transform`, cut to `mask`.
pub(crate) fn paint_path(
    pixmap: &mut PixmapMut<'_>,
    path: &tiny_skia::Path,
    color: Rgba,
    stroke: Option<f32>,
    transform: Transform,
    mask: Option<&Mask>,
) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    paint.anti_alias = true;
    match stroke {
        Some(width) => {
            let stroke = Stroke {
                width,
                line_cap: LineCap::Round,
                ..Stroke::default()
            };
            pixmap.stroke_path(path, &paint, &stroke, transform, mask);
        }
        None => pixmap.fill_path(path, &paint, FillRule::Winding, transform, mask),
    }
}

/// The buffer pixels `path` (filled, or stroked `stroke` wide) can touch under `mapping`,
/// conservatively: its control points' hull (curves stay inside it), widened by the farthest a
/// miter join of the stroke reaches in the larger ratio, and one pixel of antialiasing. `None` for an empty or non-finite
/// path.
pub fn ink_bounds(path: &Path, stroke: Option<f32>, mapping: &Mapping) -> Option<PixelBounds> {
    let [sx, _, _, sy, tx, ty] = mapping.path_transform();
    let (mut left, mut top, mut right, mut bottom) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    let mut point = |x: f32, y: f32| {
        let (x, y) = (x * sx + tx, y * sy + ty);
        left = left.min(x);
        top = top.min(y);
        right = right.max(x);
        bottom = bottom.max(y);
    };
    for command in &path.commands {
        match *command {
            PathCommand::MoveTo(x, y) | PathCommand::LineTo(x, y) => point(x, y),
            PathCommand::QuadTo(x1, y1, x, y) => {
                point(x1, y1);
                point(x, y);
            }
            PathCommand::CubicTo(x1, y1, x2, y2, x, y) => {
                point(x1, y1);
                point(x2, y2);
                point(x, y);
            }
            PathCommand::Close => {}
        }
    }
    // A stroke reaches half its width beyond the path, and a miter join up to the miter limit
    // times that (tiny-skia's default joins: miter, limit 4).
    let reach = |width: f32| width * sx.max(sy) / 2.0 * Stroke::default().miter_limit;
    let pad = stroke.map_or(0.0, reach) + 1.0;
    let (left, top) = ((left - pad).floor(), (top - pad).floor());
    let (right, bottom) = ((right + pad).ceil(), (bottom + pad).ceil());
    let fits = |v: f32| v.is_finite() && v.abs() < 1e7;
    if !(fits(left) && fits(top) && fits(right) && fits(bottom)) || right <= left || bottom <= top {
        return None;
    }
    Some(PixelBounds {
        x: left as i32,
        y: top as i32,
        width: (right - left) as u32,
        height: (bottom - top) as u32,
    })
}

/// `path`'s coverage under `mapping` within `bounds` (from [`ink_bounds`]): the pixels
/// [`paint_path`] would paint, as the alpha an opaque white paint leaves. `None` if it covers
/// nothing or the bounds can't be allocated.
pub fn coverage(
    path: &Path,
    stroke: Option<f32>,
    mapping: &Mapping,
    bounds: PixelBounds,
) -> Option<Coverage> {
    let skia = skia_path(path)?;
    let mut pixmap = Pixmap::new(bounds.width, bounds.height)?;
    // Whole pixels of offset: the rasterization is the target's, moved.
    let transform = transform(mapping).post_translate(-bounds.x as f32, -bounds.y as f32);
    let white = Rgba::rgb(0xffffff);
    paint_path(&mut pixmap.as_mut(), &skia, white, stroke, transform, None);
    let data: Vec<u8> = pixmap
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| px[3])
        .collect();
    data.iter()
        .any(|&a| a != 0)
        .then_some(Coverage { bounds, data })
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::{Metrics, Scale, WindowRect};
    use sighurt_api::ids::LayoutEpoch;

    use super::*;
    use crate::scene::PathBuilder as ScenePath;

    fn mapping(scale: u32) -> Mapping {
        let rect = WindowRect {
            x: 3,
            y: 5,
            width: 61,
            height: 33,
        };
        let scale = Scale::from_fractional(scale).unwrap();
        Mapping::new(&Metrics::new(LayoutEpoch::next(), rect, scale))
    }

    /// An acute join, whose miter reaches far beyond half the stroke.
    fn acute() -> Path {
        let mut path = ScenePath::new();
        path.move_to(10.0, 38.0);
        path.line_to(15.0, 20.0);
        path.line_to(20.0, 38.0);
        path.finish()
    }

    fn icon() -> Path {
        let mut path = ScenePath::new();
        path.move_to(10.0, 10.0);
        path.line_to(20.0, 14.5);
        path.quad_to(25.0, 30.0, 12.0, 25.0);
        path.finish()
    }

    /// The mask painted in its bounds is exactly what painting on the whole target paints,
    /// and nothing of it falls outside the bounds.
    #[test]
    fn coverage_is_the_targets_painting_moved() {
        for scale in [120, 150, 180, 240] {
            let map = mapping(scale);
            for (path, stroke) in [(icon(), None), (icon(), Some(1.5)), (acute(), Some(5.0))] {
                let bounds = ink_bounds(&path, stroke, &map).unwrap();
                let mask = coverage(&path, stroke, &map, bounds).unwrap();
                let mut whole = Pixmap::new(200, 120).unwrap();
                let skia = skia_path(&path).unwrap();
                let white = Rgba::rgb(0xffffff);
                paint_path(
                    &mut whole.as_mut(),
                    &skia,
                    white,
                    stroke,
                    transform(&map),
                    None,
                );
                for y in 0..120 {
                    for x in 0..200 {
                        let painted = whole.pixel(x, y).unwrap().alpha();
                        let (mx, my) = (x as i32 - bounds.x, y as i32 - bounds.y);
                        let inside = (0..bounds.width as i32).contains(&mx)
                            && (0..bounds.height as i32).contains(&my);
                        let masked = if inside {
                            mask.data[(my * bounds.width as i32 + mx) as usize]
                        } else {
                            0
                        };
                        assert_eq!(painted, masked, "({x}, {y}) at {scale}, {stroke:?}");
                    }
                }
            }
        }
    }

    /// A path that crosses the surface's edges, rasterized only within the surface, has the
    /// coverage the CPU backend paints into the surface: tiny-skia clips a path to its
    /// pixmap, and that clipping changes the antialiasing along the path, so the bounds must
    /// be cut to the buffer (not to a clip inside it, which the CPU backend applies as a
    /// mask after painting). One that reaches millions of pixels beyond the surface costs
    /// only the surface.
    #[test]
    fn coverage_within_the_surface_is_what_the_surface_paints() {
        for scale in [120, 150, 180, 240] {
            for (x, y, width, height) in [(3, 5, 61, 33), (15, 17, 9, 8), (2, 2, 14, 20)] {
                let rect = WindowRect {
                    x,
                    y,
                    width,
                    height,
                };
                let scale = Scale::from_fractional(scale).unwrap();
                let metrics = Metrics::new(LayoutEpoch::next(), rect, scale);
                let map = Mapping::new(&metrics);
                let buffer = metrics.buffer();
                let surface = crate::scene::PixelRect::from_ltrb(
                    0.0,
                    0.0,
                    buffer.width as f32,
                    buffer.height as f32,
                )
                .unwrap();
                for (path, stroke) in [(icon(), None), (icon(), Some(1.5)), (acute(), Some(5.0))] {
                    let mut painted = Pixmap::new(buffer.width, buffer.height).unwrap();
                    let skia = skia_path(&path).unwrap();
                    let white = Rgba::rgb(0xffffff);
                    let to = transform(&map);
                    paint_path(&mut painted.as_mut(), &skia, white, stroke, to, None);
                    let visible =
                        ink_bounds(&path, stroke, &map).and_then(|bounds| bounds.within(surface));
                    let mask = visible.and_then(|b| coverage(&path, stroke, &map, b));
                    for py in 0..buffer.height {
                        for px in 0..buffer.width {
                            let expected = painted.pixel(px, py).unwrap().alpha();
                            let got = mask.as_ref().map_or(0, |mask| {
                                let b = mask.bounds;
                                let (mx, my) = (px as i32 - b.x, py as i32 - b.y);
                                let inside = (0..b.width as i32).contains(&mx)
                                    && (0..b.height as i32).contains(&my);
                                if inside {
                                    mask.data[(my * b.width as i32 + mx) as usize]
                                } else {
                                    0
                                }
                            });
                            assert_eq!(
                                got, expected,
                                "({px}, {py}) of {rect:?} at {scale:?}, {stroke:?}"
                            );
                        }
                    }
                }
            }
        }
        let map = mapping(240);
        let mut huge = ScenePath::new();
        huge.move_to(-4.0e6, -4.0e6);
        huge.line_to(4.0e6, -4.0e6);
        huge.line_to(0.0, 4.0e6);
        huge.close();
        let huge = huge.finish();
        let bounds = ink_bounds(&huge, None, &map).unwrap();
        assert!(bounds.width > 10_000_000, "{bounds:?}");
        let buffer = crate::scene::PixelRect::from_ltrb(0.0, 0.0, 122.0, 66.0).unwrap();
        let visible = bounds.within(buffer).unwrap();
        assert_eq!((visible.width, visible.height), (122, 66));
        let covered = coverage(&huge, None, &map, visible).unwrap();
        assert!(
            covered.data.iter().all(|&a| a == 255),
            "the surface is inside the triangle"
        );
        let outside = crate::scene::PixelRect::from_ltrb(-9.0e9, 0.0, -8.0e9, 1.0).unwrap();
        assert_eq!(bounds.within(outside), None);
    }
}
