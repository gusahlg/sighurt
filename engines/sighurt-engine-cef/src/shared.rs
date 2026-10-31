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

/// The most events held for the owner (page-controlled ones: popups, downloads, held links).
pub const MAX_QUEUED: usize = 4096;
/// The most bytes (estimated) of such events.
pub const MAX_QUEUED_BYTES: usize = 16 << 20;
/// The most of them from one view: a flooding page cannot starve the others.
pub const MAX_QUEUED_PER_VIEW: usize = 512;
/// The most commands held for a browser that is not created yet.
pub const MAX_PENDING: usize = 4096;

struct Queue {
    events: VecDeque<EngineEvent>,
    /// Events refused for want of room, for the diagnostics.
    dropped: u64,
}

/// The runtime's event queue.
pub struct Shared {
    queue: Mutex<Queue>,
    waker: EngineWaker,
    /// Browsers created and not yet closed (shutdown waits for it to reach zero).
    pub live: AtomicUsize,
}

impl Shared {
    /// An empty queue that wakes `waker` when something arrives.
    pub fn new(waker: EngineWaker) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Queue {
                events: VecDeque::new(),
                dropped: 0,
            }),
            waker,
            live: AtomicUsize::new(0),
        })
    }

    /// Queues `events`. A replaceable event supersedes an undelivered earlier one of its kind
    /// for the same view (the later keeps its own place), so those are bounded by kind. Every
    /// other event a page can cause is admitted only within [`MAX_QUEUED`],
    /// [`MAX_QUEUED_BYTES`] and [`MAX_QUEUED_PER_VIEW`]; what does not fit is dropped and
    /// counted (a page that floods popups or downloads loses the excess, not the browser's
    /// memory). Life-cycle events are always admitted. Wakes the owner loop.
    pub fn push_all(&self, events: impl IntoIterator<Item = EngineEvent>) {
        let mut queue = lock(&self.queue);
        let mut any = false;
        for event in events {
            if replaceable(&event) {
                let kind = discriminant(&event);
                let view = event.view();
                queue
                    .events
                    .retain(|queued| discriminant(queued) != kind || queued.view() != view);
            } else if !always_admitted(&event) {
                let view = event.view();
                let from_view = queue.events.iter().filter(|q| q.view() == view).count();
                let bytes: usize = queue.events.iter().map(weight).sum::<usize>() + weight(&event);
                if queue.events.len() >= MAX_QUEUED
                    || bytes > MAX_QUEUED_BYTES
                    || from_view >= MAX_QUEUED_PER_VIEW
                {
                    if queue.dropped == 0 {
                        log::warn!("cef: the event queue is full; page events are being dropped");
                    }
                    queue.dropped += 1;
                    continue;
                }
            }
            queue.events.push_back(event);
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
        let count = queue.events.len().min(max);
        out.extend(queue.events.drain(..count));
        !queue.events.is_empty()
    }

    /// Drops what is queued for `view` (it closed), except a `ViewClosed` that is already
    /// there: that one is the core's only way to forget the view.
    pub fn forget(&self, view: ViewId) {
        lock(&self.queue)
            .events
            .retain(|e| e.view() != Some(view) || matches!(e, EngineEvent::ViewClosed { .. }));
    }

    /// Events refused for want of room so far.
    pub fn dropped(&self) -> u64 {
        lock(&self.queue).dropped
    }
}

/// Events that end or begin a view's life, or fail the engine: never refused.
fn always_admitted(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::ViewReady { .. }
            | EngineEvent::ViewClosed { .. }
            | EngineEvent::ViewFailed { .. }
            | EngineEvent::RuntimeFailed { .. }
    )
}

