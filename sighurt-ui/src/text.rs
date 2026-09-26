//! Fonts and shaped text: font discovery, shaping and fallback with cosmic-text, a bounded
//! cache of shaped lines, and where each offset of a shaped line sits.
//!
//! Text is shaped once in logical pixels. The painter rasterizes the same glyphs at the
//! target's scale (see [`crate::paint_cpu`]), so caret positions, hit testing and the drawn
//! glyphs agree at every scale.
//!
//! Must not own: glyph images (the painter's cache) or what the text says.

use std::collections::HashMap;
use std::rc::Rc;

use cosmic_text::{fontdb, Attrs, Buffer, Family, FontSystem, LayoutGlyph, Shaping, Weight, Wrap};

/// Shaped lines kept before the cache starts over.
const MAX_SHAPED: usize = 256;
/// Bytes of text in the cache before it starts over. A shaped character costs a few hundred
/// bytes, so this bounds the cache to tens of megabytes even for long addresses.
const MAX_SHAPED_BYTES: usize = 128 * 1024;

/// Families the chrome prefers, in order, when using the system's fonts. The first one
/// installed is the chrome's font; others fill in through fallback.
const PREFERRED_FAMILIES: [&str; 7] = [
    "Inter",
    "Cantarell",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Open Sans",
    "FreeSans",
];

/// How a piece of chrome text is set: its size and line height in logical pixels, and weight.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TextStyle {
    /// Font size.
    pub size: u16,
    /// Line height; the text is centred in it.
    pub line_height: u16,
    /// Font weight, 100 to 900.
    pub weight: u16,
}

impl TextStyle {
    /// 12 on 16: the status line, captions.
    pub const XS: TextStyle = TextStyle::new(12, 16);
    /// 14 on 20: tabs, the address line, most text.
    pub const SM: TextStyle = TextStyle::new(14, 20);
    /// 20 on 28, bold: headings of the chrome's pages.
    pub const HEADING: TextStyle = TextStyle::new(20, 28).weight(700);

    const fn new(size: u16, line_height: u16) -> Self {
        Self {
            size,
            line_height,
            weight: 400,
        }
    }

    /// The same style with another weight.
    pub const fn weight(self, weight: u16) -> Self {
        Self { weight, ..self }
    }
}

/// The chrome's fonts and its cache of shaped text.
pub struct Fonts {
    system: FontSystem,
    /// The chrome's font family; `None` leaves the choice to the font database's sans-serif.
    family: Option<String>,
    shaped: HashMap<ShapeKey, Rc<ShapedText>>,
    shaped_bytes: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ShapeKey {
    text: String,
    style: TextStyle,
    /// The wrap width's bits, or `None` for one line.
    wrap: Option<u32>,
}

impl Fonts {
    /// The system's fonts, found through fontconfig's configuration, with the first installed
    /// family of a short preference list as the chrome's font.
    ///
    /// This reads every installed font's metadata, so do it once: it took 3 ms with the fonts
    /// of the machine it was written on, and grows with the number of fonts installed.
    pub fn system() -> Self {
        let system = FontSystem::new();
        let family = PREFERRED_FAMILIES
            .into_iter()
            .find(|family| has_family(system.db(), family))
            .map(str::to_owned);
        Self::with_family(system, family)
    }

    /// Only the given font files, with the first font's family as the chrome's font and a fixed
    /// locale, so text renders the same on every machine. For tests and bundled fonts.
    pub fn from_data(fonts: impl IntoIterator<Item = Vec<u8>>) -> Self {
        let mut db = fontdb::Database::new();
        for font in fonts {
            db.load_font_data(font);
        }
        let family = db
            .faces()
            .next()
            .and_then(|face| face.families.first())
            .map(|(name, _)| name.clone());
        Self::with_family(
            FontSystem::new_with_locale_and_db("en-US".into(), db),
            family,
        )
    }

    fn with_family(mut system: FontSystem, family: Option<String>) -> Self {
        if let Some(family) = &family {
            system.db_mut().set_sans_serif_family(family.clone());
        }
        Self {
            system,
            family,
            shaped: HashMap::new(),
            shaped_bytes: 0,
        }
    }

    /// The chrome's font family, if one of the preferred ones (or the given fonts) had it.
    pub fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }

    /// The font system, for rasterizing glyphs of text this shaped.
    pub(crate) fn font_system(&mut self) -> &mut FontSystem {
        &mut self.system
    }

    /// `text` shaped on one line.
    pub(crate) fn line(&mut self, text: &str, style: TextStyle) -> Rc<ShapedText> {
        self.shape(text, style, None)
    }

