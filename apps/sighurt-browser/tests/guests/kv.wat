;; Keeps a key in the key-value store, then says "#stored" with push_state, or "#failed" if the
;; store refused it: engines.sh's check of where `[engines.wasm] storage` keeps the store.
(module
  (import "oxide" "api_kv_store_set" (func $kv (param i32 i32 i32 i32) (result i32)))
  (import "oxide" "api_push_state" (func $push (param i32 i32 i32 i32 i32 i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "#stored")
  (data (i32.const 16) "#failed")
  (data (i32.const 32) "key")
  (data (i32.const 48) "value")
  (func (export "start_app")
    (if (i32.eqz (call $kv (i32.const 32) (i32.const 3) (i32.const 48) (i32.const 5)))
      (then (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                        (i32.const 0) (i32.const 7)))
      (else (call $push (i32.const 0) (i32.const 0) (i32.const 0) (i32.const 0)
                        (i32.const 16) (i32.const 7))))))
