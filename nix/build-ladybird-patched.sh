#!/usr/bin/env bash
# Builds nix/ladybird.nix (nixpkgs' Ladybird plus the patches of nix/ladybird/): a full Ladybird build,
# hours of work, so on louise-pc, never on the desktop. Run it through tools/remote, which syncs this
# tree and takes that machine's heavy lock:
#
#   SIG_REMOTE_DEST=repos/sighurt-wt-ladybird SIG_WHAT="ladybird build" \
#     tools/remote tools/heavy nix/build-ladybird-patched.sh
#
# Parallelism is capped to fit 15 GB (one build job, 8 cores). The result is rooted at
# ~/ladybird-patched on that machine and its path is printed; nix/pull-ladybird-patched.sh brings it
# to the desktop.
set -euo pipefail
repo=$(cd "$(dirname "$0")/.." && pwd)
if [[ -e /tmp/gusahlg-desktop-embargo ]]; then
  echo "build-ladybird-patched: the user is using this machine (/tmp/gusahlg-desktop-embargo)" >&2
  exit 75
fi
start=$(date +%s)
nix-build "$repo/nix/ladybird.nix" --cores 8 --max-jobs 1 -o "$HOME/ladybird-patched"
echo "built in $(( ($(date +%s) - start) / 60 )) min: $(readlink -f "$HOME/ladybird-patched")"
