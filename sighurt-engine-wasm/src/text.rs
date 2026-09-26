//! Text measurement for `api_canvas_measure_text`.
//!
//! The browser paints guest text with its own fonts, so measurements can only approximate its
//! layout. They are shaped with [cosmic-text](https://crates.io/crates/cosmic-text), the shaper
//! GPUI uses on Linux, against the system fonts, and pick fonts the way GPUI does, which matches
//! the browser's rendering there.

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{LazyLock, Mutex};

use cosmic_text::fontdb::Query;
use cosmic_text::{
    Attrs, AttrsList, Family, FontSystem, ShapeLine, Shaping, Stretch, Style, Weight, Wrap,
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

/// The system fonts, loaded on first use (scanning them takes a moment), and the
/// measurements made with them.
static FONTS: LazyLock<Mutex<Fonts>> = LazyLock::new(|| {
    Mutex::new(Fonts {
        system: FontSystem::new(),
        measured: HashMap::new(),
    })
});

struct Fonts {
    system: FontSystem,
    /// Measurements of short lines, by a hash of their [`Key`], which is kept to tell keys with
    /// the same hash apart. Guests that lay out text measure the same strings every frame, and
    /// shaping them costs far more than the lookup.
    measured: HashMap<u64, (Key, [f32; 3])>,
}

/// What a measurement depends on: line, size bits, family, weight and italic.
type Key = (String, u32, String, u16, bool);

/// Longest line plus family name, in bytes, whose measurement is kept. Longer text is rare,
/// and would make the cache hold whatever megabytes a guest passes.
const MAX_CACHED_TEXT: usize = 1024;

/// Width, ascent and descent in pixels of `text` laid out on one line at `size` pixels. Only its
/// first line is measured. An empty `family` means the browser's default font.
pub fn measure(text: &str, size: f32, family: &str, weight: u16, italic: bool) -> [f32; 3] {
    // cosmic-text shapes a single paragraph.
    let line = text.split(is_paragraph_separator).next().unwrap_or("");
    let mut fonts = FONTS.lock().unwrap();
    if line.len() + family.len() > MAX_CACHED_TEXT {
        return shape(&mut fonts.system, line, size, family, weight, italic);
    }
    let key = (line, size.to_bits(), family, weight, italic);
    let hash = {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    };
    if let Some(((l, s, f, w, i), metrics)) = fonts.measured.get(&hash) {
        if (l.as_str(), *s, f.as_str(), *w, *i) == key {
            return *metrics;
        }
    }
    let metrics = shape(&mut fonts.system, line, size, family, weight, italic);
    // Bound the memory a guest measuring ever new strings can take.
    if fonts.measured.len() >= 4096 {
        fonts.measured.clear();
    }
    let key = (line.to_string(), key.1, family.to_string(), weight, italic);
    fonts.measured.insert(hash, (key, metrics));
    metrics
}

fn shape(
    fonts: &mut FontSystem,
    line: &str,
    size: f32,
    family: &str,
    weight: u16,
    italic: bool,
) -> [f32; 3] {
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
    let Some(face) = db.query(&query).and_then(|id| db.face(id)) else {
        return [0.0; 3];
    };
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
        .first()
        .map_or([0.0; 3], |l| [l.w, l.max_ascent, l.max_descent])
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
        let fonts = FONTS.lock().unwrap();
        assert!(fonts
            .measured
            .values()
            .all(|((line, ..), _)| line.len() <= MAX_CACHED_TEXT));
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
}
