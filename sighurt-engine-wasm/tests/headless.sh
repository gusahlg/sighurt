#!/usr/bin/env bash
# Runs the Wasm engine against a real compositor, in a private headless sway, never on the
# desktop:
#
# - tests/wayland.rs: `CpuTarget` for the host's SHM `TargetLease` (#[ignore]d in a plain
#   `cargo test`; included here);
# - the wasm-view example on raf-demo (animating) and gradient-demo (static), each through a live
#   resize to 1000x700 and a rescale to 125 %, checking what the example prints on exit: exit 0,
#   no failure, frames submitted at the final buffer size, at most three buffers (the host's
#   bound: a third only while the compositor holds both others) and no retiring generation;
#   raf-demo animating, gradient-demo presenting only when its layout changes.
#
#   sighurt-engine-wasm/tests/headless.sh [directory for the logs and screenshots]
#
# The screenshots (three per page: before the resize, after it, after the rescale) are for looking
# at; this script doesn't compare them.
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
view=target/debug/examples/wasm-view
guests=target/wasm32-unknown-unknown/release

# Inside the compositor: runs the example on $2 for 6 s, screenshots and changes the output, and
# writes its output and exit status to $3.log.
if [[ ${1:-} == --inside ]]; then
  page=$2 out=$3
  "$view" "$page" --exit-after 6 > "$out.log" 2>&1 &
  pid=$!
  sleep 2; grim "$out-a.png"
  swaymsg output HEADLESS-1 resolution 1000x700 > /dev/null; sleep 1; grim "$out-b.png"
  swaymsg output HEADLESS-1 scale 1.25 > /dev/null; sleep 1.5; grim "$out-c.png"
  status=0
  wait "$pid" || status=$?
  echo "exit $status" >> "$out.log"
  exit 0
fi

out=$(realpath "${1:-$(mktemp -d)}")
mkdir -p "$out"
SIG_WHAT="build the Wasm engine's headless checks" tools/cargo build --target wasm32-unknown-unknown \
  --release -p raf-demo -p gradient-demo
tools/cargo build -p sighurt-engine-wasm --example wasm-view
bin=$(tools/cargo test -p sighurt-engine-wasm --test wayland --no-run 2>&1 | tee /dev/stderr |
  grep -o 'target/debug/deps/wayland-[0-9a-f]*' | tail -1)
[[ -n $bin ]] || { echo "headless.sh: no test binary" >&2; exit 1; }

tools/heavy tools/headless-sway -- env SIGHURT_HEADLESS=1 "$bin" --include-ignored --test-threads 1

failed=0
fail() { echo "headless.sh: $1: $2" >&2; failed=1; }
# Checks the log of the run named $1; $2 and $3 bound the frames it submitted.
check() {
  local name=$1 least=$2 most=$3 log="$out/$1.log" page
  grep -qx 'exit 0' "$log" || fail "$name" "did not exit 0"
  ! grep -qE 'connection lost|present failed|view failed|runtime failed|panicked' "$log" ||
    fail "$name" "reported a failure"
  page=$(grep '^page: ' "$log") || { fail "$name" "printed no counters"; return; }
  local re='buffer ([0-9]+)x([0-9]+), .*submissions: ([0-9]+),.*BufferStats \{ buffers: ([0-9]+), bytes: ([0-9]+), retiring_generations: ([0-9]+)'
  [[ $page =~ $re ]] || { fail "$name" "unreadable counters: $page"; return; }
  local w=${BASH_REMATCH[1]} h=${BASH_REMATCH[2]} submitted=${BASH_REMATCH[3]} buffers=${BASH_REMATCH[4]}
  local bytes=${BASH_REMATCH[5]} retiring=${BASH_REMATCH[6]}
  # 1000x700 output pixels at 125 %: an 800x560 window, a 1000x700 buffer.
  [[ ${w}x$h == 1000x700 ]] || fail "$name" "final buffer ${w}x$h, not 1000x700"
  ((submitted >= least && submitted <= most)) || fail "$name" "$submitted frames, not $least to $most"
  ((buffers <= 3)) || fail "$name" "$buffers buffers"
  ((bytes == buffers * 4 * w * h)) || fail "$name" "$bytes bytes in $buffers buffers of ${w}x$h"
  ((retiring == 0)) || fail "$name" "$retiring retiring generations"
  echo "headless.sh: $name: $page"
}
for page in raf-demo gradient-demo; do
  tools/heavy tools/headless-sway -- "$0" --inside "$guests/${page//-/_}.wasm" "$out/$page"
done
# About 6 s at 60 Hz, allowing for a slow debug build; a static page presents once per layout
# (hello-sighurt is not static: its caret blinks twice a second).
check raf-demo 120 1000
check gradient-demo 1 8
echo "headless.sh: logs and screenshots in $out"
exit "$failed"
