#!/usr/bin/env bash
# End-to-end checks of the chromes in `sig`, in a private headless sway: never on the desktop,
# since they type and click. Builds `sig` with the demo engine and the line chrome
# (--no-default-features --features engine-demo,chrome-line) and sighurt-browser's
# virtual-pointer helper with tools/cargo, copies both into OUT (another build can replace
# target/debug/sig meanwhile), and runs the scenarios below inside tools/headless-sway, printing
# PASS or FAIL for each check. The checks read `sig`'s diagnostics (each chrome surface's target
# by name, whether it is mapped and its size; counters), its session file and log, and screenshot
# pixels. Screenshots, `sig`'s log and its diagnostics go to OUT; look at the screenshots too.
#
#   ui/sighurt-ui/tests/headless.sh OUT                  build, then run every scenario
#   ui/sighurt-ui/tests/headless.sh --no-build OUT       run with the binaries already in OUT
#   ui/sighurt-ui/tests/headless.sh OUT line overlay     only these scenarios
#
# Scenarios, each with a fresh configuration and session directory:
#   line      [ui] chrome = "line": its surfaces are "line" and, on sig://home, "page"; typing
#             an address opens it; a click on the line focuses it; idle repaints nothing
#   overlay   [chrome.standard] status = "overlay": listed only with status text, half the
#             page wide, and the page under it keeps the pointer; switching to the strip and
#             back applies live
#   left      tabs = "left": a click on the third row selects tab 3; chrome.toggle-tabs (a
#             configured key) hides the list and shows it again
#   font      [ui] font-size = 18 makes the chrome taller, and 14 again at once; a font that
#             isn't installed falls back and the log says so; a percent-encoded address reads
#             decoded (screenshot)
#
# Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: headless.sh [--no-build] OUT [SCENARIO ...]}")
  shift
  scenarios=("$@")
  [[ ${#scenarios[@]} == 0 ]] && scenarios=(line overlay left font)
  mkdir -p "$out/bin"
  if [[ $build == 1 ]]; then
    SIG_WHAT="build sig (demo engine, line chrome) for the chrome tests" tools/cargo build \
      -p sighurt-browser --no-default-features --features engine-demo,chrome-line \
      --bins --example virtual-pointer || exit 1
    tdir=${CARGO_TARGET_DIR:-target}
    cp "$tdir/debug/sig" "$tdir/debug/examples/virtual-pointer" "$out/bin/"
  fi
  failed=0
  for scenario in "${scenarios[@]}"; do
    tools/heavy tools/headless-sway -- "$0" --inside "$out/$scenario" "$scenario" "$out/bin"
    failed=$((failed + $?))
  done
  echo "headless.sh: $failed failed check(s); screenshots and logs in $out"
  exit $failed
fi

# ── Inside the private compositor ─────────────────────────────────────────────────────────
out=$2 scenario=$3 bin=$4
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "headless.sh: run me through tools/headless-sway" >&2
  exit 2
}
rm -rf "$out"
mkdir -p "$out/config/sighurt" "$out/state"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state
export SIG_DIAG=1 RUST_LOG=${RUST_LOG:-info}
config=$out/config/sighurt/config.toml
# configure TOP [KEYS]: the scenario's configuration TOP, the demo engine and keys, and KEYS.
configure() {
  cat > "$config" <<CONF
version = 1
$1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
[input]
keymap = "bindings"
[keys]
"ctrl-t" = "tab.new"
"ctrl-l" = "location.focus"
"alt-1" = "tab.select 1"
"alt-2" = "tab.select 2"
"alt-3" = "tab.select 3"
"ctrl-q" = "browser.quit"
${2:-}
CONF
}

failures=0
check() { # check NAME CONDITION-EXIT-STATUS DETAIL
  if [[ $2 == 0 ]]; then echo "PASS $scenario: $1 ($3)"; else echo "FAIL $scenario: $1 ($3)"; failures=$((failures + 1)); fi
}
step() { echo "[$(date +%T.%3N)] $*" >> "$out/steps.log"; }
shot() { sleep "${2:-0.5}"; grim "$out/$1.png"; step "screenshot $1"; }

# Input devices that stay plugged in (a new device per command could lose the first events).
wtype -s 3600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
"$bin/virtual-pointer" < "$XDG_RUNTIME_DIR/pointer" &
pointer_pid=$!
exec 3> "$XDG_RUNTIME_DIR/pointer"
point() { echo "move $1 $2 1280 800" >&3; }
click() { point "$1" "$2"; sleep 0.1; echo click >&3; sleep 0.2; }
key() { wtype "$@"; sleep 0.25; }

sig_pid=
start_sig() {
  "$bin/sig" --diagnostics "$out/diag.json" "$@" > "$out/sig.log" 2>&1 &
  sig_pid=$!
  sleep 2
}
# diag PYTHON-EXPRESSION: asks sig for its diagnostics (SIGUSR1) and evaluates the expression
# with `d` the JSON, `c` its counters, `t` its targets and `s(name)` the chrome's target of that
# name (None if it has none).
diag() {
  local before
  before=$(stat -c %Y.%y "$out/diag.json" 2>/dev/null)
  kill -USR1 "$sig_pid"
  for _ in $(seq 50); do
    [[ $(stat -c %Y.%y "$out/diag.json" 2>/dev/null) != "$before" ]] && break
    sleep 0.02
  done
  sleep 0.05
  python3 -c "import json; d=json.load(open('$out/diag.json')); c=d['counters']; t=d['targets']; s=lambda n: next((x for x in t if x['role']==n), None); v=($1); print(json.dumps(v) if isinstance(v, tuple) else v)"
}
shows_tab() {
  diag "0 if d['active_tab']==$1 and [x['tab'] for x in t if x['role']=='view' and x['shown']]==[$1] else 1"
}
tabs_now() { diag "d['active_tab'], [(x['tab'], x['shown']) for x in t if x['role']=='view']"; }
# surfaces: the chrome's targets: (name, mapped, width, height).
surfaces() { diag "[(x['role'], x['mapped'], x['width'], x['height']) for x in t if x['role'] not in ('view', 'retiring')]"; }
# column NAME X Y: screenshot NAME.ppm; 0 if the resize page's vertical crosshair line passes
# through output pixel (X, Y), give or take a pixel.
column() {
  grim -t ppm "$out/$1.ppm"
  python3 - "$out/$1.ppm" "$2" "$3" <<'PY'
import re, sys
data = open(sys.argv[1], 'rb').read()
m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', data)
w, pixels = int(m[1]), data[m.end():]
at = lambda x, y: pixels[3 * (y * w + x):3 * (y * w + x) + 3]
x, y = int(sys.argv[2]), int(sys.argv[3])
found = [c for c in range(x - 1, x + 2) if at(c, y)[2] - at(c, y)[0] > 128]
print(0 if found else 1, f"crosshair line at columns {found}")
PY
}
finish() {
  exec 3>&-
  kill "$keyboard" "$pointer_pid" 2>/dev/null
  wait "$keyboard" "$pointer_pid" 2>/dev/null
  grep -q 'panicked' "$out/sig.log" && check "no panic" 1 "see sig.log"
  exit $failures
}

case $scenario in
line)
  configure '[ui]
chrome = "line"'
  start_sig demo://pattern
  shot 01-line
  check "the line chrome's one surface, mapped" \
    "$(diag "0 if [x['role'] for x in t if x['role'] not in ('view', 'retiring')]==['line'] and s('line')['mapped'] and (s('line')['width'], s('line')['height'])==(1280, 28) else 1")" \
    "$(surfaces)"
  check "the page below it" "$(diag "0 if [(x['width'], x['height']) for x in t if x['role']=='view']==[(1280, 772)] else 1")" \
    "$(diag "[(x['width'], x['height']) for x in t if x['role']=='view']")"
  step "ctrl-t: a new tab on sig://home, which the line chrome draws"
  key -M ctrl t -m ctrl
  key -k Escape
  shot 02-home
  check "sig://home is its page surface" \
    "$(diag "0 if s('page') and s('page')['mapped'] and s('line')['mapped'] else 1")" "$(surfaces)"
  step "ctrl-l, type demo://resize, Enter"
  key -M ctrl l -m ctrl
  key 'demo://resize'
  shot 03-typing 0.2
  key -k Return
  shot 04-opened 1
  check "the typed address opens in the second tab" "$(shows_tab 2)" "$(tabs_now)"
  check "and the page surface is gone" "$(diag "0 if not s('page')['mapped'] else 1")" "$(surfaces)"
  step "a click on the line focuses it; type demo://animation into it"
  click 640 14
  key 'demo://animation'
  key -k Return
  sleep 1
  before=$(diag "c['page_frames_presented'], c['chrome_paints']")
  sleep 2
  after=$(diag "c['page_frames_presented'], c['chrome_paints']")
  shot 05-animation 0
  frames=$(python3 -c "a=$before; b=$after; print(b[0]-a[0])")
  paints=$(python3 -c "a=$before; b=$after; print(b[1]-a[1])")
  check "the line, clicked, opens what is typed: the animation runs" \
    "$([[ $frames -ge 60 ]] && [[ $(shows_tab 2) == 0 ]]; echo $?)" "$frames page frames in 2 s; $(tabs_now)"
  check "page animation repaints no chrome" "$([[ $paints == 0 ]]; echo $?)" "$paints chrome paints in 2 s"
  step "idle: alt-1, the static pattern"
  key -M alt 1 -m alt
  point 640 400
  sleep 1.5
  before=$(diag "c['chrome_paints'], c['wakeups']")
  sleep 3
  after=$(diag "c['chrome_paints'], c['wakeups']")
  paints=$(python3 -c "a=$before; b=$after; print(b[0]-a[0])")
  wakeups=$(python3 -c "a=$before; b=$after; print(b[1]-a[1])")
  check "idle: no chrome paints in 3 s" "$([[ $paints == 0 ]]; echo $?)" "$paints chrome paint(s)"
  check "idle: no wakeups in 3 s" "$([[ $wakeups -le 1 ]]; echo $?)" "$wakeups wakeup(s), the report's own included"
  shot 06-idle 0
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$code" "exit code $code"
  check "the session holds what was typed" \
    "$(grep -q 'demo://animation' "$out/state/sighurt/session.toml"; echo $?)" \
    "$(grep -o 'demo://[a-z]*' "$out/state/sighurt/session.toml" | tr '\n' ' ')"
  ;;

overlay)
  configure '[chrome.standard]
