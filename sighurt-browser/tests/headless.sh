#!/usr/bin/env bash
# End-to-end tests of `sig` with the demo engine, in a private headless sway: never on the
# desktop, since they type and click. Builds `sig` (--no-default-features --features
# engine-demo) and the virtual-pointer helper with tools/cargo, then runs the checks below inside
# tools/headless-sway, and prints PASS or FAIL for each. The checks read `sig`'s diagnostics
# (which tab and view are shown, what the status strip shows, counters and peaks), its session
# file and log, and screenshot pixels (the resize page's crosshairs). Screenshots, `sig`'s log,
# its diagnostics and sway's log go to OUT; look at the screenshots too.
#
#   sighurt-browser/tests/headless.sh OUT                  build, then run every scenario
#   sighurt-browser/tests/headless.sh --no-build OUT       run with the binaries in target/
#   sighurt-browser/tests/headless.sh OUT config close     only these scenarios
#
# Scenarios, each with a fresh configuration and session directory:
#   main        demo pages; switching tabs by click and by configured keys; typing into the
#               address bar and a page (å, ä, ö); pointer coordinates and motion coalescing;
#               resize, scale; the recoverable errors; animation without chrome repaints; idle
#               without wakeups; a clean quit
#   config      hot reload: a new binding and color apply at once; an invalid file or an unknown
#               color keeps the configuration in use and says why, before the page's status
#   close       the compositor's close request (swaymsg kill) quits and saves the session
#   disconnect  the compositor going away: sig saves the session and exits with 1
#
# Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: headless.sh [--no-build] OUT [SCENARIO ...]}")
  shift
  scenarios=("$@")
  [[ ${#scenarios[@]} == 0 ]] && scenarios=(main config close disconnect)
  mkdir -p "$out"
  if [[ $build == 1 ]]; then
    SIG_WHAT="build sig (demo engine) for headless tests" tools/cargo build -p sighurt-browser \
      --no-default-features --features engine-demo --bins --example virtual-pointer || exit 1
  fi
  failed=0
  for scenario in "${scenarios[@]}"; do
    tools/heavy tools/headless-sway -- "$0" --inside "$out/$scenario" "$scenario"
    failed=$((failed + $?))
  done
  echo "headless.sh: $failed failed check(s); screenshots and logs in $out"
  exit $failed
fi

# ── Inside the private compositor ─────────────────────────────────────────────────────────
out=$2 scenario=$3
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "headless.sh: run me through tools/headless-sway" >&2
  exit 2
}
rm -rf "$out"
mkdir -p "$out/config/sighurt" "$out/state" "$out/downloads"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_DOWNLOAD_DIR=$out/downloads
export SIG_DIAG=1 RUST_LOG=${RUST_LOG:-info}
cat > "$out/config/sighurt/config.toml" <<'CONF'
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
[keys]
"ctrl-t" = "tab.new"
"ctrl-w" = "tab.close"
"ctrl-tab" = "tab.next"
"alt-1" = "tab.select 1"
"alt-2" = "tab.select 2"
"alt-3" = "tab.select 3"
"ctrl-l" = "location.focus"
"f5" = "page.reload"
"ctrl-q" = "browser.quit"
CONF

failures=0
check() { # check NAME CONDITION-EXIT-STATUS DETAIL
  if [[ $2 == 0 ]]; then echo "PASS $scenario: $1 ($3)"; else echo "FAIL $scenario: $1 ($3)"; failures=$((failures + 1)); fi
}
step() { echo "[$(date +%T.%3N)] $*" >> "$out/steps.log"; }
shot() { sleep "${2:-0.5}"; grim "$out/$1.png"; step "screenshot $1"; }

# Input devices that stay plugged in (wtype and wlrctl plug in a new device per command, so
# sig could miss events sent before it has bound the new wl_keyboard or wl_pointer).
wtype -s 3600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
target/debug/examples/virtual-pointer < "$XDG_RUNTIME_DIR/pointer" &
pointer_pid=$!
exec 3> "$XDG_RUNTIME_DIR/pointer"
layout_w=1280 layout_h=800
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
click() { point "$1" "$2"; sleep 0.1; echo click >&3; sleep 0.2; }
key() { wtype "$@"; sleep 0.25; }

sig_pid=
start_sig() {
  target/debug/sig --diagnostics "$out/diag.json" "$@" > "$out/sig.log" 2>&1 &
  sig_pid=$!
  sleep 2
}
# diag PYTHON-EXPRESSION: asks sig for its diagnostics (SIGUSR1) and evaluates the expression
# with `d` the JSON, `c` its counters and `t` its targets.
diag() {
  local before
  before=$(stat -c %Y.%y "$out/diag.json" 2>/dev/null)
  kill -USR1 "$sig_pid"
  for _ in $(seq 50); do
    [[ $(stat -c %Y.%y "$out/diag.json" 2>/dev/null) != "$before" ]] && break
    sleep 0.02
  done
  sleep 0.05
  python3 -c "import json; d=json.load(open('$out/diag.json')); c=d['counters']; t=d['targets']; v=($1); print(json.dumps(v) if isinstance(v, tuple) else v)"
}
# shows_tab N: 0 if tab N is active and its view the one shown.
shows_tab() {
  diag "0 if d['active_tab']==$1 and [x['tab'] for x in t if x['role']=='view' and x['shown']]==[$1] else 1"
}
tabs_now() { diag "d['active_tab'], [(x['tab'], x['shown']) for x in t if x['role']=='view']"; }
# crosshair NAME X Y: takes screenshot NAME.ppm and prints 0 if the resize page's crosshairs
# cross at output pixel (X, Y), give or take a pixel (its vertical line 40 pixels below, its
# horizontal line 40 pixels to the left), then what it found. A line counts where blue
# dominates: at fractional scales the compositor's filtering blends a one-pixel line a little.
crosshair() {
  grim -t ppm "$out/$1.ppm"
  python3 - "$out/$1.ppm" "$2" "$3" <<'PY'
import re, sys
data = open(sys.argv[1], 'rb').read()
m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', data)
w, pixels = int(m[1]), data[m.end():]
at = lambda x, y: pixels[3 * (y * w + x):3 * (y * w + x) + 3]
blue = lambda p: p[2] - p[0] > 128  # the crosshairs are #2563eb, the checkerboard grey
x, y = int(sys.argv[2]), int(sys.argv[3])
columns = [c for c in range(x - 1, x + 2) if blue(at(c, y + 40))]
rows = [r for r in range(y - 1, y + 2) if blue(at(x - 40, r))]
print(0 if columns and rows else 1, f"crosshair columns {columns}, rows {rows}")
PY
}
finish() {
  exec 3>&-
  kill "$keyboard" "$pointer_pid" 2>/dev/null
  wait "$keyboard" "$pointer_pid" 2>/dev/null
  grep -q 'panicked' "$out/sig.log" && check "no panic" 1 "see sig.log"
  cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null
  exit $failures
}

case $scenario in
main)
  start_sig demo://pattern demo://animation demo://resize
  shot 01-pattern-tab-1
  check "three tabs, the first shown" "$(( $(diag "0 if sum(1 for x in t if x['role']=='view')==3 else 1") + $(shows_tab 1) ))" \
    "$(tabs_now) (active tab, [(view's tab, shown)])"
  check "renderer line: the demo engine paints on the CPU" \
    "$(grep -q 'renderer engine=demo mode=cpu$' "$out/sig.log"; echo $?)" "$(grep -o 'renderer engine=.*' "$out/sig.log")"

  step "alt-2: the animation tab by a configured key"
  key -M alt 2 -m alt
  sleep 1
  check "alt-2 shows the second tab" "$(shows_tab 2)" "$(tabs_now)"
  check "background tabs report no navigation time" \
    "$([[ $(grep -c ' first-frame ' "$out/sig.log") == 1 ]]; echo $?)" \
    "$(grep -o 'first-frame .*' "$out/sig.log" | tr '\n' ';') (only the first tab's)"
  before=$(diag "c['page_frames_presented'], c['chrome_paints']")
  sleep 2
  after=$(diag "c['page_frames_presented'], c['chrome_paints']")
  shot 02-animation-tab-2 0
  frames=$(python3 -c "a=$before; b=$after; print(b[0]-a[0])")
  paints=$(python3 -c "a=$before; b=$after; print(b[1]-a[1])")
  check "the animation runs at the compositor's pace" "$([[ $frames -ge 60 ]]; echo $?)" "$frames page frames in 2 s"
  check "page animation repaints no chrome" "$([[ $paints == 0 ]]; echo $?)" "$paints chrome paints in 2 s"

  step "click the third tab"
  click 560 20
  shot 03-resize-tab-3-by-click
  check "a click shows the third tab" "$(shows_tab 3)" "$(tabs_now)"
  step "pointer over the page at window (300, 300): page-local (300, 216)"
  point 300 300
  shot 04-resize-pointer-300-216
  found=$(crosshair 04-resize-pointer-300-216 300 300)
  check "the pointer is where the page draws it at 100 %" "${found%% *}" "window (300, 300): ${found#* }"
  before=$(diag "c['page_frames_presented']")
  sleep 1
  after=$(diag "c['page_frames_presented']")
  check "a hidden animation presents nothing" "$([[ $before == "$after" ]]; echo $?)" \
    "$((after - before)) page frames in 1 s on a static page with the animation in the background"

  step "sig stopped while the pointer moves 30 times over the page: the motion is coalesced"
  before=$(diag "c['motions_coalesced']")
  kill -STOP "$sig_pid"
  for i in $(seq 30); do point $((300 + i * 5)) 300; done
  kill -CONT "$sig_pid"
  sleep 0.5
  after=$(diag "c['motions_coalesced']")
  check "queued pointer motion is coalesced" "$([[ $((after - before)) -ge 20 ]]; echo $?)" \
    "$((after - before)) of 30 motion events dropped for a newer one"

  step "ctrl-tab: to the first tab"
  key -M ctrl -k Tab -m ctrl
  shot 05-pattern-by-ctrl-tab
  check "ctrl-tab shows the first tab" "$(shows_tab 1)" "$(tabs_now)"
  step "alt-3, then ctrl-l and type åäö into the address bar"
  key -M alt 3 -m alt
  check "alt-3 shows the third tab" "$(shows_tab 3)" "$(tabs_now)"
  key -M ctrl l -m ctrl
  key 'åäö'
  shot 06-address-typed-aao
  step "Escape discards; ctrl-l, demo://slow?åäö, Enter"
  key -k Escape
  key -M ctrl l -m ctrl
  key 'demo://slow?åäö'
  key -k Return
  shot 07-slow-opened 1
  check "the typed address opens in the third tab" "$(shows_tab 3)" "$(tabs_now)"
  step "click the page, type hej åäö into it"
  click 640 400
  key 'hej åäö'
  shot 08-page-typed-into

  step "resize the output to 1000x700: the slow page's frame is late"
  swaymsg -q output HEADLESS-1 resolution 1000x700
  layout_w=1000 layout_h=700
  shot 09-resized-slow-fallback 0.15
  shot 10-resized-slow-frame 1
  step "resize storm: 20 sizes"
  for i in $(seq 20); do
    swaymsg -q output HEADLESS-1 resolution $((800 + i * 20))x$((600 + i * 10)); sleep 0.05
  done
  swaymsg -q output HEADLESS-1 resolution 1280x800
  layout_w=1280 layout_h=800
  sleep 1.5
  # The host's bound: 3 buffers of the current size and at most 2 retiring generations.
  check "buffers stay bounded through the storm (peaks)" \
    "$(diag "0 if d['peaks']['max_target_buffers']<=9 and d['peaks']['max_retiring_generations']<=2 else 1")" \
    "$(diag "d['peaks']['max_target_buffers'], d['peaks']['max_retiring_generations'], d['peaks']['max_buffer_bytes']") (most buffers of one target, retiring generations, bytes of all, at any time)"
  check "and the old ones are released after it" \
    "$(diag "0 if all(x['buffers']<=3 and x['retiring_generations']==0 for x in t) else 1")" \
    "$(diag "max(x['buffers'] for x in t), max(x['retiring_generations'] for x in t)") (most buffers, retiring generations now)"

  step "scale 1.5"
  swaymsg -q output HEADLESS-1 scale 1.5
  layout_w=853 layout_h=533
  sleep 1
  key -M alt 3 -m alt
  shot 11-scale-150-slow 1
  check "buffers follow the scale" \
    "$(diag "0 if all(x['buffer_width']==int(x['width']*1.5+0.5) and x['scale']==180 for x in t if x['mapped']) else 1")" \
    "$(diag "[(x['role'], x['width'], x['buffer_width']) for x in t if x['mapped']]")"
  step "back to the resize page, pointer at layout (400, 300): page-local (400, 216), buffer (600, 324)"
  key -M alt 1 -m alt
  key -M ctrl l -m ctrl
  key 'demo://resize'
  key -k Return
  sleep 0.5
  point 400 300
  shot 12-scale-150-resize-pointer
  found=$(crosshair 12-scale-150-resize-pointer 600 450)
  check "the pointer is where the page draws it at 150 %" "${found%% *}" "layout (400, 300) = output (600, 450): ${found#* }"
  swaymsg -q output HEADLESS-1 scale 1
  layout_w=1280 layout_h=800
  sleep 0.5

  step "demo://error in the second tab: the navigation fails and the animation stays"
  key -M alt 2 -m alt
  key -M ctrl l -m ctrl
  key 'demo://error'
  key -k Return
  shot 13-error-page-stays
  check "a failed navigation keeps the page and says why" \
    "$(( $(diag "0 if d['status']=='error' else 1") + $(shows_tab 2) ))" "$(diag "d['status']") in the status strip; $(tabs_now)"
  key -M alt 1 -m alt
  step "demo://crash, click it: the view fails; F5 reloads it"
  key -M ctrl l -m ctrl
  key 'demo://crash'
  key -k Return
  sleep 0.5
  click 640 400
  shot 14-crashed
  check "a crashed view stays and the status strip says why" \
    "$(( $(diag "0 if d['status']=='error' else 1") + $(shows_tab 1) ))" "$(diag "d['status']"); $(tabs_now)"
  key -k F5
  shot 15-reloaded
  check "F5 recovers it" "$(( $(diag "0 if d['status']!='error' else 1") + $(shows_tab 1) ))" \
    "$(diag "d['status']"); $(tabs_now)"

  step "idle: the pattern tab, nothing happening"
  key -M alt 2 -m alt
  key -M alt 1 -m alt
  key -M ctrl l -m ctrl
  key 'demo://pattern'
  key -k Return
  point 640 790
  sleep 1.5
  before=$(diag "c['wakeups'], c['page_frames_presented'], c['chrome_paints'], c['iterations']")
  sleep 3
  after=$(diag "c['wakeups'], c['page_frames_presented'], c['chrome_paints'], c['iterations']")
  wakeups=$(python3 -c "a=$before; b=$after; print(b[0]-a[0])")
  iterations=$(python3 -c "a=$before; b=$after; print(b[3]-a[3])")
  work=$(python3 -c "a=$before; b=$after; print(b[1]-a[1], b[2]-a[2])")
  # The one wakeup is the signal that asked for the second report.
  check "idle: no wakeups in 3 s" "$([[ $wakeups -le 1 ]]; echo $?)" "$wakeups wakeup(s), the report's own included"
  check "idle: no loop iterations in 3 s" "$([[ $iterations -le 2 ]]; echo $?)" "$iterations iteration(s), the report's own included"
  check "idle: nothing painted" "$([[ $work == '0 0' ]]; echo $?)" "page frames, chrome paints: $work"
  shot 16-idle-pattern 0

  step "ctrl-q quits"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$code" "exit code $code"
  session=$out/state/sighurt/session.toml
  check "the session lists the tabs" \
    "$(grep -c 'demo://' "$session" | awk '{print ($1 >= 2) ? 0 : 1}')" \
    "$(grep -o 'demo://[a-z]*' "$session" | tr '\n' ' ')"
  check "the address typed with å, ä, ö was opened" \
    "$(grep -Eq '"demo://slow\?(åäö|%C3%A5%C3%A4%C3%B6)"' "$session"; echo $?)" "$(grep -o 'demo://slow[^"]*' "$session")"
  check "hej åäö typed into the page reached it" "$(grep -q 'title = ".* · hej åäö"' "$session"; echo $?)" \
    "$(grep -o 'title = ".*hej.*"' "$session")"
  check "diagnostics written at exit" \
    "$(python3 -c "import json; d=json.load(open('$out/diag.json')); print(0 if d['exit'] else 1)")" "exit=true"
  ;;

close)
  start_sig demo://animation
  step "swaymsg kill: the compositor asks the window to close"
  swaymsg -q '[app_id=sighurt] kill'
  wait "$sig_pid"; code=$?
  check "a close request quits cleanly" "$code" "exit code $code"
  check "the session was saved" "$(grep -q 'demo://animation' "$out/state/sighurt/session.toml"; echo $?)" "session.toml"
  step "restart: the saved tab comes back"
  start_sig
  shot 01-restored
  check "the saved tab is restored" "$(diag "0 if sum(1 for x in t if x['role']=='view')==1 else 1")" "one demo view"
  kill -TERM "$sig_pid"
  wait "$sig_pid"; code=$?
  check "SIGTERM quits cleanly" "$code" "exit code $code"
  ;;

