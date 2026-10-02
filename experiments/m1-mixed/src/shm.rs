//! SHM buffers for the CPU-painted surfaces (root and chrome), reused only after the
//! compositor releases them (§10, "SHM buffer rules").

use sighurt_api::geometry::BufferSize;
use smithay_client_toolkit::shm::raw::RawPool;
use smithay_client_toolkit::shm::Shm;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Dispatch, QueueHandle};

/// Which surface's buffers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Root,
    Chrome,
}

/// Each buffer's user data, so a release finds its set, generation and slot.
#[derive(Clone, Copy, Debug)]
pub struct BufferKey {
    pub owner: Owner,
    generation: u64,
    slot: usize,
}

/// A fixed number of equally sized XRGB8888 buffers for one surface.
///
/// A buffer is free (ours to draw into) until it is attached, and then the compositor's until
/// `wl_buffer.release`. With none free, [`acquire`](Self::acquire) returns `None` and the caller
/// keeps its invalidation for the next release. A new size starts a new generation: the old
/// generation's free buffers are destroyed at once, held ones on release. A new size waits
/// (`None` again) while it would leave more than `count` buffers of old sizes awaiting release,
/// so a resize storm keeps at most `count` old buffers, one generation's worth (§10). The
/// release it waits for always comes: every old buffer has been superseded by a newer one,
/// since the caller attaches and commits each buffer it acquires.
pub struct ShmBuffers {
    owner: Owner,
    count: usize,
    generation: u64,
    size: BufferSize,
    pool: Option<RawPool>,
    slots: Vec<Slot>,
    retiring: usize,
    /// The most buffers of old generations that were waiting for release at once.
    pub max_retiring: usize,
    /// How often [`acquire`](Self::acquire) refused a new size until an old buffer's release.
    pub size_waits: u64,
}

struct Slot {
    buffer: WlBuffer,
    held: bool,
}

impl ShmBuffers {
    pub fn new(owner: Owner, count: usize) -> Self {
        Self {
            owner,
            count,
            generation: 0,
            size: BufferSize::default(),
            pool: None,
            slots: Vec::new(),
            retiring: 0,
            max_retiring: 0,
            size_waits: 0,
        }
    }

    /// A free buffer of `size`, as its slot and its pixels (rows of `width × 4` bytes, B G R X
    /// in memory). `None` while the compositor holds every buffer, or, for a new size, while it
    /// holds too many buffers of old sizes.
    pub fn acquire<D>(
        &mut self,
        shm: &Shm,
        size: BufferSize,
        qh: &QueueHandle<D>,
    ) -> Option<(usize, &mut [u8])>
    where
        D: Dispatch<WlBuffer, BufferKey> + 'static,
    {
        if size != self.size || self.pool.is_none() {
            let held = self.slots.iter().filter(|slot| slot.held).count();
            if self.retiring + held > self.count {
                self.size_waits += 1;
                return None;
            }
            self.reallocate(shm, size, qh);
        }
        let slot = self.slots.iter().position(|slot| !slot.held)?;
        let len = size.width as usize * 4 * size.height as usize;
        let pool = self.pool.as_mut()?;
        Some((slot, &mut pool.mmap()[slot * len..][..len]))
    }

    /// Attaches `slot` to `surface`. From here the compositor holds it until it releases it;
    /// the caller damages and commits.
    pub fn attach(&mut self, slot: usize, surface: &WlSurface) {
        surface.attach(Some(&self.slots[slot].buffer), 0, 0);
        self.slots[slot].held = true;
    }

    /// Handles `wl_buffer.release` for one of these buffers.
    pub fn release(&mut self, key: BufferKey, buffer: &WlBuffer) {
        if key.generation == self.generation {
            self.slots[key.slot].held = false;
        } else {
            buffer.destroy();
            self.retiring -= 1;
        }
    }

    /// Destroys the current buffers. Retiring ones go with the connection.
    pub fn destroy(&mut self) {
        for slot in self.slots.drain(..) {
            slot.buffer.destroy();
        }
        self.pool = None;
    }

    fn reallocate<D>(&mut self, shm: &Shm, size: BufferSize, qh: &QueueHandle<D>)
    where
        D: Dispatch<WlBuffer, BufferKey> + 'static,
    {
        for slot in self.slots.drain(..) {
            if slot.held {
                self.retiring += 1;
            } else {
                slot.buffer.destroy();
            }
        }
        self.max_retiring = self.max_retiring.max(self.retiring);
        self.generation += 1;
        self.size = size;
        let stride = size.width as usize * 4;
        let len = stride * size.height as usize;
        // Dropping the old pool unmaps it and destroys the wl_shm_pool; the compositor keeps
        // the memory of buffers it still holds until they are destroyed.
        let mut pool = RawPool::new(len * self.count, shm).expect("cannot create an SHM pool");
        for slot in 0..self.count {
            let key = BufferKey {
                owner: self.owner,
                generation: self.generation,
                slot,
            };
            let buffer = pool.create_buffer(
                (slot * len) as i32,
                size.width as i32,
                size.height as i32,
                stride as i32,
                Format::Xrgb8888,
                key,
                qh,
            );
            self.slots.push(Slot {
                buffer,
                held: false,
            });
        }
        self.pool = Some(pool);
    }
}
