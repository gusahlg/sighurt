//! Seats, pointer and keyboard: raw Wayland input into `sighurt_api::input` events.
//!
//! Tracks each seat separately. Keyboard: xkbcommon keymaps and compose, the physical key, the
//! layout's key value, modifiers (AltGr separate from Ctrl and Alt) and the modifiers the layout
//! consumed, client-side key repeat, and committed text, so Swedish characters, dead keys and
//! compose sequences survive. Subsurfaces never get keyboard focus, so keyboard input for the
//! window is routed to the logical focus target the browser chose; losing window activation
//! releases held keys, repeat and captures. Pointer: positions stay surface-local to the target
//! under the pointer; a pressed button keeps its target until release or cancellation, even
//! across surface boundaries. Pointer motion, buttons and scrolling carry the seat's current
//! modifiers. Scroll keeps smooth pixels and wheel 120ths apart. Also sets the cursor, by shape
//! where supported, else from the cursor theme.
//!
//! Must not own: shortcut policy (the core decides) or input-method text (see `text_input`).
