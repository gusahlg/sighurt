#!/usr/bin/env bash
# Fault injection: `sig` with the demo and Wasm engines under conditions it must survive, each
# case a run of its own with private XDG directories, in a private headless sway (never on the
# desktop, since it types). Builds `sig` (--no-default-features --features
# engine-demo,engine-wasm,frontend-tui,render-wgpu) with tools/cargo, then runs the scenarios
# below and prints PASS or FAIL for each check, with what was expected and what was found.
#
#   apps/sighurt-browser/tests/faults.sh OUT                  build, then every scenario
#   apps/sighurt-browser/tests/faults.sh --no-build OUT       with the binary already built
#   apps/sighurt-browser/tests/faults.sh OUT dirs signals     only these scenarios
#
# Scenarios, each in a sway of its own:
#   dirs        the state directory read-only, or a regular file: sig runs and says the tabs
#               won't be saved; history.clear there can't empty the history file, says so,
#               and leaves it whole; the configuration directory missing (the built-in configuration),
#               a regular file or a symlink loop (sig stops with status 2 before anything starts);
#               HOME unset (the XDG directories are enough); XDG_RUNTIME_DIR unset (no compositor:
#               status 1 and a message, at once)
#   lock        the session lock held by a live process: this sig restores the tabs, says it
#               won't save them, and never writes the file, nor the history file beside it
#               (history.clear forgets in this sig only, and says so) and leaves the owner's
#               temporary files alone; a lock file a dead sig left (and its temporary files): taken
#               over, the session's and the history's temporaries removed, the session saved
#   writes      file writes failing (ulimit -f, EFBIG): the session save and a download fail, sig
#               keeps running, says so in the status line, and the previous session file stays
#               whole; and its log on stderr failing too (a file that can't grow, a closed pipe)
#   session     a corrupted session file (kept aside, sig starts), a 50 MB one, one of 5000 tabs
#               (only session::MAX_TABS = 1000 restored)
#   config      an invalid configuration at startup (status 2, DID_NOT_START, no window, no
#               session lock) and one reloaded while sig runs (kept out, a notice)
#   compositor  sway killed (SIGKILL) while sig runs: sig exits within 5 s with status 1 and a
#               message, having saved the session, without spinning
#   signals     SIGTERM, SIGINT and SIGHUP while pages load: the session saved, status 0 within
#               5 s, no thread or child process left
#   terminal    sig://terminal ([pages.terminal] on, /bin/sh, a private home): 100 cycles of
#               opening the page, starting the shell and closing the tab, each shell started,
#               ended and reaped, and sig's descriptors and threads not grown; then the shell
#               ends with sig on ctrl-q, SIGKILL (the kernel's hang-up), SIGTERM, SIGINT, SIGHUP
#               and the compositor killed
#   crash       demo://crash failing while tabs switch and the window resizes, next to
#               demo://slow: the failure names its tab, the other tabs keep working, F5 recovers
#   flood       200 tab.new in about a second, then 1000 resizes: no placement errors, buffers
#               and memory bounded, sig still answers keys and quits within 5 s
#
# Exits with the number of failed checks. Logs, diagnostics and screenshots go to OUT.
set -u
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: faults.sh [--no-build] OUT [SCENARIO ...]}")
  shift
  scenarios=("$@")
  [[ ${#scenarios[@]} == 0 ]] && scenarios=(dirs lock writes session config compositor signals terminal crash flood)
  mkdir -p "$out"
  tdir=$(realpath -m "${CARGO_TARGET_DIR:-target}")
  link="ln -f $tdir/debug/sig $tdir/debug/sig-faults"
  if [[ $build == 1 ]]; then
    SIG_WHAT="build sig (demo, wasm) for faults.sh" tools/heavy bash -c \
      "cargo build -p sighurt-browser --no-default-features \
       --features engine-demo,engine-wasm,frontend-tui,render-wgpu --bins && $link" || exit 1
  else
    $link || exit 1
  fi
  failed=0
  for scenario in "${scenarios[@]}"; do
    tools/heavy tools/headless-sway -- "$0" --inside "$out/$scenario" "$scenario" "$tdir/debug/sig-faults"
    failed=$((failed + $?))
  done
  echo "faults.sh: $failed failed check(s); logs and screenshots in $out"
  exit $failed
fi

# ── Inside the private compositor ─────────────────────────────────────────────────────────
out=$2 scenario=$3 sig=$4
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "faults.sh: run me through tools/headless-sway" >&2
  exit 2
}
source "$repo/apps/sighurt-browser/tests/lib.sh"
rm -rf "$out"
mkdir -p "$out"
export RUST_LOG=${RUST_LOG:-info}

wtype -s 100000000 ' ' &
keyboard=$!
finish() {
  kill "$keyboard" 2>/dev/null
  wait "$keyboard" 2>/dev/null
  grep -l panicked "$out"/*/sig.log 2>/dev/null | while read -r log; do echo "panic in $log"; done | grep -q . &&
    check "no panic" 1 "$(grep -l panicked "$out"/*/sig.log | tr '\n' ' ')"
  cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null
  exit $failures
}
# windows: sig's windows in sway's tree.
windows() { swaymsg -t get_tree | grep -c '"app_id": "sighurt"'; }
# quit: ctrl-q, and the exit status within 5 s.
quit() { key -M ctrl q -m ctrl; exited "$sig_pid" 5; }
# history FILE: a history file of two visits, both of now.
history() {
  local now
  now=$(date +%s)
  printf 'version = 1\n[[visits]]\nurl = "https://www.example.com/"\nlast = %s\ncount = 1\n[[visits]]\nurl = "https://example.org/a"\nlast = %s\ncount = 2\n' \
    "$now" "$now" > "$1"
}
# session FILE TABS [ACTIVE]: a session of TABS demo tabs, each address padded to PAD bytes.
session() {
  python3 - "$@" <<'PY'
import sys
path, tabs = sys.argv[1], int(sys.argv[2])
pad = int(sys.argv[3]) if len(sys.argv) > 3 else 0
pages = ['pattern', 'animation', 'resize']
with open(path, 'w') as f:
    f.write('version = 1\nactive = 0\n')
    for i in range(tabs):
        url = f'demo://{pages[i % 3]}?{i}' + ('x' * pad if pad else '')
        f.write(f'\n[[tabs]]\ncurrent = 0\nzoom = 1.0\n\n[[tabs.segments]]\nengine = "demo"\n'
                f'entries = ["{url}"]\ncurrent = 0\ntitle = "Tab {i}"\n')
PY
}
alive() { kill -0 "$sig_pid" 2>/dev/null; }

case $scenario in
dirs)
  step "the state directory read-only"
  d=$out/state-readonly
  private "$d"
  mkdir -p "$d/state/sighurt"
  history "$d/state/sighurt/history.toml"
  sum=$(md5sum < "$d/state/sighurt/history.toml")
  chmod 500 "$d/state/sighurt"
  start_sig "$d" demo://pattern
  verdict "read-only state: sig runs and shows the page" "$(dg "len(views)==1 and shown is not None, len(views)")"
  verdict "and says the tabs won't be saved" "$(dg "d['status']=='notice', d['status']")"
  shot state-readonly
  cmd history.clear
  verdict "history.clear says the history file wasn't emptied" "$(dg "d['status']=='notice', d['status']")"
  shot state-readonly-history-clear
  check "and the history file is left whole" "$([[ $(md5sum < "$d/state/sighurt/history.toml") == "$sum" ]]; echo $?)" "md5 compared"
  check "sig still runs" "$(alive; echo $?)" "pid $sig_pid"
  quit; code=$status
  check "and quits cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  check "the log says why" "$(grep -q 'cannot save the session' "$d/sig.log"; echo $?)" \
    "$(grep -o 'cannot save the session.*' "$d/sig.log" | head -1)"
  chmod 700 "$d/state/sighurt"

  step "the state directory a regular file"
  d=$out/state-file
  private "$d"
  rm -rf "$d/state"
  echo "not a directory" > "$d/state"
  start_sig "$d" demo://pattern
  verdict "state a file: sig runs and shows the page" "$(dg "len(views)==1, len(views)")"
  verdict "and says the tabs won't be saved" "$(dg "d['status']=='notice', d['status']")"
  quit; code=$status
  check "and quits cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  check "the file is left as it was" "$([[ $(cat "$d/state") == "not a directory" ]]; echo $?)" "$(head -c 40 "$d/state")"

  step "the configuration directory missing"
  d=$out/config-missing
  private "$d"
  rm -rf "$d/config"
  start_sig "$d" demo://pattern
  # No configuration: the built-in one, which has no demo routing; the home page shows.
  verdict "no configuration directory: the built-in configuration" "$(dg "len(d['tabs'])==1 and d['status']!='notice', (len(d['tabs']), d['status'])")"
  kill -TERM "$sig_pid"
  exited "$sig_pid" 5; code=$status
  check "and SIGTERM quits it cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"

  for kind in file loop; do
    step "the configuration directory a $kind"
    d=$out/config-$kind
    private "$d"
    rm -rf "$d/config/sighurt"
    if [[ $kind == file ]]; then echo "x" > "$d/config/sighurt"; else ln -s sighurt "$d/config/sighurt"; fi
    before=$(windows)
    start_sig "$d" demo://pattern
    exited "$sig_pid" 5; code=$status
    check "config dir a $kind: sig stops with status 2" "$([[ $code == 2 ]]; echo $?)" "exit status $code"
    check "and says why, ending with DID_NOT_START" \
      "$(grep -q 'sig did not start. Fix the file' "$d/sig.log"; echo $?)" "$(grep -m1 -o 'cannot read.*' "$d/sig.log")"
    check "no window was made, no session lock taken" \
      "$([[ $(windows) == "$before" && ! -e $d/state/sighurt/session.lock ]]; echo $?)" "windows $(windows), lock $(ls "$d/state/sighurt" 2>/dev/null)"
  done

  step "HOME unset"
  d=$out/no-home
  private "$d"
  launch=(env -u HOME)
  start_sig "$d" demo://pattern
  launch=()
  verdict "HOME unset: the XDG directories are enough" "$(dg "len(views)==1 and d['status']!='notice', (len(views), d['status'])")"
  quit; code=$status
  check "and quits cleanly, saving the tab" "$([[ $code == 0 && -s $d/state/sighurt/session.toml ]]; echo $?)" "exit status $code"

  step "XDG_RUNTIME_DIR unset"
  d=$out/no-runtime-dir
  private "$d"
  launch=(env -u XDG_RUNTIME_DIR)
  start_sig "$d" demo://pattern
  launch=()
  exited "$sig_pid" 5; code=$status
  check "no XDG_RUNTIME_DIR: sig exits with status 1 at once" "$([[ $code == 1 ]]; echo $?)" "exit status $code"
  check "and says why" "$(grep -q '^sig: ' "$d/sig.log"; echo $?)" "$(grep -m1 '^sig: ' "$d/sig.log")"
  ;;

