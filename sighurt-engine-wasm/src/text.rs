//! Guest text: measuring it for `api_canvas_measure_text`, and laying it out and rasterizing its
//! glyphs for the rasterizer.
//!
//! Both go through [`layout_line`], shaped with [cosmic-text](https://crates.io/crates/cosmic-text)
//! against the system fonts, so what a guest measures is exactly what gets painted. Fonts are
//! picked the way the pre-rewrite browser (GPUI on Linux) picked them: the requested family,
//! then a fixed list of fallbacks.
//!
//! Guests measure on worker threads and the rasterizer paints for the owner thread, so each has
//! a font system of its own, made from one scan of the system fonts: a guest measuring long
//! text never holds a lock that painting needs. Painting holds its lock to lay a line out and
//! find its glyphs' images, not to blend them, so bands of a frame blend text at once.
//!
//! Owns: the font systems, the measurement cache, the cache of lines laid out for painting and
//! the glyph cache, each bounded. Must not own: where text goes (the rasterizer).

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use cosmic_text::fontdb::{Database, Query};
use cosmic_text::{
    Attrs, AttrsList, CacheKey, Family, FontSystem, LayoutLine, ShapeLine, Shaping, Stretch, Style,
    SwashCache, SwashImage, Weight, Wrap,
};

/// The families GPUI tries, in about this order, for the browser's default font (`""`) and for
/// fonts that aren't installed: its system UI font on Linux, then its fallback stack.
const FALLBACKS: [&str; 10] = [
    "IBM Plex Sans",
    "Lilex",
    "Helvetica",
    "Segoe UI",
    "Ubuntu",
    "Adwaita Sans",
    "Cantarell",
    "Noto Sans",
    "DejaVu Sans",
    "Arial",
];

/// The system fonts and the locale, scanned on first use (which takes a moment).
static SYSTEM_FONTS: LazyLock<(String, Database)> = LazyLock::new(|| {
    let system = FontSystem::new();
    (system.locale().to_string(), system.db().clone())
});

/// Guests' measurements.
static MEASURING: LazyLock<Mutex<Measuring>> = LazyLock::new(|| {
    Mutex::new(Measuring {
        system: font_system(),
        measured: HashMap::new(),
    })
});

/// The rasterizer's text.
static PAINTING: LazyLock<Mutex<Painting>> = LazyLock::new(|| {
    Mutex::new(Painting {
        system: font_system(),
        glyphs: Glyphs::default(),
        lines: HashMap::new(),
        line_glyphs: 0,
    })
});

fn font_system() -> FontSystem {
    let (locale, db) = &*SYSTEM_FONTS;
    FontSystem::new_with_locale_and_db(locale.clone(), db.clone())
}

struct Measuring {
    system: FontSystem,
    /// Measurements of short lines, by a hash of their [`Key`], which is kept to tell keys with
    /// the same hash apart. Guests that lay out text measure the same strings every frame, and
    /// shaping them costs far more than the lookup.
    measured: HashMap<u64, (Key, [f32; 3])>,
}

struct Painting {
    system: FontSystem,
    glyphs: Glyphs,
    /// Lines of up to [`MAX_CACHED_TEXT`] bytes laid out, by a hash of their [`Key`] as in
    /// [`Measuring::measured`]: guests draw the same strings every frame, and shaping them again
    /// would cost most of a frame. At most 4096 lines and [`MAX_CACHED_GLYPHS`] glyphs.
    lines: HashMap<u64, (Key, Option<LayoutLine>)>,
    /// The glyphs `lines` holds.
    line_glyphs: usize,
}

/// What a measurement depends on: line, size bits, family, weight and italic.
type Key = (String, u32, String, u16, bool);

/// Longest line plus family name, in bytes, whose measurement is kept. Longer text is rare,
/// and would make the cache hold whatever megabytes a guest passes.
const MAX_CACHED_TEXT: usize = 1024;
/// Most glyphs of laid-out lines kept; past it the cache starts over. A laid-out glyph takes 80
/// bytes, so this is about 5 MiB, whatever lines a guest draws (and their keys at most 4 MiB).
const MAX_CACHED_GLYPHS: usize = 64 * 1024;
/// Most bytes of glyph images kept; past it the cache starts over.
const MAX_GLYPH_BYTES: usize = 32 * 1024 * 1024;

