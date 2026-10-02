#!/usr/bin/env bash
# Runs the WPE adapter's compositor tests (tests/engine.rs) in a private headless sway, on a D-Bus
# session of their own (tests/private-bus), never on a desktop. Needs the WPE WebKit prefix
# (docs/rewrite/WPE.md):
#
#   sighurt-engine-wpe/tests/headless.sh            # sway's GLES2 renderer: DMA-BUF
#   sighurt-engine-wpe/tests/headless.sh pixman     # no DMA-BUF: the shared-memory fallback
#
# The test is #[ignore]d, so a plain `cargo test` lists it as ignored. WebKit starts once per
# process, so the test binary is one process with one scenario after another.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
renderer=${1:-gles2}
bin=$(tools/cargo test -p sighurt-engine-wpe --test engine --no-run 2>&1 | tee /dev/stderr |
  grep -o 'target/debug/deps/engine-[0-9a-f]*' | tail -1)
[[ -n $bin ]] || { echo "headless.sh: no test binary" >&2; exit 1; }
export SWAY_UNSUPPORTED_GPU=true
tools/heavy tools/headless-sway --renderer "$renderer" -- \
  sighurt-engine-wpe/tests/private-bus timeout 600 "$bin" --include-ignored --nocapture --test-threads 1
