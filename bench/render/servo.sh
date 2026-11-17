#!/usr/bin/env bash
# The chrome's renderers beside Servo (docs/rewrite/RENDER.md §16): `sig` with Servo, the demo
# engine and the GPU renderers in one process, once per renderer and repetition, in a private
# GPU compositor (tools/headless-sway --renderer gles2, 1280×800 at scale 1), with fresh XDG
# directories. Servo shows a page that animates (CSS and requestAnimationFrame), so Servo's GL
# presents into its subsurface at 60 Hz while the chrome's surfaces are drawn by the renderer
# under test. Per run: 5 s to settle, then
#   animate 10 s on the animating page: sig's CPU time, owner-loop wakeups and chrome paints
#           (the chrome must not repaint for a page's animation: plan §26), RSS at the end,
#           resident memory per mapping (smaps.txt) and each thread's CPU ticks (threads-*.txt);
#   churn   10 s of tab switches every 250 ms between the animating page, sig://home and a
#           demo page: CPU time, wakeups, chrome paints;
#   resize  a storm of 12 output sizes 80 ms apart, a 1280×80 window (no room for the page) and
#           back, scale 1.5 and back to 1;
# then a clean quit, and its log searched for renderer failures, fallbacks, validation errors
# and panics. CHROME_RENDERERS ("cpu wgpu vulkan" by default) picks the renderers; with
# SIG_VULKAN_VALIDATION=1 the GPU renderers run with validation (their RSS is then not the
# renderer's). Prints a table and PASS or FAIL per run; logs in OUT.
#
#   tools/heavy tools/headless-sway --renderer gles2 -- bench/render/servo.sh SIG OUT [REPS]
#
# SIG is a `sig` built with `--features frontend-gui,engine-servo,engine-demo,render-wgpu,render-vulkan`.
set -u
sig=$(realpath "${1:?usage: servo.sh SIG OUT [REPS]}")
out=$(realpath -m "${2:?usage: servo.sh SIG OUT [REPS]}")
reps=${3:-3}
[[ ${SIG_SWAY_RENDERER:-} == gles2 ]] || { echo "servo.sh: run me in tools/headless-sway --renderer gles2" >&2; exit 2; }
mkdir -p "$out"
wtype -s 3600000 ' ' &
keyboard=$!
trap 'kill $keyboard 2>/dev/null' EXIT
hz=$(getconf CLK_TCK)
cat > "$out/animate.html" <<'HTML'
<!doctype html>
<title>Animating</title>
<style>
  body { margin: 0; background: #20242c; color: #e8e8e8; font: 16px sans-serif; }
  .box { width: 120px; height: 120px; margin: 80px; background: #5b9bd5;
         animation: spin 2s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }
</style>
<div class="box"></div>
<p id="n">0</p>
<script>
  let n = 0;
  function tick() { document.getElementById('n').textContent = ++n; requestAnimationFrame(tick); }
  requestAnimationFrame(tick);
</script>
HTML

cpu_ticks() { awk '{print $14 + $15}' "/proc/$1/stat"; }
threads() { for task in /proc/$1/task/*; do echo "$(sed 's/.*) //' "$task/stat" | awk '{print $12 + $13}') $(cat "$task/comm")"; done; }
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

failed=0
for rep in $(seq "$reps"); do
  for renderer in ${CHROME_RENDERERS:-cpu wgpu vulkan}; do
    run=$out/$renderer-$rep
    rm -rf "$run" && mkdir -p "$run/config/sighurt" "$run/state" "$run/downloads"
    cat > "$run/config/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "servo"
[routing.demo]
schemes = ["demo"]
[input]
keymap = "bindings"
[keys]
"alt-1" = "tab.select 1"
"alt-2" = "tab.select 2"
"alt-3" = "tab.select 3"
"ctrl-q" = "browser.quit"
[ui]
renderer = "$renderer"
CONF
    swaymsg -q output HEADLESS-1 resolution 1280x800 scale 1
    XDG_CONFIG_HOME=$run/config XDG_STATE_HOME=$run/state XDG_DOWNLOAD_DIR=$run/downloads \
      XDG_CACHE_HOME=$run/cache XDG_DATA_HOME=$run/data SIG_DIAG=1 RUST_LOG=${RUST_LOG:-info} \
      "$sig" --diagnostics "$run/diag.json" "file://$out/animate.html" sig://home demo://pattern \
      > "$run/sig.log" 2>&1 &
    pid=$!
    sleep 5
    threads $pid > "$run/threads-start.txt"
    t0=$(cpu_ticks $pid); w0=$(diag $pid "$run/diag.json" "c['wakeups'], c['chrome_paints']")
    sleep 10
    t1=$(cpu_ticks $pid); w1=$(diag $pid "$run/diag.json" "c['wakeups'], c['chrome_paints']")
    threads $pid > "$run/threads-end.txt"
    rss=$(awk '/VmRSS|VmHWM/ {printf "%s ", $2}' "/proc/$pid/status")
    awk '/^[0-9a-f]+-[0-9a-f]+ / {name = (NF >= 6 ? $6 : "[anon]")} /^Rss:/ {rss[name] += $2}
      END {for (n in rss) if (rss[n] > 0) printf "%8d %s\n", rss[n], n}' "/proc/$pid/smaps" \
      | sort -rn > "$run/smaps.txt"
    for i in $(seq 13); do
      wtype -M alt 2 -m alt; sleep 0.25
      wtype -M alt 3 -m alt; sleep 0.25
      wtype -M alt 1 -m alt; sleep 0.25
    done
    t2=$(cpu_ticks $pid); w2=$(diag $pid "$run/diag.json" "c['wakeups'], c['chrome_paints']")
    for i in $(seq 12); do
      swaymsg -q output HEADLESS-1 resolution $((880 + i * 33))x$((560 + i * 20)); sleep 0.08
    done
    swaymsg -q output HEADLESS-1 resolution 1280x80; sleep 0.5
    swaymsg -q output HEADLESS-1 resolution 1280x800; sleep 0.5
    wtype -M alt 2 -m alt
    swaymsg -q output HEADLESS-1 scale 1.5; sleep 1
    swaymsg -q output HEADLESS-1 scale 1; sleep 1
    wtype -M alt 1 -m alt; sleep 1
    report=$(diag $pid "$run/diag.json" "d['renderer']['name'], d['renderer']['frames'], d['renderer']['fallbacks'], c['chrome_deferred']")
    wtype -M ctrl q -m ctrl
    for _ in $(seq 150); do kill -0 $pid 2>/dev/null || break; sleep 0.1; done
    if kill -0 $pid 2>/dev/null; then kill -9 $pid; code=timeout; else wait $pid; code=$?; fi
    problems=$(grep -ciE "rendering the chrome's|chrome-renderer-fallback|validation error|VUID-|panicked" "$run/sig.log")
    verdict=PASS
    fallbacks=$(echo "$report" | cut -d, -f3 | tr -d ' ')
    [[ $code == 0 && $problems == 0 && $report == "$renderer, "* && $fallbacks == 0 ]] || { verdict=FAIL; failed=$((failed + 1)); }
    echo "$renderer $rep anim_ticks=$((t1 - t0)) churn_ticks=$((t2 - t1)) hz=$hz anim=($w0)->($w1) churn=($w1)->($w2) hwm_rss_kb=($rss) report=($report) exit=$code problems=$problems $verdict" | tee -a "$out/runs.txt"
  done
done

python3 - "$out" <<'PY'
import os, re, statistics, sys
out = sys.argv[1]
runs = {}
for line in open(os.path.join(out, 'runs.txt')):
    m = re.match(r'(\w+) (\d+) anim_ticks=(\d+) churn_ticks=(\d+) hz=(\d+) anim=\((\d+), (\d+)\)->\((\d+), (\d+)\) churn=\((\d+), (\d+)\)->\((\d+), (\d+)\) hwm_rss_kb=\((\d+) (\d+) \)', line)
    if not m:
        continue
    r, rep, at, ct, hz, aw0, ap0, aw1, ap1, cw0, cp0, cw1, cp1, hwm, rss = m.groups()
    hz = int(hz)
    runs.setdefault(r, []).append(dict(
        anim_cpu=int(at) / hz / 10 * 100, churn_cpu=int(ct) / hz / 10 * 100,
        anim_wakeups=int(aw1) - int(aw0) - 1, anim_paints=int(ap1) - int(ap0),
        churn_wakeups=int(cw1) - int(cw0) - 1, churn_paints=int(cp1) - int(cp0),
        rss=int(rss) / 1024, hwm=int(hwm) / 1024))
med = lambda xs: statistics.median(xs)
print('renderer | animate CPU % | animate wakeups | animate chrome paints | churn CPU % | churn wakeups | churn chrome paints | RSS MiB | peak RSS MiB')
for r, xs in runs.items():
    f = lambda k: med([x[k] for x in xs])
    print(f"{r} | {f('anim_cpu'):.1f} | {f('anim_wakeups'):.0f} | {f('anim_paints'):.0f} | {f('churn_cpu'):.1f} | {f('churn_wakeups'):.0f} | {f('churn_paints'):.0f} | {f('rss'):.1f} | {f('hwm'):.1f}")
PY
echo "servo.sh: $failed failed run(s)"
exit $failed
