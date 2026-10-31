# Sighurt's patch of servo-script 0.7.0

This directory is the crates.io source of servo-script 0.7.0 (the version in `Cargo.lock`),
with its MPL-2.0 notices. The workspace uses it through `[patch.crates-io]`.

The patch is in `dom/webgl/webglbuffer.rs` only. Loading github.com in Servo panicked the
script thread there (`assertion failed: self.is_deleted()`, then `refcount underflowed`, then
a panic across the mozjs FFI boundary, which aborts the process). The buffer's attached
counter (VAO attribute and element-array references) can get out of balance on some WebGL
call sequences, and the code asserted instead of tolerating it.

- `DroppableWebGLBuffer::delete` returns when the buffer is not deleted instead of asserting.
- `increment_attached_counter` and `decrement_attached_counter` saturate instead of panicking.

A page's WebGL bookkeeping error then costs at most a leaked or early-deleted GL buffer, never
the browser. Changing this crate rebuilds servo-script and everything above it.
