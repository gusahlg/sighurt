#!/usr/bin/env bash
# Builds sig-cef-helper into $1 (default: dist/ of the CEF prefix). It links the patched
# libcef.so in that directory; run inside `nix-shell nix/cef.nix -A shell`, as nix/build-cef.sh does.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
prefix=${SIGHURT_CEF_PREFIX:-/data/sig-builds/deps/cef-150.0.22}
out=${1:-$prefix/dist}
mkdir -p "$out"
cc -O2 -Wall -Wextra -DCEF_API_VERSION=15000 -I"$prefix" -o "$out/sig-cef-helper" "$here/helper.c" \
  -L"$out" -lcef -Wl,--allow-shlib-undefined -Wl,-rpath,'$ORIGIN'
