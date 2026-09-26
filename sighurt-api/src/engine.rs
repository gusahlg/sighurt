//! The contract every content engine implements, and what the browser tells an engine.
//!
//! The boundary has four parts: control ([`EngineCommand`], [`EngineRuntime::open_view`],
//! [`EngineRuntime::close_view`]), presentation (a host-managed target and frame permits),
//! services ([`crate::services`]) and semantics ([`crate::events`]). A surface is where an
//! engine draws; it is not the whole boundary.
//!
//! Engines are compiled into `sig` and run in its process. One runtime serves every view of its
//! engine kind. Its calls run on the owner thread; work it does elsewhere wakes that thread
//! through an [`EngineWaker`]. Being in-process makes the contract cooperative: it keeps correct
//! engines correct, and is no sandbox against a compromised one.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::events::EngineEvent;
use crate::geometry::Metrics;
use crate::ids::{NavigationId, RequestId, ViewId};
use crate::input::InputEvent;
use crate::services::ServiceResult;

/// One engine kind's runtime, serving any number of views.
///
/// The executable binds [`Self::Target`] and [`Self::FramePermit`] to the Wayland host's target
/// lease and frame permit; the core only ever sees ids, commands and events.
///
/// The trait deliberately doesn't require `Send` or `Sync`: Servo and its rendering context
/// are bound to the thread that created them. Don't make an engine `Send` with an unsafe impl.
///
/// Returning `Ok` from [`open_view`](Self::open_view) or [`command`](Self::command) means the
/// request was accepted, not that it finished: results arrive as [`EngineEvent`]s. An
/// operation the engine doesn't support returns [`EngineError::Unsupported`]; it never
/// pretends to succeed.
///
/// # Event delivery
///
/// - Events reach the owner only through [`pump`](Self::pump), including those that
///   `open_view`, `command` or `close_view` queued. The owner therefore pumps a runtime in
///   every loop iteration in which it called it, as well as whenever its [`EngineWaker`] fired
///   or its [`PumpStatus`] deadline passed. Work on the engine's own threads wakes the waker.
/// - Events about one view arrive in the order they happened. The one exception is a
///   replaceable event (see [`pump`](Self::pump)), which supersedes an undelivered earlier one
///   of the same kind for the same view: the earlier is dropped, the later keeps its own place.
/// - A view accepts commands from the moment `open_view` returns `Ok`, queueing them itself if
///   it isn't ready; the owner never has to hold them back until
///   [`EngineEvent::ViewReady`].
pub trait EngineRuntime {
    /// Where a view presents: a host-managed target lease.
    type Target;
    /// Permission to present one frame for one view at one layout epoch. Consumed by
    /// [`present`](Self::present), and not `Clone`.
    type FramePermit;

    /// What this engine supports.
    fn capabilities(&self) -> EngineCapabilities;

