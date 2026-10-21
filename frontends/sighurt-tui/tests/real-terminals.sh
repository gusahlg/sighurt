#!/usr/bin/env bash
# `sig --tui` in real terminal emulators, Rio and kitty, each in a private headless sway: never on
# the desktop, since it types (wtype) and takes screenshots (grim). The fake terminal of
# sighurt-browser/tests/tui.rs can't tell what a terminal really sends; this can. Each terminal
# gets throwaway XDG directories (its own configuration, sig's configuration, session and log), so
# the user's are never read or written. sig opens https://example.com/ (the network is needed).
#
# Checks, per terminal, PASS or FAIL each:
#   render    example.com is a kitty image: the page rows of a screenshot are mostly not the
#             terminal's background, and Servo presented frames (diagnostics)
#   engine    `e` switches that page to WPE and back to Servo (diagnostics, page focus)
#   navigate  `o`, an address and Enter, typed in one burst, open it: the session file names it
#   newtab    `t` opens a second tab (diagnostics) whose page rows have readable text
#   switch    Escape, then `J` and `K` switch to tab 1 and back to tab 2 (diagnostics)
#   error     an address that can't load shows readable text in the page rows (screenshot)
#   quit      `q` quits with status 0, the terminal's settings are what they were (stty -g before
#             and after, in the same terminal) and the shell's text is back (alternate screen left)
#
#   sighurt-tui/tests/real-terminals.sh OUT               build sig (default features), then run
#   sighurt-tui/tests/real-terminals.sh --no-build OUT    run target/debug/sig as it is
#   sighurt-tui/tests/real-terminals.sh OUT rio           only these terminals (rio, kitty)
#
# Screenshots (*.png), sig's diagnostics and log, and the session files go to OUT/<terminal>; look
# at the screenshots too. Rio is the user's (`rio` on PATH) or the pinned nixpkgs' one; kitty is
# the pinned nixpkgs' one. Exits with the number of failed checks.
set -u
repo=$(cd "$(dirname "$0")/../../.." && pwd)
cd "$repo"

if [[ ${1:-} != --inside ]]; then
  build=1
  [[ ${1:-} == --no-build ]] && { build=0; shift; }
  out=$(realpath -m "${1:?usage: real-terminals.sh [--no-build] OUT [rio|kitty ...]}")
  shift
  terminals=("$@")
  [[ ${#terminals[@]} == 0 ]] && terminals=(rio kitty)
  mkdir -p "$out/bin"
  # Other builds replace target/debug/sig. A copy named sig is what these runs exec, so its
  # process name is sig. Made under the same hold of the machine's lock as the build.
  if [[ $build == 1 ]]; then
    SIG_WHAT="servo build: sig for real-terminal tests" tools/heavy bash -c \
      "cargo build -p sighurt-browser && cp -f target/debug/sig $(printf '%q' "$out/bin/sig")" || exit 1
  else
    cp -f target/debug/sig "$out/bin/sig" || exit 1
  fi
  pkgs=$repo/nix/nixpkgs.nix
  failed=0
  for terminal in "${terminals[@]}"; do
    case $terminal in
      rio) exe=$(command -v rio || echo "$(nix-build "$pkgs" -A rio --no-out-link)/bin/rio") ;;
      kitty) exe="$(nix-build "$pkgs" -A kitty --no-out-link)/bin/kitty" ;;
      *) echo "real-terminals.sh: no terminal $terminal (rio, kitty)" >&2; exit 2 ;;
    esac
    [[ -x $exe ]] || { echo "real-terminals.sh: $terminal not found" >&2; failed=$((failed + 1)); continue; }
    tools/heavy tools/headless-sway --renderer gles2 -- "$0" --inside "$out/$terminal" "$terminal" "$exe"
    failed=$((failed + $?))
  done
  echo "real-terminals.sh: $failed failed check(s); screenshots and logs in $out"
  exit $failed
fi

# ── Inside the private compositor ─────────────────────────────────────────────────────────
out=$2 terminal=$3 exe=$4
[[ -n ${WAYLAND_DISPLAY:-} && $XDG_RUNTIME_DIR == /tmp/sighl.* ]] || {
  echo "real-terminals.sh: run me through tools/headless-sway" >&2
  exit 2
}
rm -rf "$out"
home=$out/home
mkdir -p "$home/config/sighurt" "$home/config/rio" "$home/state" "$home/data" "$home/cache"
export XDG_CONFIG_HOME=$home/config XDG_STATE_HOME=$home/state XDG_DATA_HOME=$home/data
export XDG_CACHE_HOME=$home/cache
# Rio shows a welcome screen instead of the command when it has no configuration.
if [[ -f $HOME/.config/rio/config.toml ]]; then
  cp "$HOME/.config/rio/config.toml" "$home/config/rio/"
