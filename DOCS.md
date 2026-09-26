# WASM engine and SDK guide

`sig-wasm` is Sighurt's engine for WebAssembly apps. `sig` starts one `sig-wasm` process per page and talks to it over `sighurt-ipc`. The engine fetches the `.wasm`, runs it in a Wasmtime sandbox and sends what the app draws back to `sig` as a draw list. Apps are Rust crates built for `wasm32-unknown-unknown` against [`sighurt-sdk`](./sighurt-sdk/).

The guest ABI is Oxide's: every host function is imported from the WASM module `"oxide"`. Apps built for [Oxide](https://github.com/niklabh/oxide) run unchanged, except ones that use Oxide's widget kit (`ui_*`), which this engine doesn't provide.

For building `sig` itself, see the [README](./README.md).

## A first app

```bash
cargo new --lib my-app
```

`Cargo.toml` (`sighurt-sdk` is not on crates.io, so depend on it by path):

```toml
[lib]
crate-type = ["cdylib"]

[dependencies]
sighurt-sdk = { path = "../sighurt/sighurt-sdk" }

[profile.release]
opt-level = "s"
lto = true
```

`src/lib.rs`: a text field and a button, drawn by the app itself.

```rust
use std::cell::RefCell;
use sighurt_sdk::*;

thread_local! {
    static NAME: RefCell<String> = const { RefCell::new(String::new()) };
}

#[no_mangle]
pub extern "C" fn start_app() {
    log("started");
}

#[no_mangle]
pub extern "C" fn on_frame(_dt_ms: u32) {
    NAME.with_borrow_mut(|name| {
        name.push_str(&text_input());
        if key_pressed(KEY_BACKSPACE) {
            name.pop();
        }

        canvas_clear(30, 30, 46, 255);
        canvas_rounded_rect(20.0, 20.0, 240.0, 32.0, 6.0, 50, 50, 70, 255);
        canvas_text(30.0, 28.0, 16.0, 255, 255, 255, 255, name);

        let (mx, my) = mouse_position();
        let over = (20.0..120.0).contains(&mx) && (70.0..100.0).contains(&my);
        canvas_rounded_rect(20.0, 70.0, 100.0, 30.0, 6.0, 70, 70, if over { 140 } else { 100 }, 255);
        canvas_text(34.0, 77.0, 14.0, 255, 255, 255, 255, "Greet");
        if (over && mouse_button_clicked(0)) || key_pressed(KEY_ENTER) {
            log(&format!("Hello, {name}!"));
        }
    });
}
```

Build and open it:

```bash
cargo build --target wasm32-unknown-unknown --release
sig target/wasm32-unknown-unknown/release/my_app.wasm
```

`sig` routes URLs ending in `.wasm`, and http(s) responses with `Content-Type: application/wasm`, to this engine. To serve apps over HTTP, any static server works: `python3 -m http.server 8080` in the output directory, then open `http://localhost:8080/my_app.wasm`.

## Guest contract

1. Export `start_app()`, called once after the module loads.
2. Optionally export `on_frame(dt_ms: u32)`, called about 60 times a second.
3. Optionally export `on_timer(callback_id: u32)` for `set_timeout`, `set_interval` and `request_animation_frame`.
4. Optionally export `on_event(callback_id: u32)` for built-in and custom events, called before timers and `on_frame`.

   `on_timer` and `on_event` are delivered from the frame loop, so they only run if the module also exports `on_frame`.
5. Build as a `cdylib` for `wasm32-unknown-unknown`, and import only from `"oxide"` (never WASI).

Without the SDK, a valid guest is:

```rust
#[link(wasm_import_module = "oxide")]
extern "C" {
    fn api_log(ptr: u32, len: u32);
    fn api_canvas_clear(r: u32, g: u32, b: u32, a: u32);
}

#[no_mangle]
pub extern "C" fn start_app() {
    let msg = "Hello from raw WASM!";
    unsafe {
        api_log(msg.as_ptr() as u32, msg.len() as u32);
        api_canvas_clear(20, 20, 40, 255);
    }
}
```

## Drawing and input

The app draws each frame onto a canvas in logical pixels, divided by the page zoom; `canvas_dimensions()` gives its size. The engine turns the canvas into `sighurt-ipc` draw operations, and `sig` paints them with its own fonts. There are no widgets: buttons, fields and lists are shapes the app draws and hit-tests against `mouse_position()`. Typed text comes from `text_input()`; keys from `key_pressed` / `key_down` with the `KEY_*` constants. For content taller than the page, call `set_content_size` and offset drawing by `scroll_position()`.

