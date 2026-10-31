# Ladybird (LibWeb) for Sighurt: feasibility, build recipe and engine design

Status: **the engine exists and runs in `sig`** (branch `engine-ladybird`): the shim, the crate, the
wiring behind `engine-ladybird`, and a screenshot of https://example.com/ in Ladybird inside `sig`
in a private compositor. See "Implementation status" at the end for what is done, with evidence,
and what is missing. Written 31 October 2026 against nixpkgs' `ladybird`
`0-unstable-2026-09-18` (the pinned nixpkgs of `nix/nixpkgs.nix`). Decisions are numbered L1.. and
are proposals until recorded in `STATUS.md`.

## Verdict

An engine `sighurt-engine-ladybird`, compiled into `sig` behind `engine-ladybird` and loaded
lazily like WPE WebKit, **is feasible** and can very probably reach a complete engine, not only a
half-finished one. What was proved on this machine, with no Ladybird build:

- Everything needed except the generated headers is installed by the nixpkgs package and is in
  cache.nixos.org: shared libraries `lib/liblagom-*.so` (including `liblagom-webview.so`), the
  headers of LibAK, LibCore, LibGfx, LibIPC, LibJS, LibWeb and **LibWebView**
  (`Application.h`, `ViewImplementation.h`, `HeadlessWebView.h`), the generated IPC endpoint
  headers, the helper processes in `libexec/` (WebContent, RequestServer, ImageDecoder, Compositor,
  WebWorker, WasmCompiler) and a CMake package (`share/Lagom/LagomConfig.cmake`).
- The headers that are *not* installed (the cbindgen `RustFFI.h` family and ~15 generator outputs)
  can be regenerated from the exact source nixpkgs built, in about 2 minutes and 340 MB, without
  building Ladybird (`gen-rust-headers.sh`, `gen-generated-headers.sh`).