    /// Starts opening `view`, which presents into `target`. [`EngineEvent::ViewReady`] follows,
    /// or, if opening fails later, [`EngineEvent::ViewFailed`] and [`EngineEvent::ViewClosed`].
    /// `Err` means the view was never opened: the engine has dropped `target`, and no events
    /// follow. Expensive setup happens off the owner thread where the engine allows it.
    ///
    /// The target's metrics may be empty (see [`Metrics`]); opening still succeeds, and the
    /// engine defers what needs a size, such as a GL context, until they are not.
    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: Self::Target,
    ) -> Result<(), EngineError>;

    /// Hands `command` to `view`.
    fn command(&mut self, view: ViewId, command: EngineCommand) -> Result<(), EngineError>;

    /// Does a bounded slice of the work that is due and appends what happened to `events`.
    /// Returns whether more work is pending, so the owner loop knows when to call again and
    /// when it may sleep: with work left over, the engine stops and returns
    /// `Pending { deadline: None }`, so the owner handles input before pumping again. The loop
    /// cannot preempt a pump, so a runtime keeps each one short.
    ///
    /// Replaceable events, which state the latest value of something, are coalesced: an engine
    /// keeps only the latest [`TitleChanged`](EngineEvent::TitleChanged),
    /// [`StatusTextChanged`](EngineEvent::StatusTextChanged),
    /// [`CursorChanged`](EngineEvent::CursorChanged),
    /// [`LoadingChanged`](EngineEvent::LoadingChanged),
    /// [`HistoryChanged`](EngineEvent::HistoryChanged),
    /// [`TextInputChanged`](EngineEvent::TextInputChanged) and
    /// [`FrameReady`](EngineEvent::FrameReady) per view until it is delivered, so a page
    /// changing its title in a loop adds one event, not millions. Every other event is ordered
    /// and delivered, none dropped.
    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus;

    /// Paints and submits `view`'s latest frame, if it has one for the permit's layout epoch.
    fn present(
        &mut self,
        view: ViewId,
        permit: Self::FramePermit,
    ) -> Result<PresentOutcome, EngineError>;

    /// Closes `view`: it stops presenting and accepting commands, drops its pending service
    /// requests, and releases its resources on the owner thread, graphics contexts before the
    /// target they draw to. [`EngineEvent::ViewClosed`] follows. Closing an unknown view does
    /// nothing.
    fn close_view(&mut self, view: ViewId);

    /// Shuts the runtime down, on the owner thread, before returning: closes every view still
    /// open (their graphics contexts first), releases every target lease, then shuts the engine
    /// itself down. This is the explicit order the host relies on; nothing native is left for
    /// `Drop`. The browser is exiting, so no events are reported for it. The executable calls
    /// it for every runtime before releasing the Wayland host.
    ///
    /// Afterwards [`open_view`](Self::open_view), [`command`](Self::command) and
    /// [`present`](Self::present) return [`EngineError::Runtime`], [`pump`](Self::pump)
    /// returns [`PumpStatus::Idle`], and calling `shutdown` again does nothing.
    fn shutdown(&mut self);
}

/// The optional operations an engine supports. The core uses these to decide what to do, the
/// chrome to explain what is unavailable. The default supports nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct EngineCapabilities {
    /// Back and forward within the engine's own session history.
    pub history_traversal: bool,
    /// Input-method composition (preedit and commit), beyond plain key events.
    pub text_input: bool,
    /// An accessibility tree for its content.
    pub accessibility: bool,
    /// Screenshots of a view's content.
    pub screenshots: bool,
    /// Find in page.
    pub find_in_page: bool,
    /// Page zoom ([`EngineCommand::SetZoom`]).
    pub page_zoom: bool,
    /// Pointer lock.
    pub pointer_lock: bool,
    /// Requests for native popups (select menus, context menus, dialogs) for the host to show.
    /// The API has no requests for these yet (deferred to M4), so engines report `false`.
    pub native_popups: bool,
}

/// How a view starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ViewOptions {
    /// Whether the view starts visible. A hidden view, such as a background tab, keeps its
    /// state but presents nothing until [`EngineCommand::SetVisibility`] shows it. It still has
    /// the page area's non-empty metrics: hiding is not an empty rectangle (see [`Metrics`]).
    pub visible: bool,
    /// Page zoom to start with, as a factor: 1.0 is 100 %. A tab keeps its zoom when it
    /// switches engines.
    pub zoom: f64,
}

impl Default for ViewOptions {
    fn default() -> Self {
        Self {
            visible: true,
            zoom: 1.0,
        }
    }
}

/// What the browser tells a view. Closing is [`EngineRuntime::close_view`].
#[derive(Clone, Debug, PartialEq)]
pub enum EngineCommand {
    /// Load `url`.
    Navigate {
        /// Identifies this navigation in the events it causes.
        navigation: NavigationId,
        /// An absolute URL the core has validated.
        url: String,
    },
    /// Reload the current document.
    Reload {
        /// Identifies the reload in the events it causes.
        navigation: NavigationId,
    },
    /// Stop loading.
    Stop,
    /// Move through the engine's own session history.
    Traverse {
        /// Identifies the traversal in the events it causes.
        navigation: NavigationId,
        /// Entries to move: -1 is back, 1 is forward.
        delta: i32,
    },
    /// The view's target has new metrics; render at them from now on.
    SetMetrics(Metrics),
    /// Show or hide the view. A hidden view presents nothing and keeps its metrics; what else
    /// it stops (timers, audio, network) is the engine's documented background policy.
    SetVisibility(bool),
    /// Give or take the view's logical focus: the address bar taking focus blurs the page.
    ///
    /// Losing focus releases every key the view received as pressed, modifiers included: the
    /// engine delivers the key-ups to its content itself and stops auto-repeat, exactly as for
    /// [`FocusEvent::Deactivated`](crate::input::FocusEvent::Deactivated). The real releases
    /// go to whoever has focus by then (Ctrl+L: the page got Ctrl down, the address bar gets
    /// Ctrl up). Pointer buttons are unaffected: they stay with the pointer's target until
    /// released.
    SetFocus(bool),
    /// Set page zoom, as a factor: 1.0 is 100 %. Separate from the display scale.
    SetZoom(f64),
    /// Input for the view, with positions surface-local to its target.
    Input(InputEvent),
    /// The answer to one of the view's service requests.
    ResolveServiceRequest {
        /// The request being answered.
        request: RequestId,
        /// The answer.
        result: ServiceResult,
    },
}

