//! What CEF's UI thread and the owner thread share: the event queue, and per view the geometry
//! CEF asks for, the staged frame, the browser handle and the navigation state.
//!
//! CEF's callbacks run on its own UI thread (`multi_threaded_message_loop`, decision in
//! docs/rewrite/CEF.md). They only queue events here and wake the owner loop; the owner drains
//! them in `pump`. Nothing in this module calls CEF.
//!
//! Must not own: CEF objects other than the thread-safe browser handle, or any policy.

use std::collections::VecDeque;
use std::mem::discriminant;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sighurt_api::engine::EngineWaker;
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::SurfaceRect;
use sighurt_api::ids::ViewId;

use crate::actions::Action;
use crate::capi::{CefBrowser, CefBrowserHost, CefRect};
use crate::ffi::Ref;
use crate::nav::Nav;

/// Locks, ignoring poison: a panic in one callback must not wedge the rest of the engine.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The most bytes of selected text kept for a copy.
pub const MAX_SELECTION: usize = 1 << 20;

/// The runtime's event queue.
pub struct Shared {
    queue: Mutex<VecDeque<EngineEvent>>,
    waker: EngineWaker,
    /// Browsers created and not yet closed (shutdown waits for it to reach zero).
    pub live: AtomicUsize,
}

impl Shared {
    /// An empty queue that wakes `waker` when something arrives.
    pub fn new(waker: EngineWaker) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            waker,
            live: AtomicUsize::new(0),
        })
    }

    /// Queues `events`. A replaceable event supersedes an undelivered earlier one of its kind
    /// for the same view (the later keeps its own place). Wakes the owner loop.
    pub fn push_all(&self, events: impl IntoIterator<Item = EngineEvent>) {
        let mut queue = lock(&self.queue);
        let mut any = false;
        for event in events {
            if replaceable(&event) {
                let kind = discriminant(&event);
                let view = event.view();
                queue.retain(|queued| discriminant(queued) != kind || queued.view() != view);
            }
            queue.push_back(event);
            any = true;
        }
        drop(queue);
        if any {
            self.waker.wake();
        }
    }

    /// Queues one event.
    pub fn push(&self, event: EngineEvent) {
        self.push_all([event]);
    }

    /// Takes up to `max` events; whether more are left.
    pub fn drain(&self, max: usize, out: &mut Vec<EngineEvent>) -> bool {
        let mut queue = lock(&self.queue);
        let count = queue.len().min(max);
        out.extend(queue.drain(..count));
        !queue.is_empty()
    }

    /// Drops what is queued for `view` (it closed).
    pub fn forget(&self, view: ViewId) {
        lock(&self.queue).retain(|event| event.view() != Some(view));
    }
}

fn replaceable(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::TitleChanged { .. }
            | EngineEvent::StatusTextChanged { .. }
            | EngineEvent::CursorChanged { .. }
            | EngineEvent::LoadingChanged { .. }
            | EngineEvent::HistoryChanged { .. }
            | EngineEvent::TextInputChanged { .. }
            | EngineEvent::SemanticsChanged { .. }
            | EngineEvent::FrameReady { .. }
    )
}

/// The size and scale CEF is told the view has.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    /// Logical width.
    pub width: u32,
    /// Logical height.
    pub height: u32,
    /// The scale factor (the host's rational as a float; buffer sizes come from what CEF paints).
    pub scale: f32,
}

/// The view's last frame from CEF: BGRA, premultiplied, rows top to bottom.
#[derive(Default)]
pub struct Staged {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `4 * width * height` bytes.
    pub pixels: Vec<u8>,
    /// Whether anything was ever painted.
    pub valid: bool,
    /// Bytes copied from CEF's buffers, for the diagnostics.
    pub copied: u64,
    /// Frames received.
    pub frames: u64,
}

impl Staged {
    /// Copies the `rects` of CEF's `buffer` (`width` by `height`) in. A change of size copies
    /// the whole frame.
    pub fn apply(&mut self, buffer: &[u8], width: u32, height: u32, rects: &[CefRect]) {
        let (w, h) = (width as usize, height as usize);
        if buffer.len() < w * h * 4 || w == 0 || h == 0 {
            return;
        }
        let full = self.width != width || self.height != height || !self.valid;
        if full {
            self.width = width;
            self.height = height;
            self.pixels.clear();
            self.pixels.extend_from_slice(&buffer[..w * h * 4]);
            self.copied += (w * h * 4) as u64;
        } else {
            for rect in rects {
                let x0 = usize::try_from(rect.x.max(0)).unwrap_or(0).min(w);
                let y0 = usize::try_from(rect.y.max(0)).unwrap_or(0).min(h);
                let x1 = usize::try_from(rect.x.saturating_add(rect.width).max(0))
                    .unwrap_or(0)
                    .min(w);
                let y1 = usize::try_from(rect.y.saturating_add(rect.height).max(0))
                    .unwrap_or(0)
                    .min(h);
                if x0 >= x1 {
                    continue;
                }
                for y in y0..y1 {
                    let from = (y * w + x0) * 4;
                    let to = (y * w + x1) * 4;
                    self.pixels[from..to].copy_from_slice(&buffer[from..to]);
                }
                self.copied += ((x1 - x0) * (y1 - y0) * 4) as u64;
            }
        }
        self.valid = true;
        self.frames += 1;
    }

