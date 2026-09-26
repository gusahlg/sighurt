#!/usr/bin/env bash
# One short run on the real desktop. It injects no input. Screenshots capture this window only
# (grim -T: ext-foreign-toplevel-list plus ext-image-copy-capture; this fails on River 0.4.8,
# see docs/rewrite/M1.md), and Servo's files go to OUT.
#
#   tools/heavy experiments/m1-mixed/live-check.sh OUT [m1-mixed args...]
#
# Only run it when the user has allowed a live check. Pass --exit-after: the window closes itself.
set -u
out=$1
shift
bins=$(nix-build '<nixpkgs>' -A lswt -A grim --no-out-link 2>/dev/null | sed 's|$|/bin|' | paste -sd:)
export PATH="$bins:$PATH"
bin=${M1_BIN:-target/debug/m1-mixed}
mkdir -p "$out/home"
export XDG_CONFIG_HOME=$out/home/config XDG_DATA_HOME=$out/home/data XDG_CACHE_HOME=$out/home/cache
"$bin" "$@" > "$out/m1.log" 2>&1 &
pid=$!
# shot NAME DELAY: captures the window after DELAY seconds.
shot() {
  sleep "$2"
  local id
  id=$(lswt -c ai | awk -F, '$1 == "sighurt-m1" { print $2; exit }')
  if [[ -n $id ]]; then grim -T "$id" "$out/$1.png"; fi
}
shot 1-early 3
shot 2-late 9
wait "$pid"
echo "m1-mixed exited with $?" >> "$out/m1.log"
