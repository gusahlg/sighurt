//! Shared-memory buffers for CPU presenters: the chrome, the demo engine and the root fallback.
//!
//! Two buffers per surface, a third only when the compositor holds both. Each buffer is free,
//! being drawn, or submitted; submitted storage is reused only after the compositor releases it
//! (`wl_buffer.release`), and with no free buffer the invalidation waits instead of overwriting
//! memory in use. A resize starts a new pool generation: the old generation's free buffers are
//! destroyed at once and its submitted ones retire, kept alive until released. At most
//! two generations retire at a time; beyond that a new size waits for a release,
//! which always comes because every retiring generation but the newest has been superseded by a
//! later commit. So a resize storm holds at most nine buffers: three sizes of three.
//!
//! A hidden (unmapped) surface frees its buffers once the compositor has released them
//! ([`Swapchain::trim`]); showing it again allocates afresh.
//!
//! Every submission damages the whole buffer, and the painter repaints all of it: a reused
//! buffer holds the frame before last, and there is no damage history yet.
//!
//! # Pixel format
//!
//! Every buffer is `wl_shm` ARGB8888 with premultiplied alpha: each pixel is a little-endian
//! `u32` `0xAARRGGBB`, so its bytes in memory are **B, G, R, A**. Rows are `width × 4` bytes
//! with no padding. tiny-skia's pixmaps are premultiplied **R, G, B, A** bytes, so a painter
//! using one swaps red and blue once, with [`rgba_to_argb8888`], before submitting.
//!
//! Must not own: page pixels. A page-sized CPU copy per frame is exactly what the rewrite
//! removes.

use std::io;

use sighurt_api::engine::PresentOutcome;
use sighurt_api::geometry::{BufferSize, Metrics};
use sighurt_api::ids::{TargetId, WindowId};
use smithay_client_toolkit::error::GlobalError;
use smithay_client_toolkit::globals::ProvidesBoundGlobal;
use smithay_client_toolkit::shm::raw::RawPool;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::{Connection, Dispatch, QueueHandle};

use crate::connection::Host;
use crate::targets::FramePermit;

/// Bytes per pixel of every buffer the host lends.
pub const BYTES_PER_PIXEL: usize = 4;

/// Buffers a surface may have in its current generation.
pub(crate) const MAX_BUFFERS: usize = 3;

/// Superseded generations that may still hold submitted buffers.
pub(crate) const MAX_RETIRING: usize = 2;

/// Converts premultiplied RGBA bytes, tiny-skia's layout, into the buffers' ARGB8888 (B, G, R,
/// A bytes), in place. Alpha and premultiplication are unchanged.
pub fn rgba_to_argb8888(pixels: &mut [u8]) {
    for pixel in pixels.as_chunks_mut::<BYTES_PER_PIXEL>().0 {
        pixel.swap(0, 2);
    }
}

/// Identifies one buffer of one surface: its generation and its slot in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct BufferKey {
    pub generation: u64,
    pub index: usize,
}

/// Where a buffer is in its life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BufferState {
    /// Ours to draw into.
    Free,
    /// Lent to a painter, between acquiring and submitting it.
    Drawing,
    /// Attached and committed: the compositor may read it until it releases it.
    Submitted,
}

/// Why no buffer can be drawn into right now. Either way the invalidation waits for a release.
#[derive(Debug)]
pub(crate) enum AcquireError {
    /// Every buffer of the current generation is in use.
    InUse,
    /// A new size needs a new generation, and too many old ones are still held.
    Retiring,
    /// Allocating shared memory failed.
    Alloc(io::Error),
}

struct Slot<B> {
    state: BufferState,
    bytes: usize,
    /// The storage; `None` while it is lent to a painter (`Drawing`).
    buffer: Option<B>,
}

struct Generation<B> {
    id: u64,
    size: BufferSize,
    // Indexed by `BufferKey::index`; a retired slot becomes `None` so the indices of the others,
    // baked into their `wl_buffer`s, stay valid.
    slots: Vec<Option<Slot<B>>>,
}

