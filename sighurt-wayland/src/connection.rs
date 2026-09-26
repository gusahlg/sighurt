//! The Wayland connection: connecting, binding globals, and dispatching events.
//!
//! Binds only the protocol versions the host knows how to use. `wl_compositor`,
//! `wl_subcompositor`, `wl_shm` and xdg-shell are required, and their absence is a clear
//! startup error; optional globals (viewporter, fractional scale, text input, cursor shape,
//! presentation time, decorations) are reported as capabilities. The event queue joins the
//! application's calloop loop through a supported adapter, with the prepare-read, dispatch,
//! flush cycle; there are no hand-written polling loops or blocking roundtrips. Uses the
//! system libwayland backend, which raw-window-handle interop requires. A protocol error
//! invalidates the whole connection and is reported as a platform failure, not a page error.
//!
//! Must not own: what the window shows.
