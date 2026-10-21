#!/usr/bin/env bash
# The CPU presenter (tests/offscreen.rs): no display server, a private D-Bus session.
# WebKit starts once per process. The test is #[ignore]d, so a plain `cargo test` skips it.
#
#   engines/sighurt-engine-wpe/tests/offscreen.sh
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"
bin=$(tools/cargo test -p sighurt-engine-wpe --test offscreen --no-run 2>&1 | tee /dev/stderr |
  grep -o 'target/debug/deps/offscreen-[0-9a-f]*' | tail -1)
[[ -n $bin ]] || { echo "offscreen.sh: no test binary" >&2; exit 1; }
# The build released the heavy lock. The run needs the nix-shell (WebKit's libraries) and no display.
exec tools/heavy env -u WAYLAND_DISPLAY -u DISPLAY -u LIBGL_ALWAYS_SOFTWARE -u DRI_PRIME \
  engines/sighurt-engine-wpe/tests/private-bus \
  timeout 180 "$bin" --include-ignored --nocapture --test-threads 1
