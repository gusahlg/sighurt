#!/usr/bin/env bash
# Exploratory long tab churn, alternating the two builds; each run under the lock on its own.
set -u
cd ~/sighurt-bench/harness
out=~/sighurt-bench/churn
mkdir -p "$out"
for run in rewrite-1 baseline-1 rewrite-2 baseline-2 rewrite-3; do
  build=${run%-*}
  case $build in
    rewrite) def=bench/builds/rewrite.toml dir=~/sighurt-bench/builds/3c510fe0b051 ;;
    baseline) def=bench/builds/baseline.toml dir=~/sighurt-baseline ;;
  esac
  SIG_WHAT="bench: long tab churn $run" tools/heavy tools/headless-sway --renderer gles2 -- \
    bench/measure --build "$def" --build-dir "$dir" --workload tab-churn-long \
    --workloads "$out/churn.toml" --out "$out/$run.json" --raw "$out/raw/$run"
  echo "$run: $?"
done
