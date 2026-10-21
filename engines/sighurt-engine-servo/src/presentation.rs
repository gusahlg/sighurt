//! Presenting Servo's frames into the view's Wayland subsurface.
//!
//! [`PageContext`] is Servo's `WindowRenderingContext` (surfman) on the target lease's raw
//! display and window handles: WebRender draws into the EGL window surface's back buffer
//! (`WebView::paint`) and [`PageContext::swap`] swaps it, which attaches and commits the leaf
//! inside the driver, so the host never commits buffers on it. Swaps never wait (swap interval
//! 0): the host paces them with its frame callbacks, one permit per callback. [`FrameDriver`]
//! is Servo's refresh driver, started when a permit arrives. A hidden view gives its EGL window
//! surface back ([`PageContext::release_surface`]) and makes a new one when shown again; the
//! GL context stays.
//!
//! [`PageSurface`] is the part this Wayland surface and the offscreen surface in
//! [`crate::offscreen`] both implement, so the runtime can present either. This module owns
//! only the Wayland surface. Nothing reads page pixels back on this path: every `read_to_image`
//! is counted, and with [`crate::runtime::ServoConfig::audit_gl`] every
//! `glReadPixels`/`glGetTexImage` WebRender makes is counted too. Diagnostics name the GL
//! renderer, so a software fallback (llvmpipe) can't pass for the GPU path.
//!
//! The EGL display is one per Wayland connection, shared by every view's context and Servo's
//! WebGL thread, so [`Egl::terminate`] runs once, at shutdown, after Servo and every context.
//!
//! Must not own: the surface, its role or its placement (the host).

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use dpi::PhysicalSize;
use euclid::default::Size2D;
use gleam::gl::{self, Gl, ProfilingGl};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use servo::{DeviceIntRect, RefreshDriver, RenderingContext, RgbaImage, WindowRenderingContext};
use sighurt_api::geometry::BufferSize;
use sighurt_api::present::Frame;
use sighurt_wayland::GlHandles;
use surfman::{Connection as SurfmanConnection, Surface, SurfaceTexture};

/// What the GL context turned out to be, for diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlInfo {
    /// `GL_RENDERER`, such as "NVIDIA GeForce RTX 3070/PCIe/SSE2" or "llvmpipe (...)".
    pub renderer: String,
    /// `GL_VENDOR`.
    pub vendor: String,
    /// `GL_VERSION`.
    pub version: String,
    /// `EGL_VENDOR` and `EGL_VERSION`.
    pub egl: String,
    /// A software rasterizer (llvmpipe, softpipe, swrast): the diagnosed fallback, not the GPU
    /// path.
    pub software: bool,
}

/// Readbacks of page pixels to the CPU. Both stay 0 on the normal display path.
/// An offscreen present increments [`Self::read_to_image`] once per successful read into a
/// lent frame, through glow, so [`Self::gl_reads`] stays the audit of WebRender's own calls.
#[derive(Clone, Debug, Default)]
pub(crate) struct Readbacks {
    pub read_to_image: Rc<Cell<u64>>,
    /// `glReadPixels`/`glGetTexImage` calls, when auditing.
    pub gl_reads: Option<Rc<Cell<u64>>>,
}

/// Which surfaceless adapter an offscreen view asked for.
///
/// `Hardware` is surfman's hardware adapter, tried first. Context creation does not set
/// `DRI_PRIME` or `LIBGL_ALWAYS_SOFTWARE` (`third_party/surfman/SIGHURT-PATCH.md`); Mesa uses
/// the device the process was started with. `Software` is used only after the hardware adapter
/// failed to produce a context, and cannot force llvmpipe on its own. The GL renderer can
/// still be llvmpipe when the hardware adapter was asked for; [`GlInfo::software`] says so.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OffscreenAdapter {
    /// Surfman's hardware adapter.
    Hardware,
    /// Surfman's software adapter, after the hardware one failed.
    Software,
}

impl std::fmt::Display for OffscreenAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hardware => "hardware",
            Self::Software => "software",
        })
    }
}

