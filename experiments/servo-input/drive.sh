#!/usr/bin/env bash
# Inside tools/headless-sway only. Drives target/try/sig (Servo 0.5) and, with
# --engine wpe, the same binary's WPE engine. Throwaway XDG dirs under the OUT
# argument. Never the user's desktop and never the user's config.
#
#   drive.sh OUT
set -uo pipefail
out_root=${1:?usage: drive.sh OUT}
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
[[ -n ${WAYLAND_DISPLAY:-} && ${XDG_RUNTIME_DIR:-} == /tmp/sighl.* ]] || {
  echo "FAILED drive.sh: not inside tools/headless-sway" >&2
  exit 2
}
echo "STARTED drive.sh out=$out_root sway=$SIG_SWAY_RENDERER"
mkdir -p "$out_root"
probe=$repo/apps/sighurt-browser/tests/probe.py
sig=${SIG_BIN:-$repo/target/try/sig}
pointer_bin=$repo/target/debug/examples/virtual-pointer
sample=$repo/experiments/servo-input/sample_threads.py
trace=$out_root/trace.log
exec > >(tee -a "$trace") 2>&1
echo "started $(date -Is) sig=$sig"

# A local page with a fixed-position link, a CSS :hover, and a text field.
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1]); s.close()')
python3 -m http.server --bind 127.0.0.1 "$port" \
  --directory "$repo/experiments/servo-input/pages" >"$out_root/http.log" 2>&1 &
http_pid=$!
local_url=http://127.0.0.1:$port/hover.html
for _ in $(seq 50); do
  python3 -c "import urllib.request,sys; urllib.request.urlopen(sys.argv[1], timeout=1)" "http://127.0.0.1:$port/hover.html" 2>/dev/null && break
  sleep 0.1
done

# One virtual keyboard and one virtual pointer for the whole compositor.
wtype -s 3600000 ' ' >/dev/null 2>&1 &
keyboard_pid=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
"$pointer_bin" <"$XDG_RUNTIME_DIR/pointer" >"$out_root/pointer.log" 2>&1 &
pointer_pid=$!
exec 3>"$XDG_RUNTIME_DIR/pointer"
sleep 0.3

sig_pid=
win_x=0 win_y=0 win_w=1280 win_h=800
out_w=1280 out_h=800
chrome_h=84 status_h=20

cleanup() {
  [[ -n ${sig_pid:-} ]] && kill -TERM "$sig_pid" 2>/dev/null || true
  wait "${sig_pid:-}" 2>/dev/null || true
  kill "$keyboard_pid" "$pointer_pid" "$http_pid" 2>/dev/null || true
}
trap cleanup EXIT

note() { echo "[$(date +%T)] $*"; }

move_win() {
  local x=$((win_x + $1)) y=$((win_y + $2))
  echo "move $x $y $out_w $out_h" >&3
}
click_win() {
  move_win "$1" "$2"
  sleep 0.35
  echo click >&3
  sleep 0.45
}
park() { move_win 8 8; sleep 0.15; }

shot() {
  local dest=$1
  sleep "${2:-0.25}"
  grim -t ppm "$dest.ppm" || note "grim failed for $dest"
  [[ -s $dest.ppm ]] && python3 "$probe" png "$dest.ppm" "$dest.png" >/dev/null
  note "shot $(basename "$dest")"
}

differ() {
  # differ A B  x0 y0 x1 y1  -> count, or -1
  python3 "$probe" differ "$1.ppm" "$2.ppm" "$3" "$4" "$5" "$6" 2>/dev/null || echo -1
}

