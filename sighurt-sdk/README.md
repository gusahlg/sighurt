# Sighurt SDK

Guest-side SDK for building WebAssembly applications for the WASM engine of
Sighurt, a browser shell that hosts content engines. Guest apps are Rust
libraries compiled to `wasm32-unknown-unknown` that draw on a canvas and
reach the host only through explicit capability APIs.

This crate provides safe Rust wrappers around the raw host-imported functions
exposed by the `"oxide"` import module. It has **zero dependencies**.

`sighurt-sdk` is a fork of [`oxide-sdk`](https://crates.io/crates/oxide-sdk) from
[Oxide](https://github.com/niklabh/oxide) by Nikhil Ranjan. The import module keeps
the name `"oxide"` because it is the Oxide app ABI, the way `wasi_snapshot_preview1`
keeps its name. Apps built for Oxide, including ones built with the upstream
`oxide-sdk` crate, run unchanged in Sighurt's WASM engine unless they use Oxide's
widget kit (`ui_*`), which Sighurt does not provide.

## Quick Start

`sighurt-sdk` is not published on crates.io under this name. Depend on it by path
from a Sighurt checkout, or use the upstream `oxide-sdk = "0.7"` from crates.io,
which speaks the same ABI apart from the widget kit (then write `use oxide_sdk::*`).

```toml
[package]
name = "my-app"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
sighurt-sdk = { path = "../sighurt/sighurt-sdk" }   # path to your Sighurt checkout
```

Write your app:

```rust
use sighurt_sdk::*;

#[no_mangle]
pub extern "C" fn start_app() {
    log("Hello from Sighurt!");
    canvas_clear(30, 30, 46, 255);
    canvas_text(20.0, 40.0, 28.0, 255, 255, 255, 255, "Welcome to Sighurt");
}
```

Build for wasm and open it in Sighurt:

```bash
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
sig "$PWD/target/wasm32-unknown-unknown/release/my_app.wasm"
```

## Available APIs

| Category | Functions |
|----------|-----------|
| **Canvas** | `canvas_clear`, `canvas_rect`, `canvas_circle`, `canvas_text`, `canvas_line`, `canvas_image`, `canvas_dimensions` |
| **Console** | `log`, `warn`, `error` |
| **Input** | `mouse_position`, `mouse_button_down/clicked`, `key_down/pressed`, `text_input`, `scroll_delta`, `modifiers` |
| **Storage** | `storage_set/get/remove` (session), `kv_store_set/get/delete` (persistent) |
| **Networking** | `fetch`, `fetch_get`, `fetch_post`, `fetch_put`, `fetch_delete`, streaming `fetch_begin` / `fetch_recv` |
| **Navigation** | `navigate`, `push_state`, `replace_state`, `get_url`, `history_back/forward` |
| **Crypto** | `hash_sha256`, `hash_sha512`, `hmac_sha256`, `random_bytes`, `uuid_v4`, `base64_encode/decode` |
| **Compression** | `compress`, `decompress` |
| **System** | `system_theme`, `system_locale`, `system_timezone`, `battery_level` |
| **Clipboard** | `clipboard_read`, `clipboard_write` (always refused by the engine) |
| **Time / Random** | `time_now_ms`, `random_u64`, `random_f64` |
| **Dynamic loading** | `load_module` |

There are no host-rendered widgets: apps draw their whole UI with the canvas API and
poll input each frame. A text field is a rectangle the app draws itself, fed by
`text_input()` plus `key_pressed(KEY_ENTER)` / `key_pressed(KEY_BACKSPACE)`.

## License

Apache-2.0. Based on Oxide by Nikhil Ranjan (github.com/niklabh/oxide).
