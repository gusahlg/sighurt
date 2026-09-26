//! Windows and tabs: the tab list of each window, its order, the active tab, and the engine
//! view attached to each tab.
//!
//! A tab is identified by its `TabId`, never by its position, and keeps it across navigations
//! and engine switches. Each attached view has a lifecycle state (opening, live and visible,
//! live and hidden, closing, closed, failed) kept separate from its navigation state. Only the
//! active tab's view is visible. Switching a tab to another engine opens the new view first and
//! switches visible ownership when it is ready. Events from a view that is no longer attached to
//! its tab are dropped, so a late title or permission request from an old view can never touch
//! the new document.
//!
//! Page-supplied data is bounded before it is kept: titles and status text are cut to 16 KiB,
//! and a URL longer than 2 MiB is dropped, never shortened (a shortened URL points elsewhere).
//! A page's `OpenRequested` goes through the router like any URL; a page never opens `sig://`.
//! A page may open a new tab only within 5 s of a click or key press the core routed to its
//! view (user activation), and one activation opens one tab, so a page calling `window.open` in
//! a loop or from the background opens nothing.
//!
//! Must not own: how many surfaces realize a view, or anything about its rendering.
