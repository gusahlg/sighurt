/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Offscreen OpenGL context for Servo 0.5, on surfman's Mesa surfaceless backend.
//!
//! Owns the EGL display, the GL context and the framebuffer Servo paints into.
//! Does not own Servo, the event loop, or a Wayland or X11 display.
//!
//! Adapted from `SoftwareRenderingContext` in servo-paint-api 0.5
//! (`rendering_context.rs`). That type calls `surfman::Connection::new()`. Here
//! `servo-paint-api` enables surfman's `sm-x11` feature, so that default is the
//! unix multi backend: Wayland, then X11, and only then Mesa surfaceless, and
//! only when opening the connection fails. The surfaceless adapter it would
//! pick is hardware. This context opens `surfman::mesa_surfaceless` directly and
//! chooses the adapter itself: hardware (`create_hardware_adapter`, which is
//! `HardwarePrime` and sets `DRI_PRIME=1`) or software (`create_software_adapter`,
//! which sets `LIBGL_ALWAYS_SOFTWARE=1`).
//!
//! `RenderingContext::connection` has to return that multi `Connection`.
//! `Paint::register_rendering_context` calls `create_adapter` on it and panics
//! when it is `None`. The value returned here is the multi backend's `Alternate`
//! variant around this surfaceless connection. `create_adapter` on that variant
//! returns a hardware adapter and does not touch the process environment;
//! environment variables change only in `create_context_descriptor`, which this
//! context already ran for the adapter it chose. WebGL later creating a device
//! from the stored connection would ask for a window config and reset the
//! environment to hardware. This experiment's pages do not use WebGL.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void, CStr};
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use euclid::default::Size2D;
use gleam::gl::{self, Gl};
use glow::HasContext;
use servo::{DeviceIntRect, RenderingContext, RgbaImage};
use surfman::mesa_surfaceless::connection::Connection as SurfacelessConnection;
use surfman::mesa_surfaceless::context::Context as SurfacelessContext;
use surfman::mesa_surfaceless::device::Device as SurfacelessDevice;
use surfman::{
    Connection, ContextAttributeFlags, ContextAttributes, Error, GLApi, GLVersion, SurfaceAccess,
    SurfaceType,
};

/// Which surfman adapter the surfaceless device is created with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterChoice {
    /// `Connection::create_hardware_adapter`: Mesa's `HardwarePrime`.
    Hardware,
    /// `Connection::create_software_adapter`: llvmpipe via `LIBGL_ALWAYS_SOFTWARE`.
    Software,
}

/// What creating the context did to the process environment, and which GL it got.
#[derive(Clone, Debug)]
pub struct ContextReport {
    /// Always `mesa_surfaceless`: this context never opens another backend.
    pub backend: &'static str,
    /// `HardwarePrime` or `Software`, from the adapter's `Debug`.
    pub adapter: String,
    pub choice: AdapterChoice,
    pub egl_vendor: String,
    pub egl_version: String,
    pub gl_vendor: String,
    pub gl_renderer: String,
    pub gl_version: String,
    /// `LIBGL_ALWAYS_SOFTWARE` before `create_context_descriptor`.
    pub libgl_before: String,
    /// `LIBGL_ALWAYS_SOFTWARE` after the context exists.
    pub libgl_after: String,
    /// `DRI_PRIME` before `create_context_descriptor`.
    pub dri_prime_before: String,
    /// `DRI_PRIME` after the context exists.
    pub dri_prime_after: String,
    /// `std::any::type_name` of `surfman::Connection`, the type Servo stores.
    pub surfman_connection_type: &'static str,
}

/// Servo's rendering context for one offscreen framebuffer.
pub struct OffscreenContext {
    size: Cell<PhysicalSize<u32>>,
    device: RefCell<SurfacelessDevice>,
    context: RefCell<SurfacelessContext>,
    gleam: Rc<dyn Gl>,
    glow: Arc<glow::Context>,
    framebuffer: Cell<glow::Framebuffer>,
    /// Multi-backend connection whose alternate is this surfaceless display. Servo stores it.
    servo_connection: Connection,
    report: ContextReport,
}

