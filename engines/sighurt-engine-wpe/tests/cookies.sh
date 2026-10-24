#!/usr/bin/env bash
# Persistent cookies (tests/cookies.rs): no display server, a private D-Bus session.
# WebKit starts once per process, so the tests spawn themselves. They are #[ignore]d;
# a plain `cargo test` skips them.
#
#   engines/sighurt-engine-wpe/tests/cookies.sh
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"
export SIG_WHAT="${SIG_WHAT:-wpe build: cookie persistence}"
bin=$(tools/cargo test -p sighurt-engine-wpe --test cookies --no-run 2>&1 | tee /dev/stderr |
  grep -o 'target/debug/deps/cookies-[0-9a-f]*' | tail -1)
[[ -n $bin ]] || { echo "cookies.sh: no test binary" >&2; exit 1; }
# The build released the heavy lock. The run needs the nix-shell (WebKit's libraries) and no display.
exec tools/heavy env -u WAYLAND_DISPLAY -u DISPLAY -u LIBGL_ALWAYS_SOFTWARE -u DRI_PRIME \
  engines/sighurt-engine-wpe/tests/private-bus \
  timeout 300 "$bin" --include-ignored --nocapture --test-threads 1