impl<B> Generation<B> {
    fn held(&self) -> impl Iterator<Item = &Slot<B>> {
        self.slots.iter().flatten()
    }
}

/// The buffers of one surface, by state: the bookkeeping, generic over the storage so the rules
/// are tested without a compositor.
pub(crate) struct Swapchain<B> {
    current: Generation<B>,
    retiring: Vec<Generation<B>>,
    next_generation: u64,
}

impl<B> Swapchain<B> {
    pub fn new() -> Self {
        Self {
            current: Generation {
                id: 0,
                size: BufferSize::default(),
                slots: Vec::new(),
            },
            retiring: Vec::new(),
            next_generation: 1,
        }
    }

    /// A free buffer of `size` to draw into, lent out with its storage: its slot is `Drawing`
    /// until [`submit`](Self::submit) or [`abandon`](Self::abandon) gives the storage back.
    /// `alloc` makes a new one of `bytes` bytes when the current generation has room for one
    /// more.
    pub fn acquire(
        &mut self,
        size: BufferSize,
        bytes: usize,
        alloc: impl FnOnce(BufferKey) -> io::Result<B>,
    ) -> Result<(BufferKey, B), AcquireError> {
        if size != self.current.size {
            self.start_generation(size)?;
        }
        let generation = self.current.id;
        let slots = &mut self.current.slots;
        let free = slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| matches!(slot, Some(s) if s.state == BufferState::Free));
        if let Some((index, Some(slot))) = free {
            slot.state = BufferState::Drawing;
            let buffer = slot.buffer.take().expect("a free buffer has its storage");
            return Ok((BufferKey { generation, index }, buffer));
        }
        if slots.len() >= MAX_BUFFERS {
            return Err(AcquireError::InUse);
        }
        let key = BufferKey {
            generation,
            index: slots.len(),
        };
        let buffer = alloc(key).map_err(AcquireError::Alloc)?;
        slots.push(Some(Slot {
            state: BufferState::Drawing,
            bytes,
            buffer: None,
        }));
        Ok((key, buffer))
    }

    /// Whether [`acquire`](Self::acquire) would lend a buffer of `size` now.
    pub fn can_acquire(&self, size: BufferSize) -> bool {
        if size != self.current.size {
            let holds_any = self.current.held().any(|s| s.state != BufferState::Free);
            return !holds_any || self.retiring.len() < MAX_RETIRING;
        }
        self.current.slots.len() < MAX_BUFFERS
            || self.current.held().any(|s| s.state == BufferState::Free)
    }

    fn start_generation(&mut self, size: BufferSize) -> Result<(), AcquireError> {
        let holds_any = self.current.held().any(|s| s.state != BufferState::Free);
        if holds_any && self.retiring.len() >= MAX_RETIRING {
            return Err(AcquireError::Retiring);
        }
        let id = self.next_generation;
        self.next_generation += 1;
        let mut old = std::mem::replace(
            &mut self.current,
            Generation {
                id,
                size,
                slots: Vec::new(),
            },
        );
        // Free buffers of the old size are dropped (destroyed) now; held ones retire.
        for slot in &mut old.slots {
            if matches!(slot, Some(s) if s.state == BufferState::Free) {
                *slot = None;
            }
        }
        if old.held().next().is_some() {
            self.retiring.push(old);
        }
        Ok(())
    }

    /// Frees the storage of a surface that is hidden: when every buffer of the current
    /// generation is free (none drawn into, none with the compositor), they are destroyed and
    /// the next acquire starts a new generation. Returns whether it did.
    pub fn trim(&mut self) -> bool {
        let idle = self
            .current
            .slots
            .iter()
            .flatten()
            .all(|s| s.state == BufferState::Free);
        if !idle || self.current.held().next().is_none() {
            return false;
        }
        let id = self.next_generation;
        self.next_generation += 1;
        self.current = Generation {
            id,
            size: BufferSize::default(),
            slots: Vec::new(),
        };
        true
    }

    /// The painter finished `key` and gives its storage back: it is attached and committed now.
    pub fn submit(&mut self, key: BufferKey, buffer: B) {
        if let Some(slot) = self.slot_mut(key) {
            debug_assert_eq!(slot.state, BufferState::Drawing);
            slot.state = BufferState::Submitted;
            slot.buffer = Some(buffer);
        }
    }

    /// The painter gives `key` back without submitting it: free again, or destroyed if its
    /// generation retired meanwhile.
    pub fn abandon(&mut self, key: BufferKey, buffer: B) {
        if let Some(slot) = self.slot_mut(key) {
            slot.buffer = Some(buffer);
        }
        self.make_free(key);
    }

    /// The compositor released `key`: it may be drawn into again, or, if its generation
    /// retired, it is destroyed.
    pub fn release(&mut self, key: BufferKey) {
        self.make_free(key);
    }

    fn make_free(&mut self, key: BufferKey) {
        if key.generation == self.current.id {
            if let Some(slot) = self.slot_mut(key) {
                slot.state = BufferState::Free;
            }
            return;
        }
        if let Some(position) = self.retiring.iter().position(|g| g.id == key.generation) {
            let generation = &mut self.retiring[position];
            if let Some(slot) = generation.slots.get_mut(key.index) {
                *slot = None;
            }
            if generation.held().next().is_none() {
                self.retiring.remove(position);
            }
        }
    }

    fn slot_mut(&mut self, key: BufferKey) -> Option<&mut Slot<B>> {
        let generation = if key.generation == self.current.id {
            &mut self.current
        } else {
            self.retiring.iter_mut().find(|g| g.id == key.generation)?
        };
        generation.slots.get_mut(key.index)?.as_mut()
    }

    /// The state of `key`, if it still exists.
    #[cfg(test)]
    pub fn state(&self, key: BufferKey) -> Option<BufferState> {
        std::iter::once(&self.current)
            .chain(&self.retiring)
            .find(|g| g.id == key.generation)
            .and_then(|g| g.slots.get(key.index)?.as_ref())
            .map(|slot| slot.state)
    }

    /// Counts for diagnostics.
    pub fn stats(&self) -> BufferStats {
        let mut stats = BufferStats {
            retiring_generations: self.retiring.len(),
            ..BufferStats::default()
        };
        for slot in self.current.held() {
            stats.buffers += 1;
            stats.bytes += slot.bytes;
        }
        for slot in self.retiring.iter().flat_map(Generation::held) {
            stats.buffers += 1;
            stats.bytes += slot.bytes;
            stats.retiring_bytes += slot.bytes;
        }
        stats
    }
}

