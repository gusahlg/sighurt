# Servo 0.7 notes

This branch ports `sighurt-engine-servo` from Servo 0.5.0 / surfman 0.13.0 to Servo 0.7.0 /
surfman 0.14.0. The adapter's contract with `sighurt-api` is unchanged. STATUS decision 3
(pin Servo at 0.5) is superseded on this branch only; `docs/rewrite/STATUS.md` is not edited
here.

Sources checked: the published crates (servo 0.7.0, servo-paint-api, servo-config,
servo-constellation-traits, servo-embedder-traits, surfman 0.14.0) and Servo commit
`aac43a3f31a259f04a574f5ec4e959c943ad7cc7` for constellation and paint files that the crate
does not ship. Claims below that name a function were read there. Behaviour that only a run
can show is marked as not measured.

## What improved

- **Hidden is a WebView flag.** `WebView::hide` / `show` send `SetWebViewHidden`. The
  constellation stores it and throttles every pipeline of that WebView.
  `Pipeline::spawn` and history activation pass the flag on, so a new document inherits it.
  Servo 0.5's `set_throttled` applied only to the current document, which is why
  `View::keep_throttled` re-sent it on every history change.
- **System focus is per WebView.** `WebView::set_focused(bool)` sends
  `SetWebViewHasSystemFocus(id, focused)` and does not clear other WebViews. That matches
  the core, which already sends `SetFocus(false)` on the old view before `SetFocus(true)`
  on the new one. `WebView::focus` / `blur` and `notify_focus_changed` are gone.
- **AccessKit actions.** `Servo::forward_accessibility_action(ActionRequest)` exists. The
  adapter does not call it. The terminal frontend's accessibility work can, without an
  `sighurt-api` change, once a caller exists.
- **New preferences** the adapter does not set: `network_http_disk_cache`,
  `network_http_disk_cache_size`, `js_mem_gc_malloc_threshold_base_mb`,
  `js_mem_gc_urgent_threshold_mb`, `layout_flexbox_balance`, `perf_thread_boost_enabled`,
  `thread_pool_canvas_workers` (name from the 0.7 pref list; confirm before using), and
  `dom_webgpu_*`. `thread_pool_canvas_workers` is a `u64` (default 3 in the 0.7 pref
  list). Disk cache and the GC thresholds are the ones most likely to matter.
- **WebGL is a feature.** 0.5 always compiled it. 0.7's default features are `bundled`,
  `clipboard`, `js_jit`, `multiprocess`. This workspace enables `webgl` as well. WebGPU
  stays off, as it was.
- **Multiprocess compiles and stays off.** The feature is in Servo's defaults.
  `Opts.multiprocess` still defaults to `false`, and the adapter still passes
  `Opts { hard_fail: false, config_dir, ..default }`. Pages stay in this process.
- **Shutdown in `Drop` is the same shape.** `ServoInner::drop` sends `Exit`, sets
  `ShuttingDown`, and spins until the event loop stops. The profile is still written on
  that path.

## What got easier

- A hidden view can be hidden once the WebView exists. `NewWebView` and `SetWebViewHidden`
  share the constellation channel, so `hide` immediately after `WebViewBuilder::build`
  is processed after the WebView is created. Later documents inherit the flag, so
  `keep_throttled` no longer has to re-throttle each new document. It still calls `hide`
  because `hide` is a no-op when paint already recorded hidden, and because a hide sent
  before the constellation has the WebView is dropped (`set_webview_hidden` returns
  without recording it, while paint has already stored the flag and will not send again).
- Showing recreates the EGL window surface, resizes, then calls `WebView::show`. Hiding
  calls `WebView::hide` and then releases the EGL surface. Both are still required:
  `hide` does not free the Wayland window surface.
- Focus tracking matches the browser. The adapter records which view it focused and calls
  `set_focused(false)` on any WebView that should not have focus, including one just
  built: constellation `ConstellationWebView::new` sets `has_system_focus: true`, and the
  embedder `WebView` starts `focused: true`. `set_focused` sends nothing when the flag is
  already right. Pop-ups are hidden and unfocused after they are built.

## What broke, or stayed missing

