/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! One offscreen GL framebuffer per Servo view, on surfman's Mesa surfaceless backend.
//!
//! Owns the EGL display, the GL context, the framebuffer Servo paints into, and one pixel-pack
//! buffer of the current frame. Does not own Servo, the event loop, a Wayland or X11 display,
//! or the [`CpuTarget`](sighurt_api::present::CpuTarget) buffer a present reads into.
//!
//! Adapted from `experiments/tui-servo/src/context.rs`, itself adapted from
//! `SoftwareRenderingContext` in servo-paint-api (`rendering_context.rs`). That type calls
//! `surfman::Connection::new()`. Here `servo-paint-api` enables surfman's `sm-x11` feature, so
//! that default is the unix multi backend: Wayland, then X11, and only then Mesa surfaceless,
//! and only when opening the connection fails. This context opens `surfman::mesa_surfaceless`
//! directly. The hardware adapter is the only attempt. A software-adapter retry uses the same
//! already-initialised EGL display, so it cannot change the result. The call does not write
//! `LIBGL_ALWAYS_SOFTWARE` or `DRI_PRIME`: surfman's `create_context_descriptor` no longer does
//! (`third_party/surfman/SIGHURT-PATCH.md`), and Mesa uses the device this process was started
//! with.
//!
//! `RenderingContext::connection` returns the multi `Connection`'s `Alternate` around this
//! surfaceless connection. `Paint::register_rendering_context` calls `create_adapter` on it
//! and panics when it is `None`. `create_adapter` on `Alternate` does not touch the process
//! environment. WebGL later creating a device from the stored connection does not either.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
#[cfg(test)]
use std::time::Instant;

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
    Connection, ContextAttributeFlags, ContextAttributes, Error, GLApi, GLVersion, Surface,
    SurfaceAccess, SurfaceTexture, SurfaceType,
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
    /// `Open` until a WebGL surface cannot be shared into this context. Then the context
    /// stops trying to texture those canvases.
    webgl_share: Cell<WebGlShare>,
    /// `Some` after the first `GL_BGRA` + `UNSIGNED_BYTE` probe.
    bgra_supported: Cell<Option<bool>>,
    /// One pixel-pack buffer. Never more than the current frame; replaced when the size changes.
    pack: RefCell<Option<PackBuffer>>,
    /// Set when a pack-buffer read fails, so later frames use a client read instead.
    pbo_failed: Cell<bool>,
}

struct PackBuffer {
    buffer: glow::Buffer,
    bytes: usize,
}

