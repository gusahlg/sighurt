# Sighurt — CLAUDE.md

Sighurt (binary `sig`) is a small, configurable, Wayland-only browser: a host with interchangeable content engines compiled in, and browser chrome drawn independently of them. It is being rewritten on the `rewrite` branch.

- **The specification is `sighurt-rewrite-plan.md`.** Read the sections your task touches before you start. Where it disagrees with README, DOCS, SECURITY, CONTRIBUTING or any other older document, the plan wins: those still describe the old design until milestone M7.
- **The tracker is `docs/rewrite/STATUS.md`**: milestones M0–M7 with their pass criteria, and the log of decisions already made. Don't reopen a logged decision without new evidence. Record new decisions there, and tick a pass criterion only with evidence (a test, a measurement, a run) that you name.
- **The pre-rewrite code** (GPUI chrome, engines as separate processes over `sighurt-ipc`) is at git tag `gpui-baseline`: `git show gpui-baseline:<path>`.

Sighurt is a fork of Oxide by Nikhil Ranjan (github.com/niklabh/oxide, Apache-2.0). Keep the attribution intact (LICENSE, NOTICE, `authors` in `[workspace.package]`, `sig --version`).

---

## Machine rules

The user's machines are shared with another Claude session. Breaking these rules can crash a machine.

- Run **every** cargo command as `tools/cargo <args>`, never plain `cargo`, and any other heavy command (benchmarks, GPU work, long test runs) as `tools/heavy <cmd>`. Both take the machine-wide lock `/tmp/gusahlg-heavy.lock`, which the other session takes too, cap memory at 9 G and enter `nix-shell`. Waiting for the lock is normal; never work around it. Build only the crates you need (`-p <crate>`).
- Build Servo on the second machine: `SIG_WHAT="servo build" tools/remote tools/cargo build -p <crate>` syncs this working tree to louise-pc and builds there, in its own target directory. Both machines get the same toolchain and native libraries from the nixpkgs pin in `nix/nixpkgs.nix` (Rust 1.98.1), which `shell.nix` imports. When louise-pc is busy, `tools/cargo` builds Servo here, in this desktop's `target/` (Servo is compiled there), unless the user is using the desktop: while a game runs or `/tmp/gusahlg-desktop-embargo` exists, `tools/heavy` refuses Servo jobs here.
- Never start a Wayland client on the user's desktop unless your task explicitly allows it, and never inject input there. Test Wayland code in a private compositor: `tools/headless-sway [--scale S] [--size WxH] -- <cmd>`. Inside it `wtype`, `wlrctl`, `swaymsg`, `grim` and `wayland-info` drive and inspect that compositor only. Never use ydotool or anything that writes to `/dev/uinput`.
- **Never capture the user's desktop.** `grim` (or any screenshot tool) run outside `tools/headless-sway` photographs every monitor of the user's live session, with whatever private windows are open. Screenshots come only from a private compositor; live-desktop checks use logs and diagnostics.
- **Never build a copy of the workspace into the shared `target/`.** Cargo identifies path crates by their path relative to the workspace root, so a copy's artifacts are taken as up to date for the real tree and break every other agent's builds. Build copies with their own `CARGO_TARGET_DIR`.

---

## Architecture

| Layer | Crate | Owns | Does not own |
|---|---|---|---|
| Browser core | `sighurt-core` | Tabs, engine selection, commands, navigation coordination, permission decisions, session state | Windows, surfaces, glyphs, GPU contexts, any I/O |
| Chrome | `sighurt-ui` | Address/command editing, chrome layout, hit testing, CPU painting | Web layout, engine internals, guest permissions |
| Wayland host | `sighurt-wayland` | Connection, surface roles, target lifetime, placement, scaling, input normalization, SHM presentation, frame scheduling | DOM layout, guest execution, engine rendering |
| Engines | `sighurt-engine-*` | Interpreting, running and rendering content; engine-local state and history | Tab policy, the chrome, global commands |
| Executable | `sighurt-browser` (`sig`) | Config and session files, the engine registry, the owner event loop, effects, OS services, diagnostics | Browser policy |

