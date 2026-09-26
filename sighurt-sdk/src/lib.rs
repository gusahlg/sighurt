#![allow(clippy::too_many_arguments)]

//! # Sighurt SDK
//!
//! Guest-side SDK for building WebAssembly applications for the WASM engine of
//! Sighurt, a browser shell that hosts content engines.
//! This crate provides safe Rust wrappers around the raw host-imported functions
//! exposed by the `"oxide"` wasm import module.
//!
//! The import module keeps the name `"oxide"`: it is the Oxide app ABI (like
//! `wasi_snapshot_preview1` for WASI), so apps built for [Oxide](https://github.com/niklabh/oxide),
//! including apps built with the upstream `oxide-sdk` crate, run unchanged in Sighurt's
//! WASM engine unless they use Oxide's widget kit (`ui_*`), which Sighurt does not
//! provide. This crate is a fork of `oxide-sdk`.
//!
//! The WASM engine runs as its own process (`sig-wasm`). Each frame it turns your
//! canvas calls into a list of draw operations (rectangles, paths, text, images)
//! and sends it to the browser, which paints it.
//!
//! ## Quick Start
//!
//! `sighurt-sdk` is not published on crates.io; depend on it by path:
//!
//! ```toml
//! [lib]
//! crate-type = ["cdylib"]
//!
//! [dependencies]
//! sighurt-sdk = { path = "../sighurt/sighurt-sdk" }
//! ```
//!
//! ### Static app (one-shot render)
//!
//! ```rust,ignore
//! use sighurt_sdk::*;
//!
//! #[no_mangle]
//! pub extern "C" fn start_app() {
//!     log("Hello from Sighurt!");
//!     canvas_clear(30, 30, 46, 255);
//!     canvas_text(20.0, 40.0, 28.0, 255, 255, 255, 255, "Welcome to Sighurt");
//! }
//! ```
//!
//! ### Interactive app (frame loop)
//!
//! ```rust,ignore
//! use sighurt_sdk::*;
//!
//! #[no_mangle]
//! pub extern "C" fn start_app() {
//!     log("Interactive app started");
//! }
//!
//! #[no_mangle]
//! pub extern "C" fn on_frame(_dt_ms: u32) {
//!     canvas_clear(30, 30, 46, 255);
//!     let (mx, my) = mouse_position();
//!     canvas_circle(mx, my, 20.0, 255, 100, 100, 255);
//!
//!     // A hand-drawn button: draw it, then hit-test the mouse against it.
//!     canvas_rounded_rect(20.0, 20.0, 100.0, 30.0, 6.0, 70, 70, 100, 255);
//!     canvas_text(34.0, 27.0, 14.0, 255, 255, 255, 255, "Click me!");
//!     let over = (20.0..120.0).contains(&mx) && (20.0..50.0).contains(&my);
//!     if over && mouse_button_clicked(0) {
//!         log("Button was clicked!");
//!     }
//! }
//! ```
//!
//! ### High-level drawing API
//!
//! The [`draw`] module provides ergonomic types for less
//! boilerplate:
//!
//! ```rust,ignore
//! use sighurt_sdk::draw::*;
//!
//! #[no_mangle]
//! pub extern "C" fn start_app() {
//!     let c = Canvas::new();
//!     c.clear(Color::hex(0x1e1e2e));
//!     c.fill_rect(Rect::new(10.0, 10.0, 200.0, 100.0), Color::rgb(80, 120, 200));
//!     c.fill_circle(Point2D::new(300.0, 200.0), 50.0, Color::RED);
//!     c.text("Hello!", Point2D::new(20.0, 30.0), 24.0, Color::WHITE);
//! }
//! ```
//!
//! Build with `cargo build --target wasm32-unknown-unknown --release`.
//!
//! ## API Categories
//!
//! | Category | Key types / functions |
//! |----------|-----------|
//! | **Drawing (high-level)** | [`draw::Canvas`], [`draw::Color`], [`draw::Rect`], [`draw::Point2D`], [`draw::GradientStop`] |
//! | **Canvas (low-level)** | [`canvas_clear`], [`canvas_rect`], [`canvas_circle`], [`canvas_text`], [`canvas_line`], [`canvas_image`], [`canvas_dimensions`] |
//! | **Extended shapes** | [`canvas_rounded_rect`], [`canvas_arc`], [`canvas_bezier`], [`canvas_gradient`] |
//! | **Canvas state** | [`canvas_save`], [`canvas_restore`], [`canvas_transform`], [`canvas_clip`], [`canvas_opacity`] |
//! | **GPU** (not functional: kept for compatibility, every call returns 0) | [`gpu_create_buffer`], [`gpu_create_texture`], [`gpu_create_shader`], [`gpu_create_pipeline`], [`gpu_draw`], [`gpu_dispatch_compute`] |
//! | **Console** | [`log`], [`warn`], [`error`] |
//! | **HTTP** | [`fetch`], [`fetch_get`], [`fetch_post`], [`fetch_post_proto`], [`fetch_put`], [`fetch_delete`] |
//! | **HTTP (streaming)** | [`fetch_begin`], [`fetch_begin_get`], [`fetch_state`], [`fetch_status`], [`fetch_recv`], [`fetch_error`], [`fetch_abort`], [`fetch_remove`] |
//! | **Protobuf** | [`proto::ProtoEncoder`], [`proto::ProtoDecoder`] |
//! | **Storage** | [`storage_set`], [`storage_get`], [`storage_remove`], [`kv_store_set`], [`kv_store_get`], [`kv_store_delete`] |
//! | **Download & Print** | [`download_data`], [`download_url`], [`canvas_print_pdf`] |
//! | **Audio** | [`audio_play`], [`audio_play_url`], [`audio_detect_format`], [`audio_play_with_format`], [`audio_pause`], [`audio_channel_play`] |
//! | **Video** | [`video_load`], [`video_load_url`], [`video_render`], [`video_play`], [`video_hls_open_variant`], [`subtitle_load_srt`] |
//! | **Media capture** | [`camera_open`], [`camera_capture_frame`], [`microphone_open`], [`microphone_read_samples`], [`screen_capture`] |
//! | **WebRTC** | [`rtc_create_peer`], [`rtc_create_offer`], [`rtc_create_answer`], [`rtc_create_data_channel`], [`rtc_send`], [`rtc_recv`], [`rtc_signal_connect`] |
//! | **WebSocket** | [`ws_connect`], [`ws_send_text`], [`ws_send_binary`], [`ws_recv`], [`ws_ready_state`], [`ws_close`], [`ws_remove`] |
//! | **Server-Sent Events** | [`sse_open`], [`sse_state`], [`sse_recv`], [`sse_error`], [`sse_close`], [`sse_remove`] |
//! | **MIDI** | [`midi_input_count`], [`midi_output_count`], [`midi_input_name`], [`midi_output_name`], [`midi_open_input`], [`midi_open_output`], [`midi_send`], [`midi_recv`], [`midi_close`] |
//! | **Timers** | [`set_timeout`], [`set_interval`], [`clear_timer`], [`request_animation_frame`], [`cancel_animation_frame`], [`time_now_ms`] |
//! | **Events** | [`on_event`], [`off_event`], [`emit_event`], [`event_type`], [`event_data`], [`event_data_into`] |
//! | **Navigation** | [`navigate`], [`push_state`], [`replace_state`], [`get_url`], [`history_back`], [`history_forward`] |
//! | **Input** | [`mouse_position`], [`mouse_button_down`], [`mouse_button_clicked`], [`key_down`], [`key_pressed`], [`text_input`], [`scroll_delta`], [`modifiers`] |
//! | **Crypto** | [`hash_sha256`], [`hash_sha512`], [`hmac_sha256`], [`random_bytes`], [`uuid_v4`], [`base64_encode`], [`base64_decode`] |
//! | **Compression** | [`compress`], [`decompress`], [`CompressionFormat`] |
//! | **System info** | [`system_theme`], [`system_locale`], [`system_timezone`], [`system_timezone_offset_minutes`], [`battery_level`], [`battery_charging`] |
//! | **Other** | [`clipboard_write`], [`clipboard_read`], [`random_u64`], [`random_f64`], [`notify`], [`upload_file`], [`load_module`] |
//!
//! ## Guest Module Contract
//!
//! Every `.wasm` module loaded by the WASM engine must:
//!
//! 1. **Export `start_app`** — `extern "C" fn()` entry point, called once on load.
//! 2. **Optionally export `on_frame`** — `extern "C" fn(dt_ms: u32)` for
//!    interactive apps with a render loop (called every frame, fuel replenished).
//! 3. **Optionally export `on_timer`** — `extern "C" fn(callback_id: u32)`
//!    to receive callbacks from [`set_timeout`], [`set_interval`], and [`request_animation_frame`].
//! 4. **Optionally export `on_event`** — `extern "C" fn(callback_id: u32)`
//!    to receive built-in (`resize`, `focus`, `touch_*`, `gamepad_*`, …)
//!    and custom events registered via [`on_event`] / [`emit_event`].
//!
//!    `on_timer` and `on_event` are delivered from the frame loop, so they are
//!    only called if the module also exports `on_frame`.
//! 5. **Compile as `cdylib`** — `crate-type = ["cdylib"]` in `Cargo.toml`.
//! 6. **Target `wasm32-unknown-unknown`** — no WASI, pure capability-based I/O.
//!
//! The engine provides no widgets: an app draws its whole UI with the canvas API
//! and polls input each frame. A text field, for example, is a rectangle the app
//! draws itself, fed by [`text_input`] plus [`key_pressed`] for [`KEY_ENTER`] and
//! [`KEY_BACKSPACE`].
//!
//! ## Full API Documentation
//!
//! Build the docs with `cargo doc --no-deps -p sighurt-sdk --open` for the
//! complete API reference, or browse the individual function documentation below.
//! `DOCS.md` in the repository has a guide with examples.

pub mod draw;
pub mod proto;

// ─── Raw FFI imports from the host ──────────────────────────────────────────
//
// Host functions whose public wrapper takes and returns exactly what the host
// does are declared with `host_fns!` next to that wrapper. Everything else is
// declared here and wrapped further down, where arguments and results are
// converted.

