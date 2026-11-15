# Sighurt's patch of servo-script 0.7.0

This directory is the crates.io source of servo-script 0.7.0 (the version in `Cargo.lock`),
with its MPL-2.0 notices. The workspace uses it through `[patch.crates-io]`.

The patch has two parts.

## WebGL buffer bookkeeping (`dom/webgl/webglbuffer.rs`)

Loading github.com in Servo panicked the
script thread there (`assertion failed: self.is_deleted()`, then `refcount underflowed`, then
a panic across the mozjs FFI boundary, which aborts the process). The buffer's attached
counter (VAO attribute and element-array references) can get out of balance on some WebGL
call sequences, and the code asserted instead of tolerating it.

- `DroppableWebGLBuffer::delete` returns when the buffer is not deleted instead of asserting.
- `increment_attached_counter` and `decrement_attached_counter` saturate instead of panicking.

A page's WebGL bookkeeping error then costs at most a leaked or early-deleted GL buffer, never
the browser.

## Input-driven rendering updates (`event_loop/script_thread.rs`)

The script thread processes pending input events (wheel, keys, pointer) only inside "update the
rendering", and `maybe_schedule_rendering_opportunity_after_ipc_message` schedules that at most
every 20 ms (upstream's comment asks whether the delay should be reduced). Every input-driven
frame (scrolling, typing) was capped at 50 frames/s whatever the display's rate: continuous
wheel scrolling ran at 49.6 frames/s with a 20.1 ms median frame interval in a 60 Hz headless
sway (research note `window_frontend_and_engines_latency.md`, 6 Nov 2026).

- While a fully active document has pending input, the update is scheduled
  `INPUT_RENDERING_DELAY` (4 ms) after the last one, or at once if that has passed, and a timer
  already scheduled later (the 20 ms one) is cancelled first.
- Without pending input the 20 ms and 30 ms delays are upstream's.

Servo already coalesces pending mouse moves and wheel events, so a pointer flood makes at most
one update per 4 ms. The host paces painting by frame callbacks, so faster updates show up only
as far as the display allows.

Changing this crate rebuilds servo-script and everything above it.
