//! Presenting Servo's frames into the view's Wayland subsurface.
//!
//! Builds Servo's `WindowRenderingContext` (surfman) from the target lease's raw display and
//! window handles, makes it current, and on a frame permit paints and presents: the GL swap
//! commits the leaf itself, so the host never commits buffers on it. Resizes it for new
//! metrics, hides and shows it, and destroys it before the surface on close. There is no
//! `read_pixels` on the normal path; a readback fallback exists only as a development mode that
//! diagnostics report as such, with the GL renderer in use.
//!
//! Must not own: the surface or its placement (the host).
