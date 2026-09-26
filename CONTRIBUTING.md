# Contributing to Sighurt

Sighurt is meant to stay small. Before adding a feature, check whether it belongs in an engine, in the config or nowhere. The [README](./README.md) describes the architecture, [DOCS.md](./DOCS.md) the WASM engine and SDK, and [CLAUDE.md](./CLAUDE.md) the conventions in more detail.

## Setup

See [Build and run](./README.md#build-and-run) for prerequisites (`nix-shell` on NixOS).

```bash
cargo build                                                      # sig and sig-wasm
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt
cargo run -p sighurt-browser -- target/wasm32-unknown-unknown/release/hello_sighurt.wasm
```

## Checks

CI runs these; run them before pushing:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --target wasm32-unknown-unknown --workspace --exclude sighurt-browser \
  --exclude sighurt-core --exclude sighurt-ui --exclude sighurt-engine-wasm \
  --exclude fullstack-notes-backend

# Only if you touched sighurt-engine-servo or sighurt-ipc (a separate workspace):
cargo fmt --manifest-path sighurt-engine-servo/Cargo.toml -- --check
cargo clippy --manifest-path sighurt-engine-servo/Cargo.toml -- -D warnings
```

## Guidelines

- **Engines stay out of the browser.** No browser crate (`sighurt-browser`, `sighurt-core`, `sighurt-ui`) links an engine or imports its internals. The only interface is `sighurt-ipc`.
- **Keys live in the user's config.** No default bindings and no key handlers in code.
- **Treat engine output and guest input as untrusted.** Keep the bounds in `sighurt-ipc` and `sighurt-core/src/page.rs`; validate every guest `(ptr, len)`; never link WASI.
- Small, focused commits and PRs. Test pure logic (routing, config, protocol encoding). Every public SDK function has a doc comment.
- Match the surrounding style; don't refactor unrelated code.

## Adding an engine

An engine is any executable that speaks [`sighurt-ipc`](./sighurt-ipc/src/lib.rs) on stdin/stdout. `sig` starts one process per page.

1. Write `FromEngine::Hello { version: VERSION, name }` first, within 10 seconds.
2. Handle `Resize` and `Navigate`, then input (`MouseMove`, `MouseButton`, `Wheel`, `Key`, `Focus`), `Zoom`, `Reload`, `Stop`, `Back` and `Forward`.
3. Send the page as `Frame` (RGBA pixels) or as `Draw` lists (with `Image` uploads), plus `Url`, `Title`, `Loading`, `History`, `Cursor`, `Status`, `Console`, `Open` and `Error` as they change.
4. Log to stderr; stdout belongs to the protocol. Exit when stdin closes.
5. Add it to your config:

   ```toml
   [engines.gemini]
   command = ["sig-gemini"]
   schemes = ["gemini"]
   ```

Messages are length-prefixed and little-endian, so an engine can be written in any language. A Rust engine can depend on `sighurt-ipc` and call `ToEngine::read_from` / `FromEngine::write_to`. `sighurt-engine-wasm/src/main.rs` shows the whole loop.

## Adding a command

1. Add the name to `COMMANDS` in `sighurt-core/src/commands.rs`.
2. Handle it in `Browser::run_command` in `sighurt-core/src/browser.rs`.
3. List it in the comment in `sighurt-core/src/default-config.toml` and in the README's command table.

## Adding a host function

1. Register it with `linker.func_wrap("oxide", "api_<category>_<action>", …)` in `sighurt-engine-wasm/src/capabilities.rs`, or in a module's `register_<feature>_functions`. Read guest memory only through `guest_bytes` / `guest_str` and write it through `write_guest` / `write_prefix`.
2. Wrap it in `sighurt-sdk/src/lib.rs`: with `host_fns!` when the wrapper passes arguments and results through unchanged, otherwise an `_api_<name>` import plus a hand-written `<category>_<action>` wrapper. Give it a doc comment.
3. Mention it in [DOCS.md](./DOCS.md)'s API overview.

The import module name `"oxide"` is a permanent ABI: never rename it.

## Pull requests

Describe what changed, why and how you tested it. PRs are squash-merged. To report a security issue, follow [SECURITY.md](./SECURITY.md) instead of opening an issue.

## License

Sighurt is licensed under Apache-2.0 (see [LICENSE](./LICENSE) and [NOTICE](./NOTICE)); `sighurt-engine-servo` is `Apache-2.0 AND MPL-2.0`. By contributing, you agree that your contributions are licensed the same way.
