#!/usr/bin/env bash
# Runs the Wayland host's protocol tests (tests/headless.rs) in a private headless sway, never on
# the desktop: builds them with tools/cargo, then runs them one at a time inside
# tools/headless-sway, with wl-clipboard on PATH. They are #[ignore]d so that a plain `cargo test`
# lists them as ignored; this script includes them. The test that stops the compositor runs last,
# in a compositor of its own.
#
#   frontends/sighurt-wayland/tests/headless.sh [test name filter and other test-binary arguments]
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"
bin=$(SIG_WHAT="build sighurt-wayland headless tests" tools/cargo test -p sighurt-wayland \
  --test headless --no-run 2>&1 | tee /dev/stderr | grep -o 'target/debug/deps/headless-[0-9a-f]*' | tail -1)
[[ -n $bin ]] || { echo "headless.sh: no test binary" >&2; exit 1; }
clipboard=$(nix-build '<nixpkgs>' -A wl-clipboard --no-out-link)/bin
run() {
  # GL tests use Mesa in software (llvmpipe through wl_shm), which works in headless sway.
  local gl="__EGL_VENDOR_LIBRARY_FILENAMES=/run/opengl-driver/share/glvnd/egl_vendor.d/50_mesa.json LIBGL_ALWAYS_SOFTWARE=1"
  nix-shell shell.nix --run "PATH=$clipboard:\$PATH tools/headless-sway -- env SIGHURT_HEADLESS=1 $gl $bin --test-threads 1 --include-ignored $*"
}
stop=losing_the_compositor_is_reported_once
if [[ $# -gt 0 ]]; then
  run "$@"
else
  run --skip "$stop"
  run --exact "$stop"
fi