/// What an event costs while queued: its own size and its strings.
fn weight(event: &EngineEvent) -> usize {
    let strings = match event {
        EngineEvent::NavigationStarted { url, .. }
        | EngineEvent::NavigationCommitted { url, .. }
        | EngineEvent::NavigationRequested { url, .. }
        | EngineEvent::OpenRequested { url, .. } => url.len(),
        EngineEvent::ServiceRequested(request) => {
            request.origin.as_ref().map_or(0, String::len)
                + match &request.service {
                    sighurt_api::services::Service::Download {
                        source,
                        suggested_name,
                    } => {
                        suggested_name.as_ref().map_or(0, String::len)
                            + match source {
                                sighurt_api::services::DownloadSource::Url(url) => url.len(),
                                sighurt_api::services::DownloadSource::Data(data) => data.len(),
                            }
                    }
                    sighurt_api::services::Service::ClipboardWrite { items, .. } => {
                        items.iter().map(|item| item.data.len()).sum()
                    }
                    _ => 0,
                }
        }
        EngineEvent::ViewFailed { message, .. } => message.len(),
        _ => 0,
    };
    std::mem::size_of::<EngineEvent>() + strings
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
    /// What each of the latest frames changed: its number and the rectangle (`None`: all of it).
    history: VecDeque<(u64, Option<[usize; 4]>)>,
}

/// What changed in the staged frame since a frame number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Damage {
    /// Nothing.
    None,
    /// This rectangle, `[x0, y0, x1, y1]`, end excluded.
    Rect([usize; 4]),
    /// Everything, or more than is remembered.
    Full,
}

/// How many frames of damage are remembered (a lease's swapchain is a few buffers deep).
const HISTORY: usize = 16;

impl Staged {
    /// Copies the `rects` of CEF's `buffer` (`width` by `height`) in. A change of size copies
    /// the whole frame.
    pub fn apply(&mut self, buffer: &[u8], width: u32, height: u32, rects: &[CefRect]) {
        let (w, h) = (width as usize, height as usize);
        if buffer.len() < w * h * 4 || w == 0 || h == 0 {
            return;
        }
        let full = self.width != width || self.height != height || !self.valid;
        let mut damage: Option<[usize; 4]> = None;
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
                damage = Some(match damage {
                    None => [x0, y0, x1, y1],
                    Some(d) => [d[0].min(x0), d[1].min(y0), d[2].max(x1), d[3].max(y1)],
                });
            }
        }
        self.valid = true;
        self.frames += 1;
        // A full copy is all of it; a partial one, the union of its rectangles (empty: nothing).
        let entry = if full {
            None
        } else {
            Some(damage.unwrap_or([0, 0, 0, 0]))
        };
        self.history.push_back((self.frames, entry));
        if self.history.len() > HISTORY {
            self.history.pop_front();
        }
    }

    /// What changed in the frames after number `frame` (what a buffer written at `frame` lacks).
    pub fn damage_since(&self, frame: u64) -> Damage {
        if frame == self.frames {
            return Damage::None;
        }
        if frame > self.frames || self.history.front().is_none_or(|(n, _)| *n > frame + 1) {
            return Damage::Full;
        }
        let mut union: Option<[usize; 4]> = None;
        for (_, entry) in self.history.iter().filter(|(n, _)| *n > frame) {
            let Some(d) = entry else {
                return Damage::Full;
            };
            if d[0] >= d[2] || d[1] >= d[3] {
                continue;
            }
            union = Some(match union {
                None => *d,
                Some(u) => [
                    u[0].min(d[0]),
                    u[1].min(d[1]),
                    u[2].max(d[2]),
                    u[3].max(d[3]),
                ],
            });
        }
        union.map_or(Damage::None, Damage::Rect)
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
    /// Commands queued until the browser exists and its queue is drained.
    pub pending: Vec<Action>,
    /// The browser exists and nothing is waiting in `pending`: commands run at once.
    pub ready: bool,
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

    /// Does `action` now if the browser is ready, else queues it (in order, within
    /// [`MAX_PENDING`]; a view that is never created loses the excess). The lock is not held
    /// while CEF is called: a close can re-enter `OnBeforeClose`, which takes it.
    pub fn run(&self, action: Action) {
        let mut slot = lock(&self.slot);
        if slot.closed {
            return;
        }
        let (Some(browser), Some(host)) = (slot.browser.clone(), slot.host.clone()) else {
            queue_pending(&mut slot, action);
            return;
        };
        if !slot.ready {
            queue_pending(&mut slot, action);
            return;
        }
        drop(slot);
        action.perform(&browser, &host);
    }

    /// The browser's identifier of the focused frame, if there is one.
    pub fn focused_frame(&self) -> Option<String> {
        let browser = lock(&self.slot).browser.clone()?;
        // SAFETY: a live browser reference we hold; the frame is an owned reference.
        unsafe {
            let frame = Ref::adopt(crate::ffi::call!(browser.as_ptr(), get_focused_frame))?;
            Some(crate::ffi::take_capped(
                crate::ffi::call!(frame.as_ptr(), get_identifier),
                256,
            ))
        }
    }

    /// Whether `close_view` ran.
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }
}

