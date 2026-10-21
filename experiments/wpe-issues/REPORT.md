# WPE issues (WebKit 2.52.6)

Investigation only. No product crate was edited. Prefix `~/.local/share/sighurt/deps/wpewebkit-2.52.6`, source the 2.52.6 tarball. Runs: `target/try/sig --engine wpe` inside `tools/headless-sway --renderer gles2` via `engines/sighurt-engine-wpe/tests/private-bus`, throwaway XDG dirs. Local page `pages/video.html` (H.264, VP9, AV1) at `http://127.0.0.1:18766` (`file:` media is unreadable in bubblewrap). Evidence is in `out/`.

## 1. Green video

**Evidence.** `out/local.png`: H.264 (left) and AV1 (right) are solid dark green; VP9 (middle) shows the test pattern; the page clock advances (`9.6 9.5 9.4`). All three use `webkitglvideosink`. Decoders are software: `avdec_h264`, `vp9dec`, `av1dec`. VA-API fails (`nvidia_drv_video.so init failed`); nvcodec fails (`CUDA_ERROR_NO_DEVICE`). `glupload` fixates `video/x-raw(memory:DMABuf), format=DMA_DRM, drm-format=YU12` (I420 fourcc). VP9 then renegotiates `memory:GLMemory, format=I420` and looks right. H.264 and AV1 stay on YU12 DMA-BUF. U=V=0 is green.

github.com (`out/gh.log`, `out/ghscroll.log`): one video, `avdec_h264` into the same YU12 DMA-BUF caps, 2496×1416, bt709, 30 fps. `out/gh.png` and `out/ghscroll.png` are today's CSS hero ("The future of building happens together"). The scrolled capture is still that hero. The green rectangle was not in the viewport on 2026-10-21; the pipeline is the same one that paints green locally.

`WEBKIT_GST_DMABUF_SINK_DISABLED=1` (`out/nodma.png`): all three fixate GLMemory I420. VP9 is fine; H.264 and AV1 are tiled garbage.

`WEBKIT_GST_DISABLE_GL_SINK=1` (`out/nogl.png`): `MediaPlayerPrivateGStreamer.cpp:4282` logs `Disabling hardware-accelerated rendering per user request` three times. All three patterns are correct (clock ~11.4). `createVideoSinkGL` returns null and `createVideoSink` uses `webkitVideoSinkNew()` (system-memory appsink, `MediaPlayerPrivateGStreamer.cpp:4403`).

**Cause.** The GL sink's YU12 DMA-BUF import. Not a missing codec, not NVDEC, not a cmake flag. GStreamer video/GL is the WPE default. Hole-punch is off (`WEBKIT_GST_HOLE_PUNCH_QUIRK` unset).

**Fix.** In `engines/sighurt-engine-wpe/src/runtime.rs` `platform()` (near line 398, where the environment is recorded before the first web view spawns children): if `WEBKIT_GST_DISABLE_GL_SINK` is unset, set it to `1`. Children inherit the process environment. `ProcessLauncherGLib.cpp` uses `GSubprocessLauncher`; its only `setenv` is `SYSPROF_CONTROL_FD`. `WebKitWebContext` has no environment API. Leave a value the user already set.

**Risk.** Each frame is color-converted and copied on the CPU. WebKit warns a sink without `max-lateness` can desync; this fallback is WebKit's own sink and sets that property when it exists. Do not set `WEBKIT_GST_DMABUF_SINK_DISABLED`. No WebKit rebuild.

## 2. A11y bus warning

**Evidence.** `local`, `nodma`, `nogl`, `gh`, and `ghscroll` each log once: `WPEWebProcess: Can't connect to a11y bus: Could not connect: No such file or directory`. `out/a11y.log`, with `WEBKIT_A11Y_BUS_ADDRESS=` empty in the child environ, has no such line. The picture is unchanged.

**Cause.** `OptionsWPE.cmake:422` is `USE_ATSPI TRUE`. `WebProcessPoolGLib.cpp:249`: a non-NULL `WEBKIT_A11Y_BUS_ADDRESS` is the address, and an empty string counts; otherwise the bubblewrap path `unix:path=<runtime>/at-spi-bus` is passed. `private-bus` has no a11y service, so that socket is never created. `AccessibilityAtspi.cpp:56` returns when the address is empty; any other address that fails `g_dbus_connection_new_for_address` warns at line 72. `NO_AT_BRIDGE` is not what this code reads. Sighurt does not implement accessibility.

**Fix.** In the same `platform()` block: if `WEBKIT_A11Y_BUS_ADDRESS` is unset, set it to an empty string before the first view.

**Risk.** Accessibility stays off, which the engine already does not implement. Do not point `AT_SPI_BUS_ADDRESS` at a dummy path; that still tries to connect. Disabling ATSPI in cmake means rebuilding WebKit.

## 3. `ERROR: invalid option: JSC_SIGNAL_FOR_GC=12`

**Evidence.** Each `sig` run logs that line three times, and the child environ dumps contain `JSC_SIGNAL_FOR_GC=12`. `out/jsc-probe.out`: the parent sets the variable, `dlopen`s libWPEWebKit, and calls `jsc_options_set_boolean("usePollingTraps")`. Signal 12 then has a handler (`flags=0x14000004`, `SA_RESTART|SA_SIGINFO`); SIGUSR1 (10) stays `SIG_DFL`. `out/jsc-probe.err` is exactly one error line. After `unsetenv` and `exec`, the child sets the same option, logs no error, and shows both signals still `SIG_DFL`.

**Cause.** `apps/sighurt-browser/src/main.rs` `keep_webkit_off_sigusr1` sets `JSC_SIGNAL_FOR_GC=12` (SIGUSR2) before any thread (decision 78), so WTF does not take SIGUSR1, `sig`'s diagnostics signal. `ThreadingPOSIX.cpp:183` reads it once inside `WTF::initialize` (`Threading.cpp:479`, `once_flag`) and installs `signalHandlerSuspendResume`. It is not a JSC option (`OptionsList.h` has no such name). `Options.cpp:1057` treats every `JSC_*` name as an option and logs `ERROR: invalid option` otherwise. It crashes only if `validateOptions()` is on; it is not. `jscOptionsSetValue` calls `Options::initialize` on the first `jsc_options_set_boolean` (`JSCOptions.cpp:194`), which is `keep_jsc_off_signals` at `runtime.rs:270`. Full `JSC::initialize` (`InitializeThreading.cpp:91`) is `WTF::initialize()` then `Options::initialize()`, so those two cannot be split. The probe child did not install SIGUSR1; it never calls `JSC::initialize`. Web processes that do, with the variable unset, take SIGUSR1, the upstream default, which is the right signal outside `sig`.

**Fix.** Keep the `set_var` in `main.rs`. Immediately after `keep_jsc_off_signals()` returns, and before any web view, `remove_var("JSC_SIGNAL_FOR_GC")`. The UI-process handler is already latched (measured on signal 12; the `once_flag` does not re-read). Children then stop logging the error (measured). `tests/offscreen.rs:361` also sets the variable; this `remove_var` covers runs that go through `WpeRuntime::new`. No 2.52 option name both selects the signal and passes the parser. Silencing the one remaining parent line needs a WebKit change that allowlists `JSC_SIGNAL_FOR_GC` in `Options.cpp`.

**Risk.** Unsetting before `keep_jsc_off_signals` lets WTF take SIGUSR1 and replace `sig`'s diagnostics handler. A process spawned before the `remove_var` still inherits it. `WpeRuntime::new` runs before the first view.
