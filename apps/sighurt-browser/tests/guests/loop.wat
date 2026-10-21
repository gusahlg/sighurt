;; Loops forever in on_frame: the engine's fuel limit stops it (sighurt-engine-wasm's
;; LOOPS_IN_ON_FRAME). engines.sh loads this text as a module; Wasmtime compiles WAT itself.
(module
  (func (export "start_app"))
  (func (export "on_frame") (param i32) (loop $l (br $l))))
