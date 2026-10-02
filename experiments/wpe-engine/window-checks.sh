#!/usr/bin/env bash
# Checks of the WPE prototype (target/debug/wpe-window) in a private compositor: presentation,
# idle, animation without chrome repaints, hidden views, input (pointer, å ä ö and a dead key,
# focus, wheel), scale and size changes, a crashed web process, shutdown. Prints PASS, FAIL or
# INFO per check and exits with the number of failures. Never run it on the desktop: it types and
# clicks into whatever compositor it reaches.
#
#   tools/cargo build -p wpe-engine
#   tools/cargo build -p sighurt-browser --no-default-features --example virtual-pointer
#   tools/heavy tools/headless-sway --renderer gles2 -- experiments/wpe-engine/window-checks.sh OUT
#   tools/heavy tools/headless-sway --renderer gles2 -- experiments/wpe-engine/window-checks.sh OUT --shm
#
# With the pixman renderer sway offers no DMA-BUF, so the runtime falls back to shared memory by
# itself; `--shm` forces that path on the GPU renderer. OUT receives the logs and screenshots.
set -u
# Its own session bus without service activation, as nix/wpe-smoke.sh: WebKit's D-Bus calls
# reach neither the user's session nor services started for them.
if [[ -z ${WPE_CHECKS_BUS:-} ]]; then
  conf=$(mktemp)
  cat > "$conf" << 'EOF'
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
  WPE_CHECKS_BUS=1 dbus-run-session --config-file="$conf" -- "$0" "$@"
  status=$?
  rm -f "$conf"
  exit $status
fi
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
out=${1:?usage: window-checks.sh OUT [--shm]}
shift
mkdir -p "$out"
pages=experiments/wpe-engine/pages
renderer=${SIG_SWAY_RENDERER:-unknown}
failures=0
check() { # check NAME COMMAND...: runs COMMAND, prints PASS or FAIL
  if "${@:2}"; then echo "PASS $renderer: $1"; else
    echo "FAIL $renderer: $1"; failures=$((failures + 1)); fi
}
info() { echo "INFO $*"; }

# Input devices that stay plugged in (wtype and wlrctl plug in a new device per command).
wtype -s 3600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
target/debug/examples/virtual-pointer < "$XDG_RUNTIME_DIR/pointer" &
exec 3> "$XDG_RUNTIME_DIR/pointer"
layout_w=1280 layout_h=800
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
click() { point "$1" "$2"; sleep 0.1; echo click >&3; sleep 0.3; }

# start NAME ARGS...: runs wpe-window, taking actions from a FIFO.
pid=
start() {
  log=$out/$1.log
  shift
  rm -f "$XDG_RUNTIME_DIR/actions"
  mkfifo "$XDG_RUNTIME_DIR/actions"
  target/debug/wpe-window "$@" < "$XDG_RUNTIME_DIR/actions" > "$log" 2>&1 &
  pid=$!
  exec 4> "$XDG_RUNTIME_DIR/actions"
}
act() { echo "$1" >&4; sleep "${2:-0.3}"; }
stop() {
  act quit 0
  exec 4>&-
  wait "$pid"
}
# until_log SECONDS REGEX: waits until the log has a line matching REGEX.
until_log() {
  local end=$((SECONDS + $1))
  until grep -qE "$2" "$log"; do
    [[ $SECONDS -ge $end ]] && return 1
    sleep 0.1
  done
}
title() { grep -oP 'TitleChanged.*title: Some\("\K[^"]*' "$log" | tail -1; }
# mark FIELD: FIELD's value in the latest mark line ("wakeups", "chrome-paints", ...).
mark() { grep -E '\] mark: ' "$log" | grep -oP "(?<= $1 )[^ ]+" | tail -1; }
frames() { grep -E '\] mark: ViewId\('"$1"'\) frames' "$log" | tail -1 | grep -oP 'frames \K[0-9]+'; }
pixel() { # pixel X Y: the screen's pixel, as #rrggbb
  grim -t ppm - | python3 -c 'import re, sys
x, y = int(sys.argv[1]), int(sys.argv[2])
data = sys.stdin.buffer.read()
h = re.match(rb"P6\s+(\d+)\s+(\d+)\s+\d+\s", data)
at = h.end() + (y * int(h[1]) + x) * 3
print("#%02x%02x%02x" % tuple(data[at:at + 3]))' "$1" "$2"
}
eq() { [[ $1 == "$2" ]] || { echo "  got '$1', expected '$2'"; return 1; }; }
le() { [[ $1 -le $2 ]] || { echo "  got $1, expected at most $2"; return 1; }; }
ge() { [[ $1 -ge $2 ]] || { echo "  got $1, expected at least $2"; return 1; }; }

# Presentation, idle, animation, hidden views: a red page shown, an animation in a hidden tab.
start present "$@" $pages/red.html $pages/animated.html
check "the red page loads" until_log 10 'TitleChanged.*"red"'
sleep 1
check "the compositor shows it under the chrome" eq "$(pixel 640 420)" "#ff0000"
check "the chrome strip is shown" eq "$(pixel 640 20)" "#304060"
grim "$out/present.png"
act mark 5
act mark 0.5
info "$(grep 'mark: wakeups' "$log" | tail -1)"
idle=$(grep -oP 'mark: wakeups \K[0-9]+' "$log" | tail -2 | paste -sd' ' | awk '{print $2 - $1}')
dispatches=$(grep -oP 'glib-dispatches \K[0-9]+' "$log" | tail -2 | paste -sd' ' | awk '{print $2 - $1}')
info "idle: $idle loop wakeups and $dispatches GLib dispatches in 5 s"
check "idle: at most 3 wakeups in 5 s with a static page (1 is the mark)" le "$idle" 3
transport=$(mark transport)
info "transport $transport, device $(mark device), $(mark formats) DMA-BUF formats"
paints=$(mark chrome-paints)
act select=2 6
act mark 0.5
check "the animation presents in its tab (frames in 6 s)" ge "$(frames 2)" 300
check "no chrome repaint because the page animated" eq "$(mark chrome-paints)" "$paints"
info "$(grep 'mark: ViewId(2)' "$log" | tail -1)"
info "$(grep 'mark: dmabuf-frames' "$log" | tail -1)"
info "$(grep 'mark: pss-kib' "$log" | tail -1)"
if [[ $transport == "Some(Dmabuf)" ]]; then
  check "DMA-BUF: no CPU copies" eq "$(mark shm-copies)" 0
  check "DMA-BUF: buffers come back (releases)" ge "$(mark releases)" 290
