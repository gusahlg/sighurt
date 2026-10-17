# sighurt-engine-servo

The Servo (HTML, CSS, JavaScript) engine for Sighurt: a library that `sig` compiles in (feature
`engine-servo`) and runs in its own process, presenting straight into a Wayland subsurface.
See `sighurt-rewrite-plan.md` §14 for the design and `docs/rewrite/STATUS.md` (M4) for what has
been shown to work.

It is pinned at Servo 0.5 and surfman 0.13. Building it compiles Servo: do that on louise-pc
(`SIG_WHAT="servo build" tools/remote tools/cargo build -p sighurt-engine-servo`), not casually
here.

## Using it

The executable owns the process-wide setup (decision 26): it installs its panic hook (which
returns early for `should_panic_hook_suppress_termination()` and hands other panics to
`log::error!`), installs rustls's aws-lc-rs provider, builds the one `ServoRuntime` at startup
with a `ServoConfig` (the profile directory, `{data_dir}/sighurt/servo`) and an `EngineWaker`,
and calls `setup_logging()`. The runtime implements `EngineRuntime` with
`sighurt_wayland::TargetLease` (a `PresentMode::Gl` target per view) and `FramePermit`; see
`tests/common/mod.rs` for an owner loop that does all of this, and `examples/servo-window.rs`
for a window built on it:

```sh
SIG_WHAT="servo build (m4)" tools/remote tools/cargo build -p sighurt-engine-servo --example servo-window
tools/headless-sway --renderer gles2 -- target/debug/examples/servo-window --exit-after 10 PAGE...
```

## How it maps onto Servo 0.5

- **Presentation.** Each Wayland view has its own `WindowRenderingContext` on its lease's
  handles, swap interval 0. The host issues a frame permit only after the frame callback of the
  last swap, so a permit starts the frame Servo's refresh driver waits for, and `present` paints
  and swaps at the permit's buffer size. A hidden view is throttled, presents nothing and gives
  its EGL window surface back; showing it makes a new one and presents at once. No page pixels
  are read back on this path (`ServoDiagnostics::read_to_image` stays 0, and with `audit_gl`
  every GL read WebRender makes is counted in `gl_reads`).
- **Offscreen.** `ServoRuntime<Offscreen<C>>` takes any `sighurt_api::present::CpuTarget` and
  no Wayland connection. Each view has its own Mesa surfaceless framebuffer (hardware adapter,
  software only if that context cannot be made; `ServoDiagnostics::offscreen_adapter` says
  which was asked for). `present` paints, then one `glReadPixels` straight into the lent frame,
  converting RGBA bottom-up to ARGB8888 top-down in that pass. Each successful read counts in
  `read_to_image`. `ServoTiming::swap` is that readback and is logged as `swap_ms`.
  `LIBGL_ALWAYS_SOFTWARE` and `DRI_PRIME` are restored after the context exists.
- **Navigation and history.** Servo's history reports are the truth. Each commit is attributed
  to the browser's pending navigation that fits it (Servo 0.5 gives no ids) or gets an id of its
  own; a new document is known from Servo's `LoadStatus::Started` and from each entry's document,
  never from comparing URLs (`src/navigation.rs` has the rules and their limits). Servo 0.5
  drops a load asked for while another is in flight, so such a navigation waits for that load's
  document. Network errors commit Servo's own error page; an address that isn't one fails the
  navigation.
- **Input.** Keys by W3C code and key value (dead keys arrive composed, AltGr is `AltGraph`),
  held keys released on blur, input-method text as composition events, positions converted
  once to device pixels.
- **Services.** Clipboard reads and writes, permissions (camera, microphone, geolocation) and
  file pickers are service requests the browser decides on; anything else Servo asks for is
  refused. `<select>`, colour pickers, `alert`/`confirm`/`prompt`, context menus and HTTP
  authentication are resolved closed (decision 16). Servo 0.5 has no downloads and no stop: `Stop`
  runs `window.stop()`.
