# Helpers soak.sh and faults.sh share, sourced inside tools/headless-sway (they type, so never
# on the desktop). The sourcing script sets `out` (this scenario's directory), `scenario` and
# `sig` (the binary). Every `sig` started here gets XDG directories of its own under a
# directory the caller names, so no run reads or writes the user's.
#
#   check NAME STATUS DETAIL   PASS when STATUS is 0, FAIL otherwise; counts failures
#   verdict NAME "0|1 detail"  check with probe.py's answer
#   private DIR                XDG_CONFIG_HOME, _STATE_HOME, _DATA_HOME, _CACHE_HOME and
#                              XDG_DOWNLOAD_DIR under DIR, and the test configuration in it
#   start_sig DIR ARGS...      sig with DIR's directories (through `launch`, if set), its log
#                              DIR/sig.log, its diagnostics DIR/diag.json; sets sig_pid
#   dg EXPR                    probe.py diag on the running sig (names in probe.py)
#   ses DIR EXPR               probe.py session on DIR's session file
#   exited PID SECONDS         waits up to SECONDS for PID to exit; sets `status` to its exit
#                              status, or to "timeout" (then kills it with SIGKILL)
#   proc PID                   RSS and VmHWM in KiB, open descriptors and threads
#   key ARGS / cmd COMMAND     wtype; a command typed into the address line (":COMMAND")

tests=$repo/apps/sighurt-browser/tests
failures=0
check() {
  if [[ $2 == 0 ]]; then echo "PASS $scenario: $1 ($3)"; else echo "FAIL $scenario: $1 ($3)"; failures=$((failures + 1)); fi
}
verdict() { check "$1" "${2%% *}" "${2#* }"; }
step() { echo "[$(date +%T.%3N)] $*" >> "$out/steps.log"; }
shot() { sleep "${2:-0.5}"; grim "$out/$1.png"; step "screenshot $1"; }
key() { wtype "$@"; sleep 0.25; }
cmd() { key -M ctrl l -m ctrl; key ":$1"; key -k Return; }

# The configuration every run starts from: the demo engine for demo: addresses, the Wasm engine
# for .wasm files, and keys for what the tests do.
CONFIG='version = 1
[engines]
default = "demo"
[routing.demo]
schemes = ["demo"]
[input]
keymap = "bindings"
[keys]
"ctrl-t" = "tab.new"
"ctrl-w" = "tab.close"
"ctrl-tab" = "tab.next"
"ctrl-shift-tab" = "tab.previous"
"alt-1" = "tab.select 1"
"alt-2" = "tab.select 2"
"alt-3" = "tab.select 3"
"ctrl-l" = "location.focus"
"f5" = "page.reload"
"ctrl-q" = "browser.quit"'

private() {
  local dir=$1
  mkdir -p "$dir/config/sighurt" "$dir/state" "$dir/data" "$dir/cache" "$dir/downloads"
  export XDG_CONFIG_HOME=$dir/config XDG_STATE_HOME=$dir/state XDG_DATA_HOME=$dir/data \
    XDG_CACHE_HOME=$dir/cache XDG_DOWNLOAD_DIR=$dir/downloads
  [[ -e $dir/config/sighurt/config.toml ]] || printf '%s\n' "$CONFIG" > "$dir/config/sighurt/config.toml"
}

# launch: a command start_sig runs sig through (env -u NAME, ...); empty by default.
sig_pid= sig_dir= launch=()
start_sig() {
  sig_dir=$1
  shift
  "${launch[@]}" "$sig" --diagnostics "$sig_dir/diag.json" "$@" > "$sig_dir/sig.log" 2>&1 &
  sig_pid=$!
  sleep 2
}
dg() { python3 "$tests/probe.py" diag "$sig_pid" "$sig_dir/diag.json" "$1" 2>&1; }
ses() { python3 "$tests/probe.py" session "$1/state/sighurt/session.toml" "$2" 2>&1; }

# Not in a $(...): wait needs the shell that started PID.
exited() {
  local pid=$1 end=$((SECONDS + $2))
  while kill -0 "$pid" 2>/dev/null; do
    if [[ $SECONDS -ge $end ]]; then
      kill -KILL "$pid" 2>/dev/null
      wait "$pid" 2>/dev/null
      status=timeout
      return
    fi
    sleep 0.05
  done
  wait "$pid"
  status=$?
}

proc() {
  local status=/proc/$1/status
  [[ -r $status ]] || { echo "0 0 0 0"; return; }
  echo "$(awk '/^VmRSS/{r=$2} /^VmHWM/{h=$2} /^Threads/{t=$2} END{print r, h}' "$status") \
$(ls "/proc/$1/fd" 2>/dev/null | wc -l) $(awk '/^Threads/{print $2}' "$status")"
}
