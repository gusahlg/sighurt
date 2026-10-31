# CEF (Chromium) for Sighurt: feasibility, design and smoke test

Status: **implemented** (sections 1 to 7 are the study and design the engine followed; section 8
says what was built, what changed against the design, and the measurements). `sighurt-engine-cef`
is a Chromium engine compiled into `sig` behind the feature `engine-cef` (in no default feature
set), added the way Servo and WPE WebKit are (docs/rewrite/WPE.md is the model).

**Verdict: go, with CEF 150.0.22 and `multi_threaded_message_loop`.** The smoke test
(`experiments/cef-smoke`) loaded https://example.com/ windowless through the C API, with
`libcef.so` opened by `dlopen`, and received `OnPaint` BGRA frames. Two findings change the
obvious design: the external message pump does not work well (use CEF's own UI thread instead),
and CEF 152 to 156 minimal distributions did not start here (150 does).

## Dependency location

Big files stay off the SSD: **`/data/sig-builds/deps/`** (desktop HDD).

```
/data/sig-builds/deps/cef/cef_binary_150.0.22+gc1a097e+chromium-150.0.7871.256_linux64_minimal.tar.bz2   (300 MiB)
/data/sig-builds/deps/cef/index.json                 (the CEF builds index, for checksums)
/data/sig-builds/deps/cef-150.0.22/                  the unpacked distribution (2.8 GB with dist/)
/data/sig-builds/deps/cef-150.0.22/dist/             the runtime directory (see "Layout")
/data/sig-builds/deps/cef-152.0.15/                  tried and rejected; deletable (re-download recreates it)
```

`nix/cef.nix` (new, written for this task) names that prefix and lists the libraries; a
non-HDD machine (louise-pc) needs the same path or an override (`SIGHURT_CEF_PREFIX`), as with the
WPE prefix's "same home directory" constraint.

## 1. Version, source, checksum

| | |
|---|---|
| Pin | **CEF 150.0.22+gc1a097e+chromium-150.0.7871.256**, linux64 `minimal` |
| URL | `https://cef-builds.spotifycdn.com/cef_binary_150.0.22%2Bgc1a097e%2Bchromium-150.0.7871.256_linux64_minimal.tar.bz2` |
| sha1 (from index.json) | `7e7ca1e73e5d5e6317ca53dc1302fd180a3e1222` (verified) |
| sha256 (pinned) | `dd1b5cc3ac6efe659ec7e8e9b0677ba4e43216a2fefcafd5abc9340837e32d96` |
| Size | 314 890 058 bytes archive; `libcef.so` 1.4 GB (not stripped of debug-less symbols; only 243 `cef_*` symbols exported), 2.8 GB with the runtime directory |
| Released | 9 October 2026, stable channel |
| API version | `CEF_API_VERSION 15000` (build with it defined; the headers default to "experimental", whose hash never matches a release) |

Why not newer. 152.0.15 (21 Oct), 154.0.34 and 156.0.14 are newer "stable" builds. 152.0.15 was
downloaded and tried with the same program, settings and environment: `cef_initialize` returned 0
after 50 ms with `ContentMainRun failed with exit code 28` in every configuration tried (Wayland and
headless ozone, GPU on, `--disable-gpu`, `--in-process-gpu`, `--use-angle=swiftshader`,
`--single-process`, sandbox on and off, with and without a cache directory, GSettings schemas
provided, a private D-Bus session, libEGL/libGLESv2 copied in from 150). 150.0.22 started at once
with the same code. Differences seen: 150's `Release/` has `libEGL.so` and `libGLESv2.so`; 152,
154 and 156 do not (ANGLE is presumably linked into libcef). The cause of 28 is not found (it is
not a Chromium `ResultCode` that CEF lists; no log line explains it). Treat every version bump as
a re-run of the smoke test; an upstream CEF bug report with `cef-smoke` is the way forward.
A worker bumping the pin must first reproduce a working `cef-smoke` run.

## 2. The prefix, as a reproducible dependency

Same scheme as WPE: a plain directory outside the Nix store, same absolute path on both machines,
its libraries from the pinned nixpkgs (`nix/nixpkgs.nix`), a GC root for them.

- **No build.** The binary distribution is fetched and unpacked (`tar -xjf --strip-components=1`,
  30 s). `nix/build-cef.sh` downloads if absent, checks sha1 and the pinned sha256, unpacks,
  assembles `dist/`, patches, roots `runtime`, and prints a manifest. A second run whose stamp
  matches does nothing.
- **What libcef needs** (`readelf -d`, 30 `DT_NEEDED`): glib, gobject, gio, nspr4, nss3, nssutil3,
  smime3, atk, atk-bridge, atspi, dbus-1, cups, X11, Xcomposite, Xdamage, Xext, Xfixes, Xrandr, xcb,
  gbm, expat, xkbcommon, cairo, pango, udev, asound, plus libc, libm, libgcc_s. Chromium also
  `dlopen`s libGL/EGL/Vulkan drivers and fontconfig data. `nix/cef.nix` lists the nixpkgs inputs
  (`libs`), its `runtime` attribute is the GC-root text file, and `shell` has patchelf and gcc.
  With them `ldd libcef.so` has no "not found" (74 libraries) in a clean environment.
- **Patching.** `patchelf --set-rpath '$ORIGIN:<store lib dirs>' dist/libcef.so` (takes seconds on
  the 1.4 GB file; the file is copied, never patched in `Release/`). The helper is built in the
  nix shell and links `dist/libcef.so` with an `$ORIGIN` rpath. So no `LD_LIBRARY_PATH` is needed
  except that `shell.nix` sets one anyway (its entries are the same store paths).
  Pitfall found: the interactive shell's ambient `LD_LIBRARY_PATH` on this machine names an
  alsa-lib built against a newer glibc (2.44) than the pin's (2.42); run test programs under
  `tools/heavy` (whose shell replaces it) or with `env -u LD_LIBRARY_PATH`. Both of this
  project's sandboxes (WebKit's bwrap, Chromium's) pass the environment through.
- **Layout (`dist/`)**, everything Chromium finds relative to `libcef.so`/the helper:
  `libcef.so`, `libEGL.so`, `libGLESv2.so`, `libvk_swiftshader.so`, `libvulkan.so.1`,
  `vk_swiftshader_icd.json`, `v8_context_snapshot.bin`, `icudtl.dat`, `resources.pak`,
  `chrome_100_percent.pak`, `chrome_200_percent.pak`, `locales/`, and `sig-cef-helper`
  (hard links into the unpacked tree, except `libcef.so`). `chrome-sandbox` (setuid helper) is not
  used; see Sandbox. Settings point `resources_dir_path` and `locales_dir_path` at `dist/` and
  `browser_subprocess_path` at `dist/sig-cef-helper`.
- **Pinned nixpkgs interplay.** Moving the nixpkgs pin changes the libs' store paths: re-run
  `nix/build-cef.sh` to re-patch (like WPE's rebuild). CEF itself does not change.
- **Cost** (desktop, this task): download 16 s, unpack 30 s, patch 7 s, smoke build 1 s; disk
  2.8 GB per version; first load of `libcef.so` from the HDD took 4.4 s of the first run (cold
  page cache), 0.75 s warm.
- **Licence**: BSD-3 for CEF, Chromium's mixed licences (`CREDITS.html`, 19 MB, must ship with a
  distribution of `sig` that includes it). Compatible with AGPL for linking at run time through
  dlopen; the engine crate is `AGPL-3.0-or-later`. No codecs: the CEF builds are Chromium without
  proprietary codecs (no H.264/AAC), a known limit to document.

## 3. Smoke test: what was proved

`experiments/cef-smoke/{smoke.c,helper.c,build.sh}`; 500 lines of C against `include/capi/`
only. `smoke` opens `libcef.so` with `dlopen(RTLD_NOW|RTLD_LOCAL)`, checks `cef_api_hash`,
calls `cef_initialize` (`windowless_rendering_enabled=1`, `disable_signal_handlers=1`,
`no_sandbox=0`, `browser_subprocess_path=sig-cef-helper`), creates one windowless browser, loads
https://example.com/, writes the first complete frame as PPM (`example-frame.png` is that frame:
the real page, all scripts, 1280 x 800, `OnPaint` BGRA) and shuts down. The browser process never
links CEF; the helper links it (it exists only to call `cef_execute_process`).

