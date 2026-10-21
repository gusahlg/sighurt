#!/usr/bin/env bash
# Drives examples/servo-window inside a private headless sway: input, text, the clipboard,
# scale changes, resizes, tabs (hide/show), history, idle, WebGL animation, two views at once
# and a real web page. Never point it at a real desktop: it types and clicks.
#
#   tools/heavy tools/headless-sway --renderer gles2 -- engines/sighurt-engine-servo/tests/window-checks.sh OUT
#
# Run it on louise-pc (Servo builds there), with SWAY_UNSUPPORTED_GPU=true for sway's GLES2
# renderer on the NVIDIA driver. Needs target/debug/examples/servo-window and m1-mixed's
# target/debug/m1-pointer (a virtual pointer that stays plugged in). OUT receives the logs and
# screenshots. The window covers the 1280x800 output, so window coordinates are output ones.
# It prints PASS or FAIL per check and exits with the number of failures.
set -u
out=$1
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "window-checks.sh: run me inside tools/headless-sway" >&2
  exit 2
}
bin=${SERVO_WINDOW:-target/debug/examples/servo-window}
pages=engines/sighurt-engine-servo/tests/pages
m1=experiments/m1-mixed/pages
mkdir -p "$out"
export PATH="$(nix-build '<nixpkgs>' -A wl-clipboard --no-out-link)/bin:$PATH"
shot() { sleep "${2:-0.5}"; grim "$out/$1.png"; }
app='[app_id=sighurt-servo]'

wtype -s 600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
target/debug/m1-pointer < "$XDG_RUNTIME_DIR/pointer" &
exec 3> "$XDG_RUNTIME_DIR/pointer"
# point X Y [W H]: moves to (X, Y) of the W×H output-logical layout (default 1280×800).
point() { echo "move $1 $2 ${3:-1280} ${4:-800}" >&3; }
click() { echo click >&3; }

# start LOG ARGS...: runs servo-window with ARGS, its actions from fd 4.
start() {
  local log=$1
  shift
  rm -f "$XDG_RUNTIME_DIR/actions"
  mkfifo "$XDG_RUNTIME_DIR/actions"
  "$bin" --audit-gl "$@" < "$XDG_RUNTIME_DIR/actions" > "$out/$log" 2>&1 &
  pid=$!
  exec 4> "$XDG_RUNTIME_DIR/actions"
}
act() { echo "$1" >&4; }
finish() {
  act quit
  wait "$pid"
  echo "exit code $?" >> "$out/$1"
  exec 4>&-
}

# 1. Input, text, clipboard, scales, tabs, history, resizes, idle.
start run1.log "$pages/input.html" "$pages/second.html"
sleep 8
shot 01-loaded 0
point 100 100
click
sleep 0.5
point 100 215
click
sleep 0.3
wtype 'åäö'
sleep 0.5
wtype -M ctrl a -m ctrl
wtype -M ctrl c -m ctrl
sleep 0.5
wl-paste -n > "$out/copied.txt" 2>&1
wl-copy -n 'xyz'
sleep 0.3
point 300 215
click
wtype -k End
wtype -M ctrl v -m ctrl
sleep 0.8
shot 02-typed
# A wheel notch over the page (wlrctl's pointer comes and goes, m1-pointer's stays plugged in,
# so the seat keeps its pointer).
point 640 500
wlrctl pointer scroll 15 0
sleep 0.5
for scale in 1.5 2 1.25 1; do
  swaymsg -q output HEADLESS-1 scale "$scale"
  shot "03-scale-$scale" 1.5
done
swaymsg -q output HEADLESS-1 scale 1.5
sleep 1.5
point 100 100 853 533
click
sleep 0.5
swaymsg -q output HEADLESS-1 scale 1
sleep 1.5
act select=2
shot 04-second-tab 1
act select=1
shot 05-first-tab 1
for _ in 1 2 3 4 5; do
  act select=2
  sleep 0.2
  act select=1
  sleep 0.2
done
act hide
sleep 0.3
swaymsg -q output HEADLESS-1 scale 1.5
sleep 1.2
act show
shot 06-shown-after-hidden-scale 1
swaymsg -q output HEADLESS-1 scale 1
sleep 1.2
act "open=$PWD/$pages/history.html"
sleep 3
act "open=$PWD/$pages/second.html"
sleep 2
act back
sleep 1
act back
sleep 1
act forward
sleep 1
shot 07-history
swaymsg -q "$app floating enable"
for size in "800 600" "641 479" "1001 701" "333 257" "900 650"; do
  swaymsg -q "$app resize set $size"
  shot "08-resize-${size/ /x}" 0.8
done
for i in $(seq 40); do
  swaymsg -q "$app resize set $((400 + i * 17 % 500)) $((300 + i * 11 % 400))"
done
swaymsg -q "$app resize set 900 650"
shot 09-after-storm 1.5
swaymsg -t get_tree > "$out/tree-after-storm.json"
act mark
sleep 6
act mark
sleep 0.3
finish run1.log