/// Rasterized glyphs, by size, position bin and face.
#[derive(Default)]
struct Glyphs {
    scaler: Option<SwashCache>,
    images: HashMap<CacheKey, Option<Arc<SwashImage>>>,
    bytes: usize,
}

impl Glyphs {
    fn get(&mut self, system: &mut FontSystem, key: CacheKey) -> Option<Arc<SwashImage>> {
        if !self.images.contains_key(&key) {
            if self.bytes > MAX_GLYPH_BYTES {
                self.images.clear();
                self.bytes = 0;
            }
            let scaler = self.scaler.get_or_insert_with(SwashCache::new);
            let image = scaler.get_image_uncached(system, key);
            self.bytes += image.as_ref().map_or(0, |image| image.data.len());
            self.images.insert(key, image.map(Arc::new));
        }
        self.images[&key].clone()
    }
}

fn lock<T>(fonts: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    fonts.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Width, ascent and descent in pixels of `text` laid out on one line at `size` pixels. Only its
/// first line is measured. An empty `family` means the default font.
pub fn measure(text: &str, size: f32, family: &str, weight: u16, italic: bool) -> [f32; 3] {
    let line = first_line(text);
    let mut fonts = lock(&MEASURING);
    if line.len() + family.len() > MAX_CACHED_TEXT {
        return metrics(layout_line(
            &mut fonts.system,
            line,
            size,
            family,
            weight,
            italic,
        ));
    }
    let key = (line, size.to_bits(), family, weight, italic);
    let hash = hash_key(&key);
    if let Some(((l, s, f, w, i), metrics)) = fonts.measured.get(&hash) {
        if (l.as_str(), *s, f.as_str(), *w, *i) == key {
            return *metrics;
        }
    }
    let metrics = metrics(layout_line(
        &mut fonts.system,
        line,
        size,
        family,
        weight,
        italic,
    ));
    // Bound the memory a guest measuring ever new strings can take.
    if fonts.measured.len() >= 4096 {
        fonts.measured.clear();
    }
    let key = (line.to_string(), key.1, family.to_string(), weight, italic);
    fonts.measured.insert(hash, (key, metrics));
    metrics
}

/// A line of text to draw: its first line, at `size` pixels, in the face `family` (the default
/// font when empty), `weight` and `italic` pick.
pub struct Line<'a> {
    pub text: &'a str,
    pub size: f32,
    pub family: &'a str,
    pub weight: u16,
    pub italic: bool,
}

/// Lays `line` out, asks `place` where the baseline starts given the line's width, ascent and
/// descent, and hands the image of every glyph that may reach into `area` (left, top, right,
/// bottom, in the same pixels) to `glyph` with the position of its top-left pixel. `glyph` runs
/// without the painting lock.
pub fn draw(
    line: Line<'_>,
    place: impl FnOnce([f32; 3]) -> (f32, f32),
    area: [f32; 4],
    mut glyph: impl FnMut(&SwashImage, i32, i32),
) {
    for (image, x, y) in glyphs(line, place, area) {
        glyph(&image, x, y);
    }
}