lock)
  d=$out/held
  private "$d"
  mkdir -p "$d/state/sighurt" && chmod 700 "$d/state/sighurt"
  session "$d/state/sighurt/session.toml" 2
  sum=$(md5sum < "$d/state/sighurt/session.toml")
  history "$d/state/sighurt/history.toml"
  hsum=$(md5sum < "$d/state/sighurt/history.toml")
  step "the lock held by a live process"
  flock "$d/state/sighurt/session.lock" sleep 300 &
  holder=$!
  # What the live owner may be writing at this moment: its temporary files are its own.
  echo 'version = 1' > "$d/state/sighurt/session.toml.$holder.tmp"
  echo 'version = 1' > "$d/state/sighurt/history.toml.$holder.tmp"
  sleep 0.3
  start_sig "$d"
  verdict "a held lock: the saved tabs are restored" "$(dg "len(d['tabs'])==2, len(d['tabs'])")"
  check "the owner's temporary files are left alone" "$([[ -e $d/state/sighurt/session.toml.$holder.tmp && -e $d/state/sighurt/history.toml.$holder.tmp ]]; echo $?)" "$(ls "$d/state/sighurt" | tr '\n' ' ')"
  verdict "and sig says it won't save them" "$(dg "d['status']=='notice', d['status']")"
  key -M ctrl t -m ctrl
  sleep 1.5
  cmd history.clear
  verdict "history.clear says it forgot in this sig only" "$(dg "d['status']=='notice', d['status']")"
  shot held-history-clear
  quit; code=$status
  check "it quits cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  check "and never wrote the session" "$([[ $(md5sum < "$d/state/sighurt/session.toml") == "$sum" ]]; echo $?)" "md5 compared"
  check "nor the history file" "$([[ $(md5sum < "$d/state/sighurt/history.toml") == "$hsum" ]]; echo $?)" "md5 compared"
  kill "$holder" 2>/dev/null
  wait "$holder" 2>/dev/null

  d=$out/stale
  private "$d"
  mkdir -p "$d/state/sighurt" && chmod 700 "$d/state/sighurt"
  session "$d/state/sighurt/session.toml" 2
  # What a sig that died left: its lock file (an flock is the OS's, gone with the process) and a
  # temporary file of a pid that no longer runs.
  echo 999999 > "$d/state/sighurt/session.lock"
  echo 'version = 1' > "$d/state/sighurt/session.toml.999999.tmp"
  echo 'version = 1' > "$d/state/sighurt/history.toml.999999.tmp"
  step "a lock and temporary files a dead sig left"
  start_sig "$d"
  verdict "a stale lock: the tabs are restored, no notice" "$(dg "len(d['tabs'])==2 and d['status']!='notice', (len(d['tabs']), d['status'])")"
  check "the session's temporary file is removed" "$([[ ! -e $d/state/sighurt/session.toml.999999.tmp ]]; echo $?)" "$(ls "$d/state/sighurt" | tr '\n' ' ')"
  check "and the history's" "$([[ ! -e $d/state/sighurt/history.toml.999999.tmp ]]; echo $?)" "$(ls "$d/state/sighurt" | tr '\n' ' ')"
  key -M ctrl t -m ctrl
  quit; code=$status
  check "it quits cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  verdict "and saved the session (three tabs)" "$(ses "$d" "len(s['tabs'])==3, len(s['tabs'])")"
  ;;

