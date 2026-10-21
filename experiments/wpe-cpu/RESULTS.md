# WPE without a compositor (`wpe-cpu`)

Measured on louise-pc (`hostname` `louise`) on 17 October 2026. WPE WebKit 2.52.6 from
`~/.local/share/sighurt/deps/wpewebkit-2.52.6`. The binary is a debug build
(`target/debug/wpe-cpu` in `~/repos/sighurt-rewrite`). Every run below exited 0 and printed
`wpe-cpu: PASS`.

WPE's `SighurtWPEDisplay` with no DMA-BUF device delivers changing `WPEBufferSHM` frames when
`WAYLAND_DISPLAY` and `DISPLAY` are unset and the UI process has no Wayland socket. The web
process still renders with GL and reads pixels back into shared memory. JavaScript in an isolated
world works. WebKit's web and network processes exit after the same teardown order as
`sighurt-engine-wpe`. The frame rate follows how fast the UI process releases buffers: releasing
immediately produced about 1800 frames/s, and holding one buffer unacknowledged stopped further
frames for the rest of a 2 s window.

## Setup

`tools/heavy` runs the command in this repo's nix-shell (so `LD_LIBRARY_PATH`,
`GIO_EXTRA_MODULES`, `GST_PLUGIN_SYSTEM_PATH_1_0` and `SIGHURT_WPE_PREFIX` come from `shell.nix`).
`engines/sighurt-engine-wpe/tests/private-bus` starts `dbus-run-session` with a temporary config and no
service activation. The pages are `experiments/wpe-cpu/pages/animated.html` (CSS bar, 0.8 s
alternate, background `#202020`) and `second.html` (background `#102010`, two headings under the
`h1`, two links, two inputs).

```sh
SIG_WHAT="wpe-cpu run" tools/remote --no-sync tools/heavy \
  env -u WAYLAND_DISPLAY -u DISPLAY \
  engines/sighurt-engine-wpe/tests/private-bus \
  env WEBKIT_DEBUG=PlatformDisplay,Compositing LIBGL_DEBUG=verbose \
  target/debug/wpe-cpu \
  --size 640x480 --seconds 3 --resize 400x300 --out /tmp/wpe-cpu-a \
  file://$HOME/repos/sighurt-rewrite/experiments/wpe-cpu/pages/animated.html \
  --next file://$HOME/repos/sighurt-rewrite/experiments/wpe-cpu/pages/second.html
```

The program sets `JSC_SIGNAL_FOR_GC=12` and, before any thread, JavaScriptCore's
`usePollingTraps=true` and `useWasmFaultSignalHandler=false`. Stderr of each run contains
`ERROR: invalid option: JSC_SIGNAL_FOR_GC=12` (once per process) and
`Unknown logging channel: PlatformDisplay` / `Compositing`. This prefix's logging build does not
have those channels. The JavaScript checks still passed. The web process also logs
`Can't connect to a11y bus: Could not connect: No such file or directory`, which is the private
bus with no accessibility service.

`XDG_RUNTIME_DIR` was `/run/user/1000`. `WAYLAND_DISPLAY` and `DISPLAY` were empty. The host DRM
node is `card1:nvidia` (no `card0`). `/dev/dri/renderD128`, `/dev/nvidia0` and `/dev/nvidiactl`
opened in the UI process when they were left visible.

PNGs are written to the `--out` directory on louise-pc (`first.png`, `last.png`, `resized.png`).
The encoder uses zlib stored blocks, so two frames of the same size have the same file length
(1 229 438 bytes at 640×480, 480 403 bytes at 400×300). The pixel comparison is the in-memory
one the program prints (`animate-diff`). The encoder reads each file back and checks the pixels
before the run is allowed to pass.

## Case a — no display

