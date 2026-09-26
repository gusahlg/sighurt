//! The CPU painter: draws the chrome into buffer memory the host lends it.
//!
//! Shapes, falls back between fonts and rasterizes text with cosmic-text, and draws rectangles,
//! paths and images with tiny-skia, at the target's buffer size. Font discovery, shaped text
//! and glyphs are cached with explicit budgets; a scale or font change invalidates the affected
//! entries. Repaints the whole small chrome buffer on every change, and converts deliberately
//! from tiny-skia's premultiplied RGBA to the buffer's pixel format.
//!
//! Must not own: buffers or their lifetime (the host's SHM presenter), or page pixels.
