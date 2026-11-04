#!/usr/bin/env bash
# Startup time of `sig` (bench/HYPOTHESES.md H-START*): from exec to the first chrome frame and
# to the first page frame, for one or more builds interleaved, in a private compositor
# (1280×800 at scale 1), each run with fresh XDG directories (the cache too, so nothing is warm
# but the OS page cache, which the first, unrecorded, round fills).
#
#   tools/heavy tools/headless-sway [--renderer gles2] -- bench/startup/startup.sh OUT REPS SIG...
#
# Each SIG is a `sig` with the demo engine. A run starts `sig demo://pattern` with SIG_DIAG=1
# through a wrapper that prints `sig-diag t=<CLOCK_MONOTONIC> exec` just before exec, waits for
# its first `chrome` line (the first chrome surface rendered, just before its commit), its
# first `frame engine=demo` line (the page's first frame presented) and its `startup` line
# when it prints one, then quits it with ctrl+q. Prints one line per run to OUT/runs.txt and
# the medians per build at the end. Times are milliseconds after exec.
set -u
out=$(realpath -m "${1:?usage: startup.sh OUT REPS SIG...}")
reps=${2:?usage: startup.sh OUT REPS SIG...}
shift 2
sigs=()
for s in "$@"; do sigs+=("$(realpath "$s")"); done
mkdir -p "$out"
wtype -s 3600000 ' ' &
keyboard=$!
trap 'kill $keyboard 2>/dev/null' EXIT
sleep 0.5

for rep in $(seq 0 "$reps"); do # round 0 warms the page cache and is not reported
  for i in "${!sigs[@]}"; do
    sig=${sigs[$i]}
    run=$out/b$i-$rep
    rm -rf "$run" && mkdir -p "$run/config/sighurt" "$run/state" "$run/downloads"
    cat > "$run/config/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
[session]
restore = false
CONF
    XDG_CONFIG_HOME=$run/config XDG_STATE_HOME=$run/state XDG_DOWNLOAD_DIR=$run/downloads \
      XDG_CACHE_HOME=$run/cache XDG_DATA_HOME=$run/data SIG_DIAG=1 RUST_LOG=error \
      python3 -I -c 'import os, sys, time
sys.stderr.write("sig-diag t=%.6f exec\n" % time.monotonic()); sys.stderr.flush()
os.execv(sys.argv[1], sys.argv[1:])' "$sig" "${START_URL:-demo://pattern}" > "$run/sig.log" 2>&1 &
    pid=$!
    for _ in $(seq 250); do
      grep -q ' chrome surface=' "$run/sig.log" && grep -Eq "${START_WAIT:- frame engine=demo}" "$run/sig.log" && break
      sleep 0.02
    done
    sleep 0.3
    wtype -M ctrl q -m ctrl
    for _ in $(seq 100); do kill -0 $pid 2>/dev/null || break; sleep 0.05; done
    kill $pid 2>/dev/null
    wait $pid 2>/dev/null
    [[ $rep == 0 ]] && continue
    python3 -I - "$run/sig.log" "$i" "$rep" >> "$out/runs.txt" <<'PY'
import re, sys
log, build, rep = sys.argv[1:]
t = {}
startup = ''
for line in open(log, errors='replace'):
    m = re.match(r'sig-diag t=([\d.]+) (\S+)(.*)', line)
    if not m:
        continue
    at, kind, rest = float(m[1]), m[2], m[3]
    key = kind
    if kind == 'chrome':
        surface = re.search(r'surface=(\w+)', rest)[1]
        t.setdefault('chrome_' + surface, at)
        if surface == 'page':
            t.setdefault('page', at)
    elif kind == 'frame' and 'engine=demo' in rest:
        key = 'page'
    elif kind == 'startup':
        startup = rest.strip()
    t.setdefault(key, at)
ms = lambda k: (t[k] - t['exec']) * 1e3 if k in t and 'exec' in t else float('nan')
print(f"build={build} rep={rep} first_chrome_ms={ms('chrome'):.1f} first_page_ms={ms('page'):.1f} "
      f"chrome_chrome_ms={ms('chrome_chrome'):.1f} chrome_page_ms={ms('chrome_page'):.1f} {startup}")
PY
  done
done
cat "$out/runs.txt"
python3 -I - "$out/runs.txt" "${sigs[@]}" <<'PY'
import re, statistics, sys
rows = {}
for line in open(sys.argv[1]):
    fields = dict(f.split('=', 1) for f in line.split() if '=' in f)
    rows.setdefault(fields['build'], []).append(fields)
for build, xs in sorted(rows.items()):
    print(f"build {build} ({sys.argv[2 + int(build)]}), {len(xs)} runs:")
    keys = [k for k in xs[0] if k not in ('build', 'rep')]
    for k in keys:
        try:
            v = sorted(float(x[k]) for x in xs if k in x)
        except ValueError:
            continue
        print(f"  {k}: median {statistics.median(v):.2f} min {v[0]:.2f} max {v[-1]:.2f}")
PY