- **Engines are compiled in and run in-process; there is no IPC.** Cargo features on `sighurt-browser` (`engine-servo`, `engine-wasm`, `engine-demo`) decide which engines exist: the `default = [...]` line in `sighurt-browser/Cargo.toml` is the list of engines compiled into `sig`. Runtime configuration decides which of them may open pages. One runtime per engine kind serves all of its views.
- **The host creates every surface**: the root `xdg_toplevel` (an opaque fallback), a chrome subsurface, one page subsurface per live view (mapped only while selected), and overlays. The chrome and the engines present into host-managed targets through a target lease and a per-layout frame permit; the desktop compositor combines the surfaces.
- **The chrome is drawn on the CPU** with cosmic-text and tiny-skia into shared-memory buffers. GPUI is gone.
- **Servo presents GL straight into its subsurface** (`WindowRenderingContext` on surfman). The normal display path never reads page pixels back to the CPU.
- **The core is ordinary Rust**: roughly `update(state, event) -> Vec<Effect>`, with no I/O. Engines report facts as events and never change browser state; the executable carries out the effects.
- **The Wasm engine is a library too** (decision 22): `sighurt-ipc` and the `sig-wasm` process are gone. Its guests run on its worker threads and draw on the CPU into the host's SHM buffers.

## Crates and dependency rules

