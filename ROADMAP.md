# Sighurt Roadmap

Sighurt should stay a minimal browser that shows what engines draw. Items ship as they're ready.

## Done

- [x] **Every engine out of process.** Engines are separate processes speaking `sighurt-ipc` v2: finished frames or draw lists plus images, painted by `sig`. Everything they send is bounded, the handshake times out, pages can't open `sig://` URLs and new tabs from pages are throttled.
- [x] **Split browser.** `sighurt-core` (config, commands, routing, session, downloads, engine host), `sighurt-ui` (default chrome, colors from `[ui.colors]`) and `sighurt-browser` (`sig`, wiring only).
- [x] **WASM engine** (`sig-wasm`): Wasmtime sandbox with no WASI, speaking Oxide's `"oxide"` app ABI.
- [x] **Servo engine** (`sig-servo`): headless Servo rendering HTML/CSS/JS, in its own workspace.
- [x] **Commands, keys and config in TOML.** Named commands, no default key bindings (global, `page` and `location` contexts), config-driven routing by scheme, extension or `Content-Type`.

## Next

Protocol:

- [ ] Shared-memory frames with damage rectangles, instead of full RGBA frames through a pipe.
- [ ] Mouse-leave and IME composition messages.
- [ ] Capabilities in `Hello`, so an engine can say what it supports.
- [ ] `<select>` popups, context menus and dialogs (`alert`/`confirm`/`prompt`, file pickers) for Servo.

Browser:

- [ ] Modal (vim-like) key handling on top of the key contexts.
- [ ] One permission prompt and grant store for every engine, replacing the WASM engine's own prompt.
- [ ] Per-engine settings passed from the config to the engine at startup.
- [ ] Session restore.

WASM engine:

- [ ] Deliver `on_timer` and `on_event` without requiring `on_frame`.

Packaging:

- [ ] Ship `sig-servo` in releases, next to `sig` and `sig-wasm`.