writes)
  # A Wasm guest that downloads 4 KiB.
  mkdir -p "$out/guests"
  python3 - "$out/guests/big-download.wasm" <<'PY'
import sys
data = 'x' * 4096
open(sys.argv[1], 'w').write(f'''(module
  (import "oxide" "api_download_data" (func $save (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{data}")
  (data (i32.const 8192) "big.txt")
  (func (export "start_app")
    (drop (call $save (i32.const 0) (i32.const 4096) (i32.const 8192) (i32.const 7)))))
''')
PY
  d=$out/efbig
  private "$d"
  mkdir -p "$d/state/sighurt" && chmod 700 "$d/state/sighurt"
  # A previous session of 40 tabs with 2 KiB addresses (about 85 KiB), over the limit below.
  session "$d/state/sighurt/session.toml" 40 2048
  sum=$(md5sum < "$d/state/sighurt/session.toml")
  step "writes over 64 KiB fail (ulimit -f 64); sig's output goes to a pipe"
  # The diagnostics stay under the limit; the session does not.
  sig_dir=$d
  ( ulimit -f 64; exec "$sig" --diagnostics "$d/diag.json" ) > >(cat > "$d/sig.log") 2>&1 &
  sig_pid=$!
  sleep 2.5
  verdict "the 40 saved tabs are restored" "$(dg "len(d['tabs'])==40, len(d['tabs'])")"
  key -M ctrl t -m ctrl
  sleep 2
  check "sig is still running after a save failed" "$(alive; echo $?)" "$(grep -m1 -o 'cannot save the session.*' "$d/sig.log")"
  verdict "the status line says the tabs could not be saved" "$(dg "d['status']=='notice', d['status']")"
  shot efbig-save-failed
  check "the previous session file is intact" "$([[ $(md5sum < "$d/state/sighurt/session.toml") == "$sum" ]]; echo $?)" "md5 compared"
  check "no temporary file is left" "$([[ -z $(ls "$d/state/sighurt" | grep '\.tmp$') ]]; echo $?)" "$(ls "$d/state/sighurt" | tr '\n' ' ')"
  step "a 4 KiB download under ulimit -f 1 (the limit is per process; a new run)"
  kill -TERM "$sig_pid"
  exited "$sig_pid" 5; code=$status
  check "SIGTERM still quits it cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  check "and the previous session file is still intact" "$([[ $(md5sum < "$d/state/sighurt/session.toml") == "$sum" ]]; echo $?)" "md5 compared"

  d=$out/efbig-1
  private "$d"
  mkdir -p "$d/state/sighurt" && chmod 700 "$d/state/sighurt"
  session "$d/state/sighurt/session.toml" 30
  sum=$(md5sum < "$d/state/sighurt/session.toml")
  sig_dir=$d
  ( ulimit -f 1; exec "$sig" "file://$out/guests/big-download.wasm" ) > >(cat > "$d/sig.log") 2>&1 &
  sig_pid=$!
  sleep 3
  check "ulimit -f 1: sig is running after the download and the save failed" "$(alive; echo $?)" "$(grep -o 'cannot save the session.*\|download.*fail.*' "$d/sig.log" | head -2 | tr '\n' ' ')"
  check "the download left nothing behind" "$([[ -z $(ls "$d/downloads") ]]; echo $?)" "downloads: $(ls -l "$d/downloads" | tail -n +2 | tr '\n' ' ')"
  check "the previous session file is intact" "$([[ $(md5sum < "$d/state/sighurt/session.toml") == "$sum" ]]; echo $?)" "md5 compared"
  kill -TERM "$sig_pid" 2>/dev/null
  exited "$sig_pid" 5; code=$status
  check "SIGTERM quits it cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"

  d=$out/log-efbig
  private "$d"
  step "stderr to a file that can't grow (ulimit -f 1, SIG_DIAG=1: a line per frame)"
  sig_dir=$d
  ( ulimit -f 1; SIG_DIAG=1 exec "$sig" demo://animation > "$d/sig.log" 2>&1 )  &
  sig_pid=$!
  sleep 3
  check "a log that can't be written doesn't stop sig" "$(alive; echo $?)" "$(tail -c 300 "$d/sig.log" | tr '\n' ' ')"
  kill -TERM "$sig_pid" 2>/dev/null
  exited "$sig_pid" 5; code=$status
  check "SIGTERM quits it cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  : > "$d/sig.log"

  d=$out/log-epipe
  private "$d"
  step "stderr a pipe whose reader has gone (SIG_DIAG=1)"
  sig_dir=$d
  SIG_DIAG=1 "$sig" demo://animation 2> >(head -c 300 > "$d/head.txt") > "$d/sig.log" &
  sig_pid=$!
  sleep 3
  check "a closed stderr doesn't stop sig" "$(alive; echo $?)" "pid $sig_pid"
  kill -TERM "$sig_pid" 2>/dev/null
  exited "$sig_pid" 5; code=$status
  check "SIGTERM quits it cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  ;;

session)
  d=$out/corrupt
  private "$d"
  mkdir -p "$d/state/sighurt" && chmod 700 "$d/state/sighurt"
  head -c 3000 /dev/urandom > "$d/state/sighurt/session.toml"
  start_sig "$d" demo://pattern
  verdict "a corrupted session: sig starts with the page asked for" "$(dg "len(d['tabs'])==1 and len(views)==1, len(d['tabs'])")"
  verdict "and says the saved tabs can't be restored" "$(dg "d['status']=='notice', d['status']")"
  check "the broken file is kept aside" "$(ls "$d/state/sighurt" | grep -q '^session.toml.broken-'; echo $?)" "$(ls "$d/state/sighurt" | tr '\n' ' ')"
  quit; code=$status
  check "it quits cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"

  d=$out/oversized
  private "$d"
  mkdir -p "$d/state/sighurt" && chmod 700 "$d/state/sighurt"
  # 50 MB: 25 tabs whose addresses are 2 MiB less a little (core::tabs::MAX_URL is 2 MiB).
  session "$d/state/sighurt/session.toml" 25 $((2 * 1024 * 1024 - 64))
  step "a 50 MB session"
  t0=$SECONDS
  start_sig "$d"
  verdict "a 50 MB session: sig starts" "$(dg "len(d['tabs'])>=1, (len(d['tabs']), d['status'])")"
  read -r rss hwm fds threads <<< "$(proc "$sig_pid")"
  check "and its memory stays bounded (VmHWM under 1.5 GiB)" "$([[ $hwm -lt 1572864 ]]; echo $?)" "VmRSS $((rss / 1024)) MiB, VmHWM $((hwm / 1024)) MiB"
  quit; code=$status
  check "it quits within 5 s, cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code after $((SECONDS - t0)) s in all"

  d=$out/many
  private "$d"
  mkdir -p "$d/state/sighurt" && chmod 700 "$d/state/sighurt"
  session "$d/state/sighurt/session.toml" 5000
  step "a session of 5000 tabs"
  start_sig "$d"
  sleep 3
  verdict "5000 saved tabs: the first 1000 are restored" "$(dg "len(d['tabs'])==1000, len(d['tabs'])")"
  verdict "and sig says so" "$(dg "d['status']=='notice', d['status']")"
  verdict "only the shown tab has an engine view" "$(dg "len(views)<=2, len(views)")"
  read -r rss hwm fds threads <<< "$(proc "$sig_pid")"
  check "memory stays bounded (VmHWM under 1 GiB)" "$([[ $hwm -lt 1048576 ]]; echo $?)" "VmRSS $((rss / 1024)) MiB, VmHWM $((hwm / 1024)) MiB, $fds fds, $threads threads"
  key -M ctrl t -m ctrl
  verdict "tab.new with 1000 tabs open adds none, and says so" "$(dg "len(d['tabs'])==1000 and d['status']=='notice', (len(d['tabs']), d['status'])")"
  quit; code=$status
  check "it quits within 5 s, cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  verdict "and saves 1000 tabs" "$(ses "$d" "len(s['tabs'])==1000, len(s['tabs'])")"
  ;;

