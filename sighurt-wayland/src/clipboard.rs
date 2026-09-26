//! Clipboard and primary selection: the data-device objects, offers and serials.
//!
//! Performs only clipboard operations the core has allowed, keeping the ordinary clipboard and
//! the primary selection apart. Transfers run asynchronously through pipes with size limits and
//! cancellation, so a stalled source never freezes the browser. Drag-and-drop is a related but
//! separate capability, and a file drop grants no other filesystem access.
//!
//! Must not own: the decision whether content may read or write the clipboard.
