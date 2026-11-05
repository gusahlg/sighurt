# Sighurt — CLAUDE.md

Sighurt (binary `sig`) is a small, configurable browser with two frontends in one binary: a Wayland GUI (a host with interchangeable content engines compiled in, and browser chrome drawn independently of them) and a terminal frontend (`sighurt-tui`; `docs/rewrite/TUI.md`, decisions 89–98) that shows the same engines' pages as kitty or sixel graphics or as reflowed text from a semantic snapshot of the page (text mode needs an engine with the `semantics` capability: Servo, WPE WebKit and the demo engine; CEF, Ladybird and NetSurf are opt-in and have none), and `sig --dump URL`, which prints a page as text with no terminal or window. `main` picks the frontend: `--gui`/`--tui`, else the terminal when stdout is one and `TERM` isn't `dumb`, else the window. It is being rewritten on the `rewrite` branch.

- **The specification is `sighurt-rewrite-plan.md`.** Read the sections your task touches before you start. README, DOCS, SECURITY and CONTRIBUTING describe the rewrite as it is; where any document disagrees with the plan, the plan wins, as amended by the decisions in STATUS. `docs/rewrite/extension-api.md` is the design of the configuration and of the keymap and chrome APIs, and `docs/rewrite/WPE.md` of the WPE WebKit engine.
- **The tracker is `docs/rewrite/STATUS.md`**: milestones M0–M7 with their pass criteria, and the log of decisions already made. Don't reopen a logged decision without new evidence. Record new decisions there, and tick a pass criterion only with evidence (a test, a measurement, a run) that you name.
- **The pre-rewrite code** (GPUI chrome, engines as separate processes over `sighurt-ipc`) is at git tag `gpui-baseline`: `git show gpui-baseline:<path>`.

