#!/usr/bin/env bash
# End-to-end tests of `sig` with its default engines, Servo and Wasm, in a private headless sway
# with the GLES2 renderer (the GPU): never on the desktop, since they type and click. Builds
# `sig` (default features), the virtual-pointer helper and the example guests with tools/cargo,
# then runs the scenarios below inside `tools/headless-sway --renderer gles2`, printing PASS or
# FAIL for each check. A local web server (`serve.py`) serves `tests/pages` and the guests;
# the test guests in `tests/guests` are WAT, which Wasmtime compiles as it does binaries. Only
# https://example.com/ needs the network. Servo builds here only when the desktop is free
# (`tools/heavy` refuses it otherwise); then run it on louise-pc through tools/remote.
#
# The checks read what `sig` itself says: its diagnostics (which tab and view are shown, each
# tab's loading and history state, frame and readback counters, with Servo's GL reads audited:
# SIG_AUDIT_GL=1), its session file (the address and history segments of each tab, where the
# test pages report what they received through their address), its log, the server's log, and
# screenshot pixels. Screenshots, logs and diagnostics go to OUT; look at the screenshots too.
#
#   sighurt-browser/tests/engines.sh OUT                  build, then run every scenario
#   sighurt-browser/tests/engines.sh --no-build OUT       run with what is in target/
#   sighurt-browser/tests/engines.sh OUT servo cross      only these scenarios
#
# Scenarios, each with a fresh configuration, session and profile:
#   servo   M4 at browser level: local pages, https://example.com/ and a WebGL page; open, close
#           and switch tabs; text input (å, ä, ö, a dead key) and the clipboard both ways;
#           pointer positions and page size at scales 1, 1.25, 1.5 and 2; a wheel notch; back
#           and forward through same-document entries; reload; stop; idle and animation
#           without chrome repaints; zero readbacks; a clean quit with Servo shut down
#   wasm    M5 at browser level: hello-sighurt, gradient-demo, raf-demo, timer-demo and
#           events-demo run and take input; a guest looping in on_frame and one calling the host
#           in a loop are stopped while the chrome keeps working; invalid drawing fails safely;
#           busy guests don't hold up typing in the address bar
#   cross   one tab across the engine boundary: a Servo page, a Wasm app, back and forward
#           through both engines' entries, a third segment, and the segments restored after a
#           restart
#
# Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
guests=(hello-sighurt gradient-demo raf-demo timer-demo events-demo)

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: engines.sh [--no-build] OUT [SCENARIO ...]}")
  shift
  scenarios=("$@")
  [[ ${#scenarios[@]} == 0 ]] && scenarios=(servo wasm cross)
  mkdir -p "$out"
  if [[ $build == 1 ]]; then
    SIG_WHAT="servo build: sig (default features) for engines.sh" tools/cargo build \
      -p sighurt-browser --bins --example virtual-pointer || exit 1
    SIG_WHAT="build the example guests for engines.sh" tools/cargo build \
      --target wasm32-unknown-unknown --release "${guests[@]/#/-p}" || exit 1
  fi
  # Other builds replace target/debug/sig (the demo engine's tests build it with other
  # features); this link keeps the binary these runs use.
  ln -f target/debug/sig target/debug/sig-engines-e2e || exit 1
  failed=0
  for scenario in "${scenarios[@]}"; do
    tools/heavy tools/headless-sway --renderer gles2 -- "$0" --inside "$out/$scenario" "$scenario"
    failed=$((failed + $?))
  done
  echo "engines.sh: $failed failed check(s); screenshots and logs in $out"
  exit $failed
fi

# ── Inside the private compositor ─────────────────────────────────────────────────────────
out=$2 scenario=$3
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "engines.sh: run me through tools/headless-sway" >&2
  exit 2
}
rm -rf "$out"
mkdir -p "$out/config/sighurt" "$out/state" "$out/data" "$out/cache" "$out/downloads" "$out/guests"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_DATA_HOME=$out/data
export XDG_CACHE_HOME=$out/cache XDG_DOWNLOAD_DIR=$out/downloads
export SIG_DIAG=1 SIG_AUDIT_GL=1 RUST_LOG=${RUST_LOG:-warn}
export PATH="$(nix-build '<nixpkgs>' -A wl-clipboard --no-out-link 2>/dev/null)/bin:$PATH"
sig=target/debug/sig-engines-e2e
probe=sighurt-browser/tests/probe.py
session=$out/state/sighurt/session.toml
# The web server, which also serves the guests (/guests/NAME).
python3 sighurt-browser/tests/serve.py sighurt-browser/tests/pages "$out/guests" "$out/port" \
  "$out/server.log" &
server_pid=$!
for _ in $(seq 50); do [[ -s $out/port ]] && break; sleep 0.1; done
base=http://127.0.0.1:$(cat "$out/port")
served() { grep -c " GET $1\$" "$out/server.log"; }
cat > "$out/config/sighurt/config.toml" <<CONF
version = 1
[routing.wasm]
origins = ["$base"]
[keys]
"ctrl-t" = "tab.new"
"ctrl-w" = "tab.close"
"ctrl-tab" = "tab.next"
"alt-1" = "tab.select 1"
"alt-2" = "tab.select 2"
"alt-3" = "tab.select 3"
"alt-4" = "tab.select 4"
"alt-5" = "tab.select 5"
"alt-6" = "tab.select 6"
"alt-7" = "tab.select 7"
"alt-8" = "tab.select 8"
"alt-9" = "tab.select 9"
"ctrl-l" = "location.focus"
"f5" = "page.reload"
"f7" = "page.stop"
"alt-left" = "page.back"
"alt-right" = "page.forward"
"ctrl-q" = "browser.quit"
CONF
# The example guests, and the test guests in WAT, which Wasmtime compiles as it does binaries.
for name in "${guests[@]}"; do
  cp "target/wasm32-unknown-unknown/release/${name//-/_}.wasm" "$out/guests/$name.wasm"
done
for wat in sighurt-browser/tests/guests/*.wat; do
  cp "$wat" "$out/guests/$(basename "$wat" .wat).wasm"
done

failures=0
# check NAME ANSWER: ANSWER is "0 detail" (pass) or "1 detail" (fail), as probe.py prints.
check() {
  if [[ $2 == 0* ]]; then echo "PASS $scenario: $1 (${2:2})"; else
    echo "FAIL $scenario: $1 (${2:2})"; failures=$((failures + 1)); fi
}
# answer STATUS DETAIL: an answer from a shell test's exit status.
answer() { if [[ $1 == 0 ]]; then echo "0 $2"; else echo "1 $2"; fi; }
info() { echo "INFO $scenario: $*"; }
step() { echo "[$(date +%T.%3N)] $*" >> "$out/steps.log"; }
shot() { sleep "${2:-0.5}"; grim "$out/$1.png"; step "screenshot $1"; }
now() { python3 -c 'import time; print(f"{time.monotonic():.6f}")'; }

# Input devices that stay plugged in (wtype and wlrctl plug in a new device per command, so
# sig could miss events sent before it has bound the new wl_keyboard or wl_pointer).
wtype -s 3600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
target/debug/examples/virtual-pointer < "$XDG_RUNTIME_DIR/pointer" &
pointer_pid=$!
exec 3> "$XDG_RUNTIME_DIR/pointer"
layout_w=1280 layout_h=800
# The chrome above the page is 84 logical pixels tall at every scale.
chrome_h=84
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
click() { point "$1" "$2"; sleep 0.1; echo click >&3; sleep 0.2; }
key() { wtype "$@"; sleep 0.25; }
# open URL: types URL into the address bar of the active tab and opens it.
open() { key -M ctrl l -m ctrl; key "$1"; key -k Return; }
scale() {
  swaymsg -q output HEADLESS-1 scale "$1"
  layout_w=$(python3 -c "print(round(1280 / $1))") layout_h=$(python3 -c "print(round(800 / $1))")
}

sig_pid=
start_sig() {
  "$sig" --diagnostics "$out/diag.json" "$@" >> "$out/sig.log" 2>&1 &
  sig_pid=$!
  sleep 2
}
# until_ok SECONDS PROBE-ARGS...: asks probe.py every 0.25 s until its verdict holds, for at
# most SECONDS; prints its last answer.
until_ok() {
  local end=$((SECONDS + $1)) reply
  shift
  while :; do
    reply=$(python3 "$probe" "$@" 2>&1)
    [[ $reply == 0* || $SECONDS -ge $end ]] && break
    sleep 0.25
  done
  echo "$reply"
}
sess() { until_ok "$1" session "$session" "$2"; }
dg() { until_ok "$1" diag "$sig_pid" "$out/diag.json" "$2"; }
# value EXPR: a diagnostics value now.
value() { local r; r=$(python3 "$probe" diag "$sig_pid" "$out/diag.json" "True, $1"); echo "${r:2}"; }
# shows_tab N [ENGINE]: tab N is active and its view, of ENGINE if given, the one shown.
shows_tab() {
  dg 5 "d['active_tab']==$1 and shown is not None and shown['tab']==$1 and ('${2:-}' in ('', shown['engine'])), (d['active_tab'], shown and (shown['tab'], shown['engine']))"
}
# counted SECONDS: the counters' growth over SECONDS, as "frames chrome_paints wakeups".
counted() {
  local before after
  before=$(value "(c['page_frames_presented'], c['chrome_paints'], c['wakeups'])")
  sleep "$1"
  after=$(value "(c['page_frames_presented'], c['chrome_paints'], c['wakeups'])")
  python3 -c "a=$before; b=$after; print(*(y - x for x, y in zip(a, b)))"
}
# typing: types eight letters into the address bar one by one, each timed from just before wtype
# starts until sig's next paint of the chrome strip (so wtype's own start is included), then
# Escape; prints "median max lost" in milliseconds.
typing() {
  local times=() t0 ch
  key -M ctrl l -m ctrl
  sleep 0.3
  for ch in a b c d e f g h; do
    t0=$(now)
    wtype "$ch"
    sleep 0.3
    times+=("$(python3 "$probe" after "$out/sig.log" "$t0" 'chrome surface=chrome')")
  done
  key -k Escape
  python3 -c 'import statistics, sys
v = [float(x) for x in sys.argv[1:] if x != "none"]
print(f"{statistics.median(v) if v else 0:.1f} {max(v, default=0):.1f} {len(sys.argv) - 1 - len(v)}")' "${times[@]}"
}
# colour NAME X Y: takes screenshot NAME and prints its pixel at output (X, Y), as "r g b".
colour() {
  grim -t ppm "$out/$1.ppm"
  python3 "$probe" png "$out/$1.ppm" "$out/$1.png"
  python3 "$probe" pixel "$out/$1.ppm" "$2" "$3"
}
finish() {
  exec 3>&-
  kill "$keyboard" "$pointer_pid" "$server_pid" 2>/dev/null
  wait "$keyboard" "$pointer_pid" "$server_pid" 2>/dev/null
  grep -q 'panicked' "$out/sig.log" && check "no panic" "1 see sig.log"
  cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null
  exit $failures
}
check "the compositor renders with the GPU" "$(answer "$([[ $SIG_SWAY_RENDERER == gles2 ]]; echo $?)" "sway's renderer: $SIG_SWAY_RENDERER")"

case $scenario in
servo)
  step "two tabs: a local page, and a WebGL page in the background"
  start_sig "$base/input.html" "$base/animated.html"
  check "both pages commit in Servo" "$(sess 30 "url(1).startswith('$base/input.html#') and url(2)=='$base/animated.html', [url(i) for i in (1, 2)]")"
  check "two Servo views, the first shown" "$(dg 10 "len(views)==2 and all(v['engine']=='servo' for v in views) and shown and shown['tab']==1, [(v['tab'], v['engine'], v['shown']) for v in views]")"
  shot 01-input-page 1
  check "Servo renders on the GPU, not a software fallback" \
    "$(answer "$(grep -q 'renderer engine=servo mode=hardware gl=' "$out/sig.log"; echo $?)" "$(grep -o 'renderer engine=servo.*' "$out/sig.log")")"
  check "only the first tab reports a navigation time" \
    "$(answer "$([[ $(grep -c ' first-frame engine=servo' "$out/sig.log") == 1 ]]; echo $?)" "$(grep -o 'first-frame .*' "$out/sig.log" | tr '\n' ';')")"
  step "idle, with the WebGL page animating in a background tab it never showed in"
  point 1200 790
  sleep 1.5
  read -r frames paints wakeups <<< "$(counted 3)"
  check "a page animating in a background tab leaves sig asleep" "$(answer "$([[ $wakeups -le 1 && $frames == 0 && $paints == 0 ]]; echo $?)" \
    "$wakeups wakeups (the report's own included), $frames page frames, $paints chrome paints in 3 s")"

  step "alt-2: the WebGL page animates with no chrome repaint"
  key -M alt 2 -m alt
  check "alt-2 shows the second tab" "$(shows_tab 2 servo)"
  sleep 1
  read -r frames paints _ <<< "$(counted 2)"
  check "the WebGL page animates at the compositor's pace" "$(answer "$([[ $frames -ge 60 ]]; echo $?)" "$frames page frames in 2 s")"
  check "page animation repaints no chrome" "$(answer "$([[ $paints == 0 ]]; echo $?)" "$paints chrome paints in 2 s")"
  # Opened hidden before the window had its size, the page must still be laid out at the page
  # area's: its background (#f4f6fb) reaches the right and bottom of the page area.
  corner=$(colour 02-webgl 1250 760)
  check "the page opened in the background fills the page area" "$(answer "$([[ $corner == '244 246 251' ]]; echo $?)" "pixel at window (1250, 760): $corner")"
  key -M alt 1 -m alt
  check "alt-1 shows the first tab" "$(shows_tab 1 servo)"
  step "idle on the static page, the WebGL page hidden again"
  point 1200 790
  sleep 1.5
  read -r frames paints wakeups <<< "$(counted 3)"
  check "a static page leaves sig asleep" "$(answer "$([[ $wakeups -le 1 && $frames == 0 && $paints == 0 ]]; echo $?)" \
    "$wakeups wakeups (the report's own included), $frames page frames, $paints chrome paints in 3 s")"

  step "ctrl-t, https://example.com/, Enter"
  key -M ctrl t -m ctrl
  check "ctrl-t opens a third tab" "$(dg 5 "len(tabs)==3 and d['active_tab']==3, (len(tabs), d['active_tab'])")"
  key https://example.com/
  key -k Return
  check "a web page typed into the new tab opens in Servo" "$(sess 20 "url(3)=='https://example.com/', url(3)")"
  check "the new tab shows its Servo view" "$(shows_tab 3 servo)"
  shot 03-example-com 2
  step "ctrl-t, second.html, Enter; then ctrl-w"
  key -M ctrl t -m ctrl
  key "$base/second.html"
  key -k Return
  check "a fourth tab opens second.html" "$(sess 15 "url(4)=='$base/second.html', url(4)")"
  check "the fourth tab shows its Servo view" "$(shows_tab 4 servo)"
  shot 04-new-tab
  key -M ctrl w -m ctrl
  check "ctrl-w closes it and its view" "$(dg 5 "len(tabs)==3 and len(views)==3 and d['active_tab']==3, (len(tabs), len(views), d['active_tab'])")"
  check "and the session forgets it" "$(sess 5 "len(s['tabs'])==3, [url(i) for i in range(1, len(s['tabs']) + 1)]")"

  step "the tab strip: a click on the second tab, ctrl-tab to the third, alt-1 to the first"
  click 330 20
  check "a click shows the second tab" "$(shows_tab 2 servo)"
  key -M ctrl -k Tab -m ctrl
  check "ctrl-tab shows the third tab" "$(shows_tab 3 servo)"
  key -M alt 1 -m alt
  check "alt-1 shows the first tab" "$(shows_tab 1 servo)"

  step "text and the clipboard in the first tab's field"
  key -M alt 1 -m alt
  click 100 $((chrome_h + 100))
  check "a click at page (100, 100) reaches the page there" "$(sess 8 "'mousedown 100,100' in url(1), url(1).split('#')[1]")"
  click 100 $((chrome_h + 215))
  key 'åäö' -k dead_diaeresis -k a
  check "åäö and a dead key typed into a field" "$(sess 8 "url(1).endswith('#field åäöä'), url(1).split('#')[1]")"
  rm -f "$out/copied.txt"
  key -M ctrl a -m ctrl
  key -M ctrl c -m ctrl
  sleep 0.5
  wl-paste -n > "$out/copied.txt" 2>&1
  check "ctrl-c in the page puts its selection on the Wayland clipboard" \
    "$(answer "$([[ $(cat "$out/copied.txt") == åäöä ]]; echo $?)" "wl-paste: $(cat "$out/copied.txt")")"
  wl-copy -n -o 'xyz'
  sleep 0.3
  click 300 $((chrome_h + 215))
  key -k End
  key -M ctrl v -m ctrl
  check "ctrl-v pastes the Wayland clipboard into the page" "$(sess 8 "url(1).endswith('#field åäöäxyz'), url(1).split('#')[1]")"
  shot 05-typed-and-pasted

  step "a wheel notch over the page"
  point 640 500
  sleep 0.2
  wlrctl pointer scroll 15 0
  check "a wheel notch reaches the page" "$(sess 8 "url(1).endswith('#scrolled'), url(1).split('#')[1]")"

  for s in 1.25 1.5 2 1; do
    step "scale $s"
    scale "$s"
    sleep 1.5
    check "at scale $s the page has the page area's size at that ratio" \
      "$(sess 8 "url(1).split('#')[1].startswith('resize dpr $s '), url(1).split('#')[1]")"
    check "at scale $s the view's buffer is its size times the scale" \
      "$(dg 5 "shown['scale']==round($s*120) and shown['buffer_width']==round(shown['width']*$s) and shown['buffer_height']==round(shown['height']*$s), (shown['width'], shown['height'], shown['buffer_width'], shown['buffer_height'], shown['scale'])")"
    click 100 $((chrome_h + 100))
    check "at scale $s a click at page (100, 100) reaches the page there" "$(sess 8 "url(1).endswith('#mousedown 100,100'), url(1).split('#')[1]")"
    shot "06-scale-$s" 0.3
  done

  step "history: history.html (adds #one and #two itself), then second.html; back, back, forward, forward"
  open "$base/history.html"
  check "the page's own entries: a fragment and pushState" \
    "$(sess 10 "entries(1)[1:]==['$base/history.html', '$base/history.html#one', '$base/history.html#two'] and url(1)=='$base/history.html#two', entries(1)[1:]")"
  open "$base/second.html"
  check "a cross-document navigation adds an entry" "$(sess 10 "url(1)=='$base/second.html' and len(entries(1))==5, (len(entries(1)), url(1))")"
  key -M alt -k Left -m alt
  check "back returns to history.html#two" "$(sess 8 "url(1)=='$base/history.html#two' and seg(1)['current']==3, (seg(1)['current'], url(1))")"
  key -M alt -k Left -m alt
  check "back again stays in the document: #one" "$(sess 8 "url(1)=='$base/history.html#one' and seg(1)['current']==2, (seg(1)['current'], url(1))")"
  key -M alt -k Right -m alt
  check "forward: #two" "$(sess 8 "url(1)=='$base/history.html#two' and seg(1)['current']==3, (seg(1)['current'], url(1))")"
  key -M alt -k Right -m alt
  check "forward: second.html" "$(sess 8 "url(1)=='$base/second.html' and seg(1)['current']==4, (seg(1)['current'], url(1))")"
  check "it all stays one Servo segment of five entries" "$(sess 3 "segs(1)==[('servo', 5, 4)], segs(1)")"
  check "and the tab can go back, not forward" "$(dg 3 "tabs[0]['can_go_back'] and not tabs[0]['can_go_forward'], tabs[0]")"

  step "reload"
  open "$base/count.html"
  check "count.html loads once" "$(sess 10 "url(1)=='$base/count.html', url(1)")"
  key -k F5
  sleep 2
  check "F5 loads it again" "$(answer "$([[ $(served /count.html) == 2 ]]; echo $?)" "$(served /count.html) requests")"
  check "a reload adds no entry" "$(sess 5 "url(1)=='$base/count.html' and len(entries(1))==6, (len(entries(1)), url(1))")"

  step "stop: a document whose image never comes"
  open "$base/loading.html"
  check "loading.html commits and keeps loading" "$(sess 10 "url(1)=='$base/loading.html', url(1)")"
  sleep 2
  check "the tab is still loading 2 s later" "$(dg 2 "tabs[0]['loading'], tabs[0]")"
  shot 07-loading
  key -k F7
  check "F7 stops the document's loading" "$(dg 3 "not tabs[0]['loading'], tabs[0]")"
  step "stop: a navigation still waiting for its response (Servo 0.5 can't cancel it, decision 57)"
  open "$base/slow.html?s=3"
  sleep 1
  key -k F7
  sleep 4
  info "a stopped navigation still waiting for its response: $(python3 "$probe" session "$session" "True, url(1)")"

  step "ctrl-q"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  check "Servo shut down itself: no forced exit" \
    "$(answer "$(! grep -q 'could not shut down' "$out/sig.log"; echo $?)" "no 'could not shut down' in sig.log")"
  check "zero page readbacks, GL reads audited" \
    "$(python3 -c "import json; d=json.load(open('$out/diag.json')); c=d['counters']; s=[e for e in d['engines'] if e['name']=='servo'][0]; ok=c['page_readbacks']==0 and c['readback_bytes']==0 and s['readbacks']==0 and s['gl_reads']==0; print(0 if ok else 1, 'page_readbacks', c['page_readbacks'], 'readback_bytes', c['readback_bytes'], 'servo read_to_image+GL', s['readbacks'], 'GL reads', s['gl_reads'], 'of', c['page_frames_presented'], 'frames')")"
  frames=$(grep -c 'sig-diag .* frame engine=servo' "$out/sig.log")
  clean=$(grep -c 'sig-diag .* frame engine=servo .*readback_bytes=0 ' "$out/sig.log")
  check "every Servo frame line says readback_bytes=0" "$(answer "$([[ $frames -gt 0 && $frames == "$clean" ]]; echo $?)" "$clean of $frames")"
  check "Servo wrote its profile at shutdown" "$(answer "$([[ -d $out/data/sighurt/servo ]] && ls "$out/data/sighurt/servo" | grep -qv '^lock$'; echo $?)" "$(ls "$out/data/sighurt/servo" 2>&1 | tr '\n' ' ')")"
  check "the session holds the three tabs and their titles" \
    "$(python3 "$probe" session "$session" "len(s['tabs'])==3 and seg(3).get('title')=='Example Domain', [(url(i), seg(i).get('title')) for i in (1, 2, 3)]")"
  ;;
wasm)
  g=$out/guests
  step "gradient-demo alone: a static guest leaves sig asleep"
  start_sig "$g/gradient-demo.wasm"
  check "gradient-demo loads in the Wasm engine" "$(sess 30 "seg(1)['engine']=='wasm' and url(1)=='file://$g/gradient-demo.wasm', (seg(1)['engine'], url(1))")"
  check "the Wasm engine paints on the CPU" \
    "$(answer "$(grep -q 'renderer engine=wasm mode=cpu$' "$out/sig.log"; echo $?)" "$(grep -o 'renderer engine=wasm.*' "$out/sig.log")")"
  point 1200 790
  sleep 1.5
  read -r frames paints wakeups <<< "$(counted 3)"
  check "a static guest leaves sig asleep" "$(answer "$([[ $wakeups -le 1 && $frames == 0 && $paints == 0 ]]; echo $?)" \
    "$wakeups wakeups (the report's own included), $frames page frames, $paints chrome paints in 3 s")"
  step "the other four example guests, each typed into a new tab"
  for name in hello-sighurt raf-demo timer-demo events-demo; do
    key -M ctrl t -m ctrl
    key "file://$g/$name.wasm"
    key -k Return
  done
  names="['gradient-demo', 'hello-sighurt', 'raf-demo', 'timer-demo', 'events-demo']"
  check "the five guests load in the Wasm engine" "$(sess 30 "len(s['tabs'])==5 and all(seg(i)['engine']=='wasm' and url(i).endswith('/' + n + '.wasm') for i, n in enumerate($names, 1)), [(seg(i)['engine'], url(i).rsplit('/', 1)[-1]) for i in range(1, len(s['tabs']) + 1)]")"
  check "five Wasm views, the last shown" "$(dg 10 "len(views)==5 and all(v['engine']=='wasm' for v in views) and shown and shown['tab']==5, [(v['tab'], v['engine'], v['shown']) for v in views]")"
  for i in 1 2 3 4 5; do
    key -M alt "$i" -m alt
    check "alt-$i shows guest $i and it presents" "$(dg 5 "shown is not None and shown['tab']==$i and shown['engine']=='wasm' and shown['submissions']>0 and d['status']!='error', (d['active_tab'], shown and shown['submissions'], d['status'])")"
    shot "0$i-guest" 0.5
  done
  key -M alt 1 -m alt
  point 1200 790
  sleep 1.5
  read -r frames paints wakeups <<< "$(counted 3)"
  info "gradient-demo shown, four guests in background tabs (one step a second each, decision 23): $wakeups wakeups, $frames page frames, $paints chrome paints in 3 s"

  step "hello-sighurt: the pointer reaches it"
  key -M alt 2 -m alt
  point 400 $((chrome_h + 300))
  shot 06-hello-pointer-400-300
  step "raf-demo animates with no chrome repaint"
  key -M alt 3 -m alt
  sleep 1
  read -r frames paints _ <<< "$(counted 2)"
  check "raf-demo animates" "$(answer "$([[ $frames -ge 60 ]]; echo $?)" "$frames page frames in 2 s")"
  check "its animation repaints no chrome" "$(answer "$([[ $paints == 0 ]]; echo $?)" "$paints chrome paints in 2 s")"

  step "timer-demo: Start Blink (set_interval of 500 ms), then the stopwatch"
  key -M alt 4 -m alt
  click 80 $((chrome_h + 283))
  sleep 0.3
  seen=""
  for n in 1 2 3 4 5 6 7 8; do
    read -r r gr b <<< "$(colour "07-blink-$n" 200 $((chrome_h + 283)))"
    if [[ $r -lt 120 && $gr -gt 160 && $b -gt 160 ]]; then seen+=c; else seen+=.; fi
    sleep 0.15
  done
  check "its interval timer blinks the light" "$(answer "$([[ $seen == *c* && $seen == *.* ]]; echo $?)" "lit (c) or not (.) every ~0.25 s: $seen")"
  click 60 $((chrome_h + 368))
  sleep 2.2
  shot 08-stopwatch-about-2s 0
  click 1000 $((chrome_h + 600))

  step "events-demo: a held button is a touch at the pointer; Emit ping; a resize"
  key -M alt 5 -m alt
  point 600 $((chrome_h + 500))
  sleep 0.2
  echo press >&3
  sleep 0.4
  read -r r gr b <<< "$(colour 09-touch-600-500 600 $((chrome_h + 500)))"
  echo release >&3
  shot 09-released
  check "a held button reaches it as a touch, drawn at the pointer" \
    "$(answer "$([[ $r -gt 150 && $gr -lt 150 && $b -gt 120 ]]; echo $?)" "pixel at the pointer: $r $gr $b (pink is about 193 115 164)")"
  click 90 $((chrome_h + 325))
  shot 10-ping-count-1
  swaymsg -q output HEADLESS-1 resolution 1000x700
  layout_w=1000 layout_h=700
  shot 11-events-resized 1
  swaymsg -q output HEADLESS-1 resolution 1280x800
  layout_w=1280 layout_h=800
  sleep 1

  step "a guest looping in on_frame, then one calling the host in a loop"
  for guest in loop calls; do
    key -M ctrl t -m ctrl
    key "file://$g/$guest.wasm"
    key -k Return
    tab=$(value "len(tabs)")
    check "$guest: the guest is stopped and the tab says so" "$(dg 10 "tabs[$tab - 1]['error'] and d['status']=='error', (tabs[$tab - 1], d['status'])")"
    shot "12-$guest-stopped"
    key -M alt 1 -m alt
    check "$guest: the chrome still takes keys" "$(shows_tab 1 wasm)"
    key -M alt "$tab" -m alt
    check "$guest: and the stopped guest stays stopped" "$(dg 3 "tabs[$tab - 1]['error'] and shown and shown['tab']==$tab, tabs[$tab - 1]")"
  done

  step "invalid drawing and resource requests"
  key -M ctrl t -m ctrl
  key "file://$g/bad.wasm"
  key -k Return
  tab=$(value "len(tabs)")
  check "the guest lives through them and says so" "$(sess 20 "url($tab).endswith('/bad.wasm#alive'), url($tab).rsplit('/', 1)[-1]")"
  check "and the tab shows no error" "$(dg 3 "not tabs[$tab - 1]['error'] and shown and shown['tab']==$tab, (tabs[$tab - 1], d['status'])")"
  outside=$(colour 13-bad-requests 0 $((chrome_h + 0)))
  inside=$(python3 "$probe" pixel "$out/13-bad-requests.ppm" 400 $((chrome_h + 300)))
  check "its frame: the clear outside every clip, green fills inside" \
    "$(answer "$([[ $outside == '0 0 64' && $inside == '0 128 0' ]]; echo $?)" "page (0, 0): $outside, page (400, 300): $inside")"

  step "typing into the address bar while guests work"
  key -M alt 1 -m alt
  read -r median max lost <<< "$(typing)"
  info "typing over static gradient-demo: median $median ms, max $max ms, $lost keys without a repaint"
  idle_median=$median
  key -M ctrl t -m ctrl
  key "file://$g/busy.wasm"
  key -k Return
  sleep 2
  busy_tab=$(value "len(tabs)")
  read -r frames _ _ <<< "$(counted 1)"
  info "busy.wasm (40 ms of work a frame) presents $frames frames a second"
  read -r median max lost <<< "$(typing)"
  check "typing over a busy guest: every key repaints the chrome, none waits long" \
    "$(answer "$([[ $lost == 0 ]] && python3 -c "import sys; sys.exit(not ($max < 250))"; echo $?)" "median $median ms, max $max ms (static guest: median $idle_median ms), $lost keys without a repaint")"
  step "raf-demo at scale 2 (painted on the owner thread)"
  key -M alt 3 -m alt
  scale 2
  sleep 1.5
  read -r median max lost <<< "$(typing)"
  check "typing over raf-demo at scale 2: every key repaints the chrome, none waits long" \
    "$(answer "$([[ $lost == 0 ]] && python3 -c "import sys; sys.exit(not ($max < 250))"; echo $?)" "median $median ms, max $max ms (static guest at scale 1: median $idle_median ms), $lost keys without a repaint")"
  scale 1
  sleep 1
  check "no engine pump held the owner for 50 ms" "$(dg 3 "p['max_pump_us'] < 50000, (p['max_pump_us'], p['max_present_us'], p['max_chrome_paint_us'])")"

  step "ctrl-q"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  check "the session holds every tab" "$(python3 "$probe" session "$session" "len(s['tabs'])==$busy_tab, [url(i).rsplit('/', 1)[-1] for i in range(1, len(s['tabs']) + 1)]")"
  ;;

cross)
  app=$base/guests/gradient-demo.wasm
  step "a Servo page that adds two entries itself"
  start_sig "$base/history.html"
  check "the Servo page and its own entries" "$(sess 20 "segs(1)==[('servo', 3, 2)] and url(1)=='$base/history.html#two', (segs(1), url(1))")"
  step "a Wasm app from the server, in the same tab"
  open "$app"
  check "the app opens in the Wasm engine, a segment of its own" "$(sess 15 "segs(1)==[('servo', 3, 2), ('wasm', 1, 0)] and tab(1)['current']==1, (segs(1), tab(1)['current'])")"
  check "the tab shows the Wasm view" "$(shows_tab 1 wasm)"
  shot 01-wasm-app 1
  step "back, back, forward, forward"
  key -M alt -k Left -m alt
  check "back crosses into the Servo segment at its entry" "$(sess 10 "tab(1)['current']==0 and url(1)=='$base/history.html#two', (tab(1)['current'], url(1))")"
  check "the tab shows the Servo view" "$(shows_tab 1 servo)"
  check "the Servo page was kept, not loaded again" "$(answer "$([[ $(served /history.html) == 1 ]]; echo $?)" "$(served /history.html) request(s) for history.html")"
  shot 02-back-to-servo
  key -M alt -k Left -m alt
  check "back within Servo: #one" "$(sess 10 "tab(1)['current']==0 and url(1)=='$base/history.html#one', (tab(1)['current'], url(1))")"
  key -M alt -k Right -m alt
  check "forward within Servo: #two" "$(sess 10 "tab(1)['current']==0 and url(1)=='$base/history.html#two', (tab(1)['current'], url(1))")"
  key -M alt -k Right -m alt
  check "forward crosses into the Wasm segment" "$(sess 10 "tab(1)['current']==1 and url(1)=='$app', (tab(1)['current'], url(1))")"
  check "the tab shows the Wasm view again" "$(shows_tab 1 wasm)"
  check "the app was kept, not loaded again" "$(answer "$([[ $(served /guests/gradient-demo.wasm) == 1 ]]; echo $?)" "$(served /guests/gradient-demo.wasm) request(s) for the app")"
  step "a third segment: second.html from the app"
  open "$base/second.html"
  check "a Servo page after the app is a third segment" "$(sess 15 "segs(1)==[('servo', 3, 2), ('wasm', 1, 0), ('servo', 1, 0)] and tab(1)['current']==2, (segs(1), tab(1)['current'])")"
  check "the tab shows a Servo view" "$(shows_tab 1 servo)"
  key -M alt -k Left -m alt
  check "back: the app" "$(sess 10 "tab(1)['current']==1 and url(1)=='$app', (tab(1)['current'], url(1))")"
  check "shown in its Wasm view" "$(shows_tab 1 wasm)"
  key -M alt -k Left -m alt
  check "back: the first Servo segment at #two" "$(sess 10 "tab(1)['current']==0 and url(1)=='$base/history.html#two', (tab(1)['current'], url(1))")"
  check "shown in its Servo view" "$(shows_tab 1 servo)"
  check "no segment was loaded again" "$(answer "$([[ $(served /history.html) == 1 && $(served /guests/gradient-demo.wasm) == 1 && $(served /second.html) == 1 ]]; echo $?)" \
    "requests: history.html $(served /history.html), the app $(served /guests/gradient-demo.wasm), second.html $(served /second.html)")"
  check "three live views at most: the shown one and two retained" "$(dg 3 "len(views)==3 and sorted(v['engine'] for v in views)==['servo', 'servo', 'wasm'], [(v['engine'], v['shown']) for v in views]")"
  key -M alt -k Right -m alt
  key -M alt -k Right -m alt
  check "forward twice: second.html" "$(sess 10 "tab(1)['current']==2 and url(1)=='$base/second.html', (tab(1)['current'], url(1))")"
  check "the tab can go back, not forward" "$(dg 3 "tabs[0]['can_go_back'] and not tabs[0]['can_go_forward'], tabs[0]")"

  step "restart: the segments are restored as records, which load again when shown"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  cp "$session" "$out/session-before-restart.toml"
  all_entries="[e for k in range(len(tab(1)['segments'])) for e in entries(1, k)]"
  saved=$(python3 "$probe" session "$session" "True, $all_entries")
  saved=${saved:2}
  # Servo 0.5's history report names an entry before a pushState after the new one; the adapter
  # keeps each entry's own address (decision 58).
  check "the session saved every entry by its own address" \
    "$(answer "$([[ $saved == "['$base/history.html', '$base/history.html#one', '$base/history.html#two', '$app', '$base/second.html']" ]]; echo $?)" "$saved")"
  start_sig
  check "the restored tab has the three segments and shows the third" \
    "$(sess 15 "segs(1)==[('servo', 3, 2), ('wasm', 1, 0), ('servo', 1, 0)] and tab(1)['current']==2 and url(1)=='$base/second.html', (segs(1), tab(1)['current'], url(1))")"
  check "it loads second.html again" "$(answer "$(for _ in $(seq 40); do [[ $(served /second.html) == 2 ]] && break; sleep 0.25; done; [[ $(served /second.html) == 2 ]]; echo $?)" "$(served /second.html) request(s)")"
  check "in a Servo view" "$(shows_tab 1 servo)"
  key -M alt -k Left -m alt
  check "back: the app, loaded again" "$(sess 10 "tab(1)['current']==1 and url(1)=='$app', (tab(1)['current'], url(1))")"
  check "in a Wasm view" "$(shows_tab 1 wasm)"
  key -M alt -k Left -m alt
  # Its entries before #two become a record of their own (decision 29): the new view can't
  # have them in its history.
  check "back: the Servo page at #two, loaded again, every saved entry still there" \
    "$(sess 15 "url(1)=='$base/history.html#two' and seg(1)['engine']=='servo' and $all_entries==$saved, (segs(1), tab(1)['current'], url(1))")"
  check "in a Servo view" "$(shows_tab 1 servo)"
  check "each segment loaded once more, its own engine's" "$(answer "$([[ $(served /history.html) == 2 && $(served /guests/gradient-demo.wasm) == 2 ]]; echo $?)" \
    "requests: history.html $(served /history.html), the app $(served /guests/gradient-demo.wasm)")"
  shot 03-restored-servo-segment
  kill -TERM "$sig_pid"
  wait "$sig_pid"; code=$?
  check "SIGTERM exits cleanly" "$(answer "$code" "exit code $code")"
  ;;
esac
finish