    /// Releases the buffer (the view closed).
    pub fn release(&mut self) {
        *self = Self::default();
    }
}

/// Where the browser is: not created yet (commands wait), live, or closed.
#[derive(Default)]
pub struct Slot {
    /// The browser, once `OnAfterCreated` ran.
    pub browser: Option<Ref<CefBrowser>>,
    /// Its host, for the calls that go to it.
    pub host: Option<Ref<CefBrowserHost>>,
    /// Commands queued before the browser existed.
    pub pending: Vec<Action>,
    /// `OnBeforeClose` ran.
    pub closed: bool,
}

/// One view's state, shared with CEF's callbacks.
pub struct ViewShared {
    /// The view.
    pub id: ViewId,
    /// The runtime's queue.
    pub shared: Arc<Shared>,
    /// What `GetViewRect` and `GetScreenInfo` answer.
    pub geometry: Mutex<Geometry>,
    /// The latest frame.
    pub staged: Mutex<Staged>,
    /// The browser.
    pub slot: Mutex<Slot>,
    /// Navigation state.
    pub nav: Mutex<Nav>,
    /// The page's selected text, for a copy.
    pub selection: Mutex<String>,
    /// Where the text caret is, as far as known (the last click), for the input method.
    pub caret: Mutex<SurfaceRect>,
    /// `close_view` ran: `DoClose` lets the browser close.
    pub closing: AtomicBool,
}

impl ViewShared {
    /// A view with no browser yet.
    pub fn new(id: ViewId, shared: Arc<Shared>, geometry: Geometry) -> Arc<Self> {
        Arc::new(Self {
            id,
            shared,
            geometry: Mutex::new(geometry),
            staged: Mutex::new(Staged::default()),
            slot: Mutex::new(Slot::default()),
            nav: Mutex::new(Nav::new(id)),
            selection: Mutex::new(String::new()),
            caret: Mutex::new(SurfaceRect::default()),
            closing: AtomicBool::new(false),
        })
    }

    /// Does `action` now if the browser exists, else queues it when it can wait (input is
    /// dropped: a key typed before the browser exists has no page to reach).
    pub fn run(&self, action: Action) {
        let mut slot = lock(&self.slot);
        if slot.closed {
            return;
        }
        match (&slot.browser, &slot.host) {
            (Some(browser), Some(host)) => action.perform(browser, host),
            _ if action.deferrable() => slot.pending.push(action),
            _ => {}
        }
    }

    /// Whether `close_view` ran.
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_api::events::Cursor;

    fn shared() -> Arc<Shared> {
        Shared::new(EngineWaker::new(|| {}))
    }

    #[test]
    fn a_later_title_replaces_an_earlier_one_but_other_events_stay() {
        let shared = shared();
        let view = ViewId::next();
        shared.push(EngineEvent::TitleChanged {
            view,
            title: Some("a".into()),
        });
        shared.push(EngineEvent::ViewReady { view });
        shared.push(EngineEvent::TitleChanged {
            view,
            title: Some("b".into()),
        });
        shared.push(EngineEvent::CursorChanged {
            view,
            cursor: Cursor::Text,
        });
        let mut out = Vec::new();
        assert!(!shared.drain(10, &mut out));
        assert_eq!(out.len(), 3);
        assert!(matches!(out[0], EngineEvent::ViewReady { .. }));
        assert!(matches!(&out[1], EngineEvent::TitleChanged { title: Some(t), .. } if t == "b"));
    }

    #[test]
    fn drain_is_bounded() {
        let shared = shared();
        let view = ViewId::next();
        for _ in 0..5 {
            shared.push(EngineEvent::ViewReady { view });
        }
        let mut out = Vec::new();
        assert!(shared.drain(3, &mut out));
        assert!(!shared.drain(3, &mut out));
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn dirty_rects_are_all_that_is_copied_after_the_first_frame() {
        let mut staged = Staged::default();
        let first = vec![1u8; 4 * 4 * 4];
        staged.apply(&first, 4, 4, &[]);
        assert_eq!(staged.copied, 64);
        let mut second = vec![9u8; 4 * 4 * 4];
        second[0] = 7;
        let rect = CefRect {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        };
        staged.apply(&second, 4, 4, &[rect]);
        assert_eq!(staged.copied, 64 + 8);
        // Only the two pixels of row 1 changed.
        assert_eq!(staged.pixels[0], 1);
        assert_eq!(
            &staged.pixels[(4 + 1) * 4..(4 + 3) * 4],
            &second[(4 + 1) * 4..(4 + 3) * 4]
        );
        assert_eq!(staged.pixels[(4 + 3) * 4], 1);
        // A resize copies the lot.
        staged.apply(&[3u8; 2 * 2 * 4], 2, 2, &[]);
        assert_eq!(
            (staged.width, staged.height, staged.pixels.len()),
            (2, 2, 16)
        );
    }

    #[test]
    fn a_rect_outside_the_buffer_copies_nothing() {
        let mut staged = Staged::default();
        staged.apply(&[0u8; 64], 4, 4, &[]);
        let rect = CefRect {
            x: 100,
            y: 100,
            width: 5,
            height: 5,
        };
        staged.apply(&[1u8; 64], 4, 4, &[rect]);
        assert!(staged.pixels.iter().all(|&b| b == 0));
    }
}