/// How many SHM buffers a surface holds, for diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferStats {
    /// Buffers of every generation.
    pub buffers: usize,
    /// Their bytes.
    pub bytes: usize,
    /// Superseded generations still holding submitted buffers.
    pub retiring_generations: usize,
    /// The bytes those hold.
    pub retiring_bytes: usize,
}

/// One `wl_buffer` and its own pool: a buffer is freed on its own, without waiting for the
/// rest of its generation.
pub(crate) struct ShmBuffer {
    pool: RawPool,
    buffer: WlBuffer,
    len: usize,
}

impl ShmBuffer {
    /// A zeroed ARGB8888 buffer of `size`.
    pub fn new(
        shm: &WlShm,
        qh: &QueueHandle<Host>,
        size: BufferSize,
        data: BufferData,
    ) -> io::Result<Self> {
        let (width, height, stride, len) = layout(size)?;
        let mut pool = RawPool::new(len, &BoundShm(shm)).map_err(io::Error::other)?;
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, data, qh);
        Ok(Self { pool, buffer, len })
    }

    pub fn wl_buffer(&self) -> &WlBuffer {
        &self.buffer
    }

    pub fn pixels(&mut self) -> &mut [u8] {
        &mut self.pool.mmap()[..self.len]
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        // The pool destroys its wl_shm_pool when dropped, after this.
        self.buffer.destroy();
    }
}

/// `wl_shm` as smithay-client-toolkit's pool takes it.
struct BoundShm<'a>(&'a WlShm);

