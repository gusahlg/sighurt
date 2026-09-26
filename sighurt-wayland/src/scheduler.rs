//! When each target presents.
//!
//! Keeps three signals apart, in code and diagnostics: invalidation (a presenter or engine has
//! something new, `FrameReady`), the frame callback (an opportunity to present again), and
//! buffer release or graphics synchronization (storage may be reused, see [`crate::shm`]). A
//! target presents when it is shown, has pending work and its presenter can accept it; while
//! throttled only the latest invalidation is kept, never a queue of obsolete frames.
//!
//! Frame callbacks pace presentation: after a submission, the next permit waits for the
//! callback of that commit. Frames that no callback can pace are bootstrapped instead: the first
//! after mapping, and the first for a new layout epoch, whose target is a synchronized
//! subsurface whose last commit (and so its callback) the compositor holds until the root
//! commits, which in turn waits for this frame. Unmapping a target forgets its outstanding
//! callback, so a target the host hid is never waited on. A callback the compositor withholds
//! because the whole window is invisible (another workspace) is waited for: nobody can see a new
//! frame, and the compositor sends the callback when the window shows again.
//!
//! On an SHM target the host requests the callback in the commit it makes. On a GL target it
//! requests it when issuing the permit, as pending surface state that the presenter's swap
//! commits; at most one such request is pending at a time.
//!
//! There is no fixed tick: an idle browser sleeps, the chrome never repaints because a page
//! animated, and a page is never pumped because the caret blinked. Submissions and
//! presentation feedback (`wp_presentation`) are recorded separately; a submission is not
//! evidence that anything was displayed.
//!
//! Must not own: rendering, or engine pumping (the application loop does).

use std::time::Duration;

use sighurt_api::engine::PresentOutcome;
use sighurt_api::ids::TargetId;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::presentation_time::client::wp_presentation::{self, WpPresentation};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};

use crate::connection::Host;

/// The pacing state of one target.
#[derive(Debug, Default)]
pub(crate) struct FrameClock {
    invalidated: bool,
    permit_out: bool,
    callback: Callback,
    seq: u64,
    bootstrap: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Callback {
    #[default]
    None,
    /// Requested as pending surface state that no commit has carried yet (GL targets).
    Requested { seq: u64 },
    /// Committed; the next permit waits for it.
    Pending { seq: u64 },
}

impl FrameClock {
    /// A new target: its first frame is bootstrapped.
    pub fn new() -> Self {
        Self {
            bootstrap: true,
            ..Self::default()
        }
    }

    /// There is something new to present. Repeated invalidations coalesce.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
    }

    /// The target got a new layout epoch while shown: it needs a frame for it, and that one frame
    /// presents without waiting for the callback of the last commit, which a synchronized
    /// subsurface holds back until the layout's root commit.
    pub fn relayout(&mut self) {
        self.invalidated = true;
        self.bootstrap = true;
    }

    /// Whether a permit may be issued now, for a target that is `shown`.
    pub fn can_present(&self, shown: bool) -> bool {
        shown
            && self.invalidated
            && !self.permit_out
            && (self.bootstrap || !matches!(self.callback, Callback::Pending { .. }))
    }

    /// A permit was issued: the invalidation is taken.
    pub fn issue(&mut self) {
        self.permit_out = true;
        self.invalidated = false;
        self.bootstrap = false;
    }

    /// A frame callback is about to be requested; returns its sequence number, or `None` when a
    /// request that no commit carried yet is still pending (a second one would only duplicate
    /// it).
    pub fn request_callback(&mut self) -> Option<u64> {
        if let Callback::Requested { .. } = self.callback {
            return None;
        }
        self.seq += 1;
        self.callback = Callback::Requested { seq: self.seq };
        Some(self.seq)
    }

    /// The permit came back. A submission committed the requested callback.
    pub fn finished(&mut self, outcome: PresentOutcome) {
        self.permit_out = false;
        if let (PresentOutcome::Submitted, Callback::Requested { seq }) = (outcome, self.callback) {
            self.callback = Callback::Pending { seq };
        }
    }

    /// Frame callback `seq` fired: an opportunity to present again. An older one, from before
    /// the target was unmapped, changes nothing.
    pub fn frame_done(&mut self, seq: u64) {
        if self.callback == (Callback::Pending { seq }) {
            self.callback = Callback::None;
        }
    }

