#!/usr/bin/env bash
# The chrome's renderers measured against each other (docs/rewrite/RENDER.md §12, §14.5):
# `sig` with the demo engine, once per renderer and repetition, interleaved, in a private GPU
# compositor (tools/headless-sway --renderer gles2, 1280×800 at scale 1), each run with fresh
# XDG directories. Per run: 3 s to settle, then
#   idle   10 s on a static page: sig's CPU time (utime+stime from /proc), owner-loop wakeups
#          and chrome paints (diagnostics), RSS and its peak (VmRSS, VmHWM) at the end;
#   churn  10 s of tab switches, alt-1 / alt-2 every 250 ms between a demo page and sig://home
#          (the chrome's tab strip and its page surface repaint; the page surface is shown and
#          hidden): CPU time, wakeups, chrome paints;
# and the chrome's render time per surface over the whole run, from SIG_DIAG's `chrome` lines
# (render_ms: the renderer's render call, presenting included for wgpu; scene_ms: the chrome
# drawing the scene). Prints a table; logs and diagnostics in OUT.
#
#   tools/heavy tools/headless-sway --renderer gles2 -- bench/render/chrome.sh SIG OUT [REPS]
#
# SIG is a `sig` built with `--features engine-demo,render-wgpu` (state whether release).
set -u
sig=$(realpath "${1:?usage: chrome.sh SIG OUT [REPS]}")
out=$(realpath -m "${2:?usage: chrome.sh SIG OUT [REPS]}")
reps=${3:-3}
[[ ${SIG_SWAY_RENDERER:-} == gles2 ]] || { echo "chrome.sh: run me in tools/headless-sway --renderer gles2" >&2; exit 2; }
mkdir -p "$out"
wtype -s 3600000 ' ' &
keyboard=$!
trap 'kill $keyboard 2>/dev/null' EXIT
hz=$(getconf CLK_TCK)

cpu_ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
diag() { # diag PID FILE EXPR
  local before
  before=$(stat -c %Y.%y "$2" 2>/dev/null)
  kill -USR1 "$1"
  for _ in $(seq 100); do
    [[ $(stat -c %Y.%y "$2" 2>/dev/null) != "$before" ]] && break
    sleep 0.02
  done
  sleep 0.05
  python3 -c "import json; d=json.load(open('$2')); c=d['counters']; print(*[$3], sep=', ')"
}

for rep in $(seq "$reps"); do
  for renderer in cpu wgpu; do
    run=$out/$renderer-$rep
    rm -rf "$run" && mkdir -p "$run/config/sighurt" "$run/state" "$run/downloads"
    cat > "$run/config/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
[input]
keymap = "bindings"
[keys]
"alt-1" = "tab.select 1"
"alt-2" = "tab.select 2"
"ctrl-q" = "browser.quit"
[ui]
renderer = "$renderer"
CONF
    XDG_CONFIG_HOME=$run/config XDG_STATE_HOME=$run/state XDG_DOWNLOAD_DIR=$run/downloads \
      XDG_CACHE_HOME=$run/cache XDG_DATA_HOME=$run/data SIG_DIAG=1 \
      "$sig" --diagnostics "$run/diag.json" demo://pattern sig://home > "$run/sig.log" 2>&1 &
    pid=$!
    sleep 3
    t0=$(cpu_ticks $pid); w0=$(diag $pid "$run/diag.json" "c['wakeups'], c['chrome_paints']")
    sleep 10
    t1=$(cpu_ticks $pid); w1=$(diag $pid "$run/diag.json" "c['wakeups'], c['chrome_paints']")
    rss=$(awk '/VmRSS|VmHWM/ {printf "%s ", $2}' "/proc/$pid/status")
    for i in $(seq 20); do
      wtype -M alt 2 -m alt; sleep 0.25
      wtype -M alt 1 -m alt; sleep 0.25
    done
    t2=$(cpu_ticks $pid); w2=$(diag $pid "$run/diag.json" "c['wakeups'], c['chrome_paints']")
    wtype -M ctrl q -m ctrl
    wait $pid
    echo "$renderer $rep idle_ticks=$((t1 - t0)) churn_ticks=$((t2 - t1)) hz=$hz idle=($w0)->($w1) churn=($w1)->($w2) hwm_rss_kb=($rss)" | tee -a "$out/runs.txt"
  done
done

python3 - "$out" <<'PY'
import glob, os, re, statistics, sys
out = sys.argv[1]
runs = {}
for line in open(os.path.join(out, 'runs.txt')):
    m = re.match(r'(\w+) (\d+) idle_ticks=(\d+) churn_ticks=(\d+) hz=(\d+) idle=\((\d+), (\d+)\)->\((\d+), (\d+)\) churn=\((\d+), (\d+)\)->\((\d+), (\d+)\) hwm_rss_kb=\((\d+) (\d+) \)', line)
    if not m:
        continue
    r, rep, it, ct, hz, iw0, ip0, iw1, ip1, cw0, cp0, cw1, cp1, hwm, rss = m.groups()
    hz = int(hz)
    runs.setdefault(r, []).append(dict(
        idle_cpu=int(it) / hz / 10 * 100, churn_cpu=int(ct) / hz / 10 * 100,
        # Each report request is a wakeup of its own.
        idle_wakeups=int(iw1) - int(iw0) - 1, idle_paints=int(ip1) - int(ip0),
        churn_wakeups=int(cw1) - int(cw0) - 1, churn_paints=int(cp1) - int(cp0),
        rss=int(rss) / 1024, hwm=int(hwm) / 1024))
med = lambda xs: statistics.median(xs)
print('renderer | idle CPU % | idle wakeups | idle chrome paints | churn CPU % | churn wakeups | churn chrome paints | RSS MiB | peak RSS MiB')
for r, xs in runs.items():
    f = lambda k: med([x[k] for x in xs])
    print(f"{r} | {f('idle_cpu'):.2f} | {f('idle_wakeups'):.0f} | {f('idle_paints'):.0f} | {f('churn_cpu'):.1f} | {f('churn_wakeups'):.0f} | {f('churn_paints'):.0f} | {f('rss'):.1f} | {f('hwm'):.1f}")
print()
print('renderer | surface | renders | render ms median | p95 | max | scene ms median')
for r in runs:
    per = {}
    for log in glob.glob(os.path.join(out, f'{r}-*', 'sig.log')):
        for line in open(log):
            m = re.search(r' chrome surface=(\w+) draw_ms=[\d.]+ scene_ms=([\d.]+) render_ms=([\d.]+)', line)
            if m:
                per.setdefault(m[1], []).append((float(m[3]), float(m[2])))
    for s, xs in sorted(per.items()):
        render = sorted(x[0] for x in xs)
        p95 = render[min(len(render) - 1, int(len(render) * 0.95))]
        print(f"{r} | {s} | {len(xs)} | {med(render):.3f} | {p95:.3f} | {render[-1]:.3f} | {med([x[1] for x in xs]):.3f}")
PY
