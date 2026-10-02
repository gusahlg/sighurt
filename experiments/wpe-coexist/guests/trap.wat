;; A guest that clears its canvas green on start and traps (`unreachable`) in its first frame:
;; the Wasm engine must fail this view and nothing else.
(module
  (import "oxide" "api_canvas_clear" (func $clear (param i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (func (export "start_app")
    (call $clear (i32.const 0) (i32.const 160) (i32.const 0) (i32.const 255)))
  (func (export "on_frame") (param i32) unreachable))
