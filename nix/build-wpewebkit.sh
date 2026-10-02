#!/usr/bin/env bash
# Builds WPE WebKit into its prefix (nix/wpewebkit.nix), or copies a finished prefix from another
# machine; then roots the Nix store paths the prefix refers to and checks that it refers to no
# others. docs/rewrite/WPE.md says why it is built this way, with the flags, timings and disk use.
#
#   SIG_MEM=13G SIG_JOBS=6 SIG_WHAT="wpewebkit build" tools/heavy nix/build-wpewebkit.sh
#   nix/build-wpewebkit.sh --pull louise-pc
#
# A build that stopped resumes where it was; its tree is ~/.cache/sighurt/wpewebkit-<version>.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
nixfile=$here/wpewebkit.nix
log() { echo "build-wpewebkit: $(date '+%F %T'): $*"; }

# Replaces the prefix with a finished tree, so a half-copied or half-installed prefix never exists.
replace_prefix() {
  mkdir -p "$(dirname "$prefix")"
  rm -rf "$prefix.old"
  if [[ -e $prefix ]]; then mv "$prefix" "$prefix.old"; fi
  mv "$1" "$prefix"
  rm -rf "$prefix.old"
}

build() {
  local src=$work/src
  if [[ ! -d $src ]]; then
    local tarball
    tarball=$(nix-build "$nixfile" -A src --no-out-link) # fetchurl checks the sha256
    log "unpacking $tarball"
    rm -rf "$src.tmp"
    mkdir -p "$src.tmp"
    tar xJf "$tarball" -C "$src.tmp" --strip-components=1
    patch -d "$src.tmp" -p1 < "$(nix-build "$nixfile" -A sandboxPatch --no-out-link)"
    mv "$src.tmp" "$src"
  fi
  nix-shell --pure "$nixfile" -A shell --keep SIG_JOBS \
    --run "$(printf '%q ' "$here/build-wpewebkit.sh" --in-shell "$prefix" "$work")"
}

# Runs inside the build environment (nix-shell --pure).
configure_build_install() {
  local src=$work/src build=$work/build stage=$work/stage
  log "configuring"
  # Read at configure time: one link at a time, since libWPEWebKit's link alone takes gigabytes.
  export WEBKIT_NINJA_LINK_MAX=1
  # nixpkgs' g-ir-scanner names the library in the .gir and .typelib files by this directory,
  # which its setup hook sets to the shell's own (nonexistent) output.
  export NIX_GOBJECT_INTROSPECTION_DEFAULT_FALLBACK_LIBPATH=$prefix
  cmake -S "$src" -B "$build" -G Ninja \
    -DPORT=WPE \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$prefix" \
    -DCMAKE_INSTALL_LIBDIR=lib \
    -DCMAKE_INSTALL_LIBEXECDIR=libexec \
    -DCMAKE_INSTALL_RPATH="$prefix/lib" \
    -DENABLE_WPE_PLATFORM=ON \
    -DENABLE_WPE_PLATFORM_WAYLAND=ON \
    -DENABLE_WPE_PLATFORM_DRM=ON \
    -DENABLE_WPE_PLATFORM_HEADLESS=ON \
    -DENABLE_WPE_LEGACY_API=ON \
    -DENABLE_WPE_QT_API=OFF \
    -DENABLE_DOCUMENTATION=OFF \
    -DENABLE_INTROSPECTION=ON \
    -DENABLE_MINIBROWSER=ON \
    -DENABLE_BUBBLEWRAP_SANDBOX=ON \
    -DBWRAP_EXECUTABLE="$WPE_BWRAP" \
    -DDBUS_PROXY_EXECUTABLE="$WPE_DBUS_PROXY"
  log "building with ${SIG_JOBS:-6} jobs"
  ninja -C "$build" -j "${SIG_JOBS:-6}"
  log "installing"
  rm -rf "$stage"
  DESTDIR=$stage ninja -C "$build" install > /dev/null
  replace_prefix "$stage$prefix"
  rm -rf "$stage"
}

pull() {
  log "copying $1:$prefix"
  mkdir -p "$prefix.tmp"
  rsync -a --delete -e 'ssh -o BatchMode=yes' "$1:$prefix/" "$prefix.tmp/"
  replace_prefix "$prefix.tmp"
}

# A plain directory has no references Nix can see, so the store paths the prefix uses are rooted
# by a file that lists them (`runtime`), next to the prefix; on a new machine this also fetches
# them. Every store path written in the prefix (RPATHs, compiled-in paths) must be in its closure,
# except the ones nixpkgs' compiler wrapper scrubbed out of __FILE__ (hash eeee…, no such path).
root_and_check() {
  log "rooting the runtime closure at $prefix.nix-runtime"
  nix-build "$nixfile" -A runtime -o "$prefix.nix-runtime" > /dev/null
  local allowed used missing
  allowed=$(nix-store -qR "$prefix.nix-runtime" | sort -u)
  used=$(grep -rhoaE '/nix/store/[a-z0-9]{32}-[a-zA-Z0-9+._?=-]+' "$prefix" | grep -v '^/nix/store/e\{32\}-' | sort -u)
  missing=$(comm -13 <(echo "$allowed") <(echo "$used"))
  if [[ -n $missing ]]; then
    echo "build-wpewebkit: the prefix uses store paths outside the runtime closure:" >&2
    echo "$missing" >&2
    exit 1
  fi
  log "ok: $prefix uses $(wc -l <<< "$used") store paths, all in the rooted closure"
}

if [[ ${1:-} == --in-shell ]]; then
  prefix=$2 work=$3
  configure_build_install
  exit
fi
attr() { nix-instantiate --eval --raw "$nixfile" -A "$1"; }
prefix=$(attr prefix)
work=$HOME/.cache/sighurt/wpewebkit-$(attr version)
case ${1:-} in
  --pull) pull "${2:?usage: $0 --pull <host>}" ;;
  "") build ;;
  *) echo "usage: $0 [--pull <host>]" >&2; exit 2 ;;
esac
root_and_check
