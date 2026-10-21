#!/usr/bin/env bash
# Inside tools/headless-sway only. Runs sig (Servo) under gdb on URL and, once the page's
# script has called a hot function N times, prints SpiderMonkey's JS backtrace
# (js::DumpBacktrace on the JSContext the native was called with) a few times, then kills sig.
# Answers "which page script is Servo's script thread stuck in?" without changing sig.
#
#   gdb-stacks.sh OUT URL [IGNORE] [SIG]
#
# gdb must be sig's parent (kernel.yama.ptrace_scope=1). Throwaway XDG dirs under OUT.
set -uo pipefail
out=${1:?usage: gdb-stacks.sh OUT URL [IGNORE] [SIG]}
url=${2:?usage: gdb-stacks.sh OUT URL [IGNORE] [SIG]}
ignore=${3:-3000}
repo=$(cd "$(dirname "$0")/../.." && pwd)
sig=${4:-${SIG_BIN:-$repo/target/try/sig}}
[[ -n ${WAYLAND_DISPLAY:-} && ${XDG_RUNTIME_DIR:-} == /tmp/sighl.* ]] || {
  echo "gdb-stacks.sh: not inside tools/headless-sway" >&2
  exit 2
}
gdb=$(nix-build "$repo/nix/nixpkgs.nix" -A gdb --no-out-link)/bin/gdb
mkdir -p "$out"/{config/sighurt,state,data,cache}
printf 'version = 1\n' >"$out/config/sighurt/config.toml"
export XDG_CONFIG_HOME=$out/config XDG_STATE_HOME=$out/state XDG_DATA_HOME=$out/data
export XDG_CACHE_HOME=$out/cache
# One breakpoint per hot native seen in the perf profile of the spinning script thread. Each is
# a JSNative or a DOM getter, so its first argument ($rdi) is the JSContext and the JIT has left
# an exit frame that SpiderMonkey's frame iterator can walk from.
cat >"$out/cmds.gdb" <<GDB
set pagination off
set language c
set auto-load safe-path /
set confirm off
set print thread-events off
set breakpoint pending on
handle all nostop noprint pass
set \$n = 0
break _ZL5ErrorP9JSContextjPN2JS5ValueE
ignore 1 $ignore
commands 1
  silent
  set \$n = \$n + 1
  printf "=== JS backtrace at Error() (hit %d) ===\n", \$n
  call (void)_ZN2js13DumpBacktraceEP9JSContext(\$rdi)
  if \$n >= 6
    kill
    quit
  end
  continue
end
break _RINvNtNtNtNtCslNzWGeFoNuX_15script_bindings7codegen15GenericBindings18HTMLElementBinding19HTMLElement_Binding15get_offsetWidthNtNtNtNtNtCs16mJHGhCW3M_6script3dom8bindings7codegen13DomTypeHolder13DomTypeHolderEB2a_
ignore 2 $ignore
commands 2
  silent
  set \$n = \$n + 1
  printf "=== JS backtrace at offsetWidth (hit %d) ===\n", \$n
  call (void)_ZN2js13DumpBacktraceEP9JSContext(\$rdi)
  if \$n >= 6
    kill
    quit
  end
  continue
end
run
GDB
echo "gdb-stacks: $sig $url ignore=$ignore"
timeout 240 "$gdb" -q -batch -x "$out/cmds.gdb" --args "$sig" --gui --engine servo "$url" \
  >"$out/gdb.log" 2>&1
echo "gdb-stacks: exit $?"
grep -c "=== JS backtrace" "$out/gdb.log"
