#!/usr/bin/env bash
# Exploratory: the animations for 120 s, rewrite then baseline; each run under the lock on its own.
set -u
cd ~/sighurt-bench/harness
out=~/sighurt-bench/long-anim
mkdir -p "$out"
for run in rewrite-wasm-raf-long baseline-wasm-raf-long rewrite-css-animation-long baseline-css-animation-long; do
  build=${run%%-*}
  workload=${run#*-}
  case $build in
    rewrite) def=bench/builds/rewrite.toml dir=~/sighurt-bench/builds/3c510fe0b051 ;;
    baseline) def=bench/builds/baseline.toml dir=~/sighurt-baseline ;;
  esac
  SIG_WHAT="bench: long animation $run" tools/heavy tools/headless-sway --renderer gles2 -- \
    bench/measure --build "$def" --build-dir "$dir" --workload "$workload" \
    --workloads "$out/long-anim.toml" --out "$out/$run.json" --raw "$out/raw/$run"
  echo "$run: $?"
done
