#!/usr/bin/env bash
# Runs the engine's offscreen test (Chromium into an in-memory target) with no display at all:
# the engine runs Chromium with no display connection. Never touches the user's desktop.
#
#   engines/sighurt-engine-cef/tests/offscreen.sh
set -euo pipefail
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"
export SIGHURT_CEF_PREFIX=${SIGHURT_CEF_PREFIX:-/data/sig-builds/deps/cef-150.0.22}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/data/sig-builds/targets/cef}
bin=$(tools/cargo test -p sighurt-engine-cef --test offscreen --no-run --message-format=json 2>/dev/null \
  | python3 -c 'import sys,json
for l in sys.stdin:
    try: m=json.loads(l)
    except Exception: continue
    if m.get("executable") and m.get("target",{}).get("name")=="offscreen": print(m["executable"])' | tail -1)
[[ -x $bin ]] || { echo "offscreen.sh: no test binary" >&2; exit 1; }
# Nothing of the user's configuration, data or profiles is read or written.
scratch=${SIG_TEST_OUT:-/data/sig-builds/test-out/cef}/offscreen
rm -rf "$scratch"; mkdir -p "$scratch"
inner=$scratch/run.sh
cat > "$inner" <<EOS
#!/usr/bin/env bash
export XDG_CONFIG_HOME=$scratch/config XDG_DATA_HOME=$scratch/data XDG_STATE_HOME=$scratch/state XDG_CACHE_HOME=$scratch/cache
"$bin" --ignored --nocapture --test-threads=1
rc=\$?
# The zygotes go with the browser process. Anything else (the on-device model utility
# included) must be gone too; give a reparented helper a moment to exit.
sleep 3
if pgrep -f '^/[^ ]*/sig-cef-helper --type=' > /dev/null; then echo "cef-offscreen: helper processes outlived the test"; pgrep -af '^/[^ ]*/sig-cef-helper --type='; rc=1; fi
exit \$rc
EOS
chmod +x "$inner"
# No display at all: Chromium runs with no display connection (ozone headless).
SIG_WHAT="cef offscreen test" SIG_MEM=${SIG_MEM:-6G} tools/heavy env -u WAYLAND_DISPLAY -u DISPLAY "$inner"
