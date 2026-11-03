//! Rasterizing one glyph: swash through cosmic-text's `SwashCache`, hinted, from the shared font
//! system. Every backend rasterizes with [`rasterize`]: the CPU backend blends the image
//! directly, a GPU backend uploads it to its atlas. Where the glyph goes is
//! [`crate::scene::glyph_physical`]'s.
//!
//! Must not own: the cache's budget or lifetime (the backend that holds the cache), or where a
//! glyph is placed.

pub use cosmic_text::{CacheKey, SwashCache, SwashContent, SwashImage};

use crate::text::FontSystemHandle;

/// The image of the glyph `key` names, from `cache` or rasterized into it with `fonts`; `None`
/// for a glyph with no outline or bitmap (a space). The font system is borrowed for this call
/// only.
pub fn rasterize<'c>(
    fonts: &FontSystemHandle,
    cache: &'c mut SwashCache,
    key: CacheKey,
) -> Option<&'c SwashImage> {
    cache.get_image(&mut fonts.borrow_mut(), key).as_ref()
}