- `WebView::set_throttled`, `focus` and `blur` are gone. Call sites are `hide` / `show` and
  `set_focused`. `MouseButton::Left` / `Middle` / `Right` are `Primary` / `Auxiliary` /
  `Secondary` (`None` is a hidden extra variant). `keyboard-types` is still 0.8.
- **No navigation id.** `notify_history_changed` is still `(WebView, Vec<Url>, usize)`.
  `WebView::load` still returns nothing. The task's example of "which navigation committed"
  is not in the published embedder API. Attribution stays in `navigation.rs`.
- **`Started` is dropped while the WebView is already `Started`.** `WebView::set_load_status`
  returns without notifying when the status is unchanged. A new WebView starts `Started`, and
  a document navigated to before the previous one has reported `HeadParsed` or `Complete` is
  still `Started`, so the new document's `LoadStatus::Started` never reaches the embedder.
  Its `<body>` still reports `HeadParsed` (`HTMLBodyElement::bind_to_tree`). The adapter
  treats a second `HeadParsed`, or one beside the history change while a browser `Navigate`
  is waiting with no `Started`, as that document. Holding the history change for one extra
  spin was not enough: that batch also queues a title or a frame, so the runtime spins again
  immediately, before the script has sent the head, and the commit was already reported. The
  history change is held for up to 100 ms for `Started` or `HeadParsed`. A head followed by
  another history change is left for that change. Measured on louise after that hold
  (`sighurt-engine-servo/tests/headless.sh`, 2026-10-17): `after-204.html` committed as
  `NavigationId(13)` / `DocumentGeneration(8)`, `self.html` as `NavigationId(14)` /
  `DocumentGeneration(9)`, and `slow/own.html` as `NavigationId(16)` /
  `DocumentGeneration(10)`. `engine.rs`, `crash.rs` and `unavailable_context.rs` passed.
- **A load in flight is still dropped.** `Constellation::load_url` returns `None` with no
  embedder message when that browsing context is already in `pending_changes`. The queued
  navigation in `View::queued` stays. `AbortLoadUrl` still exists for 204/205.
- **No stop API.** `Stop` is still `window.stop()`, plus giving up on a load that has been
  quiet for `LOAD_WAIT`. Decision 80 stays unless a run shows otherwise.
- **Crash still leaks, and that is re-measured.** The same-process script thread is still
  the 0.5 design while `Opts.multiprocess` is false. `shutdown` still leaks Servo after a
  page crash rather than hang (decision 54). `tests/crash.rs` passed on louise
  (2026-10-17): `crash.html` committed as `NavigationId(3)` on `DocumentGeneration(3)`,
  the script panic became `ViewFailed`, and the reload committed as `NavigationId(4)`.
  The log says `Servo can't shut down after a crash; leaving it running`, and the
  constellation still logs `Did not successfully request EventLoop exit: Disconnected`.
  The test expects the leak. A green run is not a clean shutdown.
- **Workarounds kept without re-reading the 0.7 script or layout code:** history names for
  `pushState` / `replaceState` (the projection workaround in `navigation.rs`), reload
  reported as the page's own navigation, resize-while-hidden so a CSS animation does not
  keep the old layout, `layout_threads = 2`, `dom_sharedworker_enabled = false`, and
  `IntersectionObserver` / `adoptedStyleSheets` left off. IntersectionObserver's blank-page
  bug was fixed upstream in servo/servo#47693 after 0.5. Turn it back on only with a
  reddit.com / microsoft.com run.
- **Default user agent** is Firefox 153 and still contains `Servo/<version>`.
  `firefox_user_agent` still rewrites that token to `Gecko/20100101`.
- **Removed preferences** that 0.7 no longer has, because the features are always on:
  `dom_abort_controller_enabled`, `dom_clipboardevent_enabled`,
  `dom_mutation_observer_enabled`, `dom_uievent_which_enabled`,
  `layout_css_alpha_color_function_enabled`, `layout_css_attr_enabled`. The adapter did
  not set them.
- New `WebViewDelegate` methods have defaults, so the adapter compiles without them:
  `notify_input_event_handled`, `notify_media_session_event`,
  `notify_fullscreen_state_changed`, `request_unload`, `request_protocol_handler`,
  `notify_accessibility_tree_update`. The accessibility-tree callback is the other half of
  the AccessKit opportunity above. `notify_input_event_handled` could replace some of the
  adapter's own key-release bookkeeping later; it is not wired.