config)
  start_sig demo://pattern
  shown_page="0 if any(x['role']=='page' and x['shown'] for x in t) else 1"
  step "alt-9 is bound to nothing yet"
  key -M alt 9 -m alt
  shot 00-unbound-alt-9-types-nothing
  check "an unbound key does nothing" "$(diag "1 - ($shown_page)")" "the pattern page stays"
  step "bind alt-9 to tab.new: the file is read again"
  cp "$out/config/sighurt/config.toml" "$out/valid.toml"
  printf '"alt-9" = "tab.new"\n' >> "$out/valid.toml"
  sed -i 's/^version = 1$/version = 1\n[ui.colors]\ntab = "#7c3aed"/' "$out/valid.toml"
  cp "$out/valid.toml" "$out/config/sighurt/config.toml"
  sleep 0.5
  key -M alt 9 -m alt
  check "a new binding applies at once" "$(diag "$shown_page")" "alt-9 opened a new tab on the home page"
  shot 01-reloaded-with-new-binding-and-tab-color
  step "the first tab, x typed into its page: the page's status text shows"
  key -M alt 1 -m alt
  key x
  check "the page's status text shows" "$(diag "0 if d['status']=='page' else 1")" "$(diag "d['status']")"
  step "an invalid file: an unknown key"
  printf 'version = 1\nbogus = 1\n' > "$out/config/sighurt/config.toml"
  sleep 0.5
  shot 02-invalid-configuration-notice
  check "the notice comes before the page's status text" "$(diag "0 if d['status']=='notice' else 1")" "$(diag "d['status']")"
  key x
  check "and stays there while the page sets its status again" "$(diag "0 if d['status']=='notice' else 1")" "$(diag "d['status']")"
  key -M alt 2 -m alt
  key -M alt 1 -m alt
  check "an invalid file keeps the configuration in use" "$(diag "1 - ($shown_page)")" "alt-2, alt-1 still switch tabs"
  sleep 5
  check "5 s later the page's status text shows again" "$(diag "0 if d['status']=='page' else 1")" "$(diag "d['status']")"
  step "an unknown color name: the core accepts it, sig does not"
  sed 's/^tab = /tabz = /' "$out/valid.toml" > "$out/config/sighurt/config.toml"
  sleep 0.5
  shot 03-unknown-color-notice
  key -M alt 9 -m alt
  check "an unknown color keeps the configuration in use" "$(diag "$shown_page")" "alt-9 still opens a tab"
  check "every reload was read" "$(diag "0 if c['config_reloads']>=3 else 1")" "$(diag "c['config_reloads']") reloads"
  kill -TERM "$sig_pid"
  wait "$sig_pid"
  ;;

disconnect)
  start_sig demo://animation demo://pattern
  step "kill the compositor"
  kill -TERM "$SIG_SWAY_PID"
  wait "$sig_pid"; code=$?
  check "losing the compositor exits with 1" "$([[ $code == 1 ]]; echo $?)" "exit code $code"
  check "and says why" "$(grep -q 'Wayland connection is gone' "$out/sig.log"; echo $?)" \
    "$(grep -o 'the Wayland connection is gone.*' "$out/sig.log" | head -1)"
  check "the session was saved" "$(grep -c 'demo://' "$out/state/sighurt/session.toml" | awk '{print ($1 >= 2) ? 0 : 1}')" "session.toml"
  ;;
esac
finish
