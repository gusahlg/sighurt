//! How paths are rasterized, once for every backend: a scene [`Path`] as tiny-skia's, filled
//! (non-zero) or stroked with round caps at a [`Mapping`]'s affine transform (the actual
//! ratio, strokes' widths included), antialiased. The CPU backend paints with
//! [`paint_path`] straight into the target; a GPU backend rasterizes the same path into a
//! small coverage mask ([`coverage`]) at the same transform, offset by whole pixels to the
//! [`ink_bounds`] it covers, so the two put the same coverage on the same pixels.
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
/// conservatively: its control points' hull (curves stay inside it), widened by half the
/// stroke in the larger ratio and one pixel of antialiasing. `None` for an empty or non-finite
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
    let pad = stroke.map_or(0.0, |width| width * sx.max(sy) / 2.0) + 1.0;
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
            for stroke in [None, Some(1.5)] {
                let path = icon();
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
}
