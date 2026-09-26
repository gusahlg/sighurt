//! The browser chrome: a tab strip and an address/command line, drawn on the CPU.
//!
//! Four concepts, kept apart: a read-only snapshot of the browser state the chrome displays
//! (tabs, titles, committed URLs, loading, what engines can do); `ChromeState`, the chrome's
//! own interaction and editing state; a layout result with regions and hit targets; and a
//! painter that turns those into pixels for the current target metrics. The chrome emits
//! typed browser commands and its placement snapshot (chrome, page and overlay rectangles);
//! the host applies the placement.
//!
//! Must not own: tabs or navigation state (the core), Wayland objects (it holds no surface and
//! paints into memory the host lends it), web layout, or guest permissions. Page titles and
//! hovered URLs are untrusted text, drawn contained. Changing the painter never changes browser
//! state types.

pub mod accessibility;
pub mod chrome;
pub mod hit_test;
pub mod layout;
pub mod paint_cpu;
pub mod state;
pub mod text_edit;