config)
  d=$out/invalid-start
  private "$d"
  printf 'version = 1\n[ui]\nchrome = 7\n' > "$d/config/sighurt/config.toml"
  before=$(windows)
  start_sig "$d" demo://pattern
  exited "$sig_pid" 5; code=$status
  check "an invalid configuration at startup: status 2" "$([[ $code == 2 ]]; echo $?)" "exit status $code"
  check "the message ends with DID_NOT_START" \
    "$([[ $(tail -1 "$d/sig.log") == *"sig did not start. Fix the file, or move it away to start with the built-in configuration." ]]; echo $?)" "$(tail -1 "$d/sig.log")"
  check "it names the file" "$(grep -q "$d/config/sighurt/config.toml" "$d/sig.log"; echo $?)" "$(head -1 "$d/sig.log")"
  check "nothing started: no window, no session lock, no session" \
    "$([[ $(windows) == "$before" && ! -e $d/state/sighurt/session.lock && ! -e $d/state/sighurt/session.toml ]]; echo $?)" \
    "windows $(windows); state: $(ls "$d/state/sighurt" 2>/dev/null | tr '\n' ' ')"

  d=$out/invalid-reload
  private "$d"
  config=$d/config/sighurt/config.toml
  cp "$config" "$d/valid.toml"
  start_sig "$d" demo://pattern
  printf 'version = 1\n[keys]\n"ctrl-t" = "tab.nope"\n' > "$config"
  sleep 0.7
  verdict "an invalid reload: a notice" "$(dg "d['status']=='notice' and c['config_reloads']==1, (d['status'], c['config_reloads'])")"
  key -M ctrl t -m ctrl
  verdict "and the configuration in use stays (ctrl-t still opens a tab)" "$(dg "len(d['tabs'])==2, len(d['tabs'])")"
  rm "$config"
  sleep 0.7
  key -M ctrl t -m ctrl
  verdict "a deleted file keeps it too" "$(dg "len(d['tabs'])==3 and c['config_reloads']==2, (len(d['tabs']), c['config_reloads'])")"
  cp "$d/valid.toml" "$config"
  sleep 0.7
  quit; code=$status
  check "the valid file back: ctrl-q quits" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  ;;

