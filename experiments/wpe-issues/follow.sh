#!/usr/bin/env bash
# Second pass: richer JSC handler probe, and github.com scrolled to the video.
set -u
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
prefix=${SIGHURT_WPE_PREFIX:-$HOME/.local/share/sighurt/deps/wpewebkit-2.52.6}
cc -O2 -o out/jsc-probe jsc-probe.c -ldl
WPE_LIB=$prefix/lib/libWPEWebKit-2.0.so.1 ./out/jsc-probe > out/jsc-probe.out 2> out/jsc-probe.err || echo "jsc-probe exit $?"
echo "-- stdout --"; cat out/jsc-probe.out
echo "-- stderr --"; cat out/jsc-probe.err
export WAIT=8 SCROLL=1
D="GST_DEBUG_NO_COLOR=1 GST_DEBUG=2,uridecodebin:4,webkitmediaplayer:4,glupload:4"
./run.sh ghscroll "https://github.com/" $D
echo "ghscroll invalid=$(grep -c 'invalid option' out/ghscroll.log) a11y=$(grep -c 'a11y bus' out/ghscroll.log)"
grep -E "Fixate return|avdec_|expose_pad" out/ghscroll.log | head -20
