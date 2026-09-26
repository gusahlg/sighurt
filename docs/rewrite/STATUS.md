# Sighurt rewrite: status

The specification is [`sighurt-rewrite-plan.md`](../../sighurt-rewrite-plan.md); section numbers
below refer to it. The pre-rewrite browser is at git tag `gpui-baseline`.

Tick a pass criterion only with evidence, and link or name it: a test, a measurement, a run on
a named machine and compositor. A criterion that turned out not to hold goes in the decisions
log with what was decided instead.

## Where things stand

- **Foundation.** The workspace has the crate layout of §5. `sighurt-api` v0 exists: ids, units
  and scale, input, the engine contract, events, services and commands, with unit tests for id
  allocation, scale rounding, the engine waker and command parsing. `sighurt-core`,
  `sighurt-wayland`, `sighurt-ui`, `sighurt-engine-servo`, `sighurt-engine-demo` and
  `sighurt-browser` are skeletons: module files whose documentation states their
  responsibility. `sig` prints its version and help and otherwise says the rewrite is in
  progress. `sighurt-engine-wasm` and `sighurt-ipc` are unchanged, still the old `sig-wasm`
  process. The API went through a plan-conformance and implementability review; what changed
  is in decisions 9 and 11–17.

## Milestones (§23)

### M0 — Record the baseline and prove engine coexistence

Build/do:

- [ ] Baseline measured: build the old browser (`sig`, `sig-servo`, `sig-wasm`) from a
  `git worktree` of `gpui-baseline`, run it on a named machine, compositor and small fixed
  workload, and record timing and memory. M7 repeats the same workloads, so write them down.
- [ ] Required user-visible behavior of the baseline recorded (what the daily workflows are).
- [ ] Wayland and driver environment recorded: River with gharial, the GPU and its driver.
  (Dependency versions: decisions 3–5.)
- [ ] Servo and Wasmtime compiled and linked into one executable.
- [ ] Both engines initialized in that program: JavaScript and Wasm executed, a guest trap,
  several views and stores, clean shutdown.
- [ ] Process-global setup reviewed, each item decided and owned by `sig`:
  - [ ] Signal handlers: SpiderMonkey and Wasmtime both install handlers for traps
    (SIGSEGV and friends). Check that a guest trap and JavaScript both still behave with both
    engines loaded, in either initialization order.
  - [ ] TLS provider: `Cargo.lock` has rustls with both `ring` (reqwest, for the Wasm engine)
    and `aws-lc-rs` (Servo), so rustls can't pick a default: `sig` installs one explicitly.
    The old `sig-servo` installed `aws_lc_rs` (`gpui-baseline:sighurt-engine-servo/src/main.rs`).
  - [ ] Panic hook: the old `sig-servo` reported panics on Servo's style threads, which Servo
    itself misses (`report_panics`, same file); `sig` needs an equivalent that respects
    `servo::should_panic_hook_suppress_termination`.
  - [ ] Logging: one setup in `sig` (the old engine called `servo.setup_logging()`).
  - [ ] Profile path: the old engine kept `{data_dir}/sighurt/servo` with a lock file shared by
    every page's process; one runtime per process now owns it.
  - [ ] Environment mutation by either engine, and every exit path: the old engine's
    `process::exit` on backend loss and `_exit` shutdown watchdog must not come along.
- [ ] Compatibility inventory recorded:
  - [ ] Commands: the 17 names in `sighurt_api::commands::Command::NAMES`.
  - [ ] CLI: `-h`/`--help`, `-V`/`--version`, `--default-config` (printed the built-in
    configuration; not in the new `sig` yet), URLs and paths as arguments.
  - [ ] Configuration: `default_engine`, `[engines.<name>]` (`command`, `schemes`,
    `extensions`, `mime`), key bindings in `[keys]` with the `[keys.page]` and
    `[keys.location]` contexts, `[ui.colors]` (`gpui-baseline:sighurt-core/src/default-config.toml`).
  - [ ] SDK: the `"oxide"` import module, the guest exports (`start_app`, `on_frame`,
    `on_timer`, `on_event`) and the example guests as the compatibility corpus.

Pass:

- [ ] A minimal combined program links and runs correctly, or the exact reason for retaining a
  process boundary is recorded.
- [ ] No blanket unsafe workaround was needed to force compatibility.

### M1 — Prove mixed rendering in one Wayland window

- [ ] A real HTML page and an animated/WebGL test present through the GPU path.
- [ ] There is no ordinary page `read_pixels`/CPU image transport path in use.
- [ ] Repeated resize, scale changes, hide/show, and teardown behave correctly.
- [ ] Keyboard focus routes through the window to page/chrome correctly.
- [ ] Page animation does not cause repeated CPU chrome painting.
- [ ] A static chrome-only experiment sleeps when there is no work.
- [ ] Diagnostics identify the actual GL renderer and fallback mode.
- [ ] The result runs on the actual River/gharial system and GPU driver intended for use.

### M2 — Extract the core and neutral contracts

