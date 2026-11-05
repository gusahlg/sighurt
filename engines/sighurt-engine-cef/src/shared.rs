//! What CEF's UI thread and the owner thread share: the event queue, and per view the geometry
//! CEF asks for, the staged frame, the browser handle and the navigation state.
//!
//! CEF's callbacks run on its own UI thread (`multi_threaded_message_loop`, decision in
//! docs/rewrite/CEF.md). They only queue events here and wake the owner loop; the owner drains
//! them in `pump`. Nothing in this module calls CEF.
//!
//! Must not own: CEF objects other than the thread-safe browser handle, or any policy.

use std::collections::{HashMap, VecDeque};
use std::mem::{discriminant, Discriminant};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sighurt_api::engine::EngineWaker;
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::SurfaceRect;
use sighurt_api::ids::{DocumentGeneration, ViewId};
use sighurt_api::input::TextInputState;

use crate::actions::Action;
use crate::capi::{CefBrowser, CefBrowserHost, CefRect};
use crate::ffi::Ref;
use crate::find::FindState;
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
/// The most navigation events (started, committed, failed, requested) held for one view. They
/// have a budget of their own, which nothing else a page does can use up: a dropped commit would
/// leave the browser on the previous document. Chromium throttles a page's same-document
/// navigations and [`crate::nav::MAX_HELD`] bounds the held ones, so only an owner that stopped
/// pumping reaches it.
pub const MAX_NAV_QUEUED_PER_VIEW: usize = 256;
/// The most commands held for a browser that is not created yet.
pub const MAX_PENDING: usize = 4096;
/// Superseded replaceable events are skipped when drained; past this many (and more than the
/// live ones) they are swept out at once.
const MAX_STALE: usize = 1024;

/// How an event is admitted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Begins or ends a view's life, or fails the engine: never refused.
    Life,
    /// Supersedes an undelivered earlier one of its kind for the same view.
    Replaceable,
    /// A navigation event: within [`MAX_NAV_QUEUED_PER_VIEW`].
    Navigation,
    /// Anything else a page can cause: within [`MAX_QUEUED`], [`MAX_QUEUED_BYTES`] and
    /// [`MAX_QUEUED_PER_VIEW`].
    Page,
}

type Key = (Option<ViewId>, Discriminant<EngineEvent>);

/// One view's queued events, by budget.
#[derive(Default)]
struct Counts {
    page: usize,
    navigation: usize,
}

#[derive(Default)]
struct Queue {
    /// Each event with its sequence number.
    events: VecDeque<(u64, EngineEvent)>,
    next: u64,
    /// The sequence number of the live event of each replaceable kind and view: an earlier
    /// one still in `events` is superseded and skipped.
    latest: HashMap<Key, u64>,
    /// Superseded events still in `events`.
    stale: usize,
    /// Per view, what its events use of their budgets.
    counts: HashMap<Option<ViewId>, Counts>,
    /// Page events queued, and their weight.
    page: usize,
    page_bytes: usize,
    /// Events refused for want of room, for the diagnostics.
    dropped: u64,
}

impl Queue {
    /// Whether the queued `(seq, event)` was superseded.
    fn is_stale(&self, seq: u64, event: &EngineEvent) -> bool {
        class(event) == Class::Replaceable
            && self.latest.get(&(event.view(), discriminant(event))) != Some(&seq)
    }

    /// Takes the next live event, skipping superseded ones, and gives back its budget.
    fn pop(&mut self) -> Option<EngineEvent> {
        while let Some((seq, event)) = self.events.pop_front() {
            if self.is_stale(seq, &event) {
                self.stale -= 1;
                continue;
            }
            self.release(&event);
            return Some(event);
        }
        None
    }

    /// Gives back what `event` used, as it leaves the queue.
    fn release(&mut self, event: &EngineEvent) {
        let view = event.view();
        match class(event) {
            Class::Life => {}
            Class::Replaceable => {
                self.latest.remove(&(view, discriminant(event)));
            }
            Class::Navigation | Class::Page => {
                let counts = self.counts.entry(view).or_default();
                if class(event) == Class::Page {
                    counts.page -= 1;
                    self.page -= 1;
                    self.page_bytes -= weight(event);
                } else {
                    counts.navigation -= 1;
                }
                if counts.page == 0 && counts.navigation == 0 {
                    self.counts.remove(&view);
                }
            }
        }
    }

    /// Drops the superseded events at the front.
    fn skip_stale(&mut self) {
        while let Some((seq, event)) = self.events.front() {
            if !self.is_stale(*seq, event) {
                break;
            }
            self.events.pop_front();
            self.stale -= 1;
        }
    }

    /// Rebuilds the bookkeeping from the events, keeping only the live ones and those `keep`
    /// accepts.
    fn rebuild(&mut self, keep: impl Fn(&EngineEvent) -> bool) {
        let events = std::mem::take(&mut self.events);
        let live: Vec<_> = events
            .into_iter()
            .filter(|(seq, event)| !self.is_stale(*seq, event) && keep(event))
            .collect();
        self.latest.clear();
        self.counts.clear();
        self.stale = 0;
        self.page = 0;
        self.page_bytes = 0;
        for (seq, event) in live {
            self.admit(seq, &event);
            self.events.push_back((seq, event));
        }
    }