| Crate | Modules | Project dependencies |
|---|---|---|
| `sighurt-api` | `ids`, `geometry`, `input`, `engine`, `events`, `services`, `commands` | None, and no third-party crates |
| `sighurt-core` | `state`, `tabs`, `navigation`, `router`, `commands`, `keys`, `permissions`, `config`, `session` | api |
| `sighurt-wayland` | `connection`, `window`, `targets`, `placement`, `scale`, `input`, `text_input`, `clipboard`, `scheduler`, `shm` | api |
| `sighurt-ui` | `state`, `layout`, `hit_test`, `text_edit`, `text`, `chrome`, `paint_cpu`, `accessibility` | api (it paints into memory the host lends it) |
| `sighurt-engine-servo` | `runtime`, `view`, `delegate`, `presentation`, `input`, `navigation` | api, wayland; Servo 0.5 |
| `sighurt-engine-wasm` | `adapter` (`WasmRuntime`), `view`, `navigation`, `engine` (Wasmtime, limits), `live` and `pool` (guest threads), `capabilities` (host functions), `draw` and `raster` (CPU drawing), `wayland` (its `CpuTarget` for the host's SHM lease), and a module per guest subsystem | api, wayland |
| `sighurt-engine-demo` | `lib` (the runtime and its pages: static pattern, animation, slow frame, resize, failed navigation, crash), `paint` | api, wayland |
| `sighurt-browser` | `main`, `app`, `effects`, `registry`, `services`, `persistence`, `diagnostics` | All of the above; engines optional |
| `sighurt-sdk` | The guest-side Wasm API | None: independent of every native crate |

- `sighurt-core` compiles and tests without GPUI, Wayland, Servo, Wasmtime or a graphics driver.
- `sighurt-api` holds no `gpui::Entity`, `wl_surface`, `Rc<Servo>`, graphics-API type or `Any` escape hatch. It is an internal contract rebuilt with the workspace, not a plugin ABI.
- Only the executable chooses which engines and chrome are installed.
- Engines talk outward through typed events and service requests, never through a reference to the browser.
- Rendering choices never change tab or navigation state types.
- Network and filesystem effects live in the executable; the core may parse and validate.
- Shared third-party versions are pinned in the root `[workspace.dependencies]`; depend on them with `<crate>.workspace = true`.
- Every module starts with a doc comment saying what it owns and what it must not. Keep it true.

## Build commands

```bash
# Crates that need neither Servo nor Wasmtime
tools/cargo check -p sighurt-api -p sighurt-core -p sighurt-wayland -p sighurt-ui -p sighurt-engine-demo
tools/cargo check -p sighurt-browser --no-default-features
tools/cargo test -p sighurt-api -p sighurt-core
tools/cargo clippy -p <crate> --all-targets -- -D warnings
tools/cargo fmt -p <crate>

# sig with only the demo engine, for host and chrome work; run it in a private compositor
# (tools/heavy outside: headless-sway replaces XDG_RUNTIME_DIR, which tools/heavy needs)
tools/cargo build -p sighurt-browser --no-default-features --features engine-demo
tools/heavy tools/headless-sway -- target/debug/sig
# sig's unit tests, and its end-to-end tests in a private headless sway (builds sig with the
# demo engine; prints PASS or FAIL per check, screenshots and logs in OUT)
tools/cargo test -p sighurt-browser -p sighurt-engine-demo --no-default-features --features engine-demo
sighurt-browser/tests/headless.sh OUT

# Anything that builds Servo (sig's default features, sighurt-engine-servo, the M0 and M1
# experiments): on louise-pc, or with the same commands without `tools/remote` here (see
# "Machine rules")
SIG_WHAT="servo build" tools/remote tools/cargo build -p sighurt-browser
SIG_WHAT="servo build" tools/remote tools/cargo clippy -p sighurt-engine-servo -p sighurt-browser --all-targets -- -D warnings
# sig with Servo and Wasm, on the GPU, in a private compositor (with SIG_DIAG=1 it prints the
# GL renderer and a line per frame)
tools/heavy tools/headless-sway --renderer gles2 -- target/debug/sig https://example.com/

# Wasm guests (always release); the Wasm engine's tests run the first four
tools/cargo build --target wasm32-unknown-unknown --release -p hello-sighurt -p raf-demo -p gradient-demo -p typography-demo
tools/cargo build -p sighurt-sdk --target wasm32-unknown-unknown

# Compositor tests of the host and the engines (each script says how to run it)
sighurt-wayland/tests/headless.sh
sighurt-engine-wasm/tests/headless.sh [OUT]
SIG_WHAT="servo test" tools/remote sighurt-engine-servo/tests/headless.sh

# Before a commit: everything except Servo here, Servo on louise-pc (above)
tools/cargo fmt --all
tools/cargo clippy --workspace --exclude sighurt-engine-servo --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-engine --exclude wpe-coexist --all-targets -- -D warnings
tools/cargo clippy -p sighurt-browser --no-default-features --features engine-demo,engine-wasm --all-targets -- -D warnings
tools/cargo test --workspace --exclude sighurt-engine-servo --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-engine --exclude wpe-coexist
tools/cargo test -p sighurt-browser --no-default-features --features engine-demo,engine-wasm
```

`--workspace` alone, `sighurt-browser` with its default features, `m0-coexist` and `m1-mixed` compile Servo. Dependencies are built at `opt-level = 3` without debug info even in dev builds (Servo is unusable unoptimized); the Servo artifacts in `target/` were built with the same dependency settings, so keep them.

## Commands, keys and configuration

- Everything the browser does is a named command, `<area>.<action>` with kebab-case actions and an optional argument after whitespace: `tab.new`, `tab.close [n]`, `tab.next`, `tab.previous`, `tab.select <n>`, `tab.last`, `page.open <url>`, `page.reload`, `page.stop`, `page.back`, `page.forward`, `page.zoom-in`, `page.zoom-out`, `page.zoom-reset`, `location.focus`, `browser.home`, `browser.quit`. Users write these names in their configuration, so keep them.
- A command string is parsed once into `sighurt_api::commands::Command`; unknown names and bad arguments are errors when the configuration loads. `Display` gives the canonical string back. The core carries commands out, except `location.focus`, which the chrome handles. Keys and buttons use the same path; don't add a second one.
- To add a command: a variant in `Command` (and `NAMES`, `name`, parsing, `Display`), its handling in `sighurt-core`, and the README's command table.
- **Keys live only in the user's configuration**: no key handlers in code, no default bindings. Bindings match the layout's key value by default and the physical key when a binding asks for it; AltGr is never Ctrl+Alt. A consumed key press also consumes its release.
- Configuration is TOML: `{config_dir}/sighurt/config.toml` over built-in defaults, with a schema version. The core parses and validates it; the executable reads the file. Unknown keys are errors. An invalid reload keeps the previous configuration and reports the problem. Key bindings and theme apply live; a renderer change needs a restart.

## The engine contract

- Engines implement `sighurt_api::engine::EngineRuntime`; read its documentation, including "Event delivery". `Ok` from `open_view` or `command` means accepted, not finished: results arrive as `EngineEvent`s from `pump`, which returns `PumpStatus` so the loop knows when it may sleep. Unsupported operations return `EngineError::Unsupported` and are declared in `EngineCapabilities`; nothing pretends to succeed.
- Events arrive only through `pump`, so the owner pumps every runtime it called in that loop iteration. Events for one view keep their order. A pump does a bounded slice of work and returns `Pending` when more remains; replaceable events (title, status, cursor, loading, history, text input, frame ready) keep only the latest per view. A view accepts commands as soon as `open_view` returns `Ok`; `ViewReady` means it can present.
- `ViewFailed` means the document is gone but the view can be reloaded; a view that can't continue is `ViewFailed` then `ViewClosed`. `RuntimeFailed` is the whole engine. `EngineRuntime::shutdown` closes everything in an explicit order before the host is released; never rely on `Drop` order for native lifetimes.
- The trait is not `Send` or `Sync`. Never add an unsafe `Send` impl to fit an engine into an executor. Work on other threads wakes the owner loop through `EngineWaker`.
- Every view event carries its `ViewId`, navigation events a `NavigationId`, document-bound requests a `DocumentGeneration`. Each id kind has one process-wide counter (`ViewId::next()`), so ids are never reused and a late event from a closed view is recognisably stale. An adapter allocates the `NavigationId` of a navigation its page started itself; a commit supersedes the view's earlier pending navigations.
- A page asks to open a URL with `OpenRequested` (in its own tab or a new one); the core routes it like any URL, possibly to another engine. An engine performs only navigations it owns and hands the rest over this way.
- Keys: `KeyCode` is the evdev code (`w3c_code()` gives "KeyA"), `key` has W3C `KeyboardEvent.key` semantics (the composed "ä" on the press that completes a dead key), `text` is what the press types; editors insert `text`. `SetFocus(false)` makes the engine release every key its content holds as pressed. Pointer events carry the seat's modifiers.
- An empty `Metrics` rectangle means "no room", not "hidden": visibility is `SetVisibility`, and hidden views keep the page area's metrics. `open_view` with empty metrics still succeeds.
- Delegate callbacks only queue events; they are handed over after the engine call returns. Nothing mutates browser state re-entrantly.
- Positions arrive surface-local to the view's target. Convert them once with the view's `Metrics` (`to_buffer`); never subtract the chrome's offset or apply the scale a second time. Buffer sizes come from `Scale::buffer_size`.
- `FrameReady` means "has something new to paint", not "displayed". Keep invalidation, frame callbacks and buffer release distinct. No unconditional ticks, no page-sized CPU copies on the normal path.
- Adding an engine: a crate `sighurt-engine-<name>` depending on api and wayland, a feature `engine-<name>` on `sighurt-browser`, and an entry in its `registry`.
- **Servo** is pinned at 0.5 with surfman 0.13. Read the 0.5 sources in `~/.cargo/registry/src/`, not the current (0.6) documentation. One runtime per process; its API calls and GL presentation stay on the owner thread. The adapter never calls `process::exit` or `_exit` and never installs panic hooks, logging or a TLS provider: the executable owns process-wide setup. The crate is `Apache-2.0 AND MPL-2.0`; files adapted from Servo keep their MPL notices.

## Security

- **In-process native engines are cooperative, not a sandbox.** The contracts keep correct code correct; they do not contain an exploited Servo, and a native crash takes the browser down. Never describe the in-process design as sandboxed. OS isolation would need a restricted worker process, which is a separate decision.
- **Untrusted Wasm guests get only the host functions registered in the linker.** Host access is additive and opt-in, with no wildcards. Grants are enforced at the host-function boundary, so a guest can't grant itself anything.
- **Never link WASI** in the Wasm engine: no filesystem, no environment variables, no raw sockets.
- **Validate every guest memory access** with the helpers below; a malicious guest can pass any ptr/len. Bound what a guest can make the host allocate (draw lists, images, media, network responses, caches), not just its linear memory.
- Privileged services (clipboard, downloads, file pickers, external URLs, permissions) go through `sighurt_api::services`: the core decides, per view and `DocumentGeneration`; the executable performs; unknown or unimplemented requests fail closed.
- Page titles, status text and URLs are untrusted: display them contained. A page cannot write the address bar, run a browser command through a magic URL, open `sig://` pages, or open a file chooser without the policy allowing it.
- The core bounds what pages send: text (titles, status) is cut to 16 KiB, and URLs over 2 MiB are dropped, never shortened. A page opens a new tab only within 5 s of a click or key press in its view, one tab per activation. Page-initiated downloads are throttled to one a second per tab.
- Password fields go to the input method with the `sensitive_data` and `hidden_text` hints and no surrounding text.
- Guests never see native handles: no Wayland objects, chrome target, raw pointers or GPU APIs.
- Diagnostics never log clipboard contents, credentials or page data. Don't deserialize precompiled Wasmtime artifacts that the runtime did not produce and manage itself.

## Review questions

Ask these of every change (plan §28):

1. Is every resource/state field owned by one clear layer?
2. Can a delayed event identify the view, document, or layout it belongs to?
3. Did this change add a page-sized copy or an unconditional tick?
4. Is a renderer detail leaking into the core or guest-facing contract?
5. Is a claim backed by a prototype/test, or explicitly still a proposal?

## Acceptance criteria

Hard architectural requirements (plan §26):

- Zero routine CPU page readbacks on the optimized Servo display path.
- Zero chrome repaints caused solely by page animation.
- No permanent repaint timer in the idle chrome/demo path.
- No unbounded queue, buffer-generation, or frame accumulation.
- Core tests require no windowing or engine initialization.
- Renderer changes do not require rewriting browser state/navigation code.

Hardware-dependent thresholds come only after a baseline exists, with the machine, workload, driver and method stated.

---

## Adding a host function to the WASM engine

All of this happens in `sighurt-engine-wasm` and `sighurt-sdk`; the browser is not involved.

**1. Significant state gets its own module** in `sighurt-engine-wasm/src/` (e.g. `websocket.rs`) with a `<Feature>State` struct and `register_<feature>_functions(linker: &mut Linker<HostState>) -> Result<()>`, called from the bottom of `register_host_functions`. Add `pub mod <feature>;` to `lib.rs`.

**2. Add state to `HostState`** in `capabilities.rs`. It derives `Default`, so a lazily started subsystem is just:
```rust
pub ws: Arc<Mutex<Option<crate::websocket::WsState>>>,
```
Use `with_started(&slot, start, f)` to start it on first use and `with(&slot, f)` to use it only if started.

**3. Register the host function**:
```rust
linker.func_wrap("oxide", "api_my_feature",
    |mut caller: Caller<'_, HostState>, ptr: u32, len: u32| -> u32 {
        let Some(s) = guest_str(&caller, ptr, len) else { return 0 };
        // … host logic …
        0
    },
)?;
```

**4. Expose it in the SDK** (`sighurt-sdk/src/lib.rs`). A wrapper that takes and returns exactly what the host does is declared with `host_fns!`:
```rust
host_fns! {
    /// Brief doc comment.
    pub fn my_counter(id: u32) -> u32 = "api_my_counter";
}
```
Anything that converts arguments (strings, buffers, packed results) gets an import in the `extern "C"` block and a hand-written wrapper:
```rust
#[link_name = "api_my_feature"]
fn _api_my_feature(ptr: u32, len: u32) -> u32;

/// Brief doc comment.
pub fn my_feature(s: &str) -> u32 {
    unsafe { _api_my_feature(s.as_ptr() as u32, s.len() as u32) }
}
```

### Memory helpers (`capabilities.rs`)

| Helper | Use |
|--------|-----|
| `guest_bytes(&caller, ptr, len)` | Read raw bytes; `None` if out of bounds |
| `guest_str(&caller, ptr, len)` | Read UTF-8; `None` if out of bounds or invalid |
| `write_guest(&mut caller, ptr, bytes)` | Write bytes; `false` if out of bounds |
| `write_prefix(&mut caller, ptr, cap, bytes)` | Write what fits in a `cap`-byte buffer; returns the length |

Data always crosses the boundary as `(ptr: u32, len: u32)` pairs. Never pass Rust references across it.

### The `"oxide"` import module is permanent

The guest ABI's import module is named `"oxide"` (Oxide's app ABI, comparable to `wasi_snapshot_preview1`). **Never rename** the `"oxide"` strings in `func_wrap` calls or the SDK's `#[link(wasm_import_module = "oxide")]`. Every compiled Oxide app, and the upstream `oxide-sdk` crate, depends on that name.