impl OffscreenContext {
    /// Opens a surfaceless EGL display and a GL context of `size` pixels.
    ///
    /// # Errors
    ///
    /// The EGL platform was rejected, or the context or framebuffer could not be made.
    /// The message names the stage and the process environment after the attempt.
    pub fn new(size: PhysicalSize<u32>, choice: AdapterChoice) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err(format!("size must be at least 1x1, got {size:?}"));
        }
        let width =
            i32::try_from(size.width).map_err(|_| format!("width {} exceeds i32", size.width))?;
        let height = i32::try_from(size.height)
            .map_err(|_| format!("height {} exceeds i32", size.height))?;

        let connection = SurfacelessConnection::new().map_err(|err| {
            format!(
                "mesa_surfaceless connection failed ({err:?}). \
                 eglGetPlatformDisplay(EGL_PLATFORM_SURFACELESS_MESA) was rejected \
                 (eglGetError {}). {}",
                egl_error_hex(),
                env_now()
            )
        })?;
        let adapter = match choice {
            AdapterChoice::Hardware => connection.create_hardware_adapter(),
            AdapterChoice::Software => connection.create_software_adapter(),
        }
        .map_err(|err| format!("create adapter ({choice:?}): {err:?}. {}", env_now()))?;
        let adapter_name = format!("{adapter:?}");
        let device = connection
            .create_device(&adapter)
            .map_err(|err| format!("create device ({adapter_name}): {err:?}. {}", env_now()))?;

        let libgl_before = env_value("LIBGL_ALWAYS_SOFTWARE");
        let dri_prime_before = env_value("DRI_PRIME");
        let gl_api = connection.gl_api();
        let version = match gl_api {
            GLApi::GLES => GLVersion { major: 3, minor: 0 },
            GLApi::GL => GLVersion { major: 3, minor: 2 },
        };
        // `create_context_descriptor` calls `Adapter::set_environment_variables` before
        // the driver is asked for a config, so the variables below are what EGL sees.
        let descriptor = device
            .create_context_descriptor(&ContextAttributes {
                flags: ContextAttributeFlags::ALPHA
                    | ContextAttributeFlags::DEPTH
                    | ContextAttributeFlags::STENCIL,
                version,
            })
            .map_err(|err| {
                format!(
                    "context descriptor ({adapter_name}, {gl_api:?} {}.{}) failed: {err:?}. {}",
                    version.major,
                    version.minor,
                    env_now()
                )
            })?;
        let mut context = device.create_context(&descriptor, None).map_err(|err| {
            format!(
                "create_context ({adapter_name}) failed: {err:?}. {}",
                env_now()
            )
        })?;

        let surface = match device.create_surface(
            &context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic {
                size: Size2D::new(width, height),
            },
        ) {
            Ok(surface) => surface,
            Err(err) => {
                abandon(&device, &mut context);
                return Err(format!("create_surface failed: {err:?}. {}", env_now()));
            }
        };
        if let Err((err, mut surface)) = device.bind_surface_to_context(&mut context, surface) {
            let _ = device.destroy_surface(&mut context, &mut surface);
            abandon(&device, &mut context);
            return Err(format!("bind_surface failed: {err:?}. {}", env_now()));
        }
        if let Err(err) = device.make_context_current(&context) {
            abandon(&device, &mut context);
            return Err(format!("make_current failed: {err:?}. {}", env_now()));
        }

        let framebuffer = match framebuffer_of(&device, &context) {
            Some(framebuffer) => framebuffer,
            None => {
                abandon(&device, &mut context);
                return Err(format!(
                    "the surfaceless surface has no framebuffer. {}",
                    env_now()
                ));
            }
        };

        // SAFETY: `load_with` / `from_loader_function` call the closure with a
        // GL function name. `get_proc_address` returns that function for this
        // current context, or null, which both loaders accept.
        let gleam_gl =
            unsafe { gl::GlFns::load_with(|symbol| device.get_proc_address(&context, symbol)) };
        let glow_gl = unsafe {
            glow::Context::from_loader_function(|symbol| device.get_proc_address(&context, symbol))
        };
        let (gl_vendor, gl_renderer, gl_version) = unsafe {
            (
                glow_gl.get_parameter_string(glow::VENDOR),
                glow_gl.get_parameter_string(glow::RENDERER),
                glow_gl.get_parameter_string(glow::VERSION),
            )
        };
        let (egl_vendor, egl_version) = egl_strings();
        // `Alternate` is Mesa surfaceless inside the unix multi connection Servo stores.
        // `create_adapter` on it does not call `set_environment_variables`.
        let servo_connection = Connection::Alternate(connection);

        Ok(Self {
            size: Cell::new(size),
            device: RefCell::new(device),
            context: RefCell::new(context),
            gleam: gleam_gl,
            glow: Arc::new(glow_gl),
            framebuffer: Cell::new(framebuffer),
            servo_connection,
            report: ContextReport {
                backend: "mesa_surfaceless",
                adapter: adapter_name,
                choice,
                egl_vendor,
                egl_version,
                gl_vendor,
                gl_renderer,
                gl_version,
                libgl_before,
                libgl_after: env_value("LIBGL_ALWAYS_SOFTWARE"),
                dri_prime_before,
                dri_prime_after: env_value("DRI_PRIME"),
                surfman_connection_type: std::any::type_name::<Connection>(),
            },
        })
    }

    /// Backend, adapter, GL strings and the environment before and after creation.
    pub fn report(&self) -> &ContextReport {
        &self.report
    }

    /// Read the whole framebuffer into `buf` as tightly packed RGBA, top row first.
    ///
    /// `glReadPixels` writes bottom-up; this flips the rows in `buf`.
    pub fn read_rgba(&self, buf: &mut [u8]) -> Result<(), String> {
        let size = self.size.get();
        let bytes = frame_bytes(size.width, size.height)?;
        if buf.len() != bytes {
            return Err(format!(
                "readback buffer is {} bytes, the frame is {bytes}",
                buf.len()
            ));
        }
        self.read_rect(0, 0, size.width, size.height, buf)
    }

    fn read_rect(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        buf: &mut [u8],
    ) -> Result<(), String> {
        let bytes = frame_bytes(width, height)?;
        if buf.len() != bytes {
            return Err(format!(
                "readback buffer is {} bytes, the rectangle is {bytes}",
                buf.len()
            ));
        }
        self.make_current()
            .map_err(|err| format!("make_current before readback: {err:?}"))?;
        let gl = &self.glow;
        let framebuffer = self.framebuffer.get();
        // SAFETY: the surfaceless context is current on this thread, `framebuffer`
        // is the surface's color FBO, and `buf` is exactly width*height*4 bytes.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, 0);
            // Drop a sticky error from WebRender so the check below is this read.
            let _ = gl.get_error();
            gl.read_pixels(
                x,
                y,
                i32::try_from(width).map_err(|_| format!("width {width} exceeds i32"))?,
                i32::try_from(height).map_err(|_| format!("height {height} exceeds i32"))?,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(buf)),
            );
            let err = gl.get_error();
            if err != 0 {
                return Err(format!("glReadPixels failed with GL error 0x{err:x}"));
            }
        }
        flip_rows(buf, width, height);
        Ok(())
    }
}