refresh_geom() {
  swaymsg -t get_outputs >"$out_root/outputs.json" 2>/dev/null || true
  swaymsg -t get_tree >"$out_root/tree.json" 2>/dev/null || true
  python3 - "$out_root" <<'PY' >"$out_root/window.txt" || echo "1280 800 0 0 1280 800 0" >"$out_root/window.txt"
import json, sys
root = sys.argv[1]
try:
    outs = json.load(open(f"{root}/outputs.json"))
except Exception:
    outs = []
out_w, out_h = 1280, 800
for o in outs:
    rect = o.get("rect") or {}
    if rect.get("width"):
        out_w, out_h = rect["width"], rect["height"]
        if o.get("name") == "HEADLESS-1":
            break
try:
    tree = json.load(open(f"{root}/tree.json"))
except Exception:
    tree = {}
found = []
def walk(n):
    if n.get("app_id") == "sighurt" or (n.get("name") or "").startswith("Sighurt"):
        found.append(n.get("rect") or {})
    for c in n.get("nodes", []) + n.get("floating_nodes", []):
        walk(c)
walk(tree)
r = found[0] if found else {}
print(out_w, out_h, r.get("x", 0), r.get("y", 0), r.get("width", 0), r.get("height", 0), len(found))
PY
  read -r out_w out_h win_x win_y win_w win_h found <"$out_root/window.txt" || true
  note "geom output=${out_w}x${out_h} window=${win_x},${win_y} ${win_w}x${win_h} found=${found:-0}"
}

# Ask sig for diagnostics. Prints probe's "0 value" / "1 value".
diag() {
  python3 "$probe" diag "$sig_pid" "$diagf" "$1" 2>/dev/null || echo "1 diag-failed"
}

session_url() {
  python3 "$probe" session "$session" 'url(1)' 2>/dev/null || echo "1 no-session"
}

read_layout() {
  python3 - "$diagf" <<'PY' || echo "84 20 0 0 ?"
import json, sys
d = json.load(open(sys.argv[1]))
def role(name):
    return next((t for t in d["targets"] if t["role"] == name), None)
chrome, status = role("chrome"), role("status")
view = next((t for t in d["targets"] if t["role"] == "view" and t.get("shown")), None)
print(
    (chrome or {}).get("height", 84),
    (status or {}).get("height", 20),
    (view or {}).get("width", 0),
    (view or {}).get("height", 0),
    (view or {}).get("engine") or "?",
)
loading = d["tabs"][0]["loading"] if d.get("tabs") else "?"
print(
    "loading", loading,
    "input", d["counters"]["input_events"],
    "frames", d["counters"]["page_frames_presented"],
    file=sys.stderr,
)
PY
}

# Screenshot rectangle of the page, and of the status strip, in output pixels.
page_rect() {
  echo "$win_x" $((win_y + chrome_h)) $((win_x + win_w)) $((win_y + win_h - status_h))
}
status_rect() {
  echo "$win_x" $((win_y + win_h - status_h)) $((win_x + win_w)) $((win_y + win_h))
}

wait_window() {
  local i
  for i in $(seq 80); do
    refresh_geom
    [[ ${found:-0} != 0 && ${win_w:-0} != 0 ]] && return 0
    sleep 0.25
  done
  note "no window"
  return 1
}

