#!/usr/bin/env bash
# Builds libsighurt-ladybird-shim.so, the C ABI over nixpkgs' Ladybird (docs/rewrite/LADYBIRD.md),
# without building Ladybird: it compiles against the installed headers and links the installed
# libraries of the pinned nixpkgs, with the headers nixpkgs does not install generated from its
# source (experiments/ladybird-smoke/gen-*.sh). Reproducible: the result depends on nix/nixpkgs.nix
# and this tree only.
#
#   tools/heavy nix/build-ladybird-shim.sh [PREFIX]
#
# PREFIX defaults to /data/sig-builds/ladybird (the HDD) when /data/sig-builds exists, else
# ~/.cache/sighurt/ladybird (override: SIGHURT_LADYBIRD_PREFIX_DIR). It receives
#   lib/libsighurt-ladybird-shim.so   the shim
#   ladybird-root                     a GC root of the Ladybird package it runs against
#   cache/                            source copy, generated headers, build tree (about 340 MB)
# and prints the library's path. build.rs of sighurt-engine-ladybird finds it there, or through
# SIGHURT_LADYBIRD_SHIM; at run time the engine also honors SIGHURT_LADYBIRD_SHIM.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
if [[ -n ${1:-} ]]; then prefix=$1
elif [[ -n ${SIGHURT_LADYBIRD_PREFIX_DIR:-} ]]; then prefix=$SIGHURT_LADYBIRD_PREFIX_DIR
elif [[ -d /data/sig-builds ]]; then prefix=/data/sig-builds/ladybird
else prefix=$HOME/.cache/sighurt/ladybird; fi
prefix=$(realpath -m "$prefix")
mkdir -p "$prefix/lib"
# The shim is built against the patched Ladybird (nix/ladybird.nix, NavigationRequested), which
# nix/pull-ladybird-patched.sh puts at PREFIX/patched.
if [[ ! -d $prefix/patched/include/LibWebView ]]; then
  echo "build-ladybird-shim: no patched Ladybird at $prefix/patched: build it on louise-pc" >&2
  echo "  (nix/build-ladybird-patched.sh) and run nix/pull-ladybird-patched.sh" >&2
  exit 1
fi
if [[ -z ${LADYBIRD:-} ]]; then
  exec nix-shell "$repo/experiments/ladybird-smoke/shell.nix" --argstr patchedPrefix "$(readlink -f "$prefix/patched")" \
    --run "$(printf '%q ' "$0" "$prefix")"
fi
cache=$prefix/cache
mkdir -p "$cache"
# Keep the Ladybird package's inputs (the same as the patched one's) from being collected.
nix-build --no-out-link -o "$prefix/ladybird-root" -E "(import $repo/nix/nixpkgs.nix {}).ladybird" > /dev/null
[[ -f $cache/rust/LibURL/RustFFI.h ]] || "$repo/experiments/ladybird-smoke/gen-rust-headers.sh" "$cache/rust" > /dev/null
[[ -f $cache/build/Libraries/LibWeb/CSS/Units.h ]] || "$repo/experiments/ladybird-smoke/gen-generated-headers.sh" "$cache"
export LADYBIRD_RUST_HEADERS=$cache/rust LADYBIRD_GEN=$cache/build/Libraries
# The flag guard: the definitions the shim sets must be the ones the source's own LibWebView build
# uses (they are layout-relevant), or this fails instead of producing a silently skewed library.
flags=$(ninja -C "$cache/build" -t commands Libraries/LibWebView/CMakeFiles/LibWebView.dir/HeadlessWebView.cpp.o | tail -1 | tr ' ' '\n' | grep -E '^-D(USE_|HAVE_|ENABLE_COMPILETIME|NDEBUG)' | sort -u | tr '\n' ' ')
want='-DENABLE_COMPILETIME_FORMAT_CHECK -DHAVE_WASM_COMPILER_SERVICE=1 -DNDEBUG -DUSE_FONTCONFIG=1 -DUSE_VULKAN=1 -DUSE_VULKAN_DMABUF_IMAGES=1 '
if [[ $flags != "$want" ]]; then
  echo "build-ladybird-shim: the source's compile definitions changed:" >&2
  echo "  source: $flags" >&2; echo "  shim:   $want" >&2
  echo "  update engines/sighurt-engine-ladybird/shim/CMakeLists.txt (and the ABI doc) first." >&2
  exit 1
fi
# A cache configured from another checkout (a worktree) refuses this source directory; start it over.
if [[ -f $cache/shim/CMakeCache.txt ]] && ! grep -q "CMAKE_HOME_DIRECTORY:INTERNAL=$repo/engines/sighurt-engine-ladybird/shim$" "$cache/shim/CMakeCache.txt"; then
  rm -rf "$cache/shim"
fi
cmake -S "$repo/engines/sighurt-engine-ladybird/shim" -B "$cache/shim" -G Ninja -DCMAKE_BUILD_TYPE=Release > /dev/null
cmake --build "$cache/shim" -j"${SIG_JOBS:-2}"
install -m 755 "$cache/shim/libsighurt-ladybird-shim.so" "$prefix/lib/"
echo "$prefix/lib/libsighurt-ladybird-shim.so"
