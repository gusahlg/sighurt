#!/usr/bin/env bash
# Builds the NetSurf smoke test: unpacks the pinned nixpkgs' NetSurf 3.11 source into OUT (default
# $PWD/out), adds this directory's frontend/ as frontend "sighurt", and runs NetSurf's own
# Makefile. Run inside `nix-shell shell.nix`, as `tools/heavy nix-shell experiments/netsurf-smoke/shell.nix --run ...`.
#   NS_MAKE_ARGS=NETSURF_USE_DUKTAPE=YES build.sh OUT   builds with the JavaScript engine (Duktape).
#   build.sh [OUT]   then:  OUT/netsurf-3.11/nssighurt URL OUT.png
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$here/out}
mkdir -p "$out"
[[ -d $out/netsurf-3.11 ]] || tar xzf "$NS_SRC" -C "$out"
cd "$out/netsurf-3.11"
mkdir -p frontends/sighurt
cp "$here"/frontend/* frontends/sighurt/
mkdir -p frontends/sighurt/res
cp -r resources/* frontends/sighurt/res/ 2>/dev/null || true
sed -i 's/^VLDTARGET := \(.*\)monkey/VLDTARGET := \1monkey sighurt/' frontends/Makefile.hts
# (CFLAGS goes in the environment: on the command line it would replace the Makefile's own.)
time env CFLAGS="-fcommon -Wno-error" make -j"${SIG_JOBS:-4}" TARGET=sighurt ${NS_MAKE_ARGS:-} 2>&1 | tail -${TAIL:-40}
ls -l nssighurt
