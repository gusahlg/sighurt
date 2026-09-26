//! Chrome layout: the tab strip, the address/command line and the page rectangle, for the
//! current window size and scale.
//!
//! Explicit code, not a layout language. Produces window-logical rectangles for the chrome
//! surface, the page target and overlays (the placement snapshot the host validates and
//! applies), plus the regions and hit targets inside the chrome. A region with no room gets an
//! empty rectangle, which the host unmaps; that is not how background tabs are hidden (they
//! keep the page rectangle and are hidden through visibility).
//!
//! Must not own: applying placement to surfaces (the host).
