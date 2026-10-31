#!/usr/bin/env bash
# Brings the patched Ladybird (nix/build-ladybird-patched.sh, built on louise-pc) to this machine
# without root, as a plain directory.
#
# `nix copy --from ssh://louise-pc` would put it in the store, but it is not signed, and importing
# an unsigned path needs a trusted user (here: root only). So the package is copied as files and
# relocated: every occurrence of its store path (RUNPATHs, the helper processes' wrappers, the CMake
# files) is replaced by the new directory, whose path is chosen to be exactly as long, so ELF files
# stay valid. Everything the package depends on is in this machine's store already (it is nixpkgs'
# Ladybird with one patch, so the same inputs): they are kept alive by the root of nix/build-ladybird-shim.sh.
#
#   nix/pull-ladybird-patched.sh [DEST_PARENT]      (default /data/sig-builds/ladybird)
#
# Result: DEST_PARENT/patched, a symlink to the relocated directory; its path is printed.
set -euo pipefail
parent=${1:-/data/sig-builds/ladybird}
mkdir -p "$parent"
out=$(ssh -o BatchMode=yes louise-pc 'readlink -f ~/ladybird-patched')
[[ $out == /nix/store/*-ladybird-* ]] || { echo "pull-ladybird-patched: nothing built on louise-pc ($out)" >&2; exit 1; }
want=${#out}
# A directory name that makes the whole path as long as the store path.
dir="$parent/p"
pad=$(( want - ${#dir} ))
(( pad >= 1 )) || { echo "pull-ladybird-patched: $parent is too long" >&2; exit 1; }
dest="$dir$(printf 'x%.0s' $(seq 1 "$pad"))/"
dest=${dest%/}
[[ ${#dest} == "$want" ]] || { echo "pull-ladybird-patched: cannot make a path of $want characters" >&2; exit 1; }
rm -rf "$dest.new"
rsync -a --delete -e 'ssh -o BatchMode=yes' "louise-pc:$out/" "$dest.new/"
chmod -R u+w "$dest.new"
# Same-length replacement in every file (binary ones too).
export LC_ALL=C
grep -rl --binary-files=text -F "$out" "$dest.new" | while IFS= read -r file; do
  [[ -L $file ]] && continue
  sed -i "s|$out|$dest|g" "$file"
done
# Symlinks that named the store path.
find "$dest.new" -type l | while IFS= read -r link; do
  target=$(readlink "$link")
  if [[ $target == "$out"* ]]; then ln -sfn "${target/$out/$dest}" "$link"; fi
done
rm -rf "$dest"
mv "$dest.new" "$dest"
ln -sfn "$dest" "$parent/patched"
echo "$parent/patched -> $dest (from $out)"