/// What a view's GL surface must do for the runtime, beyond Servo's [`RenderingContext`].
///
/// The Wayland surface swaps. The offscreen surface reads one frame back. Must not own the
/// target's buffers or the view's document.
pub(crate) trait PageSurface: RenderingContext {
    /// The GL strings of this context, with EGL's.
    fn gl_info(&self, egl: &Egl) -> GlInfo;

    /// Whether a present can show pixels. False while a Wayland view has given its window
    /// surface back. An offscreen framebuffer stays bound.
    fn surface_is_bound(&self) -> bool;

    /// The surfaceless adapter this surface was opened with. `None` for a Wayland surface.
    fn offscreen_adapter(&self) -> Option<OffscreenAdapter> {
        None
    }

    /// Reads the framebuffer into `frame` as ARGB8888, top row first.
    /// A Wayland surface swaps instead, and never calls this.
    fn read_argb(&self, _frame: Frame<'_>) -> Result<(), String> {
        Err("this surface does not read pixels back".into())
    }
}

/// Servo's `WindowRenderingContext` on a view's page surface, with every readback counted.
pub(crate) struct PageContext {
    window: WindowRenderingContext,
    gl: Rc<dyn Gl>,
    readbacks: Readbacks,
    /// Whether an EGL window surface is bound (false while the view is hidden).
    bound: Cell<bool>,
}

impl PageContext {
    /// Creates the context on `handles` at `size` and makes it current, with swap interval 0.
    /// `readbacks` is shared by every view, so the runtime reports one total.
    pub fn new(
        handles: GlHandles<'_>,
        size: BufferSize,
        driver: Rc<FrameDriver>,
        readbacks: &Readbacks,
    ) -> Result<Self, String> {
        let display = handles.display_handle().map_err(|e| e.to_string())?;
        let window = handles.window_handle().map_err(|e| e.to_string())?;
        let window = WindowRenderingContext::new_with_refresh_driver(
            display,
            window,
            physical(size),
            driver,
        )
        .map_err(|e| format!("cannot create the page's GL context: {e:?}"))?;
        let gl = match &readbacks.gl_reads {
            Some(reads) => {
                let reads = Rc::clone(reads);
                ProfilingGl::wrap(window.gleam_gl_api(), Duration::ZERO, move |name, _| {
                    if name.starts_with("read_pixels") || name.starts_with("get_tex_image") {
                        reads.set(reads.get() + 1);
                    }
                })
            }
            None => window.gleam_gl_api(),
        };
        let context = Self {
            window,
            gl,
            readbacks: readbacks.clone(),
            bound: Cell::new(true),
        };
        // `WindowRenderingContext::new` left the context current on its window surface.
        Egl::load()?.set_swap_interval_zero()?;
        Ok(context)
    }

    /// The GL strings of this context, with EGL's.
    pub fn info(&self, egl: &Egl) -> GlInfo {
        let renderer = self.window.gleam_gl_api().get_string(gl::RENDERER);
        let lower = renderer.to_lowercase();
        GlInfo {
            software: ["llvmpipe", "softpipe", "swrast", "software"]
                .iter()
                .any(|name| lower.contains(name)),
            vendor: self.window.gleam_gl_api().get_string(gl::VENDOR),
            version: self.window.gleam_gl_api().get_string(gl::VERSION),
            egl: egl.vendor_and_version(),
            renderer,
        }
    }

    /// Whether the EGL window surface exists (the view isn't hidden).
    pub fn is_bound(&self) -> bool {
        self.bound.get()
    }

    /// Destroys the EGL window surface and its `wl_egl_window`, keeping the GL context and
    /// WebRender's state, while the view is hidden: its buffers are the page-sized memory a
    /// background tab would otherwise hold. The `wl_surface` is untouched; the host has
    /// already unmapped it by destroying its role.
    pub fn release_surface(&self) {
        if self.bound.replace(false) {
            if let Err(error) = self.window.take_window() {
                log::warn!("servo: releasing a hidden page's EGL surface failed: {error:?}");
            }
        }
    }

