//! Sighurt's render framework: what a chrome draws, separated from how it is rendered and
//! presented (docs/rewrite/RENDER.md).
//!
//! A chrome draws each of its surfaces with a [`Pen`], which records a [`Scene`]: plain data
//! in window-logical coordinates (rectangles, rounded and bordered rectangles, paths, glyph
//! runs), with no pixel or handle in it. A [`Backend`] renders a scene for a target's metrics
//! and presents it: [`CpuBackend`] into the host's shared-memory buffers, others (behind
//! features of the frontend) on native surfaces. Text is shaped by [`Fonts`] from one font
//! system the frontend loads and shares with the backend ([`FontSystemHandle`]), and every
//! backend places and rasterizes glyphs with the same functions ([`scene::glyph_physical`],
//! [`glyph::rasterize`]), so a control has the same pixels under each.
//!
//! The modules: [`color`] has [`Rgba`]; [`scene`] the scene, its items, and the snapping every
//! backend shares; [`pen`] the scene builder a chrome draws with; [`text`] fonts, shaping and
//! shaped text; [`glyph`] rasterizing one glyph; [`backend`] the [`Backend`] trait and what
//! crosses it; [`cpu`] the CPU backend, in every build.
//!
//! This is the only crate that names tiny-skia or cosmic-text: a chrome names neither. It knows
//! nothing of Wayland (a native backend gets raw-window-handle handles) or of browser state.
//!
//! Must not own: host targets, surfaces or their lifetimes (the frontend's and the host's),
//! what is drawn (the chrome's), or browser state (the core's).

#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod backend;
pub mod color;
pub mod cpu;
pub mod glyph;
pub mod pen;
pub mod scene;
pub mod text;

pub use backend::{
    Backend, BackendStats, NativeWindow, Output, PixelBuffer, Presents, RenderError, Rendered,
    SurfaceKey,
};
pub use color::Rgba;
pub use cpu::CpuBackend;
pub use pen::Pen;
pub use scene::{PathBuilder, Scene};
pub use text::{FontSystemHandle, Fonts, ShapedText, TextStyle};
