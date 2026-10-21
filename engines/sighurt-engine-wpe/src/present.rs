//! Presenting WebKit's frames on the host's page targets: DMA-BUF buffers attached as they are,
//! or shared-memory frames copied once into our own `wl_shm` buffers (the diagnosed fallback).
//!
//! The host leases each view a native target ([`PresentMode::Gl`]: the presenter, not the
//! host, attaches and commits the leaf). This module talks to the compositor on the host's own
//! `wl_display`, through an event queue of its own, so its buffers' events never reach the
//! host: it wraps the target's `wl_surface`, binds `zwp_linux_dmabuf_v1` (version 4, for the
//! default feedback: the compositor's main DRM device and the formats it imports) and `wl_shm`.
//! Its queue is dispatched without blocking whenever the engine is pumped or presents (the host
//! reads the socket), which is where buffer releases arrive.
//!
//! A DMA-BUF frame becomes a `wl_buffer` through `zwp_linux_buffer_params_v1.create_immed`,
//! once per WebKit buffer (cached in the buffer's user data, destroyed with it): no copy. An SHM
//! frame (`WPEBufferSHM`, ARGB8888 like `wl_shm`'s) is copied into one of at most
//! [`SHM_BUFFERS`] buffers of ours per view, which are reused only after the compositor
//! releases them. On a CPU target ([`crate::Offscreen`]) there is no compositor: `copy_argb`
//! copies the SHM frame into the frame the target lends, row by row, with no conversion.
//!
//! [`PresentMode::Gl`]: sighurt_wayland::PresentMode::Gl
//!
//! Must not own: when to present (the host's permits), WebKit's buffers (WebKit allocates and
//! frees them), or the surface's geometry (the host's viewport and scale are pending state that
//! our commit applies).

use std::ffi::{c_void, CString};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;

