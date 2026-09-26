//! Servo's `ServoDelegate` and `WebViewDelegate` callbacks, turned into queued
//! `sighurt_api::events::EngineEvent`s.
//!
//! Callbacks only queue events; the runtime hands them over after the Servo call returns, so
//! nothing mutates browser state re-entrantly. Translates title, load status, status text
//! (hovered links), cursor, history (Servo's entry list) and URL changes, crashes
//! (`ViewFailed`), backend loss (`RuntimeFailed`), requests to open URLs (`OpenRequested`: new
//! views, and navigations to URLs Servo shouldn't load, which it denies), and permission,
//! clipboard, file and download requests (`ServiceRequested`, completed later from the
//! browser's answer). Answers requests Servo needs and the browser can't give yet as refused,
//! never as granted.
//!
//! Not in the API yet (M4 adds them if needed): `<select>` menus, colour pickers, `alert`,
//! `confirm` and `prompt` (`show_embedder_control`) and HTTP authentication. Until then they
//! go unanswered and Servo resolves them on drop: a `<select>` keeps its selection, `alert` is
//! acknowledged, `confirm` and `prompt` are cancelled, and no credentials are sent.
//!
//! Must not own: policy decisions, and never exits the process.
