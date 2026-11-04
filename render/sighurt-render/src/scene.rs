//! The scene: what one surface shows in one frame, as plain data, and the geometry every
//! backend shares to put it on the pixel grid.
//!
//! A [`Scene`] is a clear color and a list of [`Item`]s in draw order, in window-logical
//! coordinates (as the chrome lays out), each cut to an optional clip rectangle. It is `Clone`
//! and `PartialEq`, holds no handle and no lifetime: it can be compared with the previous
//! frame's, rendered by any backend and asserted on in tests. [`crate::Pen`] builds it.
//!
//! A backend maps it onto its target with a [`Mapping`], made from the target's `Metrics`:
//! the surface's window-logical origin, the buffer pixels per logical pixel from the actual
//! buffer size (scale is a rational; the rounded buffer decides), and the nominal scale for
//! text, radii and the one-pixel border. Paths, their strokes' widths included, map with the
//! actual ratio ([`Mapping::path_transform`], rasterized by `crate::shape`). Every backend
//! snaps with the same functions here, so a control has the same pixels under each, and the hit
//! test, which uses the logical rectangles, stays on what is drawn:
//!
//! - [`Mapping::snap`]: each edge to the first pixel whose centre is at or after it, so a
//!   rectangle covers exactly the pixels whose centres lie inside it;
//! - [`Mapping::bordered`]: a [`Kind::Bordered`] item's outer and inner rectangles, its border
//!   one pixel per whole scale step (`scale.round().max(1)`);
//! - [`glyph_physical`]: where a glyph goes and which rasterized image it needs. Baselines fall
//!   on whole buffer pixels; glyphs keep subpixel x positions (cosmic-text's bins).
//!
//! Must not own: pixels, GPU objects or anything of a target (the backends'), what is drawn
//! (the chrome's), or shaping (`crate::text`).

use cosmic_text::{fontdb, CacheKey, CacheKeyFlags};
use sighurt_api::geometry::Metrics;

use crate::color::Rgba;
use crate::text::FontSystemId;

/// The drawing of one surface for one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    /// What the frame starts from, opaque. Every frame clears: a buffer may hold an older one.
    pub clear: Rgba,
    /// What is drawn over it, in order.
    pub items: Vec<Item>,
    /// Glyph runs, referenced by [`Kind::Text`].
    pub(crate) runs: Vec<GlyphRun>,
    /// Paths, referenced by [`Kind::Path`].
    pub(crate) paths: Vec<Path>,
    /// The font system its text was shaped with.
    pub(crate) fonts: Provenance,
}

/// Which font system a scene's glyphs belong to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Provenance {
    /// It has no text.
    #[default]
    None,
    /// All of its text was shaped with this system.
    One(FontSystemId),
    /// Its text came from more than one system: no backend can render it.
    Mixed,
}

impl Default for Scene {
    /// Nothing on opaque black.
    fn default() -> Self {
        Self {
            clear: Rgba::rgb(0x000000),
            items: Vec::new(),
            runs: Vec::new(),
            paths: Vec::new(),
            fonts: Provenance::None,
        }
    }
}

impl Scene {
    /// The glyph run `id` names.
    pub fn run(&self, id: RunId) -> &GlyphRun {
        &self.runs[id.0]
    }

    /// The path `id` names.
    pub fn path(&self, id: PathId) -> &Path {
        &self.paths[id.0]
    }

    /// Whether its glyphs can be rasterized from the font system `fonts`: it has no text, or
    /// all of it was shaped there. A glyph's font is an id in one system's database, so a
    /// backend refuses a scene shaped with another.
    pub fn shaped_with(&self, fonts: FontSystemId) -> bool {
        match self.fonts {
            Provenance::None => true,
            Provenance::One(id) => id == fonts,
            Provenance::Mixed => false,
        }
    }
}

/// One thing drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Item {
    /// What it is.
    pub kind: Kind,
    /// Cut to this window-logical rectangle, snapped like a [`Kind::Rect`]; `None` is the
    /// surface.
    pub clip: Option<Rect>,
}