/// The glyphs [`draw`] hands over, found under the painting lock.
fn glyphs(
    line: Line<'_>,
    place: impl FnOnce([f32; 3]) -> (f32, f32),
    [left, top, right, bottom]: [f32; 4],
) -> Vec<(Arc<SwashImage>, i32, i32)> {
    let mut fonts = lock(&PAINTING);
    let fonts = &mut *fonts;
    let Line {
        text,
        size,
        family,
        weight,
        italic,
    } = line;
    let text = first_line(text);
    let shaped;
    let line = if text.len() + family.len() > MAX_CACHED_TEXT {
        shaped = layout_line(&mut fonts.system, text, size, family, weight, italic);
        shaped.as_ref()
    } else {
        let key = (text, size.to_bits(), family, weight, italic);
        let hash = hash_key(&key);
        let known = fonts
            .lines
            .get(&hash)
            .is_some_and(|((l, s, f, w, i), _)| (l.as_str(), *s, f.as_str(), *w, *i) == key);
        if !known {
            let line = layout_line(&mut fonts.system, text, size, family, weight, italic);
            let glyphs = line.as_ref().map_or(0, |line| line.glyphs.len());
            // Bound the memory a guest drawing ever new strings can take.
            if fonts.lines.len() >= 4096 || fonts.line_glyphs + glyphs > MAX_CACHED_GLYPHS {
                fonts.lines.clear();
                fonts.line_glyphs = 0;
            }
            let key = (text.to_string(), key.1, family.to_string(), weight, italic);
            if let Some((_, Some(old))) = fonts.lines.insert(hash, (key, line)) {
                fonts.line_glyphs -= old.glyphs.len();
            }
            fonts.line_glyphs += glyphs;
        }
        fonts.lines[&hash].1.as_ref()
    };
    let Some(line) = line else {
        return Vec::new();
    };
    let origin = place([line.w, line.max_ascent, line.max_descent]);
    let mut found = Vec::new();
    for layout in &line.glyphs {
        let physical = layout.physical(origin, 1.0);
        // The glyph's advance, and a font size around it for what sticks out.
        let (x, y) = (physical.x as f32, physical.y as f32);
        if x + layout.w + size < left
            || x - size > right
            || y + size < top
            || y - 2.0 * size > bottom
        {
            continue;
        }
        if let Some(image) = fonts.glyphs.get(&mut fonts.system, physical.cache_key) {
            let (x, y) = (
                physical.x + image.placement.left,
                physical.y - image.placement.top,
            );
            found.push((image, x, y));
        }
    }
    found
}

/// The hash of a [`Key`], borrowed.
fn hash_key(key: &(&str, u32, &str, u16, bool)) -> u64 {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    hasher.finish()
}

fn first_line(text: &str) -> &str {
    // cosmic-text shapes a single paragraph.
    text.split(is_paragraph_separator).next().unwrap_or("")
}

fn metrics(line: Option<LayoutLine>) -> [f32; 3] {
    line.map_or([0.0; 3], |l| [l.w, l.max_ascent, l.max_descent])
}

/// `line` shaped and laid out at `size` pixels, in the face the family, weight and style pick.
fn layout_line(
    fonts: &mut FontSystem,
    line: &str,
    size: f32,
    family: &str,
    weight: u16,
    italic: bool,
) -> Option<LayoutLine> {
    // Like GPUI, pick the face first (the closest weight and style in the first installed
    // family) and shape with exactly its attributes: cosmic-text itself would fall back to
    // another family for a weight the family lacks.
    let families: Vec<_> = Some(family)
        .filter(|name| !name.is_empty())
        .into_iter()
        .chain(FALLBACKS)
        .map(Family::Name)
        .chain([Family::SansSerif])
        .collect();
    let query = Query {
        families: &families,
        weight: Weight(weight),
        stretch: Stretch::Normal,
        style: if italic { Style::Italic } else { Style::Normal },
    };
    let db = fonts.db();
    let face = db.query(&query).and_then(|id| db.face(id))?;
    let (name, weight, style, stretch) = (
        face.families[0].0.clone(),
        face.weight,
        face.style,
        face.stretch,
    );
    let attrs = Attrs::new()
        .family(Family::Name(&name))
        .weight(weight)
        .style(style)
        .stretch(stretch);
    let shaped = ShapeLine::new(fonts, line, &AttrsList::new(&attrs), Shaping::Advanced, 4);
    shaped
        .layout(size, None, Wrap::None, None, None)
        .into_iter()
        .next()
}

/// The family the default font resolves to on this machine, for tests that depend on it.
pub fn default_family() -> Option<String> {
    let families: Vec<_> = FALLBACKS
        .into_iter()
        .map(Family::Name)
        .chain([Family::SansSerif])
        .collect();
    let query = Query {
        families: &families,
        ..Query::default()
    };
    let db = &SYSTEM_FONTS.1;
    let face = db.query(&query).and_then(|id| db.face(id))?;
    Some(face.families[0].0.clone())
}