- **Failures.** A page crash is `ViewFailed`: Servo shows its crash page, and the view's next
  navigation loads in a new WebView, since Servo keeps sending the crashed site's pages to the
  dead script thread. A view that can't get a GL context is `ViewFailed` then `ViewClosed`;
  losing Servo's backend is `RuntimeFailed`, and web pages then need a restart of `sig`. After a
  page crash or a lost backend Servo 0.5 can't shut down (its constellation waits for pipelines
  that will never exit), so `shutdown` leaves it running and `ServoDiagnostics::leaked` tells
  the executable to end the process without running exit handlers.

Servo's preferences are the old `sig-servo`'s (`preferences()` in `src/runtime.rs`, reasons
there): grid and other features on, `SharedWorker`, `IntersectionObserver` and
`adoptedStyleSheets` off, two layout threads, a Firefox user agent. `[engines.servo]` can turn
the experimental set on and set any preference; see below. The previous out-of-process engine,
`sig-servo`, is at git tag `gpui-baseline`; its README records site compatibility and measured
rendering costs.

## Experimental web-platform features

`[engines.servo] experimental-features` (default `false`, Servo's own default) turns on the
preferences servoshell enables with `--enable-experimental-web-platform-features`. Servo 0.5's
list, from `ports/servoshell/prefs.rs` at the published crate's commit `77fccacc` (not every
`// feature:` comment in `prefs.rs`):

`dom_async_clipboard_enabled`, `dom_exec_command_enabled`, `dom_fontface_enabled`,
`dom_indexeddb_enabled`, `dom_intersection_observer_enabled`,
`dom_navigator_protocol_handlers_enabled`, `dom_notification_enabled`,
`dom_offscreen_canvas_enabled`, `dom_permissions_enabled`, `dom_sanitizer_enabled`,
`dom_storage_manager_api_enabled`, `dom_webgl2_enabled`, `dom_webgpu_enabled`,
`layout_columns_enabled`, `layout_container_queries_enabled`, `layout_grid_enabled`,
`layout_variable_fonts_enabled`, `layout_css_alpha_color_function_enabled`,
`layout_css_attr_enabled`, `layout_css_ellipse_corners_enabled`,
`layout_css_progress_function_enabled`.

They can be unstable. IntersectionObserver, in particular, makes script spin and pages stay
blank on Servo 0.5 (a layout bug fixed upstream). The flag does not enable adopted stylesheets
(`dom_adoptedstylesheet_enabled`) or web animations (`dom_web_animations_enabled`), and Servo
has no preference for `requestIdleCallback`. It also does not turn `SharedWorker` back on or
change the two layout threads: both are outside the set, for the reasons above.

`[engines.servo.preferences]` is a table of Servo 0.5 preference names (the fields of
`Preferences`) to values, applied after the flag, so one feature can be turned on or off on its
own. A value is a boolean, an integer or a string, of that field's type, or four numbers for
`shell_background_color_rgba`. An unknown name or a value of the wrong type is an error when
the configuration loads. The name and type list is generated from `prefs.rs` 0.5
(`sighurt-core/tests/gen_servo_prefs.py`). Both settings are read when `sig` starts.

```toml
[engines.servo]
experimental-features = true

[engines.servo.preferences]
dom_adoptedstylesheet_enabled = true
dom_intersection_observer_enabled = false
```

## Tests

- `tools/remote tools/cargo test -p sighurt-engine-servo --lib`: navigation, input, event and
  profile-lock rules without a compositor, and a lost backend reported to a real Servo.
- `SIG_WHAT="servo test (m4)" tools/remote sighurt-engine-servo/tests/headless.sh`: the
  compositor tests (`tests/engine.rs`, `tests/crash.rs`, `tests/unavailable_context.rs`) in a
  headless sway. Test and example builds compile Servo with `testbinding` (a dev-dependency) for
  `ServoTestUtils.panic()`; `sig`'s never does.
- `tests/window-checks.sh`: `servo-window` driven with input, scales, resizes and tabs.

## License

`AGPL-3.0-or-later AND MPL-2.0`. Files adapted from Servo stay under the
[Mozilla Public License 2.0](https://mozilla.org/MPL/2.0/) and keep their notices: `src/offscreen.rs`,
adapted from `experiments/tui-servo` (itself from servo-paint-api's `SoftwareRenderingContext`).
The rest of the crate is AGPL-3.0-or-later like the rest of Sighurt. Servo itself is
MPL-2.0. See [NOTICE](../NOTICE).