## Guest app contract (WASM engine)

1. Export `start_app()`, called once on load.
2. Optionally export `on_frame(dt_ms: u32)`, called about 60 times a second while the page is shown and once a second while it is hidden (fuel replenished each call).
3. Optionally export `on_timer(callback_id: u32)`, called when a `set_timeout`/`set_interval`/`request_animation_frame` fires.
4. Optionally export `on_event(callback_id: u32)`, called once per pending built-in or custom event each frame (before timers and `on_frame`).

   `on_timer` and `on_event` are delivered from the frame loop, so they only run if the module also exports `on_frame`.
5. Build as `[lib] crate-type = ["cdylib"]` for `wasm32-unknown-unknown`.
6. Import everything from the `"oxide"` module (via `sighurt-sdk`), never WASI.

The engine has no widget kit: guests draw their own UI with the canvas API and poll input (`text_input()` for typed text). The rewrite keeps this contract: guests that use `on_frame` keep getting it while visible, and intentional changes are versioned separately from the native engine contract.

## Naming conventions

| Entity | Convention | Example |
|--------|------------|---------|
| Host function (linker, module `"oxide"`) | `api_<category>_<action>` | `api_ws_connect`, `api_canvas_rect` |
| SDK FFI import | `_api_<name>` | `_api_ws_connect` |
| SDK public wrapper | `<category>_<action>` | `ws_connect`, `canvas_rect` |
| State struct | `<Feature>State` | `WsState`, `RtcState` |
| Register function | `register_<feature>_functions` | `register_ws_functions` |
| Constants | `UPPER_SNAKE_CASE` | `WS_OPEN`, `KEY_ENTER` |
| Command | `<area>.<action>` | `tab.new`, `page.zoom-in` |
| Engine crate / feature / config name | `sighurt-engine-<name>` / `engine-<name>` / `<name>` | `sighurt-engine-servo` / `engine-servo` / `servo` |
| Identity | an opaque newtype, made only by its `next()` | `ViewId`, `DocumentGeneration`, `LayoutEpoch` |