use sighurt_api::present::Frame;
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_buffer_params_v1::{
    self, ZwpLinuxBufferParamsV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_feedback_v1::{
    self, ZwpLinuxDmabufFeedbackV1,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::zwp_linux_dmabuf_v1::{
    self, ZwpLinuxDmabufV1,
};

use crate::ffi;
use crate::platform::{Buffer, Dmabuf};

/// Our SHM buffers per view: one shown, one being replaced, one spare.
pub const SHM_BUFFERS: usize = 3;

/// Identifies one of our `wl_buffer`s in its release event.
pub type BufferKey = u64;

/// The presenter's side of the connection, shared by all views.
pub struct Wayland {
    conn: Connection,
    queue: EventQueue<State>,
    qh: QueueHandle<State>,
    state: State,
    shm: WlShm,
    dmabuf: Option<ZwpLinuxDmabufV1>,
    next_key: BufferKey,
    imports: u64,
}

/// What our queue's events tell us.
#[derive(Default)]
struct State {
    released: Vec<BufferKey>,
    feedback: Feedback,
    /// A DMA-BUF import the compositor refused.
    failed_imports: u64,
}

/// The default DMA-BUF feedback as it arrives.
#[derive(Default)]
struct Feedback {
    table: Vec<(u32, u64)>,
    main_device: Option<u64>,
    tranche_device: Option<u64>,
    tranche: Vec<u16>,
    formats: Vec<(u32, u64)>,
    done: bool,
}

impl Wayland {
    /// Joins the host's connection.
    ///
    /// # Safety
    ///
    /// `display` must be the host's live `wl_display`, and stay alive while this exists (a
    /// target lease guarantees both).
    pub unsafe fn new(display: NonNull<c_void>) -> Result<Self, String> {
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let conn = Connection::from_backend(backend);
        let (globals, mut queue) =
            registry_queue_init::<State>(&conn).map_err(|e| format!("registry: {e}"))?;
        let qh = queue.handle();
        let shm = globals
            .bind::<WlShm, _, _>(&qh, 1..=1, ())
            .map_err(|e| format!("wl_shm: {e}"))?;
        // Version 4 has the default feedback (main device, format table); without it, SHM only.
        let dmabuf = globals.bind::<ZwpLinuxDmabufV1, _, _>(&qh, 4..=4, ()).ok();
        let mut state = State::default();
        if let Some(dmabuf) = &dmabuf {
            let feedback = dmabuf.get_default_feedback(&qh, ());
            while !state.feedback.done {
                queue
                    .blocking_dispatch(&mut state)
                    .map_err(|e| format!("feedback: {e}"))?;
            }
            feedback.destroy();
        }
        Ok(Self {
            conn,
            queue,
            qh,
            state,
            shm,
            dmabuf,
            next_key: 1,
            imports: 0,
        })
    }

    /// The compositor's DMA-BUF support: its main device's nodes and the formats it imports
    /// there. `None` without `zwp_linux_dmabuf_v1` version 4, or when the device has no nodes
    /// under `/dev/dri` (then WebKit renders to shared memory).
    pub fn dmabuf(&self) -> Option<Dmabuf> {
        self.dmabuf.as_ref()?;
        let feedback = &self.state.feedback;
        let (primary, render) = drm_nodes(feedback.main_device?)?;
        Some(Dmabuf {
            primary,
            render,
            formats: feedback.formats.clone(),
        })
    }

    /// Our proxy for a target's `wl_surface`.
    ///
    /// # Safety
    ///
    /// `surface` must be the target's live `wl_surface`, kept alive by its lease for as long as
    /// the returned proxy is used.
    pub unsafe fn surface(&self, surface: NonNull<c_void>) -> Result<WlSurface, String> {
        let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), surface.as_ptr().cast()) }
            .map_err(|_| "not a wl_surface".to_string())?;
        WlSurface::from_id(&self.conn, id).map_err(|_| "a dead wl_surface".to_string())
    }

    /// Dispatches what the host has read for our queue; returns the buffers the compositor
    /// released since. An error means the connection is broken (a protocol error).
    pub fn dispatch(&mut self) -> Result<Vec<BufferKey>, String> {
        self.queue
            .dispatch_pending(&mut self.state)
            .map_err(|e| e.to_string())?;
        Ok(std::mem::take(&mut self.state.released))
    }

    /// The kernel driver of the compositor's main DRM device, from sysfs.
    pub fn driver(&self) -> Option<String> {
        let device = self.state.feedback.main_device?;
        let (major, minor) = (libc::major(device), libc::minor(device));
        let link =
            std::fs::read_link(format!("/sys/dev/char/{major}:{minor}/device/driver")).ok()?;
        Some(link.file_name()?.to_string_lossy().into_owned())
    }

    /// DMA-BUF imports so far, and how many of them the compositor refused.
    pub fn imports(&self) -> (u64, u64) {
        (self.imports, self.state.failed_imports)
    }

    fn key(&mut self) -> BufferKey {
        self.next_key += 1;
        self.next_key
    }

    /// The `wl_buffer` of a DMA-BUF frame, made once per WebKit buffer and kept in its user
    /// data until WebKit destroys the buffer.
    pub fn dmabuf_buffer(&mut self, buffer: &Buffer) -> Result<(WlBuffer, BufferKey), String> {
        let raw = buffer.as_ptr();
        let cached = unsafe { ffi::wpe_buffer_get_user_data(raw) }.cast::<Imported>();
        if let Some(imported) = unsafe { cached.as_ref() } {
            return Ok((imported.buffer.clone(), imported.key));
        }
        let dmabuf = self.dmabuf.as_ref().ok_or("no zwp_linux_dmabuf_v1")?;
        let (width, height) = buffer.size();
        let params = dmabuf.create_params(&self.qh, ());
        unsafe {
            let modifier = ffi::wpe_buffer_dma_buf_get_modifier(raw);
            for plane in 0..ffi::wpe_buffer_dma_buf_get_n_planes(raw) {
                // WebKit keeps the descriptor; libwayland duplicates it to send it.
                let fd = BorrowedFd::borrow_raw(ffi::wpe_buffer_dma_buf_get_fd(raw, plane));
                params.add(
                    fd,
                    plane,
                    ffi::wpe_buffer_dma_buf_get_offset(raw, plane),
                    ffi::wpe_buffer_dma_buf_get_stride(raw, plane),
                    (modifier >> 32) as u32,
                    modifier as u32,
                );
            }
        }
        let key = self.key();
        let format = unsafe { ffi::wpe_buffer_dma_buf_get_format(raw) };
        let wl_buffer = params.create_immed(
            width as i32,
            height as i32,
            format,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.qh,
            key,
        );
        params.destroy();
        self.imports += 1;
        let imported = Box::new(Imported {
            buffer: wl_buffer.clone(),
            key,
        });
        unsafe {
            ffi::wpe_buffer_set_user_data(raw, Box::into_raw(imported).cast(), Some(drop_imported));
        }
        Ok((wl_buffer, key))
    }

    /// Our SHM buffers for one view.
    pub fn shm_buffers(&self) -> ShmBuffers {
        ShmBuffers {
            buffers: Vec::new(),
        }
    }
}