    /// `text` shaped and wrapped at word boundaries (or anywhere, for a long word) to `width`.
    pub(crate) fn wrapped(&mut self, text: &str, style: TextStyle, width: f32) -> Rc<ShapedText> {
        self.shape(text, style, Some(width.max(1.0)))
    }

    /// `text` on one line, cut with an ellipsis if it is wider than `width`.
    pub(crate) fn fitted(&mut self, text: &str, style: TextStyle, width: f32) -> Rc<ShapedText> {
        let line = self.line(text, style);
        if line.width <= width {
            return line;
        }
        let room = width - self.line("\u{2026}", style).width;
        // The glyphs are in visual order; cut after the last one that still fits. Right-to-left
        // text is cut approximately, which is enough for a tab title.
        let end = line
            .glyphs()
            .filter(|glyph| glyph.x + glyph.w <= room)
            .map(|glyph| glyph.end)
            .max()
            .unwrap_or(0);
        let end = crate::text_edit::snap(text, end);
        self.line(&format!("{}\u{2026}", text[..end].trim_end()), style)
    }

    fn shape(&mut self, text: &str, style: TextStyle, wrap: Option<f32>) -> Rc<ShapedText> {
        let key = ShapeKey {
            text: text.to_owned(),
            style,
            wrap: wrap.map(f32::to_bits),
        };
        if let Some(shaped) = self.shaped.get(&key) {
            return shaped.clone();
        }
        let metrics = cosmic_text::Metrics::new(style.size.into(), style.line_height.into());
        let system = &mut self.system;
        let mut buffer = Buffer::new(system, metrics);
        let lines = match wrap {
            Some(_) => Wrap::WordOrGlyph,
            None => Wrap::None,
        };
        buffer.set_wrap(system, lines);
        buffer.set_size(system, wrap, None);
        let family = match &self.family {
            Some(name) => Family::Name(name),
            None => Family::SansSerif,
        };
        let attrs = Attrs::new().family(family).weight(Weight(style.weight));
        // Shapes and lays out every line, since the height is unbounded.
        buffer.set_text(system, text, &attrs, Shaping::Advanced);
        let shaped = Rc::new(ShapedText::new(buffer, style));

        // Text too big for the budget isn't kept at all.
        if text.len() <= MAX_SHAPED_BYTES {
            if self.shaped.len() >= MAX_SHAPED || self.shaped_bytes + text.len() > MAX_SHAPED_BYTES
            {
                self.shaped.clear();
                self.shaped_bytes = 0;
            }
            self.shaped_bytes += text.len();
            self.shaped.insert(key, shaped.clone());
        }
        shaped
    }
}

fn has_family(db: &fontdb::Database, family: &str) -> bool {
    db.faces()
        .any(|face| face.families.iter().any(|(name, _)| name == family))
}

/// Text laid out in logical pixels, with its top-left corner at the origin.
pub(crate) struct ShapedText {
    buffer: Buffer,
    /// The widest line's width.
    pub width: f32,
    /// The height of all lines.
    pub height: f32,
}

impl ShapedText {
    fn new(buffer: Buffer, style: TextStyle) -> Self {
        let (mut width, mut lines) = (0.0f32, 0);
        for run in buffer.layout_runs() {
            width = width.max(run.line_w);
            lines += 1;
        }
        Self {
            buffer,
            width,
            height: f32::from(style.line_height) * lines.max(1) as f32,
        }
    }

