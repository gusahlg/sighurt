#!/usr/bin/env bash
# Builds the rewrite's `sig` at a git revision, in release mode, on the benchmark machine, into
# a directory of its own that nothing writes again: ~/sighurt-bench/builds/<rev>[-<features>]
# with `sig`, build-info.json and the build log. It then points
# ~/sighurt-bench/builds/rewrite-latest at it, which bench/builds/rewrite.toml measures.
# Run it here, in the repository:
#
#   bench/rewrite/build.sh HEAD                          # sig's default features
#   bench/rewrite/build.sh 1a2b3c4 --features engine-demo
#   bench/remote --no-sync tail -f ~/sighurt-bench/logs/latest.log
#
# It uploads `git archive <rev>` to louise-pc and builds it there, detached (bench/remote
# --detach), under the archive's own tools/heavy and shell.nix, with the Rust of its pinned
# nixpkgs (nix/nixpkgs.nix), which build-info.json records. The target directory
# ~/sighurt-bench/target-rewrite is kept between builds, so a new revision rebuilds only what
# changed, and the binary is copied out of it, so no later build or sync (tools/remote's
# ~/repos/sighurt-rewrite) can change a build that was measured.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
root=sighurt-bench

if [[ ${1:-} == --on-host ]]; then
  # On the benchmark machine, from bench/remote: --on-host <full rev> <features>.
  rev=$2 features=$3
  short=${rev:0:12}
  name=$short
  flags=(--release -p sighurt-browser)
  if [[ $features != default ]]; then
    name=$short-${features//,/+}
    flags+=(--no-default-features --features "$features")
  fi
  dest=~/$root/builds/$name
  src=~/$root/src/$short
  [[ -e $dest ]] && { echo "$dest exists: a build is never replaced" >&2; exit 1; }
  if [[ ! -d $src ]]; then
    mkdir -p "$src.tmp"
    tar -xzf "$src.tar.gz" -C "$src.tmp"
    mv "$src.tmp" "$src"
  fi
  mkdir -p "$dest.tmp"
  cd "$src"
  # Rust from the revision's pinned nixpkgs, which revisions before shell.nix brought it lack;
  # the machine's own may be older than the workspace's rust-version.
  if [[ -f nix/nixpkgs.nix ]] && ! grep -q 'rustc cargo' shell.nix; then
    PATH=$(nix-build nix/nixpkgs.nix -A rustc -A cargo --no-out-link | sed 's|$|/bin|' | paste -sd:):$PATH
  fi
  CARGO_TARGET_DIR=~/$root/target-rewrite SIG_MEM=${SIG_MEM:-11G} SIG_WHAT="bench: rewrite sig $name" \
    tools/heavy cargo build "${flags[@]}" 2>&1 | tee "$dest.tmp/build.log"
  cp ~/$root/target-rewrite/release/sig "$dest.tmp/sig"
  cat > "$dest.tmp/build-info.json" <<EOF
{
  "revision": "$rev",
  "features": "$features",
  "cargo": "cargo build ${flags[*]}",
  "finished": "$(grep -a 'Finished' "$dest.tmp/build.log" | tail -1 | sed 's/^ *//; s/"/\\"/g')",
  "built": "$(date -Iseconds)",
  "rustc": "$(rustc --version)",
  "sha256": {"sig": "$(sha256sum "$dest.tmp/sig" | cut -d' ' -f1)"}
}
EOF
  chmod -R a-w "$dest.tmp"
  mv "$dest.tmp" "$dest"
  ln -sfn "$dest" ~/$root/builds/rewrite-latest
  cat "$dest/build-info.json"
  exit 0
fi

[[ $# -ge 1 ]] || { sed -n '2,17p' "$0"; exit 2; }
repo=$(cd "$here/../.." && pwd)
host=${SIG_BENCH_HOST:-louise-pc}
rev=$(git -C "$repo" rev-parse --verify "$1^{commit}")
features=default
if [[ ${2:-} == --features ]]; then
  features=${3:?--features needs a list}
fi
short=${rev:0:12}
# The remote login shell is fish; these commands read the same in it.
ssh -o BatchMode=yes "$host" "mkdir -p $root/src $root/builds"
git -C "$repo" archive --format=tar.gz "$rev" | ssh -o BatchMode=yes "$host" "cat > $root/src/$short.tar.gz"
exec "$repo/bench/remote" --detach bench/rewrite/build.sh --on-host "$rev" "$features"