    /// Records what `event`, numbered `seq`, uses.
    fn admit(&mut self, seq: u64, event: &EngineEvent) {
        let view = event.view();
        match class(event) {
            Class::Life => {}
            Class::Replaceable => {
                if self
                    .latest
                    .insert((view, discriminant(event)), seq)
                    .is_some()
                {
                    self.stale += 1;
                }
            }
            Class::Navigation => self.counts.entry(view).or_default().navigation += 1,
            Class::Page => {
                self.counts.entry(view).or_default().page += 1;
                self.page += 1;
                self.page_bytes += weight(event);
            }
        }
    }

    /// Whether `event` fits its budget.
    fn fits(&self, event: &EngineEvent) -> bool {
        let counts = self.counts.get(&event.view());
        match class(event) {
            Class::Life | Class::Replaceable => true,
            Class::Navigation => counts.map_or(0, |c| c.navigation) < MAX_NAV_QUEUED_PER_VIEW,
            Class::Page => {
                self.page < MAX_QUEUED
                    && self.page_bytes + weight(event) <= MAX_QUEUED_BYTES
                    && counts.map_or(0, |c| c.page) < MAX_QUEUED_PER_VIEW
            }
        }
    }
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
            queue: Mutex::new(Queue::default()),
            waker,
            live: AtomicUsize::new(0),
        })
    }

    /// Queues `events`. A replaceable event supersedes an undelivered earlier one of its kind
    /// for the same view (the later keeps its own place), so those are bounded by kind.
    /// Navigation events are admitted within their own budget, [`MAX_NAV_QUEUED_PER_VIEW`].
    /// Every other event a page can cause is admitted only within [`MAX_QUEUED`],
    /// [`MAX_QUEUED_BYTES`] and [`MAX_QUEUED_PER_VIEW`]; what does not fit is dropped and
    /// counted (a page that floods popups or downloads loses the excess, not the browser's
    /// memory, and never its navigations). Life-cycle events are always admitted. The cost of
    /// each event is constant: nothing scans the queue. Wakes the owner loop.
    pub fn push_all(&self, events: impl IntoIterator<Item = EngineEvent>) {
        let mut queue = lock(&self.queue);
        let mut any = false;
        for event in events {
            if !queue.fits(&event) {
                if queue.dropped == 0 {
                    log::warn!("cef: the event queue is full; page events are being dropped");
                }
                queue.dropped += 1;
                continue;
            }
            let seq = queue.next;
            queue.next += 1;
            queue.admit(seq, &event);
            queue.events.push_back((seq, event));
            any = true;
        }
        if queue.stale > MAX_STALE && queue.stale > queue.events.len() / 2 {
            queue.rebuild(|_| true);
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
        for _ in 0..max {
            let Some(event) = queue.pop() else {
                break;
            };
            out.push(event);
        }
        queue.skip_stale();
        !queue.events.is_empty()
    }

    /// Drops what is queued for `view` (it closed), except a `ViewClosed` that is already
    /// there: that one is the core's only way to forget the view.
    pub fn forget(&self, view: ViewId) {
        lock(&self.queue)
            .rebuild(|e| e.view() != Some(view) || matches!(e, EngineEvent::ViewClosed { .. }));
    }

    /// Events refused for want of room so far.
    pub fn dropped(&self) -> u64 {
        lock(&self.queue).dropped
    }
}

