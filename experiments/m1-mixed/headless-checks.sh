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
# At the end it checks the log, prints PASS or FAIL for each check, and exits with the number
# of failures.
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
step "F2: hide; output scale 1.5 while hidden; F2: show"
wtype -k F2
sleep 0.3
swaymsg -q output HEADLESS-1 scale 1.5
sleep 1.2
wtype -k F2
shot 04-shown-after-hidden-scale 1
swaymsg -q output HEADLESS-1 scale 1
sleep 1.2

step "hold x on the page, click the chrome meanwhile: the page gets x's key-up on blur"
point 100 140
click
sleep 0.3
wtype -P x -s 1500 -p x &
held=$!
sleep 0.6
point 300 20
click
wait "$held"
sleep 0.3

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
step "odd size 1001x701 (page 1001x661), then output scale 2 and back, twice"
swaymsg -q "$app resize set 1001 701"
sleep 0.6
for scale in 2 1 2 1; do
  swaymsg -q output HEADLESS-1 scale "$scale"
  shot "06-odd-scale-$scale" 1.2
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

# The verdict, from the log.
log=$out/m1.log
failures=0
# check NAME TEST...: runs TEST and reports it, on stdout and in the log.
check() {
  local name=$1 verdict=PASS
  shift
  "$@" || { verdict=FAIL; failures=$((failures + 1)); }
  echo "$verdict  $name" | tee -a "$log"
}
has() { grep -qF -- "$1" "$log"; }
lacks() { ! grep -qF -- "$1" "$log"; }
count() { [[ $(grep -cF -- "$1" "$log") -eq $2 ]]; }
# Lines in this order.
ordered() {
  local line
  line=$(grep -nF -- "$1" "$log" | head -1 | cut -d: -f1)
  [[ -n $line ]] && tail -n "+$line" "$log" | grep -qF -- "$2"
}
hides_match_shows() {
  local hs
  hs=$(grep -oE 'hide/show: [0-9]+ hides, [0-9]+ shows' "$log" | grep -oE '[0-9]+' | paste -sd' ')
  read -r hides shows <<< "$hs"
  [[ -n ${hides:-} && $hides -ge 7 && $hides -eq $shows ]]
}
root_focus_only() {
  has "keyboard: enter on the root surface" && lacks "keyboard: enter on a surface that is not the root"
}
shm_bounded() {
  grep -qE 'shm: at most [0-2] chrome and [01] root buffers of old sizes' "$log"
}
echo "verdict:" | tee -a "$log"
check "GL renderer and presentation path diagnosed" has "presentation: "
check "GL renderer named" has "gl: renderer "
check "page clicks at 100 % and 150 % (or integer 200 %) land at client 100,100" \
  count "mousedown client 100,100" 3
check "keyboard focus only ever on the root surface" root_focus_only
check "keys reach the page when it has focus" has "keydown key y"
check "keys reach the chrome when it has focus" has "abc\""
check "Servo told of focus and blur" \
  ordered "page: servo reports focus true" "page: servo reports focus false"
check "a key held on blur goes up with its own key value" count "keyup key x" 2
check "no key-up without a key value" lacks "keyup key Unidentified"
check "hide/show cycles all completed" hides_match_shows
check "odd-size page at scale 2 (buffer 2002x1322)" has "page buffer 2002x1322"
check "old SHM buffers bounded (at most 2 chrome, 1 root)" shm_bounded
check "no page readback" has "readbacks: read_to_image 0, GL reads 0"
check "idle: no wakeups in the last 10 s" \
  grep -qE 'in the 10 s before the one that ended the run: 0 ' "$log"
check "GL context destroyed before its surface" \
  ordered "GL context and EGL window surface destroyed" "page wl_subsurface and wl_surface destroyed"
check "no protocol error" has "the compositor reported no error"
check "exit code 0" has "m1-mixed exited with 0"
echo "$failures failed" | tee -a "$log"
exit "$failures"