    /// Makes a new EGL window surface on `handles` at `size`, with swap interval 0, when the
    /// view is shown again.
    pub fn bind_surface(&self, handles: GlHandles<'_>, size: BufferSize) -> Result<(), String> {
        if self.bound.get() {
            return Ok(());
        }
        let window = handles.window_handle().map_err(|e| e.to_string())?;
        self.window
            .set_window(window, physical(size))
            .map_err(|e| format!("cannot recreate the page's EGL surface: {e:?}"))?;
        self.bound.set(true);
        Egl::load()?.set_swap_interval_zero()
    }

    /// Swaps buffers: the driver attaches and commits the page surface. Never waits.
    pub fn swap(&self) {
        if let Err(error) = self.window.make_current() {
            log::warn!("servo: making the page's context current failed: {error:?}");
        }
        self.window.present();
    }
}

impl PageSurface for PageContext {
    fn gl_info(&self, egl: &Egl) -> GlInfo {
        self.info(egl)
    }

    fn surface_is_bound(&self) -> bool {
        self.is_bound()
    }
}

impl RenderingContext for PageContext {
    fn prepare_for_rendering(&self) {
        self.window.prepare_for_rendering();
    }

    fn read_to_image(&self, rect: DeviceIntRect) -> Option<RgbaImage> {
        let calls = &self.readbacks.read_to_image;
        calls.set(calls.get() + 1);
        log::warn!("servo: page pixels read back to the CPU ({rect:?})");
        self.window.read_to_image(rect)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.window.size()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        self.window.resize(size);
    }

    fn present(&self) {
        self.window.present();
    }

    fn make_current(&self) -> Result<(), surfman::Error> {
        self.window.make_current()
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        Rc::clone(&self.gl)
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.window.glow_gl_api()
    }

    fn create_texture(&self, surface: Surface) -> Option<(SurfaceTexture, u32, Size2D<i32>)> {
        self.window.create_texture(surface)
    }

    fn destroy_texture(&self, texture: SurfaceTexture) -> Option<Surface> {
        self.window.destroy_texture(texture)
    }

    fn connection(&self) -> Option<SurfmanConnection> {
        self.window.connection()
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        self.window.refresh_driver()
    }
}

pub(crate) fn physical(size: BufferSize) -> PhysicalSize<u32> {
    PhysicalSize::new(size.width, size.height)
}

/// Servo's frame clock for one view, started by the host's frame permits.
///
/// While Servo animates it registers for "the next frame". The host issues a permit only when
/// the frame callback of the view's last swap has arrived (or the view was just mapped), so
/// starting the waiting frame when a permit arrives paces animations at the output's rate, and
/// a hidden view, which gets no permits, animates nothing.
#[derive(Default)]
pub(crate) struct FrameDriver {
    observers: RefCell<Vec<Box<dyn Fn() + Send>>>,
}

impl FrameDriver {
    pub fn is_waiting(&self) -> bool {
        !self.observers.borrow().is_empty()
    }

    /// Starts the frame Servo waits for: each observer marks Servo ready and wakes the loop.
    pub fn start_frame(&self) {
        for observer in self.observers.take() {
            observer();
        }
    }
}

impl RefreshDriver for FrameDriver {
    fn observe_next_frame(&self, start_frame: Box<dyn Fn() + Send + 'static>) {
        self.observers.borrow_mut().push(start_frame);
    }
}

type EglDisplay = *mut c_void;

const EGL_VENDOR: i32 = 0x3053;
const EGL_VERSION: i32 = 0x3054;

type GetCurrentDisplay = unsafe extern "C" fn() -> EglDisplay;
type SwapInterval = unsafe extern "C" fn(EglDisplay, i32) -> u32;
type QueryString = unsafe extern "C" fn(EglDisplay, i32) -> *const c_char;
type Terminate = unsafe extern "C" fn(EglDisplay) -> u32;
type GetError = unsafe extern "C" fn() -> i32;