- `EmbedderControl`, `PermissionFeature` and `ServoError` variants match 0.5, so the
  delegate's matches stay exhaustive. `RenderingContext` lost `native_context` (it had a
  default). The methods this adapter and the experiments implement are unchanged.
  surfman 0.14 still has `Connection::Alternate` and the context/surface calls
  `offscreen.rs` uses. `SoftwareRenderingContext::new(PhysicalSize)` still builds a
  software adapter through `Connection::new`.

## Lockfile

`Cargo.lock` is seeded from Servo 0.7.0's published `Cargo.lock` (the file inside the
crates.io crate) merged with Sighurt's previous lock. Registry packages in Servo's lock
are taken from it, checksums included. Where both locks have the same crate at
semver-compatible versions, only Servo's version stays (accesskit 0.24.1 became 0.24.0).
Semver-incompatible copies both stay, so Sighurt's Wayland stack is not forced onto
Servo's older one: calloop 0.13 and 0.14, smithay-client-toolkit 0.19 and 0.20,
wayland-client 0.30 and 0.31, several nix releases, toml 0.8, 0.9 and 1, and
`servo_arc` 0.4.3 beside Servo's 0.5.0. Packages Servo does not use stay at Sighurt's
previous versions (wasmtime 36.0.16 among them). The published lock's root `servo`
package has no crates.io source, so it is omitted; cargo adds `servo` 0.7.0.

Bare dependency names that the merge made ambiguous were rewritten to `name version`
using the single version from the lock that package block was copied from. Thirteen
edges still named a version the merge had dropped, and their crate requirements accept
the version that remains, so they were retargeted: nine crates (`bytecheck_derive`,
`pest_generator`, `ptr_meta_derive`, `rkyv_derive`, `serde_repr`, `synstructure` 0.14,
`zbus_macros`, `zvariant_derive`, `zvariant_utils`; each `syn = "3"` or `"3.0"`) from
syn 3.0.6 to 3.0.4; `dtls`, `tungstenite` and `webrtc-srtp` (`sha1 = "0.10"`) from
sha1 0.10.7 to 0.10.6; `quinn-proto`'s wasm-only `getrandom = "0.4"` from 0.4.3 to
0.4.1. After that the lock parses with no ambiguous line and no dependency on a
missing package except the workspace's `servo` dependency, which cargo fills in.

`cargo fetch` on louise (Rust 1.98.1, via `tools/cargo`) reconciled the seed.
It added `servo` 0.7.0 and pruned 302 package versions the workspace does not
reach: WebGPU, WebXR, Bluetooth, GStreamer, winit, and crates only those
features used (including calloop 0.13 and smithay-client-toolkit 0.19).
Sighurt's own copies stay: calloop 0.14.4, smithay-client-toolkit 0.20.0,
wayland-client 0.30.2 beside 0.31.15, wasmtime 36.0.16. No package was
resolved to a version present in neither Servo's published lock nor
Sighurt's previous lock. Direct dependencies of `servo` 0.7.0 that this
feature set keeps (`webgl` plus Servo's defaults, multiprocess compiled
and off) match the published lock's versions. Two Windows-only edges
moved, which this Linux build does not compile: `arboard` 3.6.1's
`windows-sys` from 0.52.0 to 0.60.2, and `rustix` 0.38.44's `windows-sys`
from 0.52.0 to 0.59.0. Cargo also dropped version suffixes on dependency
lines that the prune left unambiguous, and dropped optional-feature edges
this resolve does not enable. Nothing was pinned back with `--precise`.

Coupled crates that were already at Servo's numbers and did not move: euclid 0.22,
dpi 0.1, gleam 0.15, glow 0.17, keyboard-types 0.8. surfman moved from 0.13.0 to 0.14.0.
stylo moved (0.20 to 0.22) as a Servo dependency, not a direct one.

## Experiments

`m0-coexist`, `m1-mixed`, `wpe-coexist` and `tui-servo` stay in the workspace.
`m1-mixed` and `tui-servo` got the same focus, hide and mouse-button renames.
`m0-coexist` uses `SoftwareRenderingContext` and needed no API edit.
`wpe-coexist` only links this crate. None were excluded.
