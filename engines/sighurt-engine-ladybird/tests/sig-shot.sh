#!/usr/bin/env bash
# sig with the Ladybird engine, in a private headless sway (never the desktop): opens a page in
# Ladybird, takes a screenshot of the private compositor and checks sig's own log.
#
#   tools/heavy tools/headless-sway -- engines/sighurt-engine-ladybird/tests/sig-shot.sh OUT [URL]
#
# Build first: tools/cargo build -p sighurt-browser --no-default-features --features engine-ladybird,engine-demo
# (CARGO_TARGET_DIR as you built it; SIG_BIN overrides the binary.) OUT gets shot.png and sig.log.
set -u
out=$(realpath -m "${1:?usage: sig-shot.sh OUT [URL]}")
url=${2:-https://example.com/}
repo=$(cd "$(dirname "$0")/../../.." && pwd)
sig=${SIG_BIN:-${CARGO_TARGET_DIR:-$repo/target}/debug/sig}
mkdir -p "$out"
home=$(mktemp -d /tmp/sig-lb.XXXXXX)
export XDG_CONFIG_HOME=$home/config XDG_DATA_HOME=$home/data XDG_STATE_HOME=$home/state XDG_CACHE_HOME=$home/cache
export FONTCONFIG_FILE=${FONTCONFIG_FILE:-/etc/fonts/fonts.conf}
mkdir -p "$XDG_CONFIG_HOME/sighurt"
# MODE=engine: Ladybird is the default engine (sig --engine ladybird). MODE=route: the demo engine
# is, and [routing.ladybird] gives Ladybird the host of the URL.
host=$(sed -E 's|^[a-z]+://([^/:]+).*|\1|' <<<"$url")
if [[ ${MODE:-engine} == second ]]; then
  # A second Ladybird view: a page from the command line, a new tab (sig://home), then
  # page.open-in ladybird into that tab.
  cat > "$home/one.html" <<HTML
<!doctype html><title>first</title><body style="background:#cfe">first
HTML
  cat > "$home/two.html" <<HTML
<!doctype html><title>second</title><body style="background:#fec">second
HTML
  cat > "$XDG_CONFIG_HOME/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "ladybird"
enabled = ["ladybird", "demo"]
[keys]
"ctrl-t" = "tab.new"
"ctrl-o" = "page.open-in ladybird file://$home/two.html"
CONF
  engine_args=(--engine ladybird)
  url_to_open=file://$home/one.html
elif [[ ${MODE:-engine} == links ]]; then
  # A click on a link in a Ladybird page to an address the routing gives the demo engine: the
  # engine asks the browser (NavigationRequested), which opens it in the demo engine.
  cat > "$home/page.html" <<HTML
<!doctype html><title>links</title><body style="margin:0">
<a href="demo://pattern" style="position:absolute;left:0;top:0;width:600px;height:200px;display:block;background:#fc0">to the demo engine</a>
HTML
  cat > "$XDG_CONFIG_HOME/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "ladybird"
enabled = ["ladybird", "demo"]
[routing.demo]
schemes = ["demo"]
CONF
  engine_args=(--engine ladybird)
  url_to_open=file://$home/page.html
elif [[ ${MODE:-engine} == typing ]]; then
  # A plain-letter binding ("t" opens a tab) must not fire while a Ladybird text field has focus.
  cat > "$home/page.html" <<HTML
<!doctype html><title>typing</title><body style="margin:0">
<input id=i style="position:absolute;left:20px;top:20px;width:400px;height:40px;font-size:24px">
HTML
  cat > "$XDG_CONFIG_HOME/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "ladybird"
enabled = ["ladybird", "demo"]
[keys]
"t" = "tab.new"
CONF
  engine_args=(--engine ladybird)
  url_to_open=file://$home/page.html
elif [[ ${MODE:-engine} == open-in ]]; then
  # The demo engine's page first, then a key bound to page.open-in sends the URL to Ladybird.
  cat > "$XDG_CONFIG_HOME/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "demo"
enabled = ["demo", "ladybird"]
[routing.demo]
schemes = ["demo"]
[keys]
"ctrl-o" = "page.open-in ladybird $url"
CONF
  engine_args=()
  url_to_open=demo://pattern
elif [[ ${MODE:-engine} == route ]]; then
  cat > "$XDG_CONFIG_HOME/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "demo"
enabled = ["demo", "ladybird"]
[routing.ladybird]
hosts = ["$host"]
CONF
  engine_args=()
else
  cat > "$XDG_CONFIG_HOME/sighurt/config.toml" <<CONF
version = 1
[engines]
default = "ladybird"
enabled = ["ladybird", "demo"]
CONF
  engine_args=(--engine ladybird)
fi
failures=0
check() { if "${@:2}"; then echo "PASS $1"; else echo "FAIL $1"; failures=$((failures + 1)); fi }
SIG_DIAG=1 RUST_LOG=${RUST_LOG:-info} "$sig" --gui "${engine_args[@]}" "${url_to_open:-$url}" > "$out/sig.log" 2>&1 &
pid=$!
for _ in $(seq 1 ${SIG_SETTLE:-15}); do sleep 1; kill -0 "$pid" 2>/dev/null || break; done
if [[ ${MODE:-engine} == second ]]; then
  wtype -s 3600000 ' ' &
  keyboard=$!
  sleep 6
  grim "$out/first.png"
  wtype -M ctrl -k t -m ctrl
  sleep 2
  wtype -M ctrl -k o -m ctrl
  sleep 12
  kill $keyboard 2>/dev/null
elif [[ ${MODE:-engine} == links ]]; then
  pointer=${CARGO_TARGET_DIR:-$repo/target}/debug/examples/virtual-pointer
  mkfifo "$XDG_RUNTIME_DIR/pointer"
  "$pointer" < "$XDG_RUNTIME_DIR/pointer" &
  pointer_pid=$!
  exec 3> "$XDG_RUNTIME_DIR/pointer"
  sleep 6
  grim "$out/before.png"
  echo "move 100 150 1280 800" >&3
  sleep 0.5   # the link under the pointer is the page's status text when the button goes down
  echo click >&3
  sleep 4
  kill $pointer_pid 2>/dev/null
elif [[ ${MODE:-engine} == typing ]]; then
  wtype -s 3600000 ' ' &
  keyboard=$!
  # A virtual pointer that stays plugged in (wlrctl's events are lost on a new device).
  # Build it with: tools/cargo build -p sighurt-browser --example virtual-pointer
  pointer=${CARGO_TARGET_DIR:-$repo/target}/debug/examples/virtual-pointer
  mkfifo "$XDG_RUNTIME_DIR/pointer"
  "$pointer" < "$XDG_RUNTIME_DIR/pointer" &
  pointer_pid=$!
  exec 3> "$XDG_RUNTIME_DIR/pointer"
  sleep 5
  # The page area starts below the chrome (84 px); click into the field.
  echo "move 100 124 1280 800" >&3
  sleep 0.5
  echo click >&3
  sleep 1
  wtype "tttest"
  sleep 2
  kill $keyboard $pointer_pid 2>/dev/null
elif [[ ${MODE:-engine} == open-in ]]; then
  # wtype plugs in a new keyboard per command, and the first key of a new one is lost.
  wtype -s 3600000 ' ' &
  keyboard=$!
  sleep 3
  grim "$out/before.png"
  wtype -M ctrl -k o -m ctrl
  kill $keyboard 2>/dev/null
  sleep 12
fi
sleep "${SIG_WAIT:-3}"
grim "$out/shot.png"
check "sig still runs" kill -0 "$pid"
check "a Ladybird helper process exists" bash -c "pgrep -f 'libexec/(\\.)?WebContent' > /dev/null"
kill "$pid" 2>/dev/null
for _ in $(seq 1 20); do kill -0 "$pid" 2>/dev/null || break; sleep 0.5; done
check "sig exited" bash -c "! kill -0 $pid 2>/dev/null"
check "no Ladybird helper left" bash -c "! pgrep -f 'libexec/(\.)?WebContent' > /dev/null"
rm -rf "$home"
echo "screenshot: $out/shot.png"
exit $failures