/// How `event` is admitted.
fn class(event: &EngineEvent) -> Class {
    match event {
        EngineEvent::ViewReady { .. }
        | EngineEvent::ViewClosed { .. }
        | EngineEvent::ViewFailed { .. }
        | EngineEvent::RuntimeFailed { .. } => Class::Life,
        EngineEvent::NavigationStarted { .. }
        | EngineEvent::NavigationCommitted { .. }
        | EngineEvent::NavigationFailed { .. }
        | EngineEvent::NavigationRequested { .. } => Class::Navigation,
        EngineEvent::TitleChanged { .. }
        | EngineEvent::StatusTextChanged { .. }
        | EngineEvent::CursorChanged { .. }
        | EngineEvent::LoadingChanged { .. }
        | EngineEvent::HistoryChanged { .. }
        | EngineEvent::TextInputChanged { .. }
        | EngineEvent::SemanticsChanged { .. }
        | EngineEvent::FindResult { .. }
        | EngineEvent::FrameReady { .. } => Class::Replaceable,
        _ => Class::Page,
    }
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
    /// Where the text caret is, as far as known (the last click, then the composition's last
    /// character), surface-local, for the input method.
    pub caret: Mutex<SurfaceRect>,
    /// The focused field's text-input state as last reported, so a moved caret can be
    /// reported again with it.
    pub text_input: Mutex<Option<TextInputState>>,
    /// `close_view` ran: `DoClose` lets the browser close.
    pub closing: AtomicBool,
    /// Find in page, as CEF's UI thread sees it.
    pub find: Mutex<FindState>,
    /// The document last reported in element fullscreen, until it is reported out of it.
    pub fullscreen: Mutex<Option<DocumentGeneration>>,
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
            text_input: Mutex::new(None),
            closing: AtomicBool::new(false),
            find: Mutex::new(FindState::default()),
            fullscreen: Mutex::new(None),
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

    /// The page entered (`true`) or left element fullscreen, as Chromium said: the event to
    /// report, if any. Entering is reported once, for the document shown; leaving, for the
    /// document that entered.
    pub fn fullscreen_changed(&self, fullscreen: bool) -> Option<EngineEvent> {
        if !fullscreen {
            return self.left_fullscreen();
        }
        let document = lock(&self.nav).document()?;
        let mut current = lock(&self.fullscreen);
        if current.is_some() {
            return None;
        }
        *current = Some(document);
        Some(EngineEvent::FullscreenChanged {
            view: self.id,
            document,
            fullscreen: true,
        })
    }

    /// Reports that the page left element fullscreen, if it was last reported in it: Chromium
    /// said so, or the document, the page or the view went while it was.
    pub fn left_fullscreen(&self) -> Option<EngineEvent> {
        let document = lock(&self.fullscreen).take()?;
        Some(EngineEvent::FullscreenChanged {
            view: self.id,
            document,
            fullscreen: false,
        })
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
    fn a_popup_flood_never_costs_a_view_its_navigations() {
        use sighurt_api::events::{HistoryEffect, OpenDisposition};
        use sighurt_api::ids::{DocumentGeneration, NavigationId};
        let shared = shared();
        let view = ViewId::next();
        let document = DocumentGeneration::next();
        // Every slot a page has, and the whole queue's, taken by popups (of many views).
        for n in 0..MAX_QUEUED * 2 {
            shared.push(EngineEvent::OpenRequested {
                view: if n % 2 == 0 { view } else { ViewId::next() },
                document,
                url: format!("https://x/{n}"),
                disposition: OpenDisposition::NewView,
            });
        }
        let navigation = NavigationId::next();
        let committed = DocumentGeneration::next();
        shared.push_all([
            EngineEvent::NavigationStarted {
                view,
                navigation,
                url: "https://a/".into(),
            },
            EngineEvent::NavigationCommitted {
                view,
                navigation,
                url: "https://a/".into(),
                document: committed,
                history: Some(HistoryEffect::Push),
            },
        ]);
        let mut out = Vec::new();
        shared.drain(usize::MAX, &mut out);
        let nav: Vec<_> = out
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    EngineEvent::NavigationStarted { .. } | EngineEvent::NavigationCommitted { .. }
                )
            })
            .collect();
        assert_eq!(nav.len(), 2, "the navigation got through the flood");
        assert!(matches!(
            out.last(),
            Some(EngineEvent::NavigationCommitted { document, .. }) if *document == committed
        ));
    }

    #[test]
    fn navigations_have_a_budget_of_their_own() {
        use sighurt_api::events::NavigationError;
        use sighurt_api::ids::NavigationId;
        let shared = shared();
        let (view, other) = (ViewId::next(), ViewId::next());
        let failed = |view| EngineEvent::NavigationFailed {
            view,
            navigation: NavigationId::next(),
            error: NavigationError::Cancelled,
        };
        for _ in 0..MAX_NAV_QUEUED_PER_VIEW + 10 {
            shared.push(failed(view));
        }
        shared.push(failed(other));
        let mut out = Vec::new();
        shared.drain(usize::MAX, &mut out);
        assert_eq!(
            out.iter().filter(|e| e.view() == Some(view)).count(),
            MAX_NAV_QUEUED_PER_VIEW
        );
        assert_eq!(out.iter().filter(|e| e.view() == Some(other)).count(), 1);
        // Drained, the budget is back.
        shared.push(failed(view));
        out.clear();
        shared.drain(usize::MAX, &mut out);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_flood_of_replaceable_events_keeps_only_the_latest_and_stays_small() {
        let shared = shared();
        let view = ViewId::next();
        for n in 0..100_000 {
            shared.push(EngineEvent::TitleChanged {
                view,
                title: Some(n.to_string()),
            });
        }
        assert!(lock(&shared.queue).events.len() <= 2 * MAX_STALE + 2);
        let mut out = Vec::new();
        assert!(!shared.drain(usize::MAX, &mut out));
        assert!(
            matches!(out.as_slice(), [EngineEvent::TitleChanged { title: Some(t), .. }] if t == "99999")
        );
    }

    #[test]
    fn a_bounded_drain_counts_only_live_events() {
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
        let mut out = Vec::new();
        // The superseded title is skipped, not delivered nor counted.
        assert!(shared.drain(1, &mut out));
        assert!(matches!(out.as_slice(), [EngineEvent::ViewReady { .. }]));
        assert!(!shared.drain(1, &mut out));
        assert!(matches!(&out[1], EngineEvent::TitleChanged { title: Some(t), .. } if t == "b"));
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