else
  check "SHM: one copy per frame" ge "$(mark shm-copies)" 300
fi
grim "$out/animated.png"
red_frames=$(frames 1)
act select=1 1
act mark 0.2
hidden_before=$(frames 2)
act mark 3
act mark 0.2
check "a hidden view presents nothing" eq "$(frames 2)" "$hidden_before"
check "the red page shows again" eq "$(pixel 640 420)" "#ff0000"
info "red page frames: $red_frames before, $(frames 1) after showing it again"
act hide 1
check "hiding unmaps the page (the window's background)" eq "$(pixel 640 420)" "#202024"
act show 1
check "showing maps it again" eq "$(pixel 640 420)" "#ff0000"
stop
check "shutdown: the window closes cleanly" until_log 5 '^shut down'
grep -E 'panicked|protocol error' "$log" && failures=$((failures + 1))

# A crashed web process: the view fails, the browser goes on, a reload brings it back. One view,
# so the one web process is its own.
start crash "$@" $pages/red.html
check "crash: the page loads" until_log 10 'TitleChanged.*"red"'
descendants() { local child; for child in $(pgrep -P "$1"); do echo "$child"; descendants "$child"; done; }
web_processes() {
  local p
  for p in $(descendants "$pid"); do [[ $(cat "/proc/$p/comm" 2> /dev/null) == WPEWebProcess ]] && echo "$p"; done
}
web=$(web_processes)
info "crash: web processes of this window: $web"
# SIGKILL: a SIGSEGV sent with kill(2) is not a fault, and JSC's handler in the web process
# returns from it without the process dying.
kill -KILL $web
check "a crashed web process is ViewFailed" until_log 10 'ViewFailed'
info "$(grep -m1 ViewFailed "$log")"
check "the browser lives on" kill -0 "$pid"
act reload 0.5
check "a reload starts a new web process" until_log 10 'NavigationCommitted.*DocumentGeneration\(2\)'
sleep 1
check "the reloaded page is shown" eq "$(pixel 640 420)" "#ff0000"
info "crash: web processes now: $(web_processes)"
stop
grep -E 'panicked|protocol error' "$log" && failures=$((failures + 1))

# Input, focus, scale and size changes.
start input "$@" $pages/input.html
check "the input page loads at the window's size" until_log 10 'TitleChanged.*(load|resize) dpr 1 1280x760'
sleep 1
click 50 90
check "a click arrives at its surface-local position" eq "$(title)" "mousedown 0 50,50 detail 1"
click 100 255
wtype 'åäö'
sleep 0.3
wtype -k dead_diaeresis -k a
sleep 0.5
check "å ä ö and a dead key are typed" eq "$(title)" "field åäöä"
wtype -k F2
sleep 0.3
check "a named key reaches the page" eq "$(title)" "keydown F2 Escape"
act focus=chrome 0.5
check "the page blurs when the chrome takes the keyboard" eq "$(title)" "blur"
wtype x
sleep 0.3
check "keys for the chrome don't reach the page" eq "$(title)" "blur"
act focus=page 0.5
check "the page has focus again" eq "$(title)" "focus"
wtype y
sleep 0.3
check "and gets keys" eq "$(title)" "field åäöäy"
point 640 400
wlrctl pointer scroll 10 0
sleep 0.5
check "the wheel scrolls the page" until_log 3 'scrollY [1-9]'
for scale in 1.5 2 1.25 1; do
  swaymsg -q output HEADLESS-1 scale $scale
  layout_w=$(python3 -c "print(round(1280 / $scale))") layout_h=$(python3 -c "print(round(800 / $scale))")
  page_h=$((layout_h - 40))
  check "scale $scale: the page sees dpr $scale and ${layout_w}x$page_h" \
    until_log 5 "resize dpr $scale ${layout_w}x$page_h"
  sleep 1
  corner=$(python3 -c "print(round(90 * $scale))")
  below=$(python3 -c "print(round(40 * $scale) + 10)")
  check "scale $scale: the page is shown, its corner at 100 CSS px" \
    eq "$(pixel 10 "$below") $(pixel "$corner" "$below")" "#00aa00 #00aa00"
  grim "$out/scale-$scale.png"
  click 50 90
  check "scale $scale: clicks land at the same CSS position" eq "$(title)" "mousedown 0 50,50 detail 1"
done
swaymsg -q output HEADLESS-1 resolution 1000x700
check "a new size reaches the page" until_log 5 'resize dpr 1 1000x660'
sleep 1
check "and is shown" eq "$(pixel 10 50)" "#00aa00"
act mark 0.3
info "$(grep 'mark: wakeups' "$log" | tail -1)"
stop
grep -E 'panicked|protocol error' "$log" && failures=$((failures + 1))

kill "$keyboard" 2> /dev/null
echo "$failures failed; logs and screenshots in $out"
exit "$failures"
