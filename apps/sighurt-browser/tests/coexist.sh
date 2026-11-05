#!/usr/bin/env bash
# End-to-end coexistence test: every engine of an `--all-features` sig in ONE process, in a private
# headless sway (GLES2) on a private D-Bus session, never on the desktop. One tab each in Servo
# (127.0.0.1), WPE WebKit (127.0.0.2), Ladybird (127.0.0.3), CEF (127.0.0.4), NetSurf (127.0.0.5),
# Wasm (an example guest, if built) and the demo engine (demo://pattern), the web engines'
# pages served by serve.py on those loopback addresses and routed by [routing.<engine>] hosts.
#
#   apps/sighurt-browser/tests/coexist.sh [--build] OUT
#
# OUT must be under /data/sig-builds/test-out/ (every XDG directory of the run is under it: the
# user's own configuration, session and profile are never touched). --build builds sig with
# --all-features first (it takes the heavy lock; Servo builds only when the desktop is free).
# Uses target/debug/sig, which must be built with every engine (SIG_BIN overrides it).
#
# Checks: every tab loads in its engine (diagnostics, session titles); each tab's screenshot
# shows its page when selected; switching back and forth; typing into one engine's field in
# INSERT mode reaches that page only (and no keymap binding); a link in the Servo page routed to WPE moves
# the tab; closing Ladybird's and CEF's tabs and reopening them leaves WPE's web process alive
# and its page working; `q` quits within the shutdown bound; no child of sig is left. It also
# records the process tree's RSS and the idle CPU of the whole tree over 10 s.
# Screenshots and logs go to OUT; look at the screenshots. Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"
if [[ ${1:-} != --inside ]]; then
  build=0
  [[ ${1:-} == --build ]] && { build=1; shift; }
  out=$(realpath -m "${1:?usage: coexist.sh [--build] OUT}")
  [[ $out == /data/sig-builds/test-out/?* ]] || { echo "coexist.sh: OUT must be under /data/sig-builds/test-out/" >&2; exit 2; }
  if [[ $build == 1 ]]; then
    SIG_WHAT="servo build: sig --all-features for coexist.sh" tools/cargo build -p sighurt-browser \
      --all-features --bins --example virtual-pointer || exit 1
    SIG_WHAT="build the example guests for coexist.sh" tools/cargo build --target wasm32-unknown-unknown \
      --release -p gradient-demo || exit 1
  fi
  SIG_WHAT="coexist.sh" tools/heavy tools/headless-sway --renderer gles2 -- \
    engines/sighurt-engine-wpe/tests/private-bus "$0" --inside "$out"
  exit $?
fi

out=$2
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "coexist.sh: run me through tools/headless-sway" >&2
  exit 2
}
[[ $out == /data/sig-builds/test-out/?* ]] || exit 2
rm -rf "$out"
mkdir -p "$out/config/sighurt" "$out/state" "$out/data" "$out/cache" "$out/downloads" "$out/guests" "$out/home"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_DATA_HOME=$out/data
export XDG_CACHE_HOME=$out/cache XDG_DOWNLOAD_DIR=$out/downloads HOME=$out/home
export FONTCONFIG_FILE=${FONTCONFIG_FILE:-/etc/fonts/fonts.conf}
export SIG_DIAG=1 RUST_LOG=${RUST_LOG:-warn}
sig=${SIG_BIN:-target/debug/sig}
vptr=target/debug/examples/virtual-pointer
probe=apps/sighurt-browser/tests/probe.py
session=$out/state/sighurt/session.toml
scenario=coexist

# Five servers, one per loopback address, on the same port.
python3 apps/sighurt-browser/tests/serve.py apps/sighurt-browser/tests/pages "$out/guests" "$out/port" \
  "$out/server-1.log" &