/// What an item draws. Every backend is in the workspace, so this is exhaustive: a new kind
/// is added to each of them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// A rectangle, each edge snapped ([`Mapping::snap`]).
    Rect {
        /// Where.
        rect: Rect,
        /// Its color.
        color: Rgba,
    },
    /// A rectangle with rounded corners of `radius` logical pixels, snapped like `Rect`.
    RoundedRect {
        /// Where.
        rect: Rect,
        /// The corners' radius.
        radius: f32,
        /// Its color.
        color: Rgba,
    },
    /// A rounded rectangle with a one-pixel border ([`Mapping::bordered`]): `border` around
    /// `fill`.
    Bordered {
        /// Where, the border included.
        rect: Rect,
        /// The outer corners' radius.
        radius: f32,
        /// Inside the border.
        fill: Rgba,
        /// The border.
        border: Rgba,
    },
    /// A filled path, or a stroked one `stroke` logical pixels wide with round caps (icons).
    /// Window-logical coordinates, unsnapped.
    Path {
        /// The path.
        path: PathId,
        /// Its color.
        color: Rgba,
        /// The stroke's width, or `None` to fill (non-zero winding).
        stroke: Option<f32>,
    },
    /// Positioned glyphs in one color.
    Text {
        /// The glyphs.
        run: RunId,
        /// Their color.
        color: Rgba,
    },
}

/// A rectangle in window-logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl Rect {
    /// The part of `self` inside `other`: empty (zero width or height) where they don't meet.
    pub fn intersect(self, other: Rect) -> Rect {
        let left = self.x.max(other.x);
        let top = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        Rect {
            x: left,
            y: top,
            width: (right - left).max(0.0),
            height: (bottom - top).max(0.0),
        }
    }
}

impl From<[f32; 4]> for Rect {
    /// `[x, y, width, height]`.
    fn from([x, y, width, height]: [f32; 4]) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

/// A glyph run of a scene: an index into [`Scene::run`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RunId(pub(crate) usize);

/// A path of a scene: an index into [`Scene::path`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PathId(pub(crate) usize);

/// One line of shaped text, placed: what cosmic-text's layout run says, minus the scale, which
/// the backend applies with [`glyph_physical`].
#[derive(Clone, Debug, PartialEq)]
pub struct GlyphRun {
    /// The window-logical point the text's top-left corner was drawn at.
    pub origin: (f32, f32),
    /// The line's baseline, logical pixels below `origin`.
    pub line_y: f32,
    /// Its glyphs, in visual order.
    pub glyphs: Vec<Glyph>,
}

/// One positioned glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    /// The font, in the font system the scene was shaped with.
    pub font: FontRef,
    /// The glyph in that font.
    pub glyph: u16,
    /// Logical offset from the run's origin to the glyph's origin, its shaping offset included.
    pub x: f32,
    /// Logical offset above the baseline (cosmic-text's `y` less its shaping offset).
    pub y: f32,
    /// Font size in logical pixels, as shaped.
    pub font_size: f32,
    /// How to rasterize it (fake italic and the like).
    pub flags: GlyphFlags,
}

/// A font of the shared font system ([`crate::text::FontSystemHandle`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FontRef(pub(crate) fontdb::ID);

/// Rasterization flags of a glyph, as its shaping chose them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphFlags(pub(crate) CacheKeyFlags);

/// A path: what [`PathBuilder`] recorded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Path {
    /// The commands, in order.
    pub commands: Vec<PathCommand>,
}

/// One step of a path, in window-logical coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PathCommand {
    /// Starts a contour at `(x, y)`.
    MoveTo(f32, f32),
    /// A line to `(x, y)`.
    LineTo(f32, f32),
    /// A quadratic curve through control point `(x1, y1)` to `(x, y)`.
    QuadTo(f32, f32, f32, f32),
    /// A cubic curve through `(x1, y1)` and `(x2, y2)` to `(x, y)`.
    CubicTo(f32, f32, f32, f32, f32, f32),
    /// Closes the contour.
    Close,
}

/// Records a path for [`crate::Pen::stroke`] and [`crate::Pen::shape`], in window-logical
/// coordinates. Each method behaves as tiny-skia's `PathBuilder` method of that name, which
/// the CPU backend replays.
#[derive(Debug, Default)]
pub struct PathBuilder {
    commands: Vec<PathCommand>,
}

