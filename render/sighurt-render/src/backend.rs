//! The backend contract: what renders a [`Scene`] and presents it, and what a frontend hands it.
//!
//! A frontend starts one backend and serves every chrome surface of its window with it. A
//! backend either fills pixel memory the frontend lends ([`Presents::Pixels`]: the host's
//! shared-memory buffers) or presents on a native surface it bound to with the host's handles
//! ([`Presents::Native`]); the frontend makes each chrome target the matching kind. Surfaces
//! are told apart by [`SurfaceKey`], which the frontend makes from its own target ids.
//!
//! A backend does no work between calls: no threads of its own that wake the owner, no
//! timers, nothing that repaints by itself. Its per-surface objects die in
//! [`Backend::unbind`], which the frontend calls before it drops the target, and the backend
//! itself is dropped after every surface is unbound and before the host.
//!
//! Must not own: host targets or their lifetime (the frontend's and the host's), Wayland
//! objects, what is drawn (the chrome's), or browser state.

use std::fmt;

use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use sighurt_api::geometry::Metrics;

use crate::scene::Scene;

/// A frontend's surface, as a backend knows it: the frontend's target id, opaque.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SurfaceKey(pub u64);

/// Memory in `WL_SHM_FORMAT_ARGB8888` (premultiplied, little-endian: bytes B, G, R, A) to
/// render into.
pub struct PixelBuffer<'a> {
    /// The pixels, row by row.
    pub data: &'a mut [u8],
    /// Width in buffer pixels.
    pub width: u32,
    /// Height in buffer pixels.
    pub height: u32,
    /// Bytes from one row's start to the next's: at least `width * 4`.
    pub stride: usize,
}

/// Where a scene goes.
pub enum Output<'a> {
    /// CPU memory the host lends, `metrics.buffer()` in size, for a [`Presents::Pixels`]
    /// backend; the frontend submits it after the render.
    Pixels(PixelBuffer<'a>),
    /// The native surface the backend bound with [`Backend::bind`], for a
    /// [`Presents::Native`] backend, which presents it itself.
    Native,
}

/// How a backend presents, so the frontend makes the right kind of target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Presents {
    /// Into pixel memory the frontend lends and submits: [`Output::Pixels`].
    Pixels,
    /// On a native surface it binds to: [`Output::Native`].
    Native,
}

/// What became of a render.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rendered {
    /// A frame was rendered: for `Pixels`, the memory holds it and the frontend submits it;
    /// for `Native`, the backend presented it.
    Submitted,
    /// The scene and metrics are those of the surface's last frame, which it still shows:
    /// nothing was rendered, nothing is to be submitted, the output is untouched.
    Unchanged,
    /// Nothing could be rendered now (no swapchain image yet): the frontend keeps the surface
    /// dirty and tries again soon, once.
    Deferred,
}

/// Why a render or a bind failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// The output was the wrong kind, size or layout for the surface.
    Output(String),
    /// The surface can't be presented on (lost, outdated beyond repair): unbind it and bind it
    /// again once; then treat it as `Device`.
    Surface(String),
    /// The backend can't go on (device lost, a validation error): the frontend replaces it.
    Device(String),
}

impl fmt::Display for RenderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Output(why) => write!(f, "wrong output: {why}"),
            Self::Surface(why) => write!(f, "the surface is lost: {why}"),
            Self::Device(why) => write!(f, "the device is lost: {why}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// What a backend counted since it started, for the diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackendStats {
    /// Frames rendered ([`Rendered::Submitted`]).
    pub frames: u64,
    /// Renders that found the surface's last frame current ([`Rendered::Unchanged`]).
    pub unchanged: u64,
    /// Renders put off ([`Rendered::Deferred`]).
    pub deferred: u64,
    /// The longest render, in microseconds.
    pub max_render_us: u64,
    /// Rasterized glyphs held now.
    pub glyphs: u64,
    /// Times the glyph cache started over (its budget, or a new scale).
    pub glyph_resets: u64,
}

/// What a native backend binds to: the host's display and window handles for one surface.
pub trait NativeWindow: HasDisplayHandle + HasWindowHandle {}

impl<T: HasDisplayHandle + HasWindowHandle + ?Sized> NativeWindow for T {}

/// A renderer of scenes. Not `Send` or `Sync`: it runs on the owner thread with the host.
pub trait Backend {
    /// Its name in `[ui] renderer`.
    fn name(&self) -> &'static str;

    /// How it presents.
    fn presents(&self) -> Presents;

    /// What it runs on, for `sig --version`-style output and the diagnostics: adapter, driver,
    /// API version; for the CPU backend, the rasterizer.
    fn describe(&self) -> String;

    /// Binds `surface` to the native window the host made for it. `Pixels` backends never get
    /// this call. It creates only the API's surface; the swapchain is configured by the first
    /// [`render`](Self::render), at the metrics it is given.
    ///
    /// # Safety
    ///
    /// The handles are valid for the call. What the backend creates from them keeps using the
    /// window and its display afterwards; the caller promises to call
    /// [`unbind`](Self::unbind) for `surface` before the window or its connection is
    /// destroyed (the contract of the host's lease).
    #[allow(unsafe_code)]
    unsafe fn bind(
        &mut self,
        surface: SurfaceKey,
        window: &dyn NativeWindow,
    ) -> Result<(), RenderError>;

    /// Drops everything the backend keeps for `surface`: what it bound, and the last frame it
    /// knows, so the next render renders. Called when the surface is hidden or goes, and for
    /// every surface before the backend is dropped. A surface it doesn't know is fine.
    fn unbind(&mut self, surface: SurfaceKey);

    /// Renders `scene` for `metrics` into `output`, whole, for a surface whose target has
    /// `metrics` now. `Native` also presents (never waiting for vsync: the host paces frames).
    fn render(
        &mut self,
        surface: SurfaceKey,
        metrics: &Metrics,
        scene: &Scene,
        output: Output<'_>,
    ) -> Result<Rendered, RenderError>;

    /// What it counted.
    fn stats(&self) -> BackendStats;
}
