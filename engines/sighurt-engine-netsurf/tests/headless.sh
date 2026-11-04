#!/usr/bin/env bash
# `sig` with the NetSurf engine, end to end, in a private headless sway (never the desktop: it
# types and clicks). Builds sig (--no-default-features --features engine-netsurf,engine-demo,
# frontend-tui) and the virtual-pointer helper into $CARGO_TARGET_DIR (default
# /data/sig-builds/targets/netsurf), then runs the checks and prints PASS or FAIL for each. The
# configuration, session and profile directories are all under OUT: nothing touches the
# user's own sig files. Screenshots and the log go to OUT; look at them too.
#
#   engines/sighurt-engine-netsurf/tests/headless.sh OUT
#   engines/sighurt-engine-netsurf/tests/headless.sh --no-build OUT
#
# Checks: the page renders (background, heading, text) through the host's SHM target; a click on
# a link follows it (through NavigationRequested); typing into a field, with accents; scale 2 and
# a resize; the idle chrome repaints nothing; and sig --tui in kitty shows the page as a kitty
# graphics image.
# Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/data/sig-builds/targets/netsurf}

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: headless.sh [--no-build] OUT}")
  mkdir -p "$out/bin"
  if [[ $build == 1 ]]; then
    SIG_WHAT="netsurf e2e build" tools/cargo build -p sighurt-browser --no-default-features \
      --features engine-netsurf,engine-demo,frontend-tui --bins --example virtual-pointer || exit 1
  fi
  cp "$CARGO_TARGET_DIR/debug/sig" "$CARGO_TARGET_DIR/debug/examples/virtual-pointer" "$out/bin/"
  SIG_WHAT="netsurf e2e" tools/heavy tools/headless-sway --size 1000x700 --renderer auto -- "$0" --inside "$out"
  exit $?
fi

out=$2
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "headless.sh: run me through tools/headless-sway" >&2
  exit 2
}
pages=$repo/engines/sighurt-engine-netsurf/tests/pages
mkdir -p "$out/config/sighurt" "$out/state" "$out/data" "$out/cache" "$out/downloads"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_DATA_HOME=$out/data
export XDG_CACHE_HOME=$out/cache XDG_DOWNLOAD_DIR=$out/downloads HOME=$out
export SIG_DIAG=1 RUST_LOG=${RUST_LOG:-info}
cat > "$out/config/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "netsurf"
# These keys are the bindings keymap's; the default keymap (example) ignores [keys].
[input]
keymap = "bindings"
[keys]
"ctrl-t" = "tab.new"
"ctrl-l" = "location.focus"
"ctrl-q" = "browser.quit"
"j" = "page.zoom-in"
CONF

failures=0
check() { if [[ $2 == 0 ]]; then echo "PASS $1 ($3)"; else echo "FAIL $1 ($3)"; failures=$((failures + 1)); fi; }
shot() { sleep "${2:-0.7}"; grim -t ppm "$out/$1.ppm"; grim "$out/$1.png"; }
# px FILE X Y: the pixel as rrggbb.
px() {
  python3 - "$1" "$2" "$3" <<'PY'
import re, sys
d = open(sys.argv[1], 'rb').read()
m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', d)
w, p = int(m[1]), d[m.end():]
x, y = int(sys.argv[2]), int(sys.argv[3])
print(p[3*(y*w+x):3*(y*w+x)+3].hex())
PY
}
# dark FILE X0 Y0 X1 Y1: how many dark pixels in the rectangle.
dark() {
  python3 - "$@" <<'PY'
import re, sys
d = open(sys.argv[1], 'rb').read()
m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', d)
w, p = int(m[1]), d[m.end():]
x0, y0, x1, y1 = map(int, sys.argv[2:6])
n = 0
for y in range(y0, y1):
    for x in range(x0, x1):
        r, g, b = p[3*(y*w+x):3*(y*w+x)+3]
        n += r < 100 and g < 100 and b < 100
print(n)
PY
}

wtype -s 3600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
"$out/bin/virtual-pointer" < "$XDG_RUNTIME_DIR/pointer" &
pointer_pid=$!
exec 3> "$XDG_RUNTIME_DIR/pointer"
layout_w=1000 layout_h=700
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
click() { point "$1" "$2"; sleep 0.15; echo click >&3; sleep 0.4; }

"$out/bin/sig" --gui --diagnostics "$out/diag.json" "file://$pages/page.html" > "$out/sig.log" 2>&1 &
sig_pid=$!
sleep 3
shot 01-page
# The page: #f4f1e8 body, the dark blue heading #234, a white box. Chrome is 84 px tall.
check "the page's background is NetSurf's" "$([[ $(px "$out/01-page.ppm" 700 400) == f4f1e8 ]]; echo $?)" "$(px "$out/01-page.ppm" 700 400)"
check "the box is white" "$([[ $(px "$out/01-page.ppm" 300 230) == ffffff ]]; echo $?)" "$(px "$out/01-page.ppm" 300 230)"
check "the heading's text is drawn" "$([[ $(dark "$out/01-page.ppm" 20 100 350 145) -gt 300 ]]; echo $?)" "$(dark "$out/01-page.ppm" 20 100 350 145) dark pixels"
check "the engine chip says netsurf" "$(grep -q 'renderer engine=netsurf mode=cpu' "$out/sig.log"; echo $?)" "$(grep -o 'renderer engine=netsurf[^ ]*[^ ]* [^ ]*' "$out/sig.log" | head -1)"