impl ProvidesBoundGlobal<WlShm, 1> for BoundShm<'_> {
    fn bound_global(&self) -> Result<WlShm, GlobalError> {
        Ok(self.0.clone())
    }
}

/// `(width, height, stride, bytes)` of an ARGB8888 buffer of `size`, as `wl_shm` takes them.
pub(crate) fn layout(size: BufferSize) -> io::Result<(i32, i32, i32, usize)> {
    let too_large = || {
        io::Error::other(format!(
            "a {}×{} buffer is too large",
            size.width, size.height
        ))
    };
    let width = i32::try_from(size.width).map_err(|_| too_large())?;
    let height = i32::try_from(size.height).map_err(|_| too_large())?;
    let stride = width
        .checked_mul(BYTES_PER_PIXEL as i32)
        .ok_or_else(too_large)?;
    let len = stride.checked_mul(height).ok_or_else(too_large)?;
    if len <= 0 {
        return Err(io::Error::other("an empty buffer"));
    }
    Ok((width, height, stride, len as usize))
}

/// What a `wl_buffer` belongs to, so its release reaches the right swapchain.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BufferData {
    pub owner: BufferOwner,
    pub key: BufferKey,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum BufferOwner {
    Target(TargetId),
    Root(WindowId),
}

impl Dispatch<WlBuffer, BufferData> for Host {
    fn event(
        host: &mut Host,
        _: &WlBuffer,
        event: wl_buffer::Event,
        data: &BufferData,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        let wl_buffer::Event::Release = event else {
            return;
        };
        match data.owner {
            BufferOwner::Target(id) => {
                if let Some(target) = host.targets.get(&id) {
                    let mut target = target.borrow_mut();
                    target.stats.releases += 1;
                    let hidden = target.role.is_none();
                    if let Some(buffers) = target.shm.as_mut() {
                        buffers.release(data.key);
                        if hidden {
                            buffers.trim();
                        }
                    }
                }
            }
            BufferOwner::Root(id) => {
                if let Some(window) = host.windows.get_mut(&id) {
                    window.background_released(data.key);
                }
            }
        }
    }
}

/// A buffer lent to a painter for one frame of an SHM target, from [`FramePermit::shm_frame`].
///
/// Paint every pixel (see the [module documentation](self) for the format), then
/// [`submit`](Self::submit). Dropping it instead gives the buffer back unshown. The frame owns
/// the buffer while it is lent, so painting holds no borrow of the host's state: the painter may
/// ask its lease or the host anything meanwhile.
pub struct ShmFrame {
    /// The buffer, until it is submitted or given back.
    lent: Option<(BufferKey, ShmBuffer)>,
    size: BufferSize,
    permit: FramePermit,
}

impl ShmFrame {
    pub(crate) fn new(
        permit: FramePermit,
        key: BufferKey,
        buffer: ShmBuffer,
        size: BufferSize,
    ) -> Self {
        Self {
            lent: Some((key, buffer)),
            size,
            permit,
        }
    }

    /// The buffer's size: the permit's [`Metrics::buffer`].
    pub fn size(&self) -> BufferSize {
        self.size
    }

    /// Bytes per row: `width × 4`, without padding.
    pub fn stride(&self) -> usize {
        self.size.width as usize * BYTES_PER_PIXEL
    }

    /// The metrics this frame is for.
    pub fn metrics(&self) -> Metrics {
        self.permit.metrics()
    }

    /// The pixels, ARGB8888 premultiplied (B, G, R, A bytes), row after row. They hold whatever
    /// this buffer showed last, or zeros when it is new: paint all of them.
    pub fn pixels(&mut self) -> &mut [u8] {
        // Only `submit`, which consumes the frame, takes the buffer out.
        let (_, buffer) = self.lent.as_mut().expect("a frame has its buffer");
        buffer.pixels()
    }

