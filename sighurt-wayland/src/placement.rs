//! Applying a placement snapshot: where each target sits, as one layout transaction.
//!
//! Takes the chrome's layout (window-logical rectangles for chrome, page and overlays),
//! validates it, allocates a new `LayoutEpoch`, and applies it: child positions and stacking,
//! viewport destinations, input regions, and unmapping targets whose rectangle is empty (no
//! room in the layout; a hidden view keeps its non-empty metrics). Leaves normally run
//! desynchronized; a layout change switches the affected children to synchronized commits,
//! pauses submissions for the superseded epoch, informs presenters of the new metrics, and
//! commits the parent to reveal a coherent placement. A page too slow to repaint keeps its
//! last frame for a short, measured grace period and is then hidden behind the root fallback;
//! input stays consistent with what is shown.
//!
//! Must not own: computing the layout (the chrome does) or any z-order the chrome didn't ask for.
