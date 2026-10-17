# Contributing to Sighurt

Sighurt is meant to stay small. Before adding a feature, check whether it belongs in an engine, in the configuration or nowhere. The [README](./README.md) describes the browser and its configuration, [DOCS.md](./DOCS.md) the WASM engine and SDK, [`sighurt-rewrite-plan.md`](./sighurt-rewrite-plan.md) the design, [`docs/rewrite/STATUS.md`](./docs/rewrite/STATUS.md) what has been decided, and [CLAUDE.md](./CLAUDE.md) the conventions in more detail.

## Setup

See [Build and run](./README.md#build-and-run) for the prerequisites (`nix-shell` on NixOS, and the WPE WebKit prefix for `sig`'s default features).

```bash
cargo build -p sighurt-browser --no-default-features --features engine-demo   # quick: no Servo
cargo build -p sighurt-browser                                                # Servo, WPE WebKit, Wasm
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt
target/debug/sig target/wasm32-unknown-unknown/release/hello_sighurt.wasm
```

Build only the crates you need (`-p <crate>`): `--workspace` and `sig`'s default features compile Servo, and the crates and experiments that link Servo or WPE WebKit (`sighurt-engine-servo`, `sighurt-engine-wpe`, `sighurt-browser` with its defaults, `m0-coexist`, `m1-mixed`, `wpe-coexist`) take long to build.

## Checks

CI runs the checks that need neither Servo nor WPE WebKit; run them before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --exclude sighurt-engine-servo --exclude sighurt-engine-wpe \
  --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-coexist \
  --all-targets -- -D warnings
cargo clippy -p sighurt-browser --no-default-features \
  --features engine-demo,engine-wasm,keymap-modal,chrome-line --all-targets -- -D warnings
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt -p raf-demo -p gradient-demo -p typography-demo
cargo test --workspace --exclude sighurt-engine-servo --exclude sighurt-engine-wpe \
  --exclude sighurt-browser --exclude m0-coexist --exclude m1-mixed --exclude wpe-coexist
cargo test -p sighurt-browser --no-default-features --features engine-demo,engine-wasm,keymap-modal,chrome-line
```

If you touched an engine or `sig`, also run what builds it:

```bash
cargo clippy -p sighurt-engine-servo -p sighurt-engine-wpe -p sighurt-browser --all-targets -- -D warnings
cargo clippy -p sighurt-browser --all-features --all-targets -- -D warnings
cargo test -p sighurt-engine-servo -p sighurt-engine-wpe --lib
cargo test -p sighurt-browser --all-features
```

The tests that need a compositor run in a private headless sway (`tools/headless-sway`), never on your desktop, since they type and click. Each script says what it checks:

| Script | What it runs |
|---|---|
| `sighurt-browser/tests/headless.sh OUT` | `sig` with the demo engine: tabs, typing, coordinates, scaling, buffers, idle, the configuration, the modal keymap, settings |
| `sighurt-browser/tests/engines.sh OUT` | `sig` with Servo, WPE WebKit and Wasm on the GPU: pages, input, history, links between engines, settings, a configuration showcase |
| `sighurt-ui/tests/headless.sh OUT` | the chromes in `sig`: the line chrome, the status overlay, tabs on the left, fonts |
| `sighurt-wayland/tests/headless.sh` | the Wayland host against sway |
| `sighurt-engine-{servo,wpe,wasm}/tests/headless.sh` | each engine's compositor tests |

They print `PASS` or `FAIL` for each check and leave screenshots to look at in `OUT`.

## Guidelines

- **Engines stay out of the browser's state.** Engines implement `sighurt_api::engine::EngineRuntime`, report what happens as events and never change browser state; the core decides (`update(state, event) -> effects`, no I/O), and `sig` carries the effects out. Read the trait's documentation before you change an engine.
- **The core has no platform.** `sighurt-core` builds and tests without Wayland, Servo, WebKit, Wasmtime or a graphics driver.
- **Keys live in the user's configuration.** No default bindings and no key handlers in code: everything goes through commands and the keymap.
- **No copies, no ticks.** Nothing reads a page back to the CPU to show it, the chrome repaints only what changed, and an idle browser sleeps. The end-to-end tests check all three.
- **Treat what pages and guests say as untrusted.** Keep the core's bounds, validate every guest `(ptr, len)`, never link WASI.
- Small, focused commits and PRs. Test real behavior, not the implementation's own assumptions. Every public SDK function has a doc comment, and every module says what it owns and what it must not.
- Match the surrounding style; don't refactor unrelated code.

## Adding an engine

An engine is a crate `sighurt-engine-<name>` that depends on `sighurt-api` and `sighurt-wayland` and implements `EngineRuntime` with the host's `TargetLease` and `FramePermit`:

1. Open views on the leases the host gives you; present only with a frame permit; report `FrameReady` when there is something new to paint.
2. Report navigations, history, titles, loading, cursors and text input as `EngineEvent`s from `pump`, each with its view, navigation and document ids. Ask before performing a navigation the page started (`NavigationRequested`), so links follow the routing.
3. Send clipboard, download, file and permission requests as `ServiceRequested` and wait for the answer; never perform them yourself.
4. Declare what you support in `EngineCapabilities` and return `EngineError::Unsupported` for the rest. Set `process_per_view` if each view is an OS process of its own: a tab then keeps fewer of your views alive in its history.
5. Shut down in an explicit order in `shutdown`; never exit the process or install process-wide handlers.

Then add a feature `engine-<name>` to `sighurt-browser/Cargo.toml`, `sighurt-runtime/Cargo.toml` and `sighurt-gui/Cargo.toml`, the engine's descriptor to `sighurt-runtime/src/engines.rs` (its name, capabilities and the `[engines.<name>]` settings it honors), and how the GUI starts it to `sighurt-gui/src/engines.rs`. `sighurt-engine-demo` is the smallest complete engine.

## Adding a keymap or a chrome

See [Writing your own keymap or chrome](./README.md#writing-your-own-keymap-or-chrome) in the README.

## Adding a command

1. Add a variant to `Command` in `sighurt-api/src/commands.rs`, with its name in `NAMES`, `name`, its parsing and `Display`.
2. Carry it out in `sighurt-core`.
3. List it in `sighurt-core/src/default-config.toml` and in the README's command table.

## Adding a host function

1. Register it with `linker.func_wrap("oxide", "api_<category>_<action>", …)` in `sighurt-engine-wasm/src/capabilities.rs`, or in a module's `register_<feature>_functions`. Read guest memory only through `guest_bytes` / `guest_str` and write it through `write_guest` / `write_prefix`, and bound what a guest can make the host allocate.
2. Wrap it in `sighurt-sdk/src/lib.rs`: with `host_fns!` when the wrapper passes arguments and results through unchanged, otherwise an `_api_<name>` import plus a hand-written `<category>_<action>` wrapper. Give it a doc comment.
3. Mention it in [DOCS.md](./DOCS.md)'s API overview.

The import module name `"oxide"` is a permanent ABI: never rename it.

## Pull requests

Describe what changed, why and how you tested it. PRs are squash-merged. To report a security issue, follow [SECURITY.md](./SECURITY.md) instead of opening an issue.

## License

Sighurt is licensed under `AGPL-3.0-or-later` (see [LICENSE](./LICENSE) and [NOTICE](./NOTICE)); `sighurt-engine-servo` is `AGPL-3.0-or-later AND MPL-2.0`, and `sighurt-sdk` and the example apps are `Apache-2.0` (see [LICENSE-APACHE](./LICENSE-APACHE)). By contributing, you agree that your contributions are licensed the same way as the files you contribute to.
