;; Saves "hello" under an absolute path outside the download folder (api_download_data):
;; engines.sh's check that a guest's download goes through sig, into the download folder, named
;; after the path's last part.
(module
  (import "oxide" "api_download_data" (func $save (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "hello")
  (data (i32.const 16) "/tmp/sighurt-e2e-escape.txt")
  (func (export "start_app")
    (drop (call $save (i32.const 0) (i32.const 5) (i32.const 16) (i32.const 27)))))