server_pids=($!)
for _ in $(seq 50); do [[ -s $out/port ]] && break; sleep 0.1; done
port=$(cat "$out/port")
for n in 2 3 4 5; do
  SERVE_BIND=127.0.0.$n SERVE_PORT=$port python3 apps/sighurt-browser/tests/serve.py \
    apps/sighurt-browser/tests/pages "$out/guests" "$out/port-$n" "$out/server-$n.log" &
  server_pids+=($!)
done
sleep 0.5
url() { echo "http://127.0.0.$1:$port/coexist.html"; }
typed_log() { cat "$out"/server-*.log 2>/dev/null | grep -o ' GET /typed/.*' | sed 's| GET /typed/||'; }

wasm=
for g in gradient_demo hello_sighurt; do
  [[ -z $wasm && -f target/wasm32-unknown-unknown/release/$g.wasm ]] && {
    cp "target/wasm32-unknown-unknown/release/$g.wasm" "$out/guests/guest.wasm"
    wasm=$out/guests/guest.wasm
  }
done

cat > "$out/config/sighurt/config.toml" <<CONF
version = 1
[downloads]
directory = "$out/downloads"
[routing.wpe]
hosts = ["127.0.0.2"]
[routing.ladybird]
hosts = ["127.0.0.3"]
[routing.cef]
hosts = ["127.0.0.4"]
[routing.netsurf]
hosts = ["127.0.0.5"]
[routing.demo]
schemes = ["demo"]
CONF

failures=0
check() {
  if [[ $2 == 0* ]]; then echo "PASS $scenario: $1 (${2:2})"; else
    echo "FAIL $scenario: $1 (${2:2})"; failures=$((failures + 1)); fi
}
answer() { if [[ $1 == 0 ]]; then echo "0 $2"; else echo "1 $2"; fi; }
info() { echo "INFO $scenario: $*"; }
note() { echo "NOTE $scenario: $*"; }
step() { echo "[$(date +%T.%3N)] $*" >> "$out/steps.log"; }
shot() { sleep "${2:-0.5}"; grim -t ppm "$out/$1.ppm"; python3 "$probe" png "$out/$1.ppm" "$out/$1.png"; step "screenshot $1"; }

wtype -s 3600000 ' ' &
keyboard=$!
mkfifo "$XDG_RUNTIME_DIR/pointer"
"$vptr" < "$XDG_RUNTIME_DIR/pointer" &
pointer_pid=$!
exec 3> "$XDG_RUNTIME_DIR/pointer"
trap 'kill "$keyboard" "$pointer_pid" "${server_pids[@]}" ${sig_pid:-} 2>/dev/null' EXIT
layout_w=1280 layout_h=800 chrome_h=84
point() { echo "move $1 $2 $layout_w $layout_h" >&3; }
click() { point "$1" "$2"; sleep 0.3; echo click >&3; sleep 0.2; }
key() { wtype "$@"; sleep 0.25; }
# Every page has its field at page (20..320, 20..50) and its link at (20..520, 100..160).
field_click() { click 100 $((chrome_h + 35)); sleep 0.4; }
# The example keymap gives the page keys only in INSERT mode: Escape enters it and leaves it.
insert() { key -k Escape; key "$@"; key -k Escape; }
blank_click() { click 1000 $((chrome_h + 600)); sleep 0.3; }
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
shows_tab() {
  dg 8 "d['active_tab']==$1 and shown is not None and shown['tab']==$1 and ('${2:-}' in ('', shown['engine'])), (d['active_tab'], shown and (shown['tab'], shown['engine']))"
}
# colour NAME X Y: screenshot NAME, then its pixel as "r g b".
colour() { shot "$1" "${4:-0.5}" >/dev/null; python3 "$probe" pixel "$out/$1.ppm" "$2" "$3"; }
# near "r g b" R G B: within 8 per channel.
near() { python3 -c "import sys; a=list(map(int,sys.argv[1].split())); b=list(map(int,sys.argv[2:])); sys.exit(any(abs(x-y)>8 for x,y in zip(a,b)))" "$@"; }
# distinct PPM: how many distinct colours the page area of a screenshot has (a painted guest has many).
distinct() {
  python3 - "$1" <<'PY'
import re, sys
d = open(sys.argv[1], 'rb').read()
m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', d)
w, p = int(m[1]), d[m.end():]
print(len({p[3*(y*w+x):3*(y*w+x)+3] for y in range(100, 780, 3) for x in range(0, 1270, 3)}))
PY
}

