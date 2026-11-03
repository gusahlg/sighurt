//! Fonts and shaped text, as the chrome API offers them: sighurt-render's
//! ([`sighurt_render::text`]), re-exported so a chrome needs no other crate to shape its text.
//! The frontend shares one font system between the chrome's [`Fonts`] and its backend.
//!
//! Must not own: anything; the text machinery is sighurt-render's.

pub use sighurt_render::text::{
    at_size, FontSystemHandle, Fonts, ShapedText, TextStyle, DEFAULT_FONT_SIZE,
};