impl PathBuilder {
    /// An empty path.
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a contour at `(x, y)`.
    pub fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::MoveTo(x, y));
    }

    /// A line to `(x, y)`.
    pub fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::LineTo(x, y));
    }

    /// A quadratic curve through `(x1, y1)` to `(x, y)`.
    pub fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.commands.push(PathCommand::QuadTo(x1, y1, x, y));
    }

    /// A cubic curve through `(x1, y1)` and `(x2, y2)` to `(x, y)`.
    pub fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.commands
            .push(PathCommand::CubicTo(x1, y1, x2, y2, x, y));
    }

    /// Closes the contour.
    pub fn close(&mut self) {
        self.commands.push(PathCommand::Close);
    }

    /// A closed circle of radius `r` around `(x, y)`, clockwise; nothing for a negative radius.
    /// Recorded as the curves tiny-skia makes of it.
    pub fn push_circle(&mut self, x: f32, y: f32, r: f32) {
        let Some(circle) = tiny_skia::PathBuilder::from_circle(x, y, r) else {
            return;
        };
        for segment in circle.segments() {
            self.commands.push(match segment {
                tiny_skia::PathSegment::MoveTo(p) => PathCommand::MoveTo(p.x, p.y),
                tiny_skia::PathSegment::LineTo(p) => PathCommand::LineTo(p.x, p.y),
                tiny_skia::PathSegment::QuadTo(c, p) => PathCommand::QuadTo(c.x, c.y, p.x, p.y),
                tiny_skia::PathSegment::CubicTo(c1, c2, p) => {
                    PathCommand::CubicTo(c1.x, c1.y, c2.x, c2.y, p.x, p.y)
                }
                tiny_skia::PathSegment::Close => PathCommand::Close,
            });
        }
    }

    /// The path recorded.
    pub(crate) fn finish(self) -> Path {
        Path {
            commands: self.commands,
        }
    }
}

/// A rectangle in buffer pixels, its edges on the pixel grid after [`Mapping::snap`]. Valid:
/// finite, `left <= right`, `top <= bottom`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelRect {
    /// Left edge.
    pub left: f32,
    /// Top edge.
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge.
    pub bottom: f32,
}

impl PixelRect {
    /// The rectangle with these edges, if they make one: finite, in order, with a finite size.
    pub fn from_ltrb(left: f32, top: f32, right: f32, bottom: f32) -> Option<Self> {
        let finite = [left, top, right, bottom, right - left, bottom - top]
            .iter()
            .all(|v| v.is_finite());
        (finite && left <= right && top <= bottom).then_some(Self {
            left,
            top,
            right,
            bottom,
        })
    }
}

/// A [`Kind::Bordered`] item on the pixel grid.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BorderedPixels {
    /// The whole item, filled with the border's color.
    pub outer: PixelRect,
    /// Its corners' radius in buffer pixels.
    pub outer_radius: f32,
    /// The inside, filled with the fill color, and its radius: `None` when the border leaves
    /// no room.
    pub inner: Option<(PixelRect, f32)>,
}

/// How a scene's window-logical coordinates map onto one target's buffer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mapping {
    /// The target's window-logical origin.
    origin: (f32, f32),
    /// Buffer pixels per logical pixel, from the actual buffer size.
    ratio: (f32, f32),
    /// The nominal scale, for text, radii and borders (paths use `ratio`).
    scale: f32,
}

impl Mapping {
    /// The mapping onto a target with `metrics`. A target with empty metrics has no buffer to
    /// map onto; its ratio is meaningless and nothing should be drawn.
    pub fn new(metrics: &Metrics) -> Self {
        let target = metrics.rect();
        let buffer = metrics.buffer();
        Self {
            origin: (target.x as f32, target.y as f32),
            ratio: (
                buffer.width as f32 / target.width as f32,
                buffer.height as f32 / target.height as f32,
            ),
            scale: metrics.scale().factor() as f32,
        }
    }

    /// The target's window-logical origin.
    pub fn origin(&self) -> (f32, f32) {
        self.origin
    }

    /// Buffer pixels per logical pixel, horizontally and vertically.
    pub fn ratio(&self) -> (f32, f32) {
        self.ratio
    }

    /// The nominal scale.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Window-logical `x` in buffer pixels, snapped to the pixel grid.
    pub fn x(&self, x: f32) -> f32 {
        snap((x - self.origin.0) * self.ratio.0)
    }

    /// Window-logical `y` in buffer pixels, snapped to the pixel grid.
    pub fn y(&self, y: f32) -> f32 {
        snap((y - self.origin.1) * self.ratio.1)
    }