    /// The host unmapped the target: its outstanding callback may never come, so forget it and
    /// bootstrap the next frame.
    pub fn unmapped(&mut self) {
        self.callback = Callback::None;
        self.bootstrap = true;
    }

    /// Whether an invalidation is waiting.
    pub fn is_invalidated(&self) -> bool {
        self.invalidated
    }

    /// Whether the next frame waits for a frame callback.
    pub fn awaits_callback(&self) -> bool {
        matches!(self.callback, Callback::Pending { .. })
    }
}

/// Presentation counters of one target, for diagnostics. Each signal is counted apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PresentStats {
    /// Permits issued.
    pub permits: u64,
    /// Frames submitted (an SHM commit, or a GL swap the presenter reported).
    pub submissions: u64,
    /// Frame callbacks received.
    pub frame_callbacks: u64,
    /// SHM buffers the compositor released.
    pub releases: u64,
    /// Frames the compositor reported shown (`wp_presentation_feedback.presented`).
    pub presented: u64,
    /// Frames it reported never shown (`discarded`).
    pub discarded: u64,
    /// Layouts whose grace period ended without this target's frame, so it was unmapped until
    /// its frame came (the slow-resize fallback).
    pub late: u64,
    /// From submission to display, for the last presented frame, when the compositor's
    /// presentation clock is `CLOCK_MONOTONIC`.
    pub last_latency: Option<Duration>,
}

/// A frame callback's target and sequence number.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FrameData {
    pub target: TargetId,
    pub seq: u64,
}

impl Dispatch<WlCallback, FrameData> for Host {
    fn event(
        host: &mut Host,
        _: &WlCallback,
        event: wl_callback::Event,
        data: &FrameData,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            if let Some(target) = host.targets.get(&data.target) {
                let mut target = target.borrow_mut();
                target.stats.frame_callbacks += 1;
                target.clock.frame_done(data.seq);
            }
        }
    }
}

/// A presentation feedback's target and when its frame was submitted, in nanoseconds of
/// `CLOCK_MONOTONIC`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FeedbackData {
    pub target: TargetId,
    pub submitted_ns: u128,
}

/// Now on `CLOCK_MONOTONIC`, in nanoseconds: the clock `wp_presentation` usually reports in.
pub(crate) fn monotonic_ns() -> u128 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    now.tv_sec as u128 * 1_000_000_000 + now.tv_nsec as u128
}

/// `CLOCK_MONOTONIC`'s id in `wp_presentation.clock_id`.
const CLOCK_MONOTONIC: u32 = 1;