compositor)
  d=$out/killed
  private "$d"
  start_sig "$d" demo://animation demo://pattern
  key -M ctrl t -m ctrl
  sleep 1.5
  step "kill -KILL sway"
  kill -KILL "$SIG_SWAY_PID"
  t0=$(date +%s.%N)
  exited "$sig_pid" 5; code=$status
  took=$(python3 -c "import time; print(f'{time.time() - $t0:.2f}')")
  check "sway killed: sig exits within 5 s with status 1" "$([[ $code == 1 ]]; echo $?)" "exit status $code after $took s"
  check "and says why" "$(grep -q 'Wayland connection is gone' "$d/sig.log"; echo $?)" "$(grep -m1 -o 'the Wayland connection is gone.*' "$d/sig.log")"
  verdict "the session was saved, three tabs" "$(ses "$d" "len(s['tabs'])==3, len(s['tabs'])")"
  exit_line=$(python3 -c "import json; d=json.load(open('$d/diag.json')); print(d['exit'], d['counters']['iterations'])")
  check "diagnostics written at exit, no spin" "$([[ ${exit_line%% *} == True ]]; echo $?)" "exit, loop iterations: $exit_line"
  ;;

signals)
  for signal in TERM INT HUP; do
    d=$out/sig$signal
    private "$d"
    step "SIG$signal while pages load"
    start_sig "$d" demo://slow "file://$repo/examples/does-not-exist.wasm" demo://animation
    key -M ctrl l -m ctrl
    key demo://slow?again
    wtype -k Return
    children=$(pgrep -P "$sig_pid" | tr '\n' ' ')
    kill "-$signal" "$sig_pid"
    exited "$sig_pid" 5; code=$status
    check "SIG$signal: status 0 within 5 s" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
    verdict "SIG$signal: the session was saved" "$(ses "$d" "len(s['tabs'])==3 and url(1)=='demo://slow?again', (len(s['tabs']), url(1))")"
    left=$(for p in $children; do kill -0 "$p" 2>/dev/null && echo "$p"; done)
    check "SIG$signal: no process left" "$([[ ! -e /proc/$sig_pid && -z $left ]]; echo $?)" "children before: ${children:-none}; left: ${left:-none}"
  done
  ;;