Sighurt is a fork of Oxide by Nikhil Ranjan (github.com/niklabh/oxide, Apache-2.0) and is licensed `AGPL-3.0-or-later` (decision 88); `sighurt-sdk` and `examples/` stay `Apache-2.0`. `sighurt-engine-netsurf` is `GPL-2.0-only` (decision 119): `engine-netsurf` is never a default feature, and a `sig` built with it is for personal use and must not be distributed. Keep the attribution intact (LICENSE, LICENSE-APACHE with Oxide's notice, NOTICE, `authors` in `[workspace.package]`, `sig --version`).

---

## Machine rules

The user's machines are shared with another Claude session. Breaking these rules can crash a machine.

- Run **every** cargo command as `tools/cargo <args>`, never plain `cargo`, and any other heavy command (benchmarks, GPU work, long test runs) as `tools/heavy <cmd>`. Both take the machine-wide lock `/tmp/gusahlg-heavy.lock`, which the other session takes too, cap memory at 9 G and enter `nix-shell`. Waiting for the lock is normal; never work around it. Build only the crates you need (`-p <crate>`).
- Servo (and everything that links it, `sig`'s default features included) builds here, in this desktop's `target/` where it is compiled, unless the user is using the desktop: while a game runs or `/tmp/gusahlg-desktop-embargo` exists, `tools/heavy` refuses Servo jobs here. Then build on the second machine: `tools/remote <cmd>` syncs this working tree to louise-pc and runs `<cmd>` there, in its own target directory (command under "Build commands").
- Both machines take the Rust toolchain (the pin's 1.98.1, the workspace's `rust-version`), their native libraries and the WPE WebKit prefix from `shell.nix` and the nixpkgs pin in `nix/nixpkgs.nix`; `tools/cargo` and `tools/heavy` run in that shell. The pin's Rust is this desktop's own, so Servo's build in `target/` stays fresh; louise-pc's own Rust (1.95) is never used.
- Never start a Wayland client on the user's desktop unless your task explicitly allows it, and never inject input there. Test Wayland code in a private compositor: `tools/headless-sway [--scale S] [--size WxH] -- <cmd>`. Inside it `wtype`, `wlrctl`, `swaymsg`, `grim` and `wayland-info` drive and inspect that compositor only. Never use ydotool or anything that writes to `/dev/uinput`.
- **Never capture the user's desktop.** `grim` (or any screenshot tool) run outside `tools/headless-sway` photographs every monitor of the user's live session, with whatever private windows are open. Screenshots come only from a private compositor; live-desktop checks use logs and diagnostics.
- **Never build a copy of the workspace into the shared `target/`.** Cargo identifies path crates by their path relative to the workspace root, so a copy's artifacts are taken as up to date for the real tree and break every other agent's builds. Build copies with their own `CARGO_TARGET_DIR`.

---

## Architecture

| Layer | Crate | Owns | Does not own |
|---|---|---|---|
| Browser core | `sighurt-core` | Tabs, engine selection, commands, navigation coordination, permission decisions, session state | Windows, surfaces, glyphs, GPU contexts, any I/O |
| Chrome | `sighurt-ui` (the standard chrome and the chrome API), `sighurt-chrome-line` | Address/command editing, chrome layout, hit testing, drawing scenes | Web layout, engine internals, guest permissions, what a key does (the keymap's), pixels |
| Render | `sighurt-render` (`docs/rewrite/RENDER.md`), `sighurt-render-wgpu`, `sighurt-render-vulkan` | Scenes and the pen chromes draw with, text shaping and glyph rasterization, the `Backend` trait, the CPU backend, the GPU preparation (meshes, atlas, WGSL) and the wgpu and Vulkan backends | Host targets and Wayland objects, what is drawn, browser state |
| Wayland host | `sighurt-wayland` | Connection, surface roles, target lifetime, placement, scaling, input normalization, SHM presentation, frame scheduling | DOM layout, guest execution, engine rendering |
| Engines | `sighurt-engine-*` | Interpreting, running and rendering content; engine-local state and history | Tab policy, the chrome, global commands |
| Runtime | `sighurt-runtime` | Frontend-neutral: config and session files, the engine registry and descriptors, carrying out effects, OS services, diagnostics, shutdown; the `Frontend` trait | Browser policy, windows, terminals, pixels |
| GUI frontend | `sighurt-gui` | The Wayland owner loop, the chrome's targets and placement, binding engines to Wayland targets | Browser policy, engine internals |
| Terminal frontend | `sighurt-tui` | The terminal owner loop, terminal I/O, the cell grid and the terminal chrome, the page's CPU target and its kitty and sixel encodings, text mode (reflowing a semantic snapshot), `--dump`, binding engines to the CPU target | Browser policy, engine internals, anything of Wayland or sighurt-ui |
| Executable | `sighurt-browser` (`sig`) | Arguments, process-wide setup, choosing the frontend | Everything else |

- **Engines are compiled in and run in-process; there is no IPC.** Cargo features on `sighurt-browser` (`engine-servo`, `engine-wasm`, `engine-wpe`, `engine-demo`, `engine-cef`, `engine-ladybird`, `engine-netsurf`) decide which engines exist. The `default = [...]` line in `apps/sighurt-browser/Cargo.toml` is what a default `sig` contains (Servo, Wasm and WPE WebKit). `engine-cef`, `engine-ladybird` and `engine-netsurf` are in no default set. Runtime configuration decides which compiled engines may open pages: Servo is the default engine; the others open what `[routing.<name>]` and `page.open-in <name>` give them. `page.switch-engine` cycles the enabled web engines in descriptor order: servo, wpe, ladybird, cef, netsurf. One runtime per engine kind serves all of its views. Keymaps and chromes are compiled in the same way (`chrome-line`).
- **The host creates every surface**: the root `xdg_toplevel` (an opaque fallback), a subsurface for each surface the chrome names (at most 8), one page subsurface per live view (mapped only while selected), and overlays. The chrome and the engines present into host-managed targets through a target lease and a per-layout frame permit; the desktop compositor combines the surfaces.
- **The chrome draws scenes; a renderer renders them.** A chrome records each surface with sighurt-render's `Pen` (window-logical, no pixels); the backend `[ui] renderer` names (`auto`, `cpu`, `wgpu`, `vulkan`; a default `sig` has `cpu` and `wgpu`, and `auto` picks `cpu` until wgpu is measured at worst equal, RENDER.md §14.5) renders it. The CPU backend draws with tiny-skia and cosmic-text into shared-memory buffers, and commits nothing when a surface's scene and metrics are those of its last frame. The wgpu backend (Vulkan only, never wgpu's GL backend, which would share Servo's EGL display) binds each chrome target's native surface when the target is made, keeps it across hides, and renders and presents once per frame permit through a MAILBOX swapchain; a lost surface is bound again once, then (and on a device failure) the CPU backend replaces it for the whole window, every chrome target made again in shared memory (RENDER.md §14.4, §15). The Vulkan backend (`vulkan`, ash 0.38, opt-in, never `auto`'s) does the same with its own swapchains, the shared WGSL compiled to SPIR-V by naga in `build.rs`, and one frame in flight per backend. One font system, loaded once, is shared by the chrome and the backend. GPUI is gone.
- **Servo presents GL straight into its subsurface** (`WindowRenderingContext` on surfman). The normal display path never reads page pixels back to the CPU.
- **WPE WebKit attaches its DMA-BUFs to its subsurface** (the WPE Platform API; WebKit's UI-process half runs on the owner thread, its pages in WebKit's own web processes). Where the compositor has no DMA-BUF, it falls back to shared memory with one copy per frame, counted and reported as the fallback.
- **CEF presents CPU frames from Chromium's `OnPaint`** (dirty rectangles into a staging buffer, then the whole frame into the host's lease). `libcef.so` is `dlopen`ed when the runtime first starts, from the prefix `nix/build-cef.sh` installs (`SIGHURT_CEF_PREFIX`, `nix/cef.nix`, `/data/sig-builds/deps/cef-150.0.22`). Chromium's renderer, GPU and network processes are `sig-cef-helper`. CEF's UI thread runs its multi-threaded message loop; callbacks only queue events and wake the owner. Chromium needs a Wayland or X11 display even windowless, so the runtime refuses to start when the session has neither `WAYLAND_DISPLAY` nor `DISPLAY` (in the terminal, only with a display). No `semantics`, so no text mode and no `--dump`.
- **Ladybird presents by copying one frame** from its backing store into the host's CPU target. A C++ shim (`nix/build-ladybird-shim.sh`, against the patched Ladybird of `nix/ladybird.nix` and `nix/ladybird/*.patch`) is `dlopen`ed when the first page opens. The patched package is built on louise-pc and brought here by `nix/pull-ladybird-patched.sh`. Ladybird's pages run in its own helper processes. No `semantics`.
- **NetSurf 3.11 paints on the CPU** into the host's shared-memory target. `build.rs` compiles the NetSurf core and this crate's C frontend and links them statically. The crate is `GPL-2.0-only` (decision 119): never a default feature, and a `sig` built with it must not be distributed. No `semantics`.
- **The core is ordinary Rust**: roughly `update(state, event) -> Vec<Effect>`, with no I/O. Engines report facts as events and never change browser state; the executable carries out the effects.
- **The Wasm engine is a library too** (decision 22): `sighurt-ipc` and the `sig-wasm` process are gone. Its guests run on its worker threads and draw on the CPU into the host's SHM buffers.

## Crates and dependency rules

The workspace is one Cargo workspace. Package names are unchanged; the directories are grouped:

```text
sighurt/
├── crates/        sighurt-api, sighurt-core, sighurt-runtime
├── frontends/     sighurt-gui, sighurt-tui, sighurt-wayland
├── ui/            sighurt-ui
├── render/        sighurt-render, sighurt-render-wgpu, sighurt-render-vulkan
├── engines/       sighurt-engine-servo, sighurt-engine-wpe, sighurt-engine-wasm,
│                  sighurt-engine-demo, sighurt-engine-cef, sighurt-engine-ladybird,
│                  sighurt-engine-netsurf, sighurt-semantic
├── extensions/    sighurt-chrome-line, sighurt-config-example
├── sdk/           sighurt-sdk
├── apps/          sighurt-browser (`sig`)
├── examples/      Wasm guests
├── experiments/   standalone investigations
└── bench/, tools/, nix/, docs/
```

| Crate | Modules | Project dependencies |
|---|---|---|
| `sighurt-api` | `ids`, `geometry`, `input`, `engine`, `events`, `services`, `commands`, `settings`, `keymap` | None, and no third-party crates |
| `sighurt-core` | `state`, `tabs`, `navigation`, `router`, `commands`, `keys`, `permissions`, `config`, `session` | api |
| `sighurt-wayland` | `connection`, `window`, `targets`, `placement`, `scale`, `input`, `text_input`, `clipboard`, `scheduler`, `shm` | api |
| `sighurt-ui` | `state`, `layout`, `hit_test`, `text_edit`, `text` (sighurt-render's, re-exported), `chrome`, `paint` (the standard chrome's scenes), `accessibility`, `extension` (the chrome API), `standard` (the standard chrome through it) | api, render (it draws scenes, never pixels) |
| `sighurt-render` | `color` (`Rgba`), `scene` (items, clips, the shared snapping and glyph placement), `pen` (the scene builder), `text` (`Fonts`, `ShapedText`, `TextStyle`, the shared `FontSystemHandle`), `glyph` (rasterizing one glyph, the bounded `GlyphCache`), `shape` (path rasterization every backend shares), `backend` (the `Backend` trait, `Output`, `RenderOutcome`, `RenderError`, `BackendStats`), `cpu` (`CpuBackend`), `gpu` (feature `gpu`: `prepare`, `Prepared`, `Atlas`, the vertex layout, `shaders.wgsl`) | api; the only crate that names cosmic-text or tiny-skia |
| `sighurt-render-wgpu` | `lib` (`WgpuBackend`: instance, adapter and device at the first bind, a surface per chrome target, presenting), `device` (pipeline, atlas textures, buffers, one pass), `testing` (feature: `headless`, `render_to_image`) | api, render (`gpu`); wgpu 30 (Vulkan, WGSL), raw-window-handle |
| `sighurt-render-vulkan` | `lib` (`VulkanBackend`: the loader at run time, instance, device at the first bind, a surface and swapchain per chrome target, presenting), `device` (the device, pipeline, atlas images, buffers, the frame slot), `target` (a surface and its swapchain), `spirv` (the SPIR-V `build.rs` makes from the shared WGSL), `testing` (feature: `headless`, `render_to_image`) | api, render (`gpu`); ash 0.38 (the loader `dlopen`ed), raw-window-handle; naga at build time |
| `sighurt-chrome-line` | `lib` (the line chrome, through the chrome API alone) | api, ui |
| `sighurt-config-example` | `lib` (the example configuration, the default keymap `example`: modes and key sequences), `table` (its bindings, in Rust), `actions` (what it computes from the browser view) | api |
| `sighurt-engine-servo` | `runtime`, `view`, `delegate`, `presentation`, `offscreen` (the terminal's GL context), `input`, `navigation`, `semantic` (page text) | api, wayland, semantic; Servo 0.7 |
| `sighurt-engine-wpe` | `runtime`, `view`, `navigation`, `platform` (its `WPEDisplay` and `WPEView`), `present`, `input`, `ime`, `clipboard`, `glib_loop` (WebKit's GLib context in the owner loop), `ffi` | api, wayland; WPE WebKit 2.52 |
| `sighurt-engine-wasm` | `adapter` (`WasmRuntime`), `view`, `navigation`, `engine` (Wasmtime, limits), `live` and `pool` (guest threads), `capabilities` (host functions), `draw` and `raster` (CPU drawing), `wayland` (its `CpuTarget` for the host's SHM lease), and a module per guest subsystem | api, wayland |
| `sighurt-engine-demo` | `lib` (the runtime and its pages: static pattern, animation, slow frame, resize, failed navigation, crash), `paint` | api, wayland |
| `sighurt-engine-cef` | `capi` (generated C API layouts), `ffi` (`dlopen` of `libcef.so`), `handlers` (per-view client; callbacks only queue), `shared` (event queue, geometry, staged frame), `nav` (navigation events), `find` (find in page: operations as UI-thread tasks, results stamped with the search they answer), `actions`, `keys`, `edit`, `present`, `runtime` (`CefRuntime<T: CpuTarget>`) | api; CEF 150.0.22, `dlopen`, not linked |
| `sighurt-engine-ladybird` | `ffi` (`dlopen` of the shim), `runtime` (`LadybirdRuntime<T: CpuTarget>`), `waiter` (polls the shim's loop fd and wakes the owner) | api; Ladybird through the shim, `dlopen`, not linked |
| `sighurt-engine-netsurf` | `ffi` (the C frontend), `input`, `runtime` (`NetSurfRuntime`), `watcher` (descriptor watcher). The C core and frontend are `csrc/`, built by `build.rs` | api, wayland; NetSurf 3.11, linked statically. `GPL-2.0-only` |
| `sighurt-semantic` | `extract` (the DOM extraction script), `parse` (its bounded JSON parser), `action` (action scripts), `world` | api; shared by the Servo and WPE adapters |
| `sighurt-tui` | `app` (the owner loop and its `Frontend`), `term` (terminal I/O, parser, capability probe), `grid`, `chrome` (the terminal chrome), `input`, `kitty` and `sixel` (graphics encoders), `target` (the page's CPU target), `engines` (compiled-in engines bound to it), `text` and `textmode` (reflowing a semantic snapshot, and `textmode::find`, text mode's own search), `sanitize`, `dump` (`sig --dump`); binary `sighurt-tui-worker` | api, core, runtime; engines optional (never wayland or ui) |
| `sighurt-runtime` | `session` (the core, effects, pumps, presenting, shutdown), `frontend` (the `Frontend` trait, with the find hook and the line and find fields), `registry`, `engines` (descriptors), `view`, `services`, `persistence`, `history` (visited pages, their file, completing from them), `diagnostics` | api, core; engines optional (never wayland or ui types in its own code) |
| `sighurt-gui` | `app` (the owner loop), `chrome`, `render` (the renderers, `RENDERERS`, and `[ui] renderer`), `engines` (engines bound to Wayland targets) | api, core, runtime, wayland, ui, render; engines and renderers optional |
| `sighurt-browser` | `main` | runtime, gui; engines optional |
| `sighurt-sdk` | The guest-side Wasm API | None: independent of every native crate |

- `sighurt-core` compiles and tests without GPUI, Wayland, Servo, Wasmtime or a graphics driver.
- `sighurt-api` holds no `gpui::Entity`, `wl_surface`, `Rc<Servo>`, graphics-API type or `Any` escape hatch. It is an internal contract rebuilt with the workspace, not a plugin ABI.
- Only the executable chooses which engines, keymaps, chromes and renderers are installed.
- `sighurt-render` never depends on sighurt-wayland (a native backend gets raw-window-handle handles) and is the only crate naming tiny-skia or cosmic-text; `sighurt-runtime` and `sighurt-tui` never depend on it. The GPU backends are their own crates behind features on `sighurt-browser` and `sighurt-gui`: `render/sighurt-render-wgpu` (`render-wgpu`, in `sig`'s default set) and `render/sighurt-render-vulkan` (`render-vulkan`, opt-in, in no default set). They never depend on sighurt-wayland: they bind the lease's raw-window-handle handles (`TargetLease::gl_handles`).
- Engines talk outward through typed events and service requests, never through a reference to the browser.
- Rendering choices never change tab or navigation state types.
- Network and filesystem effects live in the executable; the core may parse and validate.
- Shared third-party versions are pinned in the root `[workspace.dependencies]`; depend on them with `<crate>.workspace = true`.
- Every module starts with a doc comment saying what it owns and what it must not. Keep it true.

## Build commands

```bash
# Crates that need neither Servo, WPE WebKit nor Wasmtime. CEF and Ladybird compile without
# their prefixes (they are dlopen'd at run time). NetSurf's build.rs compiles its C core and
# needs the nix-shell's SIGHURT_NETSURF_* variables, so it is not in this list.
tools/cargo check -p sighurt-api -p sighurt-core -p sighurt-wayland -p sighurt-ui -p sighurt-render -p sighurt-render-wgpu -p sighurt-render-vulkan -p sighurt-engine-demo -p sighurt-chrome-line -p sighurt-config-example -p sighurt-engine-cef -p sighurt-engine-ladybird
tools/cargo check -p sighurt-browser --no-default-features
tools/cargo test -p sighurt-api -p sighurt-core
tools/cargo clippy -p <crate> --all-targets -- -D warnings
tools/cargo fmt -p <crate>

# sig with only the demo engine, for host and chrome work; run it in a private compositor
# (tools/heavy outside: headless-sway replaces XDG_RUNTIME_DIR, which tools/heavy needs)
tools/cargo build -p sighurt-browser --no-default-features --features engine-demo
tools/heavy tools/headless-sway -- target/debug/sig
# sig's unit tests, and its end-to-end tests in a private headless sway (builds sig with the
# demo engine, render-wgpu and render-vulkan; prints PASS or FAIL per check, screenshots and
# logs in OUT; its `renderers` scenario runs cpu, wgpu, vulkan and their fallbacks in a gles2
# sway);
# the chromes' end-to-end tests (the line chrome, the standard chrome's options)
tools/cargo test -p sighurt-browser -p sighurt-engine-demo --no-default-features --features engine-demo
apps/sighurt-browser/tests/headless.sh OUT
ui/sighurt-ui/tests/headless.sh OUT
# Reliability: a soak of the demo and Wasm engines (default 10 minutes; RSS, fds, threads and
# counters held to bounds) and fault injection (directories, locks, failing writes, sessions,
# configurations, the compositor killed, signals, crashes, floods), each in private sways;
# the terminal's faults (resize and input floods, a silent or garbage probe answer, the
# terminal closed) are in apps/sighurt-browser/tests/tui.rs
apps/sighurt-browser/tests/soak.sh OUT [MINUTES]
apps/sighurt-browser/tests/faults.sh OUT [SCENARIO ...]

# The chrome's renderers: sighurt-render with its GPU preparation (naga validates the WGSL; a
# reference rasterizer compares prepared frames with the CPU backend), and the wgpu backend
# against the CPU one on the standard chrome's fixtures (needs a Vulkan adapter: tools/heavy;
# says "skipped" without one)
tools/cargo test -p sighurt-render --all-features
tools/heavy cargo test -p sighurt-render-wgpu -p sighurt-render-vulkan
tools/cargo build -p sighurt-browser --no-default-features --features engine-demo,render-wgpu,render-vulkan
apps/sighurt-browser/tests/headless.sh OUT renderers

# The terminal frontend: its unit tests, and sig --tui / sig --dump in a pty with a fake terminal
# on the master side against the demo engine (tests/tui.rs; needs frontend-tui and engine-demo)
tools/cargo test -p sighurt-tui --features engine-demo,engine-wasm
tools/cargo test -p sighurt-browser --no-default-features --features frontend-tui,engine-demo
# sig --tui in real terminals (Rio, kitty) in a private headless sway: builds sig with default
# features (Servo, WPE) unless --no-build, and takes the heavy lock itself; PASS or FAIL per check, screenshots and logs in OUT
frontends/sighurt-tui/tests/real-terminals.sh [--no-build] OUT [rio|kitty]

# Anything that builds Servo or WPE WebKit (sig's default features, sighurt-engine-servo,
# sighurt-engine-wpe, the experiments). WPE WebKit needs its prefix (nix/build-wpewebkit.sh,
# docs/rewrite/WPE.md); without it, build sig with --no-default-features --features
# engine-servo,engine-wasm
SIG_WHAT="servo build" tools/cargo build -p sighurt-browser
SIG_WHAT="servo build" tools/cargo clippy -p sighurt-engine-servo -p sighurt-engine-wpe -p sighurt-browser --all-targets -- -D warnings
# The same on louise-pc
SIG_WHAT="servo build" tools/remote tools/cargo build -p sighurt-browser
# sig with Servo, WPE WebKit and Wasm on the GPU, in a private compositor on a D-Bus session of
# its own (with SIG_DIAG=1 it prints each engine's renderer and a line per frame), and its
# end-to-end tests (Servo, Wasm, WPE WebKit, links across engines, settings, a configuration
# showcase with the example keymap and the line chrome)
tools/heavy tools/headless-sway --renderer gles2 -- engines/sighurt-engine-wpe/tests/private-bus target/debug/sig https://example.com/
apps/sighurt-browser/tests/engines.sh OUT

# Seeded random-input tests ("fuzz-lite", tools/fuzz/mod.rs) of every parser and decoder that
# takes untrusted or user-written input, in ordinary `cargo test`: commands and keystrokes, the
# configuration, addresses and routing, sessions and the files that hold them, the semantic
# snapshot, the terminal's input, the chrome's contained text and readable URLs, the sixel and
# kitty encoders and the grid, download names, the external-link denylist, the Wasm network
# policy. N = 2000 cases by default; a failure prints the seed and case to replay.
tools/cargo test -p sighurt-api -p sighurt-core -p sighurt-runtime -p sighurt-semantic -p sighurt-ui -p sighurt-tui -p sighurt-engine-wasm -- fuzz
SIG_FUZZ_ITERATIONS=100000 tools/cargo test -p sighurt-core -- fuzz     # more cases
SIG_FUZZ_SEED=<seed> SIG_FUZZ_CASE=<case> tools/cargo test -p <crate> -- <test>   # replay one

# Wasm guests (always release); the Wasm engine's tests run the first four
tools/cargo build --target wasm32-unknown-unknown --release -p hello-sighurt -p raf-demo -p gradient-demo -p typography-demo
tools/cargo build -p sighurt-sdk --target wasm32-unknown-unknown

# Compositor tests of the host and the engines (each script says how to run it)
frontends/sighurt-wayland/tests/headless.sh
engines/sighurt-engine-wasm/tests/headless.sh [OUT]
SIG_WHAT="servo test" engines/sighurt-engine-servo/tests/headless.sh
engines/sighurt-engine-wpe/tests/headless.sh

# CEF (Chromium through CEF 150.0.22). The crate compiles without the prefix. Running it needs
# nix/build-cef.sh (prefix /data/sig-builds/deps/cef-150.0.22, SIGHURT_CEF_PREFIX, nix/cef.nix),
# the helper sig-cef-helper, and a display (Wayland or X11), including in the terminal.
# offscreen.sh starts Chromium in a private sway; cef.sh is sig with the cef and demo engines.
nix/build-cef.sh
tools/cargo build -p sighurt-browser --no-default-features --features engine-cef,engine-demo
tools/cargo test -p sighurt-engine-cef --lib
engines/sighurt-engine-cef/tests/offscreen.sh
apps/sighurt-browser/tests/cef.sh [OUT]

# Ladybird. The crate compiles without the shim. Running it needs the patched Ladybird
# (nix/ladybird.nix plus nix/ladybird/*.patch, built on louise-pc) and the shim, which the
# engine dlopens. One copy per frame; Ladybird's own helper processes. sig-shot.sh takes OUT
# and an optional URL; MODE=route|open-in|typing|links selects [routing.ladybird] or page.open-in.
tools/remote tools/heavy nix/build-ladybird-patched.sh
nix/pull-ladybird-patched.sh
tools/heavy nix/build-ladybird-shim.sh
tools/cargo build -p sighurt-browser --no-default-features --features engine-ladybird,engine-demo
tools/heavy tools/headless-sway -- engines/sighurt-engine-ladybird/tests/sig-shot.sh OUT [URL]

# NetSurf 3.11. GPL-2.0-only: engine-netsurf is never a default feature, and a sig built with
# it is for personal use and must not be distributed. build.rs needs the nix-shell's
# SIGHURT_NETSURF_* variables (tools/cargo enters that shell) and compiles the C core. CI
# excludes the crate. headless.sh builds sig with the netsurf and demo engines and runs in a
# private sway (its build line still names keymap-modal; that feature is gone, decision 116).
tools/cargo build -p sighurt-browser --no-default-features --features engine-netsurf,engine-demo,frontend-tui
engines/sighurt-engine-netsurf/tests/headless.sh OUT

# Every engine of an --all-features sig in one process (Servo, so the heavy lock and a free
# desktop; the WPE, CEF and Ladybird prefixes, and NetSurf's sources). OUT must be under
# /data/sig-builds/test-out/.
apps/sighurt-browser/tests/coexist.sh [--build] OUT

# Before a commit: everything but Servo, WPE WebKit and NetSurf (what CI runs), then the rest.
# CEF and Ladybird compile without their prefixes and stay in the workspace commands. NetSurf's
# build.rs does not. `--all-features` below also builds NetSurf and compiles the CEF and
# Ladybird crates; running those engines still needs their prefixes, and a sig with NetSurf
# must not be distributed.
tools/cargo fmt --all
tools/cargo clippy --workspace --exclude sighurt-engine-servo --exclude sighurt-engine-wpe --exclude sighurt-engine-netsurf --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-coexist --exclude tui-servo --exclude wpe-cpu --exclude wgpu-subsurface --all-targets -- -D warnings
tools/cargo clippy -p sighurt-browser -p sighurt-runtime -p sighurt-gui --no-default-features --features engine-demo,engine-wasm,chrome-line,render-wgpu --all-targets -- -D warnings
tools/cargo test --workspace --exclude sighurt-engine-servo --exclude sighurt-engine-wpe --exclude sighurt-engine-netsurf --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-coexist --exclude tui-servo --exclude wpe-cpu --exclude wgpu-subsurface
tools/cargo test -p sighurt-browser -p sighurt-runtime -p sighurt-gui --no-default-features --features engine-demo,engine-wasm,chrome-line,render-wgpu
SIG_WHAT="servo build" tools/cargo clippy -p sighurt-engine-servo -p sighurt-engine-wpe -p sighurt-browser -p m0-coexist -p m1-mixed -p wpe-coexist -p tui-servo -p wpe-cpu --all-targets -- -D warnings
SIG_WHAT="servo build" tools/cargo clippy -p sighurt-browser -p sighurt-runtime -p sighurt-gui --all-features --all-targets -- -D warnings
SIG_WHAT="servo build" tools/cargo test -p sighurt-engine-servo -p sighurt-engine-wpe --lib
SIG_WHAT="servo build" tools/cargo test -p sighurt-browser -p sighurt-runtime -p sighurt-gui --all-features
```

`--workspace` alone, `sighurt-browser` with its default features, `m0-coexist`, `m1-mixed`, `wpe-coexist`, `tui-servo` and `wpe-cpu` compile Servo; `sighurt-engine-wpe`, `wpe-coexist`, `wpe-cpu` and `sig`'s default features need the WPE WebKit prefix. `sighurt-engine-netsurf` compiles NetSurf's C core and needs the nix-shell (`SIGHURT_NETSURF_SRC` and the rest); CI excludes it, and so do the workspace commands above. `sighurt-engine-cef` and `sighurt-engine-ladybird` compile without their prefixes (a missing prefix is a `cargo::warning`, and the runtime refuses to start). Dependencies are built at `opt-level = 3` without debug info even in dev builds (Servo is unusable unoptimized); the Servo artifacts in `target/` were built with the same dependency settings, so keep them.

## Commands, keys and configuration

- Everything the browser does is a named command, `<area>.<action>` with kebab-case actions and an optional argument after whitespace: `tab.new [url]`, `tab.close [n]`, `tab.duplicate`, `tab.move left|right|<n>`, `tab.reopen`, `tab.next`, `tab.previous`, `tab.select <n>`, `tab.last`, `page.open <url>`, `page.open-in <engine> [url]`, `page.switch-engine`, `page.reload`, `page.stop`, `page.back`, `page.forward`, `page.zoom-in`, `page.zoom-out`, `page.zoom-reset`, `page.send-key <key>`, `page.find [text]`, `page.find-next`, `page.find-previous`, `page.find-stop`, `page.exit-fullscreen`, `clipboard.copy <text>`, `location.focus [text]`, `site.forget [host]`, `history.clear`, `browser.home`, `browser.fullscreen`, `browser.quit`, and a chrome's own `chrome.<action>` (no argument; the core hands it to the chrome). Users write these names in their configuration, so keep them.
- A command string is parsed once into `sighurt_api::commands::Command`; unknown names and bad arguments are errors when the configuration loads. `Display` gives the canonical string back. The core carries commands out, except `location.focus`, which the chrome handles. Keys and buttons use the same path; don't add a second one.
- To add a command: a variant in `Command` (and `NAMES`, `name`, parsing, `Display`), its handling in `sighurt-core`, the command list in `crates/sighurt-core/src/default-config.toml`, and the README's command table.
- **Keys live only in the configuration**: the example configuration crate (the default, in Rust) or the `bindings` keymap's `[keys]` tables. The core, the frontends and the chromes bind no keys; a frontend's chrome handles only its own editing keys (Enter, Escape, arrows, Ctrl+A/C/X/V in the address line; Enter, Shift+Enter and Escape in the find field, which is the chrome's own editor mode opened by `page.find` with no argument; Right or Tab accepting an inline completion from the visited pages). A keymap reads the browser through the read-only `sighurt_api::keymap::Browser` view and may return several commands (`KeyAction::RunAll`); it changes nothing itself. Bindings match the layout's key value by default and the physical key when a binding asks for it; AltGr is never Ctrl+Alt. A consumed key press also consumes its release.
- Renderers are compiled in too and chosen by `[ui] renderer` at startup (`auto` by default); they are listed in `sighurt-gui`'s `render` (`RENDERERS`). The core accepts every project renderer name (`sighurt_core::config::RENDERERS`, in `Environment::renderers` for both frontends and `--dump`); a window built without the one named says so and uses `auto`.
- Keymaps and chromes are compiled in and chosen like engines (`docs/rewrite/extension-api.md`): a keymap implements `sighurt_api::keymap::Keymap` and is chosen by `[input] keymap` (`example`, the example configuration, by default; the core's `bindings` reads `[keys]`); a chrome implements `sighurt_ui::extension::Chrome` and is chosen by `[ui] chrome` at startup (`standard` by default). Keymaps are listed in `sighurt-runtime`'s `registry` (`KEYMAPS`), chromes in `sighurt-gui`'s `chrome` (`CHROMES`); the defaults go through the same traits.
- Configuration is TOML: `{config_dir}/sighurt/config.toml` over built-in defaults (`crates/sighurt-core/src/default-config.toml`, which documents every setting), with a schema version, still 1. The core parses and validates it against the build (its engines, keymaps and chromes); the executable reads the file. Unknown keys are errors; a table for something the build lacks gets a notice. An invalid reload keeps the previous configuration and reports the problem. Everything applies live (`[history] remember = false` drops the visits in memory at once) except `[ui] chrome`, `[ui] renderer` and the engines' own settings (`[engines.<name>]`), which need a restart.

## The engine contract

- Engines implement `sighurt_api::engine::EngineRuntime`; read its documentation, including "Event delivery". `Ok` from `open_view` or `command` means accepted, not finished: results arrive as `EngineEvent`s from `pump`, which returns `PumpStatus` so the loop knows when it may sleep. Unsupported operations return `EngineError::Unsupported` and are declared in `EngineCapabilities`; nothing pretends to succeed.
- Events arrive only through `pump`, so the owner pumps every runtime it called in that loop iteration. Events for one view keep their order. A pump does a bounded slice of work and returns `Pending` when more remains; replaceable events (title, status, cursor, loading, history, text input, frame ready) keep only the latest per view. A view accepts commands as soon as `open_view` returns `Ok`; `ViewReady` means it can present.
- `ViewFailed` means the document is gone but the view can be reloaded; a view that can't continue is `ViewFailed` then `ViewClosed`. `RuntimeFailed` is the whole engine. `EngineRuntime::shutdown` closes everything in an explicit order before the host is released; never rely on `Drop` order for native lifetimes.
- The trait is not `Send` or `Sync`. Never add an unsafe `Send` impl to fit an engine into an executor. Work on other threads wakes the owner loop through `EngineWaker`.
- Every view event carries its `ViewId`, navigation events a `NavigationId`, document-bound requests a `DocumentGeneration`. Each id kind has one process-wide counter (`ViewId::next()`), so ids are never reused and a late event from a closed view is recognisably stale. An adapter allocates the `NavigationId` of a navigation its page started itself; a commit supersedes the view's earlier pending navigations.
- Find in page: every find command on a view is the core's `Effect::Find`; the runtime offers it to the frontend first (`Frontend::find`: the terminal's text mode searches its own text), then sends `EngineCommand::Find` to an engine that declares `find_in_page`, and otherwise answers `BrowserEvent::FindResult { supported: false }`. Each `FindCommand` names its search, and `Search` its document; an adapter stamps `EngineEvent::FindResult` (replaceable per view) with the search it answers, and the core drops a result whose search or document is not the view's. WPE WebKit reports a total only, NetSurf found only, Servo 0.7 has no find API.
- A page asks to open a URL with `OpenRequested` (in its own tab or a new one); the core routes it like any URL, possibly to another engine. An engine performs only navigations it owns and hands the rest over this way.
- Before performing a navigation its page started that it would load itself, an engine asks with `NavigationRequested` (the asking document and a `RequestId`) and holds it until `AnswerNavigation`: the core hands it to another engine when routing names one and the user clicked a link to that very URL in the page within 5 s (the page's status text at the press), never while the tab loads an address the user asked for. At most 16 held per view; what is still held when the document is replaced, the view closes or the runtime shuts down is denied, and a late answer does nothing. An engine may ask only about what the user started (WPE: a link or GET form the user clicked); the demo and Wasm engines never ask.
- Keys: `KeyCode` is the evdev code (`w3c_code()` gives "KeyA"), `key` has W3C `KeyboardEvent.key` semantics (the composed "ä" on the press that completes a dead key), `text` is what the press types; editors insert `text`. `SetFocus(false)` makes the engine release every key its content holds as pressed. Pointer events carry the seat's modifiers.
- An empty `Metrics` rectangle means "no room", not "hidden": visibility is `SetVisibility`, and hidden views keep the page area's metrics. `open_view` with empty metrics still succeeds.
- Delegate callbacks only queue events; they are handed over after the engine call returns. Nothing mutates browser state re-entrantly.
- Positions arrive surface-local to the view's target. Convert them once with the view's `Metrics` (`to_buffer`); never subtract the chrome's offset or apply the scale a second time. Buffer sizes come from `Scale::buffer_size`.
- `FrameReady` means "has something new to paint", not "displayed". Keep invalidation, frame callbacks and buffer release distinct. No unconditional ticks. Servo's normal path does no page-sized CPU copy. CEF, Ladybird and NetSurf paint on the CPU; that copy is their frame and it is counted.
- Adding an engine: a crate `sighurt-engine-<name>` depending on `sighurt-api`, and on `sighurt-wayland` only when it names the host's target types. CEF and Ladybird present through `CpuTarget` and do not depend on wayland; the frontends bind them to a shared-memory lease or the terminal's CPU target. The crate exports `CAPABILITIES`. A feature `engine-<name>` goes on `sighurt-browser`, `sighurt-runtime`, `sighurt-gui` and `sighurt-tui` (in no default set unless the user has decided otherwise). Its descriptor is in `sighurt-runtime`'s `engines` (name, `web` so `page.switch-engine` cycles it, capabilities, the `[engines.<name>]` settings it honors). Each frontend starts it: `sighurt-gui`'s `engines` (`PresentMode::Shm` for a CPU engine) and `sighurt-tui`'s `engines`. The demo engine is the smallest complete one. An engine whose build needs a prefix CI does not have is excluded from CI's workspace commands: WPE WebKit's build script stops without its prefix, and NetSurf's compiles the C core from the nix-shell's sources. CEF and Ladybird only `dlopen` at run time, so a missing prefix is a warning and the crates stay in those commands.
- **Servo** is pinned at 0.7 with surfman 0.14 (`webgl` enabled; multiprocess compiled but off). Two of its crates are vendored with small patches in `third_party/` through `[patch.crates-io]`, each with a `SIGHURT-PATCH.md`: surfman (no environment writes, decision 102) and servo-net (the crash-safe, private profile writer, decision 113); changing either rebuilds Servo. Read the 0.7 sources in `~/.cargo/registry/src/`, not a newer upstream. One runtime per process; its API calls and GL presentation stay on the owner thread. The adapter never calls `process::exit` or `_exit` and never installs panic hooks, logging or a TLS provider: the executable owns process-wide setup. The crate is `AGPL-3.0-or-later AND MPL-2.0`; files adapted from Servo keep their MPL notices. What 0.7 changed for the adapter is in `engines/sighurt-engine-servo/SERVO-0.7.md`.
- **WPE WebKit** is 2.52.6, built from source into a prefix (`nix/build-wpewebkit.sh`; the build script finds it through `SIGHURT_WPE_PKG_CONFIG_PATH`, never `PKG_CONFIG_PATH`, which would rebuild Servo). One runtime per process, on the owner thread; its GLib context runs inside the owner loop. JavaScriptCore stays off signals in `sig`'s process (decision 66), and `sig` sets `JSC_SIGNAL_FOR_GC=12` at the start of `main`, since its default is SIGUSR1, `sig`'s diagnostics signal (decision 78). `sig` is linked with `-Wl,--exclude-libs,ALL` so WebKit can't bind to the C libraries linked into it (decision 65). WebKit's library is not linked: the engine opens it (`ffi::load`) when its runtime first starts, so a session without WPE pages maps none of it (decision 84); call `ffi` only after that.
- **CEF** is 150.0.22 (`nix/cef.nix`, prefix `/data/sig-builds/deps/cef-150.0.22`). The crate is `AGPL-3.0-or-later`; the CEF and Chromium BSD notices live in `engines/sighurt-engine-cef/README.md`. `libcef.so` is `dlopen`ed, never linked, so the crate compiles with no prefix. The feature `engine-cef` is in no default set. Chromium needs a display. No `semantics`.
- **Ladybird** is nixpkgs' package plus `nix/ladybird/0001-navigation-hold-and-download-view.patch` (LibWebView: hold a user-activated navigation, and name the view a download belongs to), approved by the user on 31 October 2026 (decision 118). The shim is `dlopen`ed; the crate compiles without it. `engine-ladybird` is in no default set. Ladybird is BSD-2-Clause; the shim and the crate are AGPL. No `semantics`.
- **NetSurf** (`sighurt-engine-netsurf`, `docs/rewrite/NETSURF.md`) is NetSurf 3.11's core with a frontend of ours in C, built by `build.rs` from the nixpkgs tarball into `/data/sig-builds/netsurf` (or `$SIGHURT_NETSURF_BUILD`), and linked statically. It is `GPL-2.0-only`, so `engine-netsurf` is in **no** default feature set and a `sig` built with it is for personal use and must not be distributed. One runtime per process, on the owner thread; it never ticks (its deadline and its fetchers' descriptors reach the owner loop through `pump` and a watcher thread). Its patches (no certificate exceptions, no page-planted credentials, failed loads fail, `file:` only from `file:` pages) are in `build.rs` and are security rules: keep them. Process-wide effects that are NetSurf's own and not moved into `main`: `SIGPIPE` ignored (already the case in Rust), `curl_global_init`, and `setlocale` patched out. GPL-2.0-only and AGPL-3.0-or-later cannot both cover one program that is conveyed; personal use of a binary given to no one is the user's decision (119). CI excludes the crate: its build script stops without the nix-shell's `SIGHURT_NETSURF_*` variables. No `semantics`.

## Security

- **In-process native engines are cooperative, not a sandbox.** The contracts keep correct code correct; they do not contain an exploited Servo or WebKit UI process, and a native crash takes the browser down. Never describe the in-process design as sandboxed (WebKit's own web processes run in bubblewrap; that is WebKit's sandbox for its pages, not Sighurt's). OS isolation would need a restricted worker process, which is a separate decision. Keymaps and chromes are trusted code too.
- **Untrusted Wasm guests get only the host functions registered in the linker.** Host access is additive and opt-in, with no wildcards. Grants are enforced at the host-function boundary, so a guest can't grant itself anything.
- **Never link WASI** in the Wasm engine: no filesystem, no environment variables, no raw sockets.
- **Validate every guest memory access** with the helpers below; a malicious guest can pass any ptr/len. Bound what a guest can make the host allocate (draw lists, images, media, network responses, caches), not just its linear memory.
- Privileged services (clipboard, downloads, file pickers, external URLs, permissions) go through `sighurt_api::services`: the core decides, per view and `DocumentGeneration`; the executable performs; unknown or unimplemented requests fail closed.
- Page titles, status text and URLs are untrusted: display them contained. A page cannot write the address bar, run a browser command through a magic URL, open `sig://` pages, or open a file chooser without the policy allowing it.
- The core bounds what pages send: text (titles, status) is cut to 16 KiB, and URLs over 2 MiB are dropped, never shortened. A page opens a new tab only within 5 s of a click or key press in its view, and moves its tab to another engine only within 5 s of a click on that link, one per activation. Page-initiated downloads are throttled to one a second per tab. `sig` contains every string of a chrome snapshot to its bound before a chrome sees it.
- Visited pages (`[history] remember`, on by default) are recorded only for http and https addresses without userinfo, fragment stripped, at most 8 KiB, committed as a new document in the shown tab; never for an engine launched with ephemeral storage, never for `--dump`. The file (`history.toml` in the state directory) is mode 0600, at most 5 000 visits, 4 MiB and 90 days; `history.clear` and `site.forget` rewrite it at once and say when they could not; a second `sig` never writes it.
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

**1. Significant state gets its own module** in `engines/sighurt-engine-wasm/src/` (e.g. `websocket.rs`) with a `<Feature>State` struct and `register_<feature>_functions(linker: &mut Linker<HostState>) -> Result<()>`, called from the bottom of `register_host_functions`. Add `pub mod <feature>;` to `lib.rs`.

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

**4. Expose it in the SDK** (`sdk/sighurt-sdk/src/lib.rs`). A wrapper that takes and returns exactly what the host does is declared with `host_fns!`:
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
- `sighurt-api`, `-core`, `-wayland`, `-ui`, `-render`, `-engine-demo`: `tools/cargo check`, `test` and `clippy -- -D warnings` on the crate (`sighurt-render` with and without `--features gpu`).
- `sighurt-render-wgpu`: `tools/cargo clippy -p sighurt-render-wgpu --all-targets -- -D warnings`, `tools/heavy cargo test -p sighurt-render-wgpu` (the comparison with the CPU backend needs a GPU), and `apps/sighurt-browser/tests/headless.sh OUT renderers` (a gles2 sway). wgpu stays Vulkan-only (`default-features = false, features = ["vulkan", "wgsl"]`): never its GL backend in `sig`'s process.
- `sighurt-render-vulkan`: the same, with `-p sighurt-render-vulkan` (its tests also hold the SPIR-V's vertex inputs against `VERTEX_ATTRIBUTES`); `SIG_VULKAN_VALIDATION=1` turns the Khronos validation layer on (the `renderers` scenario does, and fails on a validation error). A change to the CPU backend or a chrome's drawing must keep the golden images (`ui/sighurt-ui/tests/golden`, `extensions/sighurt-chrome-line/tests/golden`) pixel for pixel, or say why.
- `sighurt-browser`, `sighurt-runtime`, `sighurt-gui`: `tools/cargo check -p sighurt-browser -p sighurt-runtime -p sighurt-gui --no-default-features`, plus `--features engine-demo`, plus the feature sets your change touches (`--all-features` covers them all). `sighurt-runtime`'s own code never names a `sighurt_wayland` or `sighurt_ui` type.
- `sighurt-engine-servo`, `sighurt-engine-wpe`, or anything built with them: `SIG_WHAT="servo build" tools/cargo check -p <crate>` (and `-p sighurt-browser` with its default features), on louise-pc when the desktop is busy.
- `sighurt-engine-cef`, `sighurt-engine-ladybird`: `tools/cargo check -p <crate>` compiles with no prefix. Running them needs the prefix or the shim, and for CEF a display, under `tools/headless-sway`.
- `sighurt-engine-netsurf`: `tools/cargo check -p sighurt-engine-netsurf` (the nix-shell provides the sources). The crate is `GPL-2.0-only`; do not distribute a `sig` built with it.
- Keymap or chrome change: its crate's tests (they use the API from outside), and `apps/sighurt-browser/tests/headless.sh` or `ui/sighurt-ui/tests/headless.sh`.
- WASM engine change: `tools/cargo build -p sighurt-engine-wasm`.
- SDK change: `tools/cargo build -p sighurt-sdk --target wasm32-unknown-unknown`.
- Example change: `tools/cargo build --target wasm32-unknown-unknown --release -p <example>`.
- Wayland behavior: run it under `tools/headless-sway`, never on the user's desktop.
- Host functions: confirm registration in `register_host_functions` (or a `register_*_functions` it calls) and an SDK wrapper with a doc comment.
- Commands: confirm there is no hardcoded key handler.

---

## Common pitfalls

- **`Cargo.lock` was reseeded from Servo 0.7.0's published lockfile** so Servo keeps the dependency versions that release was tested with. Don't run a broad `cargo update`: changing any of Servo's dependencies means rebuilding all of Servo.
- **Servo 0.7, surfman 0.14.** Check every API against the pinned 0.7 sources, not a newer upstream.
- **Scale is a rational.** Keep `Scale` (numerator over 120) and derive buffer sizes and input transforms from the actual rounded dimensions, not from a float factor.
- **Empty is not hidden.** A zero-sized rectangle means the layout has no room: the host unmaps the surface and the engine keeps its last size, allocates nothing and presents nothing (Servo can't make a 0×0 context). Hiding a view (a background tab) is `SetVisibility(false)` with its metrics unchanged. Never wait for a frame callback on an unmapped or hidden surface.
- **`HostState.memory` is set before instantiation**, so `caller.data().memory` is always there inside a host function. The helpers `expect` it.
- **`block_on` vs spawn.** RTC uses `runtime.block_on` for synchronous host calls; WebSocket uses `runtime.spawn` for background tasks. Match the subsystem you're in.
- **`Arc<Mutex<Option<T>>>` lazy init.** Subsystems such as rtc, ws, midi, fetch, sse and workers are `None` until first use; go through `with_started` / `with`.
- **Guest limits.** Each callback (`on_frame`, `on_timer`, `on_event`) gets 50M instructions (`FRAME_FUEL_LIMIT`) and `start_app` 500M (`FUEL_LIMIT`), and each has a wall-clock limit (1 s, `start_app` 10 s) that time spent waiting in host functions doesn't count against (`SandboxPolicy` in `engine.rs`). Host functions don't count against fuel and can't call back into the guest.
- **Guests run on worker threads, in `sig`'s process.** Every view of the Wasm engine shares its runtime, and several `sig` processes may run at once, so state shared between pages (the kv store) works without locks; see `kv.rs`.
