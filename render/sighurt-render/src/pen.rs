//! The pen a chrome draws with: [`Pen`] records a [`Scene`] for one surface, in window-logical
//! coordinates, as the chrome lays out. It draws nothing itself; a backend renders the scene
//! (`crate::backend`).
//!
//! Rectangles and rounded rectangles are snapped to buffer pixels when rendered, so controls
//! stay crisp at fractional scales and the hit test stays on what is drawn. Text is drawn from
//! [`ShapedText`] that [`crate::Fonts`] shaped in logical pixels; its glyphs are rasterized at
//! the target's scale by the backend, from the same shared font system.
//!
//! Must not own: pixels, glyph images or caches (a backend's), fonts (the shared font system and
//! the chrome's [`crate::Fonts`]), or what is drawn (the chrome's).

use sighurt_api::geometry::WindowRect;

use crate::color::Rgba;
use crate::scene::{
    FontRef, Glyph, GlyphFlags, GlyphRun, Item, Kind, PathBuilder, PathId, Provenance, Rect, RunId,
    Scene,
};
use crate::text::ShapedText;

/// Records what one surface shows in one frame. What falls outside the surface's rectangle is
/// cut off when the scene is rendered.
#[derive(Debug)]
pub struct Pen {
    rect: WindowRect,
    scene: Scene,
    /// What [`Pen::clip`] cuts everything to now.
    clip: Option<Rect>,
}

impl Pen {
    /// A pen for a surface at `rect`, window-logical, with an empty scene on opaque black.
    pub fn new(rect: WindowRect) -> Self {
        Self {
            rect,
            scene: Scene::default(),
            clip: None,
        }
    }

    /// The surface's rectangle, window-logical.
    pub fn rect(&self) -> WindowRect {
        self.rect
    }

    /// The scene drawn.
    #[must_use]
    pub fn finish(self) -> Scene {
        self.scene
    }

    /// Fills the whole surface with `color`, made opaque: chrome surfaces are opaque. What was
    /// drawn before is gone.
    pub fn clear(&mut self, color: Rgba) {
        self.scene = Scene {
            clear: color.opaque(),
            ..Scene::default()
        };
    }

    /// Fills `rect` (`[x, y, width, height]`), its edges snapped to the buffer's pixels.
    pub fn fill(&mut self, rect: [f32; 4], color: Rgba) {
        let rect = rect.into();
        self.push(Kind::Rect { rect, color }, None);
    }

    /// Fills `rect` with corners of `radius` logical pixels, its edges snapped.
    pub fn fill_rounded(&mut self, rect: [f32; 4], radius: f32, color: Rgba) {
        let rect = rect.into();
        self.push(
            Kind::RoundedRect {
                rect,
                radius,
                color,
            },
            None,
        );
    }

    /// A rounded rectangle with a one-pixel border: `border` around a `fill` inside.
    pub fn bordered(&mut self, rect: [f32; 4], radius: f32, fill: Rgba, border: Rgba) {
        let rect = rect.into();
        let kind = Kind::Bordered {
            rect,
            radius,
            fill,
            border,
        };
        self.push(kind, None);
    }

    /// Draws what `draw` draws cut to `rect` (`[x, y, width, height]`, snapped like
    /// [`Pen::fill`]), within any clip already in force: clips nest by intersecting.
    pub fn clip(&mut self, rect: [f32; 4], draw: impl FnOnce(&mut Pen)) {
        let outer = self.clip;
        let rect = Rect::from(rect);
        self.clip = Some(outer.map_or(rect, |outer| outer.intersect(rect)));
        draw(self);
        self.clip = outer;
    }

    /// Strokes the lines `build` makes (in window-logical coordinates) `width` logical pixels
    /// wide, with round caps.
    pub fn stroke(&mut self, width: f32, color: Rgba, build: impl FnOnce(&mut PathBuilder)) {
        let path = self.path(build);
        let kind = Kind::Path {
            path,
            color,
            stroke: Some(width),
        };
        self.push(kind, None);
    }

    /// Fills the shape `build` makes (in window-logical coordinates).
    pub fn shape(&mut self, color: Rgba, build: impl FnOnce(&mut PathBuilder)) {
        let path = self.path(build);
        let kind = Kind::Path {
            path,
            color,
            stroke: None,
        };
        self.push(kind, None);
    }

