#!/usr/bin/env bash
# One locked run: JSC probe, then sig --engine wpe on a local H.264/VP9/AV1 page and on github.com.
# Invoke as: tools/heavy experiments/wpe-issues/probe.sh
set -u
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
mkdir -p out pages
prefix=${SIGHURT_WPE_PREFIX:-$HOME/.local/share/sighurt/deps/wpewebkit-2.52.6}

echo "=== clips ==="
need=0
for f in h264.mp4 vp9.webm av1.mp4; do
  [[ -s pages/$f ]] || need=1
done
if [[ $need == 1 ]]; then
  ./mkclips.sh
fi
ls -l pages

echo "=== jsc probe ==="
cc -O2 -o out/jsc-probe jsc-probe.c -ldl
WPE_LIB=$prefix/lib/libWPEWebKit-2.0.so.1 ./out/jsc-probe > out/jsc-probe.out 2> out/jsc-probe.err || echo "jsc-probe exit $?"
echo "-- stdout --"; cat out/jsc-probe.out
echo "-- stderr --"; cat out/jsc-probe.err

echo "=== http ==="
python3 -m http.server 18766 --bind 127.0.0.1 --directory "$here/pages" > out/http.log 2>&1 &
srv=$!
trap 'kill $srv 2>/dev/null || true' EXIT
sleep 0.4
url=http://127.0.0.1:18766/video.html
D="GST_DEBUG_NO_COLOR=1 GST_DEBUG=2,uridecodebin:5,decodebin:5,webkitmediaplayer:4,webkitglvideosink:5,webkitvideosink:4,glupload:4,glcolorconvert:4"

export WAIT=12
./run.sh local "$url" $D
./run.sh nodma "$url" $D WEBKIT_GST_DMABUF_SINK_DISABLED=1
./run.sh nogl "$url" $D WEBKIT_GST_DISABLE_GL_SINK=1
./run.sh a11y "$url" WEBKIT_A11Y_BUS_ADDRESS=
export WAIT=20
./run.sh gh "https://github.com/" $D

echo "=== summary ==="
python3 - << 'PY'
import pathlib, re, subprocess, collections
out = pathlib.Path("out")
interesting = re.compile(r"invalid option|a11y bus|autoplug|webkitglvideosink|webkitvideosink|vah264|nvh264|nvvp9|nvav1|avdec_|vp9dec|av1dec|dav1d|openh264|caps =|Got message: error|Resource not found|Disabling hardware|DMABUF|dma", re.I)
for name in ["local","nodma","nogl","a11y","gh"]:
    log = out / f"{name}.log"
    if not log.exists():
        print(f"\n## {name}: NO LOG")
        continue
    text = log.read_text(errors="replace")
    print(f"\n## {name}: {text.count(chr(10))+1} lines, invalid={text.count('invalid option')}, a11y={text.count('a11y bus')}")
    envs = list(out.glob(f"{name}.env.*"))
    for e in envs:
        body = e.read_text(errors="replace")
        jsc = [ln for ln in body.splitlines() if ln.startswith("JSC_") or ln.startswith("WEBKIT_A11Y") or ln.startswith("AT_SPI") or "GST_DMABUF" in ln or "DISABLE_GL" in ln]
        print(f"  env {e.name}: {'; '.join(jsc) or '(no JSC/a11y)'}")
    shown = 0
    for ln in text.splitlines():
        if interesting.search(ln):
            # strip gst color leftovers and truncate
            s = re.sub(r"\x1b\[[0-9;]*m", "", ln)
            if len(s) > 240: s = s[:240] + "…"
            print(" ", s)
            shown += 1
            if shown > 40:
                print("  …")
                break
    png = out / f"{name}.png"
    if png.exists() and png.stat().st_size:
        raw = subprocess.run(["ffmpeg","-loglevel","error","-i",str(png),"-vf","scale=16:10","-f","rawvideo","-pix_fmt","rgb24","-"], capture_output=True)
        data = raw.stdout
        if len(data) >= 16*10*3:
            greens = 0
            px = []
            for i in range(0, len(data), 3):
                r,g,b = data[i], data[i+1], data[i+2]
                if g > 60 and g > r + 25 and g > b + 25:
                    greens += 1
                px.append(f"{r:02x}{g:02x}{b:02x}")
            print(f"  png {png.stat().st_size}B greenish-cells {greens}/160 sample {px[20:36]}")
        else:
            print(f"  png ffmpeg failed {raw.stderr[:200]!r}")
PY
echo "=== probe done ==="
