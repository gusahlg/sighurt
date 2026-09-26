//! Presentation targets: the host-managed destinations chrome and engine views present into.
//!
//! A target (`TargetId`) is a subsurface the host created, with its role, parent and stacking.
//! A `TargetLease` keeps its native backing alive and gives a trusted presenter the target id,
//! the current `Metrics`, the presentation mode chosen at attachment (SHM, or a native GL
//! path), and, for trusted GL code, the raw display and window handles through one controlled
//! accessor whose unsafe lifetime guarantees are documented here and nowhere else. A
//! `FramePermit` allows one presentation for one view at one layout epoch: it is consumed by
//! use and not `Clone`, and a result for a closed view or superseded epoch is discarded rather
//! than shown as current.
//!
//! Retirement follows the documented close order: the presenter destroys its graphics context
//! before the surface, and the surface outlives every legitimate lease. A presenter that commits
//! a leaf itself (a GL swap does) owns that leaf's buffer submission; the host never commits
//! buffer state on the same leaf.
//!
//! Must not own: what is drawn, or moving live views between windows.