---

## LLM coding guidelines

**Think before coding.**
- State assumptions explicitly. If multiple interpretations exist, present them — don't pick silently.
- If something is unclear, stop and ask before writing code.
- If a simpler approach exists, say so. Push back when warranted.

**Simplicity first.**
- Minimum code that solves the problem. Nothing speculative.
- No abstractions for single-use code. No "flexibility" that wasn't asked for.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

**Surgical changes.**
- Touch only what you must. Don't improve adjacent code, comments, or formatting.
- Match the existing style (e.g. `block_on` for async in RTC, unbounded mpsc + background task in websocket).
- When your changes make something unused (import, variable, function), remove it. Don't remove pre-existing dead code unless asked.
- Every changed line should trace directly to the request.

**Verify your work.**
- `sighurt-api`, `-core`, `-wayland`, `-ui`, `-engine-demo`: `tools/cargo check`, `test` and `clippy -- -D warnings` on the crate.
- `sighurt-browser`: `tools/cargo check -p sighurt-browser --no-default-features`, plus `--features engine-demo`.
- `sighurt-engine-servo`, or anything built with Servo: `SIG_WHAT="servo build" tools/remote tools/cargo check -p sighurt-engine-servo` (and `-p sighurt-browser` with its default features).
- WASM engine change: `tools/cargo build -p sighurt-engine-wasm`.
- SDK change: `tools/cargo build -p sighurt-sdk --target wasm32-unknown-unknown`.
- Example change: `tools/cargo build --target wasm32-unknown-unknown --release -p <example>`.
- Wayland behavior: run it under `tools/headless-sway`, never on the user's desktop.
- Host functions: confirm registration in `register_host_functions` (or a `register_*_functions` it calls) and an SDK wrapper with a doc comment.
- Commands: confirm there is no hardcoded key handler.

