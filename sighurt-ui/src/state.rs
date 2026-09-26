//! `ChromeState`: the chrome's temporary interaction state, and the browser snapshot it reads.
//!
//! Holds what only the chrome needs: which widget has focus and hover, what is pressed, the
//! address and command editors' text, selection and scroll, and open overlays. The browser
//! snapshot is read-only input from the core.
//!
//! Must not own: the committed URL, the tab list or anything else the core owns. Editing the
//! address never changes the committed address; submitting it issues a command.
