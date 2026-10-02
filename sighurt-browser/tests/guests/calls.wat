;; Calls the host in a loop in on_frame: cheap in fuel, expensive in time
;; (sighurt-engine-wasm's CALLS_THE_HOST_IN_A_LOOP).
(module
  (import "oxide" "api_time_now_ms" (func $now (result i64)))
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (loop $l (drop (call $now)) (br $l))))