impl OffscreenContext {
    /// Opens a surfaceless context of `size` on the hardware adapter. The process environment
    /// is whatever it was before the call. A failure is returned as-is: retrying the software
    /// adapter on this display does not produce a different context.
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
        try_adapter(size, OffscreenAdapter::Hardware, &driver, readbacks).map_err(|hardware| {
            format!("offscreen GL context: hardware adapter failed ({hardware})")
        })
    }

    /// Reads the framebuffer just painted into `frame` as ARGB8888, top row first.
    /// Counts one readback on success. Glow, not gleam, so an audit of WebRender's reads
    /// does not count this present twice. The timing test measures each path itself;
    /// every other caller takes the pack-buffer read.
    fn read_frame(&self, frame: Frame<'_>) -> Result<(), String> {
        let size = self.size.get();
        if frame.size.width != size.width || frame.size.height != size.height {
            return Err(format!(
                "frame is {}×{}, the surface is {}×{}",
                frame.size.width, frame.size.height, size.width, size.height
            ));
        }
        #[cfg(test)]
        if probe_is_on() {
            self.read_frame_probed(frame)?;
        } else {
            self.read_fast(frame)?;
        }
        #[cfg(not(test))]
        self.read_fast(frame)?;
        let calls = &self.readbacks.read_to_image;
        calls.set(calls.get() + 1);
        Ok(())
    }

    /// BGRA into the one pack buffer when this context accepts it, mapped before return.
    /// A failed pack read is remembered; later frames use a client read of the same format.
    /// RGBA plus a swizzle is only the fallback when `GL_BGRA` itself fails.
    fn read_fast(&self, frame: Frame<'_>) -> Result<(), String> {
        let width = frame.size.width;
        let height = frame.size.height;
        let stride = frame.stride;
        let pixels = frame.pixels;
        if self.bgra_ok() && !self.pbo_failed.get() {
            match self.read_pbo_bgra(width, height, stride, pixels) {
                Ok(()) => return Ok(()),
                Err(err) => {
                    log::warn!(
                        "servo: pack-buffer read failed ({err}); later frames read into client memory"
                    );
                    self.pbo_failed.set(true);
                }
            }
        }
        self.read_client(width, height, stride, pixels, self.bgra_ok())
    }

    /// `glReadPixels` of `format`, bottom row first, into `pixels` with `stride` bytes per row.
    /// `format` is `GL_RGBA` or `GL_BGRA`. The bytes are whatever that format writes.
    #[allow(clippy::too_many_arguments)]
    fn read_pixels_into(
        &self,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        stride: usize,
        pixels: &mut [u8],
        format: u32,
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
        #[cfg(test)]
        probe_finish_before_read(&self.glow);
        let gl = &self.glow;
        let framebuffer = self.framebuffer.get();
        // SAFETY: the surfaceless context is current on this thread, `framebuffer` is its
        // color attachment, and `pixels` holds at least `stride * height` bytes.
        // `PACK_ROW_LENGTH` is the stride in pixels so the driver writes the padding itself.
        // Both pixel-store values are restored after `get_error`, which would otherwise
        // report the restore instead of the read. The pack buffer is unbound first: with
        // one bound, this pointer would be read as an offset.
        let err = unsafe {
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
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
                format,
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

    /// The timing test's paths. RGBA is the old client read plus the byte swizzle. BGRA is
    /// that read without a swizzle. PBO is the production pack-buffer read.
    #[cfg(test)]
    fn read_frame_probed(&self, frame: Frame<'_>) -> Result<(), String> {
        let width = frame.size.width;
        let height = frame.size.height;
        let stride = frame.stride;
        let pixels = frame.pixels;
        let path = PROBE.with(|probe| probe.path.get());
        if path == PATH_PBO {
            self.read_pbo_bgra(width, height, stride, pixels)
        } else {
            self.read_client(
                width,
                height,
                stride,
                pixels,
                path == PATH_BGRA && self.bgra_ok(),
            )
        }
    }

    /// One client `glReadPixels`, then either a row flip (BGRA) or the RGBA swizzle.
    fn read_client(
        &self,
        width: u32,
        height: u32,
        stride: usize,
        pixels: &mut [u8],
        bgra: bool,
    ) -> Result<(), String> {
        #[cfg(test)]
        let read_at = probe_stamp();
        self.read_pixels_into(
            0,
            0,
            width,
            height,
            stride,
            pixels,
            if bgra { glow::BGRA } else { glow::RGBA },
        )?;
        #[cfg(test)]
        let cpu_at = probe_stamp();
        if bgra {
            flip_pixel_rows(pixels, width, height, stride);
        } else {
            rgba_bottom_up_to_argb(pixels, width, height, stride);
        }
        #[cfg(test)]
        probe_record(read_at, cpu_at, bgra);
        Ok(())
    }

    /// Reads BGRA into the one pack buffer and copies rows into `pixels` top-down.
    /// Mapping the buffer waits for the read, so the frame returned is the one just painted.
    /// There is one buffer, replaced when the size changes. No second frame is kept.
    fn read_pbo_bgra(
        &self,
        width: u32,
        height: u32,
        stride: usize,
        pixels: &mut [u8],
    ) -> Result<(), String> {
        if !self.bgra_ok() {
            return self.read_client(width, height, stride, pixels, false);
        }
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
        let need_i =
            i32::try_from(need).map_err(|_| format!("frame {width}×{height} is too large"))?;
        let width_px = i32::try_from(width).map_err(|_| format!("width {width} exceeds i32"))?;
        let height_px =
            i32::try_from(height).map_err(|_| format!("height {height} exceeds i32"))?;
        let pack_row =
            i32::try_from(stride / 4).map_err(|_| format!("stride {stride} exceeds i32 pixels"))?;
        self.make_current()
            .map_err(|err| format!("make_current before pack read: {err:?}"))?;
        let gl = &self.glow;
        self.ensure_pack(need, need_i)?;
        let buffer = self
            .pack
            .borrow()
            .as_ref()
            .expect("the pack buffer was just created")
            .buffer;
        let framebuffer = self.framebuffer.get();
        #[cfg(test)]
        let started = probe_stamp();
        // SAFETY: the surfaceless context is current, `buffer` is its pack buffer of `need`
        // bytes, and `framebuffer` is the color attachment. The mapping stays valid until
        // `unmap_buffer` below; the copy finishes first.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(framebuffer));
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(buffer));
            gl.flush();
            let old_align = gl.get_parameter_i32(glow::PACK_ALIGNMENT);
            let old_row = gl.get_parameter_i32(glow::PACK_ROW_LENGTH);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, pack_row);
            let _ = gl.get_error();
            gl.read_pixels(
                0,
                0,
                width_px,
                height_px,
                glow::BGRA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::BufferOffset(0),
            );
            let err = gl.get_error();
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, old_align);
            gl.pixel_store_i32(glow::PACK_ROW_LENGTH, old_row);
            if err != 0 {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
                return Err(format!(
                    "glReadPixels into the pack buffer failed with GL error 0x{err:x}"
                ));
            }
            let ptr = gl.map_buffer_range(glow::PIXEL_PACK_BUFFER, 0, need_i, glow::MAP_READ_BIT);
            #[cfg(test)]
            let read_time = started.map(|started| started.elapsed());
            if ptr.is_null() {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
                return Err("glMapBufferRange of the pack buffer failed".into());
            }
            #[cfg(test)]
            let cpu_at = Instant::now();
            let src = std::slice::from_raw_parts(ptr, need);
            let rows = height as usize;
            for y in 0..rows {
                let from = (rows - 1 - y) * stride;
                let to = y * stride;
                pixels[to..to + row].copy_from_slice(&src[from..from + row]);
            }
            gl.unmap_buffer(glow::PIXEL_PACK_BUFFER);
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
            #[cfg(test)]
            if let Some(read_time) = read_time {
                let cpu = cpu_at.elapsed();
                PROBE.with(|probe| {
                    probe.last_read.set(read_time);
                    probe.last_cpu.set(cpu);
                    probe.last_finish.set(Duration::ZERO);
                    probe.last_bgra.set(true);
                });
            }
        }
        Ok(())
    }

    /// The context's one pack buffer, allocated to `need` bytes. A different size replaces it.
    fn ensure_pack(&self, need: usize, need_i: i32) -> Result<(), String> {
        let gl = &self.glow;
        let mut slot = self.pack.borrow_mut();
        if slot.as_ref().is_some_and(|pack| pack.bytes == need) {
            return Ok(());
        }
        if let Some(old) = slot.take() {
            // SAFETY: `make_current` succeeded in the caller, and `old.buffer` was created here.
            unsafe { gl.delete_buffer(old.buffer) };
        }
        // SAFETY: the surfaceless context is current. The new name is stored on this context
        // and deleted in `Drop` or when the size changes.
        let buffer = unsafe { gl.create_buffer() }.map_err(|err| format!("pack buffer: {err}"))?;
        let err = unsafe {
            gl.bind_buffer(glow::PIXEL_PACK_BUFFER, Some(buffer));
            gl.buffer_data_size(glow::PIXEL_PACK_BUFFER, need_i, glow::STREAM_READ);
            gl.get_error()
        };
        if err != 0 {
            unsafe {
                gl.bind_buffer(glow::PIXEL_PACK_BUFFER, None);
                gl.delete_buffer(buffer);
            }
            return Err(format!("pack buffer alloc failed with GL error 0x{err:x}"));
        }
        *slot = Some(PackBuffer {
            buffer,
            bytes: need,
        });
        Ok(())
    }

    /// Whether `GL_BGRA` + `GL_UNSIGNED_BYTE` reads succeed. Cached after one 1×1 read.
    fn bgra_ok(&self) -> bool {
        if let Some(ok) = self.bgra_supported.get() {
            return ok;
        }
        let mut pixel = [0u8; 4];
        let ok = self
            .read_pixels_into(0, 0, 1, 1, 4, &mut pixel, glow::BGRA)
            .is_ok();
        self.bgra_supported.set(Some(ok));
        ok
    }

    /// A WebGL surface that will not be textured. Destroyed when this context owns it.
    fn dispose_webgl_surface(&self, surface: Surface) {
        let Surface::Alternate(mut surface) = surface else {
            self.note_webgl_refusal(None, false);
            std::mem::forget(surface);
            return;
        };
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        let destroyed = device.destroy_surface(&mut context, &mut surface).is_ok();
        if !destroyed {
            std::mem::forget(surface);
        }
        self.note_webgl_refusal(None, destroyed);
    }

    /// Remember that WebGL surfaces are not textured here, and log that once.
    fn note_webgl_refusal(&self, error: Option<&Error>, destroyed: bool) {
        let state = self.webgl_share.get();
        if webgl_share_logs(state) {
            match error {
                Some(error) => log::warn!(
                    "servo: WebGL surfaces cannot be shared into this offscreen context ({error:?}); further canvases are not textured"
                ),
                None => log::warn!(
                    "servo: a WebGL surface of another backend is not shown; further WebGL surfaces are not textured"
                ),
            }
        }
        self.webgl_share.set(refuse_webgl_surface(state, destroyed));
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
        if let Some(pack) = self.pack.get_mut().take() {
            // The buffer is this context's. Deleting it after the context is gone is invalid.
            if self
                .device
                .get_mut()
                .make_context_current(self.context.get_mut())
                .is_ok()
            {
                // SAFETY: the surfaceless context is current, and `pack.buffer` was created
                // on it. `delete_buffer` frees that name.
                unsafe { self.glow.delete_buffer(pack.buffer) };
            }
        }
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
            glow::RGBA,
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

    /// A WebGL canvas's surface, sampled by WebRender in this context. Its surfaces come from
    /// the connection above, so they are the surfaceless (`Alternate`) kind. The trait's default
    /// would drop the surface, and surfman panics when a surface is dropped undestroyed.
    ///
    /// Sharing either works on this driver or it does not. The first failure is logged once
    /// and remembered; later calls do not try to share again. Servo has already taken the
    /// surface out of the canvas's swap chain. This context destroys the surface when it
    /// owns it, and forgets it otherwise, because dropping it panics. Forgotten surfaces are
    /// counted only up to [`WEBGL_LEAK_CAP`].
    fn create_texture(&self, surface: Surface) -> Option<(SurfaceTexture, u32, Size2D<i32>)> {
        if !webgl_share_allows_attempt(self.webgl_share.get()) {
            self.dispose_webgl_surface(surface);
            return None;
        }
        let Surface::Alternate(surface) = surface else {
            self.note_webgl_refusal(None, false);
            std::mem::forget(surface);
            return None;
        };
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        let size = device.surface_info(&surface).size;
        match device.create_surface_texture(&mut context, surface) {
            Ok(texture) => {
                let object = device
                    .surface_texture_object(&texture)
                    .map_or(0, |object| object.0.get());
                Some((SurfaceTexture::Alternate(texture), object, size))
            }
            Err((error, mut surface)) => {
                let destroyed = device.destroy_surface(&mut context, &mut surface).is_ok();
                if !destroyed {
                    // Only the WebGL context that made it can destroy it. Drop panics.
                    std::mem::forget(surface);
                }
                self.note_webgl_refusal(Some(&error), destroyed);
                None
            }
        }
    }

    fn destroy_texture(&self, texture: SurfaceTexture) -> Option<Surface> {
        let SurfaceTexture::Alternate(texture) = texture else {
            std::mem::forget(texture);
            return None;
        };
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        match device.destroy_surface_texture(&mut context, texture) {
            Ok(surface) => Some(Surface::Alternate(surface)),
            Err((error, texture)) => {
                log::warn!("servo: destroying a WebGL surface texture: {error:?}");
                std::mem::forget(texture);
                None
            }
        }
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.servo_connection.clone())
    }

    fn refresh_driver(&self) -> Option<Rc<dyn RefreshDriver>> {
        Some(Rc::clone(&self.driver) as Rc<dyn RefreshDriver>)
    }
}