terminal)
  # sig://terminal's program ends with sig however sig ends, and 100 times opening the page,
  # starting the shell and closing the tab leave no program, no zombie and nothing grown. The
  # programs carry a marker in their environment; a home of their own per run.
  term_config() {
    printf '%s\n[pages.terminal]\nenabled = true\ncommand = "/bin/sh"\n' "$CONFIG" > "$1/config/sighurt/config.toml"
  }
  start_term() {
    local d=$1
    shift
    private "$d"
    term_config "$d"
    mkdir -p "$d/home"
    launch=(env "HOME=$d/home" "PAGETERM_MARK=$d" ENV=/dev/null)
    start_sig "$d" "$@"
    launch=()
  }
  # programs DIR: "LIVE ZOMBIES", DIR's marked processes alive and sig's zombie children.
  programs() {
    python3 - "$1" "$sig_pid" <<'PY'
import os, sys
mark, sig = ('PAGETERM_MARK=' + sys.argv[1]).encode(), sys.argv[2]
live = zombies = 0
for p in filter(str.isdigit, os.listdir('/proc')):
    try:
        state, ppid = open(f'/proc/{p}/stat').read().rsplit(')', 1)[1].split()[:2]
        env = open(f'/proc/{p}/environ', 'rb').read().split(b'\0') if state != 'Z' else []
    except OSError:
        continue
    zombies += state == 'Z' and ppid == sig
    live += p != sig and mark in env
print(live, zombies)
PY
  }
  until_programs() { # until_programs DIR "LIVE ZOMBIES" [SECONDS]
    for _ in $(seq $((${3:-5} * 10))); do
      [[ $(programs "$1") == "$2" ]] && return 0
      sleep 0.1
    done
    return 1
  }

  d=$out/cycles
  start_term "$d" sig://terminal
  key -k Return
  check "a shell starts" "$(until_programs "$d" '1 0'; echo $?)" "programs $(programs "$d")"
  step "100 cycles"
  read -r _ _ fds0 threads0 <<< "$(proc "$sig_pid")"
  missed=0
  for i in $(seq 100); do
    cmd 'tab.new sig://terminal'
    sleep 0.5
    key -k Return
    until_programs "$d" '2 0' 10 || { missed=$((missed + 1)); step "cycle $i: no second shell ($(programs "$d"))"; }
    key -M ctrl w -m ctrl
    until_programs "$d" '1 0' 10 || { missed=$((missed + 1)); step "cycle $i: not ended ($(programs "$d"))"; }
    # The first 10 cycles warm up (fonts, the compiled guest).
    [[ $i == 10 ]] && read -r _ _ fds0 threads0 <<< "$(proc "$sig_pid")"
  done
  read -r rss hwm fds threads <<< "$(proc "$sig_pid")"
  check "100 cycles of open, start and close: each shell started and ended, reaped" \
    "$([[ $missed == 0 && $(programs "$d") == '1 0' ]]; echo $?)" "$missed missed; programs $(programs "$d")"
  check "and nothing grew from cycle 10 to 100" "$([[ $fds -le $fds0 && $threads -le $threads0 ]]; echo $?)" \
    "descriptors $fds0 -> $fds, threads $threads0 -> $threads, VmRSS $rss kB, VmHWM $hwm kB"
  quit; code=$status
  check "ctrl-q quits and the shell ends" "$([[ $code == 0 ]] && until_programs "$d" '0 0'; echo $?)" \
    "exit status $code, programs $(programs "$d")"

  # Last: killing the compositor ends this sway.
  for how in KILL TERM INT HUP compositor; do
    d=$out/$how
    start_term "$d" sig://terminal
    key -k Return
    until_programs "$d" '1 0'
    if [[ $how == compositor ]]; then
      step "kill -KILL sway"
      kill -KILL "$SIG_SWAY_PID"
    else
      step "kill -$how sig"
      kill "-$how" "$sig_pid"
    fi
    exited "$sig_pid" 5
    # SIGKILL: sig does nothing; the kernel hangs the terminal up when sig's descriptors close.
    check "$how: the shell does not outlive sig" "$(until_programs "$d" '0 0'; echo $?)" \
      "exit status $status, programs $(programs "$d")"
  done
  ;;