- A 150-line C++ program (`experiments/ladybird-smoke/smoke.cpp`), compiled against those headers
  and linked against the nixpkgs `.so` files, starts `WebView::Application`, creates a
  `HeadlessWebView`, loads `https://example.com/` (Ladybird spawned WebContent, RequestServer,
  ImageDecoder and Compositor itself), receives `on_url_change`, `on_title_change` ("Example
  Domain"), `on_load_finish`, three `on_ready_to_paint` frames, and writes the frame as a PNG
  (`experiments/ladybird-smoke/example.png`, 800x600 page in a 1056x856 backing store). Full run
  from empty caches: 2 min 38 s, under `tools/heavy`, 2 jobs, no Wayland.

What is not proved: the owner-loop integration (custom `EventLoopManager`), dlopen coexistence with
Servo, Wasmtime and WPE WebKit in one process, wl_shm/dmabuf presentation of Ladybird's buffers,
input, the GPU (dmabuf) transport (here the Compositor fell back to shareable bitmaps, see
"Frames"). Each has a milestone and an acceptance test below. Ladybird itself calls its state
"pre-alpha", and its LibWebView API changes weekly; the pin makes that a cost at upgrades only.

## Facts about the package (inspected, not guessed)

nixpkgs derivation `ladybird-0-unstable-2026-09-18`, source `LadybirdBrowser/ladybird` rev
`b90af890b56cbd51c452d929fe71abcb8adaa36f`, `cmakeFlags = -DENABLE_LTO_FOR_RELEASE=FALSE
-DENABLE_NETWORK_DOWNLOADS=OFF -DICU_ROOT=... -DCMAKE_INSTALL_LIBEXECDIR=libexec`, built with GCC
15.3, Qt 6.11 for the shipped UI (`bin/Ladybird`, not needed), Skia 148, Vulkan, SDL3, ICU 78.3,
Rust 1.98.1 for its Rust crates (`libweb_rust`, `libgfx_rust`, ...). Store outputs: package 191 MB
(downloaded: 72 MB of extra paths), source `gp6da8fm...-source` (258 MB), vendored crates
`hdagdkyq...-vendor`.

- **Libraries are shared.** `liblagom-{ak,core,gfx,ipc,js,web,webview,requests,url,...}.so.0`
  with RUNPATH into the store. They export only `*_API` symbols (`-fvisibility=hidden`); none of
  them defines `malloc`, `operator new` or `signal` (checked with `nm -D`); AK allocates through
  `mi_malloc` (libmimalloc as a private dependency, not a global replacement when loaded
  `RTLD_LOCAL`). `liblagom-main.a` is static (LibMain's `main`), not needed by an embedder that
  has its own.
- **Headers are installed but incomplete.** Install layout `include/{AK,LibCore,LibWeb,...}`,
  `include/Libraries/...`, `include/Services/...` (these are the three include roots the exported
  CMake targets name). Missing, so the first compile fails:
  1. `<Lib*/Export.h>` (CMake `generate_export_header`; defines `WEB_API`, `WEBVIEW_API`, ...).
     Trivial: `#define X_API __attribute__((visibility("default")))`, done in `CMakeLists.txt`.
  2. The cbindgen headers `LibURL/RustFFI.h`, `LibWeb/{ComputedValues,StyleValue,ValueParser,
     StyleEngine,Selector}RustFFI.h`, `LibWeb/Layout/*RustFFI.h`, `LibWeb/Painting/*`, ... They are
     products of the crates' `build.rs`. `gen-rust-headers.sh` runs `cargo check --frozen` on the
     source from the nix store with the vendor directory nixpkgs fetched (46 s, own target dir).
  3. Generated C++: `LibWeb/CSS/{Units,DescriptorID,Enums,Keyword,PropertyID,...}.h`,
     `LibJS/Bytecode/{Op,OpCodes}.h`, `HTML/Parser/NamedCharacterReferences.h`.
     `gen-generated-headers.sh` configures a writable copy of the source (with nixpkgs' ICU patch)
     and builds just those ninja targets (the code generators and nothing else; 16 steps).
  4. Endpoint headers that are installed beside their library but included from another path
     (`<ImageDecoder/...Endpoint.h>`, `<LibWeb/Worker/...>`, `<WasmCompiler/...>`): symlinks in a
     shim directory, and `RequestServer/{IsPrivate,CacheLevel,RequestType}.h` from the source's
     `Services/`; ANGLE's GLES headers from `pkgs.angle`.
- **The compile definitions are not exported and change layouts.** Ladybird sets
  `USE_VULKAN=1 USE_VULKAN_DMABUF_IMAGES=1 USE_FONTCONFIG=1 HAVE_WASM_COMPILER_SERVICE=1` (and
  `NDEBUG`, `-fno-exceptions`, `-fsigned-char`, ...) globally. `USE_VULKAN_DMABUF_IMAGES` adds a
  member to `Gfx::SharedImageBuffer`: without it the first smoke run compiled, linked and then read
  the wrong member (null frame, no crash). The shim's flags must be copied from
  `ninja -t commands Libraries/LibWebView/CMakeFiles/LibWebView.dir/HeadlessWebView.cpp.o` of a
  configured tree of the same source, and re-checked at every pin move (L5).
- **There is no `UI/Headless` any more.** The headless embedder is part of the library:
  `WebView::HeadlessWebView` (`Libraries/LibWebView/HeadlessWebView.{h,cpp}`) and
  `Tests/LibWebView/Test*.cpp` (e.g. `TestLinkHoverStatus.cpp`) show an embedder in ~60 lines. The
  Qt frontend (`UI/Qt/WebContentView.cpp`, `EventLoopImplementationQt.cpp`) is the model for a real
  UI: paints `front_bitmap`, replaces the event loop.
- **Helper processes** are found by `WebView::get_paths_for_helper_process`: `<prefix>/libexec/<Name>`
  where prefix is the parent of the "binary path" given to `Application::create` (the smoke test
  passes `$LADYBIRD/libexec`; `LADYBIRD_LIBEXECDIR` is compiled in only for non-default libexec
  dirs). The helpers are nix `makeCWrapper` scripts that set `QT_PLUGIN_PATH`, `XDG_DATA_DIRS`; they
  work as installed. Resources: `<prefix>/share/Lagom` (themes, fonts) via `s_ladybird_resource_root`
  (a `~/.lagom` directory overrides it).
- **`Application` is a whole browser.** `WebView::Application` (a per-process singleton) owns the
  profile (`--profile-path`, `--temporary-profile`), cookie jar, storage jar, history/bookmark/
  favicon/HSTS stores, downloads, session store, autocomplete, settings, devtools, process
  monitor. It parses `Main::Arguments` with `ArgsParser` and **exits on an unknown option**, so an
  adapter passes a synthetic argv (`--profile-path P --force-cpu-painting --allow-popups ...`) it
  controls. Useful switches: `--force-cpu-painting`, `--disable-sandbox`, `--disable-sql-database`,
  `--disable-content-blocker`, `--disable-http-disk-cache`, `--resource-map`.
- **Frames.** Since the Compositor process (new in 2026) the UI process no longer paints. WebContent
  sends a display list to the Compositor, which renders with Skia (Vulkan or CPU) into **backing
  stores it owns and shares with the UI process** (`Gfx::SharedImage`): three buffers per view,
  ids announced by `ViewImplementation::did_allocate_backing_stores`, presented by
  `server_did_paint(id, size, damage)`, returned with `release_backing_store(id)`
  (= `notify_presented_bitmap_ready_to_paint`). A buffer is either
  (a) a `ShareableBitmap` over an `AnonymousBuffer`, i.e. a **memfd** (`AnonymousBuffer::fd()`,
  size, pitch) with premultiplied BGRA, or (b) a `LinuxDmaBufHandle` (linear Vulkan image,
  `drm_format`, `pitch`, `modifier = DRM_FORMAT_MOD_LINEAR`, fd) that the UI also mmaps. Both are
  wrappable zero-copy as `wl_shm` pool and `zwp_linux_dmabuf` buffer. Observed here: backing store
  1056x856 for an 800x600 viewport; `last_painted_size` is the valid size; "Failed to allocate
  shared GPU backing store (no supported DRM format modifiers for shared image), falling back to
  shareable bitmaps" (Vulkan device available in the shell had no usable modifiers; so (a) is
  proved, (b) is not). Virtual hooks made for this: `defer_backing_store_release(id)`,
  `did_accept_presented_backing_store(id, damage)`, `on_ready_to_paint`.
- **Session history lives in the UI process** (`SessionHistory`, `CanonicalTraversable`,
  `traverse_the_history_by_delta`), so back/forward and `HistoryChanged` do not need
  reverse-engineering as WPE's did. Navigation ids exist natively (`did_start_navigation(navigation_id,
  url)`, `NavigationListener`, `on_load_start/finish`).

## Decisions

- **L1. Use the nixpkgs build as it is; compile only a shim.** No Ladybird build on this
  desktop (nor needed on louise-pc). If the header regeneration ever proves too fragile, the
  fallback is `pkgs.ladybird.overrideAttrs` with a `postInstall` that copies the generated headers
  out of the build tree: a full Ladybird build (hours, ask the user first, louise-pc, 13 GB).
- **L2. C++ shim library `libsighurt-ladybird-shim.so`, built by `nix/build-ladybird-shim.sh`**
  into the same kind of plain prefix as WPE (`~/.local/share/sighurt/deps/`), with the extracted
  flags of L5. It exposes a small `extern "C"` API (below). The Rust crate never sees a C++ type,
  never links Ladybird, and `dlopen`s the shim (`RTLD_NOW | RTLD_LOCAL`) when its first view
  opens (decision 84's pattern). The shim's RUNPATH names the store paths of the nixpkgs
  libraries, which `shell.nix` roots like WPE's runtime closure.
- **L3. UI process in the owner thread, driven by a custom event loop (L4).** Every Ladybird
  object is single-threaded and lives on the thread that runs its `Core::EventLoop`. That thread
  is the owner thread, as for WPE and Servo; no second thread. (Fallback if L4 fails: run Ladybird's
  stock loop on a dedicated thread and marshal commands with `Core::deferred_invoke`; events go out
  through a queue and `EngineWaker`. It satisfies the trait but moves clipboard and dialogs
  across a thread; take it only if the custom loop turns out wrong.)
- **L4. A custom `Core::EventLoopManager` over one epoll fd.** The stock Unix manager is `final`
  and hides its poll set. The shim installs its own `EventLoopManager` +
  `EventLoopImplementation` (a ~300-line port of the Qt one, `UI/Qt/EventLoopImplementationQt.cpp`,
  539 lines) and returns an `EventLoop` over it from `Application::create_platform_event_loop()`.
  **As built:** one manager for every thread (Ladybird runs event loops on other threads too, and
  the stock manager's class is not exported, so it cannot be delegated to): each thread has its own
  epoll instance, wake `eventfd` and timer table; the owner thread's epoll descriptor is
  `lb_loop_fd()`, readable when a notifier's descriptor, a wake or a queued event needs a pump.
  Timers have no descriptor: `lb_pump` returns the milliseconds to the next one, which becomes the
  `PumpStatus` deadline. Notifiers on one descriptor are merged into one epoll registration.
  `SIGINT` and `SIGTERM` (registered only in Ladybird's profiling mode) use `sigaction` with the
  previous handler chained and the owner's wake descriptor as the self-pipe. **`SIGCHLD` is never
  installed**: `ProcessMonitor`'s handler reaps with `waitpid(-1)`, which would take the exit status
  of children that are not Ladybird's (WPE's bubblewrap, a program the host started). The loop keeps
  the handler and never calls it; the shim watches each helper through a pidfd instead (the
  `ProcessManager`'s `on_process_added`, plus the helpers already started), reaps it by its own pid and
  does what the monitor would have done (`remove_process`, `save_crash_report`, `on_process_exited`,
  so a dead RequestServer is still restarted); shutdown reaps the rest by pid. The crate's
  wait thread polls `lb_loop_fd()` and wakes the owner through the `EngineWaker`; it sleeps until
  the owner has pumped, so a readable descriptor cannot make it spin. The idle test: 6 pumps and
  2 loop wakes in 6 s with a static page (Ladybird's own housekeeping timers, no spin).
- **L5. Pin and flag check.** `nix/build-ladybird-shim.sh` fails if the source's own
  `ninja -t commands` for LibWebView carries other `-DUSE_*`, `-DHAVE_*`, `-DNDEBUG` or format-check
  definitions than the shim's `CMakeLists.txt` sets. At load, `ffi::check_abi` compares the ABI
  version and runs a probe: `lb_abi_describe` makes a `Gfx::SharedImageBuffer` with the library's own
  constructor and reads it back with the layout of the headers the shim was compiled with; a skewed
  build reads the wrong member and `ffi::load` fails with a message instead of crashing later. Only
  `SharedImageBuffer` is probed; `ViewImplementation`'s layout is covered by the flag check alone.
- **L6. Presentation: zero-copy from Ladybird's backing stores.** The memfd buffers become a
  `wl_shm_pool` per backing store (created once, 3 per view; `wl_shm` accepts any fd
  mmap-able at the size); the dmabuf buffers become `zwp_linux_buffer_params_v1.create_immed`
  buffers (linear modifier; check the compositor's format table includes it, else force
  `--force-cpu-painting`). The shim overrides `defer_backing_store_release` to return true and calls
  `release_backing_store(id)` when the `wl_buffer` is released (or immediately for a buffer the
  host replaced). `FrameReady` is `on_ready_to_paint`; `present(permit)` attaches the latest
  front buffer, damage = `did_accept_presented_backing_store`'s rect, valid area = the view's
  device-pixel size (the buffer is larger than the viewport). The host's frame callback paces
  nothing in Ladybird (its Compositor schedules by `request_rendering_opportunity`/vsync): L8.
  No page-sized copies on the normal path; if a buffer cannot be wrapped, one counted copy per
  frame is the fallback, reported like WPE's.
- **L7. The view is `LbView : WebView::ViewImplementation`, not `HeadlessWebView`.**
  `HeadlessWebView` fakes a 1920x1080 screen and handles child windows by creating more headless
  views; sig needs the same hooks wired to the core: `on_new_web_view` -> `OpenRequested`,
  `on_request_alert/confirm/prompt` -> service requests, `on_request_file_picker`,
  `on_request_select_dropdown`, `on_request_color_picker` (all `Unsupported` at first: the dialog is
  dismissed, and `native_popups = false`). The smoke test shows the subclass pattern
  (`SmokeView : HeadlessWebView`). Implement `viewport_size()`, `to_content_position()` as identity
  (positions are already surface-local device pixels after `Metrics::to_buffer`).
- **L8. Pacing and visibility.** Hidden view = `set_system_visibility_state(Hidden)` (Ladybird's
  compositor then grants no rendering opportunities, as `TestLinkHoverStatus` documents).
  Frame throttling: `ViewImplementation::m_maximum_frames_per_second` (default 60) from the
  output's refresh rate once the host exposes it; release a presented buffer only when the host
  has presented it, so Ladybird renders at most the three buffers ahead.
- **L9. Capabilities**, initial: `history_traversal`, `page_zoom`, `find_in_page` (native
  `find_in_page*`, `on_find_in_page`), `text_input` (`set_marked_text_from_input_method`,
  `commit_text_from_input_method`, `on_input_method_state_change`), `screenshots`
  (`take_screenshot`), `process_per_view = true` (WebContent per site/tab; site isolation
  `SiteIsolationMode::TopLevel` by default), `site_data` cookies and storage (`CookieJar`,
  `StorageJar`) and hsts. `semantics`: later, not Ladybird's accessibility tree but the
  `sighurt-semantic` extraction script run through `run_javascript` (it is plain DOM JS), so text
  mode works with Ladybird. `accessibility = false`, `pointer_lock = false`, `native_popups = false`.
- **L10. Own profile, never the user's.** `--profile-path <sighurt data dir>/ladybird` plus
  `--disable-content-blocker` and no `--allow-popups` as policy; `Application` is told
  `should_coordinate_browser_process() == false` (no `BrowserProcess` singleton/IPC with other
  Ladybird instances) and `should_use_temporary_profile_by_default()` for private use.
  Ladybird's own history/bookmarks/autocomplete stores run but are unused; `--disable-sql-database`
  where it avoids them. Downloads and external URLs go through `sighurt_api::services` (the shim
  replaces `Application`'s `ask_user_for_download_path`/`ExternalURLHandler` with requests to the
  core), never directly.

## Adapter architecture

### Crates and files

| Path | Owns |
|---|---|
| `engines/sighurt-engine-ladybird/` (crate) | `EngineRuntime` for Ladybird; no C++ types |
| `.../src/ffi.rs` | the C API (`lb_*`), `dlopen`/`dlsym` of the shim (`RTLD_NOW|RTLD_LOCAL`), the ABI check |
| `.../src/runtime.rs` | `LadybirdRuntime`: one per process, spawns nothing itself; `pump` = `lb_pump` + event drain |
| `.../src/view.rs` | per-view state, id/navigation/document tracking, history effects |
| `.../src/loop_fd.rs` | the loop fd as a calloop source and the `PumpStatus` deadline |
| `.../src/present.rs` | wl_shm/dmabuf buffers from `lb_frame`, release bookkeeping, damage |
| `.../src/input.rs` | host `InputEvent` -> `lb_input_*` |
| `.../src/semantic.rs` | `SetSemantics`/`Semantic` through `sighurt-semantic` scripts and `lb_run_js` |
| `.../shim/*.cpp,*.h` | the C++ side: `loop.cpp` (L4), `app.cpp` (`Application` subclass, argv, services), `view.cpp` (`LbView`), `frames.cpp`, `events.cpp` (the event queue) |
| `nix/build-ladybird-shim.sh`, `nix/ladybird.nix` | build recipe, pin, runtime root |
| `engines/sighurt-engine-ladybird/build.rs` | like WPE's: finds the shim prefix through `SIGHURT_LADYBIRD_PREFIX`, links nothing |

The engine is registered like the others: feature `engine-ladybird` on `sighurt-engine-ladybird`,
`sighurt-browser`, `sighurt-runtime`, `sighurt-gui` (and `sighurt-tui` for the CPU target, where
the memfd is mapped and encoded directly), a descriptor in `sighurt-runtime`'s `engines`
(settings `[engines.ladybird]`: `profile`, `force-cpu-painting`, `sandbox`), and a start function in
`sighurt-gui`'s `engines`. Not in `default = [...]` until M4 passes.

### The C ABI (sketch)

```c
int      lb_abi_version(void);                       // shim <-> crate
int      lb_init(const char* const* argv, int argc, const char* prefix, const char* profile);
int      lb_loop_fd(void);                           // the epoll fd: add to calloop
int      lb_pump(int max_events, int64_t* next_deadline_ms /* -1 = none */);
void     lb_set_event_callback(void (*)(const lb_event*, void* user), void* user);   // only inside lb_pump
uint64_t lb_view_open(int w_px, int h_px, double dpr, int visible, double zoom);
void     lb_view_close(uint64_t);
void     lb_navigate(uint64_t, const char* url);  void lb_reload(uint64_t); void lb_stop(uint64_t);
void     lb_traverse(uint64_t, int delta);
void     lb_set_viewport(uint64_t, int w_px, int h_px, double dpr);
void     lb_set_visible(uint64_t, int);  void lb_set_focus(uint64_t, int);  void lb_set_zoom(uint64_t, double);
void     lb_input_key(uint64_t, const lb_key*);  /* + pointer, wheel, ime */
int      lb_frame(uint64_t, lb_frame_info*);  // fd, kind (memfd|dmabuf), size, pitch, drm_format, id, damage
void     lb_frame_release(uint64_t, int id);
void     lb_run_js(uint64_t, const char* script, uint64_t request);
void     lb_shutdown(void);
```

Events (`lb_event`, tagged union, copied strings): `ViewReady`, `UrlChanged`, `TitleChanged`,
`LoadStarted/Finished`, `LoadingState`, `CursorChanged`, `LinkHover`, `NewViewRequested`,
`HistoryChanged`, `FrameReady`, `Crashed(reason)`, `JsResult`, `FindResult`, `InputMethodState`,
`DownloadRequested`, `ExternalUrl`, `Closed`. Callbacks only queue (the contract: "delegate callbacks
only queue events"); `lb_pump` returns after the loop iteration and the crate drains them.

### Mapping onto `EngineRuntime`

| Contract | Ladybird |
|---|---|
| `open_view(view, options, target)` | `lb_view_open`; `ViewReady` on the first `on_ready_to_paint`/client initialised (WebContent spawn is async: `initialize_client`); the target lease holds the Wayland surface, `present.rs` owns the buffers |
| `command(Navigate{navigation,url})` | `ViewImplementation::load(url)`; the adapter maps its `NavigationId` to Ladybird's `navigation_id` string from `did_start_navigation`, or to the next `on_load_start` |
| `Reload / Stop / Traverse` | `reload()`, `stop_loading()`, `traverse_the_history_by_delta(delta)` (native session history -> `HistoryChanged` with entries from `SessionHistory`) |
| `SetMetrics` | `m_viewport_size`, `handle_resize()`, `set_window_size`; device pixel ratio via `m_device_pixel_ratio` (Scale is a rational: pass `Scale` as num/120 -> double; buffer size from the host, not recomputed) |
| `SetVisibility / SetFocus` | `set_system_visibility_state`; focus: `Web::FocusEvent` / blur through `enqueue_input_event`, releases held keys itself |
| `SetZoom` | `set_zoom(level)` |
| `Input` | `enqueue_input_event(Web::KeyEvent / MouseEvent / DragEvent)`; `KeyCode` evdev -> Ladybird `KeyCode`, `text` -> code point, modifiers from the seat; `on_finish_handling_key_event` is the unhandled-key signal (not needed for core keymap, which consumes first) |
| `ResolveServiceRequest` | alert/confirm/prompt: `alert_closed`, `confirm_closed`, `prompt_closed`; clipboard: `insert_clipboard_item`/`clipboard_item()` overrides, page data never stays in Ladybird (as WPE); file picker answered by path |
| `NavigationRequested/AnswerNavigation` | Ladybird has `NavigationLoader`/`handle_external_url`; first version never asks (like Wasm/demo): every page-started navigation is performed by Ladybird. Later: gate through `on_...` hooks. |
| `OpenRequested` | `on_new_web_view` (returns a handle: the adapter answers by creating a view and returning its handle string) and `open_url_in_new_window` |
| `pump` | `lb_pump` (bounded) + drain events; `PumpStatus` from the next timer's deadline; replaceable events coalesced in the crate |
| `present(view, permit)` | attach the front backing store as a `wl_buffer` (L6); `Unchanged` if no new frame; `NotReady` while `has_usable_bitmap` is false after a resize |
| `close_view` | `request_close()` then `force_close()` after 1 s as `HeadlessWebView` does; `ViewClosed` from `on_close` |
| `shutdown` | close views, drain until WebContent exits, destroy `Application` (stops helpers; Ladybird has no `process::exit`-free guarantee: check that it never calls `exit`), unmap buffers, then `dlclose` is *not* called (like WPE) |
| `site_data` | `CookieJar` (`get_all_cookies`, delete by domain), `StorageJar`, HSTS store; `SitesListed`/`SiteDataDone` events |
| Crash | `on_web_content_crashed(reason)` -> `ViewFailed`; Ladybird shows its crash overlay page itself (`m_crash_state`), reload recovers: `RuntimeFailed` only if the Compositor or RequestServer dies and does not restart |

## Risks

1. **Event loop replacement (L4)** is the one novel piece of C++: `Core::EventLoop`'s internals
   (`ThreadEventQueue`, notifier ownership, `spin_until` used from inside callbacks, nested pumps)
   must behave as Qt's port does. Mitigation: port Qt's, and run the smoke test on it first (M1).
2. **Coexistence with Servo, Wasmtime and WPE in one process.** Shim libraries bring Skia,
   HarfBuzz 13.2.1, libpng-apng, fontconfig, OpenSSL, ICU 78.3, SDL3, libmimalloc, Vulkan loader;
   WebKit and Servo bring their own. `sig` is already linked with `--exclude-libs,ALL` (decision 65),
   so its own copies are not exported; dlopen with `RTLD_LOCAL` keeps Ladybird's out of the global
   scope. Not yet tested: two Skia/ICU/FreeType in one process, `fontconfig` global state,
   `FONTCONFIG_FILE` (the helpers print "Cannot load default config file" with the store
   fontconfig unless `FONTCONFIG_FILE` is set; set it to the system file for sig's children),
   signal use (Ladybird's `Application` registers `SIGINT/SIGTERM` handlers only in profiling
   mode; `ProcessMonitor` uses `SIGCHLD` and `waitpid(-1)` (see L4); JIT-less, so no JSC-like signals),
   `atexit` handlers, `setenv` calls (`Application` and the smoke test set `XDG_*`: the adapter must
   pass them to children explicitly, never `setenv` in sig's process, decision 102).
3. **ABI skew.** A header/flag mismatch compiles and silently misreads (seen once, L5). Guard: the
   probe values and the flag check; pin moves rebuild the shim and re-run the acceptance tests.
4. **Moving API.** LibWebView changed shape within weeks (Compositor process, site isolation,
   `defer_backing_store_release`). The shim isolates the Rust crate; an upgrade is a shim
   change. Pin to the nixpkgs revision; do not follow Ladybird.
5. **GPU buffers.** In this environment the Compositor could not make dmabuf images; the CPU
   path (memfd) works and is the first target. dmabuf import needs the compositor to accept
   `DRM_FORMAT_MOD_LINEAR` for `ARGB8888`; untested on the real GPU. `--force-cpu-painting` makes
   the Compositor render with Skia on the CPU (slower, safe).
6. **Sandbox and security.** Ladybird's helpers have their own seccomp/landlock sandboxes
   (`RendererSandboxLinux`, `Compositor/SandboxLinux`); the UI process half (LibWebView,
   RequestServer's client, CookieJar) runs in `sig`, so it is cooperative, not sandboxed (CLAUDE.md
   security). RequestServer is a separate process that does the network and TLS: unlike WPE's
   network process it is Ladybird's, with its own certificate store (`--certificate`): settings
   must not diverge from the other engines' policy (HSTS, content blocking, cookies are per engine).
   Ladybird is pre-alpha: expect crashes, missing features, and security hardening not at
   WebKit's level; document that in the README as "experimental".
7. **Process-wide singleton and heavy `Application`.** One per process; it opens SQLite databases
   in its profile directory, starts a DNS/`RequestServer` set, may phone out (content-blocker list
   update, autocomplete, search suggestions, translations). Options to disable each must be found
   and set (M3); until then run with `--disable-content-blocker --disable-sql-database` and verify
   with `strace -f -e connect` that the only network traffic is the user's navigation.
8. **Build cost and fragility of header regeneration.** 2.5 minutes and 340 MB now; breaks
   if nixpkgs changes the source layout. Cache in `/data/sig-builds/ladybird/` (the HDD) when it
   exists, else `~/.cache/sighurt/` (the smoke script honours `LADYBIRD_SMOKE_CACHE`). Fallback:
   L1's override build on louise-pc, with the user's approval first.
9. **Licensing.** Ladybird is BSD-2-Clause; the shim is AGPL like the rest of `sig`; keep Ladybird's
   notice (NOTICE) when it ships. Skia is BSD-3. No Servo/WPE-style MPL files are derived.
10. **Features not mappable at first**: native popups (select, color, file), drag and drop,
    printing, a11y tree, pointer lock, media sessions, WebDriver, devtools (leave off:
    `Application` starts DevTools only when asked).

## Plan with acceptance tests

Each step is for a worker with no context: read this file, CLAUDE.md, `crates/sighurt-api/src/engine.rs`,
`docs/rewrite/WPE.md`, and `engines/sighurt-engine-wpe/` as the model. Machine rules apply
(`tools/cargo`, `tools/heavy`, no Wayland on the desktop, caches under `/data/sig-builds/ladybird` when
it exists).

(Status of each milestone is in "Implementation status".)

- **M0. Smoke test (done).** `tools/heavy experiments/ladybird-smoke/smoke.sh OUT.png` prints
  `PASS wrote OUT.png`, title "Example Domain" and `frame ...` lines; PNG shows the page.
- **M1. Event loop.** `nix/build-ladybird-shim.sh` builds `libsighurt-ladybird-shim.so` into
  the prefix; `shim/loop.cpp` implements L4. Test `experiments/ladybird-loop/` (C program, no
  Rust): `dlopen` the shim, `lb_init`, open a view, `epoll_wait` on `lb_loop_fd()` only, call
  `lb_pump` on readiness, load a data: URL and `https://example.com/`, assert title, load finish and a
  frame, then idle 5 s with **zero wakeups** (count `epoll_wait` returns) and exit cleanly with no
  child left (`pgrep WebContent`). Also with `--force-cpu-painting`.
