//! The root surface of a window: its `xdg_toplevel` role and configure handshake.
//!
//! Collects the latest configure, acknowledges it as xdg-shell requires, sets the window
//! geometry deliberately, and starts a layout transaction for the new size. The root is a stable
//! anchor that stays mapped while any child is visible, and shows an opaque fallback: a solid
//! buffer redrawn only on resize, or a tiny buffer scaled with `wp_viewporter` when available.
//! Close requests go to the browser as events. Decoration negotiation is optional; without it
//! the window is deliberately borderless.
//!
//! Must not own: chrome content, or a repaint when a page animates.