/// Declares host functions that need no conversion: each
/// `pub fn name(args) -> ret = "api_name";` becomes a safe public function that
/// calls the `"oxide"` import `api_name` with the same signature.
macro_rules! host_fns {
    ($(
        $(#[$attr:meta])*
        pub fn $name:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? = $import:literal;
    )*) => {$(
        $(#[$attr])*
        pub fn $name($($arg: $ty),*) $(-> $ret)? {
            #[link(wasm_import_module = "oxide")]
            extern "C" {
                #[link_name = $import]
                fn import($($arg: $ty),*) $(-> $ret)?;
            }
            unsafe { import($($arg),*) }
        }
    )*};
}

#[link(wasm_import_module = "oxide")]
extern "C" {
    #[link_name = "api_log"]
    fn _api_log(ptr: u32, len: u32);

    #[link_name = "api_warn"]
    fn _api_warn(ptr: u32, len: u32);

    #[link_name = "api_error"]
    fn _api_error(ptr: u32, len: u32);

    #[link_name = "api_get_location"]
    fn _api_get_location(out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_upload_file"]
    fn _api_upload_file(name_ptr: u32, name_cap: u32, data_ptr: u32, data_cap: u32) -> u64;

    #[link_name = "api_file_pick"]
    fn _api_file_pick(
        title_ptr: u32,
        title_len: u32,
        filters_ptr: u32,
        filters_len: u32,
        multiple: u32,
        out_ptr: u32,
        out_cap: u32,
    ) -> i32;

    #[link_name = "api_folder_pick"]
    fn _api_folder_pick(title_ptr: u32, title_len: u32) -> u32;

    #[link_name = "api_folder_entries"]
    fn _api_folder_entries(handle: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_file_read"]
    fn _api_file_read(handle: u32, out_ptr: u32, out_cap: u32) -> i64;

    #[link_name = "api_file_read_range"]
    fn _api_file_read_range(
        handle: u32,
        offset_lo: u32,
        offset_hi: u32,
        len: u32,
        out_ptr: u32,
        out_cap: u32,
    ) -> i64;

    #[link_name = "api_file_metadata"]
    fn _api_file_metadata(handle: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_canvas_clear"]
    fn _api_canvas_clear(r: u32, g: u32, b: u32, a: u32);

    #[link_name = "api_canvas_rect"]
    fn _api_canvas_rect(x: f32, y: f32, w: f32, h: f32, r: u32, g: u32, b: u32, a: u32);

    #[link_name = "api_canvas_circle"]
    fn _api_canvas_circle(cx: f32, cy: f32, radius: f32, r: u32, g: u32, b: u32, a: u32);

    #[link_name = "api_canvas_text"]
    fn _api_canvas_text(
        x: f32,
        y: f32,
        size: f32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
        ptr: u32,
        len: u32,
    );

    #[link_name = "api_canvas_text_ex"]
    fn _api_canvas_text_ex(
        x: f32,
        y: f32,
        size: f32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
        family_ptr: u32,
        family_len: u32,
        weight: u32,
        style: u32,
        align: u32,
        text_ptr: u32,
        text_len: u32,
    );

    #[link_name = "api_canvas_measure_text"]
    fn _api_canvas_measure_text(
        size: f32,
        family_ptr: u32,
        family_len: u32,
        weight: u32,
        style: u32,
        text_ptr: u32,
        text_len: u32,
        out_ptr: u32,
    ) -> u32;

    #[link_name = "api_canvas_line"]
    fn _api_canvas_line(
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
        thickness: f32,
    );

    #[link_name = "api_canvas_dimensions"]
    fn _api_canvas_dimensions() -> u64;

    #[link_name = "api_get_scroll_position"]
    fn _api_get_scroll_position() -> u64;

    #[link_name = "api_canvas_image"]
    fn _api_canvas_image(x: f32, y: f32, w: f32, h: f32, data_ptr: u32, data_len: u32);

    // ── Extended Shape Primitives ──────────────────────────────────

    #[link_name = "api_canvas_rounded_rect"]
    fn _api_canvas_rounded_rect(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
    );

    #[link_name = "api_canvas_arc"]
    fn _api_canvas_arc(
        cx: f32,
        cy: f32,
        radius: f32,
        start_angle: f32,
        end_angle: f32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
        thickness: f32,
    );

    #[link_name = "api_canvas_bezier"]
    fn _api_canvas_bezier(
        x1: f32,
        y1: f32,
        cp1x: f32,
        cp1y: f32,
        cp2x: f32,
        cp2y: f32,
        x2: f32,
        y2: f32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
        thickness: f32,
    );

    #[link_name = "api_canvas_gradient"]
    fn _api_canvas_gradient(
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        kind: u32,
        ax: f32,
        ay: f32,
        bx: f32,
        by: f32,
        stops_ptr: u32,
        stops_len: u32,
    );

    #[link_name = "api_storage_set"]
    fn _api_storage_set(key_ptr: u32, key_len: u32, val_ptr: u32, val_len: u32);

    #[link_name = "api_storage_get"]
    fn _api_storage_get(key_ptr: u32, key_len: u32, out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_storage_remove"]
    fn _api_storage_remove(key_ptr: u32, key_len: u32);

    #[link_name = "api_clipboard_write"]
    fn _api_clipboard_write(ptr: u32, len: u32);

    #[link_name = "api_clipboard_read"]
    fn _api_clipboard_read(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_on_event"]
    fn _api_on_event(type_ptr: u32, type_len: u32, callback_id: u32) -> u32;

    #[link_name = "api_off_event"]
    fn _api_off_event(listener_id: u32) -> u32;

    #[link_name = "api_emit_event"]
    fn _api_emit_event(type_ptr: u32, type_len: u32, data_ptr: u32, data_len: u32);

    #[link_name = "api_event_type_len"]
    fn _api_event_type_len() -> u32;

    #[link_name = "api_event_type_read"]
    fn _api_event_type_read(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_event_data_len"]
    fn _api_event_data_len() -> u32;

    #[link_name = "api_event_data_read"]
    fn _api_event_data_read(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_notify"]
    fn _api_notify(title_ptr: u32, title_len: u32, body_ptr: u32, body_len: u32);

    #[link_name = "api_fetch"]
    fn _api_fetch(
        method_ptr: u32,
        method_len: u32,
        url_ptr: u32,
        url_len: u32,
        ct_ptr: u32,
        ct_len: u32,
        body_ptr: u32,
        body_len: u32,
        out_ptr: u32,
        out_cap: u32,
    ) -> i64;

    #[link_name = "api_fetch_begin"]
    fn _api_fetch_begin(
        method_ptr: u32,
        method_len: u32,
        url_ptr: u32,
        url_len: u32,
        ct_ptr: u32,
        ct_len: u32,
        body_ptr: u32,
        body_len: u32,
    ) -> u32;

    #[link_name = "api_fetch_recv"]
    fn _api_fetch_recv(id: u32, out_ptr: u32, out_cap: u32) -> i64;

    #[link_name = "api_fetch_error"]
    fn _api_fetch_error(id: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_fetch_abort"]
    fn _api_fetch_abort(id: u32) -> i32;

    #[link_name = "api_load_module"]
    fn _api_load_module(url_ptr: u32, url_len: u32) -> i32;

    #[link_name = "api_hash_sha256"]
    fn _api_hash_sha256(data_ptr: u32, data_len: u32, out_ptr: u32) -> u32;

    #[link_name = "api_hash_sha512"]
    fn _api_hash_sha512(data_ptr: u32, data_len: u32, out_ptr: u32) -> u32;

    #[link_name = "api_hmac_sha256"]
    fn _api_hmac_sha256(
        key_ptr: u32,
        key_len: u32,
        data_ptr: u32,
        data_len: u32,
        out_ptr: u32,
    ) -> u32;

    #[link_name = "api_random_bytes"]
    fn _api_random_bytes(out_ptr: u32, len: u32) -> u32;

    #[link_name = "api_uuid_v4"]
    fn _api_uuid_v4(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_compress"]
    fn _api_compress(format: u32, data_ptr: u32, data_len: u32, out_ptr: u32, out_cap: u32) -> i64;

    #[link_name = "api_decompress"]
    fn _api_decompress(
        format: u32,
        data_ptr: u32,
        data_len: u32,
        out_ptr: u32,
        out_cap: u32,
    ) -> i64;

    #[link_name = "api_system_locale"]
    fn _api_system_locale(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_system_timezone"]
    fn _api_system_timezone(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_base64_encode"]
    fn _api_base64_encode(data_ptr: u32, data_len: u32, out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_base64_decode"]
    fn _api_base64_decode(data_ptr: u32, data_len: u32, out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_kv_store_set"]
    fn _api_kv_store_set(key_ptr: u32, key_len: u32, val_ptr: u32, val_len: u32) -> i32;

    #[link_name = "api_kv_store_get"]
    fn _api_kv_store_get(key_ptr: u32, key_len: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_kv_store_delete"]
    fn _api_kv_store_delete(key_ptr: u32, key_len: u32) -> i32;

    // ── Navigation ──────────────────────────────────────────────────

    #[link_name = "api_navigate"]
    fn _api_navigate(url_ptr: u32, url_len: u32) -> i32;

    #[link_name = "api_push_state"]
    fn _api_push_state(
        state_ptr: u32,
        state_len: u32,
        title_ptr: u32,
        title_len: u32,
        url_ptr: u32,
        url_len: u32,
    );

    #[link_name = "api_replace_state"]
    fn _api_replace_state(
        state_ptr: u32,
        state_len: u32,
        title_ptr: u32,
        title_len: u32,
        url_ptr: u32,
        url_len: u32,
    );

    #[link_name = "api_get_url"]
    fn _api_get_url(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_get_state"]
    fn _api_get_state(out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_history_back"]
    fn _api_history_back() -> i32;

    #[link_name = "api_history_forward"]
    fn _api_history_forward() -> i32;

    // ── Hyperlinks ──────────────────────────────────────────────────

    #[link_name = "api_register_hyperlink"]
    fn _api_register_hyperlink(x: f32, y: f32, w: f32, h: f32, url_ptr: u32, url_len: u32) -> i32;

    // ── Input Polling ────────────────────────────────────────────────

    #[link_name = "api_mouse_position"]
    fn _api_mouse_position() -> u64;

    #[link_name = "api_mouse_button_down"]
    fn _api_mouse_button_down(button: u32) -> u32;

    #[link_name = "api_mouse_button_clicked"]
    fn _api_mouse_button_clicked(button: u32) -> u32;

    #[link_name = "api_key_down"]
    fn _api_key_down(key: u32) -> u32;

    #[link_name = "api_key_pressed"]
    fn _api_key_pressed(key: u32) -> u32;

    #[link_name = "api_scroll_delta"]
    fn _api_scroll_delta() -> u64;

    #[link_name = "api_text_input"]
    fn _api_text_input(out_ptr: u32, out_cap: u32) -> u32;

    // ── Audio Playback ──────────────────────────────────────────────

    #[link_name = "api_audio_play"]
    fn _api_audio_play(data_ptr: u32, data_len: u32) -> i32;

    #[link_name = "api_audio_play_url"]
    fn _api_audio_play_url(url_ptr: u32, url_len: u32) -> i32;

    #[link_name = "api_audio_detect_format"]
    fn _api_audio_detect_format(data_ptr: u32, data_len: u32) -> u32;

    #[link_name = "api_audio_play_with_format"]
    fn _api_audio_play_with_format(data_ptr: u32, data_len: u32, format_hint: u32) -> i32;

    #[link_name = "api_audio_last_url_content_type"]
    fn _api_audio_last_url_content_type(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_audio_is_playing"]
    fn _api_audio_is_playing() -> u32;

    #[link_name = "api_audio_set_loop"]
    fn _api_audio_set_loop(enabled: u32);

    #[link_name = "api_audio_channel_play"]
    fn _api_audio_channel_play(channel: u32, data_ptr: u32, data_len: u32) -> i32;

    #[link_name = "api_audio_channel_play_with_format"]
    fn _api_audio_channel_play_with_format(
        channel: u32,
        data_ptr: u32,
        data_len: u32,
        format_hint: u32,
    ) -> i32;

    // ── Video ─────────────────────────────────────────────────────────

    #[link_name = "api_video_detect_format"]
    fn _api_video_detect_format(data_ptr: u32, data_len: u32) -> u32;

    #[link_name = "api_video_load"]
    fn _api_video_load(data_ptr: u32, data_len: u32, format_hint: u32) -> i32;

    #[link_name = "api_video_load_url"]
    fn _api_video_load_url(url_ptr: u32, url_len: u32) -> i32;

    #[link_name = "api_video_last_url_content_type"]
    fn _api_video_last_url_content_type(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_video_hls_variant_url"]
    fn _api_video_hls_variant_url(index: u32, out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_video_set_loop"]
    fn _api_video_set_loop(enabled: u32);

    #[link_name = "api_subtitle_load_srt"]
    fn _api_subtitle_load_srt(ptr: u32, len: u32) -> i32;

    #[link_name = "api_subtitle_load_vtt"]
    fn _api_subtitle_load_vtt(ptr: u32, len: u32) -> i32;

    // ── Media capture ─────────────────────────────────────────────────

    #[link_name = "api_camera_capture_frame"]
    fn _api_camera_capture_frame(out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_camera_frame_dimensions"]
    fn _api_camera_frame_dimensions() -> u64;

    #[link_name = "api_microphone_read_samples"]
    fn _api_microphone_read_samples(out_ptr: u32, max_samples: u32) -> u32;

    #[link_name = "api_screen_capture"]
    fn _api_screen_capture(out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_screen_capture_dimensions"]
    fn _api_screen_capture_dimensions() -> u64;

    #[link_name = "api_media_pipeline_stats"]
    fn _api_media_pipeline_stats() -> u64;

    // ── GPU / WebGPU-style API ────────────────────────────────────

    #[link_name = "api_gpu_create_buffer"]
    fn _api_gpu_create_buffer(size_lo: u32, size_hi: u32, usage: u32) -> u32;

    #[link_name = "api_gpu_create_shader"]
    fn _api_gpu_create_shader(src_ptr: u32, src_len: u32) -> u32;

    #[link_name = "api_gpu_create_render_pipeline"]
    fn _api_gpu_create_render_pipeline(
        shader: u32,
        vs_ptr: u32,
        vs_len: u32,
        fs_ptr: u32,
        fs_len: u32,
    ) -> u32;

    #[link_name = "api_gpu_create_compute_pipeline"]
    fn _api_gpu_create_compute_pipeline(shader: u32, ep_ptr: u32, ep_len: u32) -> u32;

    #[link_name = "api_gpu_write_buffer"]
    fn _api_gpu_write_buffer(
        handle: u32,
        offset_lo: u32,
        offset_hi: u32,
        data_ptr: u32,
        data_len: u32,
    ) -> u32;

    #[link_name = "api_gpu_draw"]
    fn _api_gpu_draw(pipeline: u32, target: u32, vertex_count: u32, instance_count: u32) -> u32;

    #[link_name = "api_gpu_dispatch_compute"]
    fn _api_gpu_dispatch_compute(pipeline: u32, x: u32, y: u32, z: u32) -> u32;

    #[link_name = "api_gpu_destroy_buffer"]
    fn _api_gpu_destroy_buffer(handle: u32) -> u32;

    #[link_name = "api_gpu_destroy_texture"]
    fn _api_gpu_destroy_texture(handle: u32) -> u32;

    // ── WebRTC / Real-Time Communication ─────────────────────────

    #[link_name = "api_rtc_create_peer"]
    fn _api_rtc_create_peer(stun_ptr: u32, stun_len: u32) -> u32;

    #[link_name = "api_rtc_close_peer"]
    fn _api_rtc_close_peer(peer_id: u32) -> u32;

    #[link_name = "api_rtc_create_offer"]
    fn _api_rtc_create_offer(peer_id: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_rtc_create_answer"]
    fn _api_rtc_create_answer(peer_id: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_rtc_set_local_description"]
    fn _api_rtc_set_local_description(
        peer_id: u32,
        sdp_ptr: u32,
        sdp_len: u32,
        is_offer: u32,
    ) -> i32;

    #[link_name = "api_rtc_set_remote_description"]
    fn _api_rtc_set_remote_description(
        peer_id: u32,
        sdp_ptr: u32,
        sdp_len: u32,
        is_offer: u32,
    ) -> i32;

    #[link_name = "api_rtc_add_ice_candidate"]
    fn _api_rtc_add_ice_candidate(peer_id: u32, cand_ptr: u32, cand_len: u32) -> i32;

    #[link_name = "api_rtc_poll_ice_candidate"]
    fn _api_rtc_poll_ice_candidate(peer_id: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_rtc_create_data_channel"]
    fn _api_rtc_create_data_channel(
        peer_id: u32,
        label_ptr: u32,
        label_len: u32,
        ordered: u32,
    ) -> u32;

    #[link_name = "api_rtc_send"]
    fn _api_rtc_send(
        peer_id: u32,
        channel_id: u32,
        data_ptr: u32,
        data_len: u32,
        is_binary: u32,
    ) -> i32;

    #[link_name = "api_rtc_recv"]
    fn _api_rtc_recv(peer_id: u32, channel_id: u32, out_ptr: u32, out_cap: u32) -> i64;

    #[link_name = "api_rtc_poll_data_channel"]
    fn _api_rtc_poll_data_channel(peer_id: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_rtc_poll_track"]
    fn _api_rtc_poll_track(peer_id: u32, out_ptr: u32, out_cap: u32) -> i32;

    #[link_name = "api_rtc_signal_connect"]
    fn _api_rtc_signal_connect(url_ptr: u32, url_len: u32) -> u32;

    #[link_name = "api_rtc_signal_join_room"]
    fn _api_rtc_signal_join_room(room_ptr: u32, room_len: u32) -> i32;

    #[link_name = "api_rtc_signal_send"]
    fn _api_rtc_signal_send(data_ptr: u32, data_len: u32) -> i32;

    #[link_name = "api_rtc_signal_recv"]
    fn _api_rtc_signal_recv(out_ptr: u32, out_cap: u32) -> i32;

    // ── WebSocket API ────────────────────────────────────────────────

    #[link_name = "api_ws_connect"]
    fn _api_ws_connect(url_ptr: u32, url_len: u32) -> u32;

    #[link_name = "api_ws_send_text"]
    fn _api_ws_send_text(id: u32, data_ptr: u32, data_len: u32) -> i32;

    #[link_name = "api_ws_send_binary"]
    fn _api_ws_send_binary(id: u32, data_ptr: u32, data_len: u32) -> i32;

    #[link_name = "api_ws_recv"]
    fn _api_ws_recv(id: u32, out_ptr: u32, out_cap: u32) -> i64;

    // ── Server-Sent Events API ──────────────────────────────────────

    #[link_name = "api_sse_open"]
    fn _api_sse_open(url_ptr: u32, url_len: u32) -> u32;

    #[link_name = "api_sse_recv"]
    fn _api_sse_recv(id: u32, out_ptr: u32, out_cap: u32) -> i64;

    #[link_name = "api_sse_error"]
    fn _api_sse_error(id: u32, out_ptr: u32, out_cap: u32) -> i32;

    // ── Background Workers API ──────────────────────────────────────

    #[link_name = "api_spawn_worker"]
    fn _api_spawn_worker(url_ptr: u32, url_len: u32) -> i32;

    #[link_name = "api_worker_post_message"]
    fn _api_worker_post_message(handle: u32, ptr: u32, len: u32) -> i32;

    #[link_name = "api_worker_recv"]
    fn _api_worker_recv(handle: u32, out_ptr: u32, out_cap: u32) -> i64;

    #[link_name = "api_worker_post"]
    fn _api_worker_post(ptr: u32, len: u32) -> i32;

    #[link_name = "api_worker_message_read"]
    fn _api_worker_message_read(out_ptr: u32, out_cap: u32) -> u32;

    // ── MIDI API ────────────────────────────────────────────────────

    #[link_name = "api_midi_input_name"]
    fn _api_midi_input_name(index: u32, out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_midi_output_name"]
    fn _api_midi_output_name(index: u32, out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_midi_send"]
    fn _api_midi_send(handle: u32, data_ptr: u32, data_len: u32) -> i32;

    #[link_name = "api_midi_recv"]
    fn _api_midi_recv(handle: u32, out_ptr: u32, out_cap: u32) -> i32;

    // ── URL Utilities ───────────────────────────────────────────────

    #[link_name = "api_url_resolve"]
    fn _api_url_resolve(
        base_ptr: u32,
        base_len: u32,
        rel_ptr: u32,
        rel_len: u32,
        out_ptr: u32,
        out_cap: u32,
    ) -> i32;

    #[link_name = "api_url_encode"]
    fn _api_url_encode(input_ptr: u32, input_len: u32, out_ptr: u32, out_cap: u32) -> u32;

    #[link_name = "api_url_decode"]
    fn _api_url_decode(input_ptr: u32, input_len: u32, out_ptr: u32, out_cap: u32) -> u32;

    // ── Download & Print-to-PDF ──────────────────────────────────────

    #[link_name = "api_download_data"]
    fn _api_download_data(
        data_ptr: u32,
        data_len: u32,
        filename_ptr: u32,
        filename_len: u32,
    ) -> i32;

    #[link_name = "api_download_url"]
    fn _api_download_url(url_ptr: u32, url_len: u32) -> i32;

    #[link_name = "api_canvas_print_pdf"]
    fn _api_canvas_print_pdf(filename_ptr: u32, filename_len: u32) -> i32;
}

// ─── Boundary helpers ───────────────────────────────────────────────────────

/// Give the host `cap` bytes to write into. `call(ptr, cap)` returns how many
/// bytes it wrote; a negative status, or a size larger than `cap` (the host
/// needs more room and wrote nothing), comes back as the error.
///
/// The buffer is not zeroed first: the host writes every byte it reports, so
/// a poll that finds nothing costs no memset of the whole buffer.
fn host_bytes(cap: usize, call: impl FnOnce(u32, u32) -> i64) -> Result<Vec<u8>, i64> {
    let mut buf = Vec::with_capacity(cap);
    let n = call(buf.as_mut_ptr() as u32, cap as u32);
    match usize::try_from(n) {
        Ok(len) if len <= cap => {
            // SAFETY: the host initialised the first `len` bytes of the buffer.
            unsafe { buf.set_len(len) };
            buf.shrink_to_fit();
            Ok(buf)
        }
        _ => Err(n),
    }
}

/// [`host_bytes`] for calls that answer a too-small buffer with the size they
/// need (as `need` or `-need`): retries once with exactly that much room if it
/// is at most `max`.
fn host_bytes_resizing(
    cap: usize,
    max: usize,
    call: impl Fn(u32, u32) -> i64,
) -> Result<Vec<u8>, i64> {
    host_bytes(cap, &call).or_else(|status| {
        let need = usize::try_from(status.unsigned_abs()).unwrap_or(usize::MAX);
        if need > cap && need <= max {
            host_bytes(need, &call)
        } else {
            Err(status)
        }
    })
}

/// A UTF-8 string from `call(ptr, cap)`, which writes up to `cap` bytes and
/// returns how many.
fn host_string(cap: usize, call: impl FnOnce(u32, u32) -> u32) -> String {
    into_string(host_bytes(cap, |ptr, cap| call(ptr, cap).into()).unwrap_or_default())
}

/// Host bytes as text, replacing invalid UTF-8 (valid text is not copied).
fn into_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

/// Split a host return value packed as `(hi << 32) | lo`.
fn unpack_u32(packed: u64) -> (u32, u32) {
    ((packed >> 32) as u32, packed as u32)
}

/// Like [`unpack_u32`], for two `f32` bit patterns.
fn unpack_f32(packed: u64) -> (f32, f32) {
    let (hi, lo) = unpack_u32(packed);
    (f32::from_bits(hi), f32::from_bits(lo))
}

/// For [`host_bytes`]: a host return packed as `(meta << 32) | len` becomes
/// `len`, with `meta` stored; a negative status passes through.
fn split_len(packed: i64, meta: &mut u32) -> i64 {
    if packed < 0 {
        return packed;
    }
    let (hi, len) = unpack_u32(packed as u64);
    *meta = hi;
    len.into()
}

// ─── Console API ────────────────────────────────────────────────────────────

/// Print a message to the browser console (log level).
pub fn log(msg: &str) {
    unsafe { _api_log(msg.as_ptr() as u32, msg.len() as u32) }
}

/// Print a warning to the browser console.
pub fn warn(msg: &str) {
    unsafe { _api_warn(msg.as_ptr() as u32, msg.len() as u32) }
}

/// Print an error to the browser console.
pub fn error(msg: &str) {
    unsafe { _api_error(msg.as_ptr() as u32, msg.len() as u32) }
}

// ─── Geolocation API ────────────────────────────────────────────────────────

/// Get the device's geolocation as a `"lat,lon"` string (currently a mock location).
///
/// Gated by an in-browser permission prompt on first use per origin. Errors:
/// [`PERMISSION_PENDING`] while the prompt is showing (retry on a later frame), `-1` once
/// blocked — by the user or by an app manifest that doesn't declare `geolocation`.
pub fn get_location() -> Result<String, i32> {
    host_bytes(128, |ptr, cap| {
        unsafe { _api_get_location(ptr, cap) }.into()
    })
    .map(into_string)
    .map_err(|e| e as i32)
}

// ─── File Upload API ────────────────────────────────────────────────────────

/// File returned from the native file picker.
pub struct UploadedFile {
    pub name: String,
    pub data: Vec<u8>,
}

/// Opens the native OS file picker and returns the selected file.
/// Returns `None` if the user cancels.
pub fn upload_file() -> Option<UploadedFile> {
    let mut name = [0u8; 256];
    let mut data = vec![0u8; 1024 * 1024]; // 1MB max
    let result = unsafe {
        _api_upload_file(
            name.as_mut_ptr() as u32,
            name.len() as u32,
            data.as_mut_ptr() as u32,
            data.len() as u32,
        )
    };
    if result == 0 {
        return None;
    }
    let (name_len, data_len) = unpack_u32(result);
    Some(UploadedFile {
        name: String::from_utf8_lossy(&name[..name_len as usize]).into_owned(),
        data: data[..data_len as usize].to_vec(),
    })
}

// ─── File / Folder Picker API ───────────────────────────────────────────────
//
// Handle-based picker. Paths never cross the sandbox boundary — the host
// keeps a `HashMap<handle, PathBuf>` and returns opaque `u32` handles.
// Use [`file_read`] / [`file_read_range`] / [`file_metadata`] with the
// handle; [`folder_entries`] lists a picked directory.

/// Metadata returned by [`file_metadata`], parsed from the host's JSON reply.
pub struct FileMetadata {
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub modified_ms: u64,
    pub is_dir: bool,
}

/// One child returned by [`folder_entries`].
pub struct FolderEntry {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub handle: u32,
}

/// Open the native file picker and return the selected file handles.
///
/// `filters` is a comma-separated list of extensions (e.g. `"png,jpg,gif"`);
/// pass `""` to allow any file. Set `multiple = true` for multi-select.
/// Returns an empty `Vec` if the user cancels.
pub fn file_pick(title: &str, filters: &str, multiple: bool) -> Vec<u32> {
    let mut buf = [0u32; 64];
    let n = unsafe {
        _api_file_pick(
            title.as_ptr() as u32,
            title.len() as u32,
            filters.as_ptr() as u32,
            filters.len() as u32,
            u32::from(multiple),
            buf.as_mut_ptr() as u32,
            (buf.len() * 4) as u32,
        )
    };
    buf[..n.clamp(0, 64) as usize].to_vec()
}

/// Open the native folder picker and return a directory handle.
///
/// Returns `None` if the user cancels. Use [`folder_entries`] to list the
/// selected directory.
pub fn folder_pick(title: &str) -> Option<u32> {
    let handle = unsafe { _api_folder_pick(title.as_ptr() as u32, title.len() as u32) };
    (handle != 0).then_some(handle)
}

/// List the children of a picked folder handle.
///
/// Each returned entry includes a fresh sub-handle that can be passed to
/// [`file_read`], [`file_read_range`], or [`file_metadata`] (or recursively
/// to `folder_entries` for directories).
pub fn folder_entries(handle: u32) -> Vec<FolderEntry> {
    host_bytes_resizing(8 * 1024, usize::MAX, |ptr, cap| {
        unsafe { _api_folder_entries(handle, ptr, cap) }.into()
    })
    .map(|json| parse_folder_entries(&into_string(json)))
    .unwrap_or_default()
}

fn parse_folder_entries(json: &str) -> Vec<FolderEntry> {
    let mut fields = JsonCursor(json);
    let mut entries = Vec::new();
    while let Some(name) = fields.string("name") {
        entries.push(FolderEntry {
            name,
            size: fields.number("size").unwrap_or(0),
            is_dir: fields.bool("is_dir").unwrap_or(false),
            handle: fields.number("handle").unwrap_or(0) as u32,
        });
    }
    entries
}

/// Reads the host's flat JSON replies, whose fields come in a fixed order. Each
/// lookup continues after the previous value, so keys or braces inside strings
/// can't be mistaken for structure. Avoids pulling in serde_json.
struct JsonCursor<'a>(&'a str);

impl JsonCursor<'_> {
    /// Move past the next `"key":`.
    fn seek(&mut self, key: &str) -> Option<()> {
        let pattern = format!("\"{key}\":");
        let at = self.0.find(&pattern)?;
        self.0 = self.0[at + pattern.len()..].trim_start();
        Some(())
    }

    fn string(&mut self, key: &str) -> Option<String> {
        self.seek(key)?;
        let mut chars = self.0.strip_prefix('"')?.chars();
        let mut out = String::new();
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    c => out.push(c),
                },
                c => out.push(c),
            }
        }
        self.0 = chars.as_str();
        Some(out)
    }

    fn number(&mut self, key: &str) -> Option<u64> {
        self.seek(key)?;
        let end = self
            .0
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(self.0.len());
        let (digits, rest) = self.0.split_at(end);
        self.0 = rest;
        digits.parse().ok()
    }

    fn bool(&mut self, key: &str) -> Option<bool> {
        self.seek(key)?;
        if self.0.starts_with("true") {
            Some(true)
        } else if self.0.starts_with("false") {
            Some(false)
        } else {
            None
        }
    }
}

/// Read the full contents of a picked file.
///
/// Returns `None` if the handle is unknown, the file cannot be read, or the
/// file is larger than 64 MiB (the wrapper's retry cap).
pub fn file_read(handle: u32) -> Option<Vec<u8>> {
    host_bytes_resizing(64 * 1024, 64 * 1024 * 1024, |ptr, cap| unsafe {
        _api_file_read(handle, ptr, cap)
    })
    .ok()
}

/// Read `len` bytes from `offset` of a picked file.
///
/// Returns the bytes actually read (may be shorter than `len` at EOF).
/// `None` indicates an invalid handle or I/O error.
pub fn file_read_range(handle: u32, offset: u64, len: u32) -> Option<Vec<u8>> {
    let (offset_hi, offset_lo) = unpack_u32(offset);
    host_bytes(len as usize, |ptr, cap| unsafe {
        _api_file_read_range(handle, offset_lo, offset_hi, len, ptr, cap)
    })
    .ok()
}

/// Inspect a picked file or folder: name, size, MIME type, last-modified.
pub fn file_metadata(handle: u32) -> Option<FileMetadata> {
    let json = host_bytes_resizing(8 * 1024, usize::MAX, |ptr, cap| {
        unsafe { _api_file_metadata(handle, ptr, cap) }.into()
    })
    .ok()?;
    let json = into_string(json);
    let mut fields = JsonCursor(&json);
    Some(FileMetadata {
        name: fields.string("name").unwrap_or_default(),
        size: fields.number("size").unwrap_or(0),
        mime: fields.string("mime").unwrap_or_default(),
        modified_ms: fields.number("modified_ms").unwrap_or(0),
        is_dir: fields.bool("is_dir").unwrap_or(false),
    })
}

// ─── Canvas API ─────────────────────────────────────────────────────────────

/// Clear the canvas with a solid RGBA color.
pub fn canvas_clear(r: u8, g: u8, b: u8, a: u8) {
    unsafe { _api_canvas_clear(r as u32, g as u32, b as u32, a as u32) }
}

/// Draw a filled rectangle.
pub fn canvas_rect(x: f32, y: f32, w: f32, h: f32, r: u8, g: u8, b: u8, a: u8) {
    unsafe { _api_canvas_rect(x, y, w, h, r as u32, g as u32, b as u32, a as u32) }
}

/// Draw a filled circle.
pub fn canvas_circle(cx: f32, cy: f32, radius: f32, r: u8, g: u8, b: u8, a: u8) {
    unsafe { _api_canvas_circle(cx, cy, radius, r as u32, g as u32, b as u32, a as u32) }
}

/// Draw text on the canvas with RGBA color.
pub fn canvas_text(x: f32, y: f32, size: f32, r: u8, g: u8, b: u8, a: u8, text: &str) {
    unsafe {
        _api_canvas_text(
            x,
            y,
            size,
            r as u32,
            g as u32,
            b as u32,
            a as u32,
            text.as_ptr() as u32,
            text.len() as u32,
        )
    }
}

/// Normal (upright) font style. Pass to [`canvas_text_ex`] / [`canvas_measure_text`].
pub const FONT_STYLE_NORMAL: u32 = 0;
/// Italic font style.
pub const FONT_STYLE_ITALIC: u32 = 1;
/// Oblique font style (slanted upright; falls back to italic where oblique isn't available).
pub const FONT_STYLE_OBLIQUE: u32 = 2;

/// Text is anchored at its left edge (baseline `(x, y)`).
pub const TEXT_ALIGN_LEFT: u32 = 0;
/// Text is horizontally centred around `x`.
pub const TEXT_ALIGN_CENTER: u32 = 1;
/// Text is anchored at its right edge (`x` is the right edge).
pub const TEXT_ALIGN_RIGHT: u32 = 2;

/// Shaped-line metrics returned by [`canvas_measure_text`]. All values are in pixels.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextMetrics {
    /// Advance width of the shaped line.
    pub width: f32,
    /// Distance from baseline to the top of the tallest glyph (positive).
    pub ascent: f32,
    /// Distance from baseline to the bottom of the lowest glyph (positive).
    pub descent: f32,
}

/// Draw text with explicit family, weight (CSS `100..=900`; `0` = default 400),
/// style ([`FONT_STYLE_NORMAL`] / [`FONT_STYLE_ITALIC`] / [`FONT_STYLE_OBLIQUE`]),
/// and horizontal alignment ([`TEXT_ALIGN_LEFT`] / [`TEXT_ALIGN_CENTER`] /
/// [`TEXT_ALIGN_RIGHT`]).
///
/// Pass an empty `family` to use the system UI font. For `TEXT_ALIGN_CENTER`
/// and `TEXT_ALIGN_RIGHT`, `x` is the centre and the right edge of the line
/// respectively.
pub fn canvas_text_ex(
    x: f32,
    y: f32,
    size: f32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    family: &str,
    weight: u32,
    style: u32,
    align: u32,
    text: &str,
) {
    unsafe {
        _api_canvas_text_ex(
            x,
            y,
            size,
            r as u32,
            g as u32,
            b as u32,
            a as u32,
            family.as_ptr() as u32,
            family.len() as u32,
            weight,
            style,
            align,
            text.as_ptr() as u32,
            text.len() as u32,
        )
    }
}

/// Measure a line of text shaped with the given font parameters. Returns the
/// shaped advance width plus ascent/descent in pixels. Pass an empty `family`
/// to use the system UI font; pass `0` for `weight` to use the default (400).
///
/// Returns zeroes if measurement isn't available (e.g. called outside
/// `on_frame` or before the host text system is ready).
pub fn canvas_measure_text(
    size: f32,
    family: &str,
    weight: u32,
    style: u32,
    text: &str,
) -> TextMetrics {
    // The host writes three little-endian `f32`s: width, ascent, descent.
    let mut out = [0f32; 3];
    let ok = unsafe {
        _api_canvas_measure_text(
            size,
            family.as_ptr() as u32,
            family.len() as u32,
            weight,
            style,
            text.as_ptr() as u32,
            text.len() as u32,
            out.as_mut_ptr() as u32,
        )
    };
    if ok == 0 {
        return TextMetrics::default();
    }
    let [width, ascent, descent] = out;
    TextMetrics {
        width,
        ascent,
        descent,
    }
}

/// Draw a line between two points with RGBA color.
pub fn canvas_line(x1: f32, y1: f32, x2: f32, y2: f32, r: u8, g: u8, b: u8, a: u8, thickness: f32) {
    unsafe {
        _api_canvas_line(
            x1, y1, x2, y2, r as u32, g as u32, b as u32, a as u32, thickness,
        )
    }
}

/// Returns `(width, height)` of the canvas in pixels.
pub fn canvas_dimensions() -> (u32, u32) {
    unpack_u32(unsafe { _api_canvas_dimensions() })
}

host_fns! {
    /// Set the virtual size of the canvas content. If the content is larger than
    /// the screen/viewport dimensions, the browser host will automatically render
    /// interactive overlay scrollbars and track absolute scroll coordinates.
    pub fn set_content_size(width: u32, height: u32) = "api_set_content_size";

    /// Programmatically set the absolute scroll position `(x, y)` in pixels.
    pub fn set_scroll_position(x: f32, y: f32) = "api_set_scroll_position";
}

/// Returns the current absolute `(scroll_x, scroll_y)` coordinates in pixels.
/// Guest applications should query these coordinates each frame and translate
/// their drawing elements accordingly to support scrolling.
pub fn scroll_position() -> (f32, f32) {
    unpack_f32(unsafe { _api_get_scroll_position() })
}

/// Draw an image on the canvas from encoded image bytes (PNG, JPEG, GIF, WebP).
/// The browser decodes the image and renders it at the given rectangle.
pub fn canvas_image(x: f32, y: f32, w: f32, h: f32, data: &[u8]) {
    unsafe { _api_canvas_image(x, y, w, h, data.as_ptr() as u32, data.len() as u32) }
}

// ─── Extended Shape Primitives ──────────────────────────────────────────────

/// Draw a filled rounded rectangle with uniform corner radius.
pub fn canvas_rounded_rect(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    radius: f32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) {
    unsafe { _api_canvas_rounded_rect(x, y, w, h, radius, r as u32, g as u32, b as u32, a as u32) }
}

/// Draw a circular arc stroke from `start_angle` to `end_angle` (in radians, clockwise from +X).
pub fn canvas_arc(
    cx: f32,
    cy: f32,
    radius: f32,
    start_angle: f32,
    end_angle: f32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    thickness: f32,
) {
    unsafe {
        _api_canvas_arc(
            cx,
            cy,
            radius,
            start_angle,
            end_angle,
            r as u32,
            g as u32,
            b as u32,
            a as u32,
            thickness,
        )
    }
}

/// Draw a cubic Bézier curve stroke from `(x1,y1)` to `(x2,y2)` with two control points.
pub fn canvas_bezier(
    x1: f32,
    y1: f32,
    cp1x: f32,
    cp1y: f32,
    cp2x: f32,
    cp2y: f32,
    x2: f32,
    y2: f32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
    thickness: f32,
) {
    unsafe {
        _api_canvas_bezier(
            x1, y1, cp1x, cp1y, cp2x, cp2y, x2, y2, r as u32, g as u32, b as u32, a as u32,
            thickness,
        )
    }
}

/// Gradient type constants.
pub const GRADIENT_LINEAR: u32 = 0;
pub const GRADIENT_RADIAL: u32 = 1;

/// Draw a gradient-filled rectangle.
///
/// `kind`: [`GRADIENT_LINEAR`] or [`GRADIENT_RADIAL`].
/// For linear gradients, `(ax,ay)` and `(bx,by)` define the gradient axis.
/// For radial gradients, `(ax,ay)` is the center and `by` is the radius.
/// `stops` is a slice of `(offset, r, g, b, a)` tuples.
pub fn canvas_gradient(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    kind: u32,
    ax: f32,
    ay: f32,
    bx: f32,
    by: f32,
    stops: &[(f32, u8, u8, u8, u8)],
) {
    gradient(x, y, w, h, kind, [ax, ay, bx, by], stops.iter().copied());
}

/// [`canvas_gradient`] for any source of stops, each sent as 8 bytes:
/// `offset` (`f32`, little-endian), then `r, g, b, a`.
pub(crate) fn gradient(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    kind: u32,
    [ax, ay, bx, by]: [f32; 4],
    stops: impl ExactSizeIterator<Item = (f32, u8, u8, u8, u8)>,
) {
    let mut buf = Vec::with_capacity(stops.len() * 8);
    for (offset, r, g, b, a) in stops {
        buf.extend_from_slice(&offset.to_le_bytes());
        buf.extend_from_slice(&[r, g, b, a]);
    }
    unsafe {
        _api_canvas_gradient(
            x,
            y,
            w,
            h,
            kind,
            ax,
            ay,
            bx,
            by,
            buf.as_ptr() as u32,
            buf.len() as u32,
        )
    }
}

// ─── Canvas State API ───────────────────────────────────────────────────────

host_fns! {
    /// Push the current canvas state (transform, clip, opacity) onto an internal stack.
    /// Use with [`canvas_restore`] to scope transformations and effects.
    pub fn canvas_save() = "api_canvas_save";

    /// Pop and restore the most recently saved canvas state.
    pub fn canvas_restore() = "api_canvas_restore";

    /// Apply a 2D affine transformation to subsequent draw commands.
    ///
    /// The six values represent a column-major 3×2 matrix:
    /// ```text
    /// | a  c  tx |
    /// | b  d  ty |
    /// | 0  0   1 |
    /// ```
    ///
    /// For a simple translation, use `canvas_transform(1.0, 0.0, 0.0, 1.0, tx, ty)`.
    pub fn canvas_transform(a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32)
        = "api_canvas_transform";

    /// Intersect the current clipping region with an axis-aligned rectangle.
    /// Coordinates are in the current (possibly transformed) canvas space.
    pub fn canvas_clip(x: f32, y: f32, w: f32, h: f32) = "api_canvas_clip";

    /// Set the layer opacity for subsequent draw commands (0.0 = transparent, 1.0 = opaque).
    /// Multiplied with any parent opacity set via nested [`canvas_save`]/[`canvas_opacity`].
    pub fn canvas_opacity(alpha: f32) = "api_canvas_opacity";
}

// ─── GPU / WebGPU-style API ─────────────────────────────────────────────────
//
// Not functional: Sighurt's WASM engine keeps these imports so existing apps still load,
// but every call returns 0 (failure) and nothing is drawn.

/// GPU buffer usage flags (matches WebGPU `GPUBufferUsage`).
pub mod gpu_usage {
    pub const VERTEX: u32 = 0x0020;
    pub const INDEX: u32 = 0x0010;
    pub const UNIFORM: u32 = 0x0040;
    pub const STORAGE: u32 = 0x0080;
}

/// Create a GPU buffer of `size` bytes. Returns a handle (0 = failure).
///
/// `usage` is a bitmask of [`gpu_usage`] flags.
pub fn gpu_create_buffer(size: u64, usage: u32) -> u32 {
    unsafe { _api_gpu_create_buffer(size as u32, (size >> 32) as u32, usage) }
}

host_fns! {
    /// Create a 2D RGBA8 texture. Returns a handle (0 = failure).
    pub fn gpu_create_texture(width: u32, height: u32) -> u32 = "api_gpu_create_texture";
}

/// Compile a WGSL shader module. Returns a handle (0 = failure).
pub fn gpu_create_shader(source: &str) -> u32 {
    unsafe { _api_gpu_create_shader(source.as_ptr() as u32, source.len() as u32) }
}

/// Create a render pipeline from a shader. Returns a handle (0 = failure).
///
/// `vertex_entry` and `fragment_entry` are the WGSL function names.
pub fn gpu_create_pipeline(shader: u32, vertex_entry: &str, fragment_entry: &str) -> u32 {
    unsafe {
        _api_gpu_create_render_pipeline(
            shader,
            vertex_entry.as_ptr() as u32,
            vertex_entry.len() as u32,
            fragment_entry.as_ptr() as u32,
            fragment_entry.len() as u32,
        )
    }
}

/// Create a compute pipeline from a shader. Returns a handle (0 = failure).
pub fn gpu_create_compute_pipeline(shader: u32, entry_point: &str) -> u32 {
    unsafe {
        _api_gpu_create_compute_pipeline(
            shader,
            entry_point.as_ptr() as u32,
            entry_point.len() as u32,
        )
    }
}

/// Write data to a GPU buffer at the given byte offset.
pub fn gpu_write_buffer(handle: u32, offset: u64, data: &[u8]) -> bool {
    unsafe {
        _api_gpu_write_buffer(
            handle,
            offset as u32,
            (offset >> 32) as u32,
            data.as_ptr() as u32,
            data.len() as u32,
        ) != 0
    }
}

/// Submit a render pass: draw `vertex_count` vertices with `instance_count` instances.
pub fn gpu_draw(
    pipeline: u32,
    target_texture: u32,
    vertex_count: u32,
    instance_count: u32,
) -> bool {
    unsafe { _api_gpu_draw(pipeline, target_texture, vertex_count, instance_count) != 0 }
}

/// Submit a compute dispatch with the given workgroup counts.
pub fn gpu_dispatch_compute(pipeline: u32, x: u32, y: u32, z: u32) -> bool {
    unsafe { _api_gpu_dispatch_compute(pipeline, x, y, z) != 0 }
}

/// Destroy a GPU buffer.
pub fn gpu_destroy_buffer(handle: u32) -> bool {
    unsafe { _api_gpu_destroy_buffer(handle) != 0 }
}

/// Destroy a GPU texture.
pub fn gpu_destroy_texture(handle: u32) -> bool {
    unsafe { _api_gpu_destroy_texture(handle) != 0 }
}

// ─── Local Storage API ──────────────────────────────────────────────────────

/// Store a key-value pair in sandboxed local storage.
pub fn storage_set(key: &str, value: &str) {
    unsafe {
        _api_storage_set(
            key.as_ptr() as u32,
            key.len() as u32,
            value.as_ptr() as u32,
            value.len() as u32,
        )
    }
}

/// Retrieve a value from local storage. Returns empty string if not found.
pub fn storage_get(key: &str) -> String {
    host_string(4096, |ptr, cap| unsafe {
        _api_storage_get(key.as_ptr() as u32, key.len() as u32, ptr, cap)
    })
}

/// Remove a key from local storage.
pub fn storage_remove(key: &str) {
    unsafe { _api_storage_remove(key.as_ptr() as u32, key.len() as u32) }
}

// ─── Clipboard API ──────────────────────────────────────────────────────────

/// Copy text to the system clipboard. Sighurt's WASM engine always refuses; this does nothing.
pub fn clipboard_write(text: &str) {
    unsafe { _api_clipboard_write(text.as_ptr() as u32, text.len() as u32) }
}

/// Read text from the system clipboard. Sighurt's WASM engine always refuses; returns an empty string.
pub fn clipboard_read() -> String {
    host_string(4096, |ptr, cap| unsafe { _api_clipboard_read(ptr, cap) })
}

// ─── Timer / Clock API ─────────────────────────────────────────────────────

host_fns! {
    /// Get the current time in milliseconds since the UNIX epoch.
    pub fn time_now_ms() -> u64 = "api_time_now_ms";

    /// Schedule a one-shot timer that fires after `delay_ms` milliseconds.
    /// When it fires the host calls your exported `on_timer(callback_id)`.
    /// Returns a timer ID that can be passed to [`clear_timer`].
    pub fn set_timeout(callback_id: u32, delay_ms: u32) -> u32 = "api_set_timeout";

    /// Schedule a repeating timer that fires every `interval_ms` milliseconds.
    /// When it fires the host calls your exported `on_timer(callback_id)`.
    /// Returns a timer ID that can be passed to [`clear_timer`].
    pub fn set_interval(callback_id: u32, interval_ms: u32) -> u32 = "api_set_interval";

    /// Cancel a timer previously created with [`set_timeout`] or [`set_interval`].
    pub fn clear_timer(timer_id: u32) = "api_clear_timer";

    /// Schedule a callback for the next animation frame (vsync-aligned repaint).
    ///
    /// The host calls your exported `on_timer(callback_id)` with the provided ID on the
    /// subsequent frame. Returns a request ID usable with [`cancel_animation_frame`].
    /// Call `request_animation_frame` again from inside the callback to keep animating.
    pub fn request_animation_frame(callback_id: u32) -> u32 = "api_request_animation_frame";

    /// Cancel a pending animation frame request.
    pub fn cancel_animation_frame(request_id: u32) = "api_cancel_animation_frame";
}

// ─── Event System ───────────────────────────────────────────────────────────
//
// Register listeners for built-in or custom events. Built-in event types
// produced by the host:
//
// | Event              | Payload                                                          |
// |--------------------|------------------------------------------------------------------|
// | `resize`           | 8 bytes: `width: u32, height: u32` (little-endian)               |
// | `focus` / `blur`   | empty                                                            |
// | `visibility_change`| UTF-8 string `"visible"` or `"hidden"`                           |
// | `online`/`offline` | empty                                                            |
// | `touch_start`      | 8 bytes: `x: f32, y: f32` (little-endian)                        |
// | `touch_move`       | 8 bytes: `x: f32, y: f32`                                        |
// | `touch_end`        | 8 bytes: `x: f32, y: f32`                                        |
// | `gamepad_connected`| UTF-8 device name                                                |
// | `gamepad_button`   | 12 bytes: `id: u32, code: u32, pressed: u32`                     |
// | `gamepad_axis`     | 12 bytes: `id: u32, code: u32, value: f32`                       |
//
// Events fire via the guest-exported `on_event(callback_id: u32)` function,
// which the host calls once per pending event each frame (before timers and
// `on_frame`). Inside that callback, use [`event_type`] / [`event_data`] /
// [`event_data_into`] to inspect the current event.

/// Register a listener for events of `event_type`. When an event fires, the
/// host invokes the guest-exported `on_event(callback_id)` and exposes the
/// event payload via [`event_type`] / [`event_data`].
///
/// Returns a non-zero listener ID for [`off_event`], or `0` on failure
/// (empty event type, missing memory).
pub fn on_event(event_type: &str, callback_id: u32) -> u32 {
    unsafe {
        _api_on_event(
            event_type.as_ptr() as u32,
            event_type.len() as u32,
            callback_id,
        )
    }
}

/// Cancel a previously-registered listener. Returns `true` if a listener
/// with that ID existed and was removed.
pub fn off_event(listener_id: u32) -> bool {
    unsafe { _api_off_event(listener_id) != 0 }
}

/// Emit a custom event with an arbitrary payload. Listeners registered for
/// this event type via [`on_event`] will be invoked on the next frame
/// (before timers and `on_frame`).
pub fn emit_event(event_type: &str, data: &[u8]) {
    unsafe {
        _api_emit_event(
            event_type.as_ptr() as u32,
            event_type.len() as u32,
            data.as_ptr() as u32,
            data.len() as u32,
        )
    }
}

/// The type name of the event currently being delivered. Only meaningful
/// inside an `on_event` callback; returns an empty string otherwise.
pub fn event_type() -> String {
    let len = unsafe { _api_event_type_len() } as usize;
    host_string(len, |ptr, cap| unsafe { _api_event_type_read(ptr, cap) })
}

/// Copy the current event's payload bytes into `out` and return the number
/// of bytes written. Truncates if `out` is smaller than the payload.
pub fn event_data(out: &mut [u8]) -> usize {
    if out.is_empty() {
        return 0;
    }
    unsafe { _api_event_data_read(out.as_mut_ptr() as u32, out.len() as u32) as usize }
}

/// Allocate a fresh `Vec<u8>` containing the current event's payload.
pub fn event_data_into() -> Vec<u8> {
    let len = unsafe { _api_event_data_len() } as usize;
    host_bytes(len, |ptr, cap| {
        unsafe { _api_event_data_read(ptr, cap) }.into()
    })
    .unwrap_or_default()
}

// ─── Random API ─────────────────────────────────────────────────────────────

host_fns! {
    /// Get a random u64 from the host.
    pub fn random_u64() -> u64 = "api_random";
}

/// Get a random f64 in [0, 1).
pub fn random_f64() -> f64 {
    (random_u64() >> 11) as f64 / (1u64 << 53) as f64
}

// ─── Notification API ───────────────────────────────────────────────────────

/// Send a notification to the user (rendered in the browser console).
pub fn notify(title: &str, body: &str) {
    unsafe {
        _api_notify(
            title.as_ptr() as u32,
            title.len() as u32,
            body.as_ptr() as u32,
            body.len() as u32,
        )
    }
}

// ─── Audio Playback API ─────────────────────────────────────────────────────

/// Detected or hinted audio container (host codes: 0 unknown, 1 WAV, 2 MP3, 3 Ogg, 4 FLAC).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioFormat {
    /// Could not classify from bytes (try decode anyway).
    Unknown = 0,
    Wav = 1,
    Mp3 = 2,
    Ogg = 3,
    Flac = 4,
}

impl From<u32> for AudioFormat {
    fn from(code: u32) -> Self {
        match code {
            1 => AudioFormat::Wav,
            2 => AudioFormat::Mp3,
            3 => AudioFormat::Ogg,
            4 => AudioFormat::Flac,
            _ => AudioFormat::Unknown,
        }
    }
}

impl From<AudioFormat> for u32 {
    fn from(f: AudioFormat) -> u32 {
        f as u32
    }
}

/// Play audio from encoded bytes (WAV, MP3, OGG, FLAC).
/// The host decodes and plays the audio. Returns 0 on success, negative on error.
pub fn audio_play(data: &[u8]) -> i32 {
    unsafe { _api_audio_play(data.as_ptr() as u32, data.len() as u32) }
}

/// Sniff the container/codec from raw bytes (magic bytes / MP3 sync). Does not decode audio.
pub fn audio_detect_format(data: &[u8]) -> AudioFormat {
    AudioFormat::from(unsafe { _api_audio_detect_format(data.as_ptr() as u32, data.len() as u32) })
}

/// Play with an optional format hint (`AudioFormat::Unknown` = same as [`audio_play`]).
/// If the hint disagrees with what the host sniffs from the bytes, the host logs a warning but still decodes.
pub fn audio_play_with_format(data: &[u8], format: AudioFormat) -> i32 {
    unsafe {
        _api_audio_play_with_format(data.as_ptr() as u32, data.len() as u32, u32::from(format))
    }
}

/// Fetch audio from a URL and play it.
/// The host sends an `Accept` header listing supported codecs, records the response `Content-Type`,
/// and rejects obvious HTML/JSON error bodies when no audio signature is found (`-4`).
/// Returns 0 on success, negative on error.
pub fn audio_play_url(url: &str) -> i32 {
    unsafe { _api_audio_play_url(url.as_ptr() as u32, url.len() as u32) }
}

/// `Content-Type` header from the last successful [`audio_play_url`] response (may be empty).
pub fn audio_last_url_content_type() -> String {
    host_string(512, |ptr, cap| unsafe {
        _api_audio_last_url_content_type(ptr, cap)
    })
}

host_fns! {
    /// Pause audio playback.
    pub fn audio_pause() = "api_audio_pause";

    /// Resume paused audio playback.
    pub fn audio_resume() = "api_audio_resume";

    /// Stop audio playback and clear the queue.
    pub fn audio_stop() = "api_audio_stop";

    /// Set audio volume. 1.0 is normal, 0.0 is silent, up to 2.0 for boost.
    pub fn audio_set_volume(level: f32) = "api_audio_set_volume";

    /// Get the current audio volume.
    pub fn audio_get_volume() -> f32 = "api_audio_get_volume";

    /// Get the current playback position in milliseconds.
    pub fn audio_position() -> u64 = "api_audio_position";

    /// Seek to a position in milliseconds. Returns 0 on success, negative on error.
    pub fn audio_seek(position_ms: u64) -> i32 = "api_audio_seek";

    /// Get the total duration of the currently loaded track in milliseconds.
    /// Returns 0 if unknown or nothing is loaded.
    pub fn audio_duration() -> u64 = "api_audio_duration";
}

/// Returns `true` if audio is currently playing (not paused and not empty).
pub fn audio_is_playing() -> bool {
    unsafe { _api_audio_is_playing() != 0 }
}

/// Enable or disable looping on the default channel.
/// When enabled, subsequent `audio_play` calls will loop indefinitely.
pub fn audio_set_loop(enabled: bool) {
    unsafe { _api_audio_set_loop(u32::from(enabled)) }
}

// ─── Multi-Channel Audio API ────────────────────────────────────────────────

/// Play audio on a specific channel. Multiple channels play simultaneously.
/// Channel 0 is the default used by `audio_play`. Use channels 1+ for layered
/// sound effects, background music, etc.
pub fn audio_channel_play(channel: u32, data: &[u8]) -> i32 {
    unsafe { _api_audio_channel_play(channel, data.as_ptr() as u32, data.len() as u32) }
}

/// Like [`audio_channel_play`] with an optional [`AudioFormat`] hint.
pub fn audio_channel_play_with_format(channel: u32, data: &[u8], format: AudioFormat) -> i32 {
    unsafe {
        _api_audio_channel_play_with_format(
            channel,
            data.as_ptr() as u32,
            data.len() as u32,
            u32::from(format),
        )
    }
}

host_fns! {
    /// Stop playback on a specific channel.
    pub fn audio_channel_stop(channel: u32) = "api_audio_channel_stop";

    /// Set volume for a specific channel (0.0 silent, 1.0 normal, up to 2.0 boost).
    pub fn audio_channel_set_volume(channel: u32, level: f32) = "api_audio_channel_set_volume";
}

// ─── Video API ─────────────────────────────────────────────────────────────

/// Container or hint for [`video_load_with_format`] (host codes: 0 unknown, 1 MP4, 2 WebM, 3 AV1).
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoFormat {
    Unknown = 0,
    Mp4 = 1,
    Webm = 2,
    Av1 = 3,
}

impl From<u32> for VideoFormat {
    fn from(code: u32) -> Self {
        match code {
            1 => VideoFormat::Mp4,
            2 => VideoFormat::Webm,
            3 => VideoFormat::Av1,
            _ => VideoFormat::Unknown,
        }
    }
}

impl From<VideoFormat> for u32 {
    fn from(f: VideoFormat) -> u32 {
        f as u32
    }
}

/// Sniff container from leading bytes (magic only; does not decode).
pub fn video_detect_format(data: &[u8]) -> VideoFormat {
    VideoFormat::from(unsafe { _api_video_detect_format(data.as_ptr() as u32, data.len() as u32) })
}

/// Load video from encoded bytes (MP4, WebM, etc.). Requires FFmpeg on the host.
/// Returns 0 on success, negative on error.
pub fn video_load(data: &[u8]) -> i32 {
    video_load_with_format(data, VideoFormat::Unknown)
}

/// Load with a [`VideoFormat`] hint (unknown = same as [`video_load`]).
pub fn video_load_with_format(data: &[u8], format: VideoFormat) -> i32 {
    unsafe { _api_video_load(data.as_ptr() as u32, data.len() as u32, u32::from(format)) }
}

/// Open a progressive or adaptive (HLS) URL. The host uses FFmpeg; master playlists may list variants.
pub fn video_load_url(url: &str) -> i32 {
    unsafe { _api_video_load_url(url.as_ptr() as u32, url.len() as u32) }
}

/// `Content-Type` from the last successful [`video_load_url`] (may be empty).
pub fn video_last_url_content_type() -> String {
    host_string(512, |ptr, cap| unsafe {
        _api_video_last_url_content_type(ptr, cap)
    })
}

/// Resolved URL of HLS variant `index` from the last master playlist (empty if out of range).
pub fn video_hls_variant_url(index: u32) -> String {
    host_string(2048, |ptr, cap| unsafe {
        _api_video_hls_variant_url(index, ptr, cap)
    })
}

host_fns! {
    /// Number of variant stream URIs parsed from the last HLS master playlist (0 if not a master).
    pub fn video_hls_variant_count() -> u32 = "api_video_hls_variant_count";

    /// Open a variant playlist by index (after loading a master with [`video_load_url`]).
    pub fn video_hls_open_variant(index: u32) -> i32 = "api_video_hls_open_variant";

    /// Start or resume playback of the loaded video.
    pub fn video_play() = "api_video_play";

    /// Pause video playback.
    pub fn video_pause() = "api_video_pause";

    /// Stop video playback.
    pub fn video_stop() = "api_video_stop";

    /// Seek to a position in milliseconds. Returns 0 on success, negative on error.
    pub fn video_seek(position_ms: u64) -> i32 = "api_video_seek";

    /// Current playback position in milliseconds.
    pub fn video_position() -> u64 = "api_video_position";

    /// Total duration in milliseconds (0 if unknown).
    pub fn video_duration() -> u64 = "api_video_duration";

    /// Draw the current video frame into the given rectangle (same coordinate space as canvas).
    pub fn video_render(x: f32, y: f32, w: f32, h: f32) -> i32 = "api_video_render";

    /// Volume multiplier for the video track (0.0–2.0; embedded audio mixing may follow in future hosts).
    pub fn video_set_volume(level: f32) = "api_video_set_volume";

    /// Current video volume multiplier.
    pub fn video_get_volume() -> f32 = "api_video_get_volume";

    /// Remove loaded subtitles.
    pub fn subtitle_clear() = "api_subtitle_clear";
}

/// Enable or disable looping of the loaded video.
pub fn video_set_loop(enabled: bool) {
    unsafe { _api_video_set_loop(u32::from(enabled)) }
}

/// Load SubRip subtitles (cues rendered on [`video_render`]).
pub fn subtitle_load_srt(text: &str) -> i32 {
    unsafe { _api_subtitle_load_srt(text.as_ptr() as u32, text.len() as u32) }
}

/// Load WebVTT subtitles.
pub fn subtitle_load_vtt(text: &str) -> i32 {
    unsafe { _api_subtitle_load_vtt(text.as_ptr() as u32, text.len() as u32) }
}

// ─── Media capture API ─────────────────────────────────────────────────────

/// Returned by permission-gated APIs ([`camera_open`], [`microphone_open`], [`screen_capture`])
/// while the browser's permission prompt is awaiting the user's decision.
///
/// Not a hard failure: retry on a later frame until the call succeeds or returns `-1` (blocked).
pub const PERMISSION_PENDING: i32 = -5;

host_fns! {
    /// Opens the default camera. Gated by an in-browser permission prompt on first use per origin.
    ///
    /// Returns `0` on success. Negative codes: `-1` user blocked, `-2` no camera, `-3` open failed,
    /// [`PERMISSION_PENDING`] while the prompt is showing (retry next frame).
    pub fn camera_open() -> i32 = "api_camera_open";

    /// Stops the camera stream opened by [`camera_open`].
    pub fn camera_close() = "api_camera_close";

    /// Starts microphone capture (mono `f32` ring buffer). Gated by an in-browser permission
    /// prompt on first use per origin.
    ///
    /// Returns `0` on success. Negative codes: `-1` user blocked, `-2` no input device,
    /// `-3` stream error, [`PERMISSION_PENDING`] while the prompt is showing (retry next frame).
    pub fn microphone_open() -> i32 = "api_microphone_open";

    /// Stops microphone capture.
    pub fn microphone_close() = "api_microphone_close";

    /// Sample rate of the opened input stream in Hz (`0` if the microphone is not open).
    pub fn microphone_sample_rate() -> u32 = "api_microphone_sample_rate";
}

/// Captures one RGBA8 frame into `out`. Returns the number of bytes written (`0` if the camera
/// is not open or capture failed). Query [`camera_frame_dimensions`] after a successful write.
pub fn camera_capture_frame(out: &mut [u8]) -> u32 {
    unsafe { _api_camera_capture_frame(out.as_mut_ptr() as u32, out.len() as u32) }
}

/// Width and height in pixels of the last [`camera_capture_frame`] buffer.
pub fn camera_frame_dimensions() -> (u32, u32) {
    unpack_u32(unsafe { _api_camera_frame_dimensions() })
}

/// Dequeues up to `out.len()` mono `f32` samples from the microphone ring buffer.
/// Returns how many samples were written.
pub fn microphone_read_samples(out: &mut [f32]) -> u32 {
    unsafe { _api_microphone_read_samples(out.as_mut_ptr() as u32, out.len() as u32) }
}

/// Captures the primary display as RGBA8. Gated by an in-browser permission prompt on first
/// use per origin (the OS may prompt separately for screen recording).
///
/// Returns `Ok(bytes_written)` or an error code: `-1` user blocked, `-2` no display,
/// `-3` capture failed, `-4` buffer error, [`PERMISSION_PENDING`] while the prompt is showing
/// (retry next frame).
pub fn screen_capture(out: &mut [u8]) -> Result<usize, i32> {
    let n = unsafe { _api_screen_capture(out.as_mut_ptr() as u32, out.len() as u32) };
    usize::try_from(n).map_err(|_| n)
}

/// Width and height of the last [`screen_capture`] image.
pub fn screen_capture_dimensions() -> (u32, u32) {
    unpack_u32(unsafe { _api_screen_capture_dimensions() })
}

/// Host-side pipeline counters: total camera frames captured (high 32 bits) and current microphone
/// ring depth in samples (low 32 bits).
pub fn media_pipeline_stats() -> (u64, u32) {
    let (camera_frames, mic_ring) = unpack_u32(unsafe { _api_media_pipeline_stats() });
    (camera_frames.into(), mic_ring)
}

// ─── WebRTC / Real-Time Communication API ───────────────────────────────────

/// Connection state returned by [`rtc_connection_state`].
pub const RTC_STATE_NEW: u32 = 0;
/// Peer is attempting to connect.
pub const RTC_STATE_CONNECTING: u32 = 1;
/// Peer connection is established.
pub const RTC_STATE_CONNECTED: u32 = 2;
/// Transport was temporarily interrupted.
pub const RTC_STATE_DISCONNECTED: u32 = 3;
/// Connection attempt failed.
pub const RTC_STATE_FAILED: u32 = 4;
/// Peer connection has been closed.
pub const RTC_STATE_CLOSED: u32 = 5;

/// Track kind: audio.
pub const RTC_TRACK_AUDIO: u32 = 0;
/// Track kind: video.
pub const RTC_TRACK_VIDEO: u32 = 1;

/// Received data channel message.
pub struct RtcMessage {
    /// Channel on which the message arrived.
    pub channel_id: u32,
    /// `true` when the payload is raw bytes, `false` for UTF-8 text.
    pub is_binary: bool,
    /// Message payload.
    pub data: Vec<u8>,
}

impl RtcMessage {
    /// Interpret the payload as UTF-8 text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// Information about a newly opened remote data channel.
pub struct RtcDataChannelInfo {
    /// Handle to use with [`rtc_send`] and [`rtc_recv`].
    pub channel_id: u32,
    /// Label chosen by the remote peer.
    pub label: String,
}

/// Create a new WebRTC peer connection.
///
/// `stun_servers` is a comma-separated list of STUN/TURN URLs (e.g.
/// `"stun:stun.l.google.com:19302"`). Pass `""` for the built-in default.
///
/// Returns a peer handle (`> 0`) or `0` on failure.
pub fn rtc_create_peer(stun_servers: &str) -> u32 {
    unsafe { _api_rtc_create_peer(stun_servers.as_ptr() as u32, stun_servers.len() as u32) }
}

/// Close and release a peer connection.
pub fn rtc_close_peer(peer_id: u32) -> bool {
    unsafe { _api_rtc_close_peer(peer_id) != 0 }
}

/// Generate an SDP offer for the peer and set it as the local description.
///
/// Returns the SDP string or an error code.
pub fn rtc_create_offer(peer_id: u32) -> Result<String, i32> {
    rtc_create_sdp(peer_id, _api_rtc_create_offer)
}

/// Generate an SDP answer (after setting the remote offer) and set it as the local description.
pub fn rtc_create_answer(peer_id: u32) -> Result<String, i32> {
    rtc_create_sdp(peer_id, _api_rtc_create_answer)
}

fn rtc_create_sdp(
    peer_id: u32,
    api: unsafe extern "C" fn(u32, u32, u32) -> i32,
) -> Result<String, i32> {
    host_bytes(16 * 1024, |ptr, cap| {
        unsafe { api(peer_id, ptr, cap) }.into()
    })
    .map(into_string)
    .map_err(|e| e as i32)
}

/// Set the local SDP description explicitly.
///
/// `is_offer` — `true` for an offer, `false` for an answer.
pub fn rtc_set_local_description(peer_id: u32, sdp: &str, is_offer: bool) -> i32 {
    rtc_set_description(_api_rtc_set_local_description, peer_id, sdp, is_offer)
}

/// Set the remote SDP description received from the other peer.
pub fn rtc_set_remote_description(peer_id: u32, sdp: &str, is_offer: bool) -> i32 {
    rtc_set_description(_api_rtc_set_remote_description, peer_id, sdp, is_offer)
}

fn rtc_set_description(
    api: unsafe extern "C" fn(u32, u32, u32, u32) -> i32,
    peer_id: u32,
    sdp: &str,
    is_offer: bool,
) -> i32 {
    unsafe {
        api(
            peer_id,
            sdp.as_ptr() as u32,
            sdp.len() as u32,
            u32::from(is_offer),
        )
    }
}

/// Add a trickled ICE candidate (JSON string from the remote peer).
pub fn rtc_add_ice_candidate(peer_id: u32, candidate_json: &str) -> i32 {
    unsafe {
        _api_rtc_add_ice_candidate(
            peer_id,
            candidate_json.as_ptr() as u32,
            candidate_json.len() as u32,
        )
    }
}

host_fns! {
    /// Poll the current connection state of a peer.
    pub fn rtc_connection_state(peer_id: u32) -> u32 = "api_rtc_connection_state";

    /// Attach a media track (audio or video) to a peer connection.
    ///
    /// `kind` — [`RTC_TRACK_AUDIO`] or [`RTC_TRACK_VIDEO`].
    /// Returns a track handle (`> 0`) or `0` on failure.
    pub fn rtc_add_track(peer_id: u32, kind: u32) -> u32 = "api_rtc_add_track";
}

/// Pop the next item from one of a peer's queues, or `None` if it is empty.
fn rtc_poll(
    cap: usize,
    peer_id: u32,
    api: unsafe extern "C" fn(u32, u32, u32) -> i32,
) -> Option<String> {
    let bytes = host_bytes(cap, |ptr, cap| unsafe { api(peer_id, ptr, cap) }.into()).ok()?;
    (!bytes.is_empty()).then(|| into_string(bytes))
}

/// Poll for a locally gathered ICE candidate (JSON). Returns `None` when the
/// queue is empty.
pub fn rtc_poll_ice_candidate(peer_id: u32) -> Option<String> {
    rtc_poll(4096, peer_id, _api_rtc_poll_ice_candidate)
}

/// Create a data channel on a peer connection.
///
/// `ordered` — `true` for reliable ordered delivery (TCP-like), `false` for
/// unordered (UDP-like). Returns a channel handle (`> 0`) or `0` on failure.
pub fn rtc_create_data_channel(peer_id: u32, label: &str, ordered: bool) -> u32 {
    unsafe {
        _api_rtc_create_data_channel(
            peer_id,
            label.as_ptr() as u32,
            label.len() as u32,
            u32::from(ordered),
        )
    }
}

/// Send a UTF-8 text message on a data channel.
pub fn rtc_send_text(peer_id: u32, channel_id: u32, text: &str) -> i32 {
    rtc_send(peer_id, channel_id, text.as_bytes(), false)
}

/// Send binary data on a data channel.
pub fn rtc_send_binary(peer_id: u32, channel_id: u32, data: &[u8]) -> i32 {
    rtc_send(peer_id, channel_id, data, true)
}

/// Send data on a channel, choosing text or binary mode.
pub fn rtc_send(peer_id: u32, channel_id: u32, data: &[u8], is_binary: bool) -> i32 {
    unsafe {
        _api_rtc_send(
            peer_id,
            channel_id,
            data.as_ptr() as u32,
            data.len() as u32,
            u32::from(is_binary),
        )
    }
}

/// Poll for an incoming message on any channel of the peer (pass `channel_id = 0`)
/// or on a specific channel.
///
/// Returns `None` when no message is queued.
pub fn rtc_recv(peer_id: u32, channel_id: u32) -> Option<RtcMessage> {
    // Packed as `channel << 48 | is_binary << 32 | len`; `0` means the queue is empty.
    let mut meta = 0;
    let data = host_bytes(64 * 1024, |ptr, cap| {
        split_len(
            unsafe { _api_rtc_recv(peer_id, channel_id, ptr, cap) },
            &mut meta,
        )
    })
    .ok()?;
    if meta == 0 && data.is_empty() {
        return None;
    }
    Some(RtcMessage {
        channel_id: meta >> 16,
        is_binary: meta & 1 != 0,
        data,
    })
}

/// Poll for a remotely-created data channel that the peer opened.
///
/// Returns `None` when no new channels are pending.
pub fn rtc_poll_data_channel(peer_id: u32) -> Option<RtcDataChannelInfo> {
    let info = rtc_poll(1024, peer_id, _api_rtc_poll_data_channel)?;
    let (id, label) = info.split_once(':').unwrap_or(("0", ""));
    Some(RtcDataChannelInfo {
        channel_id: id.parse().unwrap_or(0),
        label: label.to_string(),
    })
}

/// Information about a remote media track received from a peer.
pub struct RtcTrackInfo {
    /// `RTC_TRACK_AUDIO` (0) or `RTC_TRACK_VIDEO` (1).
    pub kind: u32,
    /// Track identifier chosen by the remote peer.
    pub id: String,
    /// Media stream identifier the track belongs to.
    pub stream_id: String,
}

/// Poll for a remote media track added by the peer.
///
/// Returns `None` when no new tracks are pending.
pub fn rtc_poll_track(peer_id: u32) -> Option<RtcTrackInfo> {
    let info = rtc_poll(1024, peer_id, _api_rtc_poll_track)?;
    let mut parts = info.splitn(3, ':');
    let kind = parts.next().and_then(|k| k.parse().ok()).unwrap_or(2);
    let id = parts.next().unwrap_or("").to_string();
    let stream_id = parts.next().unwrap_or("").to_string();
    Some(RtcTrackInfo {
        kind,
        id,
        stream_id,
    })
}

/// Connect to a signaling server at `url` for bootstrapping peer connections.
///
/// Returns `1` on success, `0` on failure.
pub fn rtc_signal_connect(url: &str) -> bool {
    unsafe { _api_rtc_signal_connect(url.as_ptr() as u32, url.len() as u32) != 0 }
}

/// Join (or create) a signaling room for peer discovery.
pub fn rtc_signal_join_room(room: &str) -> i32 {
    unsafe { _api_rtc_signal_join_room(room.as_ptr() as u32, room.len() as u32) }
}

/// Send a signaling message (JSON bytes) to the connected signaling server.
pub fn rtc_signal_send(data: &[u8]) -> i32 {
    unsafe { _api_rtc_signal_send(data.as_ptr() as u32, data.len() as u32) }
}

/// Poll for an incoming signaling message.
pub fn rtc_signal_recv() -> Option<Vec<u8>> {
    let msg = host_bytes(16 * 1024, |ptr, cap| {
        unsafe { _api_rtc_signal_recv(ptr, cap) }.into()
    });
    msg.ok().filter(|msg| !msg.is_empty())
}

// ─── WebSocket API ───────────────────────────────────────────────────────────

/// WebSocket ready-state: connection is being established.
pub const WS_CONNECTING: u32 = 0;
/// WebSocket ready-state: connection is open and ready.
pub const WS_OPEN: u32 = 1;
/// WebSocket ready-state: close handshake in progress.
pub const WS_CLOSING: u32 = 2;
/// WebSocket ready-state: connection is closed.
pub const WS_CLOSED: u32 = 3;

/// A received WebSocket message.
pub struct WsMessage {
    /// `true` when the payload is raw binary; `false` for UTF-8 text.
    pub is_binary: bool,
    /// Frame payload.
    pub data: Vec<u8>,
}

impl WsMessage {
    /// Interpret the payload as a UTF-8 string.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.data).into_owned()
    }
}

/// Open a WebSocket connection to `url` (e.g. `"ws://example.com/chat"`).
///
/// Returns a connection handle (`> 0`) on success, or `0` on error.
/// The connection is established asynchronously; poll [`ws_ready_state`] until
/// it returns [`WS_OPEN`] before sending frames.
pub fn ws_connect(url: &str) -> u32 {
    unsafe { _api_ws_connect(url.as_ptr() as u32, url.len() as u32) }
}

/// Send a UTF-8 text frame on the given connection.
///
/// Returns `0` on success, `-1` if the connection is unknown or closed.
pub fn ws_send_text(id: u32, text: &str) -> i32 {
    unsafe { _api_ws_send_text(id, text.as_ptr() as u32, text.len() as u32) }
}

/// Send a binary frame on the given connection.
///
/// Returns `0` on success, `-1` if the connection is unknown or closed.
pub fn ws_send_binary(id: u32, data: &[u8]) -> i32 {
    unsafe { _api_ws_send_binary(id, data.as_ptr() as u32, data.len() as u32) }
}

/// Poll for the next queued incoming frame on `id`.
///
/// Returns `Some(WsMessage)` if a frame is available, or `None` if the queue
/// is empty.  The internal receive buffer is 64 KB; larger frames are
/// truncated to that size.
pub fn ws_recv(id: u32) -> Option<WsMessage> {
    // Packed as `is_binary << 32 | len`.
    let mut flags = 0;
    let data = host_bytes(64 * 1024, |ptr, cap| {
        split_len(unsafe { _api_ws_recv(id, ptr, cap) }, &mut flags)
    })
    .ok()?;
    Some(WsMessage {
        is_binary: flags & 1 != 0,
        data,
    })
}

host_fns! {
    /// Query the current ready-state of a connection.
    ///
    /// Returns one of [`WS_CONNECTING`], [`WS_OPEN`], [`WS_CLOSING`], or [`WS_CLOSED`].
    pub fn ws_ready_state(id: u32) -> u32 = "api_ws_ready_state";

    /// Initiate a graceful close handshake on `id`.
    ///
    /// Returns `1` if the close was initiated, `0` if the handle is unknown.
    /// After calling this function the connection will transition to [`WS_CLOSED`]
    /// asynchronously.  Call [`ws_remove`] once the state is [`WS_CLOSED`] to free
    /// host resources.
    pub fn ws_close(id: u32) -> i32 = "api_ws_close";

    /// Release host-side resources for a closed connection.
    ///
    /// Call this after [`ws_ready_state`] returns [`WS_CLOSED`] to avoid resource
    /// leaks.
    pub fn ws_remove(id: u32) = "api_ws_remove";
}

// ─── Server-Sent Events API ──────────────────────────────────────────────────

/// SSE stream is connecting or reconnecting.
pub const SSE_CONNECTING: u32 = 0;
/// SSE stream is open; events may be queued.
pub const SSE_OPEN: u32 = 1;
/// SSE stream was closed (guest close or HTTP 204).
pub const SSE_CLOSED: u32 = 2;
/// Last connection attempt failed. See [`sse_error`].
pub const SSE_ERROR: u32 = 3;

/// One event from an [`sse_open`] stream.
pub struct SseEvent {
    /// Event type (`"message"` when the server omitted `event:`).
    pub name: String,
    /// Last-Event-ID value, or empty.
    pub id: String,
    /// Payload (`data:` lines joined with a newline).
    pub data: String,
}

/// Open an EventSource-style stream at `url`.
///
/// Returns a handle (`> 0`) or `0` on error. The host reconnects automatically
/// and sends `Last-Event-ID`. Poll [`sse_state`] and drain [`sse_recv`] each frame.
pub fn sse_open(url: &str) -> u32 {
    unsafe { _api_sse_open(url.as_ptr() as u32, url.len() as u32) }
}

host_fns! {
    /// Current lifecycle state. See the `SSE_*` constants.
    pub fn sse_state(id: u32) -> u32 = "api_sse_state";

    /// Close the stream. Returns `1` if the handle was known.
    pub fn sse_close(id: u32) -> i32 = "api_sse_close";

    /// Release host resources after [`sse_state`] is [`SSE_CLOSED`].
    pub fn sse_remove(id: u32) = "api_sse_remove";
}

/// Pop the next queued event, or `None` if the queue is empty.
pub fn sse_recv(id: u32) -> Option<SseEvent> {
    let bytes = host_bytes_resizing(1024, 256 * 1024, |ptr, cap| unsafe {
        _api_sse_recv(id, ptr, cap)
    })
    .ok()?;
    decode_sse_event(&bytes)
}

/// The host's event encoding: `name` and `id` with `u16` lengths, then `data`
/// with a `u32` length, all little-endian.
fn decode_sse_event(mut bytes: &[u8]) -> Option<SseEvent> {
    let mut field = |len_size: usize| {
        let mut len = [0u8; 4];
        len[..len_size].copy_from_slice(take(&mut bytes, len_size)?);
        let value = take(&mut bytes, u32::from_le_bytes(len) as usize)?;
        Some(String::from_utf8_lossy(value).into_owned())
    };
    Some(SseEvent {
        name: field(2)?,
        id: field(2)?,
        data: field(4)?,
    })
}

/// Split `n` bytes off the front of `bytes`.
fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = bytes.split_at_checked(n)?;
    *bytes = rest;
    Some(head)
}

/// Last error message for `id`, or an empty string.
pub fn sse_error(id: u32) -> String {
    host_bytes_resizing(512, 4 * 1024, |ptr, cap| {
        unsafe { _api_sse_error(id, ptr, cap) }.into()
    })
    .map(into_string)
    .unwrap_or_default()
}

// ─── Background Workers API ────────────────────────────────────────────────────

/// Spawn a background worker from a `.wasm` module URL.
///
/// The worker runs on its own thread with isolated fuel and linear memory. Its
/// `start_app()` runs once on spawn; it then receives messages through its
/// exported `on_message(len: u32)` and replies via [`worker_post`].
///
/// `url` may be `http(s)` or `file://`. Use [`url_resolve`] against
/// [`get_url`] to load a worker module sitting next to the current app.
///
/// Returns a handle (`> 0`) on success, or `-1` on error.
pub fn spawn_worker(url: &str) -> i32 {
    unsafe { _api_spawn_worker(url.as_ptr() as u32, url.len() as u32) }
}

/// Send a message to a worker spawned with [`spawn_worker`].
///
/// Returns `0` on success, `-1` if the handle is unknown.
pub fn worker_post_message(handle: u32, data: &[u8]) -> i32 {
    unsafe { _api_worker_post_message(handle, data.as_ptr() as u32, data.len() as u32) }
}

/// Poll for one message a worker sent back via [`worker_post`].
///
/// Returns the message bytes, or `None` if the worker's outbox is empty.
pub fn worker_recv(handle: u32) -> Option<Vec<u8>> {
    host_bytes(64 * 1024, |ptr, cap| unsafe {
        _api_worker_recv(handle, ptr, cap)
    })
    .ok()
}

host_fns! {
    /// Terminate a worker and free its host-side resources.
    ///
    /// Returns `1` if the worker was running, `0` if the handle is unknown.
    pub fn worker_terminate(handle: u32) -> i32 = "api_worker_terminate";
}

/// Send a message from inside a worker back to the parent that spawned it.
///
/// Returns `0` on success, `-1` if not running inside a worker.
pub fn worker_post(data: &[u8]) -> i32 {
    unsafe { _api_worker_post(data.as_ptr() as u32, data.len() as u32) }
}

/// Copy the message currently being delivered to `on_message` into `buf`.
///
/// Valid only during a worker's `on_message` callback. Returns the number of
/// bytes written (truncated to `buf.len()`).
pub fn worker_message_read(buf: &mut [u8]) -> usize {
    unsafe { _api_worker_message_read(buf.as_mut_ptr() as u32, buf.len() as u32) as usize }
}

// ─── MIDI API ────────────────────────────────────────────────────────────────

host_fns! {
    /// Number of available MIDI input ports (physical and virtual).
    pub fn midi_input_count() -> u32 = "api_midi_input_count";

    /// Number of available MIDI output ports.
    pub fn midi_output_count() -> u32 = "api_midi_output_count";

    /// Open a MIDI input port by index and start receiving messages.
    ///
    /// Returns a handle (`> 0`) on success, or `0` if the port could not be opened.
    /// Incoming messages are queued internally; drain them with [`midi_recv`].
    pub fn midi_open_input(index: u32) -> u32 = "api_midi_open_input";

    /// Open a MIDI output port by index for sending messages.
    ///
    /// Returns a handle (`> 0`) on success, or `0` on failure.
    pub fn midi_open_output(index: u32) -> u32 = "api_midi_open_output";

    /// Close a MIDI input or output handle and free host-side resources.
    pub fn midi_close(handle: u32) = "api_midi_close";
}

/// Name of the MIDI input port at `index`.
///
/// Returns an empty string if the index is out of range.
pub fn midi_input_name(index: u32) -> String {
    host_string(128, |ptr, cap| unsafe {
        _api_midi_input_name(index, ptr, cap)
    })
}

/// Name of the MIDI output port at `index`.
///
/// Returns an empty string if the index is out of range.
pub fn midi_output_name(index: u32) -> String {
    host_string(128, |ptr, cap| unsafe {
        _api_midi_output_name(index, ptr, cap)
    })
}

/// Send raw MIDI bytes on an output `handle`.
///
/// Returns `0` on success, `-1` if the handle is unknown or the send failed.
pub fn midi_send(handle: u32, data: &[u8]) -> i32 {
    unsafe { _api_midi_send(handle, data.as_ptr() as u32, data.len() as u32) }
}

/// Poll for the next queued MIDI message on an input `handle`.
///
/// Returns `Some(bytes)` with exactly one MIDI message if one is available,
/// or `None` if the queue is empty. Channel-voice messages are 2–3 bytes;
/// SysEx can be longer. The wrapper first tries a 256-byte buffer and
/// transparently retries with a 64 KB buffer for large SysEx dumps.
pub fn midi_recv(handle: u32) -> Option<Vec<u8>> {
    let recv = |ptr: u32, cap: u32| -> i64 { unsafe { _api_midi_recv(handle, ptr, cap) }.into() };
    match host_bytes(256, recv) {
        // -2: buffer too small; the message is still queued.
        Err(-2) => host_bytes(64 * 1024, recv).ok(),
        msg => msg.ok(),
    }
}

// ─── HTTP Fetch API ─────────────────────────────────────────────────────────

/// Response from an HTTP fetch call.
pub struct FetchResponse {
    pub status: u32,
    pub body: Vec<u8>,
}

impl FetchResponse {
    /// Interpret the response body as UTF-8 text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Perform an HTTP request.  Returns the status code and response body.
///
/// `content_type` sets the `Content-Type` header (pass `""` to omit).
/// Protobuf is the native format — use `"application/protobuf"` for binary
/// payloads.
pub fn fetch(
    method: &str,
    url: &str,
    content_type: &str,
    body: &[u8],
) -> Result<FetchResponse, i64> {
    // Packed as `status << 32 | body_len`; the response buffer is 4 MB.
    let mut status = 0;
    let response = host_bytes(4 * 1024 * 1024, |ptr, cap| {
        let packed = unsafe {
            _api_fetch(
                method.as_ptr() as u32,
                method.len() as u32,
                url.as_ptr() as u32,
                url.len() as u32,
                content_type.as_ptr() as u32,
                content_type.len() as u32,
                body.as_ptr() as u32,
                body.len() as u32,
                ptr,
                cap,
            )
        };
        split_len(packed, &mut status)
    })?;
    Ok(FetchResponse {
        status,
        body: response,
    })
}

/// HTTP GET request.
pub fn fetch_get(url: &str) -> Result<FetchResponse, i64> {
    fetch("GET", url, "", &[])
}

/// HTTP POST with raw bytes.
pub fn fetch_post(url: &str, content_type: &str, body: &[u8]) -> Result<FetchResponse, i64> {
    fetch("POST", url, content_type, body)
}

/// HTTP POST with protobuf body (sets `Content-Type: application/protobuf`).
pub fn fetch_post_proto(url: &str, msg: &proto::ProtoEncoder) -> Result<FetchResponse, i64> {
    fetch("POST", url, "application/protobuf", msg.as_bytes())
}

/// HTTP PUT with raw bytes.
pub fn fetch_put(url: &str, content_type: &str, body: &[u8]) -> Result<FetchResponse, i64> {
    fetch("PUT", url, content_type, body)
}

/// HTTP DELETE.
pub fn fetch_delete(url: &str) -> Result<FetchResponse, i64> {
    fetch("DELETE", url, "", &[])
}

// ─── Streaming / non-blocking fetch ─────────────────────────────────────────
//
// The [`fetch`] family above blocks the guest until the response is fully
// downloaded. For LLM token streams, large downloads, chunked feeds, or any
// app that wants to keep rendering while a request is in flight, use the
// handle-based API below. It mirrors the WebSocket API: dispatch with
// `fetch_begin`, then poll `fetch_state`, `fetch_status`, and `fetch_recv`.

/// Request dispatched; waiting for response headers.
pub const FETCH_PENDING: u32 = 0;
/// Headers received; body chunks may still be arriving.
pub const FETCH_STREAMING: u32 = 1;
/// Body fully delivered (the queue may still have trailing chunks to drain).
pub const FETCH_DONE: u32 = 2;
/// Request failed. Call [`fetch_error`] for the message.
pub const FETCH_ERROR: u32 = 3;
/// Request was aborted by the guest.
pub const FETCH_ABORTED: u32 = 4;

/// Result of a non-blocking [`fetch_recv`] poll.
pub enum FetchChunk {
    /// One body chunk (may be part of a larger network chunk if it didn't fit
    /// in the caller's buffer).
    Data(Vec<u8>),
    /// No chunk is available right now, but more may still arrive. Call
    /// [`fetch_recv`] again next frame.
    Pending,
    /// The body has been fully delivered and all chunks have been drained.
    End,
    /// The request failed or was aborted. Inspect [`fetch_state`] and
    /// [`fetch_error`] for details.
    Error,
}

/// Dispatch an HTTP request that streams its response back to the guest.
///
/// Returns a handle (`> 0`) that identifies the request for subsequent polls,
/// or `0` if the host could not initialise the fetch subsystem. The call
/// returns immediately — the request is driven by a background task.
///
/// Pass `""` for `content_type` to omit the header, and `&[]` for `body` on
/// requests without a payload.
pub fn fetch_begin(method: &str, url: &str, content_type: &str, body: &[u8]) -> u32 {
    unsafe {
        _api_fetch_begin(
            method.as_ptr() as u32,
            method.len() as u32,
            url.as_ptr() as u32,
            url.len() as u32,
            content_type.as_ptr() as u32,
            content_type.len() as u32,
            body.as_ptr() as u32,
            body.len() as u32,
        )
    }
}

/// Convenience wrapper for GET.
pub fn fetch_begin_get(url: &str) -> u32 {
    fetch_begin("GET", url, "", &[])
}

host_fns! {
    /// Current lifecycle state of a streaming request. See the `FETCH_*` constants.
    pub fn fetch_state(handle: u32) -> u32 = "api_fetch_state";

    /// HTTP status code for `handle`, or `0` until the response headers arrive.
    pub fn fetch_status(handle: u32) -> u32 = "api_fetch_status";

    /// Free host-side resources for a completed or aborted request.
    ///
    /// Call this once you've finished draining [`fetch_recv`]. After removal the
    /// handle is invalid.
    pub fn fetch_remove(handle: u32) = "api_fetch_remove";
}

/// Poll the next body chunk into a caller-provided scratch buffer.
///
/// Use this form when you want to avoid per-chunk heap allocations. Prefer
/// [`fetch_recv`] for ergonomics in higher-level code.
///
/// Returns the number of bytes written into `buf` (which may be smaller than
/// the chunk the host has queued — in which case the remainder will be
/// returned on the next call), or one of the negative sentinels documented by
/// the host (`-1` pending, `-2` EOF, `-3` error, `-4` unknown handle).
pub fn fetch_recv_into(handle: u32, buf: &mut [u8]) -> i64 {
    unsafe { _api_fetch_recv(handle, buf.as_mut_ptr() as u32, buf.len() as u32) }
}

/// Poll the next body chunk as an owned `Vec<u8>`.
///
/// Chunks larger than 64 KiB are read in 64 KiB slices; call `fetch_recv`
/// repeatedly to drain the full network chunk.
pub fn fetch_recv(handle: u32) -> FetchChunk {
    match host_bytes(64 * 1024, |ptr, cap| unsafe {
        _api_fetch_recv(handle, ptr, cap)
    }) {
        Ok(chunk) => FetchChunk::Data(chunk),
        Err(-1) => FetchChunk::Pending,
        Err(-2) => FetchChunk::End,
        Err(_) => FetchChunk::Error,
    }
}

/// Retrieve the error message for a failed request, if any.
pub fn fetch_error(handle: u32) -> Option<String> {
    let msg = host_bytes(512, |ptr, cap| {
        unsafe { _api_fetch_error(handle, ptr, cap) }.into()
    });
    msg.ok().map(into_string)
}

/// Abort an in-flight request. Returns `true` if the handle was known.
///
/// The request transitions to [`FETCH_ABORTED`]; any body chunks already
/// queued remain readable via [`fetch_recv`] until drained.
pub fn fetch_abort(handle: u32) -> bool {
    unsafe { _api_fetch_abort(handle) != 0 }
}

// ─── Dynamic Module Loading ─────────────────────────────────────────────────

/// Fetch and execute another `.wasm` module from a URL.
/// The loaded module shares the same canvas, console, and storage context.
/// Returns 0 on success, negative error code on failure.
pub fn load_module(url: &str) -> i32 {
    unsafe { _api_load_module(url.as_ptr() as u32, url.len() as u32) }
}

// ─── Crypto / Hash API ─────────────────────────────────────────────────────

/// Compute the SHA-256 hash of the given data. Returns 32 bytes.
pub fn hash_sha256(data: &[u8]) -> [u8; 32] {
    digest(data, _api_hash_sha256)
}

/// Return SHA-256 hash as a lowercase hex string.
pub fn hash_sha256_hex(data: &[u8]) -> String {
    to_hex(&hash_sha256(data))
}

/// Compute the SHA-512 hash of the given data. Returns 64 bytes.
pub fn hash_sha512(data: &[u8]) -> [u8; 64] {
    digest(data, _api_hash_sha512)
}

/// Return SHA-512 hash as a lowercase hex string.
pub fn hash_sha512_hex(data: &[u8]) -> String {
    to_hex(&hash_sha512(data))
}

fn digest<const N: usize>(data: &[u8], api: unsafe extern "C" fn(u32, u32, u32) -> u32) -> [u8; N] {
    let mut out = [0u8; N];
    unsafe {
        api(
            data.as_ptr() as u32,
            data.len() as u32,
            out.as_mut_ptr() as u32,
        )
    };
    out
}

/// Compute the HMAC-SHA256 tag of `data` under `key`. Returns 32 bytes.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    unsafe {
        _api_hmac_sha256(
            key.as_ptr() as u32,
            key.len() as u32,
            data.as_ptr() as u32,
            data.len() as u32,
            out.as_mut_ptr() as u32,
        );
    }
    out
}

/// Return the HMAC-SHA256 tag as a lowercase hex string.
pub fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> String {
    to_hex(&hmac_sha256(key, data))
}

fn to_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                DIGITS[(b >> 4) as usize] as char,
                DIGITS[(b & 0xF) as usize] as char,
            ]
        })
        .collect()
}

/// Fill a buffer with `len` bytes of OS-grade (cryptographically secure)
/// randomness. The host serves at most 64 KiB per call, so this loops as
/// needed for larger requests.
pub fn random_bytes(len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    let mut filled = 0;
    while filled < len {
        let n =
            unsafe { _api_random_bytes(out[filled..].as_mut_ptr() as u32, (len - filled) as u32) };
        if n == 0 {
            break;
        }
        filled += n as usize;
    }
    out.truncate(filled);
    out
}

/// Generate a random RFC 4122 version-4 UUID, e.g.
/// `"3b12f1df-5232-4804-897e-917bf397618a"`.
pub fn uuid_v4() -> String {
    host_string(36, |ptr, cap| unsafe { _api_uuid_v4(ptr, cap) })
}

// ─── Base64 API ─────────────────────────────────────────────────────────────

/// Base64-encode arbitrary bytes.
pub fn base64_encode(data: &[u8]) -> String {
    host_string(data.len() * 4 / 3 + 8, |ptr, cap| unsafe {
        _api_base64_encode(data.as_ptr() as u32, data.len() as u32, ptr, cap)
    })
}

/// Decode a base64-encoded string back to bytes.
pub fn base64_decode(encoded: &str) -> Vec<u8> {
    host_bytes(encoded.len(), |ptr, cap| {
        unsafe { _api_base64_decode(encoded.as_ptr() as u32, encoded.len() as u32, ptr, cap) }
            .into()
    })
    .unwrap_or_default()
}

// ─── Compression API ────────────────────────────────────────────────────────

/// Compression format for [`compress`] / [`decompress`], mirroring the web
/// platform's `CompressionStream` formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum CompressionFormat {
    /// Gzip (RFC 1952) — like `CompressionStream("gzip")`.
    Gzip = 0,
    /// Raw deflate (RFC 1951) — like `CompressionStream("deflate-raw")`.
    Deflate = 1,
    /// Zlib (RFC 1950) — like `CompressionStream("deflate")`.
    Zlib = 2,
}

/// Compress data on the host. Returns the compressed bytes, or an empty
/// vector if compression failed (should not happen for valid input).
pub fn compress(format: CompressionFormat, data: &[u8]) -> Vec<u8> {
    // Compressed output is usually smaller than the input; +64 covers
    // headers and incompressible data. One retry if the guess is short.
    compress_call(_api_compress, format, data, data.len() + 64).unwrap_or_default()
}

/// Decompress data on the host. Returns `None` if the data is corrupt,
/// not in the given format, or the output exceeds the host's 128 MB cap.
pub fn decompress(format: CompressionFormat, data: &[u8]) -> Option<Vec<u8>> {
    compress_call(
        _api_decompress,
        format,
        data,
        data.len().saturating_mul(4) + 64,
    )
}

/// The host returns the total output size; if it exceeds our buffer, retry
/// once with exactly that size.
fn compress_call(
    api: unsafe extern "C" fn(u32, u32, u32, u32, u32) -> i64,
    format: CompressionFormat,
    data: &[u8],
    cap: usize,
) -> Option<Vec<u8>> {
    host_bytes_resizing(cap, usize::MAX, |ptr, cap| unsafe {
        api(
            format as u32,
            data.as_ptr() as u32,
            data.len() as u32,
            ptr,
            cap,
        )
    })
    .ok()
}

// ─── System Info API ────────────────────────────────────────────────────────

/// [`system_theme`] result: light colour scheme.
pub const THEME_LIGHT: u32 = 0;
/// [`system_theme`] result: dark colour scheme.
pub const THEME_DARK: u32 = 1;
/// [`system_theme`] result: preference unknown or unsupported platform.
pub const THEME_UNKNOWN: u32 = 2;

host_fns! {
    /// The OS colour scheme: [`THEME_LIGHT`], [`THEME_DARK`], or [`THEME_UNKNOWN`].
    pub fn system_theme() -> u32 = "api_system_theme";

    /// Local timezone offset in minutes east of UTC (e.g. `330` for IST, `-480` for PST).
    pub fn system_timezone_offset_minutes() -> i32 = "api_system_timezone_offset";

    /// Battery charge percentage `0..=100`, or `-1` when no battery is present.
    pub fn battery_level() -> i32 = "api_battery_level";

    /// `1` when charging or full (on AC power), `0` when discharging,
    /// `-1` when unknown or no battery is present.
    pub fn battery_charging() -> i32 = "api_battery_charging";
}

/// The system BCP 47 locale tag (e.g. `"en-IN"`), or an empty string if unknown.
pub fn system_locale() -> String {
    host_string(64, |ptr, cap| unsafe { _api_system_locale(ptr, cap) })
}

/// The IANA timezone name (e.g. `"Asia/Kolkata"`), or an empty string if unknown.
pub fn system_timezone() -> String {
    host_string(64, |ptr, cap| unsafe { _api_system_timezone(ptr, cap) })
}

// ─── Persistent Key-Value Store API ─────────────────────────────────────────

/// Store a key-value pair in the persistent on-disk KV store.
/// Returns `true` on success.
pub fn kv_store_set(key: &str, value: &[u8]) -> bool {
    let rc = unsafe {
        _api_kv_store_set(
            key.as_ptr() as u32,
            key.len() as u32,
            value.as_ptr() as u32,
            value.len() as u32,
        )
    };
    rc == 0
}

/// Convenience wrapper: store a UTF-8 string value.
pub fn kv_store_set_str(key: &str, value: &str) -> bool {
    kv_store_set(key, value.as_bytes())
}

/// Retrieve a value from the persistent KV store (read buffer: 64 KB).
/// Returns `None` if the key does not exist.
pub fn kv_store_get(key: &str) -> Option<Vec<u8>> {
    host_bytes(64 * 1024, |ptr, cap| {
        unsafe { _api_kv_store_get(key.as_ptr() as u32, key.len() as u32, ptr, cap) }.into()
    })
    .ok()
}

/// Convenience wrapper: retrieve a UTF-8 string value.
pub fn kv_store_get_str(key: &str) -> Option<String> {
    kv_store_get(key).map(into_string)
}

/// Delete a key from the persistent KV store. Returns `true` on success.
pub fn kv_store_delete(key: &str) -> bool {
    unsafe { _api_kv_store_delete(key.as_ptr() as u32, key.len() as u32) == 0 }
}

// ─── Navigation API ─────────────────────────────────────────────────────────

/// Navigate to a new URL.  The URL can be absolute or relative to the current
/// page.  Navigation happens asynchronously after the current `start_app`
/// returns.  Returns 0 on success, negative on invalid URL.
pub fn navigate(url: &str) -> i32 {
    unsafe { _api_navigate(url.as_ptr() as u32, url.len() as u32) }
}

/// Push a new entry onto this app's own history without triggering a module
/// reload.  This is analogous to `history.pushState()` in web browsers: the
/// browser's back and forward buttons walk these entries before leaving the app.
///
/// - `state`: opaque binary data retrievable later via [`get_state`].
/// - `title`: human-readable title for the history entry.
/// - `url`: the URL to display in the address bar (relative or absolute); pass
///   `""` to keep the current URL.
pub fn push_state(state: &[u8], title: &str, url: &str) {
    history_entry(_api_push_state, state, title, url)
}

/// Replace the current history entry (no new entry is pushed).
/// Analogous to `history.replaceState()`.
pub fn replace_state(state: &[u8], title: &str, url: &str) {
    history_entry(_api_replace_state, state, title, url)
}

fn history_entry(
    api: unsafe extern "C" fn(u32, u32, u32, u32, u32, u32),
    state: &[u8],
    title: &str,
    url: &str,
) {
    unsafe {
        api(
            state.as_ptr() as u32,
            state.len() as u32,
            title.as_ptr() as u32,
            title.len() as u32,
            url.as_ptr() as u32,
            url.len() as u32,
        )
    }
}

/// Get the URL of the currently loaded page.
pub fn get_url() -> String {
    host_string(4096, |ptr, cap| unsafe { _api_get_url(ptr, cap) })
}

/// Retrieve the opaque state bytes attached to the current history entry
/// (read buffer: 64 KB). Returns `None` if no state has been set.
pub fn get_state() -> Option<Vec<u8>> {
    host_bytes(64 * 1024, |ptr, cap| {
        unsafe { _api_get_state(ptr, cap) }.into()
    })
    .ok()
}

host_fns! {
    /// Return the number of entries in this app's own history (see [`push_state`]).
    pub fn history_length() -> u32 = "api_history_length";
}

/// Step back through this app's own history entries without reloading; [`get_url`]
/// and [`get_state`] change accordingly.  Returns `false` at the first entry (the
/// browser's history of other pages is not reachable from the app).
pub fn history_back() -> bool {
    unsafe { _api_history_back() == 1 }
}

/// Step forward through this app's own history entries without reloading.
/// Returns `false` at the last entry.
pub fn history_forward() -> bool {
    unsafe { _api_history_forward() == 1 }
}

// ─── Hyperlink API ──────────────────────────────────────────────────────────

/// Register a rectangular region on the canvas as a clickable hyperlink.
///
/// When the user clicks inside the rectangle the browser navigates to `url`.
/// Coordinates are in the same canvas-local space used by the drawing APIs.
/// Returns 0 on success.
pub fn register_hyperlink(x: f32, y: f32, w: f32, h: f32, url: &str) -> i32 {
    unsafe { _api_register_hyperlink(x, y, w, h, url.as_ptr() as u32, url.len() as u32) }
}

host_fns! {
    /// Remove all previously registered hyperlinks.
    pub fn clear_hyperlinks() = "api_clear_hyperlinks";
}

// ─── URL Utility API ────────────────────────────────────────────────────────

/// Resolve a relative URL against a base URL (WHATWG algorithm).
/// Returns `None` if either URL is invalid.
pub fn url_resolve(base: &str, relative: &str) -> Option<String> {
    let resolved = host_bytes(4096, |ptr, cap| {
        let (base_ptr, base_len) = (base.as_ptr() as u32, base.len() as u32);
        let (rel_ptr, rel_len) = (relative.as_ptr() as u32, relative.len() as u32);
        unsafe { _api_url_resolve(base_ptr, base_len, rel_ptr, rel_len, ptr, cap) }.into()
    });
    resolved.ok().map(into_string)
}

/// Percent-encode a string for safe inclusion in URL components.
pub fn url_encode(input: &str) -> String {
    host_string(input.len() * 3 + 4, |ptr, cap| unsafe {
        _api_url_encode(input.as_ptr() as u32, input.len() as u32, ptr, cap)
    })
}

/// Decode a percent-encoded string.
pub fn url_decode(input: &str) -> String {
    host_string(input.len() + 4, |ptr, cap| unsafe {
        _api_url_decode(input.as_ptr() as u32, input.len() as u32, ptr, cap)
    })
}

// ─── Input Polling API ──────────────────────────────────────────────────────

/// Get the mouse position in canvas-local coordinates.
pub fn mouse_position() -> (f32, f32) {
    unpack_f32(unsafe { _api_mouse_position() })
}

/// Returns `true` if the given mouse button is currently held down.
/// Button 0 = primary (left), 1 = secondary (right), 2 = middle.
pub fn mouse_button_down(button: u32) -> bool {
    unsafe { _api_mouse_button_down(button) != 0 }
}

/// Returns `true` if the given mouse button was clicked this frame.
pub fn mouse_button_clicked(button: u32) -> bool {
    unsafe { _api_mouse_button_clicked(button) != 0 }
}

/// Returns `true` if the given key is currently held down.
/// See `KEY_*` constants for key codes.
pub fn key_down(key: u32) -> bool {
    unsafe { _api_key_down(key) != 0 }
}

/// Returns `true` if the given key was pressed this frame.
pub fn key_pressed(key: u32) -> bool {
    unsafe { _api_key_pressed(key) != 0 }
}

/// Text typed since the last frame (UTF-8; empty if nothing was typed).
///
/// Use it with [`key_pressed`] for [`KEY_ENTER`] and [`KEY_BACKSPACE`] to
/// build text fields.
pub fn text_input() -> String {
    host_string(4096, |ptr, cap| unsafe { _api_text_input(ptr, cap) })
}

/// Get the scroll wheel delta for this frame.
pub fn scroll_delta() -> (f32, f32) {
    unpack_f32(unsafe { _api_scroll_delta() })
}

host_fns! {
    /// Returns modifier key state as a bitmask: bit 0 = Shift, bit 1 = Ctrl, bit 2 = Alt.
    pub fn modifiers() -> u32 = "api_modifiers";
}

/// Returns `true` if Shift is held.
pub fn shift_held() -> bool {
    modifiers() & 1 != 0
}

/// Returns `true` if Ctrl (or Cmd on macOS) is held.
pub fn ctrl_held() -> bool {
    modifiers() & 2 != 0
}

/// Returns `true` if Alt is held.
pub fn alt_held() -> bool {
    modifiers() & 4 != 0
}

// ─── Key Constants ──────────────────────────────────────────────────────────

pub const KEY_A: u32 = 0;
pub const KEY_B: u32 = 1;
pub const KEY_C: u32 = 2;
pub const KEY_D: u32 = 3;
pub const KEY_E: u32 = 4;
pub const KEY_F: u32 = 5;
pub const KEY_G: u32 = 6;
pub const KEY_H: u32 = 7;
pub const KEY_I: u32 = 8;
pub const KEY_J: u32 = 9;
pub const KEY_K: u32 = 10;
pub const KEY_L: u32 = 11;
pub const KEY_M: u32 = 12;
pub const KEY_N: u32 = 13;
pub const KEY_O: u32 = 14;
pub const KEY_P: u32 = 15;
pub const KEY_Q: u32 = 16;
pub const KEY_R: u32 = 17;
pub const KEY_S: u32 = 18;
pub const KEY_T: u32 = 19;
pub const KEY_U: u32 = 20;
pub const KEY_V: u32 = 21;
pub const KEY_W: u32 = 22;
pub const KEY_X: u32 = 23;
pub const KEY_Y: u32 = 24;
pub const KEY_Z: u32 = 25;
pub const KEY_0: u32 = 26;
pub const KEY_1: u32 = 27;
pub const KEY_2: u32 = 28;
pub const KEY_3: u32 = 29;
pub const KEY_4: u32 = 30;
pub const KEY_5: u32 = 31;
pub const KEY_6: u32 = 32;
pub const KEY_7: u32 = 33;
pub const KEY_8: u32 = 34;
pub const KEY_9: u32 = 35;
pub const KEY_ENTER: u32 = 36;
pub const KEY_ESCAPE: u32 = 37;
pub const KEY_TAB: u32 = 38;
pub const KEY_BACKSPACE: u32 = 39;
pub const KEY_DELETE: u32 = 40;
pub const KEY_SPACE: u32 = 41;
pub const KEY_UP: u32 = 42;
pub const KEY_DOWN: u32 = 43;
pub const KEY_LEFT: u32 = 44;
pub const KEY_RIGHT: u32 = 45;
pub const KEY_HOME: u32 = 46;
pub const KEY_END: u32 = 47;
pub const KEY_PAGE_UP: u32 = 48;
pub const KEY_PAGE_DOWN: u32 = 49;

// ─── Download & Print-to-PDF API ─────────────────────────────────────────────

/// Save arbitrary bytes as a file in the host Downloads directory.
///
/// Returns 0 on success, -1 on failure (empty data or filename).
pub fn download_data(data: &[u8], filename: &str) -> i32 {
    unsafe {
        _api_download_data(
            data.as_ptr() as u32,
            data.len() as u32,
            filename.as_ptr() as u32,
            filename.len() as u32,
        )
    }
}

/// Download a remote URL as a file in the host Downloads directory.
///
/// The download runs in the background with progress tracking.
/// Returns 0 on success, -1 on failure.
pub fn download_url(url: &str) -> i32 {
    unsafe { _api_download_url(url.as_ptr() as u32, url.len() as u32) }
}

/// Export the current canvas content as a PDF file.
///
/// Renders canvas draw commands (rectangles, text, lines, circles, arcs,
/// beziers, rounded rects) to a vector PDF saved in the Downloads directory.
/// Images, gradients, transforms, clipping, and opacity are not yet
/// supported — use `download_data` with a self-rendered image for those.
///
/// The output filename is auto-generated with a timestamp.
///
/// Returns 0 on success, -1 on failure.
pub fn canvas_print_pdf(filename: &str) -> i32 {
    unsafe { _api_canvas_print_pdf(filename.as_ptr() as u32, filename.len() as u32) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn host_bytes_maps_statuses() {
        assert_eq!(host_bytes(16, |_, _| 0), Ok(Vec::new()));
        assert_eq!(host_bytes(16, |_, _| -3), Err(-3));
        // More than the buffer holds: the host needs more room and wrote nothing.
        assert_eq!(host_bytes(16, |_, _| 17), Err(17));
    }

    #[test]
    fn host_bytes_resizing_retries_once_with_the_size_asked_for() {
        for need in [40, -40] {
            let caps = RefCell::new(Vec::new());
            let out = host_bytes_resizing(16, 64, |_, cap| {
                caps.borrow_mut().push(cap);
                // Ask for more room first, then report an empty read.
                if caps.borrow().len() == 1 {
                    need
                } else {
                    0
                }
            });
            assert_eq!(out, Ok(Vec::new()));
            assert_eq!(*caps.borrow(), [16, 40]);
        }
    }

    #[test]
    fn host_bytes_resizing_does_not_retry_errors_or_oversized_replies() {
        // `-2` is an error code (it fits the buffer), `1000` exceeds `max`.
        for status in [-1, -2, 1000] {
            let calls = RefCell::new(0);
            let out = host_bytes_resizing(16, 512, |_, _| {
                *calls.borrow_mut() += 1;
                status
            });
            assert_eq!(out, Err(status));
            assert_eq!(*calls.borrow(), 1);
        }
    }

    #[test]
    fn split_len_unpacks_metadata() {
        let mut meta = 0;
        let packed = (5i64 << 48) | (1 << 32) | 7;
        assert_eq!(split_len(packed, &mut meta), 7);
        assert_eq!((meta >> 16, meta & 1), (5, 1));

        let mut meta = 9;
        assert_eq!(split_len(-1, &mut meta), -1);
        assert_eq!(meta, 9);
    }

    #[test]
    fn to_hex_is_lowercase() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xab, 0xff]), "000fabff");
    }

    /// The host's `encode_event`.
    fn encode_sse(name: &str, id: &str, data: &str) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(id.len() as u16).to_le_bytes());
        out.extend_from_slice(id.as_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data.as_bytes());
        out
    }

    #[test]
    fn decodes_sse_events() {
        let ev = decode_sse_event(&encode_sse("message", "42", "line 1\nline 2")).unwrap();
        assert_eq!((ev.name.as_str(), ev.id.as_str()), ("message", "42"));
        assert_eq!(ev.data, "line 1\nline 2");

        let empty = decode_sse_event(&encode_sse("", "", "")).unwrap();
        assert!(empty.name.is_empty() && empty.id.is_empty() && empty.data.is_empty());

        let bytes = encode_sse("tick", "", "payload");
        assert!(decode_sse_event(&bytes[..bytes.len() - 1]).is_none());
        assert!(decode_sse_event(&[]).is_none());
    }

    #[test]
    fn parses_folder_entries_from_host_json() {
        // As the host's `json_escape` writes it: raw UTF-8, escaped quotes and
        // backslashes, `\u` escapes for other control characters.
        let json = r#"[{"name":"café }{ \"size\":9","size":12,"is_dir":false,"handle":3},{"name":"a\\b\tc\u0001","size":0,"is_dir":true,"handle":4}]"#;
        let entries = parse_folder_entries(json);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "café }{ \"size\":9");
        assert_eq!(
            (entries[0].size, entries[0].is_dir, entries[0].handle),
            (12, false, 3)
        );
        assert_eq!(entries[1].name, "a\\b\tc\u{1}");
        assert_eq!(
            (entries[1].size, entries[1].is_dir, entries[1].handle),
            (0, true, 4)
        );
        assert!(parse_folder_entries("[]").is_empty());
    }

    #[test]
    fn json_cursor_reads_metadata_fields_in_order() {
        let json = r#"{"name":"日本.txt","size":5,"mime":"text/plain","modified_ms":1710000000000,"is_dir":false}"#;
        let mut fields = JsonCursor(json);
        assert_eq!(fields.string("name").as_deref(), Some("日本.txt"));
        assert_eq!(fields.number("size"), Some(5));
        assert_eq!(fields.string("mime").as_deref(), Some("text/plain"));
        assert_eq!(fields.number("modified_ms"), Some(1_710_000_000_000));
        assert_eq!(fields.bool("is_dir"), Some(false));
        assert_eq!(fields.string("name"), None);
    }
}