- **M2. Crate skeleton.** `sighurt-engine-ladybird` with `ffi.rs` (load + ABI check + a
  `SIGHURT_LADYBIRD_PREFIX`-less build that compiles), `runtime.rs` implementing `EngineRuntime` for
  one view in the demo style, a unit test with a fake shim (a stub `.so` built in the test) for event
  ordering, coalescing, shutdown order. Accept: `tools/cargo check/test/clippy -p
  sighurt-engine-ladybird -- -D warnings`; with the feature off nothing in `sig` changes.
- **M3. Presentation and navigation.** `present.rs` (memfd -> `wl_shm` pool per backing store,
  release through `lb_frame_release`), navigation/history/title/URL/loading events, Reload, Stop,
  Traverse, SetMetrics (resize, scale 1, 1.5, 2: no stale-frame flash), visibility. Test in
  `tools/headless-sway`: an engine test like `engines/sighurt-engine-wpe/tests/headless.sh`
  loading a static page, asserting the screenshot's page-corner colour, a resize, back/forward
  through two pages, a hidden view presenting nothing, zero copies counted on the shm path (the
  memfd is the pool), no page-sized copy in diagnostics.
- **M4. Input, services, settings.** Key (layout key, dead keys via `text`), pointer, wheel,
  focus loss releases keys, IME, cursor, link hover, clipboard through the core, downloads and
  external URLs as service requests, alert/confirm/prompt, `OpenRequested` for new windows, own profile
  (L10), no phone-home. Test: `apps/sighurt-browser/tests/engines.sh` extended with a Ladybird
  case (page open via `page.open-in ladybird`, typing into a form, a link to another engine),
  `strace` network allow-list check.
