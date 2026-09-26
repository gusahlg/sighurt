# Sighurt

Sighurt (`sig`) is a minimal, engine-agnostic browser. It draws two things and nothing else:

- **What engines send.** Every engine is a separate, sandboxed process that speaks [`sighurt-ipc`](./sighurt-ipc/src/lib.rs) on stdin/stdout, one process per page. It sends either finished RGBA frames or a list of draw operations (rectangles, paths, text, images, clips) that `sig` paints itself.
- **What the UI code says.** The chrome (tab strip, toolbar with URL bar, status bar, `sig://home`) is trusted GPUI code in `sighurt-ui`.

Two engines come with it:

- **`sig-wasm`** runs `.wasm` apps (Rust compiled to `wasm32-unknown-unknown`) in a Wasmtime sandbox with no WASI. Apps reach the host only through explicit APIs. It speaks Oxide's app ABI, so Oxide apps run unchanged.
- **`sig-servo`** renders HTML/CSS/JavaScript with [Servo](https://servo.org). It is built separately.

Any other program that speaks `sighurt-ipc` becomes an engine with a few lines of config.

```text
 sig  (sighurt-browser: main.rs only)
  ├── sighurt-ui    chrome: tabs, toolbar, URL bar, status bar, sig://home
  └── sighurt-core  config, commands and keys, routing, tabs and history,
                    downloads, engine processes and the draw-list/frame renderer
                         │ sighurt-ipc on stdin/stdout, one process per page
                         │ ↓ navigation, input, resize, zoom
                         │ ↑ frames or draw lists, URL, title, load state
          ┌──────────────┴──────────────┐
      sig-wasm                      sig-servo, or any program
      (.wasm apps, "oxide" ABI)     speaking sighurt-ipc
```

## Build and run

You need Rust (stable) with the guest target (`rustup target add wasm32-unknown-unknown`) and the native libraries GPUI and the WASM engine link: X11/Wayland, xkbcommon, fontconfig, GTK/GLib, FFmpeg, ALSA, udev and v4l. On NixOS, `nix-shell` (see [`shell.nix`](./shell.nix)) provides everything, including what Servo needs. On Debian/Ubuntu, see the `apt-get install` line in [`ci.yml`](./.github/workflows/ci.yml). On macOS, `brew install ffmpeg pkg-config`.

```bash
nix-shell                                  # NixOS only
cargo build                                # builds sig and sig-wasm into target/debug
cargo run -p sighurt-browser               # opens sig://home
cargo run -p sighurt-browser -- --help
```

```text
sig [URL|PATH ...]     open each argument in its own tab (none: the home page)
sig --default-config   print the built-in configuration
sig --help | --version
```

An argument that names an existing file or directory becomes a `file://` URL. Anything else is treated like URL bar input: `example.com` becomes `https://example.com`, and text that doesn't look like an address becomes a search.

### WASM apps

```bash
cargo build --target wasm32-unknown-unknown --release -p hello-sighurt
cargo run -p sighurt-browser -- target/wasm32-unknown-unknown/release/hello_sighurt.wasm
```

Every crate in [`examples/`](./examples/) except `fullstack-notes/backend` is a WASM app. [`index`](./examples/index/) links to the others by relative URL, so build them all and open it:

```bash
cargo build --target wasm32-unknown-unknown --release -p index -p hello-sighurt \
  -p typography-demo -p gradient-demo -p raf-demo -p timer-demo -p events-demo -p sse-demo \
  -p audio-player -p fullstack-notes-frontend
cargo run -p sighurt-browser -- target/wasm32-unknown-unknown/release/index.wasm
```

### Web pages (Servo)

`sig-servo` has its own Cargo workspace. It needs Servo's build dependencies and the first build is long; see [`sighurt-engine-servo/README.md`](./sighurt-engine-servo/README.md).

```bash
cargo build --release --manifest-path sighurt-engine-servo/Cargo.toml
cp sighurt-engine-servo/target/release/sig-servo target/debug/
cargo run -p sighurt-browser -- https://example.com
```

`sig` looks for each engine's program next to its own binary, then on `PATH`. `sig://home` lists the configured engines and whether each was found.

## Crates

| Crate | What it is |
|-------|------------|
| [`sighurt-browser`](./sighurt-browser/) | Binary `sig`: command-line options and wiring (`main.rs` only). |
| [`sighurt-core`](./sighurt-core/) | Everything but the chrome: config, commands and keymap, routing, the session (tabs, history), downloads, and the engine host that runs engine processes and paints their frames and draw lists. |
| [`sighurt-ui`](./sighurt-ui/) | The default chrome. Swapping the UI means changing `sighurt-browser/src/main.rs`. |
| [`sighurt-ipc`](./sighurt-ipc/) | The engine protocol. Zero dependencies. |
| [`sighurt-engine-wasm`](./sighurt-engine-wasm/) | Binary `sig-wasm`: the WASM engine. |
| [`sighurt-engine-servo`](./sighurt-engine-servo/) | Binary `sig-servo`: the Servo engine. Separate workspace. |
| [`sighurt-sdk`](./sighurt-sdk/) | Guest SDK for WASM apps. |
| [`examples/`](./examples/) | WASM apps, plus the native backend of `fullstack-notes`. |

## Configuration

All settings live in TOML; there is no settings screen. The built-in configuration is [`sighurt-core/src/default-config.toml`](./sighurt-core/src/default-config.toml), which documents the format; `sig --default-config` prints it. Your file is `sighurt/config.toml` in the platform config directory (`~/.config/sighurt/config.toml` on Linux, `~/Library/Application Support/sighurt/config.toml` on macOS). It is merged over the defaults when `sig` starts:

- `home`, `search` and `default_engine` replace the defaults.
- `[engines.<name>]` tables merge by name, field by field. A new name adds an engine.
- `[keys]` and `[ui]` are yours alone: there are no default bindings or UI settings.

A file that isn't valid TOML or has an unknown key is ignored with a warning. A bad key binding is skipped with a warning.

```toml
home = "https://example.com"

[engines.servo]
command = ["/opt/sig-servo/sig-servo"]   # only the command changes

[engines.gemini]                         # a new engine
command = ["sig-gemini"]
schemes = ["gemini"]

[ui.colors]
bg = "#000000"
```

### Engines and routing

| Key | Meaning |
|-----|---------|
| `command` | Program and arguments. A bare name is looked up next to `sig`, then on `PATH`. |
| `schemes` | URL schemes the engine handles, other than http, https and file. |
| `extensions` | File extensions it renders (`wasm`, not `.wasm`). |
| `mime` | MIME types it renders. `image/*` matches every image type. |

Engines are tried in alphabetical order and the first match wins:

1. `sig://home` is drawn by the UI. Any other `sig://` URL shows an error.
2. A scheme other than http, https and file goes to the engine that lists it (by default `data:` and `about:` go to Servo), or can't be opened.
3. The file extension of the URL's path is matched against `extensions`.
4. Other `file:` URLs go to `default_engine`.
5. Other http(s) URLs are probed with a HEAD request (GET if HEAD fails). The `Content-Type` is matched against `mime`; a type no engine lists is downloaded to your download folder. If the probe fails, `default_engine` gets the URL.

### Keys and commands

Everything `sig` does is a named command. The toolbar and tab strip run commands, and so do key bindings. There are no default bindings: bind keys in your own config.

```toml
[keys]                          # everywhere
"secondary-t" = "tab.new"
"secondary-w" = "tab.close"
"secondary-1" = "tab.select 1"
"secondary-l" = "location.focus"
"secondary-=" = "page.zoom-in"

[keys.page]                     # only while the page has focus
"g g" = "location.focus"

[keys.location]                 # only while the URL bar has focus
"alt-enter" = "page.open https://example.com"
```

| Area | Commands |
|------|----------|
| `tab` | `tab.new`, `tab.close [n]`, `tab.next`, `tab.previous`, `tab.select <n>`, `tab.last` |
| `page` | `page.open <url>`, `page.reload`, `page.stop`, `page.back`, `page.forward`, `page.zoom-in`, `page.zoom-out`, `page.zoom-reset` |
| `location` | `location.focus` |
| `browser` | `browser.home`, `browser.quit` |

Keys use GPUI keystroke syntax: modifiers `ctrl`, `alt`, `shift`, `cmd` and `secondary` (cmd on macOS, ctrl elsewhere), with space-separated sequences such as `"ctrl-x ctrl-c"`. The browser sees keys before the page, so a `[keys.page]` binding takes that key away from the page.

## Writing WASM apps

A WASM app is a Rust `cdylib` for `wasm32-unknown-unknown` that depends on [`sighurt-sdk`](./sighurt-sdk/) by path (it is not on crates.io). It draws its whole UI on a canvas and polls input; there are no widgets.

```rust
use sighurt_sdk::*;

#[no_mangle]
pub extern "C" fn start_app() {
    log("Hello from Sighurt!");
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    canvas_clear(30, 30, 46, 255);
    canvas_text(20.0, 30.0, 24.0, 255, 255, 255, 255, "My app");
}
```

Host functions live in the WASM import module `"oxide"`, Oxide's app ABI, so apps built for Oxide (including with the [`oxide-sdk`](https://crates.io/crates/oxide-sdk) crate) run unchanged unless they use Oxide's widget kit. The guide and API overview are in [DOCS.md](./DOCS.md).

## Security

Engines are separate processes and `sig` treats what they send as untrusted: message sizes, text, images, draw lists and coordinates are all bounded, pages can't open `sig://` URLs, and only the front tab may open new tabs, at most one a second. WASM apps get no capabilities by default, and camera, microphone, location and screen capture need a per-origin grant. Servo pages get Servo's security model. An engine you add to the config runs with your privileges. See [SECURITY.md](./SECURITY.md) to report a vulnerability.

## Development

[CONTRIBUTING.md](./CONTRIBUTING.md) covers the checks and how to add an engine, a command or a host function. [ROADMAP.md](./ROADMAP.md) lists what's next. Before committing:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

## Credits and license

Sighurt started as a fork of [Oxide](https://github.com/niklabh/oxide) by Nikhil Ranjan.

Licensed under Apache-2.0, like Oxide. See [LICENSE](./LICENSE) and [NOTICE](./NOTICE). The exception is `sighurt-engine-servo`, which is `Apache-2.0 AND MPL-2.0`: its `src/rendering.rs` is adapted from [Servo](https://github.com/servo/servo) and stays under the Mozilla Public License 2.0, and `sig-servo` links Servo, which is MPL-2.0.