    /// Attaches the buffer, damages all of it, and commits the target's surface. This is the
    /// only place the host commits an SHM leaf with a buffer.
    pub fn submit(mut self) {
        let (key, buffer) = self.lent.take().expect("a frame has its buffer");
        self.permit.submit_shm(key, buffer);
    }
}

impl Drop for ShmFrame {
    fn drop(&mut self) {
        // Runs before the permit (a field) drops and reports the outcome.
        if let Some((key, buffer)) = self.lent.take() {
            if let Some(buffers) = self.permit.target_mut().shm.as_mut() {
                buffers.abandon(key, buffer);
            }
            self.permit.set_outcome(PresentOutcome::NotReady);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    /// Storage that counts how many buffers are alive.
    struct Tracked(Rc<Cell<usize>>);

    impl Drop for Tracked {
        fn drop(&mut self) {
            self.0.set(self.0.get() - 1);
        }
    }

    struct Chain {
        chain: Swapchain<Tracked>,
        alive: Rc<Cell<usize>>,
    }

    impl Chain {
        fn new() -> Self {
            Self {
                chain: Swapchain::new(),
                alive: Rc::new(Cell::new(0)),
            }
        }

        fn acquire(&mut self, width: u32) -> Result<(BufferKey, Tracked), AcquireError> {
            let alive = Rc::clone(&self.alive);
            let size = BufferSize { width, height: 10 };
            self.chain.acquire(size, width as usize * 40, |_| {
                alive.set(alive.get() + 1);
                Ok(Tracked(alive))
            })
        }

        fn frame(&mut self, width: u32) -> BufferKey {
            let (key, buffer) = self.acquire(width).expect("a free buffer");
            self.chain.submit(key, buffer);
            key
        }
    }

    #[test]
    fn trimming_frees_idle_buffers_and_starts_over() {
        let mut c = Chain::new();
        let a = c.frame(100);
        let b = c.frame(100);
        assert!(!c.chain.trim(), "buffers with the compositor stay");
        c.chain.release(a);
        assert!(!c.chain.trim());
        c.chain.release(b);
        assert_eq!(c.alive.get(), 2);
        assert!(c.chain.trim());
        assert_eq!(c.alive.get(), 0);
        assert!(!c.chain.trim(), "nothing left to free");
        let next = c.frame(100);
        assert_ne!(next.generation, a.generation);
        assert_eq!(c.alive.get(), 1);
    }

    #[test]
    fn a_delayed_release_never_lends_a_submitted_buffer() {
        let mut c = Chain::new();
        let a = c.frame(100);
        let b = c.frame(100);
        let third = c.frame(100);
        assert!(a != b && b != third && a != third);
        // All three are with the compositor, which hasn't released any.
        assert!(matches!(c.acquire(100), Err(AcquireError::InUse)));
        assert_eq!(c.chain.state(a), Some(BufferState::Submitted));
        c.chain.release(b);
        let (key, _lent) = c.acquire(100).unwrap();
        assert_eq!(key, b);
        assert_eq!(c.alive.get(), 3);
    }

    #[test]
    fn steady_state_alternates_between_two_buffers() {
        let mut c = Chain::new();
        let mut shown = c.frame(100);
        let mut used = vec![shown];
        for _ in 0..50 {
            // The compositor releases the previous buffer once the next one is committed.
            let next = c.frame(100);
            assert_ne!(next, shown, "never draws into the buffer on screen");
            c.chain.release(shown);
            shown = next;
            if !used.contains(&next) {
                used.push(next);
            }
        }
        assert_eq!(used.len(), 2);
        assert_eq!(c.alive.get(), 2);
    }

    #[test]
    fn a_resize_destroys_free_buffers_and_retires_held_ones() {
        let mut c = Chain::new();
        let shown = c.frame(100);
        let spare = c.frame(100);
        c.chain.release(spare);
        assert_eq!(c.alive.get(), 2);
        let resized = c.frame(120);
        // The free old buffer is gone, the one on screen retires, the new one exists.
        assert_eq!(c.alive.get(), 2);
        assert_eq!(c.chain.state(spare), None);
        assert_eq!(c.chain.state(shown), Some(BufferState::Submitted));
        assert_eq!(c.chain.stats().retiring_generations, 1);
        c.chain.release(shown);
        assert_eq!(c.chain.stats().retiring_generations, 0);
        assert_eq!(c.alive.get(), 1);
        assert_eq!(c.chain.state(resized), Some(BufferState::Submitted));
    }

    #[test]
    fn a_resize_storm_stays_bounded() {
        let mut c = Chain::new();
        let mut shown: Option<BufferKey> = None;
        // Superseded buffers and when they were superseded.
        let mut delayed: Vec<(BufferKey, u32)> = Vec::new();
        let (mut drawn, mut waited) = (0, 0);
        for tick in 0..1000u32 {
            // A new size every tick; the compositor releases a superseded buffer three ticks
            // after the commit that replaced it.
            match c.acquire(100 + tick % 97) {
                Ok((key, buffer)) => {
                    c.chain.submit(key, buffer);
                    drawn += 1;
                    if let Some(previous) = shown.replace(key) {
                        delayed.push((previous, tick));
                    }
                }
                Err(AcquireError::Retiring) => waited += 1,
                Err(error) => panic!("{error:?}"),
            }
            while delayed.first().is_some_and(|&(_, at)| at + 3 <= tick) {
                c.chain.release(delayed.remove(0).0);
            }
            let stats = c.chain.stats();
            assert!(stats.retiring_generations <= MAX_RETIRING, "{stats:?}");
            assert!(
                stats.buffers <= MAX_BUFFERS * (1 + MAX_RETIRING),
                "{stats:?}"
            );
            assert_eq!(stats.buffers, c.alive.get());
        }
        assert!(waited > 0, "slow releases made some frames wait");
        assert!(drawn > 250, "waiting frames resume: {drawn} drawn");
    }

    #[test]
    fn without_releases_a_new_size_waits_instead_of_allocating() {
        let mut c = Chain::new();
        for width in [100, 101, 102] {
            c.frame(width);
        }
        // Two generations retire, each holding its one frame; the third size must wait.
        assert!(matches!(c.acquire(103), Err(AcquireError::Retiring)));
        assert_eq!(c.alive.get(), 3);
    }

    #[test]
    fn an_abandoned_buffer_is_free_again() {
        let mut c = Chain::new();
        let (key, buffer) = c.acquire(100).unwrap();
        c.chain.abandon(key, buffer);
        assert_eq!(c.acquire(100).unwrap().0, key);
    }

    #[test]
    fn a_buffer_given_back_to_a_retired_generation_is_destroyed() {
        let mut c = Chain::new();
        let (old, lent) = c.acquire(100).unwrap();
        // The size changed while it was being drawn: its generation retired.
        let resized = c.frame(120);
        assert_eq!(c.chain.stats().retiring_generations, 1);
        c.chain.abandon(old, lent);
        assert_eq!(c.chain.stats().retiring_generations, 0);
        assert_eq!(c.alive.get(), 1);
        assert_eq!(c.chain.state(resized), Some(BufferState::Submitted));
    }

    #[test]
    fn rgba_becomes_bgra() {
        // Premultiplied half-transparent red, then opaque blue.
        let mut pixels = [128, 0, 0, 128, 0, 0, 255, 255];
        rgba_to_argb8888(&mut pixels);
        assert_eq!(pixels, [0, 0, 128, 128, 255, 0, 0, 255]);
        // As little-endian u32s: 0xAARRGGBB.
        let first = u32::from_le_bytes(pixels[..4].try_into().unwrap());
        assert_eq!(first, 0x8080_0000);
    }

    #[test]
    fn layout_rejects_what_wl_shm_cannot_take() {
        assert_eq!(
            layout(BufferSize {
                width: 3,
                height: 2
            })
            .unwrap(),
            (3, 2, 12, 24)
        );
        assert!(layout(BufferSize {
            width: 0,
            height: 2
        })
        .is_err());
        assert!(layout(BufferSize {
            width: 40_000,
            height: 40_000
        })
        .is_err());
    }
}
