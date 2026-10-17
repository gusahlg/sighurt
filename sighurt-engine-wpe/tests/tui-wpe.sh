#!/usr/bin/env bash
# `sig --tui` with WPE WebKit in a pty, with no display (tests/tui-wpe.py): the last kitty image
# of `--engine wpe https://example.com/` decoded to OUT/wpe/page.png, and WebKit's processes'
# environment with and without Servo in the same process. Needs network access, the nix-shell
# (GIO_EXTRA_MODULES) and a private D-Bus session.
#
#   sighurt-engine-wpe/tests/tui-wpe.sh [OUT]
set -euo pipefail
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
SIG_WHAT="servo build" tools/cargo build -p sighurt-browser --bin sig
out=${1:-/tmp/sighurt-wpe-tui}
rm -rf "$out"
mkdir -p "$out"
# The build took the heavy lock and dropped it. The run needs the nix-shell environment.
exec env -u WAYLAND_DISPLAY -u DISPLAY \
  tools/heavy sighurt-engine-wpe/tests/private-bus \
  timeout 360 python3 -I sighurt-engine-wpe/tests/tui-wpe.py "$repo/target/debug/sig" "$out"