/// Unicode's paragraph separators (bidi class B).
fn is_paragraph_separator(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{1c}'..='\u{1e}' | '\u{85}' | '\u{2029}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_one_line() {
        let [short, ascent, descent] = measure("Hi", 16.0, "", 400, false);
        let [long, ..] = measure("Hi there", 16.0, "", 400, false);
        assert!(short > 0.0 && long > short, "{short} {long}");
        assert!(ascent > 0.0 && descent > 0.0);
        assert_eq!(measure("Hi\nthere", 16.0, "", 400, false)[0], short);
        assert_eq!(measure("", 16.0, "", 400, false)[0], 0.0);
    }

    #[test]
    fn long_text_is_measured_but_not_kept() {
        let long = "Hi ".repeat(MAX_CACHED_TEXT);
        let [width, ..] = measure(&long, 16.0, "", 400, false);
        assert!(width > measure("Hi there", 16.0, "", 400, false)[0]);
        let fonts = lock(&MEASURING);
        assert!(fonts
            .measured
            .values()
            .all(|((line, ..), _)| line.len() <= MAX_CACHED_TEXT));
    }

    #[test]
    fn laid_out_lines_kept_are_bounded() {
        // About 100 000 glyphs of distinct lines just under the cached length.
        for i in 0..100 {
            let text = format!("{i:03} {}", "lorem ipsum ".repeat(84));
            assert!(text.len() <= MAX_CACHED_TEXT);
            let line = Line {
                text: &text,
                size: 12.0,
                family: "",
                weight: 400,
                italic: false,
            };
            draw(line, |_| (0.0, 0.0), [0.0; 4], |_, _, _| {});
        }
        let fonts = lock(&PAINTING);
        let kept: usize = fonts
            .lines
            .values()
            .filter_map(|(_, line)| line.as_ref())
            .map(|line| line.glyphs.len())
            .sum();
        assert_eq!(kept, fonts.line_glyphs);
        assert!(kept > 0 && kept <= MAX_CACHED_GLYPHS, "{kept} glyphs kept");
        eprintln!(
            "{kept} glyphs kept, {} bytes each",
            std::mem::size_of::<cosmic_text::LayoutGlyph>()
        );
    }

    #[test]
    fn glyphs_outside_the_area_are_not_rasterized() {
        let mut drawn = 0;
        let line = Line {
            text: "Hi there",
            size: 20.0,
            family: "",
            weight: 400,
            italic: false,
        };
        draw(
            line,
            |_| (10.0, 30.0),
            [60.0, 0.0, 400.0, 100.0],
            |_, x, _| {
                assert!(x >= 20, "{x}");
                drawn += 1;
            },
        );
        assert!((1..7).contains(&drawn), "{drawn} glyphs");
    }

    #[test]
    fn measuring_does_not_hold_up_painting() {
        let _ = measure("warm up", 12.0, "", 400, false);
        // A guest measuring for as long as it likes on a worker.
        let measuring = lock(&MEASURING);
        let (done, painted) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let line = Line {
                text: "Hi",
                size: 20.0,
                family: "",
                weight: 400,
                italic: false,
            };
            draw(
                line,
                |_| (0.0, 20.0),
                [0.0, 0.0, 100.0, 100.0],
                |_, _, _| {},
            );
            done.send(()).unwrap();
        });
        let wait = std::time::Duration::from_secs(10);
        assert!(painted.recv_timeout(wait).is_ok());
        drop(measuring);
    }

    #[test]
    fn missing_fonts_fall_back_to_the_default_font() {
        assert_eq!(
            measure("Hi there", 16.0, "No Such Font", 400, false),
            measure("Hi there", 16.0, "", 400, false)
        );
    }

    #[test]
    fn a_weight_the_font_lacks_uses_its_closest_face() {
        // Default fonts have a regular face but rarely a 450 one; either way the text keeps a
        // proportional font instead of falling back to whatever cosmic-text finds first.
        let [narrow, ..] = measure("iiii", 20.0, "", 450, false);
        let [wide, ..] = measure("MMMM", 20.0, "", 450, false);
        assert!(narrow < wide, "{narrow} {wide}");
    }

    #[test]
    fn drawn_glyphs_span_the_measured_width() {
        let [width, ..] = measure("Hi there", 20.0, "", 400, false);
        let (mut left, mut right) = (i32::MAX, i32::MIN);
        let line = Line {
            text: "Hi there",
            size: 20.0,
            family: "",
            weight: 400,
            italic: false,
        };
        draw(
            line,
            |_| (10.0, 30.0),
            [0.0, 0.0, 400.0, 100.0],
            |image, x, _| {
                left = left.min(x);
                right = right.max(x + image.placement.width as i32);
            },
        );
        assert!((10..14).contains(&left), "{left}");
        assert!(((right - 10) as f32 - width).abs() < 3.0, "{right} {width}");
    }
}
