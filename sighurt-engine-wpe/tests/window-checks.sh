#!/usr/bin/env bash
# Drives examples/wpe-window in a private compositor: presentation and the chrome strip, idle,
# WebGL and CSS animation without chrome repaints, hidden views, zero copies on DMA-BUF (or one
# counted copy per frame on the fallback), two views at once, a video, history, a crashed web
# process, input (pointer, å ä ö and a dead key, focus, the wheel), the clipboard both ways,
# the hovered link, scales 1.5, 2, 1.25 and 1, a resize, page zoom, and an https page. Prints
# PASS, FAIL or INFO per check and exits with the number of failures. Never run it on the
# desktop: it types and clicks into whatever compositor it reaches.
#
#   tools/cargo build -p sighurt-engine-wpe --example wpe-window
#   tools/cargo build -p sighurt-browser --no-default-features --example virtual-pointer
#   tools/heavy tools/headless-sway --renderer gles2 -- sighurt-engine-wpe/tests/window-checks.sh OUT
#   tools/heavy tools/headless-sway --renderer gles2 -- sighurt-engine-wpe/tests/window-checks.sh OUT --shm
#   tools/heavy tools/headless-sway --renderer pixman -- sighurt-engine-wpe/tests/window-checks.sh OUT
#
# With the pixman renderer sway offers no DMA-BUF, so WebKit falls back to shared memory by
# itself; `--shm` forces that path on the GPU renderer. OUT receives the logs and screenshots.
# The script runs itself on a D-Bus session of its own (tests/private-bus).
set -u
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
[[ -n ${WAYLAND_DISPLAY:-} && ${XDG_RUNTIME_DIR:-} == /tmp/sighl.* ]] || {
  echo "window-checks.sh: run me inside tools/headless-sway" >&2
  exit 2
}
if [[ -z ${WPE_CHECKS_BUS:-} ]]; then
  WPE_CHECKS_BUS=1 exec sighurt-engine-wpe/tests/private-bus "$0" "$@"
fi
out=${1:?usage: window-checks.sh OUT [--shm]}
shift
mkdir -p "$out"
out=$(cd "$out" && pwd)
bin=target/debug/examples/wpe-window
pages=sighurt-engine-wpe/tests/pages
renderer=${SIG_SWAY_RENDERER:-unknown}
export PATH="$(nix-build nix/nixpkgs.nix -A wl-clipboard --no-out-link)/bin:$PATH"
failures=0
check() { # check NAME COMMAND...: runs COMMAND, prints PASS or FAIL
  if "${@:2}"; then echo "PASS $renderer: $1"; else
    echo "FAIL $renderer: $1"; failures=$((failures + 1)); fi
}
info() { echo "INFO $*"; }