- **M5. Coexistence.** `experiments/ladybird-coexist/`: one process with Servo, Wasm, WPE and
  Ladybird views alive in the private compositor; each renders; kill WebContent -> `ViewFailed`,
  reload recovers; `lb_shutdown` order; no `SIGCHLD`/`SIGUSR1` interference with WPE's
  JavaScriptCore and `SIG_DIAG`. Then `engine-ladybird` goes into `sig`'s optional feature set.
- **M6. dmabuf and pacing.** Linear dmabuf import via `zwp_linux_dmabuf_v1` when the compositor
  accepts it; `m_maximum_frames_per_second` from the output; measure the interval like WPE's table.
- **M7. Site data, semantics, find, zoom per site, screenshots.** Fill the capabilities of L9.

## What "half-finished" looks like

If the owner-loop integration (M1) or coexistence (M5) fails, the engine is still useful in
these stages, each a product by itself:

1. **M0 as is**: `experiments/ladybird-smoke`, a proof that nixpkgs' Ladybird renders pages
   through LibWebView and its frames are accessible as memfds; also usable as `sig --dump`-style
   tool (screenshot a URL headlessly).
2. **Dedicated-thread loop (L3 fallback)**: pages load, render, navigate, take input; frames are
   handed over through memfd; slower to integrate with shutdown and dialogs, but a normal engine.
