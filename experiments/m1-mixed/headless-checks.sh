#!/usr/bin/env bash
# Runs the M1 checks that need input or a compositor that can be changed at will, inside a
# private headless sway. Never point this at a real desktop: it types and clicks.
#
#   tools/headless-sway [--renderer gles2|pixman] -- experiments/m1-mixed/headless-checks.sh OUT [m1-mixed args...]
#
# With --renderer gles2 on the proprietary NVIDIA driver, sway needs SWAY_UNSUPPORTED_GPU=true.
# OUT receives m1.log (the experiment's log and report), sway's log and screenshots. The
# window is tiled over the whole 1280x800 output at first, so window coordinates are output
# coordinates: the page starts at y = 40. Needs target/debug/m1-pointer next to m1-mixed.
set -u
out=$1
shift
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "headless-checks.sh: run me inside tools/headless-sway" >&2
  exit 2
}
bin=${M1_BIN:-target/debug/m1-mixed}
mkdir -p "$out"
: > "$out/m1.log"
step() { echo "[checks $(date +%T.%3N)] $*" >> "$out/m1.log"; }
shot() { sleep "${2:-0.4}"; grim "$out/$1.png"; }
app='[app_id=sighurt-m1]'

# Input devices that stay plugged in: wtype and wlrctl each add a virtual device and remove it
# when they exit, so the seat's capabilities would come and go with every command and the
# client would miss events sent before it has bound a new wl_keyboard or wl_pointer.
wtype -s 600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/m1-pointer"
"$(dirname "$bin")/m1-pointer" < "$XDG_RUNTIME_DIR/m1-pointer" &
exec 3> "$XDG_RUNTIME_DIR/m1-pointer"
# point X Y [W H]: moves to (X, Y) of the W×H output-logical layout (default 1280×800).
point() { echo "move $1 $2 ${3:-1280} ${4:-800}" >&3; }
click() { echo click >&3; }

"$bin" "$@" >> "$out/m1.log" 2>&1 &
pid=$!
sleep 8
shot 01-loaded 0

step "click the page at window (100, 140): page-local (100, 100)"
point 100 140
click
sleep 0.5
step "type xyz: goes to the page"
wtype xyz
step "click the chrome at window (300, 20)"
point 300 20
click
sleep 0.5
step "type abc: goes to the chrome"
wtype abc
shot 02-chrome-focused
step "Escape: back to the page; type q"
wtype -k Escape
wtype q
sleep 0.3
step "Ctrl+L: the chrome; type d; click the page"
wtype -M ctrl l -m ctrl
wtype d
point 600 400
click
sleep 0.3
step "F2: hide the page"
wtype -k F2
shot 03-hidden
step "F2: show the page"
wtype -k F2
shot 04-shown
for i in 1 2 3 4 5; do wtype -k F2; sleep 0.15; wtype -k F2; sleep 0.15; done

for scale in 1.25 1.5 2 1.75 1; do
  step "output scale $scale"
  swaymsg -q output HEADLESS-1 scale "$scale"
  shot "05-scale-$scale" 1.2
done
step "scale 1.5, click the page at window (100, 140): page-local (100, 100)"
swaymsg -q output HEADLESS-1 scale 1.5
sleep 1
point 100 140 853 533
click
sleep 0.5
swaymsg -q output HEADLESS-1 scale 1
sleep 1

step "floating, resized"
swaymsg -q "$app floating enable"
for size in "800 600" "641 479" "1001 701" "333 257" "900 650"; do
  swaymsg -q "$app resize set $size"
  shot "06-resize-${size/ /x}" 0.6
done
step "resize storm: 40 sizes without waiting"
for i in $(seq 40); do
  swaymsg -q "$app resize set $((400 + i * 17 % 500)) $((300 + i * 11 % 400))"
done
swaymsg -q "$app resize set 900 650"
shot 07-after-storm 1
step "idle for 12 s"
sleep 12
step "close"
swaymsg -q "$app kill"
wait "$pid"
step "m1-mixed exited with $?"
exec 3>&-
kill "$keyboard" 2>/dev/null
cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null
true