/// A WebKit buffer's `wl_buffer`, in the buffer's user data.
struct Imported {
    buffer: WlBuffer,
    key: BufferKey,
}

unsafe extern "C" fn drop_imported(data: glib::ffi::gpointer) {
    let imported = unsafe { Box::from_raw(data.cast::<Imported>()) };
    imported.buffer.destroy();
}

/// One view's SHM buffers, at most [`SHM_BUFFERS`], for the fallback path.
pub struct ShmBuffers {
    buffers: Vec<ShmBuffer>,
}

struct ShmBuffer {
    key: BufferKey,
    size: (u32, u32),
    map: NonNull<u8>,
    len: usize,
    wl_buffer: WlBuffer,
    busy: bool,
}

impl ShmBuffers {
    /// Copies an SHM frame into a free buffer of ours and returns it, or `None` while every
    /// buffer is still with the compositor. The one copy of the fallback path.
    pub fn copy(
        &mut self,
        wayland: &mut Wayland,
        frame: &Buffer,
    ) -> Result<Option<(WlBuffer, BufferKey)>, String> {
        let size = frame.size();
        self.buffers.retain(|b| {
            let keep = b.busy || b.size == size;
            if !keep {
                b.wl_buffer.destroy();
                unsafe { libc::munmap(b.map.as_ptr().cast(), b.len) };
            }
            keep
        });
        let index = match self.buffers.iter().position(|b| !b.busy && b.size == size) {
            Some(index) => index,
            None if self.buffers.len() < SHM_BUFFERS => {
                let key = wayland.key();
                self.buffers.push(ShmBuffer::new(wayland, size, key)?);
                self.buffers.len() - 1
            }
            None => return Ok(None),
        };
        let target = &mut self.buffers[index];
        let raw = frame.as_ptr();
        unsafe {
            if ffi::wpe_buffer_shm_get_format(raw) != ffi::WPE_PIXEL_FORMAT_ARGB8888 {
                return Err("an SHM frame that is not ARGB8888".into());
            }
            let stride = ffi::wpe_buffer_shm_get_stride(raw) as usize;
            let mut len = 0;
            let data = glib::ffi::g_bytes_get_data(ffi::wpe_buffer_shm_get_data(raw), &mut len);
            let row = size.0 as usize * 4;
            let source = std::slice::from_raw_parts(data.cast::<u8>(), len);
            let dest = std::slice::from_raw_parts_mut(target.map.as_ptr(), target.len);
            for (y, out) in dest.chunks_exact_mut(row).enumerate().take(size.1 as usize) {
                out.copy_from_slice(&source[y * stride..y * stride + row]);
            }
        }
        target.busy = true;
        Ok(Some((target.wl_buffer.clone(), target.key)))
    }

    /// Marks our buffer `key` free again; false if it isn't one of ours.
    pub fn release(&mut self, key: BufferKey) -> bool {
        match self.buffers.iter_mut().find(|b| b.key == key) {
            Some(buffer) => {
                buffer.busy = false;
                true
            }
            None => false,
        }
    }
}