crash)
  d=$out/crash
  private "$d"
  start_sig "$d" demo://crash demo://slow demo://animation
  step "a key into the crash page fails its view"
  key -M alt 1 -m alt
  key k
  sleep 0.3
  verdict "the crash is reported in its tab only" \
    "$(dg "d['status']=='error' and [x['error'] for x in d['tabs']]==[True, False, False], (d['status'], [x['error'] for x in d['tabs']])")"
  shot crashed
  step "switching tabs and resizing with the failed view and the slow page"
  for i in $(seq 15); do
    wtype -M alt $(( (i % 3) + 1 )) -m alt
    swaymsg -q output HEADLESS-1 resolution $((900 + i * 20))x$((600 + i * 12))
    sleep 0.1
  done
  swaymsg -q output HEADLESS-1 resolution 1280x800
  key -M alt 3 -m alt
  before=$(dg "c['page_frames_presented']" | cut -d' ' -f2)
  sleep 1
  after=$(dg "c['page_frames_presented']" | cut -d' ' -f2)
  check "the animation in the third tab still runs" "$([[ $((after - before)) -ge 20 ]]; echo $?)" "$((after - before)) frames in 1 s"
  key -M alt 2 -m alt
  sleep 1.5
  verdict "the slow page in the second tab shows" "$(dg "shown is not None and shown['tab']==2 and shown['submissions']>0, shown and (shown['tab'], shown['submissions'])")"
  shot slow-shown
  key -M alt 1 -m alt
  key -k F5
  sleep 0.5
  verdict "F5 recovers the failed view" "$(dg "d['status']!='error' and not d['tabs'][0]['error'] and shown is not None and shown['tab']==1, (d['status'], d['tabs'][0]['error'])")"
  verdict "no placement errors" "$(dg "c['placement_errors']==0, c['placement_errors']")"
  quit; code=$status
  check "it quits cleanly" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  ;;