3. **SHM only, no input beyond pointer and keys, no services**: a viewer-grade engine (view, scroll,
   click, type) marked experimental, off by default (`engine-ladybird` not in `default`), reachable
   only through `[routing.ladybird]` or `page.open-in ladybird`.
4. **Separate process (the escape hatch)**: if loading Ladybird's libraries into `sig` is
   unsound (risk 2), keep the shim in its own helper process (`sighurt-ladybird-host`) that sends
   the memfd/dmabuf fds and events over a socket (SCM_RIGHTS), which is Ladybird's own model
   anyway. That contradicts "no IPC" (CLAUDE.md) and the in-process architecture; it needs a
   recorded decision, and gives real OS isolation for the UI half.

## Files

- `experiments/ladybird-smoke/smoke.cpp`: the embedder (`SmokeApplication`, `SmokeView`).
- `experiments/ladybird-smoke/{CMakeLists.txt,shell.nix}`: Export.h shims, endpoint symlinks, the
  extracted compile flags, includes of the source and ANGLE.
- `experiments/ladybird-smoke/gen-rust-headers.sh`, `gen-generated-headers.sh`, `smoke.sh`: the
  recipe; `example.png`: the frame of the last run.

## Implementation status

Built and checked on this desktop (nixpkgs pin `4975466d`, Ladybird rev `b90af890`), 31 October 2026.

