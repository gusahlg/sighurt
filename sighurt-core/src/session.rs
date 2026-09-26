//! The session model: what is saved and restored of windows, tabs, their order and their
//! history segments, as plain data.
//!
//! Restoring recreates tabs with new ids and reloads their saved URLs, stating that document
//! state was lost; it never claims exact restoration.
//!
//! Must not own: writing snapshots to disk, which the executable does atomically.
