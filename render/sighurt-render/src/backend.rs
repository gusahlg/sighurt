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
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RenderOutcome {
    /// A frame was rendered: for `Pixels`, the memory holds it and the frontend submits it;
    /// for `Native`, the backend presented it.
    Submitted,
    /// The scene and metrics (layout epoch included) are those of the surface's last frame,
    /// which the surface still shows: nothing was rendered, nothing is to be submitted, the
    /// output is untouched. A backend says this only when that last frame was rendered for the
    /// surface and the frontend has not since told it the frame is gone (an `unbind`); the
    /// frontend must therefore submit every `Submitted` frame of a `Pixels` backend.
    Unchanged,
    /// Nothing could be rendered for this permit (no swapchain image in time, the surface
    /// occluded): the frontend gives the permit back unused and invalidates the target, so the
    /// host issues a new permit when it next may. The backend never retries on its own clock.
    Deferred,
}

/// Why a render or a bind failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RenderError {
    /// The output was the wrong kind, size or layout for the surface.
    Output(String),
    /// The surface can't be presented on (lost, outdated beyond repair): the frontend unbinds
    /// it and binds it again, once; if that or its next render fails too, it treats the
    /// failure as `Device`.
    Surface(String),
    /// The backend can't go on (no adapter or device, device lost, a validation error, an
    /// atlas that overflows even after starting over): the frontend replaces the whole backend
    /// with the CPU one.
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

/// What a backend counted since it started, for the diagnostics. A measurement the backend
/// can't make, or that means nothing for it (a swapchain of the CPU backend), is `None`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackendStats {
    /// Frames rendered ([`RenderOutcome::Submitted`]).
    pub frames: u64,
    /// Renders that found the surface's last frame current ([`RenderOutcome::Unchanged`]).
    pub unchanged: u64,
    /// Renders put off ([`RenderOutcome::Deferred`]).
    pub deferred: u64,
    /// The longest render, in microseconds.
    pub max_render_us: u64,
    /// The latest render, in microseconds.
    pub last_render_us: u64,
    /// Rasterized glyphs held now.
    pub glyphs: u64,
    /// Bytes of glyph images held on the CPU.
    pub glyph_bytes: u64,
    /// Times the glyph cache or atlas started over (its budget, or a new scale).
    pub glyph_resets: u64,
    /// A GPU backend's atlas.
    pub atlas: Option<AtlasStats>,
    /// Native surfaces bound now.
    pub surfaces: u64,
    /// Swapchain images per bound surface, when the API says.
    pub swapchain_images: Option<u32>,
    /// Bytes of device memory the backend's resources take (atlas pages, buffers, swapchain
    /// images at their configured size), as it can tell; `None` when it can't.
    pub device_bytes: Option<u64>,
    /// Swapchain (re)configurations.
    pub configures: u64,
    /// The longest `bind`, its first one's device creation included, in microseconds.
    pub max_bind_us: Option<u64>,
    /// The longest swapchain configuration, in microseconds.
    pub max_configure_us: Option<u64>,
    /// The longest `unbind` (waiting for the device included), in microseconds.
    pub max_unbind_us: Option<u64>,
}

/// A GPU backend's glyph and icon atlas (`crate::gpu::Atlas`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasStats {
    /// The mask page's side, in texels.
    pub mask_size: u32,
    /// The color page's side, in texels.
    pub color_size: u32,
    /// The pages' bytes, as textures.
    pub bytes: u64,
    /// Times a page doubled.
    pub grows: u64,
    /// Times a page started over at its size (full at the bound, or a new scale).
    pub resets: u64,
    /// Frames that didn't fit even after starting over.
    pub overflows: u64,
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
    /// this call. Constructing a native backend is provisional (the window doesn't exist yet):
    /// the first `bind` also chooses an adapter that can present to this surface, creates the
    /// device and checks the surface's capabilities, so a backend that can't present fails
    /// here, with [`RenderError::Device`] (nothing usable) or [`RenderError::Surface`] (this
    /// surface), and [`describe`](Self::describe) names the adapter only after it. The
    /// swapchain is configured by the first [`render`](Self::render), at the metrics it is
    /// given (a target is born with empty metrics).
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
    /// knows, so the next render renders. A surface it doesn't know is fine.
    ///
    /// The frontend calls it when the target is retired, to recover a lost surface, on a
    /// fallback, and for every surface before the backend is dropped; for a `Pixels` backend
    /// also when the surface is hidden, since the host detaches a hidden SHM surface's buffer.
    /// A native binding is kept across a hide (RENDER.md §15): the surface keeps its last
    /// buffer, and dropping and recreating the swapchain costs tens of milliseconds a show.
    fn unbind(&mut self, surface: SurfaceKey);

    /// Renders `scene` for `metrics` into `output`, whole, for a surface whose target has
    /// `metrics` now: once per frame permit, never outside one. `Native` also presents (never
    /// waiting for vsync: the host paces frames with the permits). A scene shaped with another
    /// font system than the backend's is refused ([`RenderError::Output`]).
    fn render(
        &mut self,
        surface: SurfaceKey,
        metrics: &Metrics,
        scene: &Scene,
        output: Output<'_>,
    ) -> Result<RenderOutcome, RenderError>;

    /// What it counted.
    fn stats(&self) -> BackendStats;
}