Run: `tools/heavy tools/headless-sway --renderer pixman -- env CEF_SMOKE_MT=1 CEF_SMOKE_NOPUMP=1
<dist>/cef-smoke <dist> <dist>/sig-cef-helper https://example.com/ OUT.ppm wayland`
(desktop, RTX 3070, 31 October 2026, 150.0.22, debug-less C, sandbox on).

| Measurement | Result |
|---|---|
| `dlopen` of libcef.so | 13-27 ms warm; +55 MB RSS (page-ins of relocations); 1.1 s to 4.4 s from a cold HDD cache |
| `cef_initialize` call | 47-58 ms warm (374 ms and 2.0 s on cold runs) |
| Browser created / context initialized | 65-75 ms after start (warm) |
| First `OnPaint` / load finished (example.com) | **157-268 ms / 256-259 ms** after start (warm; 750 ms with a cold run); title reported once at about 220 ms |
| Frame | `OnPaint` 1280 x 800 BGRA, full page, `PET_VIEW` |
| Process tree after load | 10 processes (browser, zygote x2, GPU, GPU broker, network utility, storage-ish utility, renderer, ...) |
| RSS / PSS of the tree | **1.06-1.16 GB RSS, 548-567 MB PSS** (own process: 285-292 MB RSS, 202 MB PSS; renderer 250 MB; GPU process 100-130 MB) |
| Owner-loop wakeups (MT loop) | 3 iterations in a 3.5 s run, all of them the test's own 1 s timeouts; **0 `OnScheduleMessagePumpWork` calls, 0 `cef_do_message_loop_work` calls** |
| Shutdown | `close_browser(force)` then `cef_shutdown`: 36-60 ms |
| Sandbox on (`no_sandbox=0`, no setuid, user namespaces) | worked, `max_user_namespaces` = 63130; the zygote used `--no-zygote-sandbox` plus the namespace sandbox. Not verified: which of Chromium's layer-1/2 sandboxes were active (needs `chrome://sandbox`) |
| Signals | `disable_signal_handlers = 1` honoured; with 0 it also started |
| Symbols | libcef exports exactly 243 symbols, all `cef_*`: no clash possible with `sig`'s libraries; its allocator is not interposed |

**What it did not prove** (open, each a step in the plan): accelerated paint (below);
`sig`'s `--exclude-libs` link with CEF loaded beside Servo and WebKit (nothing in `sig` was
touched); input, IME, clipboard, downloads, popups, permissions (APIs read from the headers, not
run); a crash of the GPU or renderer process; headless without any display (see 4.1).

### The message pump finding

The brief assumed `external_message_pump = true` driven by `OnScheduleMessagePumpWork`. On 150:

