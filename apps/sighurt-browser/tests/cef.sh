#!/usr/bin/env bash
# sig with the CEF engine, end to end, in a private headless sway on a private D-Bus session:
# example.com and a local input page open in Chromium (`page.open-in cef`), the page is
# shown, typing reaches a field and not the keymap, a paste goes through the clipboard service,
# and an idle page costs nothing. Screenshots and logs in OUT. Builds sig with the cef and
# demo engines (no Servo, no WPE). Never touches the desktop or the user's configuration: every
# XDG directory is under OUT.
#
#   apps/sighurt-browser/tests/cef.sh [OUT]       (default /data/sig-builds/test-out/cef/e2e)
set -uo pipefail
cd "$(dirname "$0")/../../.."
export SIGHURT_CEF_PREFIX=${SIGHURT_CEF_PREFIX:-/data/sig-builds/deps/cef-150.0.22}
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/data/sig-builds/targets/cef}
if [[ ${1:-} != --inside ]]; then
  out=${1:-/data/sig-builds/test-out/cef/e2e}
  SIG_WHAT="cef: sig build for cef.sh" tools/cargo build -p sighurt-browser \
    --no-default-features --features engine-cef,engine-demo --bins --example virtual-pointer || exit 1
  SIG_WHAT="cef.sh" SIG_MEM=8G tools/heavy tools/headless-sway --renderer pixman -- \
    engines/sighurt-engine-wpe/tests/private-bus "$0" --inside "$out"
  exit $?
fi
out=$2
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "cef.sh: run me through tools/headless-sway" >&2
  exit 2
}
rm -rf "$out"
mkdir -p "$out/config/sighurt" "$out/state" "$out/data" "$out/cache" "$out/downloads" "$out/guests"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_DATA_HOME=$out/data
export XDG_CACHE_HOME=$out/cache XDG_DOWNLOAD_DIR=$out/downloads
export SIG_DIAG=1 RUST_LOG=${RUST_LOG:-warn}
export PATH="$(nix-build nix/nixpkgs.nix -A wl-clipboard --no-out-link 2>/dev/null)/bin:$PATH"
sig=$CARGO_TARGET_DIR/debug/sig
vptr=$CARGO_TARGET_DIR/debug/examples/virtual-pointer
probe=apps/sighurt-browser/tests/probe.py
session=$out/state/sighurt/session.toml
python3 apps/sighurt-browser/tests/serve.py apps/sighurt-browser/tests/pages "$out/guests" "$out/port" \
  "$out/server.log" &
