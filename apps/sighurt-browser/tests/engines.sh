#!/usr/bin/env bash
# End-to-end tests of `sig` with its default engines, Servo, Wasm and WPE WebKit, in a private
# headless sway with the GLES2 renderer (the GPU), on a D-Bus session of their own
# (engines/sighurt-engine-wpe/tests/private-bus): never on the desktop, since they type and click. Builds
# `sig` (default features, with the modal keymap and the line chrome compiled in too, which only
# `showcase` selects), the virtual-pointer helper and the example guests with tools/cargo,
# then runs the scenarios below inside `tools/headless-sway --renderer gles2`, printing PASS or
# FAIL for each check. A local web server (`serve.py`) serves `tests/pages` and the guests;
# the test guests in `tests/guests` are WAT, which Wasmtime compiles as it does binaries. Only
# https://example.com/ needs the network. Servo builds here only when the desktop is free
# (`tools/heavy` refuses it otherwise); then run it on louise-pc through tools/remote. WPE
# WebKit must be installed (docs/rewrite/WPE.md).
#
# The checks read what `sig` itself says: its diagnostics (which tab and view are shown, each
# tab's loading and history state, frame and readback counters, with Servo's GL reads audited:
# SIG_AUDIT_GL=1), its session file (the address and history segments of each tab, where the
# test pages report what they received through their address), its log, the server's log, and
# screenshot pixels. Screenshots, logs and diagnostics go to OUT; look at the screenshots too.
#
#   apps/sighurt-browser/tests/engines.sh OUT                  build, then run every scenario
#   apps/sighurt-browser/tests/engines.sh --no-build OUT       run with what is in target/
#   apps/sighurt-browser/tests/engines.sh OUT servo cross      only these scenarios
#
# Scenarios, each with a fresh configuration, session and profile:
#   servo   M4 at browser level: local pages, https://example.com/ and a WebGL page; open, close
#           and switch tabs; text input (å, ä, ö, a dead key) and the clipboard both ways;
#           pointer positions and page size at scales 1, 1.25, 1.5 and 2; a wheel notch; back
#           and forward through same-document entries; reload; stop; keys sent with
#           page.send-key; [input] scroll-speed applied live; idle and animation without chrome
#           repaints; zero readbacks; a clean quit with Servo shut down
#   wasm    M5 at browser level: hello-sighurt, gradient-demo, raf-demo, timer-demo and
#           events-demo run and take input; a guest looping in on_frame and one calling the host
#           in a loop are stopped while the chrome keeps working; invalid drawing fails safely;
#           a guest's download goes into the download folder whatever its name says; busy
#           guests don't hold up typing in the address bar
#   cross   one tab across the engine boundary: a Servo page, a Wasm app, back and forward
#           through both engines' entries, a third segment, and the segments restored after a
#           restart
#   settings  [engines.servo] and [engines.wpe] user-agent (the header the engine sends and
#           navigator.userAgent) and storage = "ephemeral" for Servo, WPE and Wasm (no profile,
#           no store or cache left after quitting); a [keywords] template; page.open-in wasm for
#           an app routing sends to Servo, and back to the Servo page; a changed engine setting's
#           restart notice
#   links   links in a Servo page follow the routing: a click on a link to an app on a Wasm
#           origin, or on a host [routing.wasm] hosts claims, opens it in the Wasm engine; a page
#           that sends itself there without a click, and the links of a view page.open-in chose,
#           stay in Servo
#   wpe     WPE WebKit in sig: what the executable exports and what binds to it; a page on a
#           host [routing.wpe] hosts claims, and https://example.com/ with page.open-in wpe;
#           text input (å, ä, ö, a dead key) and the clipboard both ways; idle, and a WebGL page
#           animating without chrome repaints and with no CPU copy (DMA-BUF), and idle across
#           the whole process tree (WebKit's processes too) with it shown or hidden; one tab
#           across Servo, WPE and Wasm, back and forward; links between Servo and WPE following
#           the routing, and a script's navigation staying in WPE; WebKit's web processes killed
#           and a reload; closing every WPE tab ends WebKit's web processes; the profile WebKit
#           keeps
#   showcase  a configuration that changes much: the modal keymap (a sequence, typing in insert
#           mode, keys swallowed in normal mode, a command line), the standard chrome's tab list
#           on the left and chrome.toggle-tabs, [ui] font-size, a [routing.wpe] hosts rule, a
#           [keywords] template; a binding and a color added while sig runs apply at once, and a
#           new [ui] chrome gets a notice; started again, the line chrome with the same keymap
#   labels  the engine labels: a Servo page, a page [routing.wpe] hosts gives WPE and sig://home
#           are labelled servo, wpe and nothing, in their tabs and the address bar; engine-label =
#           false turns them off while sig runs; the line chrome names the page's engine too
#   switch  page.switch-engine moves https://example.com/ from Servo to WPE and back, checked
#           from the diagnostics and the session's history segments
#
# Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"
guests=(hello-sighurt gradient-demo raf-demo timer-demo events-demo)

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: engines.sh [--no-build] OUT [SCENARIO ...]}")
  shift
  scenarios=("$@")
  [[ ${#scenarios[@]} == 0 ]] && scenarios=(servo wasm cross settings links wpe showcase labels switch)
  mkdir -p "$out"
  # Other builds replace target/debug/sig (the demo engine's tests build it with other
  # features); this link keeps the binary these runs use. It is made under the same hold of the
  # machine's lock as the build, so no other build can come between them.
  link='ln -f target/debug/sig target/debug/sig-engines-e2e'
  if [[ $build == 1 ]]; then
    SIG_WHAT="servo build: sig (default features) for engines.sh" tools/heavy bash -c \
      "cargo build -p sighurt-browser --features keymap-modal,chrome-line --bins \
       --example virtual-pointer && $link" || exit 1
    SIG_WHAT="build the example guests for engines.sh" tools/cargo build \
      --target wasm32-unknown-unknown --release "${guests[@]/#/-p}" || exit 1
  else
    $link || exit 1
  fi
  failed=0
  for scenario in "${scenarios[@]}"; do
    tools/heavy tools/headless-sway --renderer gles2 -- engines/sighurt-engine-wpe/tests/private-bus \
      "$0" --inside "$out/$scenario" "$scenario"
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
export PATH="$(nix-build nix/nixpkgs.nix -A wl-clipboard --no-out-link 2>/dev/null)/bin:$PATH"
sig=target/debug/sig-engines-e2e
probe=apps/sighurt-browser/tests/probe.py
session=$out/state/sighurt/session.toml
# The web server, which also serves the guests (/guests/NAME).
python3 apps/sighurt-browser/tests/serve.py apps/sighurt-browser/tests/pages "$out/guests" "$out/port" \
  "$out/server.log" &
server_pid=$!
for _ in $(seq 50); do [[ -s $out/port ]] && break; sleep 0.1; done
base=http://127.0.0.1:$(cat "$out/port")
served() { grep -c " GET $1\$" "$out/server.log"; }
# Downloads go to the run's folder: the desktop's comes from user-dirs.dirs, not the environment.
cat > "$out/config/sighurt/config.toml" <<CONF
version = 1
[downloads]
directory = "$out/downloads"
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
"alt-j" = "page.send-key end"
"alt-k" = "page.send-key home"
"ctrl-q" = "browser.quit"
CONF
# The example guests, and the test guests in WAT, which Wasmtime compiles as it does binaries.
for name in "${guests[@]}"; do
  cp "target/wasm32-unknown-unknown/release/${name//-/_}.wasm" "$out/guests/$name.wasm"
done
for wat in apps/sighurt-browser/tests/guests/*.wat; do
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
# The helpers hold tools/heavy's lock with this script: they end with it, however it ends.
trap 'kill "$keyboard" "$pointer_pid" "$server_pid" ${sig_pid:-} 2>/dev/null' EXIT
layout_w=1280 layout_h=800
# The chrome above the page is 84 logical pixels tall at every scale.
chrome_h=84
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
# The pointer rests a moment before the click, so a link under it is the page's status text
# when the button goes down: a link the routing gives another engine follows it only then.
click() { point "$1" "$2"; sleep 0.3; echo click >&3; sleep 0.2; }
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

  step "keys.html: page.send-key end and home (alt-j, alt-k), then a wheel notch at scroll-speed 1 and 3"
  open "$base/keys.html"
  check "keys.html commits" "$(sess 10 "url(1).startswith('$base/keys.html#'), url(1)")"
  # The page's address says "#<key>/<code> <scrollY>".
  key -M alt j -m alt
  check "page.send-key end reaches Servo as End, with its code" \
    "$(sess 8 "url(1).split('#')[1].startswith('End/End '), url(1).split('#')[1]")"
  info "after End the page is at: $(python3 "$probe" session "$session" "True, url(1).split('#')[1]")"
  key -M alt k -m alt
  check "page.send-key home reaches Servo as Home" \
    "$(sess 8 "url(1).split('#')[1].startswith('Home/Home '), url(1).split('#')[1]")"
  scrolled() { python3 "$probe" session "$session" "True, int(url(1).split(' ')[-1])" | cut -d' ' -f2; }
  point 640 500
  sleep 0.5
  top=$(scrolled)
  wlrctl pointer scroll 15 0
  sleep 1.5
  one=$(( $(scrolled) - top ))
  key -M alt k -m alt
  sleep 1
  printf '[input]\nscroll-speed = 3\n' >> "$out/config/sighurt/config.toml"
  sleep 0.5
  top=$(scrolled)
  wlrctl pointer scroll 15 0
  sleep 1.5
  three=$(( $(scrolled) - top ))
  check "scroll-speed 3, loaded live, scrolls a notch three times as far" \
    "$(answer "$([[ $one -gt 0 ]] && python3 -c "import sys; sys.exit(abs($three - 3 * $one) > 0.1 * 3 * $one + 2)"; echo $?)" "a notch scrolled $one px at 1, $three px at 3")"

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

  step "download.wasm saves data under an absolute path: sig saves it in the download folder"
  rm -f /tmp/sighurt-e2e-escape.txt
  key -M ctrl t -m ctrl
  key "file://$g/download.wasm"
  key -k Return
  for _ in $(seq 40); do [[ -s $out/downloads/sighurt-e2e-escape.txt ]] && break; sleep 0.25; done
  check "a guest's download lands in the download folder, never where its name points" \
    "$(answer "$([[ $(cat "$out/downloads/sighurt-e2e-escape.txt" 2>/dev/null) == hello && ! -e /tmp/sighurt-e2e-escape.txt ]]; echo $?)" \
      "in the folder: $(ls "$out/downloads" | tr '\n' ' ')/tmp/sighurt-e2e-escape.txt $([[ -e /tmp/sighurt-e2e-escape.txt ]] && echo exists || echo absent)")"

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
settings)
  agent="SighurtTest/1.0 (engines.sh)"
  cat >> "$out/config/sighurt/config.toml" <<CONF
[keywords]
t = "$base/count.html?q={}"
[engines.servo]
user-agent = "$agent"
storage = "ephemeral"
[engines.wasm]
storage = "ephemeral"
[engines.wpe]
user-agent = "$agent"
storage = "ephemeral"
CONF
  step "user-agent and storage for Servo, WPE and Wasm; ua.html reports the header and navigator.userAgent"
  start_sig "$base/ua.html"
  check "Servo sends the configured user agent, and its pages see it" \
    "$(sess 20 "url(1).split('#', 1)[-1]=='[\"$agent\",\"$agent\"]', url(1).split('#', 1)[-1]")"
  check "and the configuration was valid: no notice" "$(dg 3 "d['status'] is None, d['status']")"

  step "a keyword: t åäö"
  open "t åäö"
  check "a keyword opens its template with the rest as the query" \
    "$(sess 15 "url(1)=='$base/count.html?q=åäö', url(1)")"

  step "page.open-in wasm: an app on localhost, an origin routing sends to Servo; then back"
  app=http://localhost:$(cat "$out/port")/guests/gradient-demo.wasm
  open ":page.open-in wasm $app"
  check "page.open-in opens it in the engine named" "$(shows_tab 1 wasm)"
  check "as a segment of its own after the Servo page" \
    "$(sess 15 "[e for e, _, _ in segs(1)]==['servo', 'wasm'] and url(1)=='$app', (segs(1), url(1))")"
  shot 01-open-in-wasm 1
  key -M alt -k Left -m alt
  check "back returns to the page in Servo" "$(shows_tab 1 servo)"

  step "a Wasm app that keeps a key, in a new tab"
  key -M ctrl t -m ctrl
  key "$base/guests/kv.wasm"
  key -k Return
  check "the app kept its key" "$(sess 20 "url(2).endswith('/kv.wasm#stored'), url(2)")"
  scratch=$(ls -d "$XDG_RUNTIME_DIR"/sighurt-wasm-* 2>/dev/null | head -1)
  check "with storage = ephemeral the store is in a directory of this run's" \
    "$(answer "$([[ -n $scratch && -n $(ls -A "$scratch/kv" 2>/dev/null) ]]; echo $?)" "${scratch:-none}: $(ls -A "$scratch/kv" 2>&1 | head -3 | tr '\n' ' ')")"
  check "only the user may read it" \
    "$(answer "$([[ $(stat -c %a "$scratch" 2>/dev/null) == 700 ]]; echo $?)" "mode $(stat -c %a "$scratch" 2>&1)")"

  step "page.open-in wpe ua.html, in a third tab"
  key -M ctrl t -m ctrl
  open ":page.open-in wpe $base/ua.html"
  check "WebKit sends the configured user agent, and its pages see it" \
    "$(sess 20 "seg(3)['engine']=='wpe' and url(3).split('#', 1)[-1]=='[\"$agent\",\"$agent\"]', (seg(3)['engine'], url(3).split('#', 1)[-1])")"

  step "user-agent changed while sig runs: it applies at the next start, and a notice says so"
  sed -i 's|^user-agent = .*|user-agent = "Other/2.0"|' "$out/config/sighurt/config.toml"
  check "a changed engine setting gets a notice" "$(dg 5 "d['status']=='notice', d['status']")"

  step "ctrl-q"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  check "Servo kept no profile" "$(answer "$([[ ! -e $out/data/sighurt/servo ]]; echo $?)" "$(ls -A "$out/data/sighurt" 2>&1 | tr '\n' ' ')")"
  webkit=$(find "$out/data" "$out/cache" \( -iname '*wpe*' -o -iname '*webkit*' \) 2>/dev/null | tr '\n' ' ')
  check "WebKit kept nothing: no profile, no data or cache of its own" "$(answer "$([[ -z $webkit ]]; echo $?)" "${webkit:-none}")"
  check "the Wasm engine kept no store and no compiled modules" \
    "$(answer "$([[ ! -e $out/data/sighurt/wasm && ! -e $out/cache/sighurt/wasm-aot ]]; echo $?)" "$(ls -A "$out/data/sighurt" "$out/cache/sighurt" 2>&1 | tr '\n' ' ')")"
  check "and its directory for this run is gone" \
    "$(answer "$([[ -n $scratch && ! -e $scratch ]] && ! ls -d "$XDG_RUNTIME_DIR"/sighurt-wasm-* >/dev/null 2>&1; echo $?)" "${scratch:-none}")"
  ;;
links)
  # Besides the origin every scenario's [routing.wasm] has, the host localhost, which links.html
  # (on 127.0.0.1) links to. Its links are bands 150 page pixels tall.
  sed -i 's|^origins = .*|&\nhosts = ["localhost"]|' "$out/config/sighurt/config.toml"
  wasm_host=http://localhost:$(cat "$out/port")
  band() { echo $((chrome_h + 150 * $1 - 75)); }
  step "links.html in Servo, then a click on its link to an app on the Wasm origin"
  RUST_LOG=warn,sighurt_engine_servo::timing=debug start_sig "$base/links.html"
  check "links.html opens in Servo" "$(sess 20 "url(1)=='$base/links.html' and seg(1)['engine']=='servo', (segs(1), url(1))")"
  shot 01-links 1
  click 200 "$(band 1)"
  check "a click on a link to an app on the Wasm origin opens it in the Wasm engine" "$(shows_tab 1 wasm)"
  check "as a segment after the Servo page" \
    "$(sess 15 "segs(1)==[('servo', 1, 0), ('wasm', 1, 0)] and url(1)=='$base/guests/gradient-demo.wasm', (segs(1), url(1))")"
  shot 02-app-from-a-link 1
  key -M alt -k Left -m alt
  check "back returns to the Servo page" "$(shows_tab 1 servo)"
  check "which was kept, not loaded again" "$(answer "$([[ $(served /links.html) == 1 ]]; echo $?)" "$(served /links.html) request(s)")"

  step "a click on its link to an app on the host [routing.wasm] hosts claims"
  click 200 "$(band 2)"
  check "a click on a link to a host the routing gives Wasm opens it there" "$(shows_tab 1 wasm)"
  check "the app on that host" "$(sess 15 "url(1)=='$wasm_host/guests/hello-sighurt.wasm' and seg(1)['engine']=='wasm', (segs(1), url(1))")"
  key -M alt -k Left -m alt
  check "back returns to the Servo page" "$(shows_tab 1 servo)"

  step "a click on its link to itself, which then sends itself to that host without a click"
  click 200 "$(band 3)"
  check "what a page does without a click stays in Servo" \
    "$(sess 15 "url(1)=='$wasm_host/second.html?redirected' and seg(1)['engine']=='servo', (segs(1), url(1))")"
  check "the tab shows its Servo view" "$(shows_tab 1 servo)"

  step "page.open-in servo links.html, then a click on its link to a page on that host"
  open ":page.open-in servo $base/links.html"
  check "page.open-in servo opens links.html" "$(sess 15 "url(1)=='$base/links.html', url(1)")"
  sleep 0.5
  click 200 "$(band 4)"
  check "the links of a view page.open-in chose stay in its engine" \
    "$(sess 15 "url(1)=='$wasm_host/second.html?chosen' and seg(1)['engine']=='servo', (segs(1), url(1))")"
  check "the tab shows its Servo view" "$(shows_tab 1 servo)"
  shot 03-chosen-stays

  held=$(grep -o 'navigation-answered view=[0-9]* held_ms=[0-9.]*' "$out/sig.log" | grep -o '[0-9.]*$' | tr '\n' ' ')
  info "how long Servo held each navigation for the core's answer, in ms: $held"
  check "Servo asked about each of the five navigations" "$(answer "$([[ $(wc -w <<< "$held") == 5 ]]; echo $?)" "$(wc -w <<< "$held") answered")"
  step "ctrl-q"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  ;;
wpe)
  lh=http://localhost:$(cat "$out/port")
  cat >> "$out/config/sighurt/config.toml" <<CONF
[routing.wpe]
hosts = ["localhost"]
CONF
  step "what sig exports, and which of its symbols the shared libraries bind to (decision 65)"
  exports=$(readelf --dyn-syms -W "$sig" | awk '$7 != "UND" && NR > 3 && $8 != "" {print $8}' | sort -u | tr '\n' ' ')
  check "sig exports none of the C libraries linked into it" \
    "$(answer "$([[ $(wc -w <<< "$exports") -le 1 ]]; echo $?)" "$(wc -w <<< "$exports") exported: $exports")"
  LD_DEBUG=bindings LD_BIND_NOW=1 LD_DEBUG_OUTPUT="$out/bindings" "$sig" --version > /dev/null 2>&1
  exe=$(basename "$sig")
  into=$(grep -h "binding file [^ ]* \[0\] to [^ ]*/$exe \[0\]" "$out"/bindings.* | grep -v "binding file [^ ]*/$exe \[0\]")
  rm -f "$out"/bindings.*
  webkit=$(grep -c libWPEWebKit <<< "$into")
  others=$(grep -v "symbol \`main'" <<< "$into" | grep -c .)
  check "no shared library, WebKit's included, binds to sig's copies of its libraries" \
    "$(answer "$([[ $webkit == 0 && $others == 0 ]]; echo $?)" "from WebKit's library $webkit, in all $others besides libpulse's look-up of main")"

  step "input.html on localhost, a host [routing.wpe] hosts claims"
  RUST_LOG=warn,sighurt_engine_wpe::timing=debug start_sig "$lh/input.html"
  check "a page on a host the routing gives WPE opens in WebKit" \
    "$(sess 30 "url(1).startswith('$lh/input.html#') and seg(1)['engine']=='wpe', (seg(1)['engine'], url(1))")"
  check "its WPE view is shown" "$(shows_tab 1 wpe)"
  shot 01-input 1
  check "WebKit's JavaScriptCore left sig's SIGUSR1 (diagnostics) alone" \
    "$(answer "$(! grep -q 'Overriding existing handler for signal' "$out/sig.log"; echo $?)" "$(grep -o 'Overriding existing handler for signal [0-9]*' "$out/sig.log" || echo 'no handler replaced')")"
  check "WebKit renders on the GPU and its frames come as DMA-BUFs" \
    "$(answer "$(grep -q 'renderer engine=wpe mode=hardware gl="WebKit, DMA-BUF on ' "$out/sig.log"; echo $?)" "$(grep -o 'renderer engine=wpe.*' "$out/sig.log")")"
  step "idle on the static page, once WebKit's overlay scrollbar (the page is 3000 px tall) has faded"
  point 1200 790
  for waited in $(seq 10); do read -r frames _ _ <<< "$(counted 1)"; [[ $frames == 0 ]] && break; done
  info "WebKit's scrollbar animated until about $waited s after the pointer left the page"
  read -r frames paints wakeups <<< "$(counted 3)"
  check "a static WPE page leaves sig's owner loop asleep" "$(answer "$([[ $wakeups -le 1 && $frames == 0 && $paints == 0 ]]; echo $?)" \
    "$wakeups wakeups (the report's own included), $frames page frames, $paints chrome paints in 3 s")"
  # The owner's counter misses WebKit's threads and processes: count every thread of the tree.
  tree=$(python3 "$probe" switches "$sig_pid" 3)
  check "and the whole process tree, WebKit's processes included, nearly so" \
    "$(answer "$(python3 -c "print(0 if ${tree%% *} <= 10 else 1)")" "${tree%% *} context switches a second; busiest: ${tree#* }")"

  step "text and the clipboard in the page's field"
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
  shot 02-typed-and-pasted

  step "ctrl-t, animated.html on localhost: a WebGL page"
  key -M ctrl t -m ctrl
  key "$lh/animated.html"
  key -k Return
  check "the WebGL page opens in WPE and has a WebGL context" \
    "$(sess 20 "url(2)=='$lh/animated.html' and seg(2)['engine']=='wpe' and seg(2).get('title')=='webgl', (seg(2)['engine'], url(2), seg(2).get('title'))")"
  check "the second tab shows its WPE view" "$(shows_tab 2 wpe)"
  sleep 1
  read -r frames paints _ <<< "$(counted 2)"
  check "the WebGL page animates at the compositor's pace" "$(answer "$([[ $frames -ge 60 ]]; echo $?)" "$frames page frames in 2 s")"
  check "page animation repaints no chrome" "$(answer "$([[ $paints == 0 ]]; echo $?)" "$paints chrome paints in 2 s")"
  shot 03-webgl 0
  check "no CPU copy of a WPE frame so far" \
    "$(dg 3 "engines['wpe']['readbacks']==0 and c['page_readbacks']==0, ('wpe copies', engines['wpe']['readbacks'], 'page readbacks', c['page_readbacks'], 'of', c['page_frames_presented'], 'frames')")"

  step "ctrl-t, page.open-in wpe https://example.com/"
  key -M ctrl t -m ctrl
  open ":page.open-in wpe https://example.com/"
  check "page.open-in wpe opens example.com in WebKit" \
    "$(sess 25 "url(3)=='https://example.com/' and seg(3)['engine']=='wpe' and seg(3).get('title')=='Example Domain', (seg(3)['engine'], url(3), seg(3).get('title'))")"
  check "the third tab shows its WPE view" "$(shows_tab 3 wpe)"
  shot 04-example-com 1
  step "an app on localhost typed into that tab: content WebKit can't show, so sig downloads it"
  open "$lh/guests/hello-sighurt.wasm"
  check "it lands in the download folder, whole" \
    "$(answer "$(for _ in $(seq 40); do cmp -s "$out/downloads/hello-sighurt.wasm" "$out/guests/hello-sighurt.wasm" && break; sleep 0.25; done; cmp -s "$out/downloads/hello-sighurt.wasm" "$out/guests/hello-sighurt.wasm"; echo $?)" "$(ls "$out/downloads" | tr '\n' ' ')")"
  check "and the tab keeps its page" "$(sess 5 "url(3)=='https://example.com/' and seg(3)['engine']=='wpe', (seg(3)['engine'], url(3))")"
  step "idle, with the WebGL page that animated hidden in the second tab"
  sleep 2
  tree=$(python3 "$probe" switches "$sig_pid" 5)
  info "a hidden WPE tab that animated: one thread of its web process wakes about 120 times a second (WebKit's own, almost no CPU)"
  check "a hidden WPE tab that animated renders nothing: the tree stays under 300 context switches a second" \
    "$(answer "$(python3 -c "print(0 if ${tree%% *} <= 300 else 1)")" "${tree%% *} a second; busiest: ${tree#* }")"

  step "one tab across the engines: history.html in Servo, an app in Wasm, wpe.html in WPE"
  # The new tab's first segment is its home page, sig://home: (None, 1, 0).
  key -M ctrl t -m ctrl
  key "$base/history.html"
  key -k Return
  check "a page on 127.0.0.1 opens in Servo, the default engine" "$(sess 20 "segs(4)[1:]==[('servo', 3, 2)], segs(4)")"
  open "$base/guests/gradient-demo.wasm"
  check "an app opens in the Wasm engine, a segment of its own" \
    "$(sess 15 "segs(4)[1:]==[('servo', 3, 2), ('wasm', 1, 0)], segs(4)")"
  check "the tab shows the Wasm view" "$(shows_tab 4 wasm)"
  open "$lh/wpe.html"
  check "a page on localhost opens in WPE, a third segment" \
    "$(sess 15 "segs(4)[1:]==[('servo', 3, 2), ('wasm', 1, 0), ('wpe', 1, 0)] and url(4)=='$lh/wpe.html', (segs(4), url(4))")"
  check "the tab shows the WPE view" "$(shows_tab 4 wpe)"
  shot 05-three-engines 1
  step "back three times, forward three times"
  key -M alt -k Left -m alt
  check "back: the app" "$(sess 10 "tab(4)['current']==2 and url(4)=='$base/guests/gradient-demo.wasm', (tab(4)['current'], url(4))")"
  check "shown in its Wasm view" "$(shows_tab 4 wasm)"
  key -M alt -k Left -m alt
  check "back: the Servo page at #two" "$(sess 10 "tab(4)['current']==1 and url(4)=='$base/history.html#two', (tab(4)['current'], url(4))")"
  check "shown in its Servo view" "$(shows_tab 4 servo)"
  key -M alt -k Left -m alt
  check "back within Servo: #one" "$(sess 10 "tab(4)['current']==1 and url(4)=='$base/history.html#one', (tab(4)['current'], url(4))")"
  key -M alt -k Right -m alt
  check "forward within Servo: #two" "$(sess 10 "tab(4)['current']==1 and url(4)=='$base/history.html#two', (tab(4)['current'], url(4))")"
  key -M alt -k Right -m alt
  check "forward: the app" "$(sess 10 "tab(4)['current']==2 and url(4)=='$base/guests/gradient-demo.wasm', (tab(4)['current'], url(4))")"
  check "shown in its Wasm view" "$(shows_tab 4 wasm)"
  key -M alt -k Right -m alt
  check "forward: the WPE page" "$(sess 10 "tab(4)['current']==3 and url(4)=='$lh/wpe.html', (tab(4)['current'], url(4))")"
  check "shown in its WPE view" "$(shows_tab 4 wpe)"
  check "no segment was loaded again" "$(answer "$([[ $(served /history.html) == 1 && $(served /wpe.html) == 1 && $(served /guests/gradient-demo.wasm) == 1 ]]; echo $?)" \
    "requests: history.html $(served /history.html), wpe.html $(served /wpe.html), the app $(served /guests/gradient-demo.wasm)")"

  step "the WPE page's links: to a page the routing gives Servo, to one on its own host, and a script's"
  sleep 0.5
  click 300 $((chrome_h + 75))
  check "a click on a link to a page the routing gives Servo opens it in Servo" "$(shows_tab 4 servo)"
  check "as a segment after the WPE page" \
    "$(sess 15 "segs(4)[1:]==[('servo', 3, 2), ('wasm', 1, 0), ('wpe', 1, 0), ('servo', 1, 0)] and url(4)=='$base/second.html?from-wpe', (segs(4), url(4))")"
  check "a hidden WPE view is kept alone: the Servo page and the app before it became records" \
    "$(dg 5 "sorted(v['engine'] for v in views if v['tab']==4)==['servo', 'wpe'], [(v['engine'], v['shown']) for v in views if v['tab']==4]")"
  key -M alt -k Left -m alt
  check "back returns to the WPE page" "$(shows_tab 4 wpe)"
  check "which was kept, not loaded again" "$(answer "$([[ $(served /wpe.html) == 1 ]]; echo $?)" "$(served /wpe.html) request(s)")"
  sleep 0.5
  click 300 $((chrome_h + 225))
  check "a link to a page on its own host stays in WPE" \
    "$(sess 15 "url(4)=='$lh/second.html?in-wpe' and seg(4)['engine']=='wpe' and len(entries(4))==2, (segs(4), url(4))")"
  key -M alt -k Left -m alt
  check "back within WPE: the WPE page" "$(sess 10 "url(4)=='$lh/wpe.html', url(4)")"
  sleep 0.5
  click 300 $((chrome_h + 375))
  check "a script's navigation stays in WPE, even after a click" \
    "$(sess 15 "url(4)=='$base/second.html?from-wpe-script' and seg(4)['engine']=='wpe', (segs(4), url(4))")"
  step "links.html in Servo: a click on its link to a page on localhost"
  open "$base/links.html"
  check "links.html opens in Servo" "$(sess 15 "url(4)=='$base/links.html' and seg(4)['engine']=='servo', (segs(4), url(4))")"
  sleep 0.5
  click 200 $((chrome_h + 525))
  check "a click in Servo on a link to a host the routing gives WPE opens it in WebKit" \
    "$(sess 15 "url(4)=='$lh/second.html?chosen' and seg(4)['engine']=='wpe', (segs(4), url(4))")"
  check "the tab shows its WPE view" "$(shows_tab 4 wpe)"
  shot 06-from-servo-to-wpe 0.5
  held=$(grep -o 'wpe-timing navigation-answered view=[0-9]* held_ms=[0-9.]*' "$out/sig.log" | grep -o '[0-9.]*$' | tr '\n' ' ')
  info "how long WebKit held each navigation for the core's answer, in ms: $held"
  check "WebKit asked about the two links clicked in it, not the script's navigation" \
    "$(answer "$([[ $(wc -w <<< "$held") == 2 ]]; echo $?)" "$(wc -w <<< "$held") answered")"

  step "WebKit's web processes killed, then F5 in the first tab"
  key -M alt 1 -m alt
  check "alt-1 shows the first tab" "$(shows_tab 1 wpe)"
  descendants() { local child; for child in $(pgrep -P "$1"); do echo "$child"; descendants "$child"; done; }
  web=$(for p in $(descendants "$sig_pid"); do [[ $(cat "/proc/$p/comm" 2>/dev/null) == WPEWebProcess ]] && echo "$p"; done)
  info "WebKit's web processes: $(wc -w <<< "$web")"
  # SIGKILL: JavaScriptCore's handler in the web process returns from a SIGSEGV sent by kill(2).
  kill -KILL $web
  check "the tab says its page failed" "$(dg 5 "tabs[0]['error'] and d['status']=='error', (tabs[0], d['status'])")"
  check "sig lives on" "$(answer "$(kill -0 "$sig_pid"; echo $?)" "pid $sig_pid")"
  shot 07-crashed 0.3
  before=$(value "shown['submissions']")
  key -k F5
  check "F5 starts a new web process and loads the page again" \
    "$(dg 15 "not tabs[0]['error'] and shown and shown['tab']==1 and shown['engine']=='wpe' and shown['submissions'] > $before, (tabs[0], shown and shown['submissions'], $before)")"
  again=$(for p in $(descendants "$sig_pid"); do [[ $(cat "/proc/$p/comm" 2>/dev/null) == WPEWebProcess ]] && echo "$p"; done)
  check "in a web process of its own" "$(answer "$([[ -n $again ]] && ! grep -qxF -f <(tr ' ' '\n' <<< "$web") <<< "$again"; echo $?)" "killed: $(echo $web), now: $(echo $again)")"
  shot 08-reloaded 0.5
  key -M ctrl t -m ctrl
  key "$base/second.html"
  key -k Return
  check "Servo goes on: a new tab opens a page in it" "$(shows_tab 5 servo)"

  step "every tab with a WPE page closed: WebKit's web processes end with them"
  for _ in 1 2 3 4; do key -M alt 1 -m alt; key -M ctrl w -m ctrl; done
  check "only the Servo tab is left" "$(dg 5 "len(tabs)==1 and shown and shown['engine']=='servo', (len(tabs), shown and shown['engine'])")"
  web_left() { for p in $(descendants "$sig_pid"); do [[ $(cat "/proc/$p/comm" 2>/dev/null) == WPEWebProcess ]] && echo "$p"; done; }
  for waited in $(seq 0 10); do [[ -z $(web_left) ]] && break; sleep 0.5; done
  check "no WebKit web process outlives the last WPE tab by more than 5 s" \
    "$(answer "$([[ -z $(web_left) ]]; echo $?)" "$(wc -w <<< "$(web_left)") left after $((waited / 2)).$((waited % 2 * 5)) s")"
  info "WebKit's network process stays while the runtime does: $(for p in $(descendants "$sig_pid"); do cat "/proc/$p/comm" 2>/dev/null; done | grep -c WPENetwork)"

  step "ctrl-q"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  check "zero CPU copies of WPE frames, all DMA-BUF" \
    "$(python3 -c "import json; d=json.load(open('$out/diag.json')); w=[e for e in d['engines'] if e['name']=='wpe'][0]; print(0 if w['readbacks']==0 else 1, 'wpe copies', w['readbacks'], 'gl', w['gl'])")"
  frames=$(grep -c 'sig-diag .* frame engine=wpe' "$out/sig.log")
  clean=$(grep -c 'sig-diag .* frame engine=wpe .*readback_bytes=0 ' "$out/sig.log")
  check "every WPE frame line says readback_bytes=0" "$(answer "$([[ $frames -gt 0 && $frames == "$clean" ]]; echo $?)" "$clean of $frames")"
  kept=$(find "$out/data/sighurt/wpe" -type f ! -name lock 2>/dev/null | sed "s|$out/data/sighurt/wpe/||" | tr '\n' ' ')
  check "WebKit kept its profile in the data directory" "$(answer "$([[ -n $kept ]]; echo $?)" "${kept:-nothing}")"
  ;;
showcase)
  lh=http://localhost:$(cat "$out/port")
  config=$out/config/sighurt/config.toml
  # The modal keymap instead of [keys], the standard chrome with its tabs on the left at 16 px, a
  # site for WPE and a search keyword.
  cat > "$config" <<CONF
version = 1
[downloads]
directory = "$out/downloads"
[routing.wpe]
hosts = ["localhost"]
[keywords]
t = "$base/count.html?q={}"
[input]
keymap = "modal"
[keymap.modal]
start = "normal"
typing = "insert"
pass = ["insert"]
[keymap.modal.always]
"ctrl-q" = "browser.quit"
[keymap.modal.normal]
"g t" = "tab.next"
"g T" = "tab.previous"
"o" = "location.focus"
":" = { run = "location.focus", pass = true }
"i" = "mode insert"
"b" = "chrome.toggle-tabs"
[keymap.modal.insert]
"escape" = "mode normal"
[ui]
chrome = "standard"
font-size = 16
[chrome.standard]
tabs = "left"
CONF
  # At 16 px every height of the standard chrome is its 14 px height times 16 / 14, rounded:
  # the toolbar 44 → 50, the status strip 20 → 23, and the tab list's width 220 → 251.
  toolbar=50 strip=23 list=251
  surfaces="[(x['role'], x['mapped'], x['width'], x['height']) for x in t if x['role'] not in ('view', 'retiring')]"
  step "input.html on localhost and second.html on 127.0.0.1"
  start_sig "$lh/input.html" "$base/second.html"
  check "the configuration is valid: no notice" "$(dg 3 "d['status'] is None, d['status']")"
  check "a page on localhost, which [routing.wpe] hosts claims, opens in WebKit" \
    "$(sess 30 "seg(1)['engine']=='wpe' and url(1).startswith('$lh/input.html#'), (segs(1), url(1))")"
  check "a page on 127.0.0.1 opens in Servo" "$(sess 20 "seg(2)['engine']=='servo' and url(2)=='$base/second.html', (segs(2), url(2))")"
  check "the tab list is on the left, and font-size 16 sizes the chrome" \
    "$(dg 5 "surface('tabs')['mapped'] and (surface('tabs')['width'], surface('tabs')['height'])==($list, 800 - $toolbar - $strip) and surface('chrome')['height']==$toolbar and surface('status')['height']==$strip, $surfaces")"
  check "the page beside it" "$(dg 5 "shown and (shown['width'], shown['height'])==(1280 - $list, 800 - $toolbar - $strip), shown and (shown['width'], shown['height'])")"
  point 1000 700
  shot 01-left-tabs-16px-normal 1

  step "the modal keymap: g t, then g T"
  key g
  key t
  check "g t shows the next tab" "$(shows_tab 2 servo)"
  key g
  key -M shift t -m shift
  check "g T shows the one before" "$(shows_tab 1 wpe)"
  step "a click on the WPE page's field enters insert mode: åäö; Escape, then z in normal mode"
  click $((list + 100)) $((toolbar + 215))
  key 'åäö'
  check "insert mode: what is typed reaches WebKit's field" "$(sess 8 "url(1).endswith('#field åäö'), url(1).split('#')[1]")"
  shot 02-insert-mode 0.3
  key -k Escape
  key z
  sleep 0.5
  check "normal mode: an unbound letter is swallowed" "$(sess 2 "url(1).endswith('#field åäö'), url(1).split('#')[1]")"
  shot 03-normal-mode 0.3

  step "o (location.focus), a keyword search: t åäö"
  key o
  key 't åäö'
  key -k Return
  check "a [keywords] template opens with the rest as its query, in Servo" \
    "$(sess 15 "url(1)=='$base/count.html?q=åäö' and seg(1)['engine']=='servo', (segs(1), url(1))")"
  check "the tab shows its Servo view" "$(shows_tab 1 servo)"
  step "o, an address on localhost typed: WPE again"
  key o
  key "$lh/second.html"
  key -k Return
  check "a typed address on a host the routing gives WPE opens there" \
    "$(sess 15 "url(1)=='$lh/second.html' and seg(1)['engine']=='wpe', (segs(1), url(1))")"
  check "the tab shows its WPE view" "$(shows_tab 1 wpe)"
  step ": then tab.select 2, typed after the colon the key put there"
  key :
  key 'tab.select 2'
  key -k Return
  check "a command line opened with :" "$(shows_tab 2 servo)"

  step "b: chrome.toggle-tabs, twice"
  key b
  check "chrome.toggle-tabs hides the tab list, and the page takes its room" \
    "$(dg 5 "not surface('tabs')['mapped'] and shown and shown['width']==1280, ($surfaces, shown and shown['width'])")"
  key b
  check "and shows it again" "$(dg 5 "surface('tabs')['mapped'] and shown and shown['width']==1280 - $list, ($surfaces, shown and shown['width'])")"

  step "a binding and a color added while sig runs"
  point 1000 700
  key n
  check "n is bound to nothing yet" "$(dg 3 "len(tabs)==2, len(tabs)")"
  purple() {
    grim -t ppm "$out/$1.ppm"
    python3 "$probe" png "$out/$1.ppm" "$out/$1.png"
    python3 "$probe" count "$out/$1.ppm" 0 "$toolbar" "$list" $((800 - strip)) 124 58 237
  }
  before=$(purple 04-before-reload)
  sed -i 's|^"b" = "chrome.toggle-tabs"$|&\n"n" = "tab.new"|; s|^\[ui\]$|[ui.colors]\ntab = "#7c3aed"\n&|' "$config"
  sleep 0.7
  check "the new file is valid: no notice" "$(dg 3 "d['status'] is None, d['status']")"
  after=$(purple 05-after-reload)
  check "the new tab color applies at once: the active row of the list" \
    "$(answer "$([[ $before == 0 && $after -gt 2000 ]]; echo $?)" "pixels of #7c3aed in the tab list: $before before, $after after")"
  key n
  check "the new binding applies at once: n opens a tab" "$(dg 5 "len(tabs)==3 and d['active_tab']==3, (len(tabs), d['active_tab'])")"
  shot 06-new-tab-by-new-binding
  key -k Escape

  step "[ui] chrome = \"line\" while sig runs: a notice, since a chrome changes at the next start"
  sed -i 's|^chrome = "standard"$|chrome = "line"|; /^"b" = "chrome.toggle-tabs"$/d' "$config"
  check "a new chrome gets a notice that it applies at restart" "$(dg 5 "d['status']=='notice', d['status']")"
  check "the standard chrome stays meanwhile" "$(dg 3 "surface('tabs')['mapped'] and surface('line') is None, $surfaces")"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q, bound in always, exits cleanly" "$(answer "$code" "exit code $code")"
  check "the session holds the three tabs" \
    "$(python3 "$probe" session "$session" "len(s['tabs'])==3 and [seg(i).get('engine') for i in (1, 2)]==['wpe', 'servo'], [(seg(i).get('engine'), url(i)) for i in range(1, len(s['tabs']) + 1)]")"

  step "started again: the line chrome, the same keymap; the three tabs restored"
  # The line is 28 px at 14 px, so 32 at 16.
  line=32
  start_sig
  check "the configuration is valid: no notice" "$(dg 3 "d['status'] is None, d['status']")"
  check "the line chrome: one line at the top, 32 px at font-size 16" \
    "$(dg 5 "[x['role'] for x in t if x['role'] not in ('view', 'retiring', 'page')]==['line'] and (surface('line')['width'], surface('line')['height'])==(1280, $line), $surfaces")"
  step "g T: from the home page in the third tab to the second"
  key g
  key -M shift t -m shift
  check "the modal keymap drives it: g T" "$(shows_tab 2 servo)"
  check "and the page has the window but the line" "$(dg 5 "(shown['width'], shown['height'])==(1280, 800 - $line), (shown['width'], shown['height'])")"
  point 1000 700
  shot 07-line-chrome 1
  step "o, an address on localhost typed into the line"
  key o
  key "$lh/input.html"
  key -k Return
  check "the line opens a typed address on localhost in WPE" \
    "$(sess 15 "url(2).startswith('$lh/input.html') and seg(2)['engine']=='wpe', (segs(2), url(2))")"
  check "the tab shows its WPE view" "$(shows_tab 2 wpe)"
  shot 08-line-chrome-wpe 1
  step ": then tab.select 1"
  key :
  key 'tab.select 1'
  key -k Return
  check "a command line in the line chrome" "$(shows_tab 1 wpe)"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  ;;

labels)
  lh=http://localhost:$(cat "$out/port")
  config=$out/config/sighurt/config.toml
  cat >> "$config" <<CONF
[routing.wpe]
hosts = ["localhost"]
CONF
  # At scale 1 in the 1280 × 800 window: three tabs of 220 px from x = 4, 4 px apart, in the
  # 40 px strip; the address field from x = 104 to 1272, y = 46 to 78, its right end the label's.
  tab_x=(4 228 452)
  shot_ppm() { grim -t ppm "$out/$1.ppm"; python3 "$probe" png "$out/$1.ppm" "$out/$1.png"; step "screenshot $1"; }
  # changed A B X0 Y0 X1 Y1: the pixels of the rectangle that differ between screenshots A and B.
  changed() { python3 "$probe" differ "$out/$1.ppm" "$out/$2.ppm" "$3" "$4" "$5" "$6"; }
  in_tab() { changed "$1" "$2" "${tab_x[$3]}" 4 $((tab_x[$3] + 220)) 36; }
  step "a Servo page, a page on localhost, which [routing.wpe] hosts gives WPE, and sig://home"
  start_sig "$base/second.html" "$lh/second.html" sig://home
  check "the first page opens in Servo, the second in WPE" \
    "$(sess 30 "seg(1)['engine']=='servo' and url(1)=='$base/second.html' and seg(2)['engine']=='wpe' and url(2)=='$lh/second.html', (segs(1), segs(2))")"
  check "sig tells the chrome servo, wpe and no engine for sig://home" \
    "$(dg 10 "[x['engine'] for x in tabs]==['servo', 'wpe', None], [x['engine'] for x in tabs]")"
  check "the Servo page is shown" "$(shows_tab 1 servo)"
  point 1000 700
  sleep 1
  shot_ppm 01-servo-labels
  key -M alt 2 -m alt
  check "alt-2 shows the WPE page" "$(shows_tab 2 wpe)"
  sleep 1
  shot_ppm 02-wpe-labels
  key -M alt 3 -m alt
  check "alt-3 shows sig://home" "$(dg 5 "d['active_tab']==3 and shown is None, (d['active_tab'], shown)")"
  sleep 1
  shot_ppm 03-home-labels

  step "[chrome.standard] engine-label = false while sig runs"
  printf '[chrome.standard]\nengine-label = false\n' >> "$config"
  sleep 0.7
  check "the new file is valid: no notice" "$(dg 3 "d['status'] is None, d['status']")"
  sleep 0.5
  shot_ppm 04-home-no-labels
  servo=$(in_tab 03-home-labels 04-home-no-labels 0) wpe=$(in_tab 03-home-labels 04-home-no-labels 1)
  home=$(in_tab 03-home-labels 04-home-no-labels 2)
  check "the Servo and WPE tabs had labels, which go at once; the home tab had none" \
    "$(answer "$([[ $servo -gt 20 && $wpe -gt 20 && $home == 0 ]]; echo $?)" "pixels changed: $servo in the Servo tab, $wpe in the WPE tab, $home in the home tab")"
  address=$(changed 03-home-labels 04-home-no-labels 104 46 1272 78)
  check "sig://home has no label in the address field" "$(answer "$([[ $address == 0 ]]; echo $?)" "$address pixels changed")"
  key -M alt 1 -m alt
  check "alt-1 shows the Servo page again" "$(shows_tab 1 servo)"
  sleep 1
  shot_ppm 05-servo-no-labels
  key -M alt 2 -m alt
  check "alt-2 shows the WPE page again" "$(shows_tab 2 wpe)"
  sleep 1
  shot_ppm 06-wpe-no-labels
  servo=$(changed 01-servo-labels 05-servo-no-labels 1100 46 1272 78)
  wpe=$(changed 02-wpe-labels 06-wpe-no-labels 1100 46 1272 78)
  text=$(changed 01-servo-labels 05-servo-no-labels 104 46 1100 78)
  check "the address field's right end named the shown page's engine, and its text stayed" \
    "$(answer "$([[ $servo -gt 20 && $wpe -gt 20 && $text == 0 ]]; echo $?)" "pixels changed at its right end: $servo with Servo, $wpe with WPE; $text in the rest")"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"

  step "started again with the line chrome and engine-label on: the WPE tab restored"
  sed -i '/^\[chrome.standard\]$/,$d' "$config"
  printf '[ui]\nchrome = "line"\n' >> "$config"
  start_sig
  check "the configuration is valid: no notice" "$(dg 3 "d['status'] is None, d['status']")"
  check "the WPE page is shown, and labelled wpe" \
    "$(dg 30 "shown is not None and shown['engine']=='wpe' and tabs[d['active_tab'] - 1]['engine']=='wpe', (shown and shown['engine'], [x['engine'] for x in tabs])")"
  point 1000 700
  sleep 1
  shot_ppm 07-line-wpe
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  ;;

switch)
  step "https://example.com/ opens in Servo, the default engine"
  start_sig "https://example.com/"
  check "example.com opens in Servo" \
    "$(sess 30 "url(1)=='https://example.com/' and seg(1)['engine']=='servo' and seg(1).get('title')=='Example Domain', (seg(1)['engine'], url(1), seg(1).get('title'))")"
  check "the tab shows its Servo view" "$(shows_tab 1 servo)"
  step "page.switch-engine reopens it in the next web engine, WPE"
  open ":page.switch-engine"
  check "page.switch-engine opens example.com in WPE" \
    "$(sess 30 "url(1)=='https://example.com/' and seg(1)['engine']=='wpe' and segs(1)==[('servo', 1, 0), ('wpe', 1, 0)], (segs(1), url(1))")"
  check "the tab shows its WPE view" "$(shows_tab 1 wpe)"
  step "page.switch-engine again returns it to Servo"
  open ":page.switch-engine"
  check "page.switch-engine opens example.com in Servo again" \
    "$(sess 30 "url(1)=='https://example.com/' and seg(1)['engine']=='servo' and segs(1)==[('servo', 1, 0), ('wpe', 1, 0), ('servo', 1, 0)], (segs(1), url(1))")"
  check "the tab shows its Servo view again" "$(shows_tab 1 servo)"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$(answer "$code" "exit code $code")"
  ;;
esac
finish
