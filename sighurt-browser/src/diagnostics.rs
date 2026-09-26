//! Logging setup and the measurements that show whether the architecture holds.
//!
//! Counts and times input reception, effect dispatch, engine pumps, frame-ready notifications,
//! paints, submissions and presentation feedback, kept distinct (submission is not display);
//! reports each engine's version, graphics driver and presentation path (including any readback
//! fallback), logical and buffer sizes, scale, layout epoch, buffer counts and bytes, queue
//! depths and the last error. Never logs clipboard contents, credentials or page data.
//!
//! Must not own: decisions based on these numbers.
