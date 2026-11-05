#!/usr/bin/env bash
# End-to-end tests of `sig` with the demo engine, in a private headless sway: never on the
# desktop, since they type and click. Builds `sig` (--no-default-features --features
# engine-demo) and the virtual-pointer helper with tools/cargo, then runs the checks
# below inside
# tools/headless-sway, and prints PASS or FAIL for each. The checks read `sig`'s diagnostics
# (which tab and view are shown, what the status strip shows, counters and peaks), its session
# file and log, and screenshot pixels (the resize page's crosshairs). Screenshots, `sig`'s log,
# its diagnostics and sway's log go to OUT; look at the screenshots too.
#
#   apps/sighurt-browser/tests/headless.sh OUT                  build, then run every scenario
#   apps/sighurt-browser/tests/headless.sh --no-build OUT       run with the binaries in target/
#   apps/sighurt-browser/tests/headless.sh OUT config close     only these scenarios
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
#   example     the default keymap, the example configuration: t opens a tab, Escape switches
#               to insert mode (the indicator reads INSERT MODE, keys pass to the page), Escape
#               again and x closes a tab, and normal mode gives the page no key (z, Backspace); a command line opened with ":" (where letters type),
#               a chrome command the chrome lacks, the mouse's side buttons over the chrome
#               and the page; :tab.new with an address, :tab.move left, u (tab.reopen) after
#               x; find in page (:page.find, n, N, the find field opened with /, Enter and
#               Escape); q quits, and the session has the moved tab first
#   settings    `zoom` for new tabs and page.zoom-reset, kept by open tabs across a reload;
#               page.open-in, and its errors (an engine the build lacks, a sig: page, the
#               latter in the status strip over the home page); an engine setting
#               the demo engine can't honor refused; [session] restore off (the saved tabs are
#               not restored and the file is left as it is) and on again while sig runs
#   history     completing addresses from a seeded history file (the demo engine's pages are
#               not http(s), so they are never recorded): typing a prefix shows the rest of a
#               visited address (screenshots compared), Tab makes the line that exact address
#               (pixel for pixel what typing it whole gives), Enter opens it; history.clear
#               takes the completion away at once and empties the file
#   fullscreen  f11 makes the window fullscreen: the page fills the output and the chrome hides;
#               ":" shows the line over the page, and a notice the status, until it gives way;
#               f11 again; demo://fullscreen: a click puts its element in fullscreen, with a
#               notice, and Escape (the example's) leaves it and the chrome returns;
#               demo://fullscreen-unasked is refused; with `bindings` and Escape unbound, the
#               core's own Escape leaves it, and so does another tab; f11 during a page's
#               fullscreen leaves both; after each way out its notice is gone; the
#               compositor ending fullscreen (swaymsg) resets the toggle
#   pages       sig://help, a Wasm guest built into sig: the example configuration's :help opens
#               it in a new tab, which draws (its pixels checked); a click on Keys in its sidebar
#               switches section (push_state: the session holds sig://help/keys); ? opens it again
#               and d closes that tab; demo://open-sig asks to open sig://help in its view and in a
#               new tab when clicked, and gets neither
#   terminal    sig://terminal with [pages.terminal] on, /bin/sh as the shell and a private
#               home: nothing starts until Enter in INSERT; typed text reaches the shell (a file
#               it writes); stty size is the grid that fits the page, and follows a resize of the
#               output; exit ends it, Enter starts it again; 20 s of `yes` keep sig's memory
#               bounded, another tab responsive and the view alive, and Ctrl+C gets the shell
#               back; nvim (if on PATH) writes a file with i, Escape and :wq; closing the tab,
#               navigating away, reloading, quitting and SIGTERM each leave no program and no
#               zombie; sig://terminal/config runs $VISUAL on the configuration; with the
#               setting off, Enter starts nothing
#   renderers   the chrome's renderers in a GPU compositor (--renderer gles2): the same steps
#               (tab churn that hides the page surface right after a frame, a resize storm, a
#               zero-height window and back, scale 1.5, idle, quit) with [ui] renderer = "cpu",
#               "wgpu", "wgpu" with SIG_RENDER_FAIL=surface (bound again, stays on wgpu) and
#               SIG_RENDER_FAIL=device (ends on cpu), then "vulkan" the same three ways; every
#               screenshot compared with cpu's
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
  [[ ${#scenarios[@]} == 0 ]] && scenarios=(main config close disconnect example settings history fullscreen pages terminal renderers)
  mkdir -p "$out"
  # Other builds replace target/debug/sig (with other features); this link keeps the binary these
  # runs use. It is made under the same hold of the machine's lock as the build, so no other
  # build can come between them.
  tdir=${CARGO_TARGET_DIR:-target}
  link="ln -f $tdir/debug/sig $tdir/debug/sig-headless-e2e"
  if [[ $build == 1 ]]; then
    SIG_WHAT="build sig (demo engine) for headless tests" tools/heavy bash -c \
      "cargo build -p sighurt-browser --no-default-features --features engine-demo,engine-wasm,render-wgpu,render-vulkan \
       --bins --example virtual-pointer && $link" || exit 1
  else
    $link || exit 1
  fi
  failed=0
  for scenario in "${scenarios[@]}"; do
    # wgpu presents through Vulkan swapchains, which only a GPU compositor composites.
    sway=()
    [[ $scenario == renderers ]] && sway=(--renderer gles2)
    tools/heavy tools/headless-sway "${sway[@]}" -- "$0" --inside "$out/$scenario" "$scenario"
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
tdir=${CARGO_TARGET_DIR:-target}
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
[input]
keymap = "bindings"
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
"$tdir/debug/examples/virtual-pointer" < "$XDG_RUNTIME_DIR/pointer" &
pointer_pid=$!
exec 3> "$XDG_RUNTIME_DIR/pointer"
layout_w=1280 layout_h=800
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
click() { point "$1" "$2"; sleep 0.1; echo click >&3; sleep 0.2; }
key() { wtype "$@"; sleep 0.25; }

sig_pid=
start_sig() {
  "$tdir/debug/sig-headless-e2e" --diagnostics "$out/diag.json" "$@" > "$out/sig.log" 2>&1 &
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
  # The chrome's surfaces are targets by name (the chrome API); through the storm and the scale
  # changes above, none held more than the host's 3 buffers.
  check "no chrome target held more than 3 buffers" \
    "$(diag "0 if d['peaks']['max_chrome_buffers']<=3 else 1")" \
    "$(diag "d['peaks']['max_chrome_buffers']") at most, in one of the chrome's targets"

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
  step "an unknown color name: the core checks the names the chrome has"
  sed 's/^tab = /tabz = /' "$out/valid.toml" > "$out/config/sighurt/config.toml"
  sleep 0.5
  shot 03-unknown-color-notice
  key -M alt 9 -m alt
  check "an unknown color keeps the configuration in use" "$(diag "$shown_page")" "alt-9 still opens a tab"
  check "every reload was read" "$(diag "0 if c['config_reloads']>=3 else 1")" "$(diag "c['config_reloads']") reloads"
  kill -TERM "$sig_pid"
  wait "$sig_pid"
  ;;

example)
  # The default keymap, the example configuration (sighurt-config-example): no [keys], no [input].
  cat > "$out/config/sighurt/config.toml" <<'CONF'
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
CONF
  start_sig demo://resize
  tab_count="len(d['tabs'])"
  indicator="d['keymap_indicator']"
  check "the configuration is valid: no notice" "$(diag "0 if d['status']!='notice' else 1")" "$(diag "d['status']")"
  step "t opens a tab"
  key t
  check "t is tab.new" "$(diag "0 if $tab_count==2 and d['active_tab']==2 else 1")" "$(diag "$tab_count, d['active_tab']") (views, active tab)"
  step "alt-1: the first tab (Always bindings work in the address line the new tab has); Escape, then t: insert mode passes t to the page"
  key -M alt 1 -m alt
  check "alt-1 shows the first tab" "$(shows_tab 1)" "$(tabs_now)"
  key -k Escape
  key t
  check "after Escape, t opens no tab" "$(diag "0 if $tab_count==2 else 1")" "$(diag "$tab_count") views"
  check "the indicator reads INSERT MODE" "$(diag "0 if $indicator=='INSERT MODE' else 1")" "$(diag "$indicator")"
  shot 01-insert-mode-indicator
  step "hej in insert mode reaches the page; Escape returns to normal mode"
  key hej
  key -k Escape
  check "the indicator is gone in normal mode" "$(diag "0 if $indicator is None else 1")" "$(diag "$indicator")"
  step "z and Backspace in normal mode: taken, and the page gets neither (the title check at the end)"
  key z
  key -k BackSpace
  step "alt-2: the second tab; : opens the command line with a colon in it; t, bound in normal mode, types there"
  key -M alt 2 -m alt
  check "alt-2 shows the second tab" "$(diag "0 if d['active_tab']==2 else 1")" "$(tabs_now)"
  key :
  key 'page.open demo://pattern'
  shot 02-command-line-typed
  key -k Return
  check "t typed into the line opened no tab" "$(diag "0 if $tab_count==2 else 1")" "$(diag "$tab_count") views"
  step ":chrome.nope: the chrome has no such command"
  key :
  key 'chrome.nope'
  key -k Return
  shot 03-chrome-lacks-the-command
  check "a chrome command the chrome lacks gets a notice" "$(diag "0 if d['status']=='notice' else 1")" "$(diag "d['status']")"
  step "the side buttons: back over the tab strip, forward over the page ([input.buttons]' defaults)"
  key :
  key 'page.open demo://resize'
  key -k Return
  sleep 0.5
  history="d['tabs'][1]['can_go_back'], d['tabs'][1]['can_go_forward']"
  check "a second page in the second tab" "$(diag "0 if ($history)==(True, False) else 1")" "$(diag "$history") (back, forward)"
  point 1000 20
  echo click side >&3
  sleep 0.5
  check "the back button over the chrome goes back" "$(diag "0 if ($history)==(True, True) else 1")" \
    "$(diag "$history") (back, forward)"
  point 640 400
  echo click extra >&3
  sleep 0.5
  check "the forward button over the page goes forward" "$(diag "0 if ($history)==(True, False) else 1")" \
    "$(diag "$history") (back, forward)"
  step "Escape, then x: insert mode passes x on; Escape again, then x closes the tab"
  key -k Escape
  key x
  check "in insert mode x closes nothing" "$(diag "0 if $tab_count==2 else 1")" "$(diag "$tab_count") views"
  key -k Escape
  key x
  check "x closes the second tab" "$(diag "0 if $tab_count==1 and d['active_tab']==1 else 1")" "$(diag "$tab_count, d['active_tab']") (views, active tab)"
  step ":tab.new demo://pattern opens a tab on that page; :tab.move left puts it first"
  key :
  key 'tab.new demo://pattern'
  key -k Return
  sleep 0.5
  check ":tab.new with an address opens it in a new tab" \
    "$(diag "0 if $tab_count==2 and d['active_tab']==2 and d['tabs'][1]['engine']=='demo' else 1")" \
    "$(diag "$tab_count, d['active_tab'], d['tabs'][1]['engine']") (tabs, active tab, its engine)"
  key :
  key 'tab.move left'
  key -k Return
  check ":tab.move left moves the selected tab, which stays selected" \
    "$(diag "0 if $tab_count==2 and d['active_tab']==1 else 1")" "$(diag "$tab_count, d['active_tab']") (tabs, active tab)"
  step "x closes the pattern tab, u reopens it at the end, :tab.move left puts it first again"
  key x
  check "x closes the pattern tab" "$(diag "0 if $tab_count==1 else 1")" "$(diag "$tab_count") tabs"
  key u
  sleep 0.5
  check "u (tab.reopen) brings it back, selected, at the end" \
    "$(diag "0 if $tab_count==2 and d['active_tab']==2 and d['tabs'][1]['engine']=='demo' else 1")" \
    "$(diag "$tab_count, d['active_tab'], d['tabs'][1]['engine']") (tabs, active tab, its engine)"
  key :
  key 'tab.move left'
  key -k Return
  step "find in page on demo://pattern: :page.find page, n and N; / opens the find field, Enter keeps the search, Escape ends it"
  find="d['tabs'][d['active_tab']-1]['find']"
  key :
  key 'page.find page'
  key -k Return
  sleep 0.3
  check ":page.find finds the page's three matches, the first selected" \
    "$(diag "0 if ($find or {}).get('total')==3 and $find['current']==1 else 1")" "$(diag "$find")"
  key n
  check "n (page.find-next) selects the second" "$(diag "0 if ($find or {}).get('current')==2 else 1")" "$(diag "$find")"
  # Shift+N, as a keyboard sends it: wtype types "N" as a key of its own, without Shift.
  key -M shift n -m shift
  check "N (page.find-previous) selects the first again" "$(diag "0 if ($find or {}).get('current')==1 else 1")" "$(diag "$find")"
  key -M shift n -m shift
  check "N again wraps around to the third" "$(diag "0 if ($find or {}).get('current')==3 else 1")" "$(diag "$find")"
  key /
  key Press
  shot 04-find-field
  check "/ opens the find field, which searches as it is typed (smart case: one match)" \
    "$(diag "0 if ($find or {}).get('total')==1 else 1")" "$(diag "$find")"
  key -k Return
  key n
  check "Enter keeps the search and gives the page the keyboard: n moves in it, wrapping" \
    "$(diag "0 if ($find or {}).get('total')==1 and $find['current']==1 else 1")" "$(diag "$find")"
  shot 05-find-kept
  key /
  key -k Escape
  check "Escape in the find field ends the search" "$(diag "0 if $find is None else 1")" "$(diag "$find")"
  step "q quits"
  key q
  wait "$sig_pid"; code=$?
  check "q exits cleanly" "$code" "exit code $code"
  session=$out/state/sighurt/session.toml
  check "only what insert mode passed (t, hej) reached the page; normal mode's z and Backspace did not" \
    "$(grep -q 'title = "Demo: resize · thej"$' "$session"; echo $?)" "$(grep -o 'title = .*' "$session" | tr '\n' ' ')"
  entries=$(grep -o 'entries = .*' "$session" | tr '\n' ' ')
  check "the session saves the reopened, moved tab first" \
    "$([[ $entries == 'entries = ["demo://pattern"] entries = ["demo://resize"] ' ]]; echo $?)" "$entries"
  ;;

settings)
  session=$out/state/sighurt/session.toml
  config=$out/config/sighurt/config.toml
  # until_session SECONDS EXPR: asks probe.py about the session file every 0.25 s until its
  # verdict holds, for at most SECONDS; prints its last answer, "0 detail" or "1 detail".
  until_session() {
    local end=$((SECONDS + $1)) reply
    while :; do
      reply=$(python3 apps/sighurt-browser/tests/probe.py session "$session" "$2" 2>&1)
      [[ $reply == 0* || $SECONDS -ge $end ]] && break
      sleep 0.25
    done
    echo "$reply"
  }
  # sess NAME SECONDS EXPR: a check on the session file.
  sess() { local r; r=$(until_session "$2" "$3"); check "$1" "${r%% *}" "${r#* }"; }
  # run COMMAND: types ":COMMAND" into the address line and runs it.
  run() { key -M ctrl l -m ctrl; key ":$1"; key -k Return; }
  zooms="[t['zoom'] for t in s['tabs']]"
  sed -i 's/^version = 1$/version = 1\nzoom = 1.25/' "$config"
  start_sig demo://pattern
  step "zoom = 1.25: the first tab and a new one start at it; zoom in, then reset"
  key -M ctrl t -m ctrl
  sess "new tabs start at the configured zoom" 5 "$zooms==[1.25, 1.25], $zooms"
  run page.zoom-in
  sess "page.zoom-in takes the next step" 5 "$zooms==[1.25, 1.5], $zooms"
  run page.zoom-reset
  sess "page.zoom-reset returns to the configured zoom" 5 "$zooms==[1.25, 1.25], $zooms"
  step "zoom = 0.9, loaded while sig runs: open tabs keep theirs, a new tab starts at it"
  sed -i 's/^zoom = 1.25$/zoom = 0.9/' "$config"
  sleep 0.5
  key -M ctrl t -m ctrl
  sess "a reload leaves open tabs' zoom; new tabs take the new one" 5 "$zooms==[1.25, 1.25, 0.9], $zooms"

  step ":page.open-in demo demo://resize in the first tab; then servo, not in this build"
  key -M alt 1 -m alt
  run 'page.open-in demo demo://resize'
  sess "page.open-in opens the address in the engine named" 5 "url(1)=='demo://resize', url(1)"
  check "and the tab shows it" "$(shows_tab 1)" "$(tabs_now)"
  run 'page.open-in servo https://example.com/'
  shot 01-open-in-servo-not-in-this-build
  check "page.open-in an engine the build lacks says why, in the tab" \
    "$(( $(diag "0 if d['status']=='error' else 1") + $(shows_tab 1) ))" "$(diag "d['status']"); $(tabs_now)"
  step ":page.open-in demo on the third tab's sig://home: sig: pages open in no engine"
  key -M alt 3 -m alt
  run 'page.open-in demo'
  shot 02-open-in-on-home
  check "a tab's error on its sig: page is in the status strip" \
    "$(diag "0 if d['active_tab']==3 and d['status']=='error' and d['tabs'][2]['error'] else 1")" \
    "$(diag "(d['active_tab'], d['status'], d['tabs'][2]['error'])")"

  step "[engines.demo] storage: the demo engine can't honor it, so the file is refused"
  cp "$config" "$out/valid.toml"
  printf '[engines.demo]\nstorage = "ephemeral"\n' >> "$config"
  sleep 0.5
  check "an engine setting the engine can't honor is a problem, and says so" \
    "$(grep -q "\[engines.demo\] storage: the demo engine can't honor it" "$out/sig.log"; echo $?)" \
    "$(grep -o '\[engines.demo\] storage: .*' "$out/sig.log" | head -1)"
  check "and the configuration in use stays: a notice" "$(diag "0 if d['status']=='notice' else 1")" "$(diag "d['status']")"
  cp "$out/valid.toml" "$config"
  sleep 0.5

  step "ctrl-q: the session holds the three tabs"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$code" "exit code $code"
  sess "the session holds the three tabs" 1 "len(s['tabs'])==3, $zooms"
  cp "$session" "$out/session-saved.toml"

  step "[session] restore = false: the home page, and the file left as it is"
  printf '[session]\nrestore = false\n' >> "$config"
  start_sig
  check "the saved tabs are not restored: one tab, the home page" \
    "$(diag "0 if len(d['tabs'])==1 and not [x for x in t if x['role']=='view'] else 1")" \
    "$(diag "len(d['tabs'])") tab(s), $(diag "len([x for x in t if x['role']=='view'])") view(s)"
  key -M ctrl t -m ctrl
  sleep 1.5
  check "a change to the tabs writes nothing" "$(cmp -s "$session" "$out/session-saved.toml"; echo $?)" "cmp with the saved session"
  kill -TERM "$sig_pid"
  wait "$sig_pid"; code=$?
  check "SIGTERM exits cleanly" "$code" "exit code $code"
  check "and quitting writes nothing" "$(cmp -s "$session" "$out/session-saved.toml"; echo $?)" "cmp with the saved session"

  step "restore = true again, while sig runs: the tabs as they are now are saved"
  start_sig
  key -M ctrl t -m ctrl
  sed -i 's/^restore = false$/restore = true/' "$config"
  sess "turning restore on saves the tabs at once" 5 "len(s['tabs'])==2, len(s['tabs'])"
  kill -TERM "$sig_pid"
  wait "$sig_pid"
  ;;

history)
  url=https://www.example.com/page
  history=$out/state/sighurt/history.toml
  mkdir -p "$out/state/sighurt"
  now=$(date +%s)
  printf 'version = 1\n[[visits]]\nurl = "%s"\ntitle = "Example"\nlast = %s\ncount = 3\n[[visits]]\nurl = "https://example.org/older"\ntitle = "Older"\nlast = %s\ncount = 1\n' \
    "$url" "$now" "$((now - 60))" > "$history"
  printf '"ctrl-h" = "history.clear"\n' >> "$out/config/sighurt/config.toml"
  # band NAME: a screenshot of the toolbar's band (rows 36 to 90, where the address line is).
  band() { sleep 0.4; grim -t ppm "$out/$1.ppm"; step "screenshot $1"; }
  # same A B: 0 if the address band of screenshots A and B is the same, then how many differ.
  same() {
    python3 - "$out/$1.ppm" "$out/$2.ppm" <<'PY'
import re, sys
def load(path):
    data = open(path, 'rb').read()
    m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', data)
    return int(m[1]), data[m.end():]
(w, a), (_, b) = load(sys.argv[1]), load(sys.argv[2])
rows = range(36, 90)
differing = sum(1 for y in rows for x in range(w) if a[3 * (y * w + x):3 * (y * w + x) + 3] != b[3 * (y * w + x):3 * (y * w + x) + 3])
print(0 if differing == 0 else 1, f'{differing} pixels differ in the address band')
PY
  }
  start_sig demo://pattern
  step "type exa: the rest of $url shows after the caret"
  key -M ctrl l -m ctrl
  key exa
  band 01-completion
  key -k Escape
  key -M ctrl l -m ctrl
  key exa
  key -k Left
  key -k Right
  band 02-completion-again
  r=$(same 01-completion 02-completion-again); check "the completion comes back with the caret at the end" "${r%% *}" "${r#* }"
  step "Tab: the line is $url, as typing it whole shows it"
  key -k Tab
  band 03-taken
  key -k Escape
  key -M ctrl l -m ctrl
  key "$url"
  band 04-typed
  r=$(same 03-taken 04-typed); check "Tab makes the line the exact address" "${r%% *}" "${r#* }"
  step "Enter on the taken address opens it (the demo engine can't, so the tab says so)"
  key -k Escape
  key -M ctrl l -m ctrl
  key exa
  key -k Tab
  key -k Return
  sleep 0.5
  check "Enter opens the address taken: the tab's error is the demo engine's" \
    "$(diag "0 if d['status']=='error' else 1")" "$(diag "d['status']")"
  check "nothing was recorded (only http and https pages are)" "$(diag "0 if c['visits_recorded']==0 else 1")" \
    "visits_recorded $(diag "c['visits_recorded']")"
  step "ctrl-h (history.clear) while a completion shows: it goes at once, and so does the file's"
  key -M ctrl l -m ctrl
  key exa
  band 06-before-clear
  key -M ctrl h -m ctrl
  band 07-cleared
  r=$(same 06-before-clear 07-cleared); check "history.clear takes the completion away" "$([[ ${r%% *} == 1 ]]; echo $?)" "${r#* }"
  key -k Escape
  check "and empties the file at once" "$(grep -q 'example' "$history"; [[ $? == 1 ]]; echo $?)" "$(wc -c < "$history") bytes"
  check "which stays private" "$([[ $(stat -c %a "$history") == 600 ]]; echo $?)" "mode $(stat -c %a "$history")"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$code" "exit code $code"
  check "a history save was counted" "$(python3 -c "import json; print(0 if json.load(open('$out/diag.json'))['counters']['history_saves']>=1 else 1)")" "history_saves"
  ;;

fullscreen)
  # The example configuration (f11, ":", Escape), then `bindings` with Escape unbound.
  cat > "$out/config/sighurt/config.toml" <<'CONF'
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
CONF
  fs="d['fullscreen']['wanted'], d['fullscreen']['window'], d['fullscreen']['page']"
  page_size="[(x['width'], x['height']) for x in t if x['role']=='view' and x['shown']]"
  chrome_up="sorted(x['role'] for x in t if x['role'] in ('chrome', 'status', 'page') and x['mapped'])"
  # until_diag SECONDS EXPR: asks for the diagnostics every 0.2 s until EXPR holds, for at
  # most SECONDS; prints 0 or 1.
  until_diag() {
    local end=$((SECONDS + $1))
    while :; do
      [[ $(diag "1 if ($2) else 0") == 1 ]] && { echo 0; return; }
      [[ $SECONDS -ge $end ]] && { echo 1; return; }
      sleep 0.2
    done
  }
  start_sig demo://pattern
  check "windowed at the start" "$(diag "0 if ($fs)==(False, False, False) else 1")" "$(diag "$fs") (wanted, window, page)"
  windowed=$(diag "$page_size")
  step "f11: the window's fullscreen"
  key -k F11
  check "f11 makes the window fullscreen" "$(until_diag 5 "($fs)==(True, True, False)")" "$(diag "$fs")"
  sleep 0.5
  shot 01-f11-fullscreen
  check "the page fills the output" "$(diag "0 if $page_size==[(1280, 800)] else 1")" "$(diag "$page_size") (windowed: $windowed)"
  check "the chrome hides" "$(diag "0 if $chrome_up==[] else 1")" "$(diag "$chrome_up")"
  step ": opens the command line, shown over the page"
  key :
  sleep 0.5
  shot 02-command-line-over-the-page
  check ": shows the chrome" "$(diag "0 if 'chrome' in $chrome_up else 1")" "$(diag "$chrome_up")"
  check "the page keeps the whole output" "$(diag "0 if $page_size==[(1280, 800)] else 1")" "$(diag "$page_size")"
  step "a command that answers with a notice: the status shows, then hides"
  key 'page.exit-fullscreen'
  key -k Return
  sleep 0.5
  shot 03-notice-in-fullscreen
  check "a notice shows the status surface" \
    "$(diag "0 if d['status']=='notice' and 'status' in $chrome_up else 1")" "$(diag "d['status'], $chrome_up")"
  check "the notice gives way and the chrome hides again" "$(until_diag 8 "$chrome_up==[]")" "$(diag "d['status'], $chrome_up")"
  step "f11 again: windowed"
  key -k F11
  check "f11 leaves fullscreen" "$(until_diag 5 "($fs)==(False, False, False)")" "$(diag "$fs")"
  check "the page is where it was" "$(diag "0 if $page_size==$windowed else 1")" "$(diag "$page_size") (before: $windowed)"

  step "demo://fullscreen: a click puts its element in fullscreen"
  key :
  key 'page.open demo://fullscreen'
  key -k Return
  sleep 1
  click 640 400
  check "the click's fullscreen is accepted" "$(until_diag 5 "($fs)==(False, True, True)")" "$(diag "$fs")"
  sleep 0.3
  shot 04-page-fullscreen-notice
  check "the page fills the output" "$(diag "0 if $page_size==[(1280, 800)] else 1")" "$(diag "$page_size")"
  check "the notice says so, over the page" \
    "$(diag "0 if d['status']=='notice' and 'status' in $chrome_up else 1")" "$(diag "d['status'], $chrome_up")"
  step "Escape (the example's page.exit-fullscreen) leaves it, and the mode stays"
  key -k Escape
  check "Escape leaves the page's fullscreen" "$(until_diag 5 "($fs)==(False, False, False)")" "$(diag "$fs")"
  sleep 1
  check "the fullscreen notice is gone after Escape" "$(diag "0 if d['status']!='notice' else 1")" "$(diag "d['status']")"
  check "the chrome is back" "$(until_diag 5 "'chrome' in $chrome_up")" "$(diag "$chrome_up")"
  check "the keymap stays in normal mode" "$(diag "0 if d['keymap_indicator'] is None else 1")" "$(diag "d['keymap_indicator']")"
  check "the page is where it was" "$(diag "0 if $page_size==$windowed else 1")" "$(diag "$page_size")"
  shot 05-left-page-fullscreen

  step "demo://fullscreen-unasked: fullscreen without a click is refused"
  key :
  key 'page.open demo://fullscreen-unasked'
  key -k Return
  sleep 1.5
  check "a page's fullscreen without the user is refused" "$(diag "0 if ($fs)==(False, False, False) else 1")" "$(diag "$fs")"
  shot 06-unasked-refused
  key q
  wait "$sig_pid"; code=$?
  check "q exits cleanly" "$code" "exit code $code"

  step "bindings with no Escape: the core's own Escape leaves a page's fullscreen"
  cat > "$out/config/sighurt/config.toml" <<'CONF'
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
"f11" = "browser.fullscreen"
"ctrl-q" = "browser.quit"
"alt-1" = "tab.select 1"
"alt-2" = "tab.select 2"
CONF
  start_sig demo://fullscreen demo://pattern
  key -M alt 1 -m alt
  click 640 400
  check "the click's fullscreen is accepted" "$(until_diag 5 "($fs)==(False, True, True)")" "$(diag "$fs")"
  key -k Escape
  check "an unbound Escape leaves it" "$(until_diag 5 "($fs)==(False, False, False)")" "$(diag "$fs")"
  sleep 1
  check "the fullscreen notice is gone after an unbound Escape" "$(diag "0 if d['status']!='notice' else 1")" "$(diag "d['status']")"
  step "another tab ends a page's fullscreen"
  click 640 400
  check "fullscreen on a new click" "$(until_diag 5 "($fs)==(False, True, True)")" "$(diag "$fs")"
  key -M alt 2 -m alt
  check "another tab leaves it" "$(until_diag 5 "($fs)==(False, False, False)")" "$(diag "$fs")"
  sleep 1
  check "the fullscreen notice is gone after another tab" "$(diag "0 if d['status']!='notice' else 1")" "$(diag "d['status']")"
  key -M alt 1 -m alt
  step "f11 while a page is fullscreen leaves both"
  click 640 400
  check "fullscreen again on a new click" "$(until_diag 5 "($fs)==(False, True, True)")" "$(diag "$fs")"
  key -k F11
  check "f11 leaves the page's and the window's fullscreen" "$(until_diag 5 "($fs)==(False, False, False)")" "$(diag "$fs")"
  sleep 1
  check "the fullscreen notice is gone after f11" "$(diag "0 if d['status']!='notice' else 1")" "$(diag "d['status']")"
  step "the compositor ending the window's fullscreen"
  key -k F11
  check "f11 makes the window fullscreen" "$(until_diag 5 "($fs)==(True, True, False)")" "$(diag "$fs")"
  swaymsg -q fullscreen disable
  check "the compositor's exit resets the toggle" "$(until_diag 5 "($fs)==(False, False, False)")" "$(diag "$fs")"
  key -M ctrl q -m ctrl
  wait "$sig_pid"; code=$?
  check "ctrl-q exits cleanly" "$code" "exit code $code"
  ;;