The [`draw`](./sighurt-sdk/src/draw.rs) module wraps the canvas functions in `Canvas`, `Color`, `Rect` and `Point2D`:

```rust
use sighurt_sdk::draw::*;

let c = Canvas::new();
c.clear(Color::hex(0x1e1e2e));
c.fill_rect(Rect::new(10.0, 10.0, 200.0, 100.0), Color::rgb(80, 120, 200));
c.text("Hello!", Point2D::new(20.0, 30.0), 24.0, Color::WHITE);
```

## API overview

Every function has a doc comment in [`sighurt-sdk/src/lib.rs`](./sighurt-sdk/src/lib.rs) (`cargo doc --no-deps -p sighurt-sdk --open`). The examples in [`examples/`](./examples/) use most of them.

| Area | Functions |
|------|-----------|
| Canvas | `canvas_clear`, `canvas_rect`, `canvas_rounded_rect`, `canvas_circle`, `canvas_arc`, `canvas_bezier`, `canvas_line`, `canvas_gradient`, `canvas_text`, `canvas_text_ex`, `canvas_measure_text`, `canvas_image`, `canvas_dimensions`, `canvas_save`, `canvas_restore`, `canvas_transform`, `canvas_clip`, `canvas_opacity` |
| Scrolling | `set_content_size`, `scroll_position`, `set_scroll_position`, `scroll_delta` |
| Input | `mouse_position`, `mouse_button_down`, `mouse_button_clicked`, `key_down`, `key_pressed`, `text_input`, `modifiers`, `shift_held`, `ctrl_held`, `alt_held` |
| Console | `log`, `warn`, `error` |
| Timers | `set_timeout`, `set_interval`, `clear_timer`, `request_animation_frame`, `cancel_animation_frame`, `time_now_ms` |
| Events | `on_event`, `off_event`, `emit_event`, `event_type`, `event_data`, `event_data_into` |
| Navigation | `navigate`, `push_state`, `replace_state`, `get_url`, `get_state`, `history_length`, `history_back`, `history_forward`, `register_hyperlink`, `clear_hyperlinks`, `url_resolve`, `url_encode`, `url_decode` |
| HTTP | `fetch`, `fetch_get`, `fetch_post`, `fetch_post_proto`, `fetch_put`, `fetch_delete`; streaming: `fetch_begin`, `fetch_begin_get`, `fetch_state`, `fetch_status`, `fetch_recv`, `fetch_recv_into`, `fetch_error`, `fetch_abort`, `fetch_remove` |
| WebSocket | `ws_connect`, `ws_send_text`, `ws_send_binary`, `ws_recv`, `ws_ready_state`, `ws_close`, `ws_remove` |
| Server-sent events | `sse_open`, `sse_state`, `sse_recv`, `sse_error`, `sse_close`, `sse_remove` |
| WebRTC | `rtc_create_peer`, `rtc_create_offer`, `rtc_create_answer`, `rtc_set_local_description`, `rtc_set_remote_description`, `rtc_add_ice_candidate`, `rtc_poll_ice_candidate`, `rtc_create_data_channel`, `rtc_send`, `rtc_recv`, `rtc_add_track`, `rtc_poll_track`, `rtc_signal_connect`, … |
| Storage | `storage_set`, `storage_get`, `storage_remove` (session, per origin); `kv_store_set`, `kv_store_get`, `kv_store_delete` (on disk, per origin) |
| Files | `upload_file`, `file_pick`, `folder_pick`, `folder_entries`, `file_read`, `file_read_range`, `file_metadata` (native dialogs; the app gets contents or opaque handles, never paths) |
| Downloads | `download_data`, `download_url`, `canvas_print_pdf` (saved to the download folder) |
| Audio | `audio_play`, `audio_play_url`, `audio_play_with_format`, `audio_pause`, `audio_resume`, `audio_stop`, `audio_seek`, `audio_set_volume`, `audio_set_loop`, `audio_channel_play`, … |
| Video | `video_load`, `video_load_url`, `video_play`, `video_pause`, `video_seek`, `video_render`, `video_hls_open_variant`, `subtitle_load_srt`, `subtitle_load_vtt`, … |
| Media capture | `camera_open`, `camera_capture_frame`, `microphone_open`, `microphone_read_samples`, `screen_capture`, … (permission-gated) |
| MIDI | `midi_input_count`, `midi_output_count`, `midi_open_input`, `midi_open_output`, `midi_send`, `midi_recv`, `midi_close` |
| Workers | `spawn_worker`, `worker_post_message`, `worker_recv`, `worker_terminate`; inside a worker: `worker_post`, `worker_message_read` |
| Modules | `load_module`: runs a child `.wasm` with its own memory and fuel |
| Crypto and encoding | `hash_sha256`, `hash_sha512`, `hmac_sha256` (+ `_hex`), `random_bytes`, `random_u64`, `random_f64`, `uuid_v4`, `base64_encode`, `base64_decode`, `compress`, `decompress`; [`proto`](./sighurt-sdk/src/proto.rs) is a small protobuf codec |
| System | `system_locale`, `system_timezone`, `system_timezone_offset_minutes`, `battery_level`, `battery_charging` |

