#!/usr/bin/env bash
# Memory over tab churn (bench/HYPOTHESES.md H-MEM*): `sig` with the demo and Wasm engines opens
# and closes tabs CYCLES times, and after cycles 1, 5, 10, 20, ... its RSS, the SHM buffer bytes
# its targets hold and the chrome renderer's glyph cache are recorded. A cycle: tab.new, open
# demo://animation, tab.new, open the Wasm guest WASM (an animated one: frames keep coming), wait
# 1 s, close both tabs, wait 0.5 s. Fresh XDG directories per run; the first tab (demo://pattern)
# stays. Run in a private compositor:
#
#   tools/heavy tools/headless-sway -- bench/memory/churn.sh SIG WASM OUT [REPS] [CYCLES]
#
# SIG is a `sig` built with --features engine-demo,engine-wasm. Prints one line per sample.
set -u
sig=$(realpath "${1:?usage: churn.sh SIG WASM OUT [REPS] [CYCLES]}")
wasm=$(realpath "${2:?usage: churn.sh SIG WASM OUT [REPS] [CYCLES]}")
out=$(realpath -m "${3:?usage: churn.sh SIG WASM OUT [REPS] [CYCLES]}")
reps=${4:-3}
cycles=${5:-20}
mkdir -p "$out"
wtype -s 3600000 ' ' &
keyboard=$!
trap 'kill $keyboard 2>/dev/null' EXIT
key() { wtype -M alt "$1" -m alt; sleep 0.25; }

sample() { # sample PID RUN CYCLE
  local before
  before=$(stat -c %Y.%y "$2/diag.json" 2>/dev/null)
  kill -USR1 "$1"
  for _ in $(seq 100); do
    [[ $(stat -c %Y.%y "$2/diag.json" 2>/dev/null) != "$before" ]] && break
    sleep 0.02
  done
  sleep 0.05
  local rss
  rss=$(awk '/VmRSS/ {print $2}' "/proc/$1/status")
  python3 -I - "$2/diag.json" "$3" "$rss" "$(basename "$2")" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
targets = d.get('targets', [])
buffers = sum(t.get('buffer_bytes', 0) for t in targets)
r = d.get('renderer') or {}
print(f"{sys.argv[4]} cycle={sys.argv[2]} rss_kb={sys.argv[3]} targets={len(targets)} "
      f"buffer_bytes={buffers} glyphs={r.get('glyphs', 'na')} glyph_bytes={r.get('glyph_bytes', 'na')} "
      f"tabs={len(d.get('tabs', []))}")
PY
}

for rep in $(seq "$reps"); do
  run=$out/run-$rep
  rm -rf "$run" && mkdir -p "$run/config/sighurt" "$run/state" "$run/downloads"
  cat > "$run/config/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
[session]
restore = false
[input]
keymap = "bindings"
[keys]
"alt-n" = "tab.new"
"alt-a" = "page.open demo://animation"
"alt-w" = "page.open file://$wasm"
"alt-x" = "tab.close"
"ctrl-q" = "browser.quit"
CONF
  XDG_CONFIG_HOME=$run/config XDG_STATE_HOME=$run/state XDG_DOWNLOAD_DIR=$run/downloads \
    XDG_CACHE_HOME=$run/cache XDG_DATA_HOME=$run/data RUST_LOG=error \
    "$sig" --diagnostics "$run/diag.json" demo://pattern > "$run/sig.log" 2>&1 &
  pid=$!
  sleep 2
  sample $pid "$run" 0 | tee -a "$out/samples.txt"
  for cycle in $(seq "$cycles"); do
    key n; key a; key n; key w
    sleep 1
    key x; key x
    sleep 0.5
    case $cycle in 1|5|10|20|30|40|50|60) sample $pid "$run" "$cycle" | tee -a "$out/samples.txt" ;; esac
  done
  wtype -M ctrl q -m ctrl
  for _ in $(seq 100); do kill -0 $pid 2>/dev/null || break; sleep 0.05; done
  kill $pid 2>/dev/null
  wait $pid 2>/dev/null
done