impl ShmBuffer {
    fn new(wayland: &Wayland, size: (u32, u32), key: BufferKey) -> Result<Self, String> {
        let len = size.0 as usize * size.1 as usize * 4;
        let fd = unsafe { libc::memfd_create(c"sighurt-wpe".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(format!("memfd: {}", std::io::Error::last_os_error()));
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as i64) } < 0 {
            return Err(format!("ftruncate: {}", std::io::Error::last_os_error()));
        }
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(format!("mmap: {}", std::io::Error::last_os_error()));
        }
        let pool: WlShmPool = wayland
            .shm
            .create_pool(fd.as_fd(), len as i32, &wayland.qh, ());
        let wl_buffer = pool.create_buffer(
            0,
            size.0 as i32,
            size.1 as i32,
            size.0 as i32 * 4,
            wl_shm::Format::Argb8888,
            &wayland.qh,
            key,
        );
        pool.destroy();
        Ok(Self {
            key,
            size,
            map: NonNull::new(map.cast()).expect("a mapping"),
            len,
            wl_buffer,
            busy: false,
        })
    }
}

impl Drop for ShmBuffers {
    fn drop(&mut self) {
        for buffer in self.buffers.drain(..) {
            buffer.wl_buffer.destroy();
            unsafe { libc::munmap(buffer.map.as_ptr().cast(), buffer.len) };
        }
    }
}

/// The primary and render nodes of a DRM device number, from sysfs.
fn drm_nodes(device: u64) -> Option<(CString, Option<CString>)> {
    let (major, minor) = (libc::major(device), libc::minor(device));
    let dir = std::fs::read_dir(format!("/sys/dev/char/{major}:{minor}/device/drm")).ok()?;
    let (mut primary, mut render) = (None, None);
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let node = CString::new(format!("/dev/dri/{name}")).ok();
        if name.starts_with("card") {
            primary = node;
        } else if name.starts_with("renderD") {
            render = node;
        }
    }
    Some((primary?, render))
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShm, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShm,
        _: wl_shm::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlShmPool, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlShmPool,
        _: <WlShmPool as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlBuffer, BufferKey> for State {
    fn event(
        state: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        key: &BufferKey,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            state.released.push(*key);
        }
    }
}

impl Dispatch<ZwpLinuxDmabufV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpLinuxDmabufV1,
        _: zwp_linux_dmabuf_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLinuxBufferParamsV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_linux_buffer_params_v1::Event::Failed = event {
            state.failed_imports += 1;
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use zwp_linux_dmabuf_feedback_v1::Event;
        let feedback = &mut state.feedback;
        match event {
            Event::FormatTable { fd, size } => feedback.table = read_format_table(&fd, size),
            Event::MainDevice { device } => feedback.main_device = device_number(&device),
            Event::TrancheTargetDevice { device } => {
                feedback.tranche_device = device_number(&device)
            }
            Event::TrancheFormats { indices } => feedback.tranche.extend(
                indices
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&i| u16::from_ne_bytes(i)),
            ),
            Event::TrancheDone => {
                // Formats for the device WebKit will render on; scanout tranches for other
                // devices are of no use to it.
                if feedback.tranche_device == feedback.main_device {
                    for &index in &feedback.tranche {
                        if let Some(&format) = feedback.table.get(index as usize) {
                            if !feedback.formats.contains(&format) {
                                feedback.formats.push(format);
                            }
                        }
                    }
                }
                feedback.tranche.clear();
            }
            Event::Done => feedback.done = true,
            _ => {}
        }
    }
}

fn device_number(bytes: &[u8]) -> Option<u64> {
    Some(u64::from_ne_bytes(bytes.get(..8)?.try_into().ok()?))
}

/// The feedback's format table: 16-byte entries of a fourcc, padding and a modifier.
fn read_format_table(fd: &OwnedFd, size: u32) -> Vec<(u32, u64)> {
    let len = size as usize;
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            fd.as_raw_fd(),
            0,
        )
    };
    if map == libc::MAP_FAILED {
        return Vec::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), len) };
    let table = bytes
        .as_chunks::<16>()
        .0
        .iter()
        .map(|e| {
            let format = u32::from_ne_bytes([e[0], e[1], e[2], e[3]]);
            let modifier = u64::from_ne_bytes(e[8..16].try_into().expect("8 bytes"));
            (format, modifier)
        })
        .collect();
    unsafe { libc::munmap(map, len) };
    table
}

