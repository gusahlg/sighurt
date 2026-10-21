/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! One offscreen GL framebuffer per Servo view, on surfman's Mesa surfaceless backend.
//!
//! Owns the EGL display, the GL context and the framebuffer Servo paints into. Does not own
//! Servo, the event loop, a Wayland or X11 display, or the [`CpuTarget`](sighurt_api::present::CpuTarget)
//! buffer a present reads into.
//!
//! Adapted from `experiments/tui-servo/src/context.rs`, itself adapted from
//! `SoftwareRenderingContext` in servo-paint-api (`rendering_context.rs`). That type calls
//! `surfman::Connection::new()`. Here `servo-paint-api` enables surfman's `sm-x11` feature, so
//! that default is the unix multi backend: Wayland, then X11, and only then Mesa surfaceless,
//! and only when opening the connection fails. This context opens `surfman::mesa_surfaceless`
//! directly. The hardware adapter is tried first. The software adapter is used only when that
//! context cannot be made. Neither attempt writes `LIBGL_ALWAYS_SOFTWARE` or `DRI_PRIME`:
//! surfman's `create_context_descriptor` no longer does (`third_party/surfman/SIGHURT-PATCH.md`),
//! and Mesa uses the device this process was started with. The software adapter cannot force
//! llvmpipe on its own.
//!
//! `RenderingContext::connection` returns the multi `Connection`'s `Alternate` around this
//! surfaceless connection. `Paint::register_rendering_context` calls `create_adapter` on it
//! and panics when it is `None`. `create_adapter` on `Alternate` does not touch the process
//! environment. WebGL later creating a device from the stored connection does not either.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use dpi::PhysicalSize;
use euclid::default::Size2D;
use gleam::gl::{self, Gl, ProfilingGl};
use glow::HasContext;
use servo::{DeviceIntRect, RefreshDriver, RenderingContext, RgbaImage};
use sighurt_api::geometry::BufferSize;
use sighurt_api::present::Frame;
use surfman::mesa_surfaceless::connection::Connection as SurfacelessConnection;
use surfman::mesa_surfaceless::context::Context as SurfacelessContext;
use surfman::mesa_surfaceless::device::Device as SurfacelessDevice;
use surfman::mesa_surfaceless::surface::Surface as SurfacelessSurface;
use surfman::{
    Connection, ContextAttributeFlags, ContextAttributes, Error, GLApi, GLVersion, SurfaceAccess,
    SurfaceType,
};

use crate::presentation::{
    physical, Egl, FrameDriver, GlInfo, OffscreenAdapter, PageSurface, Readbacks,
};

/// Servo's rendering context for one offscreen framebuffer.
pub(crate) struct OffscreenContext {
    size: Cell<PhysicalSize<u32>>,
    device: RefCell<SurfacelessDevice>,
    context: RefCell<SurfacelessContext>,
    gleam: Rc<dyn Gl>,
    glow: Arc<glow::Context>,
    framebuffer: Cell<glow::Framebuffer>,
    /// Multi-backend connection whose alternate is this surfaceless display. Servo stores it.
    servo_connection: Connection,
    driver: Rc<FrameDriver>,
    readbacks: Readbacks,
    /// The adapter this context was opened with, not what the renderer string later says.
    adapter: OffscreenAdapter,
}

impl OffscreenContext {
    /// Opens a surfaceless context of `size` on the hardware adapter, or the software adapter
    /// if that fails. The process environment is whatever it was before the call, either way.
    pub(crate) fn open(
        size: BufferSize,
        driver: Rc<FrameDriver>,
        readbacks: &Readbacks,
    ) -> Result<Self, String> {
        if size.width == 0 || size.height == 0 {
            return Err(format!(
                "offscreen surface must be at least 1×1, got {}×{}",
                size.width, size.height
            ));
        }
        match try_adapter(size, OffscreenAdapter::Hardware, &driver, readbacks) {
            Ok(context) => Ok(context),
            Err(hardware) => {
                log::warn!(
                    "servo: offscreen hardware adapter failed ({hardware}); trying the software adapter"
                );
                try_adapter(size, OffscreenAdapter::Software, &driver, readbacks).map_err(
                    |software| {
                        format!(
                            "offscreen GL context: hardware adapter failed ({hardware}); \
                             software adapter failed ({software})"
                        )
                    },
                )
            }
        }
    }

