# Sighurt — CLAUDE.md

Sighurt (binary `sig`) is a minimal, engine-agnostic browser. It renders what engines tell it to plus what the UI code says, nothing else. There are exactly two draw sources:

- **Sandboxed engines**: separate processes, one per page, speaking `sighurt-ipc` on stdin/stdout. They send finished RGBA frames or `DrawOp` lists plus images, which `sighurt-core` paints.
- **Trusted UI code**: the GPUI chrome in `sighurt-ui`.

The browser never links an engine. The WASM engine (`sig-wasm`) runs `.wasm` apps that speak Oxide's guest ABI (the `"oxide"` import module); guests are built with `sighurt-sdk` for `wasm32-unknown-unknown`, and host and guest share no Rust types: the boundary is FFI through linear memory.

Sighurt is a fork of Oxide by Nikhil Ranjan (github.com/niklabh/oxide, Apache-2.0). Keep the attribution intact (LICENSE, NOTICE, `authors` fields, `sig --version`).

---

## Build commands

Run everything inside `nix-shell` on NixOS (`shell.nix`); elsewhere install the native libraries listed in the README.

```bash
cargo build                                  # sig and sig-wasm into target/debug
cargo run -p sighurt-browser -- --help
cargo run -p sighurt-browser -- target/wasm32-unknown-unknown/release/hello_sighurt.wasm

# Guest apps (always release for WASM)
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt
cargo build -p sighurt-sdk --target wasm32-unknown-unknown

# Full check suite (must pass before any commit)
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Servo: its own workspace and lockfile, excluded from the root one
cargo build --release --manifest-path sighurt-engine-servo/Cargo.toml
cargo clippy --manifest-path sighurt-engine-servo/Cargo.toml -- -D warnings
cp sighurt-engine-servo/target/release/sig-servo target/debug/   # sig looks next to itself, then PATH
```

---

## Project structure

```
./
├── sighurt-browser/src/main.rs   # bin `sig`: CLI (--help, --version, --default-config) and wiring
├── sighurt-core/src/             # Everything but the chrome; no UI of its own
│   ├── config.rs                 # config.toml merged over default-config.toml
│   ├── default-config.toml       # Built-in defaults and the format's documentation. Binds no keys
│   ├── commands.rs               # COMMANDS, CONTEXTS, the RunCommand action, install_keys
│   ├── router.rs                 # Which engine opens a URL; URL bar input fixup
│   ├── browser.rs                # Session: tabs, history, routing, Browser::run_command
│   ├── page.rs                   # Engine host: spawns engine processes, paints frames and DrawOp lists
│   └── download.rs               # Saves what no engine shows to the download folder
├── sighurt-ui/src/               # Default chrome: tab strip, toolbar/URL bar, status bar
│   ├── lib.rs                    # Root view; moves focus for location.focus, else Browser::run_command
│   ├── toolbar.rs, pages.rs      # Tabs and URL bar; sig://home and the error display
│   └── colors.rs                 # Colors, overridable from [ui.colors]
├── sighurt-ipc/src/lib.rs        # Zero-dependency protocol v2: ToEngine, FromEngine, DrawOp
├── sighurt-engine-wasm/src/      # bin `sig-wasm`
│   ├── main.rs, page.rs, draw.rs, keys.rs  # The process: protocol loop, page, draw lists, key mapping
│   ├── capabilities.rs           # HostState + register_host_functions: the wasmtime Linker
│   ├── engine.rs, runtime.rs     # Sandbox policy, fuel/memory limits, AOT cache; load and run modules
│   └── fetch.rs, websocket.rs, sse.rs, rtc.rs, audio.rs, video.rs, events.rs, worker.rs, …
├── sighurt-engine-servo/         # bin `sig-servo`: headless Servo. Own workspace, Apache-2.0 AND MPL-2.0
├── sighurt-sdk/src/              # Guest SDK: lib.rs (FFI + wrappers), draw.rs, proto.rs
└── examples/                     # WASM guests (see the workspace members), plus fullstack-notes/backend
```

---

## Engines

- An engine is any executable that speaks `sighurt-ipc` on stdin/stdout, one process per page. It writes `Hello` first (within 10 s), then gets `Resize` and `Navigate`. stdout belongs to the protocol, so it logs to stderr.
- It is added through `[engines.<name>] command = [...]` in the config, with no browser changes. A bare program name is looked up next to the `sig` binary, then on `PATH`.
- Everything an engine sends is untrusted. `sighurt-ipc` caps message size (`MAX_MESSAGE_LEN`) and `page.rs` bounds text, URLs, images, draw lists and coordinates; keep it that way.
- Protocol changes go in `sighurt-ipc` (bump `VERSION` on incompatible changes) and must keep `sig-servo` building.

## Commands, keys and config

