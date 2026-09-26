//! The chrome's behavior: input and snapshot changes in, `ChromeState` changes, browser
//! commands and redraw requests out.
//!
//! Redraws only after an edit, a hover change, a tab metadata change, a configuration change, a
//! caret deadline or a layout change: a static chrome needs no tick, and a page animating never
//! repaints it. The caret timer runs only while an editor has focus.
//!
//! Must not own: key binding policy (the core resolves bindings before the chrome sees a key).