# Input devices that stay plugged in (wtype and wlrctl plug in a new device per command). They
# inherit tools/heavy's lock, so they must not outlive the script: a write to a helper that died
# fails rather than killing the script, and the helpers are stopped however it ends.
trap '' PIPE
mkfifo "$XDG_RUNTIME_DIR/pointer"
wtype -s 900000 ' ' &
keyboard=$!
target/debug/examples/virtual-pointer < "$XDG_RUNTIME_DIR/pointer" &
pointer=$!
trap 'kill "$keyboard" "$pointer" ${pid:-} 2> /dev/null' EXIT
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
  "$bin" "$@" < "$XDG_RUNTIME_DIR/actions" > "$log" 2>&1 &
  pid=$!
  exec 4> "$XDG_RUNTIME_DIR/actions"
}
act() { echo "$1" >&4; sleep "${2:-0.3}"; }
stop() {
  act quit 0
  exec 4>&-
  wait "$pid"
  echo "exit $?" >> "$log"
  check "$(basename "$log" .log): exits cleanly, no protocol error" \
    grep -q 'shut down: the compositor reported no error' "$log"
  ! grep -E 'panicked|protocol error' "$log" || failures=$((failures + 1))
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
# mark FIELD: FIELD's value in the latest mark lines ("wakeups", "chrome-paints", ...).
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

# 1. Presentation, idle, animation, hidden views: a red page shown, an animation in a hidden tab.
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
info "transport $transport, device $(mark device), driver $(mark driver), $(mark formats) DMA-BUF formats"
info "$(grep 'mark: renderer' "$log" | tail -1)"
paints=$(mark chrome-paints)
act select=2 6
act mark 0.5
check "the animation presents in its tab (frames in 6 s)" ge "$(frames 2)" 300
check "WebGL runs in the page" until_log 1 'TitleChanged.*"webgl [0-9]+"'
check "no chrome repaint because the page animated" eq "$(mark chrome-paints)" "$paints"
info "$(grep 'mark: ViewId(2)' "$log" | tail -1)"
info "$(grep 'mark: submitted' "$log" | tail -1)"
info "$(grep 'mark: pss-kib' "$log" | tail -1)"
if [[ $transport == "Some(Dmabuf)" ]]; then
  check "DMA-BUF: no CPU copies" eq "$(mark shm-copies)" 0
  check "DMA-BUF: buffers come back (releases)" ge "$(mark releases)" 290
else
  check "SHM fallback: one counted copy per frame" ge "$(mark shm-copies)" 300
  check "SHM fallback: diagnosed" grep -q 'SHARED-MEMORY FALLBACK' "$log"
fi
grim "$out/animated.png"
act select=1 1
act mark 0.2
hidden_before=$(frames 2)
act mark 3
act mark 0.2
check "a hidden view presents nothing" eq "$(frames 2)" "$hidden_before"
check "the red page shows again" eq "$(pixel 640 420)" "#ff0000"
act hide 1
check "hiding unmaps the page (the window's background)" eq "$(pixel 640 420)" "#202024"
act show 1
check "showing maps it again" eq "$(pixel 640 420)" "#ff0000"
stop

# 2. Two views at once, side by side, both animating.
start split "$@" --split $pages/animated.html $pages/animated.html
sleep 6
act mark 0.5
check "two views present at once (left)" ge "$(frames 1)" 200
check "two views present at once (right)" ge "$(frames 2)" 200
grim "$out/split.png"
stop

# 3. A video, from a clip made here (VP9 in WebM, through GStreamer), over http: WebKit's
# sandboxed web process reads file: media itself and sees no files outside its sandbox. The test
# compositor has no audio server, without which WebKit plays no media, so GStreamer gets its
# fake audio sink.
media=$(mktemp -d)
cp $pages/video.html "$media/"
ffmpeg -loglevel error -y -f lavfi -i testsrc=size=320x240:rate=30 -t 4 -pix_fmt yuv420p \
  -c:v libvpx-vp9 -b:v 300k "$media/clip.webm"
python3 -u -m http.server 0 --bind 127.0.0.1 --directory "$media" > "$out/http.log" 2>&1 &
server=$!
until port=$(grep -oP 'port \K[0-9]+' "$out/http.log"); do sleep 0.1; done
export GST_PLUGIN_FEATURE_RANK=fakeaudiosink:MAX
start video "$@" "http://127.0.0.1:$port/video.html"
check "the video plays" until_log 15 'TitleChanged.*"video playing 4 [1-9]'
info "$(title)"
grim "$out/video.png"
stop
unset GST_PLUGIN_FEATURE_RANK
kill "$server"
rm -rf "$media"

# 4. History: same-document entries, back and forward across documents, reload.
start history "$@" $pages/history.html
check "history: the page adds its entries" until_log 10 'HistoryChanged.*#two.*current: 2'
act "open=$PWD/$pages/second.html" 1.5
act back 1
act back 1
act forward 1
act reload 1.5
traversals=$(grep -oP 'NavigationCommitted.*url: "[^"]*/\K[^"]+(?=".*history: Some\((Traverse|Replace)\))' "$log" | paste -sd' ')
effects=$(grep -oP 'NavigationCommitted.*history: Some\(\K[A-Za-z]+' "$log" | paste -sd' ')
info "history: $traversals; effects $effects"
check "history: back, back, forward, reload commit as traversals and a replace" \
  eq "$traversals" "history.html#two history.html#one history.html#two history.html#two"
stop

# 5. A crashed web process: the view fails, the browser goes on, a reload brings it back. One
# view, so the one web process is its own.
start crash "$@" $pages/red.html
check "crash: the page loads" until_log 10 'TitleChanged.*"red"'
descendants() { local child; for child in $(pgrep -P "$1"); do echo "$child"; descendants "$child"; done; }
web_processes() {
  local p
  for p in $(descendants "$pid"); do [[ $(cat "/proc/$p/comm" 2> /dev/null) == WPEWebProcess ]] && echo "$p"; done
}
web=$(web_processes)
info "crash: web processes of this window: $web"
# SIGKILL: a SIGSEGV sent with kill(2) is not a fault, and JavaScriptCore's handler in the web
# process returns from it without the process dying.
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

# 6. Input, focus, the clipboard, the hovered link, scale and size changes.
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
wtype -M ctrl a -m ctrl
wtype -M ctrl c -m ctrl
sleep 0.5
check "copy asks the browser, which puts it on the Wayland clipboard" eq "$(wl-paste -n 2>&1)" "åäöäy"
wl-copy -n 'xyz'
sleep 0.3
wtype -k End
wtype -M ctrl v -m ctrl
sleep 1
check "paste asks the browser, which reads the Wayland clipboard" eq "$(title)" "field åäöäyxyz"
info "clipboard requests: $(grep -c 'ServiceRequested.*Clipboard' "$log")"
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
act zoom=2 0.5
# Asked, not read from the title: the page's scroll event replaces its resize title at once.
act "eval=innerWidth + 'x' + innerHeight + ' dpr ' + devicePixelRatio" 0.5
check "page zoom: the page sees dpr 2 and half the size" until_log 3 'script.*Ok\("500x330 dpr 2"\)'
act zoom=1 0.5
swaymsg -q output HEADLESS-1 resolution 1280x800
layout_w=1280 layout_h=800
act "open=$PWD/$pages/links.html" 1.5
point 100 60
check "the hovered link is the status text" until_log 3 'StatusTextChanged.*Some\(".*second.html"\)'
point 600 600
check "and leaves it" until_log 3 'StatusTextChanged.*text: None'
stop

# 7. A real page over https, if the network is there.
start web "$@" https://example.com/
if until_log 20 'TitleChanged.*"Example Domain"'; then
  echo "PASS $renderer: an https page loads"
  sleep 1.5
  act mark 0.3
  check "and is shown" ge "$(frames 1)" 1
else
  info "https://example.com/ did not load (no network?): $(grep -m1 NavigationFailed "$log")"
fi
grim "$out/web.png"
stop

exec 3>&-
cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2> /dev/null
echo "$failures failed; logs and screenshots in $out"
exit "$failures"
