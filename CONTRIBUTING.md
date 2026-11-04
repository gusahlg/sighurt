# Contributing to Sighurt

Sighurt is meant to stay small. Before adding a feature, check whether it belongs in an engine, in the configuration or nowhere. The [README](./README.md) describes the browser and its configuration, [DOCS.md](./DOCS.md) the WASM engine and SDK, [`sighurt-rewrite-plan.md`](./sighurt-rewrite-plan.md) the design, [`docs/rewrite/STATUS.md`](./docs/rewrite/STATUS.md) what has been decided, and [CLAUDE.md](./CLAUDE.md) the conventions in more detail.

## Setup

See [Build and run](./README.md#build-and-run) for the prerequisites (`nix-shell` on NixOS, and the WPE WebKit prefix for `sig`'s default features). The opt-in engines are documented there too: CEF needs `nix/build-cef.sh`, Ladybird needs the patched package and `nix/build-ladybird-shim.sh`, and NetSurf builds inside `nix-shell`.

```bash
cargo build -p sighurt-browser --no-default-features --features engine-demo   # quick: no Servo
cargo build -p sighurt-browser                                                # Servo, WPE WebKit, Wasm
cargo build -p sighurt-browser --no-default-features --features engine-cef,engine-demo
cargo build -p sighurt-browser --no-default-features --features engine-ladybird,engine-demo
cargo build -p sighurt-browser --no-default-features --features engine-netsurf,engine-demo
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt
target/debug/sig target/wasm32-unknown-unknown/release/hello_sighurt.wasm
```

Build only the crates you need (`-p <crate>`): `--workspace` and `sig`'s default features compile Servo, and the crates and experiments that link Servo or WPE WebKit (`sighurt-engine-servo`, `sighurt-engine-wpe`, `sighurt-browser` with its defaults, `m0-coexist`, `m1-mixed`, `wpe-coexist`, `tui-servo`, `wpe-cpu`) take long to build. `sighurt-engine-netsurf` compiles NetSurf's C core from the nix-shell's sources. It is GPL-2.0-only, so `engine-netsurf` is never a default feature and CI leaves the crate out. `sighurt-engine-cef` and `sighurt-engine-ladybird` compile without their prefixes. A `sig` built with `engine-netsurf` is for your own use and must not be distributed.

## Checks

CI runs the checks that need neither Servo, WPE WebKit nor NetSurf. CEF and Ladybird are in those checks: their crates compile without the prefixes they `dlopen` at run time. Run the same list before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude sighurt-engine-servo --exclude sighurt-engine-wpe \
  --exclude sighurt-engine-netsurf \
  --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-coexist \
  --exclude tui-servo --exclude wpe-cpu \
  --all-targets -- -D warnings
cargo clippy -p sighurt-browser -p sighurt-runtime -p sighurt-gui --no-default-features \
  --features engine-demo,engine-wasm,chrome-line --all-targets -- -D warnings
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt -p raf-demo -p gradient-demo -p typography-demo
cargo test --workspace --exclude sighurt-engine-servo --exclude sighurt-engine-wpe \
  --exclude sighurt-engine-netsurf \
  --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-coexist \
  --exclude tui-servo --exclude wpe-cpu
cargo test -p sighurt-browser -p sighurt-runtime -p sighurt-gui --no-default-features \
  --features engine-demo,engine-wasm,chrome-line
```

If you touched an engine or `sig`, also run what builds it. `--all-features` includes `engine-cef`, `engine-ladybird` and `engine-netsurf` as well as Servo and WPE WebKit, so it needs the WPE prefix, the nix-shell for NetSurf, and a machine that can compile Servo. CEF and Ladybird still only need their prefixes to run.

```bash
cargo clippy -p sighurt-engine-servo -p sighurt-engine-wpe -p sighurt-browser --all-targets -- -D warnings
cargo clippy -p sighurt-engine-cef -p sighurt-engine-ladybird --all-targets -- -D warnings
cargo clippy -p sighurt-engine-netsurf --all-targets -- -D warnings
cargo clippy -p sighurt-browser --all-features --all-targets -- -D warnings
cargo test -p sighurt-engine-servo -p sighurt-engine-wpe --lib
cargo test -p sighurt-browser --all-features
```

The tests that need a compositor run in a private headless sway (`tools/headless-sway`), never on your desktop, since they type and click. Each script says what it checks:

| Script | What it runs |
|---|---|
| `apps/sighurt-browser/tests/headless.sh OUT` | `sig` with the demo engine: tabs, typing, coordinates, scaling, buffers, idle, the configuration, the example keymap, settings |
| `apps/sighurt-browser/tests/soak.sh OUT [MINUTES]` | `sig` with the demo and Wasm engines for MINUTES (10): tab churn, history, zoom, sizes and scales, hiding, configuration reloads, the clipboard; RSS, descriptors, threads and counters sampled and held to bounds |
| `apps/sighurt-browser/tests/faults.sh OUT [SCENARIO ...]` | `sig` under faults: unusable directories, a held or stale session lock, failing writes, broken and huge sessions, invalid configurations, the compositor killed, signals, a crashing page, floods of tabs and resizes |
| `apps/sighurt-browser/tests/engines.sh OUT` | `sig` with Servo, WPE WebKit and Wasm on the GPU: pages, input, history, links between engines, settings, a configuration showcase |
| `ui/sighurt-ui/tests/headless.sh OUT` | the chromes in `sig`: the line chrome, the status overlay, tabs on the left, fonts |
| `frontends/sighurt-wayland/tests/headless.sh` | the Wayland host against sway |
| `sighurt-engine-{servo,wpe,wasm}/tests/headless.sh` | each engine's compositor tests |
| `engines/sighurt-engine-cef/tests/offscreen.sh` | Chromium into an in-memory target, in a private sway (the CEF prefix, and a display) |
| `apps/sighurt-browser/tests/cef.sh [OUT]` | `sig` with CEF and the demo engine: `page.open-in cef`, typing, clipboard, a download |
| `engines/sighurt-engine-ladybird/tests/sig-shot.sh OUT [URL]` | `sig` with Ladybird, in a private sway (the shim). `MODE=route`, `open-in`, `typing` or `links` |
| `engines/sighurt-engine-netsurf/tests/headless.sh OUT` | `sig` with NetSurf and the demo engine, including `sig --tui` |
| `apps/sighurt-browser/tests/coexist.sh [--build] OUT` | every engine in one `sig` (`--all-features`; OUT under `/data/sig-builds/test-out/`) |

They print `PASS` or `FAIL` for each check and leave screenshots to look at in `OUT`.

## Guidelines

- **Engines stay out of the browser's state.** Engines implement `sighurt_api::engine::EngineRuntime`, report what happens as events and never change browser state; the core decides (`update(state, event) -> effects`, no I/O), and `sig` carries the effects out. Read the trait's documentation before you change an engine.
- **The core has no platform.** `sighurt-core` builds and tests without Wayland, Servo, WebKit, Wasmtime or a graphics driver.
- **Keys live in the configuration.** The default keys are the example configuration's (Rust, in `extensions/sighurt-config-example`) and no key handlers are in code elsewhere: everything goes through commands and the keymap.
- **No copies, no ticks.** Servo shows a page without reading it back to the CPU, and WPE WebKit copies a frame only on its shared-memory fallback. CEF, Ladybird and NetSurf paint on the CPU; that copy is the frame and it is counted. The chrome repaints only what changed, and an idle browser sleeps. The end-to-end tests check this.
- **Treat what pages and guests say as untrusted.** Keep the core's bounds, validate every guest `(ptr, len)`, never link WASI.
- Small, focused commits and PRs. Test real behavior, not the implementation's own assumptions. Every public SDK function has a doc comment, and every module says what it owns and what it must not.
- Match the surrounding style; don't refactor unrelated code.

## Adding an engine

An engine is a crate `sighurt-engine-<name>` that implements `EngineRuntime`. It depends on `sighurt-api`. It depends on `sighurt-wayland` when it names the host's target types. CEF and Ladybird present through `CpuTarget` and do not: the GUI binds them to a shared-memory lease (`PresentMode::Shm`) and the terminal binds them to its CPU target. NetSurf paints on the CPU and does name the Wayland lease.

1. Open views on the targets the host gives you; present only with a frame permit; report `FrameReady` when there is something new to paint.
2. Report navigations, history, titles, loading, cursors and text input as `EngineEvent`s from `pump`, each with its view, navigation and document ids. Ask before performing a navigation the page started (`NavigationRequested`), so links follow the routing.
3. Send clipboard, download, file and permission requests as `ServiceRequested` and wait for the answer; never perform them yourself.
4. Declare what you support in `EngineCapabilities` and return `EngineError::Unsupported` for the rest. Set `semantics` only when the engine can give the page's text (text mode and `sig --dump`). Set `process_per_view` if each view is an OS process of its own: a tab then keeps fewer of your views alive in its history.
5. Shut down in an explicit order in `shutdown`; never exit the process or install process-wide handlers.

Then add a feature `engine-<name>` to `apps/sighurt-browser/Cargo.toml`, `crates/sighurt-runtime/Cargo.toml`, `frontends/sighurt-gui/Cargo.toml` and `frontends/sighurt-tui/Cargo.toml`. Leave it out of every `default` list unless it is meant to ship in `sig`. Add the engine's descriptor to `crates/sighurt-runtime/src/engines.rs` (its name, `web` when it opens http(s) so `page.switch-engine` cycles it, its capabilities and the `[engines.<name>]` settings it honors), how the GUI starts it to `frontends/sighurt-gui/src/engines.rs`, and how the terminal starts it to `frontends/sighurt-tui/src/engines.rs`. `sighurt-engine-demo` is the smallest complete engine.

A crate that does not compile on CI without a local prefix is excluded from the workspace commands above, as `sighurt-engine-wpe` and `sighurt-engine-netsurf` are. CEF and Ladybird compile without their prefixes, so they stay.

## Adding a keymap or a chrome

See [Writing your own keymap or chrome](./README.md#writing-your-own-keymap-or-chrome) in the README.

## Adding a command

1. Add a variant to `Command` in `crates/sighurt-api/src/commands.rs`, with its name in `NAMES`, `name`, its parsing and `Display`.
2. Carry it out in `sighurt-core`.
3. List it in `crates/sighurt-core/src/default-config.toml` and in the README's command table.

## Adding a host function

1. Register it with `linker.func_wrap("oxide", "api_<category>_<action>", …)` in `engines/sighurt-engine-wasm/src/capabilities.rs`, or in a module's `register_<feature>_functions`. Read guest memory only through `guest_bytes` / `guest_str` and write it through `write_guest` / `write_prefix`, and bound what a guest can make the host allocate.
2. Wrap it in `sdk/sighurt-sdk/src/lib.rs`: with `host_fns!` when the wrapper passes arguments and results through unchanged, otherwise an `_api_<name>` import plus a hand-written `<category>_<action>` wrapper. Give it a doc comment.
3. Mention it in [DOCS.md](./DOCS.md)'s API overview.

The import module name `"oxide"` is a permanent ABI: never rename it.

## Pull requests

Describe what changed, why and how you tested it. PRs are squash-merged. To report a security issue, follow [SECURITY.md](./SECURITY.md) instead of opening an issue.

## License

Sighurt is licensed under `AGPL-3.0-or-later` (see [LICENSE](./LICENSE) and [NOTICE](./NOTICE)); `sighurt-engine-servo` is `AGPL-3.0-or-later AND MPL-2.0`, and `sighurt-sdk` and the example apps are `Apache-2.0` (see [LICENSE-APACHE](./LICENSE-APACHE)). `sighurt-engine-cef` is `AGPL-3.0-or-later`; the CEF and Chromium BSD notices are in that crate's README. Ladybird is BSD-2-Clause and is loaded at run time; the shim and `sighurt-engine-ladybird` are `AGPL-3.0-or-later`. `sighurt-engine-netsurf` is `GPL-2.0-only`: do not add `engine-netsurf` to a default feature set, and do not distribute a `sig` built with it. Building and running that combination yourself is personal use. By contributing, you agree that your contributions are licensed the same way as the files you contribute to.