- Everything the browser does is a **named command**: `<area>.<action>`, lower-case, kebab-case actions (`tab.new`, `page.zoom-in`). Arguments follow a space (`tab.select 3`).
- Commands run in one place: `Browser::run_command` in `sighurt-core/src/browser.rs`, except `location.focus`, which the UI handles. Buttons and keys both go through the `RunCommand` action. Don't add a second path.
- To add a command: add it to `COMMANDS` in `commands.rs`, handle it in `run_command`, and list it in the comment in `default-config.toml` and in the README's command table.
- **Keys live only in the user's config**, never in code, and there are **no default bindings**. `[keys]` is global, `[keys.page]` and `[keys.location]` apply while the page or the URL bar has focus (`CONTEXTS`). GPUI keystroke syntax: `secondary` is cmd on macOS and ctrl elsewhere; sequences are space-separated.
- All settings live in TOML: `{config_dir}/sighurt/config.toml` over `default-config.toml` (`sig --default-config` prints it). There is no settings screen. Engines merge by name, field by field; `[keys]` and `[ui]` are the user's alone. Unknown keys are errors and a broken file is ignored with a warning. `[ui]` belongs to the UI crate (`sighurt-ui` reads `[ui.colors]`). Keep the default file's comments in sync with the code.
- Routing (`router.rs`): `sig://` is the UI's (only `sig://home` exists), then engine `schemes`, then `extensions`; unmatched `file:` URLs go to `default_engine`, unmatched http(s) URLs to the engine whose `mime` matches the `Content-Type` of a HEAD request, and a type no engine claims is downloaded. Engines are tried in name order.

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

---

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
| Engine crate / binary | `sighurt-engine-<name>` / `sig-<name>` | `sighurt-engine-wasm` / `sig-wasm` |
| Engine name in config | lower-case | `wasm`, `servo` |

---

## Security rules — never break these

- **Engines stay out of the browser process.** Page content never runs in `sig`, and no browser crate links an engine.
- **Treat engine output as untrusted.** Keep every bound in `sighurt-ipc` and `page.rs`. Pages may not open `sig://` URLs, and new tabs and downloads from pages stay throttled.
- **Never link WASI** in the WASM engine: no filesystem, no env vars, no raw sockets.
- **Host access is additive and opt-in.** If it isn't registered in the linker, the guest can't call it. No wildcards.
- **Validate all lengths** before touching guest memory (use the helpers above). A malicious guest can pass any ptr/len.

---

## Guest app contract (WASM engine)

1. Export `start_app()`, called once on load.
2. Optionally export `on_frame(dt_ms: u32)`, called about 60 times a second (fuel replenished each call).
3. Optionally export `on_timer(callback_id: u32)`, called when a `set_timeout`/`set_interval`/`request_animation_frame` fires.
4. Optionally export `on_event(callback_id: u32)`, called once per pending built-in or custom event each frame (before timers and `on_frame`).

   `on_timer` and `on_event` are delivered from the frame loop, so they only run if the module also exports `on_frame`.
5. Build as `[lib] crate-type = ["cdylib"]` for `wasm32-unknown-unknown`.
6. Import everything from the `"oxide"` module (via `sighurt-sdk`), never WASI.

The engine has no widget kit: guests draw their own UI with the canvas API and poll input (`text_input()` for typed text).

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
- Browser, core or UI change: `cargo build -p sighurt-browser`.
- WASM engine change: `cargo build -p sighurt-engine-wasm`.
- SDK change: `cargo build -p sighurt-sdk --target wasm32-unknown-unknown`.
- Example change: `cargo build --target wasm32-unknown-unknown --release -p <example>`.
- `sighurt-ipc` or Servo engine change: `cargo build --manifest-path sighurt-engine-servo/Cargo.toml`.
- Host functions: confirm registration in `register_host_functions` (or a `register_*_functions` it calls) and an SDK wrapper with a doc comment.
- Commands: confirm there is no hardcoded key handler.

---

## Common pitfalls

- **`HostState.memory` is set before instantiation**, so `caller.data().memory` is always there inside a host function. The helpers `expect` it.
- **`block_on` vs spawn.** RTC uses `runtime.block_on` for synchronous host calls; WebSocket uses `runtime.spawn` for background tasks. Match the subsystem you're in.
- **`Arc<Mutex<Option<T>>>` lazy init.** Subsystems such as rtc, ws, midi, fetch, sse and workers are `None` until first use; go through `with_started` / `with`.
- **Frame fuel.** Each `on_frame` gets 50M instructions (`FRAME_FUEL_LIMIT` in `runtime.rs`); a store starts with 500M (`FUEL_LIMIT` in `engine.rs`). Host functions don't count against fuel and can't call back into the guest.
- **Every page is its own process.** State shared between pages (the kv store) must work across processes without locks; see `kv.rs`.
- **Servo is single-threaded and one per process.** `sig-servo` runs everything on the thread that built `Servo`.
- **The Servo engine has its own `Cargo.lock`.** Root-workspace commands (`--workspace`, `cargo fmt --all`) don't touch it; use `--manifest-path sighurt-engine-servo/Cargo.toml`.
