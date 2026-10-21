#!/usr/bin/env bash
# inner.sh NAME URL [VAR=VALUE...]: inside tools/headless-sway + private-bus: runs sig --engine wpe
# on URL with a throwaway XDG profile and the given environment, waits, screenshots, kills it.
# Output: out/NAME.png, out/NAME.log (sig's stderr).
set -u
name=$1 url=$2; shift 2
here=$(cd "$(dirname "$0")" && pwd)
[[ $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || { echo "not in headless-sway" >&2; exit 1; }
prof=$(mktemp -d "$XDG_RUNTIME_DIR/prof.XXXX")
export XDG_CONFIG_HOME=$prof/config XDG_STATE_HOME=$prof/state XDG_DATA_HOME=$prof/data
export XDG_CACHE_HOME=$prof/cache XDG_DOWNLOAD_DIR=$prof/dl RUST_LOG=${RUST_LOG:-warn}
for kv in "$@"; do export "$kv"; done
"${SIG:-$here/../../target/try/sig}" --engine wpe "$url" > "$here/out/$name.log" 2>&1 &
pid=$!
sleep "${WAIT:-14}"
if [[ ${SCROLL:-0} == 1 ]]; then
  wlrctl pointer move 200 200
  for _ in 1 2 3 4 5 6 7 8; do
    wlrctl pointer scroll 200 0
    sleep 0.25
  done
  sleep 2
fi
grim "$here/out/$name.png"
pstree -p $pid > "$here/out/$name.tree" 2>&1
for p in $(pgrep -f "WPE(Web|GPU|Network)Process" || true); do tr '\0' '\n' < /proc/$p/environ | grep -E "^(JSC_|WEBKIT_|GST_|AT_SPI|DBUS_SESSION)" > "$here/out/$name.env.$p" || true; done
kill $pid; wait $pid 2>/dev/null
echo "$name done: $(wc -l < "$here/out/$name.log") log lines"
