/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The offscreen GL context Servo renders into, and reading frames back from it.
//!
//! Servo ships a `SoftwareRenderingContext` (CPU rendering) but no headless context for a GPU.
//! [`GpuRenderingContext`] is that missing piece: the same surfman setup on a hardware adapter,
//! with a single offscreen surface because frames are read back rather than presented. Servo
//! keeps its surfman helpers private, so this is adapted from `rendering_context.rs` in
//! `servo-paint-api` 0.5, and this file stays under Servo's MPL-2.0 licence.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::Duration;

use dpi::PhysicalSize;
use euclid::default::Size2D;
use gleam::gl::{self, Gl};
use servo::{DeviceIntRect, RefreshDriver, RenderingContext, RgbaImage, SoftwareRenderingContext};
use surfman::{
    Connection, Context, ContextAttributeFlags, ContextAttributes, Device, Error, GLApi, GLVersion,
    Surface, SurfaceAccess, SurfaceTexture, SurfaceType,
};

/// Creates the context Servo renders into and names its kind.
///
/// `SIG_SERVO_RENDERING=hardware` or `=software` forces one. By default the GPU is tried first
/// and software rendering is the fallback.
pub fn create(size: PhysicalSize<u32>) -> Result<(Rc<dyn RenderingContext>, &'static str), String> {
    let hardware = || match GpuRenderingContext::new(size) {
        Ok(context) => Ok((Rc::new(context) as Rc<dyn RenderingContext>, "hardware")),
        Err(e) => Err(format!(
            "could not create a hardware rendering context: {e:?}"
        )),
    };
    let software = || match SoftwareRenderingContext::new(size) {
        Ok(context) => Ok((Rc::new(context) as Rc<dyn RenderingContext>, "software")),
        Err(e) => Err(format!(
            "could not create a software rendering context: {e:?}"
        )),
    };
    match std::env::var("SIG_SERVO_RENDERING").as_deref() {
        Ok("hardware") => hardware(),
        Ok("software") => software(),
        Ok("") | Err(_) => hardware().or_else(|e| {
            eprintln!("sig-servo: {e}; using software rendering");
            software()
        }),
        Ok(other) => Err(format!(
            "SIG_SERVO_RENDERING must be \"hardware\" or \"software\", not {other:?}"
        )),
    }
}

/// Reads the whole frame `context` renders into as top-down RGBA8 rows, reusing `pixels`.
pub fn read_frame(context: &dyn RenderingContext, pixels: &mut Vec<u8>) {
    let size = context.size2d().to_i32();
    context.prepare_for_rendering();
    read_pixels(
        &*context.gleam_gl_api(),
        DeviceIntRect::from_size(size),
        pixels,
    );
}

/// Reads `rect` of the bound framebuffer into `pixels` as top-down RGBA8 rows.
fn read_pixels(gl: &dyn Gl, rect: DeviceIntRect, pixels: &mut Vec<u8>) {
    let (width, height) = (rect.width() as usize, rect.height() as usize);
    pixels.resize(width * height * 4, 0);
    // Servo's own readback unbinds the vertex array first to dodge a Mesa bug (servo#18606).
    gl.bind_vertex_array(0);
    gl.read_pixels_into_buffer(
        rect.min.x,
        rect.min.y,
        rect.width(),
        rect.height(),
        gl::RGBA,
        gl::UNSIGNED_BYTE,
        pixels,
    );
    // GL rows run bottom to top. Flip in place, a row at a time.
    let stride = width * 4;
    for y in 0..height / 2 {
        let (upper, lower) = pixels.split_at_mut((height - 1 - y) * stride);
        upper[y * stride..][..stride].swap_with_slice(&mut lower[..stride]);
    }
}

/// A headless GL context on the GPU that renders into one offscreen surface.
pub struct GpuRenderingContext {
    device: Device,
    context: RefCell<Context>,
    gleam: Rc<dyn Gl>,
    glow: Arc<glow::Context>,
    size: Cell<PhysicalSize<u32>>,
    refresh_driver: Rc<Refresh60>,
}