/// How many WebGL surfaces a failed share may forget. One is enough to record the failure.
/// Servo keeps handing over a new surface every frame after that; those are destroyed when
/// this context owns them, and forgotten otherwise (dropping one panics). The count does not
/// grow past the cap, and `create_surface_texture` is not called again.
const WEBGL_LEAK_CAP: u8 = 1;

/// Whether this context still tries to texture a WebGL canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WebGlShare {
    /// No share has failed yet.
    Open,
    /// Sharing failed. `leaked` is how many surfaces were forgotten, at most [`WEBGL_LEAK_CAP`].
    Refused { leaked: u8 },
}

fn webgl_share_allows_attempt(state: WebGlShare) -> bool {
    matches!(state, WebGlShare::Open)
}

/// The first transition out of [`WebGlShare::Open`] is the one log line.
fn webgl_share_logs(state: WebGlShare) -> bool {
    webgl_share_allows_attempt(state)
}

/// `destroyed` means this context freed the surface, so it is not counted as a leak.
fn refuse_webgl_surface(state: WebGlShare, destroyed: bool) -> WebGlShare {
    let mut leaked = match state {
        WebGlShare::Open => 0,
        WebGlShare::Refused { leaked } => leaked,
    };
    if !destroyed {
        leaked = leaked.saturating_add(1).min(WEBGL_LEAK_CAP);
    }
    WebGlShare::Refused { leaked }
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
        webgl_share: Cell::new(WebGlShare::Open),
        bgra_supported: Cell::new(None),
        pack: RefCell::new(None),
        pbo_failed: Cell::new(false),
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

/// Reverses rows in place. Channel order stays. Padding past `4 * width` stays put.
fn flip_pixel_rows(pixels: &mut [u8], width: u32, height: u32, stride: usize) {
    let row = 4 * width as usize;
    let height = height as usize;
    if row == 0 || height < 2 || stride < row {
        return;
    }
    debug_assert!(pixels.len() >= stride.saturating_mul(height));
    let mut buf = [0u8; 4096];
    for y in 0..height / 2 {
        let top = y * stride;
        let bottom = (height - 1 - y) * stride;
        let mut offset = 0;
        while offset < row {
            let n = (row - offset).min(buf.len());
            buf[..n].copy_from_slice(&pixels[top + offset..top + offset + n]);
            pixels.copy_within(bottom + offset..bottom + offset + n, top + offset);
            pixels[bottom + offset..bottom + offset + n].copy_from_slice(&buf[..n]);
            offset += n;
        }
    }
}

#[cfg(test)]
const PATH_RGBA: u8 = 0;
#[cfg(test)]
const PATH_BGRA: u8 = 1;
#[cfg(test)]
const PATH_PBO: u8 = 2;

#[cfg(test)]
struct Probe {
    on: Cell<bool>,
    path: Cell<u8>,
    finish: Cell<bool>,
    pending_finish: Cell<Duration>,
    last_read: Cell<Duration>,
    last_cpu: Cell<Duration>,
    last_finish: Cell<Duration>,
    last_bgra: Cell<bool>,
}

#[cfg(test)]
thread_local! {
    static PROBE: Probe = const {
        Probe {
            on: Cell::new(false),
            path: Cell::new(PATH_RGBA),
            finish: Cell::new(false),
            pending_finish: Cell::new(Duration::ZERO),
            last_read: Cell::new(Duration::ZERO),
            last_cpu: Cell::new(Duration::ZERO),
            last_finish: Cell::new(Duration::ZERO),
            last_bgra: Cell::new(false),
        }
    };
}

#[cfg(test)]
struct ProbeSample {
    read: Duration,
    cpu: Duration,
    finish: Duration,
    bgra: bool,
}

#[cfg(test)]
fn probe_configure(on: bool, path: u8, finish: bool) {
    PROBE.with(|probe| {
        probe.on.set(on);
        probe.path.set(path);
        probe.finish.set(finish);
    });
}

#[cfg(test)]
fn probe_sample() -> ProbeSample {
    PROBE.with(|probe| ProbeSample {
        read: probe.last_read.get(),
        cpu: probe.last_cpu.get(),
        finish: probe.last_finish.get(),
        bgra: probe.last_bgra.get(),
    })
}

#[cfg(test)]
fn probe_is_on() -> bool {
    PROBE.with(|probe| probe.on.get())
}

#[cfg(test)]
fn probe_stamp() -> Option<Instant> {
    PROBE.with(|probe| probe.on.get().then(Instant::now))
}

#[cfg(test)]
fn probe_record(read_at: Option<Instant>, cpu_at: Option<Instant>, bgra: bool) {
    let (Some(read_at), Some(cpu_at)) = (read_at, cpu_at) else {
        return;
    };
    let cpu_done = Instant::now();
    PROBE.with(|probe| {
        let finish = probe.pending_finish.replace(Duration::ZERO);
        probe.last_read.set(
            cpu_at
                .saturating_duration_since(read_at)
                .saturating_sub(finish),
        );
        probe
            .last_cpu
            .set(cpu_done.saturating_duration_since(cpu_at));
        probe.last_finish.set(finish);
        probe.last_bgra.set(bgra);
    });
}

/// When the timing test asks, wait for the GPU before `glReadPixels` and record that wait
/// apart from the transfer. The context is already current.
#[cfg(test)]
fn probe_finish_before_read(gl: &glow::Context) {
    PROBE.with(|probe| {
        if !probe.finish.get() {
            probe.pending_finish.set(Duration::ZERO);
            return;
        }
        let started = Instant::now();
        // SAFETY: the caller made the surfaceless context current. `finish` takes no pointers.
        unsafe { gl.finish() };
        probe.pending_finish.set(started.elapsed());
    });
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
#[cfg_attr(debug_assertions, allow(dead_code, unused_imports))]
mod tests {
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use sighurt_api::engine::{
        EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
    };
    use sighurt_api::events::EngineEvent;
    use sighurt_api::geometry::{BufferSize, Metrics, Scale, WindowRect};
    use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
    use sighurt_api::present::{CpuTarget, Frame};

    use super::{
        flip_pixel_rows, refuse_webgl_surface, rgba_bottom_up_to_argb, webgl_share_allows_attempt,
        webgl_share_logs, WebGlShare, PATH_BGRA, PATH_PBO, PATH_RGBA, WEBGL_LEAK_CAP,
    };

    /// The page area from the terminal's diagnostics, tight stride.
    const WIDTH: u32 = 1876;
    const HEIGHT: u32 = 1316;
    /// Header `#e67e22` as B, G, R, A.
    const ORANGE: [u8; 4] = [0x22, 0x7e, 0xe6, 0xff];
    /// Body `#1b4f72`.
    const BLUE: [u8; 4] = [0x72, 0x4f, 0x1b, 0xff];
    /// WebGL box cleared to `#00ff00`.
    const GREEN: [u8; 4] = [0x00, 0xff, 0x00, 0xff];
    /// Corner mark `#ff00ff`.
    const MAGENTA: [u8; 4] = [0xff, 0x00, 0xff, 0xff];

    #[test]
    fn webgl_share_failure_is_remembered_and_the_leak_count_saturates() {
        assert!(webgl_share_allows_attempt(WebGlShare::Open));
        assert!(webgl_share_logs(WebGlShare::Open));

        let once = refuse_webgl_surface(WebGlShare::Open, false);
        assert_eq!(once, WebGlShare::Refused { leaked: 1 });
        assert!(!webgl_share_allows_attempt(once));
        assert!(!webgl_share_logs(once));

        let again = refuse_webgl_surface(once, false);
        assert_eq!(
            again,
            WebGlShare::Refused {
                leaked: WEBGL_LEAK_CAP
            }
        );
        let past = refuse_webgl_surface(again, false);
        assert_eq!(past, again, "the forgotten count stays at the cap");

        // A surface this context manages to destroy is refused, and not counted.
        let freed = refuse_webgl_surface(WebGlShare::Open, true);
        assert_eq!(freed, WebGlShare::Refused { leaked: 0 });
        assert!(!webgl_share_allows_attempt(freed));
        let freed_then_leaked = refuse_webgl_surface(freed, false);
        assert_eq!(freed_then_leaked, WebGlShare::Refused { leaked: 1 });
    }

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

    #[test]
    fn flip_pixel_rows_reverses_rows_and_keeps_padding() {
        let mut pixels = vec![
            1, 2, 3, 4, 5, 6, 7, 8, 0xAB, 0xAB, 0xAB, 0xAB, 9, 10, 11, 12, 13, 14, 15, 16, 0xCD,
            0xCD, 0xCD, 0xCD,
        ];
        flip_pixel_rows(&mut pixels, 2, 2, 12);
        assert_eq!(
            pixels,
            vec![
                // Pixel bytes swapped. Each row's padding stays where it was.
                9, 10, 11, 12, 13, 14, 15, 16, 0xAB, 0xAB, 0xAB, 0xAB, 1, 2, 3, 4, 5, 6, 7, 8, 0xCD,
                0xCD, 0xCD, 0xCD,
            ]
        );
        let mut odd = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        flip_pixel_rows(&mut odd, 1, 3, 4);
        assert_eq!(odd, vec![9, 10, 11, 12, 5, 6, 7, 8, 1, 2, 3, 4]);
    }

    /// Release timing of paint, the readback and the CPU pass. Prints; asserts no absolute times.
    /// Run: `tools/cargo test --release -p sighurt-engine-servo --lib -- --ignored --exact
    /// offscreen::tests::graphics_path_timing`.
    #[test]
    #[ignore = "release timing; run --release --ignored --exact offscreen::tests::graphics_path_timing"]
    fn graphics_path_timing() {
        graphics_path_timing_release();
    }

    #[cfg(debug_assertions)]
    fn graphics_path_timing_release() {
        panic!("timing numbers are from a release build");
    }

    #[cfg(not(debug_assertions))]
    fn graphics_path_timing_release() {
        let mut log = Log::create();
        write_header(&mut log);
        bench_cpu(&mut log);
        let mut harness = match Harness::open(log) {
            Ok(harness) => harness,
            Err(err) => panic!("offscreen runtime: {err}"),
        };
        if let Err(err) = harness.run() {
            panic!("graphics path timing: {err}");
        }
    }

    struct Log {
        file: std::fs::File,
    }

    impl Log {
        fn create() -> Self {
            let file = std::fs::File::create("/tmp/sighurt-gfx-servo.txt").expect("timing log");
            Self { file }
        }

        fn say(&mut self, line: &str) {
            println!("{line}");
            let _ = writeln!(self.file, "{line}");
            let _ = self.file.flush();
            let _ = std::io::stdout().flush();
        }
    }

    fn write_header(log: &mut Log) {
        let bytes = 4 * WIDTH as usize * HEIGHT as usize;
        log.say("offscreen graphics timing");
        log.say(&format!("machine: {}", cpu_model()));
        log.say("build: release (debug_assertions off)");
        log.say(&format!(
            "frame: {WIDTH}x{HEIGHT} tight stride, {bytes} bytes, premultiplied BGRA top-down"
        ));
        log.say(
            "method: 20 frames after 4 warmup where a workload asks for 20; median is samples[n/2] after sort; p90 is nearest-rank ceil(n*0.90); wall is EngineRuntime::present only (pump is outside it); paint is WebView::paint; read is glReadPixels with an optional glFinish subtracted; cpu is the swizzle, the row flip, or the pack-buffer copy; other = wall - paint - read - cpu - finish; one reused tight buffer so the readback allocates only on the first frame",
        );
    }

    fn cpu_model() -> String {
        let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
            return "unknown".into();
        };
        text.lines()
            .find_map(|line| line.strip_prefix("model name"))
            .and_then(|line| line.split(':').nth(1))
            .map(|model| model.trim().to_string())
            .unwrap_or_else(|| "unknown".into())
    }

    fn bench_cpu(log: &mut Log) {
        let stride = 4 * WIDTH as usize;
        let bytes = stride * HEIGHT as usize;
        let mut src = vec![0u8; bytes];
        for (i, byte) in src.iter_mut().enumerate() {
            *byte = (i.wrapping_mul(17).wrapping_add(3)) as u8;
        }
        let mut byte_out = src.clone();
        let mut word_out = src.clone();
        rgba_bottom_up_to_argb(&mut byte_out, WIDTH, HEIGHT, stride);
        word_swizzle_flip(&mut word_out, WIDTH, HEIGHT, stride);
        assert_eq!(byte_out, word_out, "word swizzle matches the byte swizzle");
        assert_padded_swizzle();

        let bgra_src = byte_out.clone();
        let mut flip_out = bgra_src.clone();
        let mut byte_ms = Vec::new();
        let mut word_ms = Vec::new();
        let mut flip_ms = Vec::new();
        for _ in 0..20 {
            byte_out.copy_from_slice(&src);
            let started = Instant::now();
            rgba_bottom_up_to_argb(&mut byte_out, WIDTH, HEIGHT, stride);
            std::hint::black_box(byte_out.as_ptr());
            byte_ms.push(started.elapsed());

            word_out.copy_from_slice(&src);
            let started = Instant::now();
            word_swizzle_flip(&mut word_out, WIDTH, HEIGHT, stride);
            std::hint::black_box(word_out.as_ptr());
            word_ms.push(started.elapsed());

            flip_out.copy_from_slice(&bgra_src);
            let started = Instant::now();
            flip_pixel_rows(&mut flip_out, WIDTH, HEIGHT, stride);
            std::hint::black_box(flip_out.as_ptr());
            flip_ms.push(started.elapsed());
        }
        assert_eq!(byte_out, word_out, "timed word swizzle still matches");
        report_durations(log, "cpu byte-swizzle+flip", &byte_ms);
        report_durations(log, "cpu word-swizzle+flip", &word_ms);
        report_durations(log, "cpu row-flip only", &flip_ms);
    }

    fn assert_padded_swizzle() {
        let mut byte_out = vec![
            10, 20, 30, 40, 0xAB, 0xAB, 0xAB, 0xAB, 50, 60, 70, 80, 0xCD, 0xCD, 0xCD, 0xCD, 90,
            100, 110, 120, 0xEF, 0xEF, 0xEF, 0xEF,
        ];
        let mut word_out = byte_out.clone();
        rgba_bottom_up_to_argb(&mut byte_out, 1, 3, 8);
        word_swizzle_flip(&mut word_out, 1, 3, 8);
        assert_eq!(byte_out, word_out, "padded odd-height swizzle");
    }

    /// RGBA bottom-up to BGRA top-down, one `u32` per pixel. Same bytes as the byte loop.
    fn word_swizzle_flip(pixels: &mut [u8], width: u32, height: u32, stride: usize) {
        let row = 4 * width as usize;
        let height = height as usize;
        if row == 0 || height == 0 || stride < row || !stride.is_multiple_of(4) {
            return;
        }
        for y in 0..height / 2 {
            let top = y * stride;
            let bottom = (height - 1 - y) * stride;
            let mut x = 0;
            while x < row {
                let above = load_pixel(pixels, top + x);
                let below = load_pixel(pixels, bottom + x);
                store_pixel(pixels, top + x, swizzle_word(below));
                store_pixel(pixels, bottom + x, swizzle_word(above));
                x += 4;
            }
        }
        if height % 2 == 1 {
            let mid = (height / 2) * stride;
            let mut x = 0;
            while x < row {
                let pixel = load_pixel(pixels, mid + x);
                store_pixel(pixels, mid + x, swizzle_word(pixel));
                x += 4;
            }
        }
    }

    fn swizzle_word(pixel: u32) -> u32 {
        (pixel & 0xFF00_FF00) | ((pixel & 0x00FF_0000) >> 16) | ((pixel & 0x0000_00FF) << 16)
    }

    fn load_pixel(pixels: &[u8], index: usize) -> u32 {
        u32::from_le_bytes(pixels[index..index + 4].try_into().expect("pixel"))
    }

    fn store_pixel(pixels: &mut [u8], index: usize, pixel: u32) {
        pixels[index..index + 4].copy_from_slice(&pixel.to_le_bytes());
    }

    fn report_durations(log: &mut Log, label: &str, samples: &[Duration]) {
        if samples.is_empty() {
            log.say(&format!("{label}: n=0"));
            return;
        }
        let mut micros: Vec<u128> = samples.iter().map(Duration::as_micros).collect();
        micros.sort_unstable();
        let n = micros.len();
        let p90 = micros[p90_index(n)];
        log.say(&format!(
            "{label}: n={n} min={:.2} med={:.2} p90={:.2} max={:.2} ms",
            ms_u(micros[0]),
            ms_u(micros[n / 2]),
            ms_u(p90),
            ms_u(micros[n - 1]),
        ));
    }

    fn p90_index(n: usize) -> usize {
        let rank = (n as f64 * 0.90).ceil() as usize;
        rank.saturating_sub(1).min(n - 1)
    }

    fn ms_u(micros: u128) -> f64 {
        micros as f64 / 1000.0
    }

    fn ms(duration: Duration) -> f64 {
        duration.as_secs_f64() * 1e3
    }

    struct FrameTime {
        paint: Duration,
        read: Duration,
        cpu: Duration,
        finish: Duration,
        wall: Duration,
        swap: Duration,
        bgra: bool,
    }

    struct Collected {
        frames: Vec<FrameTime>,
        hashes: Vec<u64>,
    }

    struct Slot {
        pixels: Vec<u8>,
    }

    struct Permit {
        epoch: LayoutEpoch,
        size: BufferSize,
    }

    struct Mem {
        metrics: Metrics,
        slot: Arc<Mutex<Slot>>,
    }

    impl CpuTarget for Mem {
        type Permit = Permit;

        fn metrics(&self) -> Metrics {
            self.metrics
        }

        fn permit_epoch(permit: &Permit) -> LayoutEpoch {
            permit.epoch
        }

        fn submit(
            &mut self,
            permit: Permit,
            paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
        ) -> Result<(), String> {
            let stride = 4 * permit.size.width as usize;
            let need = stride * permit.size.height as usize;
            let mut slot = self.slot.lock().expect("frame slot");
            if slot.pixels.len() != need {
                slot.pixels.resize(need, 0);
            }
            paint(Frame {
                pixels: &mut slot.pixels,
                size: permit.size,
                stride,
            })?;
            Ok(())
        }
    }

    struct EnvGuard {
        wayland: Option<std::ffi::OsString>,
        display: Option<std::ffi::OsString>,
    }

    impl EnvGuard {
        fn hide_display() -> Self {
            let guard = Self {
                wayland: std::env::var_os("WAYLAND_DISPLAY"),
                display: std::env::var_os("DISPLAY"),
            };
            std::env::remove_var("WAYLAND_DISPLAY");
            std::env::remove_var("DISPLAY");
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore_env("WAYLAND_DISPLAY", &self.wayland);
            restore_env("DISPLAY", &self.display);
        }
    }

    fn restore_env(key: &str, value: &Option<std::ffi::OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    struct Harness {
        runtime: crate::ServoRuntime<crate::Offscreen<Mem>>,
        slot: Arc<Mutex<Slot>>,
        view: ViewId,
        epoch: LayoutEpoch,
        size: BufferSize,
        log: Log,
        env: EnvGuard,
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            self.runtime.shutdown();
            if self.runtime.diagnostics().leaked {
                self.log.say("shutdown leaked servo");
            }
            // Restored when `env` drops, after Servo has shut down.
            let _env = &self.env;
        }
    }

    impl Harness {
        fn open(mut log: Log) -> Result<Self, String> {
            let env = EnvGuard::hide_display();
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
            std::env::set_var("RUST_LOG", "error");
            let mut runtime = crate::ServoRuntime::<crate::Offscreen<Mem>>::new(
                crate::ServoConfig::default(),
                EngineWaker::new(|| {}),
            );
            runtime.setup_logging();
            let metrics = page_metrics();
            let epoch = metrics.epoch();
            let size = metrics.buffer();
            let slot = Arc::new(Mutex::new(Slot { pixels: Vec::new() }));
            let view = ViewId::next();
            if let Err(err) = runtime.open_view(
                view,
                ViewOptions::default(),
                crate::Offscreen(Mem {
                    metrics,
                    slot: Arc::clone(&slot),
                }),
            ) {
                runtime.shutdown();
                return Err(format!("open_view: {err:?}"));
            }
            let diagnostics = runtime.diagnostics();
            let gl = diagnostics.gl.as_ref();
            log.say(&format!(
                "gl: adapter={:?} software={:?} renderer={:?} vendor={:?} version={:?} egl={:?}",
                diagnostics.offscreen_adapter,
                gl.map(|info| info.software),
                gl.map(|info| info.renderer.as_str()),
                gl.map(|info| info.vendor.as_str()),
                gl.map(|info| info.version.as_str()),
                gl.map(|info| info.egl.as_str()),
            ));
            Ok(Self {
                runtime,
                slot,
                view,
                epoch,
                size,
                log,
                env,
            })
        }

        fn run(&mut self) -> Result<(), String> {
            self.phase_static()?;
            self.phase_redraw()?;
            let css = write_page("css.html", CSS_BODY, CSS_SCRIPT)?;
            self.phase_motion("css-anim", &css, Duration::from_secs(120))?;
            let canvas = write_page("canvas.html", CANVAS_BODY, CANVAS_SCRIPT)?;
            self.phase_motion("canvas", &canvas, Duration::from_secs(90))?;
            self.runtime.shutdown();
            if self.runtime.diagnostics().leaked {
                return Err("servo leaked on shutdown".into());
            }
            self.log.say("done");
            Ok(())
        }

        fn phase_static(&mut self) -> Result<(), String> {
            let page = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/pages/offscreen.html");
            self.navigate(&page)?;
            let load = self.settle_static()?;
            self.report("static-load rgba", &load, &[]);
            let last = load.last().expect("settle submitted a frame");
            self.log.say(&format!(
                "static-load last: paint={:.2} read={:.2} cpu={:.2} finish={:.2} wall={:.2} other={:.2} swap={:.2} ms",
                ms(last.paint),
                ms(last.read),
                ms(last.cpu),
                ms(last.finish),
                ms(last.wall),
                ms(other(last)),
                ms(last.swap),
            ));
            let reference = self.snapshot(PATH_RGBA)?;
            self.log
                .say(&format!("identity rgba: reference {}", probe_note()));
            for (name, path) in [("bgra", PATH_BGRA), ("pbo", PATH_PBO)] {
                let frame = self.snapshot(path)?;
                self.log.say(&format!(
                    "identity {name}: {} {}",
                    probe_note(),
                    diff_line(&reference, &frame)
                ));
            }
            let forced = self.collect_forced(PATH_RGBA, 4, 20)?;
            self.report("static-forced rgba", &forced.frames, &forced.hashes);
            Ok(())
        }

        fn phase_redraw(&mut self) -> Result<(), String> {
            let page = write_page("redraw.html", REDRAW_BODY, REDRAW_SCRIPT)?;
            self.navigate(&page)?;
            let rgba = self.collect(PATH_RGBA, false, 4, 20, Duration::from_secs(120))?;
            self.report("static-redraw rgba", &rgba.frames, &rgba.hashes);
            if rgba.frames.is_empty() {
                return Err("static-redraw rgba produced no measured frame".into());
            }
            let anchor = match colours(&self.copy_pixels()) {
                Ok(()) => "match".to_string(),
                Err(err) => err,
            };
            self.log.say(&format!("static-redraw anchor: {anchor}"));
            let bgra = self.collect(PATH_BGRA, false, 4, 20, Duration::from_secs(120))?;
            self.report("static-redraw bgra", &bgra.frames, &bgra.hashes);
            let pbo = self.collect(PATH_PBO, false, 4, 20, Duration::from_secs(120))?;
            self.report("static-redraw pbo", &pbo.frames, &pbo.hashes);
            let split = self.collect(PATH_RGBA, true, 4, 12, Duration::from_secs(90))?;
            self.report("static-redraw rgba+glFinish", &split.frames, &split.hashes);
            Ok(())
        }

        fn phase_motion(
            &mut self,
            label: &str,
            page: &Path,
            deadline: Duration,
        ) -> Result<(), String> {
            self.navigate(page)?;
            for (name, path) in [("rgba", PATH_RGBA), ("bgra", PATH_BGRA), ("pbo", PATH_PBO)] {
                let got = self.collect(path, false, 4, 20, deadline)?;
                self.report(&format!("{label} {name}"), &got.frames, &got.hashes);
            }
            Ok(())
        }

        fn navigate(&mut self, page: &Path) -> Result<(), String> {
            let url =
                url::Url::from_file_path(page).map_err(|_| format!("file url for {page:?}"))?;
            let navigation = NavigationId::next();
            self.runtime
                .command(
                    self.view,
                    EngineCommand::Navigate {
                        navigation,
                        url: url.to_string(),
                    },
                )
                .map_err(|err| format!("navigate: {err:?}"))?;
            self.wait_loaded()
        }

        fn wait_loaded(&mut self) -> Result<(), String> {
            let start = Instant::now();
            let mut committed = false;
            let mut loading = None;
            let mut ready = false;
            let mut recent = Vec::new();
            while start.elapsed() < Duration::from_secs(120) {
                let mut events = Vec::new();
                let status = self.runtime.pump(&mut events);
                for event in &events {
                    match event {
                        EngineEvent::NavigationCommitted { .. } => committed = true,
                        EngineEvent::LoadingChanged { loading: flag, .. } => loading = Some(*flag),
                        EngineEvent::FrameReady { .. } => ready = true,
                        _ => {}
                    }
                    recent.push(format!("{event:?}"));
                    if recent.len() > 8 {
                        recent.remove(0);
                    }
                }
                if committed && loading == Some(false) && ready {
                    return Ok(());
                }
                if !matches!(status, PumpStatus::Pending { deadline: None }) {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            Err(format!("load timed out; last events: {recent:?}"))
        }

        fn settle_static(&mut self) -> Result<Vec<FrameTime>, String> {
            super::probe_configure(true, PATH_RGBA, false);
            let start = Instant::now();
            let mut frames = Vec::new();
            let mut last = String::from("no submitted frame");
            while start.elapsed() < Duration::from_secs(60) {
                let mut events = Vec::new();
                self.runtime.pump(&mut events);
                if let Some(frame) = self.present_once()? {
                    frames.push(frame);
                    match colours(&self.copy_pixels()) {
                        Ok(()) => return Ok(frames),
                        Err(err) => last = err,
                    }
                }
            }
            Err(format!("static colours did not arrive ({last})"))
        }

        fn snapshot(&mut self, path: u8) -> Result<Vec<u8>, String> {
            super::probe_configure(true, path, false);
            self.bump_epoch()?;
            let start = Instant::now();
            let mut last_pixels = None;
            let mut tries = 0;
            while start.elapsed() < Duration::from_secs(20) && tries < 8 {
                tries += 1;
                let mut events = Vec::new();
                self.runtime.pump(&mut events);
                if self.present_once()?.is_some() {
                    let pixels = self.copy_pixels();
                    if colours(&pixels).is_ok() {
                        return Ok(pixels);
                    }
                    last_pixels = Some(pixels);
                }
            }
            let Some(pixels) = last_pixels else {
                return Err(format!("snapshot path {path}: no submitted frame"));
            };
            self.log.say(&format!(
                "snapshot path {path}: colours never matched; using the last frame"
            ));
            Ok(pixels)
        }

        fn collect_forced(
            &mut self,
            path: u8,
            warmup: usize,
            n: usize,
        ) -> Result<Collected, String> {
            super::probe_configure(true, path, false);
            let mut frames = Vec::new();
            let mut hashes = Vec::new();
            let mut warmed = 0;
            while frames.len() < n {
                self.bump_epoch()?;
                let mut got = None;
                for _ in 0..6 {
                    let mut events = Vec::new();
                    self.runtime.pump(&mut events);
                    if let Some(frame) = self.present_once()? {
                        got = Some(frame);
                        break;
                    }
                }
                let Some(frame) = got else {
                    return Err(format!(
                        "forced present produced nothing (warmup {warmed}, measured {})",
                        frames.len()
                    ));
                };
                if warmed < warmup {
                    warmed += 1;
                    continue;
                }
                hashes.push(self.hash_slot());
                frames.push(frame);
            }
            Ok(Collected { frames, hashes })
        }

        fn collect(
            &mut self,
            path: u8,
            finish: bool,
            warmup: usize,
            n: usize,
            deadline: Duration,
        ) -> Result<Collected, String> {
            super::probe_configure(true, path, finish);
            let mut frames = Vec::new();
            let mut hashes = Vec::new();
            let mut warmed = 0usize;
            let mut misses = 0usize;
            let start = Instant::now();
            while frames.len() < n && start.elapsed() < deadline && misses < 40 {
                let mut events = Vec::new();
                let status = self.runtime.pump(&mut events);
                match self.present_once()? {
                    Some(frame) => {
                        misses = 0;
                        if warmed < warmup {
                            warmed += 1;
                        } else {
                            hashes.push(self.hash_slot());
                            frames.push(frame);
                        }
                    }
                    None => {
                        misses += 1;
                        if !matches!(status, PumpStatus::Pending { deadline: None }) {
                            std::thread::sleep(Duration::from_millis(2));
                        }
                    }
                }
            }
            Ok(Collected { frames, hashes })
        }

        fn present_once(&mut self) -> Result<Option<FrameTime>, String> {
            let wall_at = Instant::now();
            let outcome = self
                .runtime
                .present(
                    self.view,
                    Permit {
                        epoch: self.epoch,
                        size: self.size,
                    },
                )
                .map_err(|err| format!("present: {err:?}"))?;
            let wall = wall_at.elapsed();
            if outcome != PresentOutcome::Submitted {
                return Ok(None);
            }
            let timing = self.runtime.diagnostics().timing;
            let probe = super::probe_sample();
            Ok(Some(FrameTime {
                paint: timing.paint,
                read: probe.read,
                cpu: probe.cpu,
                finish: probe.finish,
                wall,
                swap: timing.swap,
                bgra: probe.bgra,
            }))
        }

        fn bump_epoch(&mut self) -> Result<(), String> {
            let metrics = page_metrics();
            self.epoch = metrics.epoch();
            self.runtime
                .command(self.view, EngineCommand::SetMetrics(metrics))
                .map_err(|err| format!("set metrics: {err:?}"))
        }

        fn copy_pixels(&self) -> Vec<u8> {
            self.slot.lock().expect("frame slot").pixels.clone()
        }

        fn hash_slot(&self) -> u64 {
            hash_pixels(&self.slot.lock().expect("frame slot").pixels)
        }

        fn report(&mut self, label: &str, frames: &[FrameTime], hashes: &[u64]) {
            let bgra = frames.last().is_some_and(|frame| frame.bgra);
            let hashes_note = if hashes.is_empty() {
                String::new()
            } else {
                format!(" unique_hashes={}", unique(hashes))
            };
            self.log.say(&format!(
                "{label}: n={} bgra_read={bgra}{hashes_note}",
                frames.len()
            ));
            report_durations(
                &mut self.log,
                &format!("{label} paint"),
                &map_dur(frames, |f| f.paint),
            );
            report_durations(
                &mut self.log,
                &format!("{label} read"),
                &map_dur(frames, |f| f.read),
            );
            report_durations(
                &mut self.log,
                &format!("{label} cpu"),
                &map_dur(frames, |f| f.cpu),
            );
            report_durations(
                &mut self.log,
                &format!("{label} finish"),
                &map_dur(frames, |f| f.finish),
            );
            report_durations(
                &mut self.log,
                &format!("{label} wall"),
                &map_dur(frames, |f| f.wall),
            );
            report_durations(
                &mut self.log,
                &format!("{label} other"),
                &map_dur(frames, other),
            );
            report_durations(
                &mut self.log,
                &format!("{label} swap"),
                &map_dur(frames, |f| f.swap),
            );
        }
    }

    fn map_dur(frames: &[FrameTime], pick: impl Fn(&FrameTime) -> Duration) -> Vec<Duration> {
        frames.iter().map(pick).collect()
    }

    fn other(frame: &FrameTime) -> Duration {
        frame
            .wall
            .saturating_sub(frame.paint)
            .saturating_sub(frame.read)
            .saturating_sub(frame.cpu)
            .saturating_sub(frame.finish)
    }

    fn probe_note() -> String {
        let probe = super::probe_sample();
        format!(
            "one frame bgra_read={} read={:.2} cpu={:.2} finish={:.2} ms",
            probe.bgra,
            ms(probe.read),
            ms(probe.cpu),
            ms(probe.finish)
        )
    }

    fn unique(hashes: &[u64]) -> usize {
        let mut copy = hashes.to_vec();
        copy.sort_unstable();
        copy.dedup();
        copy.len()
    }

    fn hash_pixels(pixels: &[u8]) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        let mut index = 0;
        while index + 8 <= pixels.len() {
            let word = u64::from_le_bytes(pixels[index..index + 8].try_into().expect("word"));
            hash ^= word;
            hash = hash.wrapping_mul(0x100_0000_01b3);
            index += 64;
        }
        hash
    }

    fn diff_line(reference: &[u8], frame: &[u8]) -> String {
        let len = reference.len().min(frame.len());
        let mut count = reference.len().abs_diff(frame.len());
        let mut first = None;
        for index in 0..len {
            if reference[index] != frame[index] {
                if first.is_none() {
                    first = Some(index);
                }
                count += 1;
            }
        }
        match first {
            Some(index) => {
                let end = (index + 4).min(len);
                format!(
                    "{count} bytes differ, first at {index}: {:02x?} vs {:02x?}",
                    &reference[index..end],
                    &frame[index..end]
                )
            }
            None if count == 0 => "identical".into(),
            None => format!("{count} bytes differ by length"),
        }
    }

    fn colours(pixels: &[u8]) -> Result<(), String> {
        let stride = 4 * WIDTH as usize;
        let need = stride * HEIGHT as usize;
        if pixels.len() < need {
            return Err(format!("frame is {} bytes, needs {need}", pixels.len()));
        }
        let checks = [
            ((4, 0), ORANGE, "top row"),
            ((4, 4), ORANGE, "header"),
            ((WIDTH / 2, 8), ORANGE, "header middle"),
            ((WIDTH / 2, 70), BLUE, "body"),
            ((0, HEIGHT - 1), BLUE, "bottom left"),
            ((100, 100), GREEN, "box (a WebGL canvas)"),
            ((WIDTH - 4, HEIGHT - 4), MAGENTA, "corner"),
        ];
        for ((x, y), expected, name) in checks {
            let got = pixel_at(pixels, stride, x, y);
            if got != expected {
                return Err(format!(
                    "{name} at ({x},{y}): got {got:02x?}, expected {expected:02x?}"
                ));
            }
        }
        Ok(())
    }

    fn pixel_at(pixels: &[u8], stride: usize, x: u32, y: u32) -> [u8; 4] {
        let index = y as usize * stride + x as usize * 4;
        pixels[index..index + 4].try_into().expect("pixel")
    }

    fn page_metrics() -> Metrics {
        Metrics::new(
            LayoutEpoch::next(),
            WindowRect {
                x: 0,
                y: 0,
                width: WIDTH,
                height: HEIGHT,
            },
            Scale::ONE,
        )
    }

    fn write_page(name: &str, body: &str, script: &str) -> Result<PathBuf, String> {
        let dir = std::env::temp_dir().join("sighurt-gfx-pages");
        std::fs::create_dir_all(&dir).map_err(|err| err.to_string())?;
        let path = dir.join(name);
        std::fs::write(&path, page_html(body, script)).map_err(|err| err.to_string())?;
        Ok(path)
    }

    fn page_html(body: &str, script: &str) -> String {
        format!(
            r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<style>
html, body {{ margin: 0; padding: 0; width: 100%; height: 100%; overflow: hidden; background: #1b4f72; }}
.top {{ position: absolute; left: 0; top: 0; width: 100%; height: 48px; background: #e67e22; }}
.box {{ position: absolute; left: 80px; top: 80px; width: 48px; height: 48px; }}
.mark {{ position: fixed; right: 0; bottom: 0; width: 24px; height: 24px; background: #ff00ff; }}
</style>
</head>
<body>
<div class="top"></div>
<canvas class="box" width="48" height="48"></canvas>
{body}
<script>
var gl = document.querySelector(".box").getContext("webgl", {{ preserveDrawingBuffer: true }});
gl.clearColor(0, 1, 0, 1);
gl.clear(gl.COLOR_BUFFER_BIT);
{script}
</script>
<div class="mark"></div>
</body>
</html>
"#
        )
    }

    const REDRAW_BODY: &str = "";
    const REDRAW_SCRIPT: &str = r#"
var speck = document.createElement("div");
speck.style.cssText = "position:absolute;left:400px;top:16px;width:10px;height:10px;background:#010101";
document.body.appendChild(speck);
var n = 0;
function tick() {
  n = (n + 1) & 255;
  speck.style.background = "rgb(" + n + "," + (255 - n) + ",0)";
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);
"#;

    const CSS_BODY: &str = r#"<div id="mover" style="position:absolute;left:0;top:200px;width:800px;height:400px;background:#c0392b"></div>"#;
    const CSS_SCRIPT: &str = r#"
var x = 0;
function tick() {
  x = (x + 17) % 900;
  document.getElementById("mover").style.transform = "translate3d(" + x + "px,30px,0)";
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);
"#;

    const CANVAS_BODY: &str =
        r#"<canvas id="c" style="position:absolute;left:0;top:0;width:100%;height:100%"></canvas>"#;
    const CANVAS_SCRIPT: &str = r##"
var c = document.getElementById("c");
var ctx = c.getContext("2d");
var n = 0;
function tick() {
  c.width = window.innerWidth;
  c.height = window.innerHeight;
  n += 1;
  ctx.fillStyle = (n & 1) === 0 ? "#112233" : "#332211";
  ctx.fillRect(0, 0, c.width, c.height);
  ctx.fillStyle = "#39ff14";
  ctx.fillRect(n % 400, 120, 80, 80);
  requestAnimationFrame(tick);
}
requestAnimationFrame(tick);
"##;
}