/// Whether a runtime has more work after [`EngineRuntime::pump`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpStatus {
    /// Nothing to do until the engine's waker fires.
    Idle,
    /// More work is pending.
    Pending {
        /// When to pump again; `None` means as soon as possible.
        deadline: Option<Instant>,
    },
}

/// What [`EngineRuntime::present`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PresentOutcome {
    /// A frame for the permit's layout epoch was submitted to the target. Not evidence that it
    /// has been displayed.
    Submitted,
    /// Nothing changed since the last submission, so nothing was submitted.
    Unchanged,
    /// The view has no frame for the permit's layout epoch yet (it is still rendering at a new
    /// size, say). Nothing was submitted; the host applies its resize fallback.
    NotReady,
}

/// Why an engine call failed.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum EngineError {
    /// No such view: never opened, or already closed.
    UnknownView(ViewId),
    /// The engine doesn't support this operation (see [`EngineCapabilities`]).
    Unsupported(&'static str),
    /// This view failed; the runtime's other views are unaffected.
    View {
        /// The failed view.
        view: ViewId,
        /// What happened, for the user.
        message: String,
    },
    /// The whole runtime failed.
    Runtime(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownView(view) => write!(f, "no such view: {view:?}"),
            Self::Unsupported(operation) => write!(f, "{operation} is not supported"),
            Self::View { view, message } => write!(f, "{view:?} failed: {message}"),
            Self::Runtime(message) => write!(f, "engine failed: {message}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Wakes the owner loop, from any thread, so it pumps the engine.
///
/// Wakes coalesce: only the first [`wake`](Self::wake) after the owner's
/// [`take`](Self::take) calls the notify function. The owner calls `take` before it pumps, so
/// a wake that arrives during the pump notifies again and is never lost.
#[derive(Clone)]
pub struct EngineWaker(Arc<WakerState>);

struct WakerState {
    pending: AtomicBool,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl EngineWaker {
    /// A waker that calls `notify` to get the owner loop's attention, such as pinging its
    /// event source.
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(WakerState {
            pending: AtomicBool::new(false),
            notify: Box::new(notify),
        }))
    }

    /// Asks for the engine to be pumped. Engines call this.
    pub fn wake(&self) {
        if !self.0.pending.swap(true, Ordering::AcqRel) {
            (self.0.notify)();
        }
    }

    /// Clears the request and returns whether there was one. The owner loop calls this right
    /// before pumping.
    pub fn take(&self) -> bool {
        self.0.pending.swap(false, Ordering::AcqRel)
    }
}

impl fmt::Debug for EngineWaker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineWaker")
            .field("pending", &self.0.pending.load(Ordering::Relaxed))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn counting_waker() -> (EngineWaker, Arc<AtomicUsize>) {
        let notified = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&notified);
        let waker = EngineWaker::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        });
        (waker, notified)
    }

    #[test]
    fn wakes_coalesce_until_taken() {
        let (waker, notified) = counting_waker();
        let engine_thread = waker.clone();
        waker.wake();
        engine_thread.wake();
        assert_eq!(notified.load(Ordering::SeqCst), 1);
        assert!(waker.take());
        assert!(!waker.take());
    }

    #[test]
    fn a_wake_during_the_pump_is_not_lost() {
        let (waker, notified) = counting_waker();
        waker.wake();
        assert!(waker.take()); // The owner is about to pump.
        waker.wake(); // The engine finds more work while being pumped.
        assert_eq!(notified.load(Ordering::SeqCst), 2);
        assert!(waker.take());
    }
}