    /// Reads the whole framebuffer into `frame` as ARGB8888, top row first, in one pass.
    /// Counts one readback on success. Glow, not gleam, so an audit of WebRender's reads
    /// does not count this present twice.
    fn read_frame(&self, frame: Frame<'_>) -> Result<(), String> {
        let size = self.size.get();
        if frame.size.width != size.width || frame.size.height != size.height {
            return Err(format!(
                "frame is {}×{}, the surface is {}×{}",
                frame.size.width, frame.size.height, size.width, size.height
            ));
        }
        self.read_pixels_into(
            0,
            0,
            frame.size.width,
            frame.size.height,
            frame.stride,
            frame.pixels,
        )?;
        rgba_bottom_up_to_argb(
            frame.pixels,
            frame.size.width,
            frame.size.height,
            frame.stride,
        );
        let calls = &self.readbacks.read_to_image;
        calls.set(calls.get() + 1);
        Ok(())
    }

    /// `glReadPixels` of RGBA, bottom row first, into `pixels` with `stride` bytes per row.
    fn read_pixels_into(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        stride: usize,
        pixels: &mut [u8],
    ) -> Result<(), String> {
        let row = 4usize
            .checked_mul(width as usize)
            .ok_or_else(|| format!("frame {width}×{height} is too large"))?;
        if stride < row || !stride.is_multiple_of(4) {
            return Err(format!(
                "frame stride {stride} is not a multiple of 4 covering {row} bytes"
            ));
        }
        let need = stride
            .checked_mul(height as usize)
            .ok_or_else(|| format!("frame {width}×{height} is too large"))?;
        if pixels.len() < need {
            return Err(format!(
                "frame buffer is {} bytes, {width}×{height} stride {stride} needs {need}",
                pixels.len()
            ));
        }
        let pack_row =
            i32::try_from(stride / 4).map_err(|_| format!("stride {stride} exceeds i32 pixels"))?;
        let width_px = i32::try_from(width).map_err(|_| format!("width {width} exceeds i32"))?;
        let height_px =
            i32::try_from(height).map_err(|_| format!("height {height} exceeds i32"))?;
        self.make_current()
            .map_err(|err| format!("make_current before readback: {err:?}"))?;
        let gl = &self.glow;
        let framebuffer = self.framebuffer.get();
        // SAFETY: the surfaceless context is current on this thread, `framebuffer` is its
        // color attachment, and `pixels` holds at least `stride * height` bytes.
        // `PACK_ROW_LENGTH` is the stride in pixels so the driver writes the padding itself.
        // Both pixel-store values are restored after `get_error`, which would otherwise
        // report the restore instead of the read.
        let err = unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.flush();
            let old_align = gl.get_parameter_i32(glow::PACK_ALIGNMENT);
            let old_row = gl.get_parameter_i32(glow::PACK_ROW_LENGTH);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, pack_row);
            // Drop a sticky error from WebRender so the check below is this read.
            let _ = gl.get_error();
            gl.read_pixels(
                x,
                y,
                width_px,
                height_px,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels[..need])),
            );
            let err = gl.get_error();
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, old_align);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, old_row);
            err
        };
        if err != 0 {
            return Err(format!("glReadPixels failed with GL error 0x{err:x}"));
        }
        Ok(())
    }
}

impl PageSurface for OffscreenContext {
    fn gl_info(&self, egl: &Egl) -> GlInfo {
        if let Err(error) = self.make_current() {
            log::warn!("servo: offscreen context is not current for GL info: {error:?}");
        }
        let renderer = self.gleam.get_string(gl::RENDERER);
        let lower = renderer.to_lowercase();
        GlInfo {
            software: ["llvmpipe", "softpipe", "swrast", "software"]
                .iter()
                .any(|name| lower.contains(name)),
            vendor: self.gleam.get_string(gl::VENDOR),
            version: self.gleam.get_string(gl::VERSION),
            egl: egl.vendor_and_version(),
            renderer,
        }
    }

    fn surface_is_bound(&self) -> bool {
        true
    }