/// Counts of what presentation did, for diagnostics.
#[derive(Clone, Copy, Debug, Default)]
pub struct PresentStats {
    /// Frames attached as DMA-BUF `wl_buffer`s: no CPU copy.
    pub dmabuf_frames: u64,
    /// `wl_buffer`s created from WebKit's DMA-BUFs (once per WebKit buffer).
    pub dmabuf_imports: u64,
    /// Frames copied into our SHM buffers: one CPU copy each.
    pub shm_copies: u64,
    /// Bytes those copies moved.
    pub shm_bytes: u64,
    /// Frames replaced before they could be shown (WebKit rendered faster than the host
    /// presented, or a resize made them stale).
    pub dropped: u64,
    /// Releases of our buffers by the compositor.
    pub releases: u64,
}

/// Copies a `WPEBufferSHM` into `frame`.
///
/// WebKit's only shared-memory format is `WPE_PIXEL_FORMAT_ARGB8888`: premultiplied alpha,
/// little-endian (B, G, R, A), the first row at the top. That is the CPU frame's format, so
/// this is a row copy and not a conversion. A different format is an error.
pub(crate) fn copy_argb(buffer: &Buffer, frame: Frame<'_>) -> Result<(), String> {
    if buffer.is_dma_buf() {
        return Err("a DMA-BUF frame has no shared-memory pixels".into());
    }
    let (width, height) = buffer.size();
    if frame.size.width != width || frame.size.height != height {
        return Err(format!(
            "frame is {}×{}, the SHM buffer is {width}×{height}",
            frame.size.width, frame.size.height
        ));
    }
    let raw = buffer.as_ptr();
    // SAFETY: `buffer` is a live `WPEBufferSHM` this process holds until it is released. The
    // bytes are only read, into `frame`, which the caller lends for this call.
    unsafe {
        let format = ffi::wpe_buffer_shm_get_format(raw);
        if format != ffi::WPE_PIXEL_FORMAT_ARGB8888 {
            return Err(format!("SHM format {format} is not ARGB8888"));
        }
        let stride = ffi::wpe_buffer_shm_get_stride(raw) as usize;
        let bytes = ffi::wpe_buffer_shm_get_data(raw);
        if bytes.is_null() {
            return Err("SHM frame has no bytes".into());
        }
        let mut len = 0usize;
        let data = glib::ffi::g_bytes_get_data(bytes, &mut len);
        if data.is_null() {
            return Err("SHM frame data is null".into());
        }
        let row = width as usize * 4;
        if stride < row || frame.stride < row || !frame.stride.is_multiple_of(4) {
            return Err(format!(
                "SHM stride {stride} or frame stride {} does not cover {row} bytes",
                frame.stride
            ));
        }
        let need = stride
            .checked_mul(height as usize)
            .ok_or_else(|| format!("SHM frame {width}×{height} is too large"))?;
        if len < need || frame.pixels.len() < frame.stride * height as usize {
            return Err(format!(
                "SHM frame is {len} bytes, {width}×{height} stride {stride} needs {need}"
            ));
        }
        let source = std::slice::from_raw_parts(data.cast::<u8>(), len);
        for y in 0..height as usize {
            let from = y * stride;
            let to = y * frame.stride;
            frame.pixels[to..to + row].copy_from_slice(&source[from..from + row]);
        }
    }
    Ok(())
}

/// Damage for `wl_surface.damage_buffer`, or the whole buffer.
pub fn damage(surface: &WlSurface, rects: &[ffi::WPERectangle], size: (u32, u32)) {
    if rects.is_empty() {
        surface.damage_buffer(0, 0, size.0 as i32, size.1 as i32);
    }
    for r in rects {
        surface.damage_buffer(r.x, r.y, r.width, r.height);
    }
}