flood)
  d=$out/flood
  private "$d"
  start_sig "$d" demo://resize
  read -r rss0 hwm0 fds0 threads0 <<< "$(proc "$sig_pid")"
  step "200 ctrl-t in one wtype"
  args=()
  for _ in $(seq 200); do args+=(-M ctrl t -m ctrl); done
  t0=$(date +%s.%N)
  wtype "${args[@]}"
  typed=$(python3 -c "import time; print(f'{time.time() - $t0:.2f}')")
  sleep 2
  verdict "200 tab.new: 201 tabs" "$(dg "len(d['tabs'])==201, len(d['tabs'])")"
  info=$(dg "c['placement_errors'], p['max_host_queue'], c['input_events']" | cut -d' ' -f2-)
  check "typed in about a second" "$(python3 -c "print(0 if $typed < 3 else 1)")" "$typed s for 200 presses; placement errors, deepest host queue, input events: $info"
  read -r rss1 hwm1 fds1 threads1 <<< "$(proc "$sig_pid")"
  check "200 home tabs: memory bounded (under 100 KiB a tab)" "$([[ $((rss1 - rss0)) -lt 20480 ]]; echo $?)" \
    "VmRSS $((rss0 / 1024)) -> $((rss1 / 1024)) MiB, fds $fds0 -> $fds1, threads $threads0 -> $threads1"
  key -M alt 1 -m alt
  step "1000 resizes, a swaymsg each, nothing in between"
  t0=$(date +%s.%N)
  for i in $(seq 1000); do
    swaymsg -q output HEADLESS-1 resolution $((640 + i % 97 * 7))x$((480 + i % 89 * 5))
  done
  swaymsg -q output HEADLESS-1 resolution 1280x800
  resized=$(python3 -c "import time; print(f'{time.time() - $t0:.2f}')")
  sleep 2
  verdict "1000 resizes: no placement errors" "$(dg "c['placement_errors']==0, (c['placement_errors'], c['placements'])")"
  verdict "buffers bounded through them (peaks)" \
    "$(dg "p['max_target_buffers']<=9 and p['max_retiring_generations']<=2, (p['max_target_buffers'], p['max_retiring_generations'], p['max_buffer_bytes'])")"
  verdict "and released after them" "$(dg "all(x['buffers']<=3 and x['retiring_generations']==0 for x in t), [(x['role'], x['buffers']) for x in t]")"
  read -r rss2 hwm2 fds2 threads2 <<< "$(proc "$sig_pid")"
  check "memory bounded after the resizes (within 32 MiB of before)" "$([[ $((rss2 - rss1)) -lt 32768 ]]; echo $?)" \
    "$resized s; VmRSS $((rss1 / 1024)) -> $((rss2 / 1024)) MiB, VmHWM $((hwm2 / 1024)) MiB, fds $fds1 -> $fds2, threads $threads1 -> $threads2"
  shot after-flood
  key -M ctrl w -m ctrl
  verdict "sig still answers keys: ctrl-w closes a tab" "$(dg "len(d['tabs'])==200, len(d['tabs'])")"
  quit; code=$status
  check "and quits within 5 s" "$([[ $code == 0 ]]; echo $?)" "exit status $code"
  ;;
esac
finish