    fn offscreen_adapter(&self) -> Option<OffscreenAdapter> {
        Some(self.adapter)
    }

    fn read_argb(&self, frame: Frame<'_>) -> Result<(), String> {
        self.read_frame(frame)
    }
}

impl Drop for OffscreenContext {
    fn drop(&mut self) {
        let device = self.device.get_mut();
        let context = self.context.get_mut();
        if let Err(err) = device.destroy_context(context) {
            log::warn!("servo: offscreen destroy_context: {err:?}");
        }
    }
}

impl RenderingContext for OffscreenContext {
    fn prepare_for_rendering(&self) {
        let id = self.framebuffer.get().0.get();
        self.gleam.bind_framebuffer(gl::FRAMEBUFFER, id);
    }

    fn read_to_image(&self, source: DeviceIntRect) -> Option<RgbaImage> {
        let calls = &self.readbacks.read_to_image;
        calls.set(calls.get() + 1);
        log::warn!("servo: page pixels read back to the CPU ({source:?})");
        let width = source.width();
        let height = source.height();
        if width <= 0 || height <= 0 {
            return None;
        }
        let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height)) else {
            return None;
        };
        let stride = 4 * width as usize;
        let mut pixels = vec![0u8; stride * height as usize];
        self.read_pixels_into(
            source.min.x,
            source.min.y,
            width,
            height,
            stride,
            &mut pixels,
        )
        .ok()?;
        flip_rows_tight(&mut pixels, width, height);
        RgbaImage::from_raw(width, height, pixels)
    }

    fn size(&self) -> PhysicalSize<u32> {
        self.size.get()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 || self.size.get() == size {
            return;
        }
        let Ok(width) = i32::try_from(size.width) else {
            log::warn!("servo: offscreen resize width {} exceeds i32", size.width);
            return;
        };
        let Ok(height) = i32::try_from(size.height) else {
            log::warn!("servo: offscreen resize height {} exceeds i32", size.height);
            return;
        };
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        if !replace_surface(&device, &mut context, width, height) {
            return;
        }
        let Some(framebuffer) = framebuffer_of(&device, &context) else {
            log::warn!("servo: offscreen resize produced no framebuffer");
            return;
        };
        self.framebuffer.set(framebuffer);
        self.size.set(size);
    }

    fn present(&self) {
        // One generic surface, already the buffer Servo paints. A flush makes those commands
        // visible to a later `glReadPixels` on this context. The read itself also flushes.
        // SAFETY: `flush` is a GL command on the glow context loaded for this surfaceless
        // context. It does not dereference caller memory.
        unsafe { self.glow.flush() };
    }

    fn make_current(&self) -> Result<(), Error> {
        self.device
            .borrow()
            .make_context_current(&self.context.borrow())
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        Rc::clone(&self.gleam)
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        Arc::clone(&self.glow)
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.servo_connection.clone())
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        Some(Rc::clone(&self.driver) as Rc<dyn RefreshDriver>)
    }
}

/// One adapter attempt. Does not change the process environment (see `SIGHURT-PATCH.md`).
fn try_adapter(
    size: BufferSize,
    adapter: OffscreenAdapter,
    driver: &Rc<FrameDriver>,
    readbacks: &Readbacks,
) -> Result<OffscreenContext, String> {
    build_context(size, adapter, driver, readbacks)
}

