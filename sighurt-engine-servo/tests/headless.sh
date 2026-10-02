#!/usr/bin/env bash
# Runs the Servo adapter's compositor tests (tests/engine.rs, crash.rs, unavailable_context.rs) in a
# private headless sway, never on a desktop. They build Servo, so run this on louise-pc:
#
#   SIG_WHAT="servo test (m4)" tools/remote sighurt-engine-servo/tests/headless.sh
#
# Each test binary is one process (Servo initializes once per process) with its own compositor.
# The tests are #[ignore]d so that a plain `cargo test` lists them as ignored; crash.rs has its
# own main, which does nothing without SIGHURT_SERVO_HEADLESS. SIG_RENDERER picks
# sway's renderer: gles2 (default; sway needs SWAY_UNSUPPORTED_GPU on the NVIDIA driver, and the
# pages render on the GPU) or pixman (then set Mesa's EGL vendor for llvmpipe yourself).
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
build() {
  SIG_WHAT="servo build (m4 tests)" tools/cargo test -p sighurt-engine-servo --test "$1" --no-run 2>&1 |
    tee /dev/stderr | grep -o "target/debug/deps/$1-[0-9a-f]*" | tail -1
}
engine=$(build engine)
crash=$(build crash)
unavailable=$(build unavailable_context)
[[ -n $engine && -n $crash && -n $unavailable ]] || { echo "headless.sh: no test binary" >&2; exit 1; }
export SWAY_UNSUPPORTED_GPU=true
# `timeout`: a Servo that never finishes shutting down would hold the machine's lock forever.
run() {
  tools/heavy tools/headless-sway --renderer "${SIG_RENDERER:-gles2}" -- \
    timeout 600 env SIGHURT_SERVO_HEADLESS=1 "$@" --test-threads 1 --include-ignored --nocapture
}
status=0
run "$engine" || status=$?
run "$crash" || status=$?
# An EGL vendor list naming no driver: libglvnd gives surfman no display, so no view gets a context.
run __EGL_VENDOR_LIBRARY_FILENAMES=/nonexistent/no-driver.json "$unavailable" || status=$?
exit "$status"
