#!/usr/bin/env bash
# Builds smoke and the helper into $1 (default: the CEF prefix's `dist` directory, see CEF.md).
# Run inside `nix-shell nix/cef.nix -A shell`.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
prefix=${SIGHURT_CEF_PREFIX:-/data/sig-builds/deps/cef-150.0.22}
out=${1:-$prefix/dist}
mkdir -p "$out"
cc -O2 -Wall -Wextra -DCEF_API_VERSION=${CEF_API:-15000} -I"$prefix" -o "$out/cef-smoke" "$here/smoke.c" -ldl
cc -O2 -Wall -Wextra -DCEF_API_VERSION=${CEF_API:-15000} -I"$prefix" -o "$out/sig-cef-helper" "$here/helper.c" -L"$out" -lcef -Wl,--allow-shlib-undefined \
  -Wl,-rpath,'$ORIGIN'