| Mode | Result |
|---|---|
| External pump, schedule requests only | `cef_initialize` returns, the context initializes, `OnScheduleMessagePumpWork` is called **once** and then never again in 30 s: the browser is never created. A `cef_do_message_loop_work` after `create_browser` did not help |
| External pump + a bounded re-arm timer after each slice (cefclient's practice; 30 ms or 1000 ms) | works, but first paint at 2.0-2.3 s (against 0.27 s) and 91-161 schedule calls; with 30 ms the loop spun (110 000 iterations in 4 s) in the smoke program's naive loop |
| **`multi_threaded_message_loop = 1`, no pump** | works, first paint 0.27 s, no pump calls, no ticks |

So the engine uses CEF's own UI thread (`multi_threaded_message_loop = 1`;
`external_message_pump = 0`; the owner never calls `cef_do_message_loop_work`). This is what the
"engines must not tick" rule wants: every CEF callback runs on CEF's UI thread, pushes an event
into a mutex-guarded queue, and calls `EngineWaker::wake()`; `pump` drains the queue on the owner
thread. Costs: CEF's UI thread is a second thread running Chromium's UI-process code (the WPE
engine ran WebKit's UI half on the owner thread; here that is not possible without the pump);
CEF API calls from the owner thread are legal from any thread for the browser host/browser
objects (they are posted to the UI thread internally), but handler callbacks must be `Send`-safe
queues, and `cef_initialize`/`cef_shutdown` must run on the thread that calls them in the same
order (do both on the owner thread; shutdown blocks until CEF's threads end, tens of ms here).
The trait-level rule "never add an unsafe `Send` impl" holds: the adapter's shared state is an
`Arc<Mutex<..>>` of plain data.

### Chromium rewrites argv

After `cef_initialize` the strings `argv[i]` run together (`"wayland"` became `"wayland accel"`):
Chromium overwrites the argv area to set the process title. The adapter (and `sig`'s
`main`, which keeps `std::env::args()` copies, so it is unaffected: Rust copies argv at start)
must never keep raw `argv` pointers; the smoke test now `strdup`s. Also check `/proc/self/cmdline`
users (diagnostics) after CEF started.

## 4. Integration design

### 4.1 Process model and the helper

- **`sig-cef-helper`**: a separate tiny binary (`engines/sighurt-engine-cef/helper/`, a bin target
  of the engine crate or its own crate, built by the same cargo invocation) that links
  `libcef.so` and calls `cef_execute_process(args, NULL, NULL)` (and checks `cef_api_hash`).
  `sig`'s `main` has no CEF code; the engine's `CefConfig` carries the helper's path (found next to
  `sig` or in the prefix's `dist/`, installed by `nix/build-cef.sh`). It is the renderer, GPU and
  utility process. Because it has no `app`, the render-process side is the stock one; if
  per-process setup is ever needed (custom schemes, a V8 extension) it gets an `app` here, never
  in `sig`.
- **One CEF runtime per process** (a CEF rule: `cef_initialize` once, `cef_shutdown` at the end,
  no re-initialization). `CefRuntime::new` on the first CEF page (like WPE's decision 84); a
  session without CEF pages maps no CEF.
- **Display**: Chromium runs with `--ozone-platform=headless` and SwiftShader
  (`use-angle=swiftshader`, `enable-unsafe-swiftshader`). It opens no Wayland or X11 connection.
  A session with `WAYLAND_DISPLAY` and `DISPLAY` unset still starts the engine: the GUI, the TUI
  and the offscreen test all do. Without `--ozone-platform` it used to pick Wayland when
  `WAYLAND_DISPLAY` was set, and headless ozone without SwiftShader failed EGL init
  (`Could not open the default X display`). There is no GPU rendering. `--dump` still refuses
  the engine, because it has no semantics. CPU frames are presented, so a CPU host is supported.
- **No second display connection**: the browser process does not open Wayland, does not share
  `sighurt-wayland`'s connection, and creates no surface. Frames arrive through `OnPaint`.

### 4.2 C API and loading

- **FFI**: hand-written like `sighurt-engine-wpe::ffi` (decision: no bindgen at build time; it
  would need libclang and the CEF headers on every build machine, and the surface used is about
  35 structs and 12 functions). Written from `include/capi/*.h` of the pinned version, struct
  layouts mirrored with `size` fields, pointers to handler structs built by a small
  `ref_counted!` macro (the C API's `cef_base_ref_counted_t`: a counter with `add_ref`,
  `release`, `has_one_ref`, `has_at_least_one_ref`; the smoke test used no-op refcounts on static
  objects, the adapter needs real ones, `Arc`-backed, since views are created and destroyed).
- **Loading**: `ffi::load(path)` does `dlopen(RTLD_NOW | RTLD_LOCAL)` on `dist/libcef.so`, then
  `dlsym`s 20-odd `cef_*` functions into a table (the smoke test does this for 6). Measured
  cost: 13-27 ms warm, +55 MB RSS. First call is `cef_api_hash(CEF_API_VERSION, 0)` compared
  with the hash compiled into the crate (`210767725a6feb2e4becd3956b648cab6a006712` for 15000);
  mismatch is `RuntimeFailed`. `build.rs` finds the prefix via `SIGHURT_CEF_PREFIX` (declared
  with `rerun-if-env-changed`, never `PKG_CONFIG_PATH`), checks that `include/cef_version.h`
  matches the pin, and hands the library path to the crate as `SIGHURT_CEF_LIBRARY`.
- **Consequence for `-Wl,--exclude-libs,ALL`** (decision 65): libcef is not linked into `sig`,
  so the flag is irrelevant to it. libcef's own exports are `cef_*` only (243) and, loaded
  `RTLD_LOCAL`, do not enter the global scope. Chromium statically links its own copies of
  zlib, libpng, FreeType, HarfBuzz, BoringSSL, ICU, V8, SQLite; they are hidden (not exported), so
  they cannot bind to `sig`'s Servo/Wasmtime libraries, and `sig`'s do not bind to them.
  Verify as the WPE proof did (`LD_BIND_NOW=1`, count bindings from libcef's address range).
  **Allocator**: Chromium uses its own allocator inside libcef.so (PartitionAlloc shim symbols
  are hidden); it does not interpose `malloc` for `sig`. Confirm by `ltrace`-free means: `nm -D`
  shows no `malloc`/`free`/`operator new` exports (checked: none).
- **Signals**: `disable_signal_handlers = 1` stops CEF installing its crash/SIGINT/SIGTERM
  handlers in `sig`'s process (the smoke test also ran with 0). Chromium still installs SIGCHLD and
  SIGPIPE handling for its process launcher: the plan's step 8 checks `/proc/self/status`
  `SigCgt` before and after `cef_initialize` and records the diff; SpiderMonkey (Servo) and
  Wasmtime (SIGSEGV/SIGBUS traps) and the existing JSC_SIGNAL_FOR_GC=12 choice must stay
  untouched. V8 (in the CEF *renderer*, another process) does not run in `sig`'s process: only
  the browser-process half of Chromium does, so V8's signal use is not a clash.
- **Threads**: CEF creates its own (UI, IO, ThreadPool, network, D-Bus, about 40 in the browser
  process). The browser process's "UI thread" is not the owner thread (3.3).

### 4.3 Mapping onto `EngineRuntime`

`CefRuntime` (module `runtime`) holds the `CefApi` table, `Arc<Shared>` (event queue, per-view
state, `EngineWaker`) and the main-thread-only views map. Views are windowless browsers created
with `cef_browser_host_create_browser` (asynchronous: `on_after_created` hands over the
`cef_browser_t`; `open_view` returns `Ok` at once, `ViewReady` is sent from `on_after_created`
plus the first `get_view_rect` answer). Everything below is queued from CEF's UI thread.

| Contract | CEF |
|---|---|
| `open_view(view, options, metrics)` | `cef_browser_host_create_browser`, `windowless_frame_rate = 60`, `windowless_rendering_enabled`, a per-view `cef_client_t` with render, life-span, load, display, request, download, permission, keyboard, focus, context-menu, jsdialog, dialog handlers. `get_view_rect` answers from the view's `Metrics` (logical size); `get_screen_info` from its scale (`device_scale_factor` = `Scale` / 120 exactly representable only for the host's rational scales: send `ScreenInfo.device_scale_factor` as `f32` of the rational and derive buffer sizes from the *received* `OnPaint` width/height, never from the float) |
| `Navigate`, `Reload`, `Stop`, `Back`, `Forward` | `frame->load_url`, `browser->reload`, `stop_load`, `go_back`, `go_forward` (all asynchronous; `can_go_back`/`can_go_forward` from `on_loading_state_change`) |
| Navigation events (`NavigationStarted`, committed, failed) | `cef_load_handler_t::on_load_start` (transition type), `on_load_end`, `on_load_error` (`error_code`, failed URL; `ERR_ABORTED` = `Cancelled`); the document identity: the main frame's `cef_frame_t::get_identifier` plus a counter incremented at `on_load_start` of the main frame; history from `cef_navigation_entry` through `browser->get_host()->get_navigation_entry` visitor (async; take it at `on_loading_state_change`). Same-document navigation: `cef_display_handler_t::on_address_change` without a load start |
| Title, URL, loading, status, favicon, cursor | `on_title_change`, `on_address_change`, `on_loading_state_change`, `on_status_message`, `on_favicon_urlchange` (the icon bytes are not fetched in step 1), `on_cursor_change` (type enum; custom cursors give the image, ignored) |
| New window / `window.open` / target=_blank | `cef_life_span_handler_t::on_before_popup` -> `OpenRequested { new tab, url }`, then return 1 (cancel); *never* let CEF create a popup browser. `user_gesture` arg feeds the core's 5 s click rule |
| `NavigationRequested` / `AnswerNavigation` | `cef_request_handler_t::on_before_browse` (has `user_gesture`, `is_redirect`, transition type): for a user-clicked link (`TT_LINK`, gesture) return 1 (cancel) and queue `NavigationRequested`; on an "ours" answer `load_url` again with a one-shot allow flag (the held-navigation model of the contract becomes cancel-and-replay, since CEF has no "hold"). Limit 16 per view; the page cannot tell. Replay loses a POST body (only GET, as WPE) |
| Frames: `OnPaint` | `PET_VIEW` full-frame BGRA (premultiplied) with dirty rects. See 4.4 |
| Input: pointer, wheel, keys | `cef_browser_host_t::send_mouse_move/click/wheel_event`, `send_key_event` (`cef_key_event_t`: `windows_key_code` (VKEY from the evdev code via a table), `native_key_code` = evdev+8, `character`, `unmodified_character`, `modifiers`, type `KEYEVENT_RAWKEYDOWN`/`KEYEVENT_CHAR`/`KEYEVENT_KEYUP`; a char event for each `text`). AltGr is never Ctrl+Alt (contract); modifiers from the host. `SetFocus(false)` -> `set_focus(0)` plus key-ups for held keys |
| IME / text input | Built as: the helper's render-process handler reports the focused DOM node (`sig.focus` process message: editable, form-control type, bounds), which becomes `TextInputChanged` with the password purpose and no surrounding text; `on_virtual_keyboard_requested` proved unreliable and is not used. Input-method text: `ime_set_composition` while composing, `ime_commit_text` only inside a composition, characters as `KEYEVENT_CHAR` otherwise (a bare `ime_commit_text` does nothing) |
| Zoom | `set_zoom_level` (log scale, `log(factor)/log(1.2)`) |
| Visibility | `was_hidden(1/0)` (stops painting; background tab = hidden, empty metrics = `was_resized` is not called, last size kept) |
| Metrics / scale change | `was_resized`, `notify_screen_info_changed`; an empty rectangle keeps the last size (never `get_view_rect` of 0x0: CEF asserts) |
| Downloads | `cef_download_handler_t::on_before_download` -> cancel and queue the contract's download request through services (the browser fetches it: no cookies; same limit as WPE). `can_download` returns 0 until the core says yes |
| Permissions | `cef_permission_handler_t::on_request_media_access_permission` and `on_show_permission_prompt` -> service request `Permission`; unanswered or unimplemented: deny (fail closed) |
| Clipboard | see 4.5 |
| Dialogs | `on_jsdialog`: alert/confirm/prompt answered by the core (as the contract allows) or `callback->cont(0)`; `on_before_unload_dialog` leave; file chooser `cef_dialog_handler_t::on_file_dialog` -> service `FilePicker` or cancel; HTTP auth (`get_auth_credentials`) refuse; certificate errors (`on_certificate_error`) refuse |
| Context menus | `on_before_context_menu`: clear the model (no native menu), queue nothing in step 1 |
| Crash | `cef_request_handler_t::on_render_process_terminated` -> `ViewFailed` (reloadable); GPU process death: Chromium restarts it (and falls back to software after repeated failures): count in diagnostics; browser process death takes `sig` down (document) |
| Close | `close_browser(force)`; `do_close` returns 0; `on_before_close` -> `ViewClosed` |
| Shutdown | close every browser, wait for all `on_before_close` (bounded, 5 s), then `cef_shutdown` once; the adapter never calls `exit`/`_exit` or installs hooks |
| Site data (`site.forget`) | `cef_request_context_t::clear_http_auth_credentials`, `cef_cookie_manager::delete_cookies`, `cef_request_context::clear_certificate_exceptions`; origin storage: only via DevTools protocol `Storage.clearDataForOrigin` (`browser_host.execute_dev_tools_method`): declare `SiteDataKinds` cookies+storage once step 11 proves it |
| Capabilities | `semantics` **false** in step 1 (CEF has no semantic snapshot; `sighurt-semantic`'s extraction script can run via `frame->execute_java_script`, but returning a value needs a render-process message round trip: step 12) |

### 4.4 Presentation

**CPU path (the smoke test's, step 1 of the engine).** `OnPaint` hands `buffer`, `width`, `height`,
dirty rects of a BGRA buffer owned by CEF and valid only inside the callback. The adapter copies it
(one copy per frame, counted and reported, like WPE's SHM fallback) into the host's SHM lease when a
frame permit is available, on CEF's UI thread? No: the host's target lease and permit are
owner-thread objects. So the callback copies the dirty region into a per-view `Arc<Mutex<Staging>>`
buffer (one copy, bounded: one staging buffer plus a "dirty since last present" rect union,
never a queue of frames), sends `FrameReady`, and the owner's present copies staging into the SHM
lease (second copy) or, when a permit is not free, leaves it. That is **two copies per frame** on
the owner path (CEF's buffer -> staging -> SHM); the contract's "no page-sized copies on the normal
path" is violated until the DMA-BUF path works. Reduce to one copy by making the callback write
straight into the lease's mapped memory when the lease is `Send` (SHM memory is plain shared
memory: a `Send` mapping wrapper in `sighurt-wayland` is not unsafe per se, but that is a host
change, not for this crate; recorded as a follow-up). Frame rate is capped by
`windowless_frame_rate` (60); on an unmapped/hidden view CEF paints nothing (`was_hidden`).

**DMA-BUF path (`OnAcceleratedPaint`).** CEF 150 on Linux has `shared_texture_enabled`
in `cef_window_info_t` and `on_accelerated_paint(info)` with `cef_accelerated_paint_info_t`:
`plane_count`, per-plane `fd`, `stride`, `offset`, `size`, `modifier`, `format` (a
`cef_color_type_t`), i.e. exactly a `zwp_linux_dmabuf` import description, so it can attach to the
host's subsurface the way `sighurt-engine-wpe::present` does with no CPU copy. **Not proved**:
with `shared_texture_enabled=1` under headless sway (GLES2 renderer, NVIDIA 595) the program got no
`OnAcceleratedPaint` and no `OnPaint`; the GPU log shows `shared_image_representation.cc:429 Unable
to initialize SkSurface` with `--use-angle=gl`/`gles`/`swiftshader`; with `--use-angle=vulkan` and with
`--use-gl=egl` frames arrived through `OnPaint` instead (shared texture silently unused).
Likely causes to separate in step 10: the NVIDIA GBM/EGL stack (WPE's DMABuf worked on the same
machine through Mesa-less `/run/opengl-driver`), ANGLE's backend choice, and the fds' lifetime
(CEF closes them after the callback returns: the adapter must `dup` and import before returning,
or on a thread that holds the `Arc` until the compositor releases the `wl_buffer`: the
`FrameReady`/buffer-release separation of the contract applies). Until step 10 passes, the engine
declares `PresentMode::Cpu` like Wasm. If it never passes, the verdict "no routine CPU readback"
(plan §26) can only be claimed for Servo and WPE.

### 4.5 Clipboard, downloads, permissions, privileged services

- **Clipboard.** In OSR Chromium's browser process still reads and writes the system clipboard
  itself through ozone's `ui::Clipboard` (Wayland data-control/wl_data_device via its own
  connection: it needs a focused surface it does not have in OSR; behaviour not measured).
  The page-facing requests (`navigator.clipboard`, Ctrl+C/V editing commands) do not go through a
  CEF handler: `cef_browser_t::get_focused_frame()->copy/paste/cut` execute editing commands that
  use Chromium's clipboard. **There is no "clipboard handler" in the C API.** So to make the
  browser own the clipboard (the rule that engines use services), the engine (a) never sends
  Ctrl+C/X/V as keys to the page: it maps the *commands* the core sends (`EditCopy`, ...) to
  `frame->copy()` and then reads the result with `cef_clipboard`? (no such C API); alternative
  (b): disable Chromium's access with the command-line switch `--disable-features=...`
  (not found) or run the helper and browser with no Wayland data-device. Open question, risk
  R4; step 9 decides between: (1) accept Chromium touching the clipboard itself for CEF pages
  only, documented as a deviation needing the user's decision; (2) page text via
  `on_text_selection_changed` (gives selected text) for copy and
  `ime_commit_text`/`frame->execute_java_script` insertion for paste, with Chromium's own paste
  suppressed by intercepting the key in `cef_keyboard_handler::on_pre_key_event` and
  `is_keyboard_shortcut`. (2) is plain text only, the same limit as WPE's, and keeps the
  browser as the owner. Recommendation: (2).
- **Downloads and permissions**: as in 4.3: cancel in CEF, forward through
  `sighurt_api::services`, fail closed.
- **Chromium's own network stack and storage**: cookies, cache, HTTP auth, the `Variations`
  field-trial and component updater run in the browser process: set
  `--disable-component-update`, `--disable-background-networking`, `--no-pings`,
  `--disable-sync`, `--no-default-browser-check`, `--disable-field-trial-config`,
  `--metrics-recording-only` is not wanted; `--disable-breakpad`/crash reporting is off
  without `crash_reporter.cfg` (confirmed absent in `dist`; it logged "crash_reporter.cfg
  ENOENT"). The cache directory is `{data_dir}/sighurt/cef` (`root_cache_path` and
  `cache_path` equal; ephemeral sessions: empty `cache_path` = in-memory). `user_agent` and
  `accept_language_list` come from `[engines.cef]`.

### 4.6 Sandbox

Chromium's sandbox needs either the setuid `chrome-sandbox` (not possible on NixOS without a
`security.wrappers` entry the user would have to add) or unprivileged user namespaces. With
`no_sandbox = 0`, no setuid helper, and the kernel allowing namespaces (63130), the smoke test ran
every process, so the namespace sandbox is available here. `sig` must not default to
`no_sandbox = 1`; the engine passes `no_sandbox = 0`, and if Chromium fails to start the sandbox
the error is `RuntimeFailed` with a hint, never a silent downgrade. `[engines.cef] no-sandbox =
false` is the only way to turn it off, logged loudly. This is Chromium's sandbox for its
renderer/utility processes; it says nothing about `sig`'s process: CEF's browser process half
(network service, in the sandboxless utility `--service-sandbox-type=none` in the run;
profile, cookies, UI) runs in `sig`'s process or its helper children per CEF's architecture:
the browser process is `sig` itself. A compromised Chromium browser-process component is a
compromise of `sig`: same statement as for Servo and WPE (CLAUDE.md, "Security"). Document in
README/SECURITY when the engine ships.

The GPU process ran (sandboxed GPU sandbox on; it printed `viz`/SkSurface errors only on the accel
path) under headless sway with the NVIDIA driver; software fallback is swiftshader
(`libvk_swiftshader.so` in `dist/`).

### 4.7 Coexistence summary

| Concern | Finding / plan |
|---|---|
| Servo (SpiderMonkey signal handlers), Wasmtime traps, JSC's SIGUSR2 | CEF's V8 is in the renderer process, not `sig`'s; `disable_signal_handlers=1`; step 8 diffs `SigCgt` |
| Allocator | no interposition (libcef exports 243 `cef_*` symbols; no `malloc`) |
| Symbols / `--exclude-libs` | irrelevant for dlopen; confirm with `LD_BIND_NOW=1` as for WPE |
| argv rewriting | copy argv before CEF (found); see 3 |
| Environment | Chromium reads `XDG_*`, `DBUS_SESSION_BUS_ADDRESS` (it logs and retries UPower/ScreenSaver lookups; without a session bus init was not slower) and sets none that matter; `GIO_EXTRA_MODULES`/GSettings schemas missing give a `GLib-GIO-CRITICAL` warning at start (harmless on 150, suppressed by `GSETTINGS_SCHEMA_DIR`) |
| GLib | libcef links libglib; its main context is Chromium's own on its UI thread (no `glib_loop` module needed, unlike WPE) |
| Memory | about 560 MB PSS for one page (10 processes) against WPE's ~170 MB prefix + pages: the heaviest engine; the user's machine has about 3 GB spare while other jobs run, so open-on-demand only (`[routing.cef]`, `page.open-in cef`) |
| Disk | 2.8 GB per version on HDD; cold start 4.4 s from HDD vs 0.75 s warm: put `dist/libcef.so` where page cache keeps it or accept a first-use delay |

## 5. The crate (as designed; the built modules are in section 8)

`engines/sighurt-engine-cef/`, features `engine-cef` on `sighurt-browser`, `sighurt-runtime`,
`sighurt-gui` (not in the `default` list until step 14, because the prefix is 2.8 GB, so
`default` stays Servo + Wasm + WPE and CEF is opt-in `--features engine-cef`; the CLAUDE.md
default-features sentence is updated only if the user decides it joins them).

| Module | Owns |
|---|---|
| `runtime` | `CefRuntime` (`EngineRuntime`), `CefConfig` (prefix, helper path, cache dir, user agent, sandbox), `Diagnostics`, `CAPABILITIES`; one per process |
| `ffi` | the C API it calls (about 35 structs, 12 functions), written from the 150 headers; `load()` = `dlopen` + `dlsym` table + `cef_api_hash` check; `ref_counted!` |
| `handlers` | the `cef_app_t`? (none: no app in the browser process beyond `on_before_command_line_processing`), client and handler structs, one per view; each callback only queues a `Note` and wakes |
| `view` | per-view state (`Page`), browser handle, requests the view made, input state |
| `queue` | the `Arc<Shared>`: event queue with replace-latest per kind and per view (title, status, cursor, loading, frame), bounds (16 held navigations, bytes caps per contract), `EngineWaker` |
| `navigation` | ids, documents, history from navigation entries; pure, unit-tested (like WPE's) |
| `present` | staging buffer, copy into the host's lease or DMA-BUF import (step 10); counted copies |
| `input` | host input to `cef_mouse_event_t` / `cef_key_event_t` (VKEY table from evdev) |
| `ime` | `ime_*` calls and the page's composition range into `TextInputState` |
| `clipboard` | selection text in, paste out, per 4.5 |
| `services` | download, permission, file dialog, dialog requests and the answers |
| `target` | `CefTarget` for the host's SHM lease / `Offscreen` (as WPE's `target`) |
| `semantic` | step 12 only |

`sighurt-engine-cef/helper/` is the helper bin (`sig-cef-helper`).
`build.rs`: prefix via `SIGHURT_CEF_PREFIX` (default `/data/sig-builds/deps/cef-150.0.22`),
`cargo::error` with the instructions when missing; `rerun-if-env-changed`.
Descriptor in `sighurt-runtime::engines`: name `cef`, settings `user-agent`, `storage`
(`profile`/`ephemeral`), `no-sandbox`, `frame-rate`; `sighurt-gui::engines` starts it with the
Wayland targets; the TUI does not (4.1).

## 6. Risks

1. **Newer versions do not start** (exit 28 on 152): pinned to 150.0.22 (Chromium 150, a branch
   that receives only security updates for a few more weeks; Chromium's security fix cadence means
   a pin goes stale in weeks: the engine ships a security-sensitive browser engine in the user's
   process: bump discipline is a maintenance cost). Mitigation: file the CEF bug with the smoke test
   and re-test each branch.
2. **Pump semantics**: the MT loop is the only mode proven. It puts Chromium's UI-process code on
   its own thread beside Servo's and the owner loop; thread-safety of the adapter is on us
   (every callback is a queue push).
3. **No zero-copy frames yet** (4.4): two CPU copies of every page frame until
   `OnAcceleratedPaint` works under NVIDIA.
4. **Clipboard ownership** (4.5): no CEF hook; Chromium may touch the Wayland clipboard itself;
   plan (2) with key interception, plain text only.
5. **Memory and process count**: 10 processes, ~560 MB PSS for example.com.
6. **Headless ozone**: proved, with SwiftShader. No display connection, and no GPU rendering.
   DMA-BUF remains open (section 8).
7. **Chromium's browser process is `sig`**: a browser-process exploit owns `sig`; same class of
   risk as WPE's UI process, with a far larger attack surface (network service in the browser
   process group, no sandbox for it in this config).
8. **Passwords/semantics**: `sensitive_data` hints need a render-process handler (helper code);
   `semantics` capability absent in the first versions.
9. **argv rewriting and environment**: found one; there may be more (`setproctitle`, `prctl
   PR_SET_NAME` of `sig`'s main thread by Chromium? not seen, check `/proc/self/comm`).
10. **Build/pin drift**: the nixpkgs pin moves libs under the patched rpath (re-patch step);
    `libcef.so`'s 1.4 GB means an unpatched copy per version on disk.
11. **Codecs**: no proprietary video/audio codecs; sites needing H.264 will not play video.
12. **Cold start from HDD**: 1-4 s on the first CEF page after a reboot.

## 7. Implementation plan (for workers with no context)

Rules for all steps: read CLAUDE.md first; `tools/cargo`/`tools/heavy` only; Wayland only inside
`tools/headless-sway`; the dependency lives in `/data/sig-builds/deps/`; do not change any
architecture decision here without approval; one worker per file set; every step ends with a
build and its acceptance test; log decisions in STATUS.md.

1. **`nix/build-cef.sh`**: fetch (`curl`, URL in 1), verify sha1 `7e7ca1e7...` and a pinned
   sha256 (compute and pin on first run), unpack, assemble `dist/` as in 2 (hard links except
   `libcef.so`), `patchelf` rpath from `nix/cef.nix`, root `runtime` as a GC root
   (`nix-build -o <prefix>.nix-runtime`), build the smoke test, print a manifest. `shell.nix`
   exposes `SIGHURT_CEF_PREFIX` when `dist/libcef.so` exists, without changing the shell's
   derivation otherwise (check as WPE.md "Using the prefix": `nix-instantiate shell.nix` before
   and after identical without the prefix). *Accept*: second run is a no-op;
   `env -u LD_LIBRARY_PATH ldd dist/libcef.so` has no "not found"; `tools/heavy tools/headless-sway
   -- env CEF_SMOKE_MT=1 CEF_SMOKE_NOPUMP=1 dist/cef-smoke ...` prints `paint_size=1280x800`.
2. **Crate skeleton + `ffi`**: `sighurt-engine-cef` with `ffi::load`, the `cef_api_hash` check,
   the handler structs and `ref_counted!` (real refcounts, unit-tested with a mock `release`),
   `build.rs` prefix check. *Accept*: `tools/cargo check -p sighurt-engine-cef`; a test (`--ignored`,
   needs the prefix) loads the library and checks the hash; the engine's build fails with
   the instruction message without the prefix.
3. **Runtime + one view, CPU path**: `CefRuntime::new` (initialize with the settings of 3,
   `multi_threaded_message_loop=1`, `windowless_rendering_enabled=1`, `disable_signal_handlers=1`,
   `no_sandbox=0`, helper path, cache dir, `strdup`ed strings), `open_view`, `Navigate`,
   `OnPaint` into staging, `FrameReady`, `Close`, `shutdown`. *Accept*: a test in the style of
   `sighurt-engine-wpe/tests` renders `data:` and `https://example.com/` into an `Offscreen`
   target; title and load events arrive through `pump`; `sig`-side `PumpStatus` allows sleeping
   (no timer in the idle path: assert wakeups over 3 s on an idle page <= 2).
4. **Events**: navigation (3.3 table), history, title/URL/loading/status/cursor, crashes
   (`kill -9` of the renderer gives `ViewFailed`, reload recovers), popups -> `OpenRequested`.
   *Accept*: pure `navigation` unit tests; `engines.sh`-style scenario `cef`: link click opens
   per routing, `window.open` becomes `OpenRequested`.
5. **Input and focus**: mouse, wheel, keys (VKEY table, dead keys ä å ö as WPE's test), focus,
   zoom, visibility. *Accept*: the `input.html` scenario types and clicks as for WPE.
6. **Registry and GUI**: descriptor, `CefConfig`, `sighurt-gui::engines` binding to Wayland SHM
   targets, `--features engine-cef`; the page subsurface shows the frame; check with
   `swaymsg -t get_tree` that CEF creates no window of its own. *Accept*: `engines.sh` scenario
   passes under headless sway; `sig` without the feature builds as before.
7. **Contract details**: `NavigationRequested` cancel-and-replay, hold limit 16, routing
   scenario (Servo -> CEF -> Servo, back/forward across engines as in WPE.md).
8. **Coexistence checks**: `SigCgt` before/after, `LD_BIND_NOW=1` bindings from libcef, argv
   copies, `/proc/self/comm`, Servo + Wasm + CEF in one `sig` with an idle test (no chrome repaint
   caused by page animation; no wakeups when idle). *Accept*: numbers in this file.
9. **Services**: clipboard per 4.5(2), downloads, permissions, dialogs, file chooser, auth refusal,
   certificate errors refused; password hints via the helper's render-process handler.
   *Accept*: clipboard scenario like WPE's (wl-copy/wl-paste) with Chromium proved not to touch the
   clipboard (watch the Wayland protocol with `WAYLAND_DEBUG=1` on its connection).
10. **DMA-BUF**: make `OnAcceleratedPaint` fire (try `--use-angle`, `--ozone-platform`,
    `--enable-features=...`, the host's DMA-BUF feedback format/modifier list), import into the
    subsurface with no CPU copy. *Accept*: 0 copies, `readback_bytes=0` on every frame of a WebGL
    page, as in WPE.md; or a written finding why not.
11. **Site data**: `site.forget` through cookies and `Storage.clearDataForOrigin`.
12. **Semantics** (optional): `sighurt-semantic` extraction through the helper.
13. **Security review**: sandbox state (`chrome://sandbox` dump through DevTools), flags, data
    directories, update procedure for the pin.
14. **Documentation**: README engines table, STATUS decisions, SECURITY, DOCS; decide whether
    `engine-cef` joins `default`.

## 8. As built

**Where**: `engines/sighurt-engine-cef` (crate; AGPL-3.0-or-later; README with the CEF and Chromium
BSD notices), `engines/sighurt-engine-cef/helper/` (`sig-cef-helper`, built into `dist/` by
`nix/build-cef.sh`), registration in `sighurt-runtime` (descriptor `cef`, `cef_config`, registry
variant), `sighurt-gui` (Wayland SHM targets), `sighurt-tui` (its CPU target) and the feature
`engine-cef` on `sighurt-browser`, `-runtime`, `-gui`, `-tui` (in no default set; build with
`--no-default-features --features engine-cef,engine-demo`). `sig`'s `main`, the core and the
runtime have no CEF code beyond that registration.

| Module | Owns |
|---|---|
| `capi` | the C API's layouts (generated by `tools/gen_capi.py`; every vtable's size is asserted against gcc's, and the ones used were re-checked with gcc here) |
| `ffi` | `dlopen` of `libcef.so` (API hash checked), strings, reference-counted objects (`new_object`, `Ref`), the `call!` macro |
| `handlers` | the per-view client and its handlers; every callback only updates shared state, queues events and wakes the owner |
| `shared` | the event queue (replaceable events coalesced, bounded drains), the view's geometry, staged frame, browser slot |
| `nav` | navigation state as the contract's events (pure, 9 unit tests): ids, documents, history effects, held navigations (at most 16) |
| `actions` | what the owner asks of a live browser |
| `keys` | VK codes (the layout's letter first), modifier flags, cursors |
| `runtime` | `CefRuntime<T: CpuTarget>`: start (once per process), views, input translation, present, shutdown |

**Changes against the design.**
- **Strings and ownership.** CEF's C API passes every reference-counted callback argument as an
  *owned* reference, and adopts the reference of every object we pass in (`cef_initialize`'s app,
  `create_browser`'s client, a visitor): the code releases the former and never the latter.
- **No initial address.** `create_browser` gets an empty URL; an `about:blank` first load raced
  the browser's first `Navigate` and took its id. `Nav` also drops a blank commit nobody asked for.
- **Text input** comes from the helper (above), not from `on_virtual_keyboard_requested`, which
  fired only sometimes. The helper is now more than `cef_execute_process`: it carries the
  render-process handler (about 90 lines of C).
- **Paste** types the text as characters (`ime_commit_text` alone does nothing); Ctrl+C/X/V are
  recognised by the layout's letter (`Key::Character`), since synthesized keyboards (wtype) send
  one physical code for every key.
- **Downloads** need a `can_download` slot or CEF never calls `OnBeforeDownload`.
- **The CEF distribution is not shared with other users of the prefix**: nothing of sig's own
  state is read or written by CEF: its root cache is `{data_dir}/sighurt/cef` or, ephemeral, the
  engine's scratch directory.
- **`--dump` and text mode** refuse the engine (no `semantics`, as designed). The graphical TUI
  hosts it with no display: `sig --tui --engine cef` with `WAYLAND_DISPLAY` and `DISPLAY` unset
  answered the terminal probe, sent kitty graphics, and exited 0 with no helper left
  (2026-10-31).
- **Zygotes outlive `cef_shutdown`** until the process exits; the helper's other processes end
  with it. `offscreen.sh` checks that nothing remains after the test process exits.
- The clipboard is not handled by a CEF hook (there is none): the engine takes the chords
  before the page sees them and asks the services; Chromium cannot read or write the desktop
  clipboard in windowless mode anyway (no focused surface), so a page's own `navigator.clipboard`
  and `copy` events reach only Chromium's internal clipboard.

**Support, as built.** Navigation, history (traverse by delta), reload, stop, held user-clicked
links (`NavigationRequested`, cancel-and-replay with a one-shot allow), popups and middle clicks
as `OpenRequested`, `window.close()` as `CloseRequested`, zoom, resize and scale (the buffer is
what CEF paints; a mismatch with the host's rounding is padded with opaque white), hidden views,
pointer, wheel, keys with text, focus (held keys released on loss), the cursor, title and status,
crashes as `ViewFailed` (reload recovers), downloads as `Service::Download`, clipboard as above.
Refused: JS dialogs (suppressed), permission prompts and media access (denied), file choosers
(cancelled), context menus (emptied), HTTP authentication and certificate errors (cancelled),
`javascript:` addresses from the browser. Not supported: `<select>` popups (`PET_POPUP` is
ignored), drag and drop, find in page, accessibility, semantics, DMA-BUF frames. Site data is
declared; listing and forgetting it is refused.

**Tests.**
- `cargo test -p sighurt-engine-cef`: 17 unit tests (navigation, keys, queue, staging).
- `engines/sighurt-engine-cef/tests/offscreen.sh` (Chromium into an in-memory `CpuTarget`, in a
  private headless sway, 1 test, about 6 s warm): page pixels (ARGB, top-down, stride padding),
  started/committed order with a Push, a held link answered, traversal (Traverse; beyond the list
  Cancelled), the text-input state of a focused field, typing, Ctrl+V through the service, an
  input-method commit, an `OpenRequested` popup, an attachment as a download request, a resize,
  idle wakeups (0 in 3 s), `ViewClosed`, and no helper process after `shutdown`.
- `apps/sighurt-browser/tests/cef.sh` (`sig` with the cef and demo engines, private sway and
  D-Bus, every XDG directory under OUT): `page.open-in cef https://example.com/` opens in
  Chromium, the tab is labelled `cef` and shows it (screenshot looked at), a local input page takes
  typed letters while the keymap's bound plain letter (`h` = `tab.new`) does not fire, Ctrl+V pastes
  and Ctrl+C copies through the Wayland clipboard, an attachment is saved whole by the download
  service, `sig` exits with no Chromium process left. 11 checks pass.

**Measured** (desktop RTX 3070, headless sway with pixman, 31 October 2026, debug build of `sig`,
example.com then a local input page):

| | |
|---|---|
| Runtime start (`CefRuntime::new`) | 60-150 ms warm; 4.5-8 s on the first start after a long idle (a cold read of the 1.4 GB `libcef.so` from the HDD) |
| Process tree | 11 processes, 1.28-1.34 GiB resident |
| Idle CPU (tree, 10 s) | 0.3-0.7 % of one core |
| `sig`'s idle wakeups | 1-2 in 10 s (the diagnostics' own); the engine's waker 0 in 3 s |
| Copies per frame | 2 (CEF's buffer into staging: dirty rectangles only after the first frame; staging into the lease: whole frame); `sig --diag` reports the staged bytes as the engine's readbacks |

**Open problems.**
1. DMA-BUF (`OnAcceleratedPaint`) still not working (4.4): two CPU copies per frame.
2. Chromium's `navigator.clipboard`/`copy` events never reach the desktop clipboard.
3. A password field is reported with the password purpose and no surrounding text, but the
   composition text a real input method shows is not hidden by anything here.
4. `<select>` popups, drag and drop, find, accessibility, semantics, site data.
5. Newer CEF branches (152 and up) do not start here (exit code 28), so the pin stays at 150.
6. 1.3 GiB for one page; the engine is opt-in for that reason.
7. Run on louise-pc: not done (the prefix is only on this machine's HDD).

## 9. After the independent review (Astra)

The review's 15 findings were checked against the code; all were right. What changed:

- **Chromium has no display connection** (`--ozone-platform=headless`, software compositing with
  SwiftShader, which also draws WebGL). The study's finding that CEF needs a display was about
  the default platform: with headless ozone CEF 150 starts and paints with `WAYLAND_DISPLAY` and
  `DISPLAY` unset (smoke test: first paint 112 ms with `--disable-gpu`, 261 ms with SwiftShader,
  warm; cold 5 to 12 s). So Chromium cannot reach the desktop clipboard or any other part of
  the desktop (P1.3), the terminal frontend can use the engine without a display, and the
  display refusal is gone. Cost: no GPU rendering. The engine still takes every clipboard key
  form itself (Ctrl+C/X/V, Ctrl+Insert, Shift+Insert, Shift+Delete, repeats) and asks the
  services; a cut deletes the selection (`frame.del()`) only after the write was approved.
- **Adoption is unsafe** (`Ref::adopt`), and `Send`/`Sync` hold only for `CefBrowser` and
  `CefBrowserHost` (P1.1).
- **A paste is bound to the document and focused frame that asked**, rechecked on CEF's UI thread
  before every insertion; long text goes in 1024-unit tasks (P1.2, P2.12). Test: a paste
  answered after a navigation is not typed.
- **No lock is held while CEF is called**; a close during creation is posted after
  `OnAfterCreated`; commands issued before the browser exists are queued in order (4096 at most) and
  run after it (P1.4, P2.11). Test: a view closed immediately after `open_view`, three times.
- **Bounds**: page text is converted at most 16 K units (an address 2 M units, over which it is
  dropped), the selection 1 MiB; the event queue admits 4096 events, 16 MiB and 512 a view, with
  life-cycle events always admitted (P1.5). `forget` keeps a published `ViewClosed` (P2.9).
- **Only GET navigations are held**; a POST form goes ahead in Chromium with its body (P1.6).
  Test: a form posted to a local server arrives whole and is never asked about.
- **Navigation ids**: a newer request supersedes an older pending one (which fails `Cancelled`);
  a late `ERR_ABORTED` never takes the current navigation's id; same-document changes are seen
  whenever no cross-document navigation is under way, also with an unchanged address (P2.8, P2.10).
- `leaked()` is forwarded by both bindings (P2.7); the engine declares the site data Chromium
  keeps and refuses listing and forgetting explicitly (P2.13); download URLs and file names are no
  longer logged (P2.15).
- **Presentation copies only what a lease buffer lacks** (`present.rs`): the damage CEF reported
  since that buffer was last written. A buffer is trusted only if probe pixels recorded at its last
  write are still in it (a target need not keep buffers), else it gets the whole frame. The offscreen
  test paints every frame into a kept buffer and a fresh one and compares them (P2.14). DMA-BUF
  stays future work (open problem 1 of section 8).
- **A cleared windowless page is opaque white.** A zeroed `CefBrowserSettings` leaves
  `background_color` alpha at 0, and a windowless browser treats that as transparent painting
  (the 150 headers). A resized page then showed a transparent 800×600 hole padded with white.
  The settings write opaque white at C offset 248, the same colour as `CefSettings.background_color`.
- **A new document clears text input.** The offscreen test's second focus check was flaky: a
  reload can report the field in the same pump as the commit, and a click on a field that already
  has focus does not report it again. The test looks from the commit, and clicks only after the
  load has finished when no focus was reported.
- **Present-copy measurement** (P2.14), debug build, a 24×24 square blinking in a 320×240 page.
  A full frame is 307,200 bytes. Thirty submitted frames per run after five warmup frames, three
  runs. The square sits off the probe pixels, so a trusted buffer is asked only for its rectangle.

  | | bytes copied per frame | `present` time |
  |---|---:|---:|
  | before, whole frame (that build still required a display, so the runs used a private headless sway) | 307,200, 307,200, 307,200; median 307,200 | 100, 95, 96 µs; median 96 µs |
  | after, damage only (no display) | 2,304, 2,304, 2,304; median 2,304 | 42, 46, 44 µs; median 44 µs |

  2,304 bytes is the square (24×24×4). In the contract test a keystroke copied 44,352 bytes into
  the kept buffer of a 1,920,000-byte frame.