fn queue_pending(slot: &mut Slot, action: Action) {
    if slot.pending.len() < MAX_PENDING {
        slot.pending.push(action);
    } else {
        log::warn!("cef: a view that is not created yet dropped a command");
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
    fn a_flooding_view_is_bounded_and_life_cycle_events_still_get_through() {
        use sighurt_api::events::OpenDisposition;
        use sighurt_api::ids::DocumentGeneration;
        let shared = shared();
        let (flood, other) = (ViewId::next(), ViewId::next());
        let document = DocumentGeneration::next();
        for n in 0..10_000 {
            shared.push(EngineEvent::OpenRequested {
                view: flood,
                document,
                url: format!("https://x/{n}"),
                disposition: OpenDisposition::NewView,
            });
        }
        // Another view's request still fits, and the flood's end is still reported.
        shared.push(EngineEvent::OpenRequested {
            view: other,
            document,
            url: "https://y/".into(),
            disposition: OpenDisposition::NewView,
        });
        shared.push(EngineEvent::ViewClosed { view: flood });
        let mut out = Vec::new();
        shared.drain(usize::MAX, &mut out);
        let from_flood = out.iter().filter(|e| e.view() == Some(flood)).count();
        assert_eq!(
            from_flood,
            MAX_QUEUED_PER_VIEW + 1,
            "the cap, and the ViewClosed"
        );
        assert!(out.iter().any(|e| e.view() == Some(other)));
        assert!(shared.dropped() >= 10_000 - MAX_QUEUED_PER_VIEW as u64);
    }

    #[test]
    fn the_queue_has_a_byte_budget_too() {
        use sighurt_api::events::OpenDisposition;
        use sighurt_api::ids::DocumentGeneration;
        let shared = shared();
        let document = DocumentGeneration::next();
        // 1 MiB addresses from many views: no view hits its own cap, the budget does.
        for _ in 0..64 {
            shared.push(EngineEvent::OpenRequested {
                view: ViewId::next(),
                document,
                url: "u".repeat(1 << 20),
                disposition: OpenDisposition::NewView,
            });
        }
        let mut out = Vec::new();
        shared.drain(usize::MAX, &mut out);
        assert!(
            out.len() <= MAX_QUEUED_BYTES >> 20,
            "{} events of 1 MiB",
            out.len()
        );
        assert!(!out.is_empty());
    }

    #[test]
    fn a_published_view_closed_survives_the_forgetting() {
        let shared = shared();
        let view = ViewId::next();
        shared.push(EngineEvent::TitleChanged {
            view,
            title: Some("t".into()),
        });
        shared.push(EngineEvent::ViewClosed { view });
        shared.forget(view);
        let mut out = Vec::new();
        shared.drain(10, &mut out);
        assert!(matches!(out.as_slice(), [EngineEvent::ViewClosed { .. }]));
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