status = "overlay"'
  point 640 20
  start_sig demo://resize
  shot 01-no-status
  check "no status text, no status surface" "$(diag "0 if not (s('status') or {}).get('mapped') else 1")" "$(surfaces)"
  check "the page has the strip's room" "$(diag "0 if [(x['width'], x['height']) for x in t if x['role']=='view']==[(1280, 716)] else 1")" \
    "$(diag "[(x['width'], x['height']) for x in t if x['role']=='view']")"
  step "the pointer over the page: the resize page's status text, in the overlay"
  point 300 300
  shot 02-status
  check "the overlay is mapped, half the page wide" \
    "$(diag "0 if s('status')['mapped'] and (s('status')['width'], s('status')['height'])==(640, 20) else 1")" "$(surfaces)"
  check "the page didn't move" "$(diag "0 if [(x['width'], x['height']) for x in t if x['role']=='view']==[(1280, 716)] else 1")" \
    "$(diag "[(x['width'], x['height']) for x in t if x['role']=='view']")"
  step "hovering under the overlay: the pointer at (300, 790), then (500, 790), held 1 s"
  placements=$(diag "c['placements']")
  point 300 790
  sleep 0.3
  found=$(column 03-under-overlay-300 300 745)
  check "the page under the overlay gets the pointer" "${found%% *}" "x 300: ${found#* }"
  point 500 790
  sleep 1
  found=$(column 04-under-overlay-500 500 745)
  check "and follows it there" "${found%% *}" "x 500: ${found#* }"
  check "the overlay stays mapped" "$(diag "0 if s('status')['mapped'] else 1")" "$(surfaces)"
  check "and doesn't flicker" "$(diag "0 if c['placements'] - $placements <= 1 else 1")" \
    "$(diag "c['placements'] - $placements") placement(s) while the pointer moved under it"
  shot 05-under-overlay 0
  step "status = \"strip\": applies at once"
  sed -i 's/^status = "overlay"/status = "strip"/' "$config"
  sleep 0.7
  shot 06-strip
  check "the strip is mapped across the window" \
    "$(diag "0 if s('status')['mapped'] and (s('status')['width'], s('status')['height'])==(1280, 20) else 1")" "$(surfaces)"
  check "and the page gave it room" "$(diag "0 if [(x['width'], x['height']) for x in t if x['role']=='view']==[(1280, 696)] else 1")" \
    "$(diag "[(x['width'], x['height']) for x in t if x['role']=='view']")"
  sed -i 's/^status = "strip"/status = "overlay"/' "$config"
  sleep 0.7
  check "and back" "$(diag "0 if [(x['width'], x['height']) for x in t if x['role']=='view']==[(1280, 716)] else 1")" \
    "$(diag "[(x['width'], x['height']) for x in t if x['role']=='view']")"
  kill -TERM "$sig_pid"
  wait "$sig_pid"; code=$?
  check "SIGTERM exits cleanly" "$code" "exit code $code"
  ;;