    /// `rect` in buffer pixels, each edge snapped: exactly the pixels whose centres lie inside
    /// it. `None` if that makes no rectangle.
    pub fn snap(&self, rect: Rect) -> Option<PixelRect> {
        let Rect {
            x,
            y,
            width,
            height,
        } = rect;
        PixelRect::from_ltrb(self.x(x), self.y(y), self.x(x + width), self.y(y + height))
    }

    /// The width of a one-pixel line: one buffer pixel per whole scale step.
    pub fn line(&self) -> f32 {
        self.scale.round().max(1.0)
    }

    /// A [`Kind::Bordered`] item of `rect` with corners of `radius` on the pixel grid.
    pub fn bordered(&self, rect: Rect, radius: f32) -> Option<BorderedPixels> {
        let outer = self.snap(rect)?;
        let line = self.line();
        let radius = radius * self.scale;
        let inner = PixelRect::from_ltrb(
            outer.left + line,
            outer.top + line,
            outer.right - line,
            outer.bottom - line,
        );
        Some(BorderedPixels {
            outer,
            outer_radius: radius,
            inner: inner.map(|inner| (inner, radius - line)),
        })
    }

    /// The affine transform of paths, unsnapped, as tiny-skia's `Transform::from_row` takes it:
    /// `(sx, ky, kx, sy, tx, ty)`.
    pub fn path_transform(&self) -> [f32; 6] {
        [
            self.ratio.0,
            0.0,
            0.0,
            self.ratio.1,
            -self.origin.0 * self.ratio.0,
            -self.origin.1 * self.ratio.1,
        ]
    }
}

/// The pixel edge for buffer position `v`: the first pixel whose centre is at or after it.
pub fn snap(v: f32) -> f32 {
    (v - 0.5).ceil()
}

/// Where `glyph` of `run` goes in the buffer, and the key of its rasterized image: returns the
/// buffer position of the glyph's origin (`x`, and `y` on the snapped baseline) and the key.
/// The run's origin maps with the actual ratio; offsets and the baseline scale with the nominal
/// scale, and the baseline is rounded to a whole pixel on its own. The image's own placement
/// (`left`, `top`) is relative to that position.
pub fn glyph_physical(mapping: &Mapping, run: &GlyphRun, glyph: &Glyph) -> (i32, i32, CacheKey) {
    let left = (run.origin.0 - mapping.origin.0) * mapping.ratio.0;
    let top = (run.origin.1 - mapping.origin.1) * mapping.ratio.1;
    let baseline = (top + run.line_y * mapping.scale).round() as i32;
    let (key, x, y) = CacheKey::new(
        glyph.font.0,
        glyph.glyph,
        glyph.font_size * mapping.scale,
        (
            glyph.x * mapping.scale + left,
            // Hinting in y, as cosmic-text's `LayoutGlyph::physical` does.
            (glyph.y * mapping.scale + 0.0).trunc(),
        ),
        glyph.flags.0,
    );
    (x, baseline + y, key)
}

#[cfg(test)]
mod tests {
    use sighurt_api::geometry::{Scale, WindowRect};
    use sighurt_api::ids::LayoutEpoch;

    use super::*;

    fn mapping(rect: WindowRect, scale: u32) -> Mapping {
        let scale = Scale::from_fractional(scale).unwrap();
        Mapping::new(&Metrics::new(LayoutEpoch::next(), rect, scale))
    }

    const RECT: WindowRect = WindowRect {
        x: 0,
        y: 37,
        width: 333,
        height: 41,
    };

    /// Snapped edges are whole pixels, a rectangle's pixels are those whose centres it covers,
    /// and adjacent rectangles share their edge: nothing is drawn twice or left out.
    #[test]
    fn snapping_covers_the_pixels_whose_centres_lie_inside() {
        for scale in [120, 150, 180, 240] {
            let map = mapping(RECT, scale);
            let (rx, _) = map.ratio();
            for x in [0.0, 0.3, 1.0, 7.5, 12.25, 100.0] {
                let left = map.x(x);
                assert_eq!(left, left.round());
                // The first pixel at or after the edge, by its centre.
                assert!(
                    left + 0.5 >= x * rx && left - 0.5 < x * rx,
                    "{x} at {scale}"
                );
            }
            let a = map.snap([10.0, 40.0, 5.5, 3.0].into()).unwrap();
            let b = map.snap([15.5, 40.0, 4.0, 3.0].into()).unwrap();
            assert_eq!(a.right, b.left, "at {scale}");
            assert_eq!(a.top, map.y(40.0));
        }
        let map = mapping(RECT, 120);
        assert_eq!(map.snap([5.0, 40.0, 0.0, 1.0].into()).unwrap().left, 5.0);
        assert_eq!(map.snap([5.0, 40.0, -3.0, 1.0].into()), None);
        assert_eq!(map.snap([f32::NAN, 40.0, 1.0, 1.0].into()), None);
    }