server_pid=$!
for _ in $(seq 50); do [[ -s $out/port ]] && break; sleep 0.1; done
base=http://127.0.0.1:$(cat "$out/port")
cat > "$out/config/sighurt/config.toml" <<CONF
version = 1
[downloads]
directory = "$out/downloads"
# The keys below are the bindings keymap's; the default keymap (example) ignores [keys], and
# binds neither ctrl-l nor h in the window.
[input]
keymap = "bindings"
[keys]
"ctrl-t" = "tab.new"
"ctrl-l" = "location.focus"
"ctrl-q" = "browser.quit"
"h" = "tab.new"
CONF
failures=0
check() {
  if [[ $2 == 0* ]]; then echo "PASS cef: $1 (${2:2})"; else echo "FAIL cef: $1 (${2:2})"; failures=$((failures + 1)); fi
}
info() { echo "INFO cef: $*"; }
shot() { sleep "${2:-0.5}"; grim "$out/$1.png"; }
wtype -s 3600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
"$vptr" < "$XDG_RUNTIME_DIR/pointer" &
pointer_pid=$!
exec 3> "$XDG_RUNTIME_DIR/pointer"
trap 'kill "$keyboard" "$pointer_pid" "$server_pid" ${sig_pid:-} 2>/dev/null' EXIT
layout_w=1280 layout_h=800
chrome_h=84
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
click() { point "$1" "$2"; sleep 0.3; echo click >&3; sleep 0.2; }
key() { wtype "$@"; sleep 0.25; }
open() { key -M ctrl l -m ctrl; key "$1"; key -k Return; }
until_ok() {
  local end=$((SECONDS + $1)) reply
  shift
  while :; do
    reply=$(python3 "$probe" "$@" 2>&1)
    [[ $reply == 0* || $SECONDS -ge $end ]] && break
    sleep 0.25
  done
  echo "$reply"
}
sess() { until_ok "$1" session "$session" "$2"; }
dg() { until_ok "$1" diag "$sig_pid" "$out/diag.json" "$2"; }
value() { local r; r=$(python3 "$probe" diag "$sig_pid" "$out/diag.json" "True, $1"); echo "${r:2}"; }
# The tree's resident set (MiB) and CPU seconds, over sig and every descendant.
tree() {
  local pids=("$sig_pid") i=0
  while ((i < ${#pids[@]})); do
    mapfile -t kids < <(pgrep -P "${pids[i]}")
    pids+=("${kids[@]}")
    i=$((i + 1))
  done
  local rss=0 cpu=0
  for p in "${pids[@]}"; do
    rss=$((rss + $(awk '/^VmRSS/ {print $2}' "/proc/$p/status" 2>/dev/null || echo 0)))
    cpu=$((cpu + $(awk '{print $14 + $15}' "/proc/$p/stat" 2>/dev/null || echo 0)))
  done
  echo "${#pids[@]} $((rss / 1024)) $cpu"
}

"$sig" --gui --diagnostics "$out/diag.json" >> "$out/sig.log" 2>&1 &
sig_pid=$!
sleep 3
open ":page.open-in cef https://example.com/"
check "example.com opens in Chromium" \
  "$(sess 40 "url(1)=='https://example.com/' and segs(1)[-1][0]=='cef', (segs(1), url(1))")"
check "its title arrives" "$(sess 10 "seg(1).get('title')=='Example Domain', seg(1).get('title')")"
check "its view is the one shown" \
  "$(dg 10 "shown is not None and shown['engine']=='cef', shown and shown['engine']")"
shot 01-example 1
info "screenshot $out/01-example.png"
open ":page.open-in cef $base/cef-input.html"
check "the local input page opens in Chromium" \
  "$(sess 25 "url(1)=='$base/cef-input.html' and segs(1)[-1][0]=='cef', (segs(1), url(1))")"
sleep 1
click 100 $((chrome_h + 35))
sleep 0.5
key -k h
key -k e
key -k l
key -k l
key -k o
served() { grep -c " GET $1\$" "$out/server.log"; }
waitfor() { for _ in $(seq 40); do [[ $(served "$1") -ge 1 ]] && break; sleep 0.25; done; [[ $(served "$1") -ge 1 ]]; echo "$? the page reported $1: $(served "$1") time(s)"; }
check "typed letters reach the field" "$(waitfor /typed/hello)"
check "and the keymap's plain-letter binding did not fire (one tab)" \
  "$(sess 3 "len(s['tabs'])==1, len(s['tabs'])")"
printf 'PASTED' | wl-copy
sleep 1
key -M ctrl v -m ctrl
check "Ctrl+V pastes the desktop clipboard through the service" "$(waitfor /typed/helloPASTED)"
key -M ctrl a -m ctrl
sleep 0.5
key -M ctrl c -m ctrl
sleep 1
copied=$(wl-paste -n 2>/dev/null)
check "Ctrl+C puts the selection on the desktop clipboard through the service" \
  "$([[ $copied == helloPASTED ]]; echo "$? clipboard: '$copied'")"
shot 02-typed 0.5
printf 'downloaded by chromium' > "$out/guests/dl.txt"
open ":page.open-in cef $base/attachment/dl.txt"
for _ in $(seq 60); do [[ -s $out/downloads/dl.txt ]] && break; sleep 0.25; done
shot 03-attachment 0.2
check "an attachment goes to the browser's download service, whole" \
  "$([[ $(cat "$out/downloads/dl.txt" 2>/dev/null) == 'downloaded by chromium' ]]; echo "$? $(ls "$out/downloads" | tr '\n' ' ')")"
check "Ctrl+L took the keyboard from the page's field: none of the command reached it" \
  "$([[ $(served '/typed/%3A.*') == 0 ]]; echo "$? the page reported $(served '/typed/%3A.*') typed command prefix(es)")"
info "screenshot $out/02-typed.png"
# Idle: after load, nothing wakes sig and Chromium's tree does not burn CPU.
sleep 3
before=$(tree)
w0=$(value "c['wakeups']")
sleep 10
after=$(tree)
w1=$(value "c['wakeups']")
read -r n rss cpu0 <<< "$before"
read -r _ _ cpu1 <<< "$after"
hz=$(getconf CLK_TCK)
info "tree: $n processes, $rss MiB resident; idle CPU over 10 s: $(python3 -c "print(f'{($cpu1 - $cpu0) / $hz / 10 * 100:.1f}')") % of one core; sig wakeups in 10 s: $((w1 - w0))"
check "sig is woken at most a few times in 10 s of idle" "$(python3 -c "print('0' if $w1 - $w0 <= 25 else '1', $w1 - $w0)")"
kill -TERM "$sig_pid" 2>/dev/null
for _ in $(seq 50); do kill -0 "$sig_pid" 2>/dev/null || break; sleep 0.2; done
# 3s after exit: an on-device model utility used to be reparented to init and still be alive.
sleep 3
check "sig exits and no Chromium process is left" \
  "$([[ -z $(pgrep -f '^/[^ ]*/sig-cef-helper --type=') ]]; echo "$? left=$(pgrep -fc '^/[^ ]*/sig-cef-helper --type=')")"
echo "cef.sh: $failures failed check(s); screenshots and logs in $out"
exit $failures