left)
  configure '[chrome.standard]
tabs = "left"' '"ctrl-b" = "chrome.toggle-tabs"'
  start_sig demo://pattern demo://animation demo://resize
  shot 01-left
  check "the tab list is its own surface, left of the page" \
    "$(diag "0 if s('tabs')['mapped'] and (s('tabs')['width'], s('tabs')['height'])==(220, 736) and (s('chrome')['width'], s('chrome')['height'])==(1280, 44) else 1")" \
    "$(surfaces)"
  check "the page beside it" "$(diag "0 if [(x['width'], x['height']) for x in t if x['role']=='view']==[(1060, 736)]*3 else 1")" \
    "$(diag "[(x['width'], x['height']) for x in t if x['role']=='view']")"
  step "a click on the third row: 44 + 4 + 2 × 36 + 16"
  click 100 136
  shot 02-third
  check "a click on the third row shows tab 3" "$(shows_tab 3)" "$(tabs_now)"
  step "ctrl-b: chrome.toggle-tabs"
  before=$(diag "c['chrome_paints']")
  key -M ctrl b -m ctrl
  shot 03-toggled
  check "chrome.toggle-tabs hides the list" "$(diag "0 if not s('tabs')['mapped'] else 1")" "$(surfaces)"
  check "and the page takes its room" "$(diag "0 if [(x['width'], x['height']) for x in t if x['role']=='view' and x['shown']]==[(1280, 736)] else 1")" \
    "$(diag "[(x['width'], x['height']) for x in t if x['role']=='view' and x['shown']]")"
  key -M ctrl b -m ctrl
  shot 04-toggled-back
  check "and shows it again" "$(diag "0 if s('tabs')['mapped'] else 1")" "$(surfaces)"
  step "alt-1, by key: the first row is the active tab again"
  key -M alt 1 -m alt
  check "alt-1 shows tab 1" "$(shows_tab 1)" "$(tabs_now)"
  kill -TERM "$sig_pid"
  wait "$sig_pid"; code=$?
  check "SIGTERM exits cleanly" "$code" "exit code $code"
  ;;

font)
  configure '[ui]
font = "Nope Sans"
font-size = 18'
  start_sig 'demo://pattern?%C3%A5%C3%A4%C3%B6%E2%80%AE#K%C3%A4llor'
  shot 01-font-18-decoded-address
  check "18 px: a 51 + 57 px chrome and a 26 px status strip" \
    "$(diag "0 if s('chrome')['height']==108 and s('status')['height']==26 else 1")" "$(surfaces)"
  check "a font that isn't installed falls back, and the log says so" \
    "$(grep -q 'the font "Nope Sans" is not installed' "$out/sig.log"; echo $?)" \
    "$(grep -o 'the font .*' "$out/sig.log" | head -1)"
  step "font-size 14: applies at once"
  sed -i 's/^font-size = 18/font-size = 14/' "$config"
  sleep 0.7
  shot 02-font-14
  check "14 px: 84 and 20 px again" "$(diag "0 if s('chrome')['height']==84 and s('status')['height']==20 else 1")" "$(surfaces)"
  kill -TERM "$sig_pid"
  wait "$sig_pid"; code=$?
  check "SIGTERM exits cleanly" "$code" "exit code $code"
  ;;
esac
finish
