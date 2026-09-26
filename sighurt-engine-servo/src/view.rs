//! One Servo view: its WebView, target lease and rendering context, with its visibility,
//! focus, zoom and current metrics.
//!
//! A hidden view keeps its WebView but presents nothing. Closing it releases the rendering
//! context before the target lease, on the owner thread.
//!
//! Must not own: the tab it belongs to (the core).
