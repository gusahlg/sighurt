#!/usr/bin/env bash
# A soak test of `sig` with the demo and Wasm engines, in a private headless sway (pixman unless
# SIG_SOAK_RENDERER names another): never on the desktop, since it types. Builds `sig`
# (--no-default-features --features engine-demo,engine-wasm,frontend-tui,render-wgpu) and two
# Wasm guests with tools/cargo, then repeats a cycle for MINUTES (default 10):
#
#   tab churn     20 tabs opened over the three kept ones, across the demo pages (pattern,
#                 animation, resize, slow) and two Wasm guests (raf-demo, hello-sighurt), then
#                 closed again
#   switching     ctrl-tab through them all, alt-1..3
#   history       a page opened in the third tab, then back, forward, back; a Wasm guest from
#                 the local web server (serve.py), a visit recorded while `[history] remember`
#                 is on (odd cycles) and none while it is off; an address completed from it
#   zoom          page.zoom-in, zoom-out, zoom-reset
#   find, tabs    page.find in the first tab, find-next, find-previous, find-stop;
#                 tab.duplicate, tab.move left, the copy closed and tab.reopen'ed and closed
#   fullscreen    browser.fullscreen on and off again, the window's state following
#   output        four sizes at scales 1, 1.25, 1.5 and 2, then 1280x800 at 1
#   hide, show    another workspace shown over sig's for a second
#   configuration config.toml rewritten with another tab colour and `[history] remember`
#                 flipped; every third cycle an invalid file first, which must be reported (a
#                 notice) and kept out (the keys still work)
#   clipboard     clipboard.copy, read back with wl-paste; wl-copy, pasted into the address line
#                 and opened
#
# Each cycle ends where it started (three tabs, 1280x800 at scale 1, sig's workspace shown); a
# sample is taken there, and every 30 s besides: VmRSS, VmHWM, open descriptors, threads, and the
# diagnostics' counters (placements, placement errors, chrome paints, engine views, pending host
# events, buffer bytes of all targets, targets, the glyph cache's bytes). samples.tsv has the
# timeline, cycles.tsv the end-of-cycle samples the verdicts compare. FAIL on: sig exiting, a
# placement error, growth from the second end-of-cycle sample (the first is the warm-up: fonts,
# glyphs, the Wasm engine's compiled guests) to the last in descriptors, threads, targets, views
# or buffer bytes, RSS above the bound printed with its method, sig not quitting within 5 s of
# ctrl-q, or the saved session not coming back.
#
#   apps/sighurt-browser/tests/soak.sh OUT [MINUTES]
#   apps/sighurt-browser/tests/soak.sh --no-build OUT [MINUTES]
#
# Logs, samples, diagnostics and screenshots go to OUT. Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: soak.sh [--no-build] OUT [MINUTES]}")
  minutes=${2:-10}
  mkdir -p "$out"
  tdir=$(realpath -m "${CARGO_TARGET_DIR:-target}")
  wasm=$tdir/wasm32-unknown-unknown/release
  link="ln -f $tdir/debug/sig $tdir/debug/sig-soak"
  if [[ $build == 1 ]]; then
    SIG_WHAT="build sig (demo, wasm) for soak.sh" tools/heavy bash -c \
      "cargo build -p sighurt-browser --no-default-features \
       --features engine-demo,engine-wasm,frontend-tui,render-wgpu --bins && \
       cargo build --target wasm32-unknown-unknown --release -p raf-demo -p hello-sighurt && $link" || exit 1
  else
    $link || exit 1
  fi
  mkdir -p "$out/guests"
  cp "$wasm/raf_demo.wasm" "$wasm/hello_sighurt.wasm" "$out/guests/" || exit 1
  tools/heavy tools/headless-sway --renderer "${SIG_SOAK_RENDERER:-pixman}" -- \
    "$0" --inside "$out" "$minutes" "$tdir/debug/sig-soak"
  failed=$?
  echo "soak.sh: $failed failed check(s); samples, logs and screenshots in $out"
  exit $failed
fi

