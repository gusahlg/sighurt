//! Input methods through `zwp_text_input_v3`.
//!
//! Enables text input while an editable element has logical focus, forwards its surrounding
//! text and caret rectangle (translated from the reporting target's coordinates into the
//! surface the protocol uses) and its content type (a password field always with the
//! `sensitive_data` and `hidden_text` hints), and turns preedit, commit and delete-surrounding
//! into one `sighurt_api::input::TextInputEvent` per `done`. Text an input method commits is
//! never also delivered as key text. When the protocol is missing, keyboard and compose input still work
//! and the missing IME support is reported.
//!
//! Must not own: the editors themselves (chrome and engines own them).