# A click on the link "second page" (page-local (118, 144)): follows it.
click 118 228
shot 02-second-page 1.2
check "the link was followed" "$([[ $(px "$out/02-second-page.ppm" 700 400) == e8f1f4 ]]; echo $?)" "background $(px "$out/02-second-page.ppm" 700 400)"

# Back to the first page by its link, then type into the field.
click 100 190
shot 03-back 1.2
check "the link back was followed" "$([[ $(px "$out/03-back.ppm" 700 400) == f4f1e8 ]]; echo $?)" "$(px "$out/03-back.ppm" 700 400)"
before=$(dark "$out/03-back.ppm" 20 330 215 356)
click 115 343
wtype 'jjhéllo' ; sleep 0.8
shot 04-typed 0.3
after=$(dark "$out/04-typed.ppm" 20 330 215 356)
check "typing in a field draws the text, accents included" "$([[ $after -gt $before ]]; echo $?)" "$before -> $after dark pixels in the field"

# Plain letters typed into the page do not trigger the configured binding ("j" = zoom in):
heading_before=$(dark "$out/03-back.ppm" 20 100 350 145)
heading_after=$(dark "$out/04-typed.ppm" 20 100 350 145)
check "j typed into the field is text, not the zoom-in binding" "$([[ $heading_before == $heading_after ]]; echo $?)" "heading $heading_before -> $heading_after dark pixels"

swaymsg output HEADLESS-1 scale 2 > /dev/null
shot 05-scale-2 1.2
check "scale 2 repaints the page" "$([[ $(dark "$out/05-scale-2.ppm" 20 100 700 300) -gt 100 ]]; echo $?)" "$(dark "$out/05-scale-2.ppm" 20 100 700 300) dark pixels"
swaymsg output HEADLESS-1 scale 1 > /dev/null
shot 06-scale-1 1.0

# Idle: the chrome and the page repaint nothing.
sleep 1
kill -USR1 "$sig_pid"; sleep 0.3
paints_a=$(python3 -c "import json; print(json.load(open('$out/diag.json'))['counters']['chrome_paints'])")
sleep 4
kill -USR1 "$sig_pid"; sleep 0.3
paints_b=$(python3 -c "import json; print(json.load(open('$out/diag.json'))['counters']['chrome_paints'])")
check "an idle page repaints no chrome" "$([[ $paints_a == $paints_b ]]; echo $?)" "$paints_a -> $paints_b chrome paints in 4 s"

kill "$sig_pid" 2>/dev/null; wait "$sig_pid" 2>/dev/null

# The terminal frontend: kitty graphics. Kitty is the pinned nixpkgs' one.
kitty=$(nix-build "$repo/nix/nixpkgs.nix" -A kitty --no-out-link 2>/dev/null)/bin/kitty
if [[ -x $kitty ]]; then
  mkdir -p "$out/kitty-config"
  cat > "$out/tui.sh" <<RUN
#!/usr/bin/env bash
exec "$out/bin/sig" --tui --engine netsurf "file://$pages/page.html"
RUN
  chmod +x "$out/tui.sh"
  XDG_CONFIG_HOME=$out/config "$kitty" --config NONE "$out/tui.sh" > "$out/kitty.log" 2>&1 &
  kitty_pid=$!
  sleep 6
  shot tui-01-kitty 0.2
  # Page rows of the terminal window: the page's background is a kitty image, not the terminal's.
  light=$(python3 - "$out/tui-01-kitty.ppm" <<'PY'
import re, sys
d = open(sys.argv[1], 'rb').read()
m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', d)
w, h, p = int(m[1]), int(m[2]), d[m.end():]
n = sum(1 for i in range(0, w*h*3, 3) if abs(p[i]-0xf4) < 6 and abs(p[i+1]-0xf1) < 6 and abs(p[i+2]-0xe8) < 6)
print(n * 100 // (w*h))
PY
)
  check "sig --tui in kitty draws the page as an image" "$([[ $light -gt 30 ]]; echo $?)" "$light % of the window is the page's background"
  kill "$kitty_pid" 2>/dev/null; wait "$kitty_pid" 2>/dev/null
else
  check "kitty is available" 1 "no kitty"
fi
exec 3>&-
kill "$keyboard" "$pointer_pid" 2>/dev/null
grep -q panicked "$out/sig.log" && check "no panic" 1 "see sig.log"
cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null
echo "headless.sh: $failures failed check(s); screenshots and logs in $out"
exit $failures