| | |
|---|---|
| First SHM frame | 353.5 ms |
| Window | 5403 frames in 3.000 s, 1800.96 frames/s, all 640×480 |
| Copied frames | 5404 (none stale) |
| Copy | mean 0.082 ms, max 0.803 ms, total 441.263 ms |
| Pixels different, first vs last | 118 946 |
| Samples | both frames `#202020ff` at the top-left, centre and bottom-right (the bar was elsewhere; the count above is the difference) |
| Navigate | `LOAD_FINISHED`, uri `.../second.html`, `finishes=2`, 8 frames of 640×480 during the load |
| Resize | one 400×300 frame, 7 stale 640×480 frames acknowledged. `resized.png` corners `#102010ff` (the second page's background), centre `#ffffffff` |
| Isolated world `sighurt` | `{"title":"wpe-cpu second","headings":3,"links":2,"inputs":2,"first_link_href":"file:///home/gusahlg/repos/sighurt-rewrite/experiments/wpe-cpu/pages/animated.html"}` |
| Isolated probe | `{"marker":null,"dom_title":"wpe-cpu second"}` — the page set `window.__sighurtPage` in the main world; the isolated world does not see it and does see the DOM |
| Main world | `{"marker":"page-world"}` |
| Throttle (2.000 s, no rendered/released) | produced 1, acknowledged 0, held 1, last frame at 0.0 ms, cap of 48 not hit |
| After shutdown | web and network processes gone (`web-and-network-gone=yes`), `reaped=0`, `glib_dispatches=5541` |

The UI process (`wpe-cpu`) mapped `libgbm`, libglvnd's `libEGL` / `libGLX` / `libGLdispatch` and
`libdrm`, and its interesting fds were empty: no Wayland socket, no `/dev/dri`, no NVIDIA device.
The web process mapped `libEGL_nvidia.so.595.80`, `libnvidia-eglcore`, `libnvidia-glsi`,
`libnvidia-gpucomp`, `libnvidia-allocator`, `libnvidia-egl-gbm`, the NVIDIA EGL Wayland/X11
platform libraries, `libEGL_mesa.so.0.0.0`, `libgbm`, `libGLESv2` and `libdrm`, and held fds for
`/dev/dri/renderD128`, `/dev/nvidia0` and `/dev/nvidiactl`. No Wayland socket. Frames were still
`WPEBufferSHM` (a DMA-BUF frame fails the run). So the web process used NVIDIA's EGL on the RTX
4060 and copied into shared memory. `WEBKIT_DEBUG` could not name the renderer string; the maps
and fds are the evidence. `LIBGL_DEBUG=verbose` printed nothing from this NVIDIA path.

A separate pacing check, same page and machine, before the loop waited on GLib's watch thread
(it slept up to 50 ms between pumps): 59 frames in 3.001 s, 19.66 frames/s, startup 562.2 ms,
copy mean 0.188 ms, 304 159 pixels different. The last frame's centre was `#9e3961ff` (between
the bar's `#ff3300` and `#0044ff`) on a `#202020ff` background. Delivery followed the UI loop.

## Case b — `/dev/dri` hidden, no root

`unshare --user --map-root-user --mount` succeeded. Inside that mount namespace:

- `mount -t tmpfs tmpfs /dev/dri` exited 0 and left `/dev/dri` empty. `open("/dev/dri/renderD128")` returned `No such file or directory (os error 2)`.
- `mount --bind` of an empty file onto `/dev/nvidia0` and `/dev/nvidiactl` exited 0. `open` of those paths still returned success, because the replacement is a regular file.

Bubblewrap started. The run passed: 4416 frames in 3.000 s (1471.99 frames/s), startup 1603.2 ms,
copy mean 0.060 ms, 120 480 pixels different. First centre `#fe3301ff` (the bar's `#ff3300`),
last centre `#703d8fff`, corners `#202020ff`. Navigation, the 400×300 frame, the isolated-world
JSON and shutdown matched case a. Throttle: produced 1, acknowledged 0, last frame at 0.1 ms.

The web process's interesting fds were empty (no render node, no NVIDIA device path, no Wayland
socket). Its maps still included `libEGL_nvidia.so.595.80` and the rest of the NVIDIA EGL stack,
plus `libEGL_mesa.so.0.0.0` and `libgbm`. They did not include `llvmpipe`, `swrast` or `iris`.
`/dev/nvidia0` and `/dev/nvidiactl` were not mapped, unlike case a. Stderr gained Mesa's
`libGL: Can't open configuration file /etc/drirc` lines, which case a did not print. Frames
continued with the render node gone. Which of the two mapped EGL libraries rasterised the pixels
is not identified.

## Case c — `LIBGL_ALWAYS_SOFTWARE=1`

The variable was set in the UI process and was present in the web process environment. The web
process still mapped `libEGL_nvidia.so.595.80`, still held `/dev/dri/renderD128`, `/dev/nvidia0`
and `/dev/nvidiactl`, and did not map `llvmpipe` or `swrast`. 5392 frames in 3.000 s, 1797.21
frames/s, startup 343.5 ms, copy mean 0.081 ms, 116 067 pixels different. Against case a's
1800.96 frames/s, the variable did not move this machine off NVIDIA's EGL and did not change the
delivery rate. NVIDIA's driver does not treat `LIBGL_ALWAYS_SOFTWARE` as Mesa does.

## Shutdown

The order is the engine's: acknowledge every buffer still held, clear the view sink, drop the
WPE view and `WebKitWebView`, pump GLib, drop the non-default `WebKitWebContext` (cache model
`WEBKIT_CACHE_MODEL_DOCUMENT_BROWSER`, set on the default context before `webkit_web_context_new`),
pump, drop the ephemeral network session and the display, pump, drop the GLib context. The web
process, its bubblewrap parents (their command lines contain `WPEWebProcess`) and
`WPENetworkProcess` were gone within the 8 s poll. `waitpid(WNOHANG)` reaped nothing; those
processes had already left the process table. The network session is ephemeral, so the run does
not take the profile lock `sig` uses.

## For the WPE CPU presenter (phase 3)

- `Display::new(None, …)` is enough. The presenter does not need a Wayland lease, a
  `WAYLAND_DISPLAY` socket, or a DRM node advertised to WebKit. Frames arrive as
  `WPEBufferSHM`, `WPE_PIXEL_FORMAT_ARGB8888`, and `wpe_buffer_shm_get_data` is a CPU copy
  (about 0.08 ms for 640×480 in this debug build).
- There is no `WPEScreen`, and there was no 60 Hz cap. Releasing each buffer as it arrived
  produced ~1800 frames/s on the 4060. Sleeping ~50 ms between pumps produced 19.66 frames/s.
  Holding one buffer without `wpe_view_buffer_rendered` / `wpe_view_buffer_released` yielded one
  frame and then silence for 2 s (`produced=1`, `acknowledged=0`). The presenter sets the rate
  by when it acknowledges, which is the throttle in TUI.md §8.6. Count produced, copied and
  acknowledged separately, and acknowledge at the terminal's frame budget.
- The web process still initialises NVIDIA EGL and opens the render node when it can. Hiding
  `/dev/dri` did not stop SHM frames. A CPU presenter on a machine with a GPU will still have a
  GL web process unless something further forces a software rasteriser. `LIBGL_ALWAYS_SOFTWARE=1`
  did not do that here.
- Isolated-world `webkit_web_view_evaluate_javascript` with `world_name` `"sighurt"` sees the DOM
  and not the page's JavaScript globals. DOM extraction can run there.
- Tear the view down before the web context, then the network session and the display, and pump
  GLib between those drops. The web and network processes then exit. `JSC_SIGNAL_FOR_GC=12`
  belongs in the environment before JavaScriptCore starts; the "invalid option" line is expected.

## Not run

Servo was not linked into this binary, and `SoftwareRenderingContext` was not created before
WebKit. Whether Servo's process-wide `LIBGL_ALWAYS_SOFTWARE` / `DRI_PRIME` changes (TUI.md §8.1)
break or retarget this SHM path is unmeasured.