/// The EGL display of the context current on this thread, with the calls surfman doesn't make:
/// swap interval 0 (surfman keeps the default of 1, whose swaps wait for a frame callback the
/// compositor may never send to an unmapped surface) and `eglTerminate` (surfman never
/// terminates a display it didn't open; one left initialized is torn down by the driver's exit
/// handler after the Wayland connection is gone).
pub(crate) struct Egl {
    display: EglDisplay,
    swap_interval: SwapInterval,
    query_string: QueryString,
    terminate: Terminate,
    get_error: GetError,
}

impl Egl {
    /// Resolves the entry points from the libEGL surfman already loaded, for the display of
    /// the context current on this thread.
    pub fn load() -> Result<Self, String> {
        // SAFETY: dlopen and dlsym with valid C strings; each symbol is cast to the signature
        // the EGL 1.5 specification gives it. eglGetCurrentDisplay takes no arguments and
        // returns EGL_NO_DISPLAY (null) when no context is current.
        unsafe {
            let mut lib =
                libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if lib.is_null() {
                lib = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY);
            }
            if lib.is_null() {
                return Err("libEGL.so.1 is not loaded".into());
            }
            let symbol = |name: &CStr| {
                let f = libc::dlsym(lib, name.as_ptr());
                if f.is_null() {
                    Err(format!("libEGL has no {name:?}"))
                } else {
                    Ok(f)
                }
            };
            let get_current_display = std::mem::transmute::<*mut c_void, GetCurrentDisplay>(
                symbol(c"eglGetCurrentDisplay")?,
            );
            let display = get_current_display();
            if display.is_null() {
                return Err("no EGL context is current".into());
            }
            Ok(Self {
                display,
                swap_interval: std::mem::transmute::<*mut c_void, SwapInterval>(symbol(
                    c"eglSwapInterval",
                )?),
                query_string: std::mem::transmute::<*mut c_void, QueryString>(symbol(
                    c"eglQueryString",
                )?),
                terminate: std::mem::transmute::<*mut c_void, Terminate>(symbol(c"eglTerminate")?),
                get_error: std::mem::transmute::<*mut c_void, GetError>(symbol(c"eglGetError")?),
            })
        }
    }

    /// Swap interval 0 for the current context's draw surface.
    fn set_swap_interval_zero(&self) -> Result<(), String> {
        // SAFETY: a valid, initialized display with a current context and draw surface.
        if unsafe { (self.swap_interval)(self.display, 0) } == 0 {
            return Err(format!("eglSwapInterval failed: {:#x}", unsafe {
                (self.get_error)()
            }));
        }
        Ok(())
    }

    pub(crate) fn vendor_and_version(&self) -> String {
        let query = |name| {
            // SAFETY: a valid display; the result is a static NUL-terminated string or null.
            let s = unsafe { (self.query_string)(self.display, name) };
            if s.is_null() {
                String::from("?")
            } else {
                // SAFETY: non-null, NUL-terminated, owned by the EGL implementation.
                unsafe { CStr::from_ptr(s) }.to_string_lossy().into_owned()
            }
        };
        format!("{} {}", query(EGL_VENDOR), query(EGL_VERSION))
    }

    /// Terminates the display. Only once nothing uses it: Servo (and its WebGL thread) have
    /// shut down and every page context is destroyed, and before the Wayland connection
    /// closes.
    pub fn terminate(self) -> Result<(), String> {
        // SAFETY: a valid display that nothing uses any more (the caller's promise).
        if unsafe { (self.terminate)(self.display) } == 0 {
            return Err(format!("eglTerminate failed: {:#x}", unsafe {
                (self.get_error)()
            }));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_frame_starts_every_waiting_observer_once() {
        let driver = FrameDriver::default();
        let started = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let started = Arc::clone(&started);
            driver.observe_next_frame(Box::new(move || {
                started.fetch_add(1, Ordering::SeqCst);
            }));
        }
        assert!(driver.is_waiting());
        driver.start_frame();
        assert_eq!(started.load(Ordering::SeqCst), 2);
        assert!(!driver.is_waiting());
        driver.start_frame();
        assert_eq!(started.load(Ordering::SeqCst), 2, "a frame is started once");
    }
}
