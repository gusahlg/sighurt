#!/usr/bin/env bash
# Smoke test of the installed WPE WebKit (docs/rewrite/WPE.md). Compiles nix/wpe-smoke.c against
# the prefix with only what shell.nix exposes, then loads pages with it: headless always, and on
# Wayland when it runs under a compositor. Prints PASS/FAIL per check; exits 1 if any failed.
#
#   tools/heavy tools/headless-sway --renderer auto -- nix/wpe-smoke.sh [OUT_DIR]
#
# OUT_DIR (default: a temporary directory) receives the reports, logs and screenshots.
set -uo pipefail
# Its own session bus without service activation: WebKit's D-Bus calls (accessibility, portals)
# reach neither the user's session nor services started for them.
if [[ -z ${WPE_SMOKE_BUS:-} ]]; then
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
  WPE_SMOKE_BUS=1 dbus-run-session --config-file="$conf" -- "$0" "$@"
  status=$?
  rm -f "$conf"
  exit $status
fi
here=$(cd "$(dirname "$0")" && pwd)
out=${1:-$(mktemp -d)}
mkdir -p "$out"
failures=0
check() { # check NAME COMMAND...: runs COMMAND, prints PASS or FAIL
  if "${@:2}"; then echo "PASS $1"; else echo "FAIL $1"; failures=$((failures + 1)); fi
}

[[ -n ${SIGHURT_WPE_PREFIX:-} ]] || { echo "FAIL no WPE WebKit in this shell (shell.nix)"; exit 1; }
flags=$(PKG_CONFIG_PATH=$SIGHURT_WPE_PKG_CONFIG_PATH pkg-config --cflags --libs \
  wpe-webkit-2.0 wpe-platform-2.0 wpe-platform-headless-2.0) || { echo "FAIL pkg-config"; exit 1; }
# shellcheck disable=SC2086 # pkg-config output is a list of words
check "compile against $SIGHURT_WPE_PREFIX" cc -Wall -Werror -o "$out/wpe-smoke" "$here/wpe-smoke.c" $flags

# run NAME ARGS...: loads a page, waiting for its report; while the page is held, checks that the
# web process runs in the bubblewrap sandbox and, on Wayland, waits up to 4 s for the compositor
# to show the snapshot's centre pixel, then takes a screenshot.
run() {
  local name=$1
  shift
  "$out/wpe-smoke" --hold 5 "$@" > "$out/$name.json" 2> "$out/$name.log" &
  local pid=$!
  until [[ -s $out/$name.json ]] || ! kill -0 "$pid" 2> /dev/null; do sleep 0.2; done
  if kill -0 "$pid" 2> /dev/null; then
    pgrep -af 'bwrap .*WPEWebProcess' > "$out/$name.sandbox"
    if [[ $* != *--headless* ]]; then
      local want screen
      want=$(field "$name" snapshot.centre)
      for _ in $(seq 20); do
        screen=$(grim -t ppm - | screen_centre)
        [[ $screen == "$want" ]] && break
        sleep 0.2
      done
      echo "$screen" > "$out/$name.screen"
      grim "$out/$name.png"
    fi
  fi
  wait "$pid"
  local status=$?
  echo "  $name: $(cat "$out/$name.json")"
  return $status
}
screen_centre() { # the centre pixel of the binary PPM on stdin, as #rrggbb
  python3 -c 'import re, sys
data = sys.stdin.buffer.read()
header = re.match(rb"P6\s+(\d+)\s+(\d+)\s+\d+\s", data)
width, height = int(header[1]), int(header[2])
at = header.end() + ((height // 2) * width + width // 2) * 3
print("#%02x%02x%02x" % tuple(data[at:at + 3]))'
}
field() { # field NAME PATH: a field of report NAME, e.g. field headless-red page.title
  python3 -c 'import json, sys
value = json.load(open(sys.argv[1]))
for key in sys.argv[2].split("."): value = value.get(key) if isinstance(value, dict) else None
print("null" if value is None else value)' "$out/$1.json" "$2" 2> /dev/null
}
known() { # known NAME PATH: the field is there and not null
  local value
  value=$(field "$1" "$2")
  [[ -n $value && $value != null ]]
}

red='data:text/html,<title>sighurt-wpe</title><body style="background:%23ff0000">'
check "headless: a data: page loads" run headless-red --headless "$red"
check "headless: its title" test "$(field headless-red page.title)" = sighurt-wpe
check "headless: it is painted red" test "$(field headless-red snapshot.centre)" = "#ff0000"
check "headless: WebGL in the web process" known headless-red page.webgl
for type in h264 vp9 opus; do
  check "headless: GStreamer plays $type" known headless-red "page.$type"
done
check "headless: the web process is sandboxed (bwrap)" test -s "$out/headless-red.sandbox"
check "headless: https://example.com loads (TLS)" run headless-https --headless https://example.com/
check "headless: its title" test "$(field headless-https page.title)" = "Example Domain"
check "headless: webkit://gpu loads" run headless-gpu --headless webkit://gpu
check "headless: it names a GL renderer" known headless-gpu page.gpu.GL_RENDERER

if [[ -n ${WAYLAND_DISPLAY:-} ]]; then
  check "wayland: a data: page loads" run wayland-red "$red"
  check "wayland: it is painted red" test "$(field wayland-red snapshot.centre)" = "#ff0000"
  check "wayland: WebGL in the web process" known wayland-red page.webgl
  check "wayland: the web process is sandboxed (bwrap)" test -s "$out/wayland-red.sandbox"
  check "wayland: the compositor shows it" test "$(cat "$out/wayland-red.screen")" = "#ff0000"
  check "wayland: https://example.com loads" run wayland-https https://example.com/
  check "wayland: its title" test "$(field wayland-https page.title)" = "Example Domain"
  check "wayland: webkit://gpu loads" run wayland-gpu webkit://gpu
  check "wayland: it names a GL renderer" known wayland-gpu page.gpu.GL_RENDERER
fi
echo "$failures failed; reports, logs and screenshots in $out"
[[ $failures == 0 ]]