# ── Inside the private compositor ─────────────────────────────────────────────────────────
out=$2 minutes=$3 sig=$4 scenario=soak
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "soak.sh: run me through tools/headless-sway" >&2
  exit 2
}
source "$repo/apps/sighurt-browser/tests/lib.sh"
run=$out/run
rm -rf "$run" "$out"/*.png "$out"/*.tsv "$out/steps.log"
private "$run"
export RUST_LOG=${RUST_LOG:-info}
config=$run/config/sighurt/config.toml
g=$out/guests
# The web server for the history's visits: Wasm guests over http, which the Wasm engine opens
# for its origin.
python3 "$repo/apps/sighurt-browser/tests/serve.py" "$repo/apps/sighurt-browser/tests/pages" "$g" \
  "$out/port" "$out/server.log" &
server=$!
rm -f "$out/port"
for _ in $(seq 50); do [[ -s $out/port ]] && break; sleep 0.1; done
web=http://127.0.0.1:$(cat "$out/port")
printf '[routing.wasm]\norigins = ["%s"]\n' "$web" >> "$config"
cp "$config" "$out/base.toml"
pages=(demo://pattern demo://animation demo://resize demo://slow "file://$g/raf_demo.wasm" "file://$g/hello_sighurt.wasm")

# An input device that stays plugged in: each wtype command plugs in its own keyboard.
wtype -s 100000000 ' ' &
keyboard=$!

# What a sample records, and its header.
fields="t cycle rss_kib hwm_kib fds threads placements placement_errors chrome_paints views pending buffer_bytes targets glyph_bytes tabs status"
printf '%s\n' "${fields// /$'\t'}" > "$out/samples.tsv"
printf '%s\n' "${fields// /$'\t'}" > "$out/cycles.tsv"
started=$SECONDS last_sample=-100 cycle=0
# sample FILE...: one line in each FILE.
sample() {
  dg True > /dev/null
  local line
  line="$((SECONDS - started))	$cycle	$(proc "$sig_pid" | tr ' ' '\t')	$(python3 - "$run/diag.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
c, t = d['counters'], d['targets']
r = d.get('renderer') or {}
print('\t'.join(str(v) for v in [
    c['placements'], c['placement_errors'], c['chrome_paints'],
    sum(1 for x in t if x['role'] == 'view'), d['pending_host_events'],
    sum(x['buffer_bytes'] for x in t), len(t), r.get('glyph_bytes', 0), len(d['tabs']),
    d['status']]))
PY
)"
  for file in "$@"; do echo "$line" >> "$file"; done
  last_sample=$SECONDS
}
tick() { [[ $((SECONDS - last_sample)) -ge 30 ]] && sample "$out/samples.tsv"; }
alive() { kill -0 "$sig_pid" 2>/dev/null; }
tab_count() { dg "len(d['tabs'])==$1, len(d['tabs'])"; }

start_sig "$run" demo://pattern demo://animation demo://resize
shot 00-start 1
verdict "three tabs at the start" "$(tab_count 3)"
sample "$out/samples.tsv"

colours=('#7c3aed' '#2563eb')
end=$((started + minutes * 60))
while [[ $SECONDS -lt $end ]] && alive; do
  cycle=$((cycle + 1))
  step "── cycle $cycle"

  step "churn: 20 tabs"
  for i in $(seq 20); do
    page=${pages[$((i % ${#pages[@]}))]}
    wtype -M ctrl t -m ctrl -s 40 -M ctrl l -m ctrl -s 40 "$page" -k Return
    sleep 0.15
    tick
  done
  sleep 1
  [[ $cycle == 1 ]] && shot 01-churn-23-tabs 0
  verdict "cycle $cycle: 23 tabs after the churn" "$(tab_count 23)"

  step "switching"
  for _ in $(seq 25); do wtype -M ctrl -k Tab -m ctrl; sleep 0.05; done
  for i in 1 2 3 2 1; do wtype -M alt "$i" -m alt; sleep 0.1; done
  tick

  step "history in the third tab"
  key -M alt 3 -m alt
  cmd "page.open ${pages[$((cycle % 2))]}"
  sleep 0.5
  cmd page.back
  cmd page.forward
  cmd page.back
  # Odd cycles remember (the configuration step below sets the next cycle's).
  visits=$(dg "c['visits_recorded']" | cut -d' ' -f2)
  cmd "page.open $web/guests/hello_sighurt.wasm?$cycle"
  sleep 1
  if [[ $((cycle % 2)) == 1 ]]; then
    verdict "cycle $cycle: the page over http is a visit" "$(dg "c['visits_recorded']>$visits, c['visits_recorded']")"
  else
    verdict "cycle $cycle: with remember off, it is not" "$(dg "c['visits_recorded']==$visits, c['visits_recorded']")"
  fi
  key -M ctrl l -m ctrl
  key "${web#http://}"
  key -k Escape
  cmd page.back
  step "zoom"
  cmd page.zoom-in
  cmd page.zoom-out
  cmd page.zoom-reset
  tick

  step "find in the first tab"
  key -M alt 1 -m alt
  cmd "page.find demo"
  verdict "cycle $cycle: a search in the first tab" "$(dg "d['tabs'][0]['find'] is not None, d['tabs'][0]['find']")"
  cmd page.find-next
  cmd page.find-previous
  cmd page.find-stop
  verdict "cycle $cycle: and stopped" "$(dg "d['tabs'][0]['find'] is None, d['tabs'][0]['find']")"

  step "tab.duplicate, tab.move, tab.reopen (23 tabs: the churned ones are still open)"
  cmd tab.duplicate
  cmd "tab.move left"
  verdict "cycle $cycle: the copy is first" "$(dg "len(d['tabs'])==24 and d['active_tab']==1, (len(d['tabs']), d['active_tab'])")"
  key -M ctrl w -m ctrl
  cmd tab.reopen
  verdict "cycle $cycle: closed and reopened" "$(tab_count 24)"
  key -M ctrl w -m ctrl
  verdict "cycle $cycle: 23 tabs again" "$(tab_count 23)"
  tick

  step "browser.fullscreen on and off"
  cmd browser.fullscreen
  sleep 0.5
  verdict "cycle $cycle: the window is fullscreen" "$(dg "d['fullscreen']['window'], d['fullscreen']")"
  cmd browser.fullscreen
  sleep 0.5
  verdict "cycle $cycle: and is not again" "$(dg "not d['fullscreen']['window'], d['fullscreen']")"
  tick

  step "output sizes and scales"
  for scale in 1 1.25 1.5 2; do
    swaymsg -q output HEADLESS-1 scale "$scale"
    for size in 1000x700 1440x900 800x600 1280x800; do
      swaymsg -q output HEADLESS-1 resolution "$size"
      sleep 0.15
    done
  done
  swaymsg -q output HEADLESS-1 scale 1 resolution 1280x800
  [[ $cycle == 1 ]] && shot 02-after-scales 1
  tick

  step "hidden behind another workspace, then shown"
  key -M alt 2 -m alt
  swaymsg -q workspace 2
  sleep 0.3
  before=$(dg "c['page_frames_presented']" | cut -d' ' -f2)
  sleep 1
  after=$(dg "c['page_frames_presented']" | cut -d' ' -f2)
  check "cycle $cycle: the animation presents nothing while hidden" "$([[ $((after - before)) -le 1 ]]; echo $?)" \
    "$((after - before)) frames in 1 s behind another workspace"
  swaymsg -q workspace 1
  sleep 0.5
  tick

  step "configuration reloads"
  if [[ $((cycle % 3)) == 0 ]]; then
    reloads=$(dg "c['config_reloads']" | cut -d' ' -f2-)
    { cat "$out/base.toml"; printf 'bogus = 1\n'; } > "$config"
    sleep 0.7
    verdict "cycle $cycle: an invalid file is reported" \
      "$(dg "d['status']=='notice' and c['config_reloads']>$reloads, (d['status'], c['config_reloads'])")"
    key -M ctrl t -m ctrl
    verdict "cycle $cycle: and kept out: ctrl-t still opens a tab" "$(tab_count 24)"
    key -M ctrl w -m ctrl
  fi
  remember=$([[ $(((cycle + 1) % 2)) == 1 ]] && echo true || echo false)
  { cat "$out/base.toml"; printf '[ui.colors]\ntab = "%s"\n[history]\nremember = %s\n' "${colours[$((cycle % 2))]}" "$remember"; } > "$config"
  sleep 0.7
  verdict "cycle $cycle: a valid file is taken" "$(dg "d['status']!='notice', d['status']")"
  tick

  step "clipboard"
  cmd "clipboard.copy soak-$cycle"
  sleep 0.3
  got=$(timeout 5 wl-paste -n 2>&1)
  check "cycle $cycle: clipboard.copy reaches the clipboard" "$([[ $got == "soak-$cycle" ]]; echo $?)" "wl-paste: $got"
  # In the foreground, so this run stops the one it started and nothing else.
  wl-copy --foreground "demo://resize?$cycle" &
  copier=$!
  sleep 0.2
  key -M alt 1 -m alt
  key -M ctrl l -m ctrl
  key -M ctrl a -m ctrl
  key -M ctrl v -m ctrl
  sleep 0.3
  key -k Return
  sleep 1.2
  verdict "cycle $cycle: an address pasted from the clipboard opens" \
    "$(ses "$run" "url(1)=='demo://resize?$cycle', url(1)")"
  kill "$copier" 2>/dev/null
  tick

  step "close the 20 churned tabs"
  cmd "tab.select 4"
  for _ in $(seq 20); do wtype -M ctrl w -m ctrl; sleep 0.05; done
  sleep 1.5
  verdict "cycle $cycle: three tabs again" "$(tab_count 3)"
  sample "$out/samples.tsv" "$out/cycles.tsv"
done

check "sig ran the whole soak" "$(alive; echo $?)" "$cycle cycles in $((SECONDS - started)) s"
shot 03-end 0

# ── Verdicts on the end-of-cycle samples ──────────────────────────────────────────────────
python3 - "$out/cycles.tsv" > "$out/verdicts.txt" <<'PY'
import csv, sys
rows = list(csv.DictReader(open(sys.argv[1]), delimiter='\t'))
def out(name, ok, detail):
    print(0 if ok else 1, name, '|', detail)
if len(rows) < 3:
    out('at least three cycles to compare', False, f'{len(rows)} cycles')
    sys.exit()
n = lambda r, k: int(r[k])
base, last = rows[1], rows[-1]
series = lambda k: ' '.join(r[k] for r in rows)
for k in ['fds', 'threads', 'targets', 'views', 'buffer_bytes', 'pending']:
    out(f'no growth in {k} (cycle 2 to the last)', n(last, k) <= n(base, k), series(k))
out('no placement errors', all(n(r, 'placement_errors') == 0 for r in rows), series('placement_errors'))
# RSS: the first cycle warms up (fonts, glyphs, compiled guests, allocator arenas). From the
# second on, a cycle that leaks nothing ends where the one before ended, give or take what the
# allocator keeps. The bound is the second cycle's RSS plus the larger of 8 MiB and four times
# the largest change between consecutive cycles 2..4 (the noise the run itself shows), and the
# last third of the samples must not all lie above the middle third's maximum (a steady climb).
rss = [n(r, 'rss_kib') for r in rows]
noise = max(abs(a - b) for a, b in zip(rss[1:4], rss[2:5])) if len(rss) > 2 else 0
bound = rss[1] + max(8 * 1024, 4 * noise)
out('RSS stays within the bound', max(rss[1:]) <= bound,
    f'RSS KiB per cycle: {" ".join(map(str, rss))}; bound {bound} = cycle 2 + max(8 MiB, 4 x {noise})')
third = max(1, len(rss) // 3)
if len(rss) >= 6:
    middle, final = rss[-2 * third:-third], rss[-third:]
    out('RSS does not climb steadily', min(final) <= max(middle),
        f'last third {final}, middle third {middle}')
# sighurt-render's GlyphCache admits glyphs only below MAX_GLYPH_BYTES (8 MiB); one glyph may
# pass it.
glyphs = [n(r, 'glyph_bytes') for r in rows]
out('the glyph cache stays within its bound', max(glyphs) <= 8 * 1024 * 1024 + 400 * 400, ' '.join(map(str, glyphs)))
PY
while IFS= read -r line; do
  verdict_status=${line%% *}
  rest=${line#* }
  check "${rest%% |*}" "$verdict_status" "${rest#*| }"
done < "$out/verdicts.txt"
dg True > /dev/null
cp "$run/diag.json" "$out/diag-end.json"
python3 -c "import json; d=json.load(open('$out/diag-end.json')); p=d['peaks']; print('peaks:', {k: p[k] for k in ['max_target_buffers','max_retiring_generations','max_buffer_bytes','max_chrome_buffers','max_host_queue']})" >> "$out/steps.log"
verdict "buffers stayed bounded (peaks)" \
  "$(dg "p['max_target_buffers']<=9 and p['max_retiring_generations']<=2, (p['max_target_buffers'], p['max_retiring_generations'], p['max_buffer_bytes'])")"

step "ctrl-q"
key -M ctrl q -m ctrl
exited "$sig_pid" 5; code=$status
check "ctrl-q quits within 5 s, cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
mv "$run/sig.log" "$out/sig-soak.log"
verdict "the session file holds the three tabs" "$(ses "$run" "len(s['tabs'])==3, len(s['tabs'])")"

step "start again: the session comes back"
start_sig "$run"
verdict "the restored session has the three tabs" "$(tab_count 3)"
shot 04-restored 0
key -M ctrl q -m ctrl
exited "$sig_pid" 5; code=$status
check "and quits cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
kill "$keyboard" "$server" 2>/dev/null
grep -l panicked "$out"/sig-soak.log "$run/sig.log" > /dev/null 2>&1 && check "no panic" 1 "see the logs"
cp "$run/sig.log" "$out/sig-restore.log"
cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null
exit $failures