# The process tree: every descendant of sig, remembered by pid and start time.
descendants() {
  local pids=("$sig_pid") i=0 kids
  while ((i < ${#pids[@]})); do
    mapfile -t kids < <(pgrep -P "${pids[i]}")
    pids+=("${kids[@]}")
    i=$((i + 1))
  done
  printf '%s\n' "${pids[@]}"
}
starttime() { sed 's/^.*) //' "/proc/$1/stat" 2>/dev/null | awk '{print $20}'; }
pstate() { sed 's/^.*) //' "/proc/$1/stat" 2>/dev/null | awk '{print $1}'; }
snap() {
  local p st
  for p in $(descendants); do
    st=$(starttime "$p")
    [[ -n $st ]] && echo "$p $st $(tr '\0' ' ' < "/proc/$p/cmdline" | cut -c1-200)"
  done >> "$out/procs.raw"
  sort -u -k1,2 "$out/procs.raw" -o "$out/procs.txt"
}
alive() { [[ -n $(starttime "$1") && $(starttime "$1") == "$2" && $(pstate "$1") != Z ]]; }
# tree_stats FILE: "pid rss_kib pss_kib ticks command" for sig and every descendant.
tree_stats() {
  local p
  for p in $(descendants); do
    echo "$p $(awk '/^VmRSS/ {print $2}' "/proc/$p/status" 2>/dev/null || echo 0) $(awk '/^Pss:/ {print $2}' "/proc/$p/smaps_rollup" 2>/dev/null || echo 0) $(sed 's/^.*) //' "/proc/$p/stat" 2>/dev/null | awk '{print $12 + $13}') $(tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null | awk '{n=split($1,a,"/"); print a[n]}')"
  done
}
wpe_pids() { pgrep -P "$sig_pid" -f 'WPEWebProcess' ; descendants | while read -r p; do tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null | grep -q 'WPEWebProcess' && echo "$p"; done | sort -u; }
HELPERS='WPEWebProcess|WPENetworkProcess|WebKitWebProcess|libexec/(\.)?(WebContent|RequestServer|ImageDecoder|WebWorker|Compositor|WasmCompiler)|sig-cef-helper'

engines=(servo wpe ladybird cef netsurf wasm demo)
args=("$(url 1)" "$(url 2)" "$(url 3)" "$(url 4)" "$(url 5)" "${wasm:-}" demo://pattern)
if [[ -z $wasm ]]; then
  note "no example guest under target/wasm32-unknown-unknown/release: the Wasm tab is skipped"
  engines=(servo wpe ladybird cef netsurf demo)
  args=("$(url 1)" "$(url 2)" "$(url 3)" "$(url 4)" "$(url 5)" demo://pattern)
fi
n=${#engines[@]}
tab_of() { local i; for i in "${!engines[@]}"; do [[ ${engines[i]} == "$1" ]] && { echo $((i + 1)); return; }; done; }

check "the compositor renders with the GPU" "$(answer "$([[ $SIG_SWAY_RENDERER == gles2 ]]; echo $?)" "sway's renderer: $SIG_SWAY_RENDERER")"
step "start sig with $n tabs: ${engines[*]}"
"$sig" --gui --diagnostics "$out/diag.json" "${args[@]}" >> "$out/sig.log" 2>&1 &
sig_pid=$!
# SIGUSR1 (the diagnostics probe) kills sig until it has installed its handler: wait for the first
# commit, which writes the session file, before the first probe.
for _ in $(seq 600); do [[ -s $session ]] && break; kill -0 "$sig_pid" 2>/dev/null || break; sleep 0.2; done
sleep 2

# ── Loading ───────────────────────────────────────────────────────────────────────────────────
list=$(printf "'%s', " "${engines[@]}")
check "$n tabs open, each in its engine, none failed" \
  "$(dg 120 "len(tabs)==$n and [x['engine'] for x in tabs]==[$list] and not any(x['error'] for x in tabs), [(x['engine'], x['error']) for x in tabs]")"
for e in servo wpe ladybird cef netsurf; do
  i=$(tab_of $e)
  check "tab $i ($e) commits and its title arrives" \
    "$(sess 90 "seg($i)['engine']=='$e' and seg($i).get('title')=='coexist@127.0.0.$i', (seg($i)['engine'], seg($i).get('title'))")"
done
if [[ -n $wasm ]]; then
  i=$(tab_of wasm)
  check "tab $i (wasm) loads the guest" "$(sess 60 "seg($i)['engine']=='wasm', (seg($i)['engine'], url($i))")"
fi
i=$(tab_of demo)
check "tab $i (demo) opens demo://pattern" "$(sess 30 "seg($i)['engine']=='demo' and url($i).startswith('demo:'), (seg($i)['engine'], url($i))")"
check "no tab is still loading" "$(dg 60 "not any(x['loading'] for x in tabs), [x['loading'] for x in tabs]")"
sleep 3
snap
for e in servo wpe ladybird cef netsurf; do
  note "$e engine line: $(grep -o "renderer engine=$e .*" "$out/sig.log" | head -1)"
done

# ── Each tab's page, selected ─────────────────────────────────────────────────────────────────
declare -A bg=([servo]="217 236 255" [wpe]="217 255 217" [ladybird]="255 242 194" [cef]="240 217 255" [netsurf]="255 224 194")
page_shows() {
  # page_shows TAB NAME: alt-TAB, the engine's view shown, and the page on screen.
  local i=$1 e=${engines[$1 - 1]} px d
  key -M alt "$i" -m alt
  check "alt-$i shows the $e view${2:+ ($2)}" "$(shows_tab "$i" "$e")"
  sleep 1.2
  if [[ -n ${bg[$e]:-} ]]; then
    px=$(colour "$2-$i-$e" 1000 500 0.3)
    check "$e's page is on screen${2:+ ($2)}: its background at (1000, 500)" \
      "$(answer "$(near "$px" ${bg[$e]}; echo $?)" "pixel $px, expected ${bg[$e]}")"
  else
    shot "$2-$i-$e" 0.3
    d=$(distinct "$out/$2-$i-$e.ppm")
    check "$e's view paints (${d} distinct colours in the page area)${2:+ ($2)}" "$(answer "$([[ $d -ge 8 ]]; echo $?)" "$d colours")"
  fi
}
for i in $(seq 1 "$n"); do page_shows "$i" first; done
step "switching back and forth"
for i in 3 1 4 2 5 $n 1 4 3 $n 2 5 1; do page_shows "$i" shuffle; done
check "after the shuffle sig is alive, no tab failed" "$(dg 5 "not any(x['error'] for x in tabs) and d['status']!='error', (d['status'], [x['error'] for x in tabs])")"

# ── Idle: all tabs open, the pages static ─────────────────────────────────────────────────────
step "idle, Servo's tab shown"
key -M alt 1 -m alt
point 1200 790
sleep 5
tree_stats > "$out/tree-rss.txt"
snap
cp "$out/tree-rss.txt" "$out/tree-before.txt"
w0=$(value "(c['wakeups'], c['chrome_paints'], c['page_frames_presented'])")
sleep 10
tree_stats > "$out/tree-after.txt"
w1=$(value "(c['wakeups'], c['chrome_paints'], c['page_frames_presented'])")
python3 - "$out" "$w0" "$w1" <<'PY' | tee "$out/measurements.txt" | sed 's/^/INFO coexist: /'
import sys, os, collections
out, w0, w1 = sys.argv[1], eval(sys.argv[2]), eval(sys.argv[3])
def load(f):
    r = {}
    for line in open(f):
        p = line.split(None, 4)
        if len(p) >= 5: r[int(p[0])] = (int(p[1]), int(p[2]), int(p[3]), p[4].strip())
    return r
a, b = load(f"{out}/tree-before.txt"), load(f"{out}/tree-after.txt")
hz = os.sysconf("SC_CLK_TCK")
rss = sum(v[0] for v in a.values()) / 1024
pss = sum(v[1] for v in a.values()) / 1024
print(f"process tree with every engine open: {len(a)} processes, RSS summed {rss:.0f} MiB (shared pages counted per process), PSS summed {pss:.0f} MiB")
by = collections.defaultdict(lambda: [0, 0, 0])
for v in a.values():
    by[v[3]][0] += 1; by[v[3]][1] += v[0] / 1024; by[v[3]][2] += v[1] / 1024
for k, (c, r, p) in sorted(by.items(), key=lambda x: -x[1][1]):
    print(f"  {k}: {c} process(es), RSS {r:.0f} MiB, PSS {p:.0f} MiB")
cpu = {pid: (b[pid][2] - a[pid][2]) for pid in b if pid in a}
total = sum(cpu.values()) / hz
print(f"idle CPU over 10 s, pages static (Servo's tab shown): {total:.2f} CPU-s = {total / 10 * 100:.1f} % of one core")
for pid, t in sorted(cpu.items(), key=lambda x: -x[1])[:6]:
    if t: print(f"  pid {pid} {b[pid][3]}: {t / hz:.2f} CPU-s")
print(f"sig in 10 s idle: {w1[0] - w0[0]} wakeups, {w1[1] - w0[1]} chrome paints, {w1[2] - w0[2]} page frames")
open(f"{out}/idle-cpu-percent", "w").write(f"{total / 10 * 100:.2f}")
PY
cpu=$(cat "$out/idle-cpu-percent" 2>/dev/null || echo 999)
check "an idle sig with every engine open and static pages uses under 5 % of a core in all (reference: the idle CPU above)" \
  "$(answer "$(python3 -c "import sys; sys.exit(not $cpu < 5)"; echo $?)" "$cpu % of one core in 10 s")"
check "the tabs are still alive after the idle" "$(dg 5 "not any(x['error'] for x in tabs), [x['error'] for x in tabs]")"

# ── WPE's web process, recorded ────────────────────────────────────────────────────────────────
# (the process itself, not the bwrap that starts it)
read -r wpe_pid wpe_start _ < <(awk '$3 ~ /WPEWebProcess$/ {print; exit}' "$out/procs.txt")
if [[ -n ${wpe_pid:-} ]]; then
  info "WPE's web process: pid $wpe_pid (start $wpe_start); the tree: $(awk '{print $1, $3}' "$out/procs.txt" | sed 's|/[^ ]*/||' | tr '\n' ';' | cut -c1-600)"
else
  check "WPE's web process is in sig's tree" "1 no WPEWebProcess among: $(cut -c1-400 "$out/procs.txt" | tr '\n' ';')"
fi
wpe_ok() {
  [[ -n ${wpe_pid:-} ]] && alive "$wpe_pid" "$wpe_start"
}

# ── Typing reaches one engine only ─────────────────────────────────────────────────────────────
declare -A word=([servo]=tx [wpe]=eo [ladybird]=xt [cef]=oe)
tabs_before=$(value "len(tabs)")
for e in servo wpe ladybird cef; do
  i=$(tab_of $e)
  step "typing ${word[$e]} into $e's field"
  key -M alt "$i" -m alt
  sleep 1
  field_click
  insert "${word[$e]}"
  sleep 1.5
  t=$(typed_log | grep "^127.0.0.$i/" | tr '\n' ' ')
  check "$e: '${word[$e]}' typed into its field reaches its page" \
    "$(answer "$(typed_log | grep -qx "127.0.0.$i/${word[$e]}"; echo $?)" "page reported: $t")"
  check "$e: the letters were text, not keymap commands (still $tabs_before tabs, tab $i active)" \
    "$(dg 5 "len(tabs)==$tabs_before and d['active_tab']==$i and not any(x['error'] for x in tabs), (len(tabs), d['active_tab'])")"
  shot "typed-$i-$e" 0.3
done
# NetSurf has no scripts: its field's dark pixels grow.
dark() {
  python3 - "$@" <<'PY'
import re, sys
d = open(sys.argv[1], 'rb').read()
m = re.match(rb'P6\s+(\d+)\s+(\d+)\s+\d+\s', d)
w, p = int(m[1]), d[m.end():]
x0, y0, x1, y1 = map(int, sys.argv[2:6])
print(sum(1 for y in range(y0, y1) for x in range(x0, x1) if max(p[3*(y*w+x):3*(y*w+x)+3]) < 110))
PY
}
i=$(tab_of netsurf)
key -M alt "$i" -m alt
sleep 1
shot "netsurf-before-typing" 0.3
before=$(dark "$out/netsurf-before-typing.ppm" 20 $((chrome_h + 20)) 320 $((chrome_h + 50)))
field_click
insert ox
sleep 1
shot "typed-$i-netsurf" 0.3
after=$(dark "$out/typed-$i-netsurf.ppm" 20 $((chrome_h + 20)) 320 $((chrome_h + 50)))
check "netsurf: 'ox' typed into its field draws text there" "$(answer "$([[ $after -gt $before ]]; echo $?)" "dark pixels in the field $before -> $after")"
check "netsurf: the letters were text, not commands" \
  "$(dg 5 "len(tabs)==$tabs_before and d['active_tab']==$i and not any(x['error'] for x in tabs), (len(tabs), d['active_tab'])")"
# Nothing reached a page it wasn't typed into: every report is a prefix of its own page's word.
bad=$(typed_log | python3 -c '
import sys
word = {"127.0.0.1": "tx", "127.0.0.2": "eo", "127.0.0.3": "xt", "127.0.0.4": "oe"}
for line in sys.stdin:
    host, _, v = line.strip().partition("/")
    if not (host in word and word[host].startswith(v)): print(line.strip(), end="; ")
')
check "no page reported letters typed into another engine's page" "$(answer "$([[ -z $bad ]]; echo $?)" "stray reports: ${bad:-none}; all: $(typed_log | tr '\n' ' ')")"
# The other engines' fields are still as they were: re-visit Servo's and look.
key -M alt 1 -m alt
sleep 1
shot "servo-after-others-typed" 0.3
check "Servo's field still holds its own word (tx) after the others were typed into" \
  "$(answer "$([[ $(typed_log | grep -c '^127.0.0.1/') == 2 ]]; echo $?)" "$(typed_log | grep '^127.0.0.1/' | tr '\n' ' ')")"

# ── A link in the Servo page routed to WPE moves the tab ───────────────────────────────────────
step "click on the link in the Servo page"
key -M alt 1 -m alt
sleep 1
click 100 $((chrome_h + 130))
check "the click moves tab 1 to WPE: a wpe segment after the Servo page" \
  "$(sess 30 "[e for e, _, _ in segs(1)]==['servo', 'wpe'] and url(1).startswith('http://127.0.0.2:$port/coexist.html?from=127.0.0.1'), (segs(1), url(1))")"
check "tab 1 shows a WPE view" "$(shows_tab 1 wpe)"
sleep 1.5
px=$(colour "link-moved-to-wpe" 1000 500 0.3)
check "WPE's page (green) is on screen in tab 1" "$(answer "$(near "$px" 217 255 217; echo $?)" "pixel $px")"
blank_click
# Shift+h as a keyboard sends it: wtype's own capital letters do not reach the keymap as H.
key -M shift -k h -m shift
check "back (Shift+h) returns to the Servo page, shown in its Servo view" "$(shows_tab 1 servo)"
sleep 1
px=$(colour "link-back-to-servo" 1000 500 0.3)
check "the Servo page (blue) is on screen again" "$(answer "$(near "$px" 217 236 255; echo $?)" "pixel $px")"
snap

# ── WPE survives Ladybird's and CEF's helpers coming and going ─────────────────────────────────
close_tab() {
  # close_tab TAB: select it, blur any field and close it with x.
  key -M alt "$1" -m alt
  sleep 1
  blank_click
  key x
  sleep 2
}
wpe_works() {
  # wpe_works LABEL WORD: WPE's tab (2), reloaded, takes WORD in its field.
  local i=$(tab_of wpe) w=$2
  key -M alt "$i" -m alt
  sleep 1
  blank_click
  key r
  sleep 3
  field_click
  insert "$w"
  sleep 1.5
  check "$1: WPE's page, reloaded, takes '$w' typed into it" \
    "$(answer "$(typed_log | grep -qx "127.0.0.2/$w"; echo $?)" "page reported: $(typed_log | grep '^127.0.0.2/' | tr '\n' ' ')")"
  px=$(colour "wpe-$2" 1000 500 0.3)
  check "$1: WPE's page is painted" "$(answer "$(near "$px" 217 255 217; echo $?)" "pixel $px")"
  check "$1: WPE's web process is the same one, alive" "$(answer "$(wpe_ok; echo $?)" "pid ${wpe_pid:-none}, state $(pstate "${wpe_pid:-0}")")"
}
wpe_works "after Ladybird and CEF started" te
check "WPE's tab shows no failure in the log" \
  "$(answer "$(! grep -qiE 'view[ _]?failed|runtime[ _]?failed|crashed|web process.*(terminated|exited)' "$out/sig.log"; echo $?)" "$(grep -ciE 'view[ _]?failed|runtime[ _]?failed|crashed' "$out/sig.log") matching lines")"
# A second Ladybird view while the first lives.
step "a second Ladybird tab while the first is open"
blank_click
key t
sleep 2
key -M ctrl o -m ctrl
sleep 0.5
key ":page.open-in ladybird $(url 3)"
key -k Return
second_lb=$(value "len(tabs)")
check "a second Ladybird view opens while the first is open and loads" \
  "$(sess 40 "seg($second_lb).get('engine')=='ladybird' and seg($second_lb).get('title')=='coexist@127.0.0.3', (segs($second_lb), seg($second_lb).get('title'))")"
close_tab "$second_lb"
cef_i=$(tab_of cef)
close_tab "$cef_i"
check "x closed the CEF tab" "$(dg 5 "len(tabs)==$((n - 1)) and 'cef' not in [x['engine'] for x in tabs], [x['engine'] for x in tabs]")"
lb_i=$(tab_of ladybird)
close_tab "$lb_i"
check "x closed the Ladybird tab" "$(dg 5 "len(tabs)==$((n - 2)) and 'ladybird' not in [x['engine'] for x in tabs], [x['engine'] for x in tabs]")"
sleep 2
info "helper processes left after closing both tabs: $(descendants | while read -r p; do tr '\0' ' ' < "/proc/$p/cmdline" 2>/dev/null; echo; done | grep -E "$HELPERS" | sed 's|/[^ ]*/||' | awk '{print $1}' | sort | uniq -c | tr '\n' ' ')"
wpe_works "after Ladybird's and CEF's helpers stopped" ex

# Reopen both in new tabs (a second start of their helpers), then close again.
step "reopen Ladybird and CEF in new tabs"
for e in ladybird cef; do
  blank_click
  key t
  sleep 2
  key -M ctrl o -m ctrl
  sleep 0.5
  key ":page.open-in $e $(url "$([[ $e == ladybird ]] && echo 3 || echo 4)")"
  key -k Return
  tab=$(value "len(tabs)")
  check "$e opens again in a new tab" \
    "$(sess 60 "seg($tab)['engine']=='$e' and seg($tab).get('title')=='coexist@127.0.0.$([[ $e == ladybird ]] && echo 3 || echo 4)', (segs($tab), seg($tab).get('title'))")"
  check "its view is shown" "$(shows_tab "$tab" "$e")"
  sleep 1.5
  shot "reopened-$e" 0.3
  px=$(python3 "$probe" pixel "$out/reopened-$e.ppm" 1000 500)
  check "$e's page is painted after the reopen" "$(answer "$(near "$px" ${bg[$e]}; echo $?)" "pixel $px")"
done
snap
for t in $(value "len(tabs)") $(($(value "len(tabs)") - 1)); do close_tab "$t"; done
check "the two reopened tabs closed" "$(dg 5 "len(tabs)==$((n - 2)) and 'cef' not in [x['engine'] for x in tabs] and 'ladybird' not in [x['engine'] for x in tabs], [x['engine'] for x in tabs]")"
sleep 2
wpe_works "after the second start and stop of the helpers" ox

# ── Quit ───────────────────────────────────────────────────────────────────────────────────────
step "q"
key -M alt 1 -m alt
sleep 1
blank_click
snap
check "tab 1 is shown for the quit" "$(shows_tab 1)"
t0=$(date +%s.%N)
key q
end=$((SECONDS + 15))
while kill -0 "$sig_pid" 2>/dev/null && [[ $SECONDS -lt $end ]]; do sleep 0.1; done
t1=$(date +%s.%N)
took=$(python3 -c "print(f'{$t1 - $t0:.2f}')")
if kill -0 "$sig_pid" 2>/dev/null; then
  check "q quits within the shutdown bound (5 s, plus the key's own wait)" "1 sig still running after 15 s"
  kill -TERM "$sig_pid"; sleep 3; kill -KILL "$sig_pid" 2>/dev/null
  code=killed
else
  wait "$sig_pid"; code=$?
  check "q quits within the shutdown bound (5 s, plus the key's own wait)" \
    "$(answer "$(python3 -c "import sys; sys.exit(not $took < 6.0)"; echo $?)" "sig exited $took s after q")"
fi
check "sig exits with code 0 and without a forced exit" \
  "$(answer "$([[ $code == 0 ]] && ! grep -qiE 'could not shut down|forced exit|did not return' "$out/sig.log"; echo $?)" "exit code $code; $(grep -ciE 'could not shut down|forced exit|did not return' "$out/sig.log") forced-exit lines")"
sleep 2
left=()
while read -r p st _; do
  [[ -n $p ]] && alive "$p" "$st" && left+=("$p:$(tr '\0' ' ' < "/proc/$p/cmdline" | cut -c1-60)")
done < "$out/procs.txt"
check "no process that was ever in sig's tree is left ($(wc -l < "$out/procs.txt") seen)" "$(answer "$([[ ${#left[@]} == 0 ]]; echo $?)" "${left[*]:-none left}")"
named=$(pgrep -af "$HELPERS" | grep -v 'coexist.sh' | cut -c1-150)
check "no WPE, Ladybird or CEF helper by name is left" "$(answer "$([[ -z $named ]]; echo $?)" "${named:-none}")"
grep -q 'panicked' "$out/sig.log" && check "no panic" "1 see sig.log"
grep -E 'WARN|ERROR' "$out/sig.log" | cut -c1-200 | sort | uniq -c | sort -rn | head -15 > "$out/warnings-summary.txt"
cp "${SIG_SWAY_LOG:-/dev/null}" "$out/sway.log" 2>/dev/null
exec 3>&-
echo "coexist.sh: $failures failed check(s); screenshots and logs in $out"
exit $failures