| Piece | State | Evidence |
|---|---|---|
| Shim prefix recipe, `nix/build-ladybird-shim.sh` | done | `tools/heavy nix/build-ladybird-shim.sh` builds `lib/libsighurt-ladybird-shim.so` in `/data/sig-builds/ladybird` (else `~/.cache/sighurt/ladybird`) in 3-4 min from nothing, 20 s again; roots the package in `ladybird-root`; the flag guard |
| Shim C ABI, custom event loop (M1) | done | `shim/{loop,shim}.cpp`; `tests/loop_probe.c` (no Rust) waits on `lb_loop_fd()` only: title, commit, finish, a frame, the ABI probe, 0 wakeups in 1 s idle; exported symbols are only `lb_*` |
| Crate `sighurt-engine-ladybird`, `impl EngineRuntime` (M2, M3) | done for the scope below | `tests/headless.rs`: events in order, frames with the page's colour, fragment vs document navigation, history back, a click on a link (hover status, a page-started navigation with its own id), resize, hidden, idle (6 pumps, 2 loop wakes in 6 s), close, shutdown; `clippy -D warnings` clean |
| ABI-skew guard | done | `ffi::check_abi` and its unit test; the build script's flag check |
| Wiring: feature `engine-ladybird` on `sighurt-browser`, `-runtime`, `-gui`, `-tui` (not in any default) | done | `tools/cargo check -p sighurt-browser --no-default-features --features engine-ladybird,engine-demo` and with `frontend-tui`; clippy `-D warnings` over browser, runtime, gui, tui with the feature; runtime's descriptor test (24 passed); a build without the feature is unchanged |
| Reaping only Ladybird's own helpers | done | `tests/headless.rs`: a non-Ladybird zombie child (`sleep 0.2`) stays waitable after a SIGKILLed WebContent was reaped by Ladybird |
| Services and text input | done | `tests/services.rs`; `sig-shot.sh MODE=typing` |
| `sig` shows https://example.com/ in Ladybird | done | `tests/sig-shot.sh` in `tools/headless-sway`: `--engine ladybird`, `[routing.ladybird] hosts`, and `page.open-in ladybird` from a demo page (a bound key); screenshots show the page, tab label `ladybird`, title "Example Domain"; sig's diagnostics: `frame engine=ladybird ... readback_bytes=0`; sig and every Ladybird helper process exit cleanly |