else
  echo 'confirm-before-quit = false' > "$home/config/rio/config.toml"
fi
printf 'version = 1\n' > "$home/config/sighurt/config.toml"
sig=$(dirname "$out")/bin/sig
diag=$out/diag.json session=$home/state/sighurt/session.toml
pidfile=$out/sig.pid
failed=0
pass() { echo "PASS $terminal $1"; }
fail() { echo "FAIL $terminal $1: $2"; failed=$((failed + 1)); }

# The terminal runs this: the termios before and after sig, sig's status, then a line to see.
cat > "$out/run.sh" <<RUN
#!/usr/bin/env bash
stty -g > '$out/stty-before'
'$sig' --tui --diagnostics '$diag' https://example.com/ &
echo \$! > '$pidfile'
wait \$!
echo \$? > '$out/status'
stty -g > '$out/stty-after'
echo SIG-EXITED-CLEANLY
sleep 60
RUN
chmod +x "$out/run.sh"
case $terminal in
  rio) "$exe" -e "$out/run.sh" > "$out/terminal.log" 2>&1 & ;;
  kitty) "$exe" --config NONE "$out/run.sh" > "$out/terminal.log" 2>&1 & ;;
esac
term_pid=$!

# Signals this run's sig, by the pid the shell recorded, for a fresh diagnostics report.
report() {
  local pid
  pid=$(cat "$pidfile" 2>/dev/null) || return 1
  rm -f "$diag"
  kill -USR1 "$pid" 2>/dev/null || return 1
  for _ in $(seq 40); do
    [[ -s $diag ]] && python3 -I -c 'import json,sys; json.load(open(sys.argv[1]))' "$diag" \
      2>/dev/null && return 0
    sleep 0.05
  done
  return 1
}
# A field of the last report: a Python expression over `d`.
field() { python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); print(eval(sys.argv[2]))' "$diag" "$1" 2>/dev/null; }
# Polls `cond` (a shell command) every half second for up to $1 seconds.
until_true() {
  local seconds=$1; shift
  for _ in $(seq $((seconds * 2))); do
    "$@" && return 0
    sleep 0.5
  done
  return 1
}
# Screenshot NAME (png, and ppm for the pixel check).
shot() { grim "$out/$1.png"; grim -t ppm "$out/$1.ppm"; }
# The share of the page rows (between 10 % and 90 % of the height) that isn't the terminal's
# background (the bottom-left corner's color), in percent.
covered() {
  python3 -I - "$out/$1.ppm" <<'PY'
import sys
data = open(sys.argv[1], 'rb').read()
fields, at = [], 0
while len(fields) < 4:
    while data[at:at+1].isspace(): at += 1
    if data[at:at+1] == b'#':
        while data[at:at+1] != b'\n': at += 1
        continue
    start = at
    while not data[at:at+1].isspace(): at += 1
    fields.append(data[start:at])
at += 1
w, h = int(fields[1]), int(fields[2])
px = data[at:]
def pixel(x, y):
    i = (y * w + x) * 3
    return px[i:i+3]
bg = pixel(2, h - 2)
total = other = 0
for y in range(h // 10, h * 9 // 10, 4):
    for x in range(0, w, 4):
        total += 1
        other += pixel(x, y) != bg
print(other * 100 // max(total, 1))
PY
}
loaded() { report && [[ $(field 'd["tabs"][d["active_tab"]-1]["loading"]') == False ]]; }
# The active tab's engine, once it is not loading and its view has put a frame on screen.
engine_is() {
  report && [[ $(field 'd["tabs"][d["active_tab"]-1]["loading"]') == False ]] &&
    [[ $(field 'd["tabs"][d["active_tab"]-1]["engine"]') == "$1" ]] &&
    [[ $(field "any(t['engine']=='$1' and t['shown'] and t['presented']>=1 for t in d['targets'])") == True ]]
}
# Non-background pixels in the middle band, where a centered line of text is drawn.
center_ink() {
  python3 -I - "$out/$1.ppm" <<'PY'
import sys
data = open(sys.argv[1], "rb").read()
fields, at = [], 0
while len(fields) < 4:
    while data[at:at+1].isspace(): at += 1
    if data[at:at+1] == b"#":
        while data[at:at+1] != b"\n": at += 1
        continue
    start = at
    while not data[at:at+1].isspace(): at += 1
    fields.append(data[start:at])
at += 1
w, h = int(fields[1]), int(fields[2])
px = data[at:]
def pixel(x, y):
    i = (y * w + x) * 3
    return px[i:i+3]
bg = pixel(2, h - 2)
other = 0
for y in range(h * 4 // 10, h * 6 // 10, 2):
    for x in range(0, w, 2):
        other += pixel(x, y) != bg
print(other)
PY
}
type_keys() { wtype -s 80 "$@"; }

# ── render ──
sleep 2
if until_true 40 loaded && sleep 2 && shot 1-example; then
  frames=$(field 'd["counters"]["page_frames_presented"]')
  share=$(covered 1-example)
  if [[ ${frames:-0} -ge 1 && ${share:-0} -ge 50 ]]; then
    pass "render ($frames frames, $share % of the page rows drawn)"
  else
    fail render "$frames frames, $share % of the page rows drawn"
  fi
else
  fail render "example.com did not finish loading (or sig is not running)"
  shot 1-example
fi

# ── engine ── page focus, before the address line is opened.
wtype -s 40 -k e
if until_true 45 engine_is wpe; then
  sleep 1
  shot 1b-wpe
  wtype -s 40 -k e
  if until_true 45 engine_is servo; then
    sleep 1
    shot 1c-servo
    pass "engine (wpe, then servo)"
  else
    shot 1c-servo
    fail engine "did not return to servo (engine $(field 'd["tabs"][d["active_tab"]-1]["engine"]'))"
  fi
else
  shot 1b-wpe
  fail engine "did not switch to wpe (engine $(field 'd["tabs"][d["active_tab"]-1]["engine"]'))"
fi

# ── navigate ── `o` and the rest in one burst, as fast as a paste of keys.
# wtype rejects -s 0. 20 ms is still one burst, and slow enough that press and release both arrive.
wtype -s 20 o 'https://example.org/' -k Return
if until_true 30 grep -q 'https://example.org/' "$session" && until_true 20 loaded; then
  sleep 1
  shot 2-navigated
  pass "navigate (the session names https://example.org/)"
else
  shot 2-navigated
  fail navigate "the session file never named https://example.org/"
fi

# ── newtab ──
type_keys t
sleep 1
shot 3-newtab
ink=$(center_ink 3-newtab)
if report && [[ $(field 'len(d["tabs"])') == 2 && $(field 'd["active_tab"]') == 2 && ${ink:-0} -ge 30 ]]; then
  pass "newtab (2 tabs, the second active, $ink center pixels drawn)"
else
  fail newtab "tabs $(field 'len(d["tabs"])'), active $(field 'd["active_tab"]'), $ink center pixels"
fi

# ── switch ── `t` left the address line open: Escape gives the keyboard back to the tabs.
type_keys -k Escape
# The physical chord, as Rio and kitty send Shift+J: codepoint j, the shift bit, text "j".
wtype -s 40 -M shift -p j -s 40 -P j -m shift
sleep 1
report && first=$(field 'd["active_tab"]')
shot 4-next
wtype -s 40 -M shift -p k -s 40 -P k -m shift
sleep 1
report && second=$(field 'd["active_tab"]')
if [[ ${first:-} == 1 && ${second:-} == 2 ]]; then
  pass "switch (J: tab $first, K: tab $second)"
else
  fail switch "J: tab ${first:-?}, K: tab ${second:-?}"
fi

# ── error ──
type_keys o https://nonexistent.invalid/ -k Return
if until_true 30 grep -q 'nonexistent.invalid' "$session" && until_true 20 loaded; then
  sleep 1
  shot 5-error
  ink=$(center_ink 5-error)
  title=$(grep -A3 'nonexistent.invalid' "$session" | grep -o 'title = .*' | head -1)
  if [[ ${ink:-0} -ge 30 ]]; then
    pass "error (${title:-no title}, $ink center pixels drawn)"
  else
    fail error "${title:-no title}, only $ink center pixels drawn"
  fi
else
  shot 5-error
  fail error "the session file never named https://nonexistent.invalid/"
fi

# ── quit ──
type_keys q
if until_true 20 test -s "$out/status"; then
  sleep 1
  shot 6-quit
  status=$(cat "$out/status")
  if [[ $status == 0 ]] && cmp -s "$out/stty-before" "$out/stty-after" &&
    [[ $(covered 6-quit) -lt 10 ]]; then
    pass "quit (status 0, termios restored, the shell's screen is back)"
  else
    fail quit "status $status, stty $(cmp -s "$out/stty-before" "$out/stty-after" && echo same || echo differs), $(covered 6-quit) % of the page rows drawn"
  fi
else
  shot 6-quit
  fail quit "sig did not exit"
fi

cp "$home/state/sighurt/tui.log" "$out/tui.log" 2>/dev/null
cp "$session" "$out/session.toml" 2>/dev/null
if [[ -s $pidfile ]]; then
  kill "$(cat "$pidfile")" 2>/dev/null || true
fi
kill "$term_pid" 2>/dev/null
wait 2>/dev/null
exit $failed