fn build_context(
    size: BufferSize,
    adapter: OffscreenAdapter,
    driver: &Rc<FrameDriver>,
    readbacks: &Readbacks,
) -> Result<OffscreenContext, String> {
    let pixels = physical(size);
    let width =
        i32::try_from(pixels.width).map_err(|_| format!("width {} exceeds i32", pixels.width))?;
    let height = i32::try_from(pixels.height)
        .map_err(|_| format!("height {} exceeds i32", pixels.height))?;
    let connection = SurfacelessConnection::new()
        .map_err(|err| format!("mesa_surfaceless connection failed ({err:?})"))?;
    let surfman_adapter = match adapter {
        OffscreenAdapter::Hardware => connection.create_hardware_adapter(),
        OffscreenAdapter::Software => connection.create_software_adapter(),
    }
    .map_err(|err| format!("create adapter ({adapter}): {err:?}"))?;
    let device = connection
        .create_device(&surfman_adapter)
        .map_err(|err| format!("create device ({adapter}): {err:?}"))?;
    let gl_api = connection.gl_api();
    let version = match gl_api {
        GLApi::GLES => GLVersion { major: 3, minor: 0 },
        GLApi::GL => GLVersion { major: 3, minor: 2 },
    };
    // The vendored surfman does not set `LIBGL_ALWAYS_SOFTWARE` or `DRI_PRIME` here.
    let descriptor = device
        .create_context_descriptor(&ContextAttributes {
            flags: ContextAttributeFlags::ALPHA
                | ContextAttributeFlags::DEPTH
                | ContextAttributeFlags::STENCIL,
            version,
        })
        .map_err(|err| {
            format!(
                "context descriptor ({adapter}, {gl_api:?} {}.{}) failed: {err:?}",
                version.major, version.minor
            )
        })?;
    let mut context = device
        .create_context(&descriptor, None)
        .map_err(|err| format!("create_context ({adapter}) failed: {err:?}"))?;
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
            return Err(format!("create_surface failed: {err:?}"));
        }
    };
    if let Err((err, mut surface)) = device.bind_surface_to_context(&mut context, surface) {
        let _ = device.destroy_surface(&mut context, &mut surface);
        abandon(&device, &mut context);
        return Err(format!("bind_surface failed: {err:?}"));
    }
    if let Err(err) = device.make_context_current(&context) {
        abandon(&device, &mut context);
        return Err(format!("make_current failed: {err:?}"));
    }
    let Some(framebuffer) = framebuffer_of(&device, &context) else {
        abandon(&device, &mut context);
        return Err("the surfaceless surface has no framebuffer".into());
    };
    // SAFETY: `load_with` / `from_loader_function` call the closure with a GL function name.
    // `get_proc_address` returns that function for this current context, or null, which both
    // loaders accept.
    let gleam_gl =
        unsafe { gl::GlFns::load_with(|symbol| device.get_proc_address(&context, symbol)) };
    let glow_gl = unsafe {
        glow::Context::from_loader_function(|symbol| device.get_proc_address(&context, symbol))
    };
    let gleam = match &readbacks.gl_reads {
        Some(reads) => {
            let reads = Rc::clone(reads);
            ProfilingGl::wrap(gleam_gl, Duration::ZERO, move |name, _| {
                if name.starts_with("read_pixels") || name.starts_with("get_tex_image") {
                    reads.set(reads.get() + 1);
                }
            })
        }
        None => gleam_gl,
    };
    // `Alternate` is Mesa surfaceless inside the unix multi connection Servo stores.
    let servo_connection = Connection::Alternate(connection);
    Ok(OffscreenContext {
        size: Cell::new(pixels),
        device: RefCell::new(device),
        context: RefCell::new(context),
        gleam,
        glow: Arc::new(glow_gl),
        framebuffer: Cell::new(framebuffer),
        servo_connection,
        driver: Rc::clone(driver),
        readbacks: readbacks.clone(),
        adapter,
    })
}

/// Unbinds the current surface and binds a new one of `width`×`height`. On failure the previous
/// surface is bound again. Returns whether the new surface is the one now bound.
fn replace_surface(
    device: &SurfacelessDevice,
    context: &mut SurfacelessContext,
    width: i32,
    height: i32,
) -> bool {
    let old = match device.unbind_surface_from_context(context) {
        Ok(Some(surface)) => surface,
        Ok(None) => {
            log::warn!("servo: offscreen resize found no bound surface");
            return false;
        }
        Err(err) => {
            log::warn!("servo: offscreen resize unbind: {err:?}");
            return false;
        }
    };
    let new_surface = match device.create_surface(
        context,
        SurfaceAccess::GPUOnly,
        SurfaceType::Generic {
            size: Size2D::new(width, height),
        },
    ) {
        Ok(surface) => surface,
        Err(err) => {
            rebind_or_drop(device, context, old, "create_surface", err);
            return false;
        }
    };
    if let Err((err, mut surface)) = device.bind_surface_to_context(context, new_surface) {
        let _ = device.destroy_surface(context, &mut surface);
        rebind_or_drop(device, context, old, "bind_surface", err);
        return false;
    }
    let mut old = old;
    if let Err(err) = device.destroy_surface(context, &mut old) {
        log::warn!("servo: offscreen resize destroy_surface: {err:?}");
    }
    true
}

