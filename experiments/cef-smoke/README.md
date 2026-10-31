# cef-smoke

Standalone C smoke test for docs/rewrite/CEF.md (not a workspace member): libcef.so opened with
`dlopen`, windowless rendering, one page, one frame written as PPM. `build.sh` builds
`cef-smoke` and `sig-cef-helper` into `/data/sig-builds/deps/cef-150.0.22/dist`.
Run in a private compositor:

    tools/heavy tools/headless-sway --renderer pixman -- env CEF_SMOKE_MT=1 CEF_SMOKE_NOPUMP=1 \
      D/cef-smoke D D/sig-cef-helper https://example.com/ OUT.ppm wayland

`example-frame.png` is the frame it produced.
