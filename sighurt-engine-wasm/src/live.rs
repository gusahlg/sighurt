//! A live guest between its steps: the guest itself while it waits for the next step, the input
//! for that step, and when it is due.
//!
//! The guest keeps its cadence on the worker side: a step job takes the guest, runs the step,
//! puts it back and queues the next step itself (see the adapter), about sixty a second while
//! the view is visible and one a second while it is hidden. The owner thread hands over input,
//! changes the visibility and ends the guest, and hears of a step only when the step changed
//! something the browser shows: a new frame, the history, the permission prompt, a navigation
//! request or a failure. A guest that draws the same frame every frame wakes nobody.
//!
//! Owns: a live guest's scheduling state. Must not own: running steps (the adapter) or page
//! logic (the view).

use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use sighurt_api::ids::{DocumentGeneration, ViewId};

use crate::permissions::PermissionKind;
use crate::runtime::{FrameInput, Guest};

/// Steps run at about 60 Hz while a view is visible, and once a second while it is hidden.
const FRAME: Duration = Duration::from_micros(16_667);
const HIDDEN_FRAME: Duration = Duration::from_secs(1);
/// Longest frame time a guest is told of: after a long pause, it is told of this.
const MAX_FRAME_TIME: Duration = Duration::from_millis(100);

pub struct LiveGuest {
    pub view: ViewId,
    pub document: DocumentGeneration,
    state: Mutex<State>,
}

struct State {
    /// `None` while a step runs, and once the guest ended.
    guest: Option<Box<Guest>>,
    input: FrameInput,
    visible: bool,
    /// When the next step is due, and the serial of the step job queued for it; a job with
    /// another serial was superseded and does nothing.
    due: Instant,
    serial: u64,
    /// When the last step began.
    last: Instant,
    ended: bool,
    /// What the owner last heard of.
    told: Seen,
}

/// What of a guest's state the owner shows the browser, as a step left it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Seen {
    /// The version of the document's history.
    pub history: u64,
    /// The permission request waiting for the user.
    pub prompt: Option<(String, PermissionKind)>,
}

impl Seen {
    pub fn of(guest: &Guest) -> Self {
        let state = guest.host_state();
        let history = lock(&state.navigation).version();
        let prompt = lock(&state.permissions).pending.clone();
        Self {
            history,
            prompt: prompt.map(|request| (request.origin, request.kind)),
        }
    }
}

/// A step to run, taken from a [`LiveGuest`].
pub struct Step {
    pub guest: Box<Guest>,
    pub input: FrameInput,
    pub dt_ms: u32,
}

/// What a finished step leads to.
pub struct After {
    /// The owner must hear of the step.
    pub notify: bool,
    /// When the next step is due, and its serial; `None` once the guest ended.
    pub next: Option<(Instant, u64)>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl LiveGuest {
    /// A guest whose first step is due now.
    pub fn new(
        view: ViewId,
        document: DocumentGeneration,
        guest: Box<Guest>,
        visible: bool,
    ) -> Self {
        let now = Instant::now();
        let told = Seen::of(&guest);
        Self {
            view,
            document,
            state: Mutex::new(State {
                guest: Some(guest),
                input: FrameInput::default(),
                visible,
                due: now,
                serial: 0,
                last: now,
                ended: false,
                told,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// When the next step is due, and its serial.
    pub fn due(&self) -> (Instant, u64) {
        let state = self.lock();
        (state.due, state.serial)
    }

    /// Adds to the input of the next step.
    pub fn input(&self, add: impl FnOnce(&mut FrameInput)) {
        let mut state = self.lock();
        if !state.ended {
            add(&mut state.input);
        }
    }

    /// Steps at the visible or the hidden pace from now on. A guest waiting for its next step
    /// that becomes visible is due at once: returns when, and the new serial, for the owner to
    /// queue that step.
    pub fn set_visible(&self, visible: bool) -> Option<(Instant, u64)> {
        let mut state = self.lock();
        let shown = visible && !state.visible;
        state.visible = visible;
        if !shown || state.ended || state.guest.is_none() {
            return None;
        }
        state.due = Instant::now();
        state.serial += 1;
        Some((state.due, state.serial))
    }

    /// Ends the guest: no step runs any more. Returns the guest if it was waiting, for the
    /// caller to drop off the owner thread; a guest in a step is dropped where it runs.
    pub fn end(&self) -> Option<Box<Guest>> {
        let mut state = self.lock();
        state.ended = true;
        state.guest.take()
    }

    /// Takes the guest for the step with `serial`, unless that step was superseded or the guest
    /// ended.
    pub fn begin(&self, serial: u64) -> Option<Step> {
        let mut state = self.lock();
        if state.ended || state.serial != serial {
            return None;
        }
        let guest = state.guest.take()?;
        let now = Instant::now();
        let dt = (now - state.last).min(MAX_FRAME_TIME);
        state.last = now;
        Some(Step {
            guest,
            input: std::mem::take(&mut state.input),
            dt_ms: dt.as_millis() as u32,
        })
    }

    /// A step finished: puts `guest` back, or ends the guest when the step failed (`None`).
    /// `changed` says whether it drew a new frame, asked for a navigation or failed; `seen` is
    /// what the guest's state shows now.
    pub fn finish(&self, guest: Option<Box<Guest>>, seen: Seen, changed: bool) -> After {
        let mut state = self.lock();
        if state.ended {
            drop(state);
            // Dropped here, on the worker.
            drop(guest);
            return After {
                notify: false,
                next: None,
            };
        }
        let notify = changed || seen != state.told;
        state.told = seen;
        let Some(guest) = guest else {
            state.ended = true;
            return After { notify, next: None };
        };
        state.guest = Some(guest);
        // Keep the cadence, unless the guest fell behind it.
        let now = Instant::now();
        let period = if state.visible { FRAME } else { HIDDEN_FRAME };
        state.due += period;
        if state.due < now {
            state.due = now + period;
        }
        state.serial += 1;
        After {
            notify,
            next: Some((state.due, state.serial)),
        }
    }
}
