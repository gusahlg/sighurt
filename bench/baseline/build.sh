#!/usr/bin/env bash
# Builds the pre-rewrite browser (git tag gpui-baseline) in release mode, on the benchmark machine:
# `sig` (sighurt-ui), `sig-wasm`, `sig-servo` (its own workspace) and the Wasm guests the
# workloads use. Each cargo command runs under tools/heavy. Put the tag's tree in place first:
#
#   git archive gpui-baseline | ssh louise-pc 'mkdir -p ~/sighurt-baseline && tar -x -C ~/sighurt-baseline'
#   bench/remote --detach bench/baseline/build.sh
#
# Afterwards $SIG_BASELINE/target/release holds sig, sig-wasm and a copy of sig-servo (sig looks
# for engines next to itself), and $SIG_BASELINE/build-info.json their hashes.
set -euo pipefail
heavy=$(cd "$(dirname "$0")/../.." && pwd)/tools/heavy
tree=${SIG_BASELINE:-$HOME/sighurt-baseline}
cd "$tree"

SIG_WHAT="baseline sig + sig-wasm release" "$heavy" cargo build --release -p sighurt-ui -p sighurt-engine-wasm
SIG_WHAT="baseline wasm guests" "$heavy" cargo build --release --target wasm32-unknown-unknown \
  -p hello-sighurt -p gradient-demo -p raf-demo -p timer-demo
SIG_MEM=${SIG_MEM:-11G} SIG_WHAT="baseline sig-servo release" "$heavy" \
  cargo build --release --manifest-path sighurt-engine-servo/Cargo.toml
cp sighurt-engine-servo/target/release/sig-servo target/release/

{
  echo "{"
  echo "  \"revision\": \"$(cat REVISION 2>/dev/null || echo unknown)\","
  echo "  \"built\": \"$(date -Iseconds)\","
  echo "  \"rustc\": \"$(rustc --version)\","
  echo "  \"sha256\": {"
  first=1
  for f in target/release/sig target/release/sig-wasm target/release/sig-servo \
    target/wasm32-unknown-unknown/release/*.wasm; do
    [[ $first == 1 ]] || echo ","
    first=0
    printf '    "%s": "%s"' "$f" "$(sha256sum "$f" | cut -d' ' -f1)"
  done
  echo
  echo "  }"
  echo "}"
} > build-info.json
cat build-info.json