- [ ] Core tests run without a display or either engine.
- [ ] UI/platform types are absent from the core's dependency graph.
- [ ] Late events for closed/replaced views are rejected.
- [ ] Configuration errors are handled without replacing valid state.

### M3 — Build the actual Wayland host and CPU chrome

- [ ] Usable address/command editing.
- [ ] Reliable tab selection with dummy views.
- [ ] Bounded buffer lifetime.
- [ ] No rendering while idle.
- [ ] Correct pointer/keyboard coordinates.
- [ ] Normal close and compositor disconnect handled deliberately.

### M4 — Convert and mount Servo

- [ ] Browse a fixed set of pages.
- [ ] Open, close and switch tabs.
- [ ] Navigate back/forward, including same-document entries.
- [ ] Reload and stop.
- [ ] Use the clipboard and text input.
- [ ] Resize across scales.
- [ ] Close without an adapter-triggered whole-process exit.

### M5 — Mount Wasm while preserving the SDK

- [ ] The chosen existing guests run with documented compatibility.
- [ ] Looping guests hit execution limits.
- [ ] Invalid drawing/resource requests fail safely.
- [ ] Stale completions are discarded.
- [ ] Guest activity does not starve chrome input.

### M6 — Complete browser behavior

- [ ] The user can complete the intended daily workflows.
- [ ] Failures identify the responsible view/engine.
- [ ] Every retained feature has a tested implementation or a clearly documented unsupported
  state.

### M7 — Measure, compare, and retire the old display path

- [ ] Evidence shows the normal page readback path is eliminated.
- [ ] No major functional regression in the agreed workflows remains.
- [ ] Lifecycle tests show no unbounded resource growth.
- [ ] Remaining regressions have an explicit decision.

## Decisions log

Newest last. Each entry says what was decided and why.

