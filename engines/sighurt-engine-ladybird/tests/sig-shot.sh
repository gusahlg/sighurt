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
if [[ ${MODE:-engine} == open-in ]]; then
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
SIG_DIAG=1 RUST_LOG=info "$sig" --gui "${engine_args[@]}" "${url_to_open:-$url}" > "$out/sig.log" 2>&1 &
pid=$!
for _ in $(seq 1 60); do sleep 1; kill -0 "$pid" 2>/dev/null || break; done
if [[ ${MODE:-engine} == open-in ]]; then
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
