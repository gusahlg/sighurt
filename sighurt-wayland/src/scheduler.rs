//! When each target presents.
//!
//! Keeps three signals apart, in code and diagnostics: invalidation (a presenter or engine has
//! something new, `FrameReady`), the frame callback (an opportunity to present again), and
//! buffer release or graphics synchronization (storage may be reused). A target presents when
//! it is visible, has pending work and its presenter can accept it; while throttled only the
//! latest invalidation is kept, never a queue of obsolete frames. The first frame after mapping
//! is bootstrapped without waiting for a callback that cannot come, and a hidden surface's
//! withheld callback is never waited on. There is no fixed tick: an idle browser sleeps, the
//! chrome never repaints because a page animated, and a page is never pumped because the caret
//! blinked. Records submission timing and presentation feedback where available, as separate
//! measurements.
//!
//! Must not own: rendering, or engine pumping (the application loop does).