impl Dispatch<WpPresentation, ()> for Host {
    fn event(
        host: &mut Host,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            host.presentation_clock = Some(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, FeedbackData> for Host {
    fn event(
        host: &mut Host,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        data: &FeedbackData,
        _: &Connection,
        _: &QueueHandle<Host>,
    ) {
        let Some(target) = host.targets.get(&data.target) else {
            return;
        };
        let mut target = target.borrow_mut();
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                ..
            } => {
                target.stats.presented += 1;
                if host.presentation_clock == Some(CLOCK_MONOTONIC) {
                    let secs = (u128::from(tv_sec_hi) << 32) | u128::from(tv_sec_lo);
                    let shown = secs * 1_000_000_000 + u128::from(tv_nsec);
                    let latency = shown.saturating_sub(data.submitted_ns);
                    target.stats.last_latency =
                        Some(Duration::from_nanos(latency.min(u64::MAX as u128) as u64));
                }
            }
            wp_presentation_feedback::Event::Discarded => target.stats.discarded += 1,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A permit issued, answered with `outcome`.
    fn present(clock: &mut FrameClock, outcome: PresentOutcome) {
        assert!(clock.can_present(true));
        clock.issue();
        clock.finished(outcome);
    }

    #[test]
    fn invalidations_coalesce_into_one_frame() {
        let mut clock = FrameClock::new();
        for _ in 0..5 {
            clock.invalidate();
        }
        assert!(clock.can_present(true));
        clock.issue();
        assert!(!clock.can_present(true), "one permit at a time");
        clock.finished(PresentOutcome::Unchanged);
        assert!(
            !clock.can_present(true),
            "the five invalidations were one frame"
        );
    }

    #[test]
    fn frames_wait_for_the_callback_of_the_last_commit() {
        let mut clock = FrameClock::new();
        clock.invalidate();
        clock.issue();
        let seq = clock.request_callback().unwrap();
        clock.finished(PresentOutcome::Submitted);
        clock.invalidate();
        assert!(!clock.can_present(true), "throttled until the callback");
        assert!(clock.awaits_callback());
        clock.frame_done(seq);
        assert!(clock.can_present(true));
    }

    #[test]
    fn an_idle_target_never_presents() {
        let mut clock = FrameClock::new();
        assert!(
            !clock.can_present(true),
            "no invalidation, no frame, even at bootstrap"
        );
        clock.invalidate();
        assert!(!clock.can_present(false), "a hidden target never presents");
    }

    #[test]
    fn unmapping_forgets_a_callback_that_will_never_come() {
        let mut clock = FrameClock::new();
        clock.invalidate();
        clock.issue();
        let stale = clock.request_callback().unwrap();
        clock.finished(PresentOutcome::Submitted);
        clock.unmapped();
        clock.invalidate();
        assert!(
            clock.can_present(true),
            "the frame after remapping is bootstrapped"
        );
        clock.issue();
        let fresh = clock.request_callback().unwrap();
        clock.finished(PresentOutcome::Submitted);
        // The old callback arriving late doesn't count for the new commit.
        clock.frame_done(stale);
        clock.invalidate();
        assert!(!clock.can_present(true));
        clock.frame_done(fresh);
        assert!(clock.can_present(true));
    }

    #[test]
    fn a_new_layout_presents_without_the_callback_of_a_cached_commit() {
        let mut clock = FrameClock::new();
        clock.invalidate();
        clock.issue();
        let cached = clock.request_callback().unwrap();
        clock.finished(PresentOutcome::Submitted);
        // A second placement in the same transaction: the commit above waits in the
        // compositor's cache for a root commit, which waits for this target's next frame.
        clock.relayout();
        assert!(
            clock.can_present(true),
            "the new epoch's frame is bootstrapped"
        );
        clock.issue();
        let fresh = clock.request_callback().unwrap();
        clock.finished(PresentOutcome::Submitted);
        // One bootstrapped frame per epoch; the ones after it are paced again.
        clock.invalidate();
        assert!(!clock.can_present(true));
        clock.frame_done(cached);
        assert!(
            !clock.can_present(true),
            "the older commit's callback doesn't count"
        );
        clock.frame_done(fresh);
        assert!(clock.can_present(true));
    }

    #[test]
    fn a_new_layout_waits_for_a_permit_that_is_out() {
        let mut clock = FrameClock::new();
        clock.invalidate();
        clock.issue();
        clock.relayout();
        assert!(!clock.can_present(true), "one permit at a time");
        // The presenter gives the stale permit back unused; the new epoch's frame follows.
        clock.finished(PresentOutcome::NotReady);
        assert!(clock.can_present(true));
    }

    #[test]
    fn not_ready_does_not_spin() {
        let mut clock = FrameClock::new();
        clock.invalidate();
        present(&mut clock, PresentOutcome::NotReady);
        assert!(
            !clock.can_present(true),
            "waits for the engine's next FrameReady"
        );
        clock.invalidate();
        assert!(clock.can_present(true));
    }

    #[test]
    fn a_gl_callback_request_is_not_duplicated_before_a_swap_commits_it() {
        let mut clock = FrameClock::new();
        clock.invalidate();
        clock.issue();
        let first = clock.request_callback().unwrap();
        clock.finished(PresentOutcome::NotReady);
        clock.invalidate();
        clock.issue();
        assert_eq!(
            clock.request_callback(),
            None,
            "the first request is still pending"
        );
        clock.finished(PresentOutcome::Submitted);
        clock.invalidate();
        assert!(!clock.can_present(true));
        clock.frame_done(first);
        assert!(clock.can_present(true));
    }

    #[test]
    fn frame_callbacks_alone_never_cause_frames() {
        let mut clock = FrameClock::new();
        clock.invalidate();
        clock.issue();
        let seq = clock.request_callback().unwrap();
        clock.finished(PresentOutcome::Submitted);
        clock.frame_done(seq);
        assert!(
            !clock.can_present(true),
            "a callback is an opportunity, not work"
        );
    }
}