impl Drop for OffscreenContext {
    fn drop(&mut self) {
        let device = self.device.get_mut();
        let context = self.context.get_mut();
        if let Err(err) = device.destroy_context(context) {
            eprintln!("tui-servo: destroy_context: {err:?}");
        }
    }
}

impl RenderingContext for OffscreenContext {
    fn prepare_for_rendering(&self) {
        let id = self.framebuffer.get().0.get();
        self.gleam.bind_framebuffer(gl::FRAMEBUFFER, id);
    }

    fn read_to_image(&self, source: DeviceIntRect) -> Option<RgbaImage> {
        let width = source.width();
        let height = source.height();
        if width <= 0 || height <= 0 {
            return None;
        }
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return None;
        };
        let mut pixels = vec![0u8; frame_bytes(width, height).ok()?];
        self.read_rect(source.min.x, source.min.y, width, height, &mut pixels)
            .ok()?;
        RgbaImage::from_raw(width, height, pixels)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 || self.size.get() == size {
            return;
        }
        let Some(width) = i32::try_from(size.width).ok() else {
            eprintln!("tui-servo: resize width {} exceeds i32", size.width);
            return;
        };
        let Some(height) = i32::try_from(size.height).ok() else {
            eprintln!("tui-servo: resize height {} exceeds i32", size.height);
            return;
        };
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        let mut old = match device.unbind_surface_from_context(&mut context) {
            Ok(Some(surface)) => surface,
            Ok(None) => {
                eprintln!("tui-servo: resize found no bound surface");
                return;
            }
            Err(err) => {
                eprintln!("tui-servo: resize unbind: {err:?}");
                return;
            }
        };
        if let Err(err) = device.destroy_surface(&mut context, &mut old) {
            eprintln!("tui-servo: resize destroy_surface: {err:?}");
            return;
        }
        let surface = match device.create_surface(
            &context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic {
                size: Size2D::new(width, height),
            },
        ) {
            Ok(surface) => surface,
            Err(err) => {
                eprintln!("tui-servo: resize create_surface: {err:?}");
                return;
            }
        };
        if let Err((err, mut surface)) = device.bind_surface_to_context(&mut context, surface) {
            let _ = device.destroy_surface(&mut context, &mut surface);
            eprintln!("tui-servo: resize bind_surface: {err:?}");
            return;
        }
        let Some(framebuffer) = framebuffer_of(&device, &context) else {
            eprintln!("tui-servo: resize produced no framebuffer");
            return;
        };
        self.framebuffer.set(framebuffer);
        self.size.set(size);
    }

    fn present(&self) {
        // One generic surface, already the buffer Servo paints. A flush makes
        // those commands visible to a later `glReadPixels` on this context.
        unsafe { self.glow.flush() };
    }

    fn make_current(&self) -> Result<(), Error> {
        self.device
            .borrow()
            .make_context_current(&self.context.borrow())
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.gleam.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.glow.clone()
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.servo_connection.clone())
    }
}