---

## Common pitfalls

- **`Cargo.lock` was seeded from Servo's lockfile** so Servo keeps the dependency versions already compiled. Don't run a broad `cargo update`: changing any of Servo's dependencies means rebuilding all of Servo.
- **Servo 0.5, not 0.6.** Upstream documentation describes 0.6. Check every API against the pinned sources.
- **Scale is a rational.** Keep `Scale` (numerator over 120) and derive buffer sizes and input transforms from the actual rounded dimensions, not from a float factor.
- **Empty is not hidden.** A zero-sized rectangle means the layout has no room: the host unmaps the surface and the engine keeps its last size, allocates nothing and presents nothing (Servo can't make a 0×0 context). Hiding a view (a background tab) is `SetVisibility(false)` with its metrics unchanged. Never wait for a frame callback on an unmapped or hidden surface.
- **`HostState.memory` is set before instantiation**, so `caller.data().memory` is always there inside a host function. The helpers `expect` it.
- **`block_on` vs spawn.** RTC uses `runtime.block_on` for synchronous host calls; WebSocket uses `runtime.spawn` for background tasks. Match the subsystem you're in.
- **`Arc<Mutex<Option<T>>>` lazy init.** Subsystems such as rtc, ws, midi, fetch, sse and workers are `None` until first use; go through `with_started` / `with`.
- **Guest limits.** Each callback (`on_frame`, `on_timer`, `on_event`) gets 50M instructions (`FRAME_FUEL_LIMIT`) and `start_app` 500M (`FUEL_LIMIT`), and each has a wall-clock limit (1 s, `start_app` 10 s) that time spent waiting in host functions doesn't count against (`SandboxPolicy` in `engine.rs`). Host functions don't count against fuel and can't call back into the guest.
- **Guests run on worker threads, in `sig`'s process.** Every view of the Wasm engine shares its runtime, and several `sig` processes may run at once, so state shared between pages (the kv store) works without locks; see `kv.rs`.