fn rebind_or_drop(
    device: &SurfacelessDevice,
    context: &mut SurfacelessContext,
    old: SurfacelessSurface,
    stage: &str,
    err: Error,
) {
    if let Err((bind_err, mut surface)) = device.bind_surface_to_context(context, old) {
        let _ = device.destroy_surface(context, &mut surface);
        log::warn!("servo: offscreen resize {stage}: {err:?}; rebind: {bind_err:?}");
    } else {
        log::warn!("servo: offscreen resize {stage}: {err:?}");
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
        log::warn!("servo: offscreen destroy_context while recovering: {err:?}");
    }
}

/// RGBA, bottom row first, becomes ARGB8888 (B, G, R, A), top row first, in `pixels`.
/// `stride` may be wider than `4 * width`; the padding is left where it is.
pub(crate) fn rgba_bottom_up_to_argb(pixels: &mut [u8], width: u32, height: u32, stride: usize) {
    let row = 4 * width as usize;
    let height = height as usize;
    if row == 0 || height == 0 || stride < row {
        return;
    }
    for y in 0..height / 2 {
        let top = y * stride;
        let bottom = (height - 1 - y) * stride;
        for x in (0..row).step_by(4) {
            let (a, b) = (top + x, bottom + x);
            let (r0, g0, b0, a0) = (pixels[a], pixels[a + 1], pixels[a + 2], pixels[a + 3]);
            let (r1, g1, b1, a1) = (pixels[b], pixels[b + 1], pixels[b + 2], pixels[b + 3]);
            pixels[a] = b1;
            pixels[a + 1] = g1;
            pixels[a + 2] = r1;
            pixels[a + 3] = a1;
            pixels[b] = b0;
            pixels[b + 1] = g0;
            pixels[b + 2] = r0;
            pixels[b + 3] = a0;
        }
    }
    if height % 2 == 1 {
        let mid = (height / 2) * stride;
        for x in (0..row).step_by(4) {
            pixels.swap(mid + x, mid + x + 2);
        }
    }
}

fn flip_rows_tight(pixels: &mut [u8], width: u32, height: u32) {
    let stride = 4 * width as usize;
    let height = height as usize;
    if stride == 0 || height < 2 {
        return;
    }
    let mut row = vec![0u8; stride];
    for y in 0..height / 2 {
        let top = y * stride;
        let bottom = (height - 1 - y) * stride;
        row.copy_from_slice(&pixels[top..top + stride]);
        pixels.copy_within(bottom..bottom + stride, top);
        pixels[bottom..bottom + stride].copy_from_slice(&row);
    }
}

#[cfg(test)]
mod tests {
    use super::rgba_bottom_up_to_argb;

    #[test]
    fn rgba_bottom_up_becomes_argb_top_down_without_moving_padding() {
        // Two rows, stride 12. GL's first row is the bottom of the image: red, green.
        // The second is the top: blue, white. Padding is 0xAB.
        let mut pixels = vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0xAB, 0xAB, 0xAB, 0xAB, 0, 0, 255, 255, 255, 255, 255,
            255, 0xAB, 0xAB, 0xAB, 0xAB,
        ];
        rgba_bottom_up_to_argb(&mut pixels, 2, 2, 12);
        assert_eq!(
            pixels,
            vec![
                // Top row, BGRA: blue, white. Padding untouched.
                255, 0, 0, 255, 255, 255, 255, 255, 0xAB, 0xAB, 0xAB, 0xAB,
                // Bottom row, BGRA: red, green.
                0, 0, 255, 255, 0, 255, 0, 255, 0xAB, 0xAB, 0xAB, 0xAB,
            ]
        );
        let mut one = vec![10, 20, 30, 40];
        rgba_bottom_up_to_argb(&mut one, 1, 1, 4);
        assert_eq!(one, vec![30, 20, 10, 40]);
    }
}
