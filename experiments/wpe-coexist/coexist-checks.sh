#!/usr/bin/env bash
# WPE WebKit beside Servo and Wasmtime in one process (docs/rewrite/WPE.md, "Coexistence"):
# what the executable exports, which of WebKit's symbols bind into it, and the run itself.
# Prints PASS, FAIL or INFO per check and exits with the number of failures.
#
#   tools/cargo build -p wpe-coexist
#   tools/heavy tools/headless-sway --renderer gles2 -- experiments/wpe-coexist/coexist-checks.sh OUT [BINARY]
#
# BINARY defaults to target/debug/wpe-coexist. Build it with WPE_COEXIST_EXPORT=1 to see the
# executable without `--exclude-libs ALL` (build.rs).
set -u
if [[ -z ${WPE_CHECKS_BUS:-} ]]; then
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
  WPE_CHECKS_BUS=1 dbus-run-session --config-file="$conf" -- "$0" "$@"
  status=$?
  rm -f "$conf"
  exit $status
fi
repo=$(cd "$(dirname "$0")/../.." && pwd)
cd "$repo"
out=${1:?usage: coexist-checks.sh OUT [BINARY]}
binary=${2:-target/debug/wpe-coexist}
mkdir -p "$out"
failures=0
check() { if "${@:2}"; then echo "PASS $1"; else echo "FAIL $1"; failures=$((failures + 1)); fi; }
info() { echo "INFO $*"; }

# What the executable exports from the C libraries linked into it.
readelf --dyn-syms -W "$binary" | awk '$7 != "UND" && NR > 3 && $8 != "" {sub(/@.*/, "", $8); print $8}' |
  sort -u > "$out/exports.txt"
for prefix in hb_ FT_ png_ jpeg_ deflate inflate setenv; do
  info "exports $prefix*: $(grep -c "^$prefix" "$out/exports.txt")"
done
info "exports in all: $(wc -l < "$out/exports.txt")"

# Which symbols WebKit's library and its dependencies bind to in the executable, when the
# dynamic linker resolves everything at load time (the binary exits before main's work).
LD_DEBUG=bindings LD_BIND_NOW=1 LD_DEBUG_OUTPUT="$out/bindings" "$binary" --exit > /dev/null 2>&1
exe=$(basename "$binary")
grep -h "binding file [^ ]* \[0\] to [^ ]*/$exe \[0\]" "$out"/bindings.* 2> /dev/null |
  grep -v "binding file [^ ]*/$exe \[0\]" |
  sed -E 's/.*binding file ([^ ]+) .*symbol `([^'"'"']+).*/\1 \2/' > "$out/into-executable.txt"
rm -f "$out"/bindings.*
info "bindings from shared libraries into the executable: $(wc -l < "$out/into-executable.txt")"
awk '{n = split($1, p, "/"); print p[n]}' "$out/into-executable.txt" | sort | uniq -c | sort -rn | head -12 |
  while read -r count lib; do info "  $lib: $count"; done
webkit=$(grep -c 'libWPEWebKit' "$out/into-executable.txt")
check "no symbol of WebKit's library binds into the executable" test "$webkit" -eq 0

# The run, with a screenshot once the pages show (text in Servo's and WebKit's views).
WPE_COEXIST_HOLD=3 "$binary" > "$out/run.log" 2>&1 &
pid=$!
until grep -q 'holding for' "$out/run.log" || ! kill -0 "$pid" 2> /dev/null; do sleep 0.1; done
sleep 1
grim "$out/engines.png"
wait "$pid"
status=$?
grep -E '^(PASS|FAIL|INFO)' "$out/run.log"
failures=$((failures + $(grep -c '^FAIL' "$out/run.log")))
check "the process exits with 0" test "$status" -eq 0
echo "$failures failed; output in $out"
exit "$failures"
