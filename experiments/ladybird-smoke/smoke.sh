#!/usr/bin/env bash
# Builds and runs the Ladybird smoke test (docs/rewrite/LADYBIRD.md): loads a URL through LibWebView
# in a headless embedder and writes the first frame after the load as a PNG. Everything is built
# against nixpkgs' installed `ladybird` (headers and libraries), with the headers it doesn't install
# generated from its source. Prints PASS or FAIL.
#
#   tools/heavy experiments/ladybird-smoke/smoke.sh [OUT_PNG [URL]]
#
# Needs network (the page; cache.nixos.org on the first run). Caches (about 340 MB: a copy of Ladybird's source, its generated headers, the build) in /data/storage/sighurt/ladybird-smoke, else ~/.cache/sighurt/ladybird-smoke.
# Never touches a Wayland session: WAYLAND_DISPLAY and DISPLAY are unset for the browser.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=$(realpath -m "${1:-ladybird-smoke.png}")
url=${2:-https://example.com/}
# Big trees stay off the SSD: the HDD directory when it exists (LADYBIRD_SMOKE_CACHE overrides).
if [[ -d /data/storage/sighurt ]]; then default_cache=/data/storage/sighurt/ladybird-smoke
else default_cache=$HOME/.cache/sighurt/ladybird-smoke; fi
cache=${LADYBIRD_SMOKE_CACHE:-$default_cache}
if [[ -z ${LADYBIRD:-} ]]; then
  exec nix-shell "$here/shell.nix" --run "$(printf '%q ' "$0" "$@")"
fi
mkdir -p "$cache"
[[ -f $cache/rust/LibURL/RustFFI.h ]] || "$here/gen-rust-headers.sh" "$cache/rust" > /dev/null
[[ -f $cache/build/Libraries/LibWeb/CSS/Units.h ]] || "$here/gen-generated-headers.sh" "$cache"
export LADYBIRD_RUST_HEADERS=$cache/rust LADYBIRD_GEN=$cache/build/Libraries
cmake -S "$here" -B "$cache/smoke" -G Ninja > /dev/null
cmake --build "$cache/smoke" -j"${SIG_JOBS:-2}"
export LADYBIRD_PREFIX=$LADYBIRD FONTCONFIG_FILE=/etc/fonts/fonts.conf
env -u WAYLAND_DISPLAY -u DISPLAY timeout 90 "$cache/smoke/smoke" "$url" "$out"