Registration is what every engine has and nothing more: a feature, a `Runtime` variant, a
descriptor (`web: true`, `honors: ["storage"]`, shutdown after WPE), `registry::ladybird_config`
(profile `{data_dir}/sighurt/ladybird`, or temporary with `storage = "ephemeral"`), a binding in
`sighurt-gui` (`PresentMode::Shm`) and one in `sighurt-tui` (the terminal's CPU target). Nothing in
`main`, the core or the runtime's logic knows Ladybird.

### What Ladybird does, as found (differs from the first design)

- **Navigation is announced backwards.** The loading state turning on is the start of a navigation;
  `on_url_change` is the commit and comes *before* `on_load_start` (which therefore means nothing
  here); `on_loading_state_change(false)` then `on_load_finish` end it. A page's own hash change
  commits with no loading at all. So the crate takes `LoadingChanged(true)` as `NavigationStarted`
  (the browser's pending id, else its own), a commit with a navigation active as a new document, and
  a commit with none as a same-document change.
- **A fragment navigation made by `load()` reports loading and never reports it over**, and the next
  real navigation then reports no start. The shim ends the state itself (`set_loading_state(false)`)
  when a commit differs from the previous URL only by a fragment (HTML's "navigate to a fragment");
  the crate uses the same rule to keep the document.
- **A navigation started before the view's own about:blank has loaded is dropped.** The shim holds the
  first `lb_navigate` until then and reports `ViewReady` at that moment.
- **History** comes from `session_history()` of the view (entries and current index) after each
  commit and load end; commits carry `history: None` (the core then treats a new document as a new
  navigation). `HistoryChanged` is only sent when it changed.
- **Frames:** the Compositor fell back to memfd bitmaps here ("no supported DRM format modifiers"), and
  the crate copies the valid area (`last_painted_size`, not the 1056x856 allocation) into the host's
  target, one copy per frame (`present_ms` about 1 ms at 1280x696). Zero-copy `wl_shm`/dmabuf import is
  not done; it is the next step (L6).
- **A link that opens a window** (`target=_blank`, `window.open`) makes Ladybird ask for a new view
  with an opener. The shim gives it an internal view the host never sees, and once that has a URL it
  reports `OpenRequested { NewView }` and closes it: the core opens the URL as it does for any engine.
  Untested beyond compiling; `window.open` scripts that use the returned window object get a window
  that is closed once its URL is known.

### Text input

`text_input` is true. Ladybird's input-method state (`on_input_method_state_change`) becomes
`TextInputChanged`: `Some` with the caret rectangle (device pixels from Ladybird, converted to the
target's logical ones) while an editable field has focus, `None` when it does not, so the core's
typing state works and a keymap's plain-letter bindings do not fire while the user types in a page.
The text around the caret is not reported (`surrounding: None`, purpose `Normal`): Ladybird does not
say whether a field is a password, and a password must not reach the input method. The host's
`commit` is typed (`commit_text_from_input_method`), and `preedit` is marked text. Evidence:
`tests/services.rs` (focus a field: `Some` with a caret near it; click away: `None`; a committed
"hello" is typed) and `sig` in a private sway with `"t" = "tab.new"` bound
(`MODE=typing tests/sig-shot.sh`): the click focuses a Ladybird field and "tttest" is typed into it,
with no tab opened.

### What is missing, and why

1. **`NavigationRequested`** (asking the core before a user-clicked link is followed): impossible
   without patching Ladybird. Navigations are admitted inside `WebContentClient::did_request_navigation_start`,
   a class `Application` instantiates itself; LibWebView offers no hook to hold or veto one, and
   WebContent has already started it. Intercepting the click instead would hide it from the page's own
   handlers. The engine never asks, so links stay in Ladybird; `OpenRequested` (windows) works. A
   patched Ladybird (an override of nixpkgs' derivation with a hook, built on louise-pc, a multi-hour
   build that needs the user's go-ahead) would be the way.
2. **Services (`sighurt_api::services`) as built:** every privileged request of Ladybird's UI half goes
   to the host, none is performed by Ladybird.
   - *Copy:* the page's clipboard item becomes `ClipboardWrite` (each text representation; files the
     user copied on the desktop are never exposed; over 16 MiB is dropped). The primary selection and
     Ladybird's own menu copies go nowhere.
   - *Paste:* a paste that finds nothing asks `ClipboardRead` (text/plain, 1 MiB); the granted text
     stays readable for one second and the shim repeats the paste, as WPE does, so the page sees an empty
     first paste and then the real one. `navigator.clipboard.read()` gets the empty one.
   - *Downloads:* Ladybird downloads into a private staging directory of the profile (it names no
     view or URL in its path callback), the shim polls only while one is staged (a 500 ms timer that
     exists only then), and the finished file (at most 64 MiB, else dropped) becomes
     `Download { Data, suggested_name }`; the staged file is deleted. The user's folders are the host's.
   - *File inputs:* `PickFiles` (one or several; no filters passed yet); the files named in the answer
     are read (at most 32 MiB each) into the page's file objects.
   - *External URLs (`mailto:` and so on):* Ladybird's own policy (a user gesture, a scheme not denied)
     still decides whether to ask; then the request is `OpenExternal` and the host's answer decides.
     Nothing runs `xdg-open`.
   - *Attribution:* Ladybird's download and external-URL callbacks do not say which view they are for,
     so they are attributed to the view the host last gave input, focus or a navigation: with several
     Ladybird tabs, a request from a background tab can be charged to the wrong view. The core still
     decides per that view's document and origin.
   - *Dialogs:* the API has no dialog service, so `alert`, `confirm` and `prompt` are answered as dismissed
     (a page never hangs on one). Select dropdowns and color pickers are not answered.
   - Everything else, and every answer that is not `Ok` with the expected reply, is refused.
   Evidence: `tests/services.rs` plays the core against a page: a copy (`ClipboardWrite` with
   `text/plain` "hello"), a paste (granted "PASTED" appears in the field), a `data:` download (bytes
   "hello", name `hello.txt`), a `mailto:` link (`OpenExternal`, denied), a file input (the page reads
   `picked.txt`).
3. **Zero-copy presentation and dmabuf** (L6), **frame pacing from the output** (L8).
4. **Text input:** done (below). **Other capabilities** beyond history traversal, zoom (wired through
   `SetZoom`, not tested), process-per-view and text input: find in page, screenshots, semantics (so no text mode and no
   `sig --dump` in the terminal; the TUI frontend binds the engine and shows its frames as graphics,
   untested), site data.
5. **Cursor shapes** map only the common `Gfx::StandardCursor`s; image cursors report the arrow.
6. **Coexistence with Servo, WPE and Wasmtime in one process (M5)** is untested: `sig` was built with
   Ladybird and the demo engine only. `ProcessMonitor`'s `waitpid(-1)` and Ladybird's `sigaction`
   for `SIGCHLD` are the known conflicts with WPE's GLib.
7. **Environment:** the helpers print "Fontconfig error: Cannot load default config file" unless
   `FONTCONFIG_FILE` is set (the nix store config path is not populated); text still renders. The
   engine does not set it (no environment writes after startup, decision 102); the scripts set it to
   `/etc/fonts/fonts.conf`. `Application` raises `RLIMIT_NOFILE` to 65536 (a process-wide effect).
8. **Ladybird's idle** is not zero: about 1 to 2 wakes a second (its own timers: TabPerformanceMonitor,
   cookie and HSTS synchronisation, session flush), no spin.

### Run it

```sh
tools/heavy nix/build-ladybird-shim.sh                 # once: the shim (nixpkgs' Ladybird, no build of it)
tools/cargo build -p sighurt-browser --no-default-features --features engine-ladybird,engine-demo
tools/heavy tools/headless-sway -- engines/sighurt-engine-ladybird/tests/sig-shot.sh OUT [URL]
#   MODE=route | MODE=open-in selects [routing.ladybird] or page.open-in; screenshots in OUT
tools/heavy cargo test -p sighurt-engine-ladybird --test headless -- --nocapture
```

`CARGO_TARGET_DIR=/data/sig-builds/targets/ladybird` keeps the build off the SSD.