renderers)
  if [[ ${SIG_SWAY_RENDERER:-} != gles2 ]]; then
    check "a GPU compositor for wgpu" 1 "sway runs ${SIG_SWAY_RENDERER:-?}; this scenario needs gles2"
    finish
  fi
  cp "$out/config/sighurt/config.toml" "$out/base.toml"
  point 640 400
  # run_renderer NAME RENDERER [SIG_RENDER_FAIL]: the steps, screenshots NAME-*.png, log
  # NAME.log, diagnostics NAME.json; checks what every renderer must do, and sets `report` to
  # the renderer's (name, frames, deferred, fallbacks, chrome_deferred, max_render_us, describe).
  run_renderer() {
    local name=$1 renderer=$2 fail=${3:-}
    step "── $name: [ui] renderer = \"$renderer\"${fail:+, SIG_RENDER_FAIL=$fail}"
    { cat "$out/base.toml"; printf '[ui]\nrenderer = "%s"\n' "$renderer"; } > "$out/config/sighurt/config.toml"
    rm -rf "$out/state" && mkdir -p "$out/state"
    swaymsg -q output HEADLESS-1 resolution 1280x800 scale 1
    if [[ -n $fail ]]; then export SIG_RENDER_FAIL=$fail; else unset SIG_RENDER_FAIL; fi
    start_sig demo://pattern sig://home
    unset SIG_RENDER_FAIL
    shot "$name-01-start" 1
    key -M alt 2 -m alt
    shot "$name-02-home"
    step "tab churn: the page surface hidden right after its frames, 10 times"
    for _ in $(seq 10); do
      wtype -M alt 1 -m alt; sleep 0.03
      wtype -M alt 2 -m alt; sleep 0.03
    done
    key -M alt 1 -m alt
    shot "$name-03-after-churn" 1
    # Shown again at the same size: a native surface kept its last frame across the hide.
    key -M alt 2 -m alt
    shot "$name-03b-home-again" 1
    key -M alt 1 -m alt
    step "resize storm, then a window with no room for the page, then back"
    for i in $(seq 10); do
      swaymsg -q output HEADLESS-1 resolution $((900 + i * 30))x$((600 + i * 15)); sleep 0.05
    done
    swaymsg -q output HEADLESS-1 resolution 1280x80
    sleep 0.5
    swaymsg -q output HEADLESS-1 resolution 1280x800
    shot "$name-04-restored" 1.5
    key -M alt 2 -m alt
    swaymsg -q output HEADLESS-1 scale 1.5
    shot "$name-05-home-150" 1.5
    swaymsg -q output HEADLESS-1 scale 1
    key -M alt 1 -m alt
    sleep 1
    before=$(diag "c['wakeups'], c['chrome_paints']")
    sleep 3
    after=$(diag "c['wakeups'], c['chrome_paints']")
    check "$name: idle, no wakeups or chrome paints in 3 s" \
      "$(python3 -c "a=$before; b=$after; print(0 if b[0]-a[0]<=1 and b[1]==a[1] else 1)")" "$before → $after (wakeups, chrome paints)"
    report=$(diag "d['renderer']['name'], d['renderer']['frames'], d['renderer']['deferred'], d['renderer']['fallbacks'], c['chrome_deferred'], d['renderer']['max_render_us'], d['renderer']['describe']")
    echo "$name: $report" >> "$out/renderers.txt"
    key -M ctrl q -m ctrl
    wait "$sig_pid"; code=$?
    check "$name: quits cleanly" "$code" "exit code $code"
    grep -q panicked "$out/sig.log" && check "$name: no panic" 1 "see $name.log"
    mv "$out/sig.log" "$out/$name.log"
    mv "$out/diag.json" "$out/$name.json"
  }
  run_renderer cpu cpu
  check "cpu: the CPU renderer draws" "$(python3 -c "r=$report; print(0 if r[0]=='cpu' and r[1]>0 else 1)")" "$report"
  run_renderer wgpu wgpu
  check "wgpu: wgpu draws, no fallback" "$(python3 -c "r=$report; print(0 if r[0]=='wgpu' and r[1]>0 and r[3]==0 else 1)")" "$report"
  check "wgpu: the adapter is named after the first bind" \
    "$(grep -q 'chrome-renderer renderer=wgpu describe="wgpu 30 on Vulkan: [^t]' "$out/wgpu.log"; echo $?)" \
    "$(grep -o 'chrome-renderer renderer=wgpu describe=[^ ]* [^ ]* [^ ]* [^ ]* [^ ]*' "$out/wgpu.log" | tail -1)"
  check "wgpu: no chrome render failed" "$(grep -q "rendering the chrome's" "$out/wgpu.log"; [[ $? == 1 ]]; echo $?)" \
    "$(grep -c "rendering the chrome's" "$out/wgpu.log") failures in wgpu.log"
  run_renderer surface wgpu surface
  check "surface: a lost surface is bound again and wgpu stays" \
    "$(python3 -c "r=$report; print(0 if r[0]=='wgpu' and r[3]==0 else 1)")" "$report; $(grep -o "lost its surface.*" "$out/surface.log" | head -1)"
  run_renderer fallback wgpu device
  check "fallback: a failed device ends on the CPU renderer" \
    "$(python3 -c "r=$report; print(0 if r[0]=='cpu' and r[3]==1 else 1)")" "$report; $(grep -o 'chrome-renderer-fallback.*' "$out/fallback.log" | head -1)"
  # The Khronos validation layer on for the Vulkan runs (it logs to sig's output).
  export SIG_VULKAN_VALIDATION=1
  run_renderer vulkan vulkan
  check "vulkan: the validation layer is on and reports nothing" \
    "$(grep -q 'describe="Vulkan.*validation layer on' "$out/vulkan.log" && ! grep -qi 'validation error' "$out/vulkan.log"; echo $?)" \
    "$(grep -ci 'validation error' "$out/vulkan.log") validation errors in vulkan.log"
  check "vulkan: Vulkan draws, no fallback" "$(python3 -c "r=$report; print(0 if r[0]=='vulkan' and r[1]>0 and r[3]==0 else 1)")" "$report"
  check "vulkan: the device is named after the first bind" \
    "$(grep -q 'chrome-renderer renderer=vulkan describe="Vulkan [0-9]' "$out/vulkan.log"; echo $?)" \
    "$(grep -o 'chrome-renderer renderer=vulkan describe="[^"]*"' "$out/vulkan.log" | tail -1)"
  check "vulkan: no chrome render failed" "$(grep -q "rendering the chrome's" "$out/vulkan.log"; [[ $? == 1 ]]; echo $?)" \
    "$(grep -c "rendering the chrome's" "$out/vulkan.log") failures in vulkan.log"
  run_renderer vulkan-surface vulkan surface
  check "vulkan-surface: a lost surface is bound again and vulkan stays" \
    "$(python3 -c "r=$report; print(0 if r[0]=='vulkan' and r[3]==0 else 1)")" "$report; $(grep -o "lost its surface.*" "$out/vulkan-surface.log" | head -1)"
  run_renderer vulkan-fallback vulkan device
  check "vulkan-fallback: a failed device ends on the CPU renderer" \
    "$(python3 -c "r=$report; print(0 if r[0]=='cpu' and r[3]==1 else 1)")" "$report; $(grep -o 'chrome-renderer-fallback.*' "$out/vulkan-fallback.log" | head -1)"
  # Every screenshot of every run against cpu's: the same pixels but near rounded corners.
  for run in wgpu surface fallback vulkan vulkan-surface vulkan-fallback; do
    for shot in 01-start 02-home 03-after-churn 03b-home-again 04-restored 05-home-150; do
      found=$(python3 - "$out/cpu-$shot.png" "$out/$run-$shot.png" <<'PY'
import struct, sys, zlib
def png(path):
    data = open(path, 'rb').read()
    w, h = struct.unpack('>II', data[16:24])
    chunks, i = b'', 8
    while i < len(data):
        n, kind = struct.unpack('>I4s', data[i:i + 8])
        if kind == b'IDAT':
            chunks += data[i + 8:i + 8 + n]
        i += 12 + n
    raw, bpp = zlib.decompress(chunks), data[25] == 6 and 4 or 3
    rows, prev, stride = [], bytearray(w * bpp), w * bpp
    for y in range(h):
        f, line = raw[y * (stride + 1)], bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b, c = prev[x], prev[x - bpp] if x >= bpp else 0
            p = a + b - c
            pr = a if abs(p - a) <= abs(p - b) and abs(p - a) <= abs(p - c) else (b if abs(p - b) <= abs(p - c) else c)
            line[x] = (line[x] + [0, a, b, (a + b) // 2, pr][f]) & 255
        rows.append(line)
        prev = line
    return w, h, bpp, rows
w1, h1, b1, a = png(sys.argv[1])
w2, h2, b2, b = png(sys.argv[2])
if (w1, h1) != (w2, h2):
    print(1, f'size {w1}x{h1} vs {w2}x{h2}'); sys.exit()
differing = worst = 0
for y in range(h1):
    for x in range(w1):
        d = max(abs(a[y][x * b1 + k] - b[y][x * b2 + k]) for k in range(3))
        worst = max(worst, d)
        differing += d > 2
allowed = w1 * h1 // 1000
print(0 if differing <= allowed and worst <= 32 else 1, f'{differing} pixels differ by more than 2 (at most {allowed}), worst {worst}')
PY
)
      check "$run-$shot looks like cpu-$shot" "${found%% *}" "${found#* }"
    done
  done
  : > "$out/sig.log"  # each run's log is NAME.log; finish looks here
  ;;

pages)
  # The example configuration, the default: its keys open help (`?`, `:help`) and close tabs (`d`).
  cat > "$out/config/sighurt/config.toml" <<'CONF'
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
CONF
  tabs_count() { diag "len(d['tabs'])"; }
  shown_engine() { diag "[x['engine'] for x in t if x['role']=='view' and x['shown']]"; }
  start_sig demo://pattern
  key ':'
  wtype help
  key -k Return
  sleep 3
  check ":help opens sig://help in a new tab" \
    "$(diag "0 if d['active_tab']==2 and [x['engine'] for x in t if x['role']=='view' and x['shown']]==['wasm'] else 1")" \
    "$(tabs_now) $(shown_engine)"
  shot 01-help 1
  page_h=$(diag "[x['height'] for x in t if x['role']=='view' and x['shown']][0]")
  # The page ends where the status strip, 20 pixels high, begins.
  top=$((layout_h - 20 - page_h))
  # The page draws: text in many colours over its background, not one flat colour.
  drawn=$(python3 - "$out/01-help.png" "$top" <<'PY'
import struct, sys, zlib
raw = open(sys.argv[1], 'rb').read()
pos, chunks = 8, []
while pos < len(raw):
    n, kind = struct.unpack('>I4s', raw[pos:pos + 8])
    chunks.append((kind, raw[pos + 8:pos + 8 + n])); pos += 12 + n
w, h, depth, ctype = struct.unpack('>IIBB', [c for k, c in chunks if k == b'IHDR'][0][:10])
bpp = {2: 3, 6: 4}[ctype]
data = zlib.decompress(b''.join(c for k, c in chunks if k == b'IDAT'))
stride, rows, prev = w * bpp, [], bytearray(w * bpp)
for y in range(h):
    f, line = data[y * (stride + 1)], bytearray(data[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
    for i in range(stride):
        a = line[i - bpp] if i >= bpp else 0
        b, c = prev[i], prev[i - bpp] if i >= bpp else 0
        p = a + b - c
        pred = [0, a, b, (a + b) // 2, a if abs(p - a) <= abs(p - b) and abs(p - a) <= abs(p - c) else (b if abs(p - b) <= abs(p - c) else c)][f]
        line[i] = (line[i] + pred) & 0xff
    rows.append(line); prev = line
top = int(sys.argv[2])
colours, counts = set(), {}
for y in range(top + 10, h - 10, 3):
    for x in range(0, w, 3):
        px = bytes(rows[y][x * bpp:x * bpp + 3]); colours.add(px); counts[px] = counts.get(px, 0) + 1
common = max(counts.values()) / sum(counts.values())
print(0 if len(colours) > 20 and common < 0.97 else 1, f'{len(colours)} colours, the commonest {common:.0%} of the page')
PY
)
  check "sig://help draws" "${drawn%% *}" "${drawn#* }"
  # The sidebar's second entry, Keys, 72 + 34 pixels into the page and 34 high: the page
  # changes section by itself (push_state).
  click 100 $((top + 123))
  sleep 2.5
  shot 02-keys 0.5
  check "a link to another section works" \
    "$(grep -q 'sig://help/keys' "$out/state/sighurt/session.toml"; echo $?)" \
    "$(grep -o 'sig://help[^"]*' "$out/state/sighurt/session.toml" | tr '\n' ' ')"
  check "and the page shows it" "$(cmp -s "$out/01-help.png" "$out/02-keys.png"; [[ $? == 1 ]]; echo $?)" "01-help.png vs 02-keys.png"
  key '?'
  sleep 2.5
  check "? opens help again" "$([[ $(tabs_count) == 3 ]]; echo $?)" "$(tabs_now)"
  key d
  sleep 1
  check "d closes the tab" "$([[ $(tabs_count) == 2 ]]; echo $?)" "$(tabs_now)"
  key ':'
  wtype "tab.new demo://open-sig"
  key -k Return
  sleep 1.5
  click 640 $((top + 200))
  sleep 0.5
  check "a demo page can't open sig://help, and is told so" \
    "$([[ $(tabs_count) == 3 && $(shown_engine) == "['demo']" && $(diag "d['status']") == notice ]]; echo $?)" \
    "$(tabs_now) $(shown_engine), status $(diag "d['status']")"
  shot 03-refused 0.5
  key q
  wait "$sig_pid"
  ;;

terminal)
  # sig://terminal with [pages.terminal] on and /bin/sh as the shell, in a private home. The
  # page's programs carry a marker in their environment (sig passes its environment on), so the
  # checks count them, and sig's zombies, from /proc.
  home=$out/home
  mkdir -p "$home"
  # An "editor" for sig://terminal/config: appends a line to the file it is given.
  printf '#!/bin/sh\necho "# edited in sig://terminal/config" >> "$1"\n' > "$home/editor"
  chmod +x "$home/editor"
  terminal_config() { # terminal_config true|false
    cat > "$out/config/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
[pages.terminal]
enabled = $1
command = "/bin/sh"
CONF
  }
  terminal_config true
  start_term() {
    HOME=$home VISUAL=$home/editor PAGETERM_MARK=$out ENV=/dev/null PS1='$ ' \
      "$tdir/debug/sig-headless-e2e" --diagnostics "$out/diag.json" "$@" >> "$out/sig.log" 2>&1 &
    sig_pid=$!
    sleep 2
  }
  # programs: "LIVE ZOMBIES": the marked processes alive, and sig's children that are zombies.
  programs() {
    python3 - "$out" "$sig_pid" <<'PY'
import os, sys
mark, sig = ('PAGETERM_MARK=' + sys.argv[1]).encode(), sys.argv[2]
live = zombies = 0
for p in filter(str.isdigit, os.listdir('/proc')):
    try:
        state, ppid = open(f'/proc/{p}/stat').read().rsplit(')', 1)[1].split()[:2]
        env = open(f'/proc/{p}/environ', 'rb').read().split(b'\0') if state != 'Z' else []
    except OSError:
        continue
    zombies += state == 'Z' and ppid == sig
    live += p != sig and mark in env
print(live, zombies)
PY
  }
  # until_programs "LIVE ZOMBIES" [SECONDS]: 0 once programs says so, else 1.
  until_programs() {
    for _ in $(seq $((${2:-5} * 10))); do
      [[ $(programs) == "$1" ]] && return 0
      sleep 0.1
    done
    return 1
  }
  enter() { key -k Return; }
  type_line() { wtype "$1"; sleep 0.2; enter; }
  normal() { wtype -M ctrl '\' -m ctrl; wtype -M ctrl n -m ctrl; sleep 0.3; } # ctrl-\ ctrl-n
  command_line() { key ':'; wtype "$1"; sleep 0.2; enter; sleep 1.5; }
  view() { diag "[(x['width'], x['height']) for x in t if x['role']=='view' and x['shown']][0]"; }
  rss() { awk '/VmRSS/ {print $2}' "/proc/$sig_pid/status"; }

  start_term sig://terminal
  shot 01-start 1
  check "sig://terminal opens and starts nothing by itself" \
    "$([[ $(diag "[x['engine'] for x in t if x['role']=='view' and x['shown']]") == "['wasm']" && $(programs) == "0 0" ]]; echo $?)" \
    "programs $(programs)"
  key -k Escape
  enter
  check "Enter in INSERT starts the shell" "$(until_programs '1 0'; echo $?)" "programs $(programs)"
  type_line 'echo hi > ~/hi.txt; stty size > ~/size1.txt'
  sleep 1
  check "typed text reaches the shell" "$([[ $(cat "$home/hi.txt" 2>/dev/null) == hi ]]; echo $?)" \
    "hi.txt: $(cat "$home/hi.txt" 2>/dev/null)"
  type_line 'clear; echo hi'
  shot 02-echo-hi 1
  read -r rows cols < "$home/size1.txt" || rows=0 cols=0
  size=$(view 2>/dev/null || echo '[0, 0]')
  # A cell is 18 pixels high (15 pixels times 1.2, the canvas's line box) and about 9 wide.
  fits=$(python3 -c "w, h = $size; print(0 if $rows == h // 18 and 7 * $cols <= w <= 11 * ($cols + 1) else 1)" 2>/dev/null || echo 1)
  check "stty size is the grid that fits the page" "$fits" "stty $rows $cols, page $size"
  step "resize the output to 1000x700"
  swaymsg -q output HEADLESS-1 resolution 1000x700
  sleep 1.5
  type_line 'stty size > ~/size2.txt'
  sleep 1
  read -r rows2 cols2 < "$home/size2.txt" || rows2=0 cols2=0
  size2=$(view 2>/dev/null || echo '[0, 0]')
  fits=$(python3 -c "(w, h), (w0, h0) = $size2, $size; print(0 if $rows2 == h // 18 and 0 < $cols2 < $cols and abs($cols2 / $cols - w / w0) < 0.03 else 1)" 2>/dev/null || echo 1)
  check "a resize of the window is followed" "$fits" "stty $rows2 $cols2, page $size2 (was $rows $cols, $size)"
  swaymsg -q output HEADLESS-1 resolution ${layout_w}x${layout_h}
  sleep 1
  type_line 'exit'
  check "exit ends the shell, reaped" "$(until_programs '0 0'; echo $?)" "programs $(programs)"
  shot 03-exited 1

  enter
  check "Enter starts it again" "$(until_programs '1 0'; echo $?)" "programs $(programs)"
  before=$(rss)
  type_line 'yes'
  sleep 5
  normal
  command_line 'tab.new demo://pattern'
  check "another tab is responsive during an output flood" "$(shows_tab 2)" "$(tabs_now)"
  # K (tab.previous), with the Shift a keyboard sends: wtype's own `K` has none.
  key -M shift k -m shift
  sleep 0.5
  key -k Escape
  sleep 15
  peak=$(rss)
  wtype -M ctrl c -m ctrl
  sleep 1
  type_line 'echo ok > ~/ok.txt'
  sleep 1.5
  check "after 20 s of yes and Ctrl+C the shell answers" \
    "$([[ $(cat "$home/ok.txt" 2>/dev/null) == ok ]]; echo $?)" "ok.txt: $(cat "$home/ok.txt" 2>/dev/null)"
  check "the flood kept sig's memory bounded (under 64 MB more)" \
    "$([[ $((peak - before)) -lt 65536 ]]; echo $?)" "VmRSS $before kB, then $peak kB"
  check "and the view alive" \
    "$([[ $(diag "[x['engine'] for x in t if x['role']=='view' and x['shown']]") == "['wasm']" ]]; echo $?)" "$(tabs_now)"
  shot 04-after-flood 1

  if command -v nvim >/dev/null; then
    type_line 'nvim -u NONE -i NONE ~/n.txt'
    sleep 2.5
    shot 05-nvim 0.5
    key i
    wtype 'written in sig'
    sleep 0.3
    key -k Escape
    wtype ':wq'
    enter
    sleep 1.5
    check "nvim: insert, Escape and :wq write the file" \
      "$([[ $(cat "$home/n.txt" 2>/dev/null) == 'written in sig' ]]; echo $?)" "n.txt: $(cat "$home/n.txt" 2>/dev/null)"
  else
    echo "SKIP $scenario: nvim is not on PATH"
  fi

  normal
  key x
  check "closing the tab ends the shell, reaped" "$(until_programs '0 0'; echo $?)" "programs $(programs)"
  command_line 'tab.new sig://terminal'
  key -k Escape
  enter
  until_programs '1 0'
  normal
  command_line 'page.open demo://pattern'
  check "navigating away ends the shell, reaped" "$(until_programs '0 0'; echo $?)" "programs $(programs)"
  key x
  command_line 'tab.new sig://terminal'
  key -k Escape
  enter
  until_programs '1 0'
  normal
  key r
  check "reloading ends the shell, reaped" "$(until_programs '0 0'; echo $?)" "programs $(programs)"
  key -k Escape
  enter
  check "and Enter after the reload starts one" "$(until_programs '1 0'; echo $?)" "programs $(programs)"
  normal
  command_line 'tab.new sig://terminal/config'
  key -k Escape
  enter
  sleep 2
  check "sig://terminal/config runs \$VISUAL on the configuration" \
    "$(grep -q 'edited in sig://terminal/config' "$out/config/sighurt/config.toml"; echo $?)" \
    "$(tail -1 "$out/config/sighurt/config.toml")"
  normal
  key q
  wait "$sig_pid"
  check "quitting ends the shell" "$(until_programs '0 0'; echo $?)" "programs $(programs)"

  start_term sig://terminal
  key -k Escape
  enter
  until_programs '1 0'
  kill -TERM "$sig_pid"
  wait "$sig_pid"
  check "SIGTERM to sig ends the shell" "$(until_programs '0 0'; echo $?)" "programs $(programs)"

  terminal_config false
  start_term sig://terminal
  key -k Escape
  enter
  sleep 1.5
  check "with [pages.terminal] off nothing starts" "$([[ $(programs) == '0 0' ]]; echo $?)" "programs $(programs)"
  shot 06-off 0.5
  normal
  key q
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
