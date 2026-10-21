;; Takes 40 ms of every frame, then draws a colour that changes each frame
;; (sighurt-engine-wasm's BUSY, with a clear so it presents).
(module
  (import "oxide" "api_time_now_ms" (func $now (result i64)))
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (global $frames (mut i32) (i32.const 0))
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (local $end i64)
    (local.set $end (i64.add (call $now) (i64.const 40)))
    (loop $l (br_if $l (i64.lt_u (call $now) (local.get $end))))
    (global.set $frames (i32.add (global.get $frames) (i32.const 1)))
    (call $clear (i32.and (global.get $frames) (i32.const 255)) (i32.const 64) (i32.const 96) (i32.const 255))))
