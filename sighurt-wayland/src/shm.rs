//! Shared-memory buffers for CPU presenters: the chrome and the root fallback.
//!
//! Two buffers per surface, a third only when justified. Each buffer is free, being drawn, or
//! submitted; submitted storage is reused only after the compositor releases it, and with no
//! free buffer the invalidation waits instead of overwriting memory in use. A resize retires the
//! pool generation while keeping in-use storage alive, with the number and bytes of retiring
//! generations bounded. Every submission repaints the whole small buffer until a sound damage
//! history exists. The pixel format is explicit: Wayland's little-endian ARGB8888 is not
//! tiny-skia's premultiplied RGBA, so the conversion is deliberate and tested.
//!
//! Must not own: page pixels. A page-sized CPU copy per frame is exactly what the rewrite
//! removes.