fn framebuffer_of(
    device: &SurfacelessDevice,
    context: &SurfacelessContext,
) -> Option<glow::Framebuffer> {
    device
        .context_surface_info(context)
        .ok()
        .flatten()
        .and_then(|info| info.framebuffer_object)
}

fn abandon(device: &SurfacelessDevice, context: &mut SurfacelessContext) {
    if let Err(err) = device.destroy_context(context) {
        eprintln!("tui-servo: destroy_context while recovering: {err:?}");
    }
}

fn frame_bytes(width: u32, height: u32) -> Result<usize, String> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("frame {width}x{height} is too large"))
}

fn flip_rows(buf: &mut [u8], width: u32, height: u32) {
    let stride = width as usize * 4;
    let height = height as usize;
    if stride == 0 || height < 2 {
        return;
    }
    let mut row = vec![0u8; stride];
    for y in 0..height / 2 {
        let top = y * stride;
        let bottom = (height - 1 - y) * stride;
        row.copy_from_slice(&buf[top..top + stride]);
        buf.copy_within(bottom..bottom + stride, top);
        buf[bottom..bottom + stride].copy_from_slice(&row);
    }
}

fn env_value(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unset".to_string())
}

fn env_now() -> String {
    format!(
        "LIBGL_ALWAYS_SOFTWARE={} DRI_PRIME={}",
        env_value("LIBGL_ALWAYS_SOFTWARE"),
        env_value("DRI_PRIME")
    )
}

const EGL_VENDOR: i32 = 0x3053;
const EGL_VERSION: i32 = 0x3054;
const EGL_SUCCESS: i32 = 0x3000;

fn egl_strings() -> (String, String) {
    // SAFETY: the symbols are EGL 1.5's `eglGetCurrentDisplay` and `eglQueryString`.
    // A context is current, so the display pointer is the one surfman initialized.
    // `eglQueryString` returns a static NUL-terminated string, or null.
    unsafe {
        let Some(egl) = EglFns::load() else {
            return ("?".to_string(), "?".to_string());
        };
        let display = (egl.get_current_display)();
        if display.is_null() {
            return ("?".to_string(), "?".to_string());
        }
        (
            egl.query(display, EGL_VENDOR),
            egl.query(display, EGL_VERSION),
        )
    }
}

/// Best-effort `eglGetError` after a failed EGL call. `ok` when EGL is quiet.
pub fn egl_error_hex() -> String {
    // SAFETY: `eglGetError` takes no display and returns an EGLint.
    unsafe {
        let Some(egl) = EglFns::load() else {
            return "libEGL unavailable".to_string();
        };
        let err = (egl.get_error)();
        if err == EGL_SUCCESS {
            "EGL_SUCCESS".to_string()
        } else {
            format!("0x{err:x}")
        }
    }
}

struct EglFns {
    get_current_display: unsafe extern "C" fn() -> *mut c_void,
    query_string: unsafe extern "C" fn(*mut c_void, i32) -> *const c_char,
    get_error: unsafe extern "C" fn() -> i32,
}

impl EglFns {
    fn load() -> Option<Self> {
        // SAFETY: `dlopen` / `dlsym` with NUL-terminated names. Each symbol is
        // cast to the signature EGL 1.5 gives it. `RTLD_NOLOAD` reuses the
        // libEGL surfman already opened; a normal `dlopen` loads it otherwise.
        unsafe {
            let mut handle =
                libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY | libc::RTLD_NOLOAD);
            if handle.is_null() {
                handle = libc::dlopen(c"libEGL.so.1".as_ptr(), libc::RTLD_LAZY);
            }
            if handle.is_null() {
                return None;
            }
            let symbol = |name: &CStr| libc::dlsym(handle, name.as_ptr());
            let get_current = symbol(c"eglGetCurrentDisplay");
            let query = symbol(c"eglQueryString");
            let get_error = symbol(c"eglGetError");
            if get_current.is_null() || query.is_null() || get_error.is_null() {
                return None;
            }
            Some(Self {
                get_current_display: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn() -> *mut c_void,
                >(get_current),
                query_string: std::mem::transmute::<
                    *mut c_void,
                    unsafe extern "C" fn(*mut c_void, i32) -> *const c_char,
                >(query),
                get_error: std::mem::transmute::<*mut c_void, unsafe extern "C" fn() -> i32>(
                    get_error,
                ),
            })
        }
    }

    /// # Safety
    ///
    /// `display` is a live EGL display. `name` is `EGL_VENDOR` or `EGL_VERSION`.
    unsafe fn query(&self, display: *mut c_void, name: i32) -> String {
        let ptr = (self.query_string)(display, name);
        if ptr.is_null() {
            "?".to_string()
        } else {
            CStr::from_ptr(ptr).to_string_lossy().into_owned()
        }
    }
}