1. **No IPC: engines are compiled into `sig` and run in its process** (§18's initial decision).
   Cargo features on `sighurt-browser` select them: `engine-servo`, `engine-wasm`,
   `engine-demo`. The `default = [...]` line of `sighurt-browser/Cargo.toml` is the one small
   place that decides which engines a default build compiles and links. *Why:* it removes the
   control-transport machinery and makes native presentation into a host-owned subsurface
   possible; §18 records the cost (shared address space and fate, no sandbox claim).
2. **GPUI is gone. The chrome is drawn on the CPU (cosmic-text + tiny-skia) into Wayland SHM
   subsurfaces, and Sighurt is Wayland-only.** *Why:* §1 and §13: a small chrome needs no GPU
   renderer of its own, and the page no longer passes through a UI toolkit's renderer.
3. **Servo stays pinned at 0.5, with surfman 0.13.** *Why:* its build is cached in `target/`, and
   `servo-paint-api` 0.5.0 already has `WindowRenderingContext::new(DisplayHandle,
   WindowHandle, size)` and `new_with_refresh_driver`, which is the presentation path M1 needs
   (§14: pin one version and code against its sources, not the 0.6 documentation).
4. **One dependency graph.** Without GPUI, Servo 0.5, Wasmtime 36 and the Wayland/text stack
   (wayland-client 0.31 with the `system` backend, wayland-protocols 0.32,
   smithay-client-toolkit 0.20, calloop 0.14, cosmic-text 0.14, tiny-skia 0.11,
   raw-window-handle 0.6) resolve together. surfman and wayland-client 0.31 share wayland-sys
   0.31, so there is one libwayland. Only the Wasm engine's `screenshots` dependency pulls in
   wayland 0.30. The shared versions are pinned in the root `[workspace.dependencies]`. So far
   only Servo and Wasmtime are in `Cargo.lock`; the Wayland and text crates enter it when
   `sighurt-wayland` and `sighurt-ui` first depend on them.
5. **The root `Cargo.lock` was seeded from the Servo engine's lockfile**, and Cargo resolved the
   rest. *Why:* Servo and all of its dependencies keep exactly the versions already compiled
   (checked: every registry entry of the seed, including `mozjs_sys`, `servo` and `surfman`, is
   unchanged). Servo and the Wasm engine resolve together without `links` conflicts; they have
   not been compiled or run together yet (that is M0). The Wasm engine's dependencies were
   re-resolved in the process (for example Wasmtime 36.0.8 became 36.0.16); it has not been
   rebuilt since.
6. **Dependencies are optimized in dev builds** (`[profile.dev.package."*"] opt-level = 3`,
   `debug = false`). *Why:* Servo is unusable unoptimized, linking Servo with debug info is slow,
   and these are the settings the cached Servo artifacts were built with.
7. **`rust-version = "1.95"` and resolver 3.** *Why:* louise-pc, where Servo builds, has Rust
   1.95; resolver 3 makes Cargo choose dependency versions that support it.
8. **The pre-rewrite code is kept at git tag `gpui-baseline`**, not in the tree. The old GPUI
   crates were deleted; `sighurt-engine-servo` became a library in the root workspace.
   `sighurt-ipc` and the `sig-wasm` process stay until the Wasm engine is converted.
9. **Ids are opaque, never reused, and each kind has one process-wide counter.** `ViewId::next()`
   (and the same for every kind in `sighurt_api::ids`) is the only way to make an id, so a
   `NavigationId` an adapter allocates for a navigation its page started itself can never equal
   one the core allocated, and the same holds for `DocumentGeneration` and `RequestId`.
   *Why:* §6 requires that a late event never matches a newer object. The first version had
   shareable `IdSource`s, where uniqueness depended on the executable creating exactly one per
   kind; a second `IdSource::new()` (or a `Default` in a runtime's state) restarted at 1 and
   collided silently. A static counter makes it structural and needs no plumbing.
10. **`sighurt-ui` depends on `sighurt-api` only**, not on `sighurt-wayland` (§5 allows both).
    *Why:* it paints into buffer memory the host lends it, so it needs no Wayland types and its
    layout, editing and painting stay testable without a compositor.
11. **`EngineCommand` has no `Close`**: closing is `EngineRuntime::close_view`, which the §7
    trait already has. **Pages ask to open URLs with `OpenRequested { view, document, url,
    disposition }`**, where the disposition is `CurrentView` or `NewView`. This is the old
    protocol's `FromEngine::Open { url, new_tab }`, which had both: the old core routed
    `new_tab: false` like any load, possibly switching engines, and refused `sig:` targets
    (`gpui-baseline:sighurt-core/src/navigable.rs`). The Wasm engine needs it for guest
    `navigate` calls and link clicks, and Servo for navigations it shouldn't perform itself.
    A new view gets no `window.opener` link, as before; adding one is an API extension for when
    M4 shows it is needed.
12. **Engine contract rules** (`sighurt_api::engine`, "Event delivery"): events reach the owner
    only through `pump`, and the owner pumps every runtime it called in that iteration; events
    for one view keep their order; a pump does a bounded slice of work and keeps only the
    latest replaceable event (title, status, cursor, loading, history, text input, frame ready)
    per view; a view accepts commands as soon as `open_view` returns `Ok`, and `ViewReady`
    means it can present; `ViewFailed` leaves a reloadable view, `ViewFailed` + `ViewClosed` a
    finished one; a commit supersedes earlier pending navigations; `EngineRuntime::shutdown`
    closes everything in order before the host is released. *Why:* §7, §16, §19 and §20
    require these, and every adapter and the app loop must agree on them; Servo 0.5 shuts down
    only in `Drop for ServoInner`, and `WebView::load` returns no navigation id.
13. **An empty metrics rectangle means "no room", not "hidden"**: visibility is only
    `ViewOptions::visible` / `SetVisibility`, and a hidden view keeps the page area's metrics.
    `Metrics::is_hidden` became `is_empty`. *Why:* Servo 0.5's `WindowRenderingContext::new`
    fails for a 0 size, so a background tab with empty metrics couldn't open, and resizing a
    live WebView on every tab switch would reflow its page.
14. **Input carries what engines need without per-adapter tables**: `key` has W3C semantics
    (the press that completes a dead key is `Character("ä")`), `text` never holds control
    characters and is `None` while Control or Logo is held; `KeyCode::w3c_code()` and
    `location()` give the W3C code ("KeyA") once, for key bindings, Servo (`Code`, `Location`)
    and the Wasm engine; pointer motion, buttons and scrolling carry the seat's modifiers;
    `SetFocus(false)` makes the engine release held keys; `TextInputState` has a
    `TextPurpose` and `multiline`, and password fields get the `sensitive_data` and
    `hidden_text` hints. `HistoryChanged` carries Servo's entry list and the current index, and
    `ServiceRequest` the requesting origin as the adapter defines it. *Why:* §12 and §16; the
    old Wasm engine remembered permissions per origin (`app_origin_of`).
15. **Page-data bounds and throttles are kept from the baseline**, now in the core: text cut to
    16 KiB, URLs over 2 MiB dropped, a page-opened tab only within 5 s of user activation in
    that view, page-initiated downloads one a second per tab
    (`gpui-baseline:sighurt-core/src/{page,navigable}.rs`). *Why:* §17 and §26.
16. **Deferred to M4: `<select>` menus, colour pickers, `alert`/`confirm`/`prompt` and HTTP
    authentication.** The API has no requests for them, `native_popups` stays `false`, and until
    then Servo resolves them on drop (a `<select>` keeps its selection, `alert` is acknowledged,
    `confirm` and `prompt` are cancelled, no credentials). The old `sig-servo` didn't handle
    them either. *Why:* M4 says to extend the API only where the second real engine shows a
    missing concept; `ServiceRequested`/`ServiceCancelled` (Servo's `hide_embedder_control`)
    are the intended shape.
17. **CI on this branch never builds Servo in the main job.** `ci.yml` runs the "before a
    commit" commands from CLAUDE.md (everything but `sighurt-engine-servo` and
    `sighurt-browser`'s default features); `servo.yml` runs clippy on `sighurt-engine-servo` in
    the root workspace when the API, the host, Servo's crate or the lockfile change. *Why:*
    §24; the old jobs would have compiled Servo on every push without its build dependencies.
