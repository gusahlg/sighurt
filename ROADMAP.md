# Sighurt Roadmap

Sighurt should stay a small browser whose important mechanisms are understandable, replaceable and measurable. The rewrite follows the milestones of [`sighurt-rewrite-plan.md`](./sighurt-rewrite-plan.md) §23; [`docs/rewrite/STATUS.md`](./docs/rewrite/STATUS.md) has each criterion with its evidence, and every decision.

## Done

- [x] **Engines in one process, no IPC** (M0, M1). Servo 0.5, Wasmtime and WPE WebKit link and run together in `sig`; Servo presents with OpenGL straight into its page surface, with no page read back.
- [x] **A core without platform** (M2). Tabs, commands, routing, history across engines, permissions, configuration and the session are plain Rust, tested without a display or an engine.
- [x] **The Wayland host and a CPU chrome** (M3). One window of subsurfaces, fractional scaling, input methods, the clipboard, bounded buffers, and nothing painted or woken while idle.
- [x] **Servo** (M4) and **the Wasm engine** (M5) as libraries, with the SDK and the example apps unchanged.
- [x] **WPE WebKit** for compatibility: a default engine for the sites the configuration gives it, presenting WebKit's DMA-BUFs without a copy.
- [x] **Configuration** in one checked TOML file, applied live where it can be: per-site engines and `page.open-in`, links that follow the routing, engine settings (user agent, ephemeral storage), search keywords, default zoom, scroll speed, the mouse's side buttons, the download folder, session restore, fonts and the chrome's options.
- [x] **Small keymap and chrome APIs**, with the defaults built on them and two proofs: the modal keymap (modes and key sequences) and the line chrome.
- [x] **Measured against the old browser** (M7, on one machine, [`bench/results/IPC-REMOVAL.md`](./bench/results/IPC-REMOVAL.md)): animated Servo pages use 43–76 % less CPU, idle wakeups went from 191 to 0.1 a second, memory roughly halved (one article 375 → 184 MiB, eight tabs 978 → 409 MiB), and no page is read back.

## Next

Before the rewrite replaces the old browser (M4, M6, M7):

- [ ] The daily workflows the browser must support, named by its user, and each one working.
- [ ] A permission prompt and a file picker in `sig` (both refused for now), shared by every engine, replacing the Wasm engine's own prompt.
- [ ] `<select>` popups, context menus, dialogs (`alert`/`confirm`/`prompt`) and HTTP authentication for Servo and WPE WebKit.
- [ ] Input, resizing and scaling on River with gharial (so far `sig` has run there only to be looked at, without input), and a second compositor family.
- [ ] Decisions on the regressions M7 measured: tab-churn memory, the compositor's cost for animated Wasm apps, static Wasm apps waking the process.

Engines:

- [ ] Stop for a Servo navigation still waiting for its response, which Servo 0.5 can't cancel (accepted as its limit for now).
- [ ] WPE WebKit's `requestAnimationFrame` at the output's rate on fast displays, and its accessibility.
- [ ] Deliver `on_timer` and `on_event` to Wasm apps without requiring `on_frame`, and their downloads through the browser's download service.

Later (plan §23, one at a time, chosen on evidence): a GPU chrome, a faster Wasm renderer, process isolation for engines, richer accessibility.
