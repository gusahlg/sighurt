//! Rasterizing one glyph: swash through cosmic-text's `SwashCache`, hinted, from the shared font
//! system. Every backend rasterizes with these functions: the CPU backend blends the image
//! directly from a bounded [`GlyphCache`], a GPU backend's atlas (`crate::gpu`) uploads it once
//! and keeps only where it put it. Where the glyph goes is [`crate::scene::glyph_physical`]'s.
//!
//! Must not own: where a glyph is placed, or which cache a backend keeps beyond [`GlyphCache`]'s
//! budget.

use std::borrow::Cow;

pub use cosmic_text::{CacheKey, SwashCache, SwashContent, SwashImage};

use crate::text::FontSystemHandle;

/// Rasterized glyphs a [`GlyphCache`] keeps before it rasterizes the rest uncached. Chrome
/// text uses a few hundred.
pub const MAX_GLYPHS: usize = 2048;
/// Bytes of glyph images a [`GlyphCache`] keeps, the same way. A chrome glyph is a few hundred
/// bytes; an emoji at 200 % tens of kilobytes.
pub const MAX_GLYPH_BYTES: usize = 8 * 1024 * 1024;

/// The image of the glyph `key` names, rasterized without caching it; `None` for a glyph with
/// no outline or bitmap (a space). The font system is borrowed for this call only.
pub fn rasterize(
    fonts: &FontSystemHandle,
    swash: &mut SwashCache,
    key: CacheKey,
) -> Option<SwashImage> {
    fonts.with(|system| swash.get_image_uncached(system, key))
}

/// Glyph images kept across frames, bounded in entries ([`MAX_GLYPHS`]) and bytes
/// ([`MAX_GLYPH_BYTES`]): a miss is admitted only while both have room; past them glyphs are
/// rasterized uncached until the owner [`clear`](Self::clear)s it (a new scale), so one frame
/// can't grow it without bound and an idle surface keeps nothing it didn't admit.
#[derive(Default)]
pub struct GlyphCache {
    swash: Option<SwashCache>,
    bytes: usize,
    resets: u64,
}

impl GlyphCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// The image of the glyph `key` names, cached if there is room; `None` for a glyph with no
    /// image.
    pub fn image(
        &mut self,
        fonts: &FontSystemHandle,
        key: CacheKey,
    ) -> Option<Cow<'_, SwashImage>> {
        let swash = self.swash.get_or_insert_with(SwashCache::new);
        if swash.image_cache.contains_key(&key) {
            return swash.image_cache[&key].as_ref().map(Cow::Borrowed);
        }
        if swash.image_cache.len() >= MAX_GLYPHS || self.bytes >= MAX_GLYPH_BYTES {
            return rasterize(fonts, swash, key).map(Cow::Owned);
        }
        let image = rasterize(fonts, swash, key);
        self.bytes += image.as_ref().map_or(0, |image| image.data.len());
        let slot = swash.image_cache.entry(key).or_insert(image);
        slot.as_ref().map(Cow::Borrowed)
    }

    /// Forgets every image; counted as a reset when it held any.
    pub fn clear(&mut self) {
        if let Some(swash) = &mut self.swash {
            if !swash.image_cache.is_empty() {
                self.resets += 1;
            }
            swash.image_cache.clear();
        }
        self.bytes = 0;
    }

    /// Glyphs held.
    pub fn len(&self) -> usize {
        self.swash
            .as_ref()
            .map_or(0, |swash| swash.image_cache.len())
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Bytes of images held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Times it was cleared while holding glyphs.
    pub fn resets(&self) -> u64 {
        self.resets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::tests::fonts;

    /// Past its budget the cache rasterizes uncached: it never grows beyond it.
    #[test]
    fn admission_is_bounded() {
        let handle = fonts().handle();
        let font = handle.with(|system| system.db().faces().next().unwrap().id);
        let mut cache = GlyphCache::new();
        let key = |glyph: u16, size: f32| {
            let flags = cosmic_text::CacheKeyFlags::empty();
            CacheKey::new(font, glyph, size, (0.0, 0.0), flags).0
        };
        let mut n = 0;
        'fill: for size in 4..400 {
            for glyph in 1..12 {
                let _ = cache.image(&handle, key(glyph, size as f32));
                n += 1;
                if n > MAX_GLYPHS + 50 {
                    break 'fill;
                }
            }
        }
        assert!(cache.len() <= MAX_GLYPHS, "{}", cache.len());
        // Admission stops once the bytes pass the budget: at most one image beyond it.
        assert!(cache.bytes() <= MAX_GLYPH_BYTES + 400 * 400);
        // A glyph past the budget is still rasterized.
        assert!(cache.image(&handle, key(5, 401.0)).is_some());
        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.bytes(), 0);
        assert_eq!(cache.resets(), 1);
    }
}