    /// The laid-out lines.
    pub fn runs(&self) -> cosmic_text::LayoutRunIter<'_> {
        self.buffer.layout_runs()
    }

    /// The first line's glyphs, in visual order.
    fn glyphs(&self) -> impl Iterator<Item = &LayoutGlyph> {
        self.buffer.layout_runs().take(1).flat_map(|run| run.glyphs)
    }

    /// Where the caret goes for `offset` into the first line: before the character that starts
    /// there, or after the one that ends there. Inside a cluster that covers several characters
    /// (a ligature), the position is interpolated.
    pub fn x_at(&self, offset: usize) -> f32 {
        let edge = |glyph: &LayoutGlyph, at_start: bool| match glyph.level.is_rtl() == at_start {
            true => glyph.x + glyph.w,
            false => glyph.x,
        };
        let mut after = None;
        for glyph in self.glyphs() {
            if glyph.start == offset {
                return edge(glyph, true);
            }
            if glyph.start < offset && offset < glyph.end {
                let fraction = (offset - glyph.start) as f32 / (glyph.end - glyph.start) as f32;
                let advance = fraction * glyph.w;
                return match glyph.level.is_rtl() {
                    true => glyph.x + glyph.w - advance,
                    false => glyph.x + advance,
                };
            }
            if glyph.end == offset {
                after.get_or_insert(edge(glyph, false));
            }
        }
        after.unwrap_or(0.0)
    }

    /// The offset into the first line whose caret position is nearest to `x`: the start or end
    /// of the cluster under `x`, or of the text when `x` is beyond it.
    pub fn offset_at(&self, x: f32) -> usize {
        let (mut left, mut right) = (f32::INFINITY, f32::NEG_INFINITY);
        let (mut left_offset, mut right_offset) = (0, 0);
        for glyph in self.glyphs() {
            if glyph.x <= x && x < glyph.x + glyph.w {
                let before = x < glyph.x + glyph.w / 2.0;
                return match before != glyph.level.is_rtl() {
                    true => glyph.start,
                    false => glyph.end,
                };
            }
            if glyph.x < left {
                left = glyph.x;
                left_offset = if glyph.level.is_rtl() {
                    glyph.end
                } else {
                    glyph.start
                };
            }
            if glyph.x + glyph.w > right {
                right = glyph.x + glyph.w;
                right_offset = if glyph.level.is_rtl() {
                    glyph.start
                } else {
                    glyph.end
                };
            }
        }
        if x < left {
            left_offset
        } else {
            right_offset
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The bundled test font (Cantarell Regular, a Latin subset; see tests/fonts/README.md).
    pub(crate) const TEST_FONT: &[u8] = include_bytes!("../tests/fonts/Cantarell-Regular.ttf");

    pub(crate) fn fonts() -> Fonts {
        Fonts::from_data([TEST_FONT.to_vec()])
    }

    #[test]
    fn test_font_is_the_chrome_font() {
        assert_eq!(fonts().family(), Some("Cantarell"));
    }

    #[test]
    fn carets_follow_the_glyphs() {
        let mut fonts = fonts();
        let text = "Blåbär";
        let line = fonts.line(text, TextStyle::SM);
        assert!(line.width > 30.0 && line.width < 60.0, "{}", line.width);
        // Each character boundary is further right than the one before, and the end is the
        // line's width.
        let boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let xs: Vec<f32> = boundaries.iter().map(|&i| line.x_at(i)).collect();
        assert_eq!(xs[0], 0.0);
        assert!(xs.windows(2).all(|pair| pair[0] < pair[1]), "{xs:?}");
        assert!((line.x_at(text.len()) - line.width).abs() < 0.01);
        // Clicking a caret position finds that offset again, and beyond the ends finds the ends.
        for (&offset, &x) in boundaries.iter().zip(&xs) {
            assert_eq!(line.offset_at(x + 0.5), offset, "at {x}");
        }
        assert_eq!(line.offset_at(-10.0), 0);
        assert_eq!(line.offset_at(line.width + 10.0), text.len());
        assert_eq!(fonts.line("", TextStyle::SM).offset_at(5.0), 0);
    }

    #[test]
    fn long_text_is_cut_with_an_ellipsis() {
        let mut fonts = fonts();
        let fitted = fonts.fitted("Sighurt is a small browser", TextStyle::SM, 80.0);
        assert!(fitted.width <= 80.0, "{}", fitted.width);
        let text: String = fitted.runs().next().unwrap().text.into();
        assert!(text.ends_with('\u{2026}'), "{text}");
        assert!(text.starts_with("Sighurt"), "{text}");
        // Short text stays whole.
        let whole = fonts.fitted("Home", TextStyle::SM, 80.0);
        assert_eq!(whole.runs().next().unwrap().text, "Home");
    }

    #[test]
    fn the_cache_is_bounded() {
        let mut fonts = fonts();
        for i in 0..MAX_SHAPED * 2 {
            fonts.line(&i.to_string(), TextStyle::XS);
            assert!(fonts.shaped.len() <= MAX_SHAPED);
        }
        fonts.line(&"x".repeat(MAX_SHAPED_BYTES + 1), TextStyle::XS);
        assert!(fonts.shaped_bytes <= MAX_SHAPED_BYTES);
        fonts.line(&"y".repeat(MAX_SHAPED_BYTES / 2 + 1), TextStyle::XS);
        fonts.line(&"z".repeat(MAX_SHAPED_BYTES / 2 + 1), TextStyle::XS);
        assert!(fonts.shaped_bytes <= MAX_SHAPED_BYTES);
    }
}
