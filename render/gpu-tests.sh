#!/usr/bin/env bash
# The chrome's GPU renderers' tests (docs/rewrite/RENDER.md §12, §16): sighurt-render-wgpu's and
# sighurt-render-vulkan's comparisons with the CPU backend, on this machine's Vulkan driver with
# the Khronos validation layer on (where installed: the Vulkan backend's runs then fail on any
# validation message), and with SIG_LAVAPIPE=1 once more on Mesa's software driver alone
# (lavapipe, from shell.nix's mesa: $SIGHURT_LAVAPIPE_ICD), so Mesa's code is tested too. With
# SIG_COST=1 also the backends' cost probes (tests/cost.rs: memory, threads and time stage by
# stage), on each driver. Opens the GPU: run it through tools/heavy, on a machine whose desktop
# nobody is using (CLAUDE.md, machine rules). Prints PASS or FAIL per pass; exits with the
# number of failures.
#
#   tools/heavy render/gpu-tests.sh [cargo test arguments, e.g. --release]
set -u
cd "$(dirname "$0")/.."
failed=0
pass() { # pass NAME ENV... -- CARGO ARGS...
  local name=$1 envs=()
  shift
  while [[ $1 != -- ]]; do envs+=("$1"); shift; done
  shift
  echo "── $name"
  if env "${envs[@]}" cargo test "$@" 2>&1 | tee "/tmp/gpu-tests-$$.log" | grep -E '^on |skipped|notice|corner pixels|^cost |test result|panicked|FAILED|VUID|Validation Error'; then :; fi
  if grep -q 'test result: FAILED\|panicked\|error: test failed\|could not compile' "/tmp/gpu-tests-$$.log" \
    || grep -q 'Validation Error\|VUID-' "/tmp/gpu-tests-$$.log"; then
    echo "FAIL $name"
    failed=$((failed + 1))
  else
    echo "PASS $name"
  fi
  rm -f "/tmp/gpu-tests-$$.log"
}
crates=(-p sighurt-render-wgpu -p sighurt-render-vulkan)
pass "the machine's driver, validation on" SIG_VULKAN_VALIDATION=1 -- "${crates[@]}" "$@" -- --nocapture
if [[ ${SIG_COST:-} == 1 ]]; then
  pass "cost on the machine's driver" -- "${crates[@]}" --test cost "$@" -- --ignored --nocapture --test-threads 1
fi
if [[ ${SIG_LAVAPIPE:-} == 1 ]]; then
  if [[ -f ${SIGHURT_LAVAPIPE_ICD:-} ]]; then
    lvp=(VK_DRIVER_FILES="$SIGHURT_LAVAPIPE_ICD" VK_ICD_FILENAMES="$SIGHURT_LAVAPIPE_ICD")
    pass "lavapipe, validation on" "${lvp[@]}" SIG_VULKAN_VALIDATION=1 -- "${crates[@]}" "$@" -- --nocapture
    if [[ ${SIG_COST:-} == 1 ]]; then
      pass "cost on lavapipe" "${lvp[@]}" -- "${crates[@]}" --test cost "$@" -- --ignored --nocapture --test-threads 1
    fi
  else
    echo "FAIL lavapipe: SIGHURT_LAVAPIPE_ICD is '${SIGHURT_LAVAPIPE_ICD:-}' (shell.nix sets it)"
    failed=$((failed + 1))
  fi
fi
echo "gpu-tests.sh: $failed failed pass(es)"
exit $failed
