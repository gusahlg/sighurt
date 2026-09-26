//! `sighurt_api::input` events into Servo's input events.
//!
//! Converts surface-local positions to Servo's device pixels once, with the view's `Metrics`;
//! maps physical key (`KeyCode::w3c_code` and `location`, parsed into Servo's `Code`), key
//! value, modifiers and text to Servo's keyboard events; releases held keys on blur; delivers
//! input method composition through Servo's IME events without also sending the text as keys;
//! maps smooth and wheel scrolling.
//!
//! Must not own: shortcut handling (the core consumes browser shortcuts before the page).