    /// The border is one pixel per whole scale step, and a border that fills the item leaves
    /// no inside.
    #[test]
    fn borders_are_whole_pixels() {
        let lines = [(120, 1.0), (150, 1.0), (180, 2.0), (240, 2.0)];
        for (scale, line) in lines {
            let map = mapping(RECT, scale);
            let bordered = map.bordered([10.0, 40.0, 50.0, 20.0].into(), 6.0).unwrap();
            let (inner, radius) = bordered.inner.unwrap();
            assert_eq!(inner.left - bordered.outer.left, line, "at {scale}");
            assert_eq!(bordered.outer_radius - radius, line);
            assert_eq!(bordered.outer_radius, 6.0 * map.scale());
        }
        let map = mapping(RECT, 240);
        let thin = map.bordered([10.0, 40.0, 1.0, 20.0].into(), 2.0).unwrap();
        assert_eq!(thin.inner, None);
    }

    /// `glyph_physical` places every glyph and keys its image as cosmic-text's own
    /// `LayoutGlyph::physical` does, for the run's origin in buffer pixels: combining marks
    /// (shaping offsets), negative bearings, fallback fonts and emoji, at fractional scales and
    /// odd buffer sizes. Uses the system's fonts; what they can't show is shaped by fallback
    /// or `.notdef` and compared all the same.
    #[test]
    fn glyphs_are_placed_as_cosmic_text_places_them() {
        let mut fonts = crate::Fonts::system();
        let texts = [
            "e\u{301}a\u{308}o\u{328}\u{302}",
            "fjord ƒ yj",
            "Blåbär 中文 Ωμέγα",
            "smile 😀 ok",
        ];
        let rect = WindowRect {
            x: 7,
            y: 3,
            width: 333,
            height: 41,
        };
        let mut glyphs = 0;
        for scale in [120, 150, 180, 240, 140] {
            let map = mapping(rect, scale);
            for text in texts {
                let shaped = fonts.line(text, crate::TextStyle::SM);
                let mut pen = crate::Pen::new(rect);
                let origin = (19.3, 8.0);
                pen.text(
                    &shaped,
                    origin,
                    Rgba::rgb(0xffffff),
                    [0.0, 0.0, 400.0, 60.0],
                );
                let scene = pen.finish();
                for (layout, item) in shaped.runs().zip(&scene.items) {
                    let Kind::Text { run, .. } = item.kind else {
                        panic!("{item:?}")
                    };
                    let run = scene.run(run);
                    let left = (origin.0 - map.origin.0) * map.ratio.0;
                    let top = (origin.1 - map.origin.1) * map.ratio.1;
                    let baseline = (top + layout.line_y * map.scale).round() as i32;
                    for (ours, theirs) in run.glyphs.iter().zip(layout.glyphs) {
                        let expected = theirs.physical((left, 0.0), map.scale);
                        let (x, y, key) = glyph_physical(&map, run, ours);
                        assert_eq!(key, expected.cache_key, "{text:?} at {scale}");
                        assert_eq!((x, y), (expected.x, baseline + expected.y), "{text:?}");
                        glyphs += 1;
                    }
                }
            }
        }
        assert!(glyphs > 100, "{glyphs}");
    }

    #[test]
    fn a_circle_is_recorded_as_tiny_skia_makes_it() {
        let mut path = PathBuilder::new();
        path.push_circle(10.0, 10.0, 3.0);
        let commands = path.finish().commands;
        assert_eq!(commands.first(), Some(&PathCommand::MoveTo(13.0, 10.0)));
        assert_eq!(commands.last(), Some(&PathCommand::Close));
        assert!(commands.len() > 4);
        let mut none = PathBuilder::new();
        none.push_circle(10.0, 10.0, -1.0);
        assert!(none.finish().commands.is_empty());
    }
}