# Block until a frame has been presented, then until frames go quiet or the cap.
wait_settled() {
  local cap=$1 quiet_need=$2
  local i frames last=-1 quiet=0 line
  for i in $(seq "$cap"); do
    line=$(diag 'c["page_frames_presented"]')
    frames=${line#* }
    if [[ ${frames:-0} != 0 && $frames != diag-failed ]]; then
      if [[ $frames == "$last" ]]; then
        quiet=$((quiet + 1))
        [[ $quiet -ge $quiet_need ]] && { note "settled frames=$frames"; return 0; }
      else
        quiet=0
        last=$frames
      fi
    fi
    sleep 0.5
  done
  note "settle timeout frames=${frames:-none}"
  return 1
}

stop_sig() {
  local wd=${1:-}
  [[ -n $wd ]] && kill "$wd" 2>/dev/null || true
  if [[ -n ${sig_pid:-} ]] && kill -0 "$sig_pid" 2>/dev/null; then
    kill -TERM "$sig_pid" 2>/dev/null || true
    local i
    for i in $(seq 60); do
      kill -0 "$sig_pid" 2>/dev/null || break
      sleep 0.1
    done
    if kill -0 "$sig_pid" 2>/dev/null; then
      note "sig did not exit on TERM; killing"
      kill -KILL "$sig_pid" 2>/dev/null || true
    fi
  fi
  wait "${sig_pid:-}" 2>/dev/null || true
  sig_pid=
}

summarize_log() {
  local log=$1
  note "---- log $log ----"
  if [[ ! -s $log ]]; then
    note "(empty log)"
    return
  fi
  local n
  n=$(grep -c "Empty hit test" "$log" || true)
  note "empty-hit-test-lines=$n"
  grep -E "panic|FATAL|Empty hit test" "$log" | head -5 | sed 's/^/  /' || true
  # Unique script errors, trimmed.
  grep -E "ERROR|error:" "$log" | sed 's/\x1b\[[0-9;]*m//g' | cut -c1-240 | sort | uniq -c | sort -nr | head -25 | sed 's/^/  /' || true
}

# The gdb commands for SIG_CONSOLE=1: print up to 300 console messages, then stop asking.
gdb=
console_gdb() {
  [[ -n $gdb ]] || gdb=$(nix-build "$repo/nix/nixpkgs.nix" -A gdb --no-out-link)/bin/gdb
  local sym
  sym=$(nm "$sig" | awk '/WebViewDelegate20show_console_message.*sighurt_gui$/ && / T / {print $3; exit}')
  # At the function's entry rcx is &String, laid out (capacity, pointer, length) in this build.
  sed "s/@SYM@/$sym/" >"$1" <<'GDB'
set pagination off
set language c
set confirm off
set print thread-events off
handle all nostop noprint pass
set $n = 0
break *@SYM@
commands 1
  silent
  set $n = $n + 1
  set $ptr = *((unsigned long*)$rcx + 1)
  set $len = *((unsigned long*)$rcx + 2)
  set $len = $len < 600 ? $len : 600
  eval "printf \"CONSOLE[%d] level=%d %%.%ds\\n\", (char*)0x%lx", $n, (int)$rdx & 255, (int)$len, $ptr
  if $n >= 300
    disable 1
  end
  continue
end
GDB
  # SIG_GDB_BREAKS: space-separated patterns of DOM binding symbols (as nm prints them); each
  # hit prints BREAK <pattern> and the page's JS backtrace (the JSContext is the first argument).
  local pat n=1
  for pat in ${SIG_GDB_BREAKS:-}; do
    sym=$(nm "$sig" | awk -v p="$pat" '$0 ~ p && $3 !~ /^_RNC|^_RNS/ {print $3; exit}')
    [[ -n $sym ]] || { echo "console_gdb: no symbol for $pat" >&2; continue; }
    n=$((n + 1))
    printf 'break *%s\ncommands %d\n  silent\n  printf "BREAK %s\\n"\n  call (void)_ZN2js13DumpBacktraceEP9JSContext($rdi)\n  continue\nend\n' \
      "$sym" "$n" "$pat" >>"$1"
  done
  echo run >>"$1"
}

# Start sig for one case. Sets sig_pid, diagf, session, case_dir.
begin_case() {
  local name=$1 engine=$2 url=$3
  # SIG_OLD_ARGS=1: a sig from before --gui and --engine (the Servo 0.7 port of 17 October).
  local engine_args="--gui --engine $engine"
  [[ -n ${SIG_OLD_ARGS:-} ]] && engine_args=
  case_dir=$out_root/$name
  rm -rf "$case_dir"
  mkdir -p "$case_dir/config/sighurt" "$case_dir/state" "$case_dir/data" \
    "$case_dir/cache" "$case_dir/downloads" "$case_dir/shots"
  cat >"$case_dir/config/sighurt/config.toml" <<'CONF'
version = 1
[session]
restore = true
[keys]
"ctrl-l" = "location.focus"
"ctrl-q" = "browser.quit"
"alt-left" = "page.back"
CONF
  # SIG_EXTRA_CONF: more TOML for this run, e.g. "[engines.servo.preferences]" lines.
  [[ -n ${SIG_EXTRA_CONF:-} ]] && printf '%b\n' "$SIG_EXTRA_CONF" >>"$case_dir/config/sighurt/config.toml"
  export XDG_CONFIG_HOME=$case_dir/config XDG_STATE_HOME=$case_dir/state
  export XDG_DATA_HOME=$case_dir/data XDG_CACHE_HOME=$case_dir/cache
  export XDG_DOWNLOAD_DIR=$case_dir/downloads
  export SIG_DIAG=1
  export RUST_LOG='error,script::script_runtime=info,paint::webview_renderer=warn,paint::painter=warn,sighurt_engine_servo=info'
  diagf=$case_dir/diag.json
  session=$case_dir/state/sighurt/session.toml
  note "CASE $name engine=$engine url=$url"
  if [[ -n ${SIG_CONSOLE:-} && $engine == servo ]]; then
    # sig under gdb (its parent, for ptrace_scope=1), printing every console message the page
    # sends (Servo's "Error at FILE:LINE:COL MESSAGE" for uncaught errors) at the delegate's
    # no-op `show_console_message`, whose 4th argument is the message (&String). The symbol is
    # this build's; see console_gdb.
    console_gdb "$case_dir/console.gdb"
    "$gdb" -q -batch -x "$case_dir/console.gdb" --args \
      "$sig" $engine_args --diagnostics "$diagf" "$url" >"$case_dir/sig.log" 2>&1 &
    local gdb_pid=$! i
    sig_pid=
    for i in $(seq 100); do
      sig_pid=$(pgrep -P "$gdb_pid" | head -1)
      [[ -n $sig_pid ]] && break
      sleep 0.1
    done
  else
    "$sig" $engine_args --diagnostics "$diagf" "$url" >"$case_dir/sig.log" 2>&1 &
    sig_pid=$!
  fi
  ( sleep 200; kill -TERM "$sig_pid" 2>/dev/null ) &
  echo $! >"$case_dir/watchdog.pid"
  if ! wait_window; then
    note "FAILED window for $name"
    return 1
  fi
  # The handler is installed before the window exists; still give the loop a moment.
  sleep 0.5
  diag 'c["page_frames_presented"]' >/dev/null || true
  return 0
}

end_case() {
  local name=$1
  local wd=
  [[ -s $case_dir/watchdog.pid ]] && wd=$(cat "$case_dir/watchdog.pid")
  stop_sig "$wd"
  summarize_log "$case_dir/sig.log"
  echo "SCENARIO_DONE $name"
}

open_url() {
  # The address line selects its text when focused, so typing replaces it.
  wtype -M ctrl l -m ctrl
  sleep 0.3
  wtype -d 8 "$1"
  sleep 0.2
  wtype -k Return
  sleep 0.4
}

layout_of() {
  diag 'true' >/dev/null || true
  local line
  line=$(read_layout | head -1)
  read -r chrome_h status_h view_w view_h view_engine <<<"$line" || true
  note "layout chrome=$chrome_h status=$status_h view=${view_w}x${view_h} engine=$view_engine"
}

# --- cases -----------------------------------------------------------------

case_local() {
  local engine=$1
  local name=${engine}-local
  begin_case "$name" "$engine" "$local_url" || { end_case "$name"; return; }
  wait_settled 30 3 || true
  layout_of
  python3 "$sample" "$sig_pid" 2 "${engine}-local-idle" || true
  local shots=$case_dir/shots
  park
  shot "$shots/before"
  # Pointer over empty page background, then over the Platform link.
  local py=$((chrome_h + 64))
  move_win 20 "$py"
  sleep 0.4
  shot "$shots/away"
  move_win 180 "$py"
  sleep 0.5
  shot "$shots/hover"
  local idle hover
  idle=$(differ "$shots/before" "$shots/away" $(page_rect))
  hover=$(differ "$shots/away" "$shots/hover" $(page_rect))
  note "local differ idle=$idle hover=$hover (page rect)"
  # Click the field and type. The field is not autofocused.
  click_win 260 $((chrome_h + 144))
  sleep 0.3
  wtype -d 12 abc
  sleep 0.6
  shot "$shots/typed"
  note "local url after type: $(session_url)"
  note "local input_events: $(diag 'c["input_events"]')"
  # Click the link (center of the 200x48 box at 80,40).
  click_win 180 "$py"
  sleep 1.2
  shot "$shots/after-click"
  note "local url after click: $(session_url)"
  end_case "$name"
}

case_github() {
  local engine=$1
  local name=${engine}-github
  begin_case "$name" "$engine" "https://github.com/" || { end_case "$name"; return; }
  wait_settled 40 4 || true
  # Give late content a moment, then measure whether the picture is still moving.
  sleep 2
  layout_of
  python3 "$sample" "$sig_pid" 2 "${engine}-github-idle" || true
  local shots=$case_dir/shots
  park
  shot "$shots/idle-a"
  sleep 1.2
  shot "$shots/idle-b"
  local still hy hx
  hy=$((chrome_h + 32))
  still=$(differ "$shots/idle-a" "$shots/idle-b" $(page_rect))
  note "github still-picture differ=$still"
  # Hover the left of the top nav (Platform is a <button>) and the right (Sign in is <a href=/login>).
  move_win 220 "$hy"
  sleep 0.6
  shot "$shots/hover-left"
  move_win 1100 "$hy"
  sleep 0.6
  shot "$shots/hover-right"
  # A point in the hero, below the header.
  move_win 480 $((chrome_h + 280))
  sleep 0.6
  shot "$shots/hover-hero"
  note "github differ left=$(differ "$shots/idle-b" "$shots/hover-left" $(page_rect)) right=$(differ "$shots/idle-b" "$shots/hover-right" $(page_rect)) hero=$(differ "$shots/idle-b" "$shots/hover-hero" $(page_rect)) status-right=$(differ "$shots/idle-b" "$shots/hover-right" $(status_rect))"
  python3 "$sample" "$sig_pid" 2 "${engine}-github-during-hover" || true
  note "github input_events after hover: $(diag 'c["input_events"]')"
  # Clicks along the header, right to left, stopping if the address changes.
  local x url_before url_now
  url_before=$(session_url)
  note "github url before clicks: $url_before"
  for x in 1220 1140 1060 980 860 700 360 200; do
    click_win "$x" "$hy"
    sleep 1.3
    url_now=$(session_url)
    note "github click x=$x y=$hy -> $url_now"
    if [[ $url_now != "$url_before" && $url_now != *no-session* ]]; then
      shot "$shots/after-click"
      break
    fi
  done
  [[ -f $shots/after-click.ppm ]] || shot "$shots/after-clicks-none"
  # The login form is server-rendered with autofocus on the username field.
  # Typing before any click separates keyboard delivery from hit testing.
  open_url "https://github.com/login"
  wait_settled 30 3 || true
  sleep 1
  layout_of
  note "login url: $(session_url)"
  park
  shot "$shots/login-before"
  wtype -d 20 aaa
  sleep 0.7
  shot "$shots/login-typed-nofocus-click"
  # Then click where the username field sits on github.com/login (centered card).
  click_win 640 $((chrome_h + 230))
  sleep 0.3
  wtype -d 20 bbb
  sleep 0.7
  shot "$shots/login-typed-after-click"
  note "login url after type: $(session_url)"
  note "login input_events: $(diag 'c["input_events"]')"
  python3 "$sample" "$sig_pid" 2 "${engine}-login-idle" || true
  end_case "$name"
}

case_chatgpt() {
  local engine=$1
  local name=${engine}-chatgpt
  begin_case "$name" "$engine" "https://chatgpt.com/" || { end_case "$name"; return; }
  wait_settled 40 4 || true
  sleep 2
  layout_of
  python3 "$sample" "$sig_pid" 2 "${engine}-chatgpt-idle" || true
  local shots=$case_dir/shots
  park
  shot "$shots/idle-a"
  sleep 1.2
  shot "$shots/idle-b"
  note "chatgpt still-picture differ=$(differ "$shots/idle-a" "$shots/idle-b" $(page_rect))"
  local hy=$((chrome_h + 36))
  move_win 1100 "$hy"
  sleep 0.6
  shot "$shots/hover-top-right"
  move_win 640 $((chrome_h + 360))
  sleep 0.6
  shot "$shots/hover-center"
  note "chatgpt differ top=$(differ "$shots/idle-b" "$shots/hover-top-right" $(page_rect)) center=$(differ "$shots/idle-b" "$shots/hover-center" $(page_rect))"
  note "chatgpt input_events after hover: $(diag 'c["input_events"]')"
  local x url_before url_now
  url_before=$(session_url)
  note "chatgpt url before clicks: $url_before"
  for x in 1180 1080 980 860 640; do
    click_win "$x" "$hy"
    sleep 1.2
    url_now=$(session_url)
    note "chatgpt click header x=$x -> $url_now"
    if [[ $url_now != "$url_before" && $url_now != *no-session* ]]; then
      shot "$shots/after-header-click"
      break
    fi
  done
  # A click in the middle of the page, where a logged-out login button often sits.
  url_before=$(session_url)
  click_win 640 $((chrome_h + 420))
  sleep 1.4
  note "chatgpt click center -> $(session_url) (was $url_before)"
  shot "$shots/after-center-click"
  # Try the email field: click the center again (a form may have opened) and type.
  click_win 640 $((chrome_h + 300))
  sleep 0.3
  wtype -d 20 abc
  sleep 0.7
  shot "$shots/typed"
  note "chatgpt url after type: $(session_url)"
  note "chatgpt input_events: $(diag 'c["input_events"]')"
  python3 "$sample" "$sig_pid" 2 "${engine}-chatgpt-after" || true
  end_case "$name"
}

# Profiles Script#1 (the thread that runs the page's JS, style and layout) for 6 s on a page
# that is already showing. SpiderMonkey writes a jitdump (IONPERF=func) so JIT frames get names.
case_perf() {
  local engine=$1 url=$2 name=$3
  mkdir -p "$out_root/$name-spew"
  export IONPERF=func PERF_SPEW_DIR=$out_root/$name-spew
  begin_case "$name" "$engine" "$url" || { end_case "$name"; return; }
  unset IONPERF PERF_SPEW_DIR
  wait_settled 40 4 || true
  sleep 4
  python3 "$sample" "$sig_pid" 2 "${name}-before-perf" || true
  local tid
  tid=$(grep -l '^Script#1$' /proc/"$sig_pid"/task/*/comm 2>/dev/null | head -1 | cut -d/ -f5)
  note "perf Script#1 tid=$tid"
  if [[ -n $tid ]]; then
    perf record --user-callchains --call-graph dwarf,32768 -e cpu-clock -F 499 -t "$tid" -o "$case_dir/perf.data" -- sleep 6 \
      >"$case_dir/perf.log" 2>&1 || note "perf failed: $(tail -2 "$case_dir/perf.log")"
  fi
  local shots=$case_dir/shots
  shot "$shots/during-perf"
  end_case "$name"
  # The jitdump is written to the working directory (or PERF_SPEW_DIR) as jit-PID.dump.
  ls -la "$out_root/$name-spew" | head || true
}

# github.com/login as the first page: is the form usable when the home page never ran?
case_github_login() {
  local engine=$1
  local name=${engine}-github-login
  begin_case "$name" "$engine" "https://github.com/login" || { end_case "$name"; return; }
  wait_settled 40 4 || true
  sleep 2
  layout_of
  python3 "$sample" "$sig_pid" 2 "${name}-idle" || true
  local shots=$case_dir/shots
  park
  shot "$shots/a-idle"
  wtype -d 30 aaa
  sleep 0.8
  shot "$shots/b-typed-autofocus"
  click_win 640 $((chrome_h + ${USER_Y:-230}))
  sleep 0.4
  wtype -d 30 bbb
  sleep 0.8
  shot "$shots/c-typed-after-click"
  note "github-login input_events: $(diag 'c["input_events"]')"
  end_case "$name"
}

# Any page: type with no click (autofocus), then click at page point X,Y and type again.
case_type_at() {
  local engine=$1 url=$2 name=$3 x=$4 y=$5
  begin_case "$name" "$engine" "$url" || { end_case "$name"; return; }
  wait_settled 40 4 || true
  sleep 3
  layout_of
  python3 "$sample" "$sig_pid" 2 "${name}-idle" || true
  note "$name url: $(session_url)"
  local shots=$case_dir/shots
  park
  shot "$shots/a-idle"
  wtype -d 30 123
  sleep 0.8
  shot "$shots/b-typed-no-click"
  click_win "$x" $((chrome_h + y))
  sleep 0.5
  wtype -d 30 456
  sleep 0.8
  shot "$shots/c-typed-after-click"
  note "$name url after: $(session_url) input_events: $(diag 'c["input_events"]')"
  end_case "$name"
}

# Any page: click X1,Y1 (a button that opens a form), type with no further click (does the form
# take focus itself?), then click X2,Y2 (the field) and type again.
case_click_then_type() {
  local engine=$1 url=$2 name=$3 x1=$4 y1=$5 x2=$6 y2=$7
  begin_case "$name" "$engine" "$url" || { end_case "$name"; return; }
  wait_settled 40 4 || true
  sleep 3
  layout_of
  local shots=$case_dir/shots
  park
  shot "$shots/a-idle"
  click_win "$x1" $((chrome_h + y1))
  sleep 4
  note "$name url after first click: $(session_url)"
  park
  shot "$shots/b-after-first-click"
  wtype -d 30 123
  sleep 0.8
  shot "$shots/c-typed-no-click"
  click_win "$x2" $((chrome_h + y2))
  sleep 0.5
  wtype -d 30 456
  sleep 0.8
  shot "$shots/d-typed-after-click"
  python3 "$sample" "$sig_pid" 2 "${name}-end" || true
  note "$name url after: $(session_url) input_events: $(diag 'c["input_events"]')"
  end_case "$name"
}

# chatgpt.com: open the "Log in" modal, click its email field, type, then check where the text
# went (the modal's field, or the "Ask ChatGPT" composer behind it).
case_chatgpt_modal() {
  local engine=$1
  local name=${engine}-chatgpt-modal
  begin_case "$name" "$engine" "https://chatgpt.com/" || { end_case "$name"; return; }
  wait_settled 40 4 || true
  sleep 2
  layout_of
  local shots=$case_dir/shots
  park
  shot "$shots/a-idle"
  # "Log in" in the header (page 1080,26 in Servo's layout, 1116,26 in WebKit's).
  click_win "${LOGIN_X:-1085}" $((chrome_h + 26))
  sleep 1.5
  shot "$shots/b-modal"
  # The modal's "Email address" field (Servo's layout: page y ~488; WebKit's ~ 500).
  click_win 640 $((chrome_h + ${EMAIL_Y:-488}))
  sleep 0.5
  shot "$shots/c-email-clicked"
  wtype -d 30 xyz
  sleep 0.8
  shot "$shots/d-typed"
  note "chatgpt-modal input_events: $(diag 'c["input_events"]')"
  end_case "$name"
}

note "local server $local_url"
"$sig" --version || true
if [[ $# -gt 1 ]]; then
  shift
  for c in "$@"; do
    # shellcheck disable=SC2086
    eval "$c"
  done
else
  case_local servo
  case_github servo
  case_chatgpt servo
  case_local wpe
  case_github wpe
  case_chatgpt wpe
fi
echo "DONE servo-input $(date -Is)"
sleep 0.5
