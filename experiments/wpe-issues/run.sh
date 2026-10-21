#!/usr/bin/env bash
# run.sh NAME URL [VAR=VALUE...]: one run in a private compositor and bus (take the heavy lock outside:
# tools/heavy experiments/wpe-issues/run.sh ...).
here=$(cd "$(dirname "$0")" && pwd)
exec "$here/../../tools/headless-sway" --renderer gles2 -- "$here/../../engines/sighurt-engine-wpe/tests/private-bus" "$here/inner.sh" "$@"