# 2. CSS animation and WebGL, with hide/show cycles.
start run2.log "$m1/animated.html"
sleep 6
shot 10-animated 0
for _ in 1 2 3; do
  act hide
  sleep 0.5
  act show
  sleep 1
done
sleep 3
finish run2.log

# 3. Two views at once, side by side, both animating.
start run3.log --split "$m1/animated.html" "$m1/animated.html"
sleep 8
shot 11-split 0
finish run3.log

# 4. A real page from the web, if the network is there.
start run4.log https://example.com/
sleep 10
shot 12-web 0
finish run4.log

exec 3>&-
kill "$keyboard" 2>/dev/null
cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null

failures=0
check() {
  local name=$1 verdict=PASS
  shift
  "$@" || { verdict=FAIL; failures=$((failures + 1)); }
  echo "$verdict  $name" | tee -a "$out/verdict.txt"
}
has() { grep -qF -- "$2" "$out/$1"; }
count_at_least() { [[ $(grep -cF -- "$2" "$out/$1") -ge $3 ]]; }
gpu() { grep -q 'report: GL Some' "$out/$1" && ! grep -q 'software: true' "$out/$1"; }
no_readback() { has "$1" "read_to_image 0, GL reads Some(0)"; }
clean_exit() { has "$1" "the compositor reported no error" && has "$1" "exit code 0"; }
frames_at_least() {
  local frames
  frames=$(grep -oE 'report: frames [0-9]+' "$out/$1" | grep -oE '[0-9]+$')
  [[ -n $frames && $frames -ge $2 ]]
}
idle() {
  local marks
  marks=($(grep -oE 'mark: [0-9]+ wakeups' "$out/run1.log" | grep -oE '[0-9]+'))
  [[ ${#marks[@]} -eq 2 && $((marks[1] - marks[0])) -le 1 ]]
}
# The commits after the first `back`: back, back, forward through history.html's entries, each
# a traversal.
traversals() {
  awk '/action back/ { on = 1 } on && /NavigationCommitted/ {
         match($0, /url: "[^"]*"/); url = substr($0, RSTART + 6, RLENGTH - 7); sub(/.*\//, "", url)
         print url, ($0 ~ /history: Some\(Traverse\)/ ? "traverse" : "other") }' "$out/run1.log" |
    head -3 | paste -sd' '
}
back_and_forward() {
  local got
  got=$(traversals)
  echo "traversal commits: $got" >> "$out/verdict.txt"
  [[ $got == "history.html#two traverse history.html#one traverse history.html#two traverse" ]]
}
# The window's last configured size before the idle marks is the size sway gave it after the
# storm (sway sometimes drops the storm's final `resize set`, M1.md).
size_after_storm() {
  local sway page
  sway=$(python3 -c 'import json, sys
def find(node):
    if node.get("app_id") == "sighurt-servo":
        return "%dx%d" % (node["rect"]["width"], node["rect"]["height"])
    for child in node.get("nodes", []) + node.get("floating_nodes", []):
        if found := find(child):
            return found
print(find(json.load(open(sys.argv[1]))) or "")' "$out/tree-after-storm.json")
  page=$(awk '/action mark/ { exit } /\] window [0-9]+x[0-9]+/ { match($0, /window [0-9]+x[0-9]+/); size = substr($0, RSTART + 7, RLENGTH - 7) }
              END { print size }' "$out/run1.log")
  echo "after the storm: sway ${sway:-?}, window ${page:-?}" >> "$out/verdict.txt"
  [[ -n $sway && $sway == "$page" ]]
}
both_views_presented() {
  [[ $(grep -cE 'report: ViewId\([0-9]+\) presented [0-9]{2,}' "$out/run3.log") -eq 2 ]]
}
: > "$out/verdict.txt"
check "GPU renderer, not a software fallback" gpu run1.log
check "clicks land at 100,100 at scale 1 and 1.5" count_at_least run1.log 'mousedown 100,100' 2
check "typed åäö reaches the field" has run1.log 'Some("field åäö")'
check "copy goes to the Wayland clipboard" grep -qx 'åäö' "$out/copied.txt"
check "paste comes from the Wayland clipboard" has run1.log 'Some("field åäöxyz")'
check "the page sees scale 1.5" has run1.log 'resize dpr 1.5'
check "the page sees scale 2" has run1.log 'resize dpr 2 '
check "same-document entries in the history" has run1.log 'history.html#two'
check "back, back, forward commit as traversals, in order" back_and_forward
check "a wheel notch reaches the page" has run1.log 'Some("scrolled")'
check "the window ends the storm at sway's size" size_after_storm
check "an idle page doesn't wake the loop" idle
check "no page readback (input run)" no_readback run1.log
check "clean exit (input run)" clean_exit run1.log
check "WebGL page animates" frames_at_least run2.log 200
check "no page readback (WebGL run)" no_readback run2.log
check "clean exit (WebGL run)" clean_exit run2.log
check "two views present at once" both_views_presented
check "clean exit (two views)" clean_exit run3.log
check "a real web page loads" has run4.log 'Some("Example Domain")'
check "clean exit (web page)" clean_exit run4.log
echo "$failures failed" | tee -a "$out/verdict.txt"
exit "$failures"