impl GpuRenderingContext {
    fn new(size: PhysicalSize<u32>) -> Result<Self, Error> {
        let connection = Connection::new()?;
        // The adapter Servo uses for windows and WebGL, so WebGL surfaces can be shown here.
        let adapter = connection.create_adapter()?;
        let device = connection.create_device(&adapter)?;
        let gl_api = connection.gl_api();
        let version = match gl_api {
            GLApi::GLES => GLVersion::new(3, 0),
            GLApi::GL => GLVersion::new(3, 2),
        };
        let flags = ContextAttributeFlags::ALPHA
            | ContextAttributeFlags::DEPTH
            | ContextAttributeFlags::STENCIL;
        let descriptor = device.create_context_descriptor(&ContextAttributes { flags, version })?;
        let context = device.create_context(&descriptor, None)?;
        let load = |name: &str| device.get_proc_address(&context, name);
        // SAFETY: the function pointers come from the context they are used with.
        let (gleam, glow) = unsafe {
            let gleam = match gl_api {
                GLApi::GL => gl::GlFns::load_with(load),
                GLApi::GLES => gl::GlesFns::load_with(load),
            };
            (gleam, glow::Context::from_loader_function(load))
        };
        // From here on `Drop` destroys the context, also when attaching the surface fails.
        let this = Self {
            device,
            context: RefCell::new(context),
            gleam,
            glow: Arc::new(glow),
            size: Cell::new(size),
            refresh_driver: Rc::new(Refresh60::new()),
        };
        this.attach_surface(size)?;
        this.make_current()?;
        Ok(this)
    }

    /// Renders into a new surface of `size` from now on, replacing the current one.
    fn attach_surface(&self, size: PhysicalSize<u32>) -> Result<(), Error> {
        let device = &self.device;
        let context = &mut *self.context.borrow_mut();
        let size = Size2D::new(size.width as i32, size.height as i32);
        let surface = device.create_surface(
            context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic { size },
        )?;
        if let Some(mut old) = device.unbind_surface_from_context(context)? {
            device.destroy_surface(context, &mut old)?;
        }
        device
            .bind_surface_to_context(context, surface)
            .map_err(|(e, mut surface)| {
                let _ = device.destroy_surface(context, &mut surface);
                e
            })
    }

    fn framebuffer(&self) -> gl::GLuint {
        self.device
            .context_surface_info(&self.context.borrow())
            .ok()
            .flatten()
            .and_then(|info| info.framebuffer_object)
            .map_or(0, |framebuffer| framebuffer.0.get())
    }
}

impl Drop for GpuRenderingContext {
    fn drop(&mut self) {
        // Also destroys the bound surface.
        let _ = self.device.destroy_context(self.context.get_mut());
    }
}

impl RenderingContext for GpuRenderingContext {
    fn prepare_for_rendering(&self) {
        self.gleam
            .bind_framebuffer(gl::FRAMEBUFFER, self.framebuffer());
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<RgbaImage> {
        let mut pixels = Vec::new();
        self.prepare_for_rendering();
        read_pixels(&*self.gleam, source_rectangle, &mut pixels);
        let size = source_rectangle.size().to_u32();
        RgbaImage::from_raw(size.width, size.height, pixels)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        if self.size.get() == size {
            return;
        }
        match self.attach_surface(size) {
            Ok(()) => self.size.set(size),
            Err(e) => eprintln!("sig-servo: failed to resize the rendering surface: {e:?}"),
        }
    }

    /// Frames are read back, never shown, so there is nothing to present.
    fn present(&self) {}

    fn make_current(&self) -> Result<(), Error> {
        self.device.make_context_current(&self.context.borrow())
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.gleam.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.glow.clone()
    }

    fn create_texture(&self, surface: Surface) -> Option<(SurfaceTexture, u32, Size2D<i32>)> {
        let context = &mut *self.context.borrow_mut();
        let size = self.device.surface_info(&surface).size;
        let texture = self
            .device
            .create_surface_texture(context, surface)
            .map_err(|(_, mut surface)| {
                let _ = self.device.destroy_surface(context, &mut surface);
            })
            .ok()?;
        let id = self
            .device
            .surface_texture_object(&texture)
            .map_or(0, |texture| texture.0.get());
        Some((texture, id, size))
    }

    fn destroy_texture(&self, surface_texture: SurfaceTexture) -> Option<Surface> {
        let context = &mut *self.context.borrow_mut();
        self.device
            .destroy_surface_texture(context, surface_texture)
            .ok()
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.device.connection())
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        Some(self.refresh_driver.clone())
    }
}

/// Starts Servo's frames at most 60 times a second instead of Servo's 120. Every frame is read
/// back and copied to the shell, so this halves what an animating page costs.
struct Refresh60(Sender<Box<dyn Fn() + Send>>);

impl Refresh60 {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel::<Box<dyn Fn() + Send>>();
        std::thread::spawn(move || {
            // Like Servo's own driver: a frame's time after the first request, then every
            // request made since.
            while let Ok(first) = rx.recv() {
                std::thread::sleep(Duration::from_micros(16_667));
                first();
                rx.try_iter().for_each(|callback| callback());
            }
        });
        Self(tx)
    }
}

impl RefreshDriver for Refresh60 {
    fn observe_next_frame(&self, callback: Box<dyn Fn() + Send + 'static>) {
        let _ = self.0.send(callback);
    }
}
