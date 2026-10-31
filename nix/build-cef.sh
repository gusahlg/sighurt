#!/usr/bin/env bash
# Fetches the pinned CEF binary distribution into its prefix (nix/cef.nix), assembles dist/,
# gives the copied libcef.so an rpath, and roots the Nix store paths it needs.
# docs/rewrite/CEF.md says why this version and why it is not built from source.
#
#   tools/heavy nix/build-cef.sh
#
# A prefix whose stamp matches is left untouched.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/.." && pwd)
nixfile=$here/cef.nix
log() { echo "build-cef: $(date '+%F %T'): $*"; }

# CEF 150.0.22+gc1a097e+chromium-150.0.7871.256, linux64 minimal. Newer minimal
# builds (152 and up) did not start here (docs/rewrite/CEF.md).
url='https://cef-builds.spotifycdn.com/cef_binary_150.0.22%2Bgc1a097e%2Bchromium-150.0.7871.256_linux64_minimal.tar.bz2'
archive_name='cef_binary_150.0.22+gc1a097e+chromium-150.0.7871.256_linux64_minimal.tar.bz2'
sha256=dd1b5cc3ac6efe659ec7e8e9b0677ba4e43216a2fefcafd5abc9340837e32d96
sha1=7e7ca1e73e5d5e6317ca53dc1302fd180a3e1222
version=150.0.22

attr() { nix-instantiate --eval --raw "$nixfile" -A "$1"; }
prefix=$(attr prefix)
archive_dir=/data/sig-builds/deps/cef
archive=$archive_dir/$archive_name
stamp=$prefix/.sighurt-stamp

ready() {
  [[ -f $stamp && $(<"$stamp") == "$sha256" ]] || return 1
  [[ -f $prefix/dist/libcef.so && -x $prefix/dist/sig-cef-helper && -x $prefix/dist/cef-smoke ]] || return 1
  [[ -e $prefix.nix-runtime ]] || return 1
  [[ -f $prefix/include/cef_version.h ]] || return 1
  grep -q "CEF_VERSION \"$version" "$prefix/include/cef_version.h"
}

if ready; then
  log "$prefix is current (stamp matches); nothing to do"
  exit 0
fi

mkdir -p "$archive_dir"
if [[ ! -f $archive ]]; then
  log "downloading $archive_name"
  curl -fL --retry 3 -o "$archive.partial" "$url"
  mv "$archive.partial" "$archive"
fi
log "checking checksums"
echo "$sha256  $archive" | sha256sum -c -
echo "$sha1  $archive" | sha1sum -c -

if ! grep -q "CEF_VERSION \"$version" "$prefix/include/cef_version.h" 2>/dev/null; then
  log "unpacking into $prefix"
  rm -rf "$prefix.unpack"
  mkdir -p "$prefix.unpack"
  tar -xjf "$archive" -C "$prefix.unpack" --strip-components=1
  # Keep a dist/ that was already assembled if we are only refreshing sources.
  if [[ -d $prefix/dist && ! -d $prefix.unpack/dist ]]; then
    mv "$prefix/dist" "$prefix.unpack/dist"
  fi
  rm -rf "$prefix.old"
  if [[ -e $prefix ]]; then mv "$prefix" "$prefix.old"; fi
  mv "$prefix.unpack" "$prefix"
  rm -rf "$prefix.old"
fi

# Hard links into the unpacked tree, except libcef.so, which is copied so patchelf
# never rewrites Release/libcef.so. chrome-sandbox is the setuid helper and is not used.
assemble() {
  local dist=$prefix/dist
  mkdir -p "$dist/locales"
  local name
  for name in libEGL.so libGLESv2.so libvk_swiftshader.so libvulkan.so.1 \
    v8_context_snapshot.bin vk_swiftshader_icd.json; do
    if [[ ! -e $dist/$name ]]; then ln -f "$prefix/Release/$name" "$dist/$name"; fi
  done
  for name in chrome_100_percent.pak chrome_200_percent.pak icudtl.dat resources.pak; do
    if [[ ! -e $dist/$name ]]; then ln -f "$prefix/Resources/$name" "$dist/$name"; fi
  done
  for name in "$prefix/Resources/locales"/*; do
    local base
    base=$(basename "$name")
    if [[ ! -e $dist/locales/$base ]]; then ln -f "$name" "$dist/locales/$base"; fi
  done
  if [[ ! -f $dist/libcef.so ]]; then
    log "copying libcef.so (1.4 GB)"
    cp -a "$prefix/Release/libcef.so" "$dist/libcef.so"
  fi
}

assemble

ldd_clean() {
  ! env -u LD_LIBRARY_PATH ldd "$prefix/dist/libcef.so" | grep -q 'not found'
}

if ldd_clean; then
  log "libcef.so rpath already resolves"
else
  log "patching rpath on dist/libcef.so"
  libpath=$(attr libraryPath)
  nix-shell "$nixfile" -A shell --run \
    "patchelf --set-rpath $(printf '%q' "\$ORIGIN:$libpath") $(printf '%q' "$prefix/dist/libcef.so")"
  ldd_clean || { echo "build-cef: ldd still reports missing libraries" >&2; exit 1; }
fi

if [[ ! -x $prefix/dist/sig-cef-helper || ! -x $prefix/dist/cef-smoke ]]; then
  log "building the smoke test and sig-cef-helper"
  nix-shell "$nixfile" -A shell --run \
    "SIGHURT_CEF_PREFIX=$(printf '%q' "$prefix") $(printf '%q' "$repo/experiments/cef-smoke/build.sh") $(printf '%q' "$prefix/dist")"
fi

log "rooting the runtime closure at $prefix.nix-runtime"
nix-build "$nixfile" -A runtime -o "$prefix.nix-runtime" > /dev/null
allowed=$(nix-store -qR "$prefix.nix-runtime" | sort -u)
# Only the patched copy carries store paths. Scanning the whole 2.8 GB tree is the
# same check with a lot of binary that does not contain them.
used=$(readelf -d "$prefix/dist/libcef.so" | grep -oE '/nix/store/[a-z0-9]{32}-[^]:[]+' | sed 's|/lib$||' | sort -u)
missing=$(comm -13 <(echo "$allowed") <(echo "$used"))
if [[ -n $missing ]]; then
  echo "build-cef: libcef.so's rpath uses store paths outside the runtime closure:" >&2
  echo "$missing" >&2
  exit 1
fi

printf '%s\n' "$sha256" > "$stamp"
log "ok: $prefix"
log "  libcef.so $(stat -c '%s' "$prefix/dist/libcef.so") bytes"
log "  helper $prefix/dist/sig-cef-helper"
log "  smoke  $prefix/dist/cef-smoke"
log "  runtime $(readlink -f "$prefix.nix-runtime")"
log "  rpath entries $(echo "$used" | wc -l), all in the rooted closure"