Present for ABI compatibility but not functional:

- `gpu_*`: stubs that return 0 or `false`.
- `clipboard_write` / `clipboard_read`: always refused (logged; reads return an empty string).
- `system_theme`: always `THEME_UNKNOWN`.
- `notify`: writes the notification to the console instead of showing it.
- `get_location`: returns a fixed placeholder location once permitted.

### Events

`on_event(type, callback_id)` subscribes; the engine calls the guest's `on_event(callback_id)` export, during which `event_type()` and `event_data()` describe the event. Built-in types: `resize` (`width: u32, height: u32`), `focus`, `blur`, `visibility_change` (`"visible"` / `"hidden"`), `online`, `offline`, `touch_start` / `touch_move` / `touch_end` (`x: f32, y: f32`, synthesised from the primary mouse button), `gamepad_connected` (device name), `gamepad_button` (`id, code, pressed: u32`) and `gamepad_axis` (`id, code: u32, value: f32`). Payloads are little-endian. `emit_event` queues custom events.

### Console

Console lines go to `sig`'s log at debug level: run `RUST_LOG=sighurt_core=debug sig …` to see them.

## Permissions and manifests

Camera, microphone, geolocation and screen capture need a grant per origin. The first call returns `PERMISSION_PENDING` (`-5`) and the engine draws a prompt over the page; retry on a later frame. After **Allow** the call works, after **Block** it returns `-1`. Decisions last as long as the page's process. An origin is scheme, host and port for network URLs, and the containing directory for `file://` URLs.

An app may ship a TOML manifest next to its `.wasm`, at the same URL with `.toml` instead of `.wasm`:

```toml
name = "Audio Player"          # required; used as the page title
description = "Plays audio"    # optional
version = "0.1.0"              # optional
permissions = ["microphone"]   # camera, microphone, geolocation, screen-capture
```

With a manifest, sensitive APIs it doesn't list are denied without a prompt. Without one, any of them may prompt. Examples ship their manifest next to their source (for example `examples/audio-player/audio_player.toml`); copy it next to the built `.wasm` to use it.

## Sandbox

| Limit | Value |
|-------|-------|
| Filesystem, environment, raw sockets | None: no WASI is linked |
| Memory | 256 MB (4096 pages) |
| Fuel | 500M instructions when the module starts, 50M per `on_frame` |
| Module size | 50 MB |

The guest reaches the outside world only through the host functions registered in `sighurt-engine-wasm/src/capabilities.rs`. Network access goes through the host's HTTP, WebSocket, SSE and WebRTC clients; files only through native dialogs the user answers. Session storage is cleared when a page navigates to another origin; the kv store keeps each origin in its own directory.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| "module must export `start_app`" | Add `#[no_mangle] pub extern "C" fn start_app()`. |
| "fuel limit exceeded" | Do less work per frame, or spread it over frames or a worker. |
| Blank page | Draw something in `on_frame`, starting with `canvas_clear`. |
| `on_timer` / `on_event` never called | Export an `on_frame`, even an empty one. |
| A `.wasm` URL downloads or opens in another engine | Serve it as `application/wasm`, or use a URL ending in `.wasm`. |
| `sig-wasm` not found | Run `cargo build` (it puts `sig-wasm` next to `sig`), or put it on `PATH`. |

To add a host function, see [CLAUDE.md](./CLAUDE.md#adding-a-host-function-to-the-wasm-engine) or [CONTRIBUTING.md](./CONTRIBUTING.md#adding-a-host-function).