    /// Draws `text` with its top-left corner at `(x, y)`, cut to `clip`. Baselines fall on
    /// whole buffer pixels, so text stays sharp vertically; horizontally, glyphs keep their
    /// subpixel positions.
    pub fn text(&mut self, text: &ShapedText, (x, y): (f32, f32), color: Rgba, clip: [f32; 4]) {
        self.scene.fonts = match self.scene.fonts {
            Provenance::None => Provenance::One(text.font_system()),
            Provenance::One(id) if id == text.font_system() => Provenance::One(id),
            _ => Provenance::Mixed,
        };
        for line in text.runs() {
            if line.glyphs.is_empty() {
                continue;
            }
            let glyphs = line.glyphs.iter().map(|glyph| Glyph {
                font: FontRef(glyph.font_id),
                glyph: glyph.glyph_id,
                // As cosmic-text's `LayoutGlyph::physical` adds them.
                x: glyph.x + glyph.font_size * glyph.x_offset,
                y: glyph.y - glyph.font_size * glyph.y_offset,
                font_size: glyph.font_size,
                flags: GlyphFlags(glyph.cache_key_flags),
            });
            let run = RunId(self.scene.runs.len());
            self.scene.runs.push(GlyphRun {
                origin: (x, y),
                line_y: line.line_y,
                glyphs: glyphs.collect(),
            });
            self.push(Kind::Text { run, color }, Some(clip.into()));
        }
    }

    fn path(&mut self, build: impl FnOnce(&mut PathBuilder)) -> PathId {
        let mut builder = PathBuilder::new();
        build(&mut builder);
        let id = PathId(self.scene.paths.len());
        self.scene.paths.push(builder.finish());
        id
    }

    fn push(&mut self, kind: Kind, clip: Option<Rect>) {
        let clip = match (self.clip, clip) {
            (Some(outer), Some(clip)) => Some(outer.intersect(clip)),
            (outer, clip) => outer.or(clip),
        };
        self.scene.items.push(Item { kind, clip });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::tests::fonts;
    use crate::text::TextStyle;

    #[test]
    fn clearing_is_opaque_and_starts_over() {
        let mut pen = Pen::new(WindowRect::default());
        pen.fill([0.0, 0.0, 4.0, 4.0], Rgba::rgb(0xff0000));
        pen.clear(Rgba::rgb_alpha(0x11223300));
        pen.fill([1.0, 1.0, 2.0, 2.0], Rgba::rgb(0x00ff00));
        let scene = pen.finish();
        assert_eq!(scene.clear, Rgba::rgb(0x112233));
        assert_eq!(scene.items.len(), 1);
    }

    #[test]
    fn clips_nest_by_intersecting_and_end_with_their_scope() {
        let mut pen = Pen::new(WindowRect::default());
        pen.clip([0.0, 0.0, 10.0, 10.0], |pen| {
            pen.fill([1.0, 1.0, 2.0, 2.0], Rgba::rgb(0xff0000));
            pen.clip([5.0, 5.0, 10.0, 10.0], |pen| {
                pen.fill([1.0, 1.0, 2.0, 2.0], Rgba::rgb(0xff0000));
            });
        });
        pen.fill([1.0, 1.0, 2.0, 2.0], Rgba::rgb(0xff0000));
        let clips: Vec<_> = pen.finish().items.iter().map(|item| item.clip).collect();
        assert_eq!(
            clips,
            [
                Some([0.0, 0.0, 10.0, 10.0].into()),
                Some([5.0, 5.0, 5.0, 5.0].into()),
                None
            ]
        );
    }

    #[test]
    fn a_scene_knows_its_font_system() {
        let mut ours = fonts();
        let theirs = fonts();
        let mut pen = Pen::new(WindowRect::default());
        assert!(
            pen.scene.shaped_with(theirs.handle().id()),
            "no text: any system"
        );
        let line = ours.line("Hej", TextStyle::SM);
        pen.text(
            &line,
            (0.0, 0.0),
            Rgba::rgb(0xffffff),
            [0.0, 0.0, 50.0, 20.0],
        );
        let scene = pen.finish();
        assert!(scene.shaped_with(ours.handle().id()));
        assert!(!scene.shaped_with(theirs.handle().id()));
    }

    #[test]
    fn text_is_one_run_per_line_with_its_clip() {
        let mut fonts = fonts();
        let shaped = fonts.wrapped("Blåbär och sylt", TextStyle::SM, 40.0);
        let lines = shaped.runs().count();
        assert!(lines > 1);
        let mut pen = Pen::new(WindowRect::default());
        pen.text(
            &shaped,
            (3.0, 4.0),
            Rgba::rgb(0xffffff),
            [0.0, 0.0, 50.0, 60.0],
        );
        let scene = pen.finish();
        assert_eq!(scene.items.len(), lines);
        for item in &scene.items {
            let Kind::Text { run, .. } = item.kind else {
                panic!("{item:?}");
            };
            assert_eq!(scene.run(run).origin, (3.0, 4.0));
            assert_eq!(item.clip, Some([0.0, 0.0, 50.0, 60.0].into()));
        }
        // Shaping the same text again records the same scene.
        let mut again = Pen::new(WindowRect::default());
        again.text(
            &shaped,
            (3.0, 4.0),
            Rgba::rgb(0xffffff),
            [0.0, 0.0, 50.0, 60.0],
        );
        assert_eq!(again.finish(), scene);
    }
}
