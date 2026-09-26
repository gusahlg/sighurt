//! Host capabilities and shared state for WebAssembly guests.
//!
//! This module defines [`HostState`] and the data structures the host and guest share
//! (console, canvas, timers, input, navigation, and more).
//! [`register_host_functions`] attaches the **`oxide`** Wasm import module to a Wasmtime
//! [`Linker`]: every host function that guest modules may call—`api_log`, `api_canvas_*`,
//! `api_storage_*`, `api_navigate`, audio APIs, etc.—is registered there under the
//! import module name `oxide`.
//!
//! Guest code imports these symbols from `oxide`; implementations run on the host and
//! read or mutate the [`HostState`] held in the Wasmtime store attached to the linker.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use wasmtime::*;

use crate::engine::{compile_cached, WasmEngine};
use crate::kv::KvStore;
use crate::navigation::{HistoryEntry, NavigationStack};
use crate::permissions::{PermissionKind, PermissionStatus, PERMISSION_PENDING};
use crate::url::{self as app_url, AppUrl};

/// All shared state between the WASM engine and a guest Wasm module (and dynamically loaded
/// children).
///
/// Most fields are behind [`Arc`] and [`Mutex`] so the same state can be shared across
/// threads and nested module loads. Host code sets fields like [`HostState::memory`] and
/// [`HostState::current_url`] before or during execution; guest imports mutate the rest
/// through the registered `oxide` functions.
#[derive(Clone, Default)]
pub struct HostState {
    /// Console lines for the browser, appended by [`HostState::log`].
    pub console: Arc<Mutex<Vec<ConsoleEntry>>>,
    /// Raster canvas: queued draw commands and decoded images for the current frame.
    pub canvas: Arc<Mutex<CanvasState>>,
    /// In-memory key/value session storage (string keys and values), similar to
    /// `sessionStorage`: it survives same-origin reloads but is cleared when the tab
    /// navigates to a different origin (see [`set_module_origin`]).
    pub storage: Arc<Mutex<HashMap<String, String>>>,
    /// Pending one-shot and interval timers; the host drains these and invokes `on_timer` on the guest.
    pub timers: Arc<Mutex<Vec<TimerEntry>>>,
    /// Pending one-shot animation frame requests. Drained every frame (before timers and
    /// `on_frame`) and fired via the existing `on_timer` export.
    pub animation_requests: Arc<Mutex<Vec<AnimationRequest>>>,
    /// Last id handed out by `api_set_timeout`, `api_set_interval` and
    /// `api_request_animation_frame`.
    pub timer_last_id: Arc<Mutex<u32>>,
    /// Per-origin grants for sensitive APIs (camera, microphone, geolocation, screen capture)
    /// plus the prompt currently awaiting a user decision (drawn by the engine over the page).
    pub permissions: crate::permissions::SharedPermissions,
    /// Manifest of the currently loaded app (`None` when the app ships without one).
    pub manifest: crate::manifest::SharedManifest,
    /// The guest’s linear memory, used to read/write pointers passed to host imports. Set
    /// before the module is instantiated, so it is always there when the guest calls in.
    pub memory: Option<Memory>,
    /// Engine and limits used by `api_load_module` and workers to run more modules.
    pub engine: Option<WasmEngine>,
    /// Same-document history for `api_push_state`, `api_replace_state` and back/forward. Each
    /// load starts a fresh stack holding just the document URL.
    pub navigation: Arc<Mutex<NavigationStack>>,
    /// Hit-test regions registered by the guest for link clicks in the canvas area.
    pub hyperlinks: Arc<Mutex<Vec<Hyperlink>>>,
    /// Links resolved against the document URL (the first field), by guest URL. Guests
    /// register their links again every frame.
    pub resolved_urls: Arc<Mutex<(String, HashMap<String, String>)>>,
    /// Set by guest `api_navigate`; the engine asks the browser to open it.
    pub pending_navigation: Arc<Mutex<Option<String>>>,
    /// The URL of the currently loaded module (set by the host before execution).
    pub current_url: Arc<Mutex<String>>,
    /// Stable origin of the currently loaded module (see [`crate::url::app_origin_of`]).
    ///
    /// Captured once per module load via [`set_module_origin`] and used to scope persistent
    /// KV storage and permissions. Unlike [`HostState::current_url`], this does not change
    /// when the guest calls `push_state` / `replace_state`.
    pub module_origin: Arc<Mutex<String>>,
    /// Input state polled by the guest each frame.
    pub input_state: Arc<Mutex<InputState>>,
    /// Audio playback (started on first use).
    pub audio: Arc<Mutex<Option<crate::audio::AudioEngine>>>,
    /// `Content-Type` of the last `api_audio_play_url` response.
    pub last_audio_url_content_type: Arc<Mutex<String>>,
    /// Video playback, decode, subtitles, and HLS variant metadata (FFmpeg).
    pub video: Arc<Mutex<crate::video::VideoPlaybackState>>,
    /// Camera, microphone, and screen capture (permission prompts + native APIs).
    pub media_capture: Arc<Mutex<crate::media_capture::MediaCaptureState>>,
    /// WebRTC peer connections, data channels, and signaling (started on first use).
    pub rtc: Arc<Mutex<Option<crate::rtc::RtcState>>>,
    /// WebSocket connections (started on first use).
    pub ws: Arc<Mutex<Option<crate::websocket::WsState>>>,
    /// MIDI input/output connections (started on first use).
    pub midi: Arc<Mutex<Option<crate::midi::MidiState>>>,
    /// Streaming / non-blocking fetch state (started on first `api_fetch_begin`).
    pub fetch: Arc<Mutex<Option<crate::fetch::FetchState>>>,
    /// Server-Sent Events streams (started on first `api_sse_open`).
    pub sse: Arc<Mutex<Option<crate::sse::SseState>>>,
    /// Native file and folder picker handles. Paths never cross the sandbox;
    /// guests only see opaque `u32` handles allocated here.
    pub file_picker: Arc<Mutex<crate::file_picker::FilePickerState>>,
    /// Event listeners, queued events, and built-in event detector state
    /// (resize, focus, online/offline, touch, gamepad, drag-drop).
    pub events: Arc<Mutex<crate::events::EventState>>,
    /// Whether the page has keyboard focus, as last reported by the browser. Consumed by the
    /// event system to fire `focus` / `blur` / `visibility_change`.
    pub focused: Arc<AtomicBool>,
    /// Content size and scroll offset of the page.
    pub scroll: Arc<Mutex<Scroll>>,
    /// Background workers spawned by this guest (started on the first `api_spawn_worker`).
    pub workers: Arc<Mutex<Option<crate::worker::WorkerState>>>,
    /// Outbound message queue, present **only** inside a worker's own state.
    /// `api_worker_post` pushes here; the parent drains it via `api_worker_recv`.
    pub worker_outbox: Option<Arc<Mutex<std::collections::VecDeque<Vec<u8>>>>>,
    /// Message currently being delivered to a worker's `on_message` export,
    /// read by `api_worker_message_read` during the callback.
    pub worker_current_msg: Arc<Mutex<Option<Vec<u8>>>>,
    /// Whether the page was told it has no GPU, which it is only once.
    pub gpu_warned: Arc<AtomicBool>,
}

impl HostState {
    /// Appends a line to the console the engine forwards to the browser.
    pub fn log(&self, level: ConsoleLevel, message: impl Into<String>) {
        console_log(&self.console, level, message.into());
    }
}

/// A single console log line: severity and message text.
#[derive(Clone, Debug)]
pub struct ConsoleEntry {
    pub level: ConsoleLevel,
    pub message: String,
}

/// Severity level for [`ConsoleEntry`] and [`console_log`].
#[derive(Clone, Copy, Debug)]
pub enum ConsoleLevel {
    Log,
    Warn,
    Error,
}

/// Appends a line to a console, for code that holds the console but not the whole state.
pub fn console_log(console: &Mutex<Vec<ConsoleEntry>>, level: ConsoleLevel, message: String) {
    console
        .lock()
        .unwrap()
        .push(ConsoleEntry { level, message });
}

/// The guest's content size and scroll offset, in canvas pixels.
#[derive(Clone, Copy, Debug, Default)]
pub struct Scroll {
    pub content_width: u32,
    pub content_height: u32,
    pub x: f32,
    pub y: f32,
}

impl Scroll {
    /// Scrolls to `(x, y)`, kept within the content for a `viewport` of the given size.
    pub fn set(&mut self, x: f32, y: f32, (viewport_w, viewport_h): (u32, u32)) {
        let max_x = (self.content_width as f32 - viewport_w as f32).max(0.0);
        let max_y = (self.content_height as f32 - viewport_h as f32).max(0.0);
        self.x = x.clamp(0.0, max_x);
        self.y = y.clamp(0.0, max_y);
    }
}

/// `0xRR, 0xGG, 0xBB, 0xAA`.
pub type Rgba = [u8; 4];

/// A color from guest arguments, which pass each channel as a `u32`.
fn rgba(r: u32, g: u32, b: u32, a: u32) -> Rgba {
    [r as u8, g as u8, b as u8, a as u8]
}

/// Most images [`CanvasState`] keeps decoded per generation; the oldest goes first.
const MAX_DECODED_IMAGES: usize = 32;

/// What the guest has drawn: the commands since the last clear and the images they use.
#[derive(Clone, Debug)]
pub struct CanvasState {
    /// Ordered draw operations accumulated since the last clear.
    pub commands: Vec<DrawCommand>,
    /// Canvas width in pixels.
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// Images [`DrawCommand::Image`] refers to by index.
    pub images: Vec<Arc<DecodedImage>>,
    /// Bumped when the canvas is cleared so the host can detect a full redraw.
    pub generation: u64,
    /// A transform, clip or opacity command is in the list.
    transformed: bool,
    /// Images decoded from guest bytes (`api_canvas_image`) since the previous clear, with the
    /// bytes they came from. Guests pass the same encoded image every frame; this saves
    /// decoding it again.
    decoded: Vec<(Vec<u8>, Arc<DecodedImage>)>,
    /// [`CanvasState::decoded`] of the generation before.
    decoded_before: Vec<(Vec<u8>, Arc<DecodedImage>)>,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            commands: Vec::new(),
            width: 800,
            height: 600,
            images: Vec::new(),
            generation: 0,
            transformed: false,
            decoded: Vec::new(),
            decoded_before: Vec::new(),
        }
    }
}

impl CanvasState {
    /// Appends `command`. An opaque fill of the whole canvas hides everything drawn before it,
    /// which is then dropped: a guest that repaints its background every frame without
    /// clearing would otherwise grow the list, and the work of every frame, without bound.
    pub fn push(&mut self, command: DrawCommand) {
        if !self.transformed && command.covers(self.width, self.height) {
            self.clear();
        }
        self.transformed |= matches!(
            command,
            DrawCommand::Transform { .. } | DrawCommand::Clip { .. } | DrawCommand::Opacity { .. }
        );
        self.commands.push(command);
    }

    /// Drops everything drawn so far.
    pub fn clear(&mut self) {
        self.commands.clear();
        self.images.clear();
        self.generation += 1;
        self.transformed = false;
        self.decoded_before = std::mem::take(&mut self.decoded);
    }

    /// Draws `image` into a rectangle.
    pub fn push_image(&mut self, image: Arc<DecodedImage>, x: f32, y: f32, w: f32, h: f32) {
        let image_id = match self.images.iter().position(|i| Arc::ptr_eq(i, &image)) {
            Some(index) => index,
            None => {
                self.images.push(image);
                self.images.len() - 1
            }
        };
        self.push(DrawCommand::Image {
            x,
            y,
            w,
            h,
            image_id,
        });
    }

    /// The image encoded in `bytes` (PNG, JPEG, GIF or WebP), decoded once per run of frames
    /// that draw it.
    fn decode(&mut self, bytes: &[u8]) -> Result<Arc<DecodedImage>, String> {
        let known = |cache: &[(Vec<u8>, Arc<DecodedImage>)]| {
            cache
                .iter()
                .find(|(encoded, _)| encoded == bytes)
                .map(|(_, image)| image.clone())
        };
        if let Some(image) = known(&self.decoded) {
            return Ok(image);
        }
        let image = match known(&self.decoded_before) {
            Some(image) => image,
            None => Arc::new(DecodedImage::decode(bytes)?),
        };
        // A guest that draws ever new images without clearing would otherwise keep them all.
        if self.decoded.len() >= MAX_DECODED_IMAGES {
            self.decoded.remove(0);
        }
        self.decoded.push((bytes.to_vec(), image.clone()));
        Ok(image)
    }
}

/// An image decoded to RGBA8 pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Raw RGBA bytes, row-major, `width * height * 4` of them.
    pub pixels: Vec<u8>,
}

impl DecodedImage {
    /// Decodes an image file, refusing images of more than 4096 × 4096 pixels before
    /// decoding them.
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        use image::ImageDecoder;
        const MAX_IMAGE_PIXELS: u32 = 4096 * 4096;
        let decoder = image::ImageReader::new(std::io::Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|e| format!("Failed to decode: {e}"))?
            .into_decoder()
            .map_err(|e| format!("Failed to decode: {e}"))?;
        let (width, height) = decoder.dimensions();
        if width.saturating_mul(height) > MAX_IMAGE_PIXELS {
            return Err(format!(
                "Rejected: {width}x{height} exceeds maximum of {MAX_IMAGE_PIXELS} pixels"
            ));
        }
        let rgba = image::DynamicImage::from_decoder(decoder)
            .map_err(|e| format!("Failed to decode: {e}"))?
            .into_rgba8();
        Ok(Self {
            width: rgba.width(),
            height: rgba.height(),
            pixels: rgba.into_raw(),
        })
    }
}

/// A single color stop inside a gradient.
#[derive(Clone, Debug, PartialEq)]
pub struct GradientStop {
    /// Position along the gradient axis, 0.0 to 1.0.
    pub offset: f32,
    pub color: Rgba,
}

/// One canvas drawing operation produced by guest `api_canvas_*` imports.
#[derive(Clone, Debug, PartialEq)]
pub enum DrawCommand {
    /// Fills the entire canvas (see `api_canvas_clear`).
    Clear { color: Rgba },
    /// Axis-aligned filled rectangle.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Rgba,
    },
    /// Filled circle centered at `(cx, cy)`.
    Circle {
        cx: f32,
        cy: f32,
        radius: f32,
        color: Rgba,
    },
    /// One line of text in the default font: the top of its `size * 1.2` line box is at `y`,
    /// its left edge at `x`.
    Text {
        x: f32,
        y: f32,
        size: f32,
        color: Rgba,
        text: String,
    },
    /// Line from `(x1, y1)` to `(x2, y2)`, `thickness` pixels wide.
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color: Rgba,
        thickness: f32,
    },
    /// Image `image_id` of [`CanvasState::images`] scaled into the rectangle.
    Image {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        image_id: usize,
    },
    /// Filled rounded rectangle with uniform corner radius.
    RoundedRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: Rgba,
    },
    /// Circular arc stroke from `start_angle` to `end_angle` (radians, CW from +X axis).
    Arc {
        cx: f32,
        cy: f32,
        radius: f32,
        start_angle: f32,
        end_angle: f32,
        color: Rgba,
        thickness: f32,
    },
    /// Cubic Bézier curve stroke from `(x1,y1)` to `(x2,y2)` with two control points.
    Bezier {
        x1: f32,
        y1: f32,
        cp1x: f32,
        cp1y: f32,
        cp2x: f32,
        cp2y: f32,
        x2: f32,
        y2: f32,
        color: Rgba,
        thickness: f32,
    },
    /// Gradient fill over an axis-aligned rectangle.
    Gradient {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        /// 0 = linear, 1 = radial.
        kind: u8,
        /// Gradient axis start (linear) or center (radial), in canvas coordinates.
        ax: f32,
        ay: f32,
        /// Gradient axis end (linear); `by` is the radius of a radial gradient.
        bx: f32,
        by: f32,
        stops: Vec<GradientStop>,
    },
    /// Push the current transform/clip/opacity state onto the stack.
    Save,
    /// Pop and restore the most recently saved state.
    Restore,
    /// Apply a 2D affine transform to subsequent draw commands (column-major: `[a,b,c,d,tx,ty]`).
    Transform {
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        tx: f32,
        ty: f32,
    },
    /// Intersect the current clip with an axis-aligned rectangle.
    Clip { x: f32, y: f32, w: f32, h: f32 },
    /// Set layer opacity for subsequent draw commands (0.0 transparent – 1.0 opaque).
    Opacity { alpha: f32 },
    /// Text with explicit family, weight, style, and alignment. `y` is the top of the line box as
    /// for [`DrawCommand::Text`]; `x` is its left edge, centre or right edge depending on `align`.
    TextEx {
        x: f32,
        y: f32,
        size: f32,
        color: Rgba,
        /// CSS-style family name (e.g. `"Helvetica"`); empty means the system UI font.
        family: String,
        /// Weight in the CSS range `100..=900`.
        weight: u16,
        /// `0` = normal, `1` = italic, `2` = oblique.
        style: u8,
        /// `0` = left (x is left edge), `1` = centre (x is centre), `2` = right (x is right edge).
        align: u8,
        text: String,
    },
}

impl DrawCommand {
    /// Whether this paints every pixel of a `width` × `height` canvas opaquely (without a
    /// transform, clip or opacity in effect).
    fn covers(&self, width: u32, height: u32) -> bool {
        let covers_rect = |x: f32, y: f32, w: f32, h: f32| {
            x <= 0.0 && y <= 0.0 && x + w >= width as f32 && y + h >= height as f32
        };
        match self {
            Self::Rect { x, y, w, h, color } => color[3] == 255 && covers_rect(*x, *y, *w, *h),
            Self::Gradient {
                x, y, w, h, stops, ..
            } => {
                !stops.is_empty()
                    && stops.iter().all(|s| s.color[3] == 255)
                    && covers_rect(*x, *y, *w, *h)
            }
            _ => false,
        }
    }
}

/// A scheduled timer: either a one-shot `setTimeout` or repeating `setInterval`.
#[derive(Clone, Debug)]
pub struct TimerEntry {
    /// Host-assigned id returned by `api_set_timeout` / `api_set_interval` for `api_clear_timer`.
    pub id: u32,
    /// Absolute time when this entry should fire next.
    pub fire_at: Instant,
    /// `None` for a one-shot timer; `Some(duration)` for an interval (rescheduled after each fire).
    pub interval: Option<Duration>,
    /// Guest-defined id passed to the exported `on_timer` callback when this timer fires.
    pub callback_id: u32,
}

/// Removes due timers from `timers` and returns their callback ids. Interval timers stay and
/// are rescheduled one interval from now.
pub fn drain_expired_timers(timers: &Mutex<Vec<TimerEntry>>) -> Vec<u32> {
    let now = Instant::now();
    let mut fired = Vec::new();
    timers.lock().unwrap().retain_mut(|timer| {
        if timer.fire_at > now {
            return true;
        }
        fired.push(timer.callback_id);
        timer.fire_at = now + timer.interval.unwrap_or_default();
        timer.interval.is_some()
    });
    fired
}

/// One-shot animation frame request. Queued by `api_request_animation_frame` and fired through
/// the guest's `on_timer` export at the next frame.
#[derive(Clone, Debug)]
pub struct AnimationRequest {
    /// Host-assigned id (for `cancel_animation_frame`).
    pub id: u32,
    /// Guest-defined id passed to `on_timer(callback_id)` when the frame fires.
    pub callback_id: u32,
}

/// Takes all pending animation frame requests and returns their callback ids.
pub fn drain_animation_frame_requests(requests: &Mutex<Vec<AnimationRequest>>) -> Vec<u32> {
    let mut requests = requests.lock().unwrap();
    requests.drain(..).map(|r| r.callback_id).collect()
}

/// A clickable axis-aligned rectangle on the canvas that navigates to a URL.
///
/// Populated by `api_register_hyperlink` and cleared with `api_clear_hyperlinks`, in canvas
/// coordinates.
#[derive(Clone, Debug)]
pub struct Hyperlink {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Target URL, resolved against the page URL when registered.
    pub url: String,
}

/// Per-frame input snapshot for guest polling via `api_mouse_*`, `api_key_*`, etc.
#[derive(Clone, Debug, Default)]
pub struct InputState {
    /// Pointer position in canvas coordinates.
    pub mouse_x: f32,
    pub mouse_y: f32,
    /// Mouse buttons currently held: index 0 = primary, 1 = secondary, 2 = middle.
    pub mouse_buttons_down: [bool; 3],
    /// Mouse buttons released this frame (same indexing as `mouse_buttons_down`).
    pub mouse_buttons_clicked: [bool; 3],
    /// Key codes currently held (polled by `api_key_down`).
    pub keys_down: Vec<u32>,
    /// Key codes that registered a press this frame (`api_key_pressed`).
    pub keys_pressed: Vec<u32>,
    /// Text typed this frame (`api_text_input`).
    pub text: String,
    pub modifiers_shift: bool,
    pub modifiers_ctrl: bool,
    pub modifiers_alt: bool,
    /// Scroll delta for this frame, positive up / left.
    pub scroll_x: f32,
    pub scroll_y: f32,
}

/// Captures the stable origin for a newly loaded module from its URL.
///
/// Called by the runtime once per load, before `start_app`. When the new origin differs from
/// the previous one, per-origin tab state is dropped: session storage is cleared (like
/// `sessionStorage` across origins in a shared tab) and live media-capture streams are
/// stopped so the new origin can't read camera frames or microphone samples opened under a
/// grant given to the previous origin.
pub fn set_module_origin(state: &HostState, url: &str) {
    let new_origin = crate::url::app_origin_of(url);
    let mut origin = state.module_origin.lock().unwrap();
    if *origin != new_origin {
        state.storage.lock().unwrap().clear();
        state.media_capture.lock().unwrap().reset();
        *origin = new_origin;
    }
}

// ── Helpers for host functions ──────────────────────────────────────────────

/// `len` bytes of guest memory at `ptr`, or `None` when that is outside the memory.
pub(crate) fn guest_bytes(caller: &Caller<'_, HostState>, ptr: u32, len: u32) -> Option<Vec<u8>> {
    let memory = caller.data().memory.expect("memory not set");
    let start = ptr as usize;
    let bytes = memory
        .data(caller)
        .get(start..start.checked_add(len as usize)?)?;
    Some(bytes.to_vec())
}

/// A UTF-8 string in guest memory, or `None` when it is outside the memory or not UTF-8.
pub(crate) fn guest_str(caller: &Caller<'_, HostState>, ptr: u32, len: u32) -> Option<String> {
    String::from_utf8(guest_bytes(caller, ptr, len)?).ok()
}

/// Copies `bytes` into guest memory at `ptr`. False when that is outside the memory.
pub(crate) fn write_guest(caller: &mut Caller<'_, HostState>, ptr: u32, bytes: &[u8]) -> bool {
    let memory = caller.data().memory.expect("memory not set");
    let start = ptr as usize;
    let Some(end) = start.checked_add(bytes.len()) else {
        return false;
    };
    match memory.data_mut(caller).get_mut(start..end) {
        Some(dest) => {
            dest.copy_from_slice(bytes);
            true
        }
        None => false,
    }
}

/// Copies as much of `bytes` as fits in a `cap`-byte guest buffer at `ptr`. Returns the number
/// of bytes written, or `None` when the buffer is outside guest memory.
pub(crate) fn write_prefix(
    caller: &mut Caller<'_, HostState>,
    ptr: u32,
    cap: u32,
    bytes: &[u8],
) -> Option<u32> {
    let len = bytes.len().min(cap as usize);
    write_guest(caller, ptr, &bytes[..len]).then_some(len as u32)
}

/// Runs `f` on a subsystem that is started on first use, if it has been.
pub(crate) fn with<T, R>(slot: &Mutex<Option<T>>, f: impl FnOnce(&mut T) -> R) -> Option<R> {
    slot.lock().unwrap().as_mut().map(f)
}

/// Runs `f` on a subsystem that is started on first use, starting it with `start` if needed.
/// `None` when it cannot be started.
pub(crate) fn with_started<T, R>(
    slot: &Mutex<Option<T>>,
    start: impl FnOnce() -> Option<T>,
    f: impl FnOnce(&mut T) -> R,
) -> Option<R> {
    let mut slot = slot.lock().unwrap();
    if slot.is_none() {
        *slot = start();
    }
    slot.as_mut().map(f)
}

/// Checks the per-origin grant for `kind`.
///
/// Returns `None` when granted, or the code the host API should return: `-1` when the user
/// blocked it (or the app's manifest doesn't declare the permission),
/// [`PERMISSION_PENDING`] while the in-browser prompt is awaiting a decision.
pub(crate) fn permission_gate(state: &HostState, kind: PermissionKind) -> Option<i32> {
    if !crate::manifest::manifest_allows(&state.manifest, kind) {
        state.log(
            ConsoleLevel::Warn,
            format!(
                "[PERMISSIONS] '{}' is not declared in the app manifest — denied",
                kind.name()
            ),
        );
        return Some(-1);
    }
    let origin = state.module_origin.lock().unwrap().clone();
    match crate::permissions::check_or_request(&state.permissions, &origin, kind) {
        PermissionStatus::Granted => None,
        PermissionStatus::Denied => Some(-1),
        PermissionStatus::Pending => Some(PERMISSION_PENDING),
    }
}

/// A blocking HTTP GET with the 30 s timeout guest fetches use. Returns the body and its
/// `Content-Type`. A body of more than `max` bytes is an error.
pub(crate) fn http_get(
    url: &str,
    accept: &str,
    max: u64,
) -> Result<(Vec<u8>, Option<String>), String> {
    use std::io::Read;
    let response = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .and_then(|client| client.get(url).header("Accept", accept).send())
        .map_err(|e| e.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let too_large = || format!("response larger than {max} bytes");
    if response.content_length().is_some_and(|len| len > max) {
        return Err(too_large());
    }
    // Content-Length can be absent or wrong: read at most one byte past the limit.
    let mut body = Vec::new();
    response
        .take(max.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|e| e.to_string())?;
    if body.len() as u64 > max {
        return Err(too_large());
    }
    Ok((body, content_type))
}

/// Resolves `raw` against the document URL, or returns it as is when either doesn't parse.
fn resolve_url(state: &HostState, raw: &str) -> String {
    let base = state.current_url.lock().unwrap();
    let mut cache = state.resolved_urls.lock().unwrap();
    let (cached_base, urls) = &mut *cache;
    if *cached_base != *base {
        cached_base.clone_from(&base);
        urls.clear();
    }
    if let Some(url) = urls.get(raw) {
        return url.clone();
    }
    let url = AppUrl::parse(&base)
        .and_then(|base| base.join(raw))
        .map_or_else(|_| raw.to_string(), |url| url.as_str().to_string());
    // A guest can make up any number of URLs, of any length; only remember a page's worth.
    if urls.len() < 1024 && raw.len() <= 1024 {
        urls.insert(raw.to_string(), url.clone());
    }
    url
}

/// The guest handle after `last`: handles count up from 1 and skip 0, which guests read as
/// failure.
pub(crate) fn next_handle(last: &mut u32) -> u32 {
    *last = last.wrapping_add(1).max(1);
    *last
}

/// The id for a new timer or animation frame request.
fn next_timer_id(state: &HostState) -> u32 {
    next_handle(&mut state.timer_last_id.lock().unwrap())
}

fn draw(caller: &Caller<'_, HostState>, command: DrawCommand) {
    caller.data().canvas.lock().unwrap().push(command);
}

/// Clamp a guest-supplied font weight to the CSS `100..=900` range. A value of
/// `0` means "use the default" and maps to `400` (normal).
fn clamp_weight(weight: u32) -> u16 {
    if weight == 0 {
        400
    } else {
        weight.clamp(100, 900) as u16
    }
}

fn getrandom(buf: &mut [u8]) {
    ::getrandom::getrandom(buf).expect("OS random number generator unavailable");
}

/// Packs two 32-bit values into the `u64` results of `api_*_position`-style imports.
pub(crate) fn pack(hi: u32, lo: u32) -> u64 {
    (u64::from(hi) << 32) | u64::from(lo)
}

/// Register every `oxide` import on `linker` so guest modules can link against them.
///
/// Each closure reads the [`HostState`] of the calling store: guest pointers are resolved
/// through [`HostState::memory`], and shared handles (`Arc<Mutex<…>>`) are updated in place.
/// Children loaded with `api_load_module` and workers get the same imports.
pub fn register_host_functions(linker: &mut Linker<HostState>) -> Result<()> {
    // ── Console ──────────────────────────────────────────────────────
    for (name, level) in [
        ("api_log", ConsoleLevel::Log),
        ("api_warn", ConsoleLevel::Warn),
        ("api_error", ConsoleLevel::Error),
    ] {
        linker.func_wrap(
            "oxide",
            name,
            move |caller: Caller<'_, HostState>, ptr: u32, len: u32| {
                let msg = guest_str(&caller, ptr, len).unwrap_or_default();
                caller.data().log(level, msg);
            },
        )?;
    }

    // ── Geolocation ──────────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_get_location",
        // Returns bytes written (>= 0), `-1` when blocked (by the user or an undeclared
        // manifest permission), or `PERMISSION_PENDING` while the prompt awaits a decision.
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> i32 {
            if let Some(code) = permission_gate(caller.data(), PermissionKind::Geolocation) {
                return code;
            }
            let location = "37.7749,-122.4194"; // mock: San Francisco
            write_prefix(&mut caller, out_ptr, out_cap, location.as_bytes()).unwrap_or(0) as i32
        },
    )?;

    // ── File Picker ──────────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_upload_file",
        |mut caller: Caller<'_, HostState>,
         name_ptr: u32,
         name_cap: u32,
         data_ptr: u32,
         data_cap: u32|
         -> u64 {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Sighurt: Select a file to upload")
                .pick_file()
            else {
                return 0;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let data = std::fs::read(&path).unwrap_or_default();
            let name_len = write_prefix(&mut caller, name_ptr, name_cap, name.as_bytes());
            let data_len = write_prefix(&mut caller, data_ptr, data_cap, &data);
            pack(name_len.unwrap_or(0), data_len.unwrap_or(0))
        },
    )?;

    // ── Canvas Drawing ───────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_canvas_clear",
        |caller: Caller<'_, HostState>, r: u32, g: u32, b: u32, a: u32| {
            let mut canvas = caller.data().canvas.lock().unwrap();
            canvas.clear();
            canvas.push(DrawCommand::Clear {
                color: rgba(r, g, b, a),
            });
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_rect",
        |caller: Caller<'_, HostState>,
         x: f32,
         y: f32,
         w: f32,
         h: f32,
         r: u32,
         g: u32,
         b: u32,
         a: u32| {
            let color = rgba(r, g, b, a);
            draw(&caller, DrawCommand::Rect { x, y, w, h, color });
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_circle",
        |caller: Caller<'_, HostState>,
         cx: f32,
         cy: f32,
         radius: f32,
         r: u32,
         g: u32,
         b: u32,
         a: u32| {
            let color = rgba(r, g, b, a);
            draw(
                &caller,
                DrawCommand::Circle {
                    cx,
                    cy,
                    radius,
                    color,
                },
            );
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_text",
        |caller: Caller<'_, HostState>,
         x: f32,
         y: f32,
         size: f32,
         r: u32,
         g: u32,
         b: u32,
         a: u32,
         txt_ptr: u32,
         txt_len: u32| {
            let text = guest_str(&caller, txt_ptr, txt_len).unwrap_or_default();
            let color = rgba(r, g, b, a);
            draw(
                &caller,
                DrawCommand::Text {
                    x,
                    y,
                    size,
                    color,
                    text,
                },
            );
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_text_ex",
        |caller: Caller<'_, HostState>,
         x: f32,
         y: f32,
         size: f32,
         r: u32,
         g: u32,
         b: u32,
         a: u32,
         fam_ptr: u32,
         fam_len: u32,
         weight: u32,
         style: u32,
         align: u32,
         txt_ptr: u32,
         txt_len: u32| {
            let family = guest_str(&caller, fam_ptr, fam_len).unwrap_or_default();
            let text = guest_str(&caller, txt_ptr, txt_len).unwrap_or_default();
            draw(
                &caller,
                DrawCommand::TextEx {
                    x,
                    y,
                    size,
                    color: rgba(r, g, b, a),
                    family,
                    weight: clamp_weight(weight),
                    style: style.min(2) as u8,
                    align: align.min(2) as u8,
                    text,
                },
            );
        },
    )?;

    // Synchronous text measurement. Writes 3 × f32 (width, ascent, descent) in
    // pixels to `out_ptr` and returns 1 on success; returns 0 if the out buffer
    // is invalid.
    linker.func_wrap(
        "oxide",
        "api_canvas_measure_text",
        |mut caller: Caller<'_, HostState>,
         size: f32,
         fam_ptr: u32,
         fam_len: u32,
         weight: u32,
         style: u32,
         txt_ptr: u32,
         txt_len: u32,
         out_ptr: u32|
         -> u32 {
            let family = guest_str(&caller, fam_ptr, fam_len).unwrap_or_default();
            let text = guest_str(&caller, txt_ptr, txt_len).unwrap_or_default();
            let metrics =
                crate::text::measure(&text, size, &family, clamp_weight(weight), style != 0);
            let mut buf = [0u8; 12];
            for (out, value) in buf.as_chunks_mut::<4>().0.iter_mut().zip(metrics) {
                *out = value.to_le_bytes();
            }
            u32::from(write_guest(&mut caller, out_ptr, &buf))
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_line",
        |caller: Caller<'_, HostState>,
         x1: f32,
         y1: f32,
         x2: f32,
         y2: f32,
         r: u32,
         g: u32,
         b: u32,
         a: u32,
         thickness: f32| {
            let color = rgba(r, g, b, a);
            draw(
                &caller,
                DrawCommand::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    color,
                    thickness,
                },
            );
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_dimensions",
        |caller: Caller<'_, HostState>| -> u64 {
            let canvas = caller.data().canvas.lock().unwrap();
            pack(canvas.width, canvas.height)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_set_content_size",
        |caller: Caller<'_, HostState>, w: u32, h: u32| {
            let mut scroll = caller.data().scroll.lock().unwrap();
            scroll.content_width = w;
            scroll.content_height = h;
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_get_scroll_position",
        |caller: Caller<'_, HostState>| -> u64 {
            let scroll = caller.data().scroll.lock().unwrap();
            pack(scroll.x.to_bits(), scroll.y.to_bits())
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_set_scroll_position",
        |caller: Caller<'_, HostState>, x: f32, y: f32| {
            let viewport = {
                let canvas = caller.data().canvas.lock().unwrap();
                (canvas.width, canvas.height)
            };
            caller.data().scroll.lock().unwrap().set(x, y, viewport);
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_rounded_rect",
        |caller: Caller<'_, HostState>,
         x: f32,
         y: f32,
         w: f32,
         h: f32,
         radius: f32,
         r: u32,
         g: u32,
         b: u32,
         a: u32| {
            let color = rgba(r, g, b, a);
            draw(
                &caller,
                DrawCommand::RoundedRect {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    color,
                },
            );
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_arc",
        |caller: Caller<'_, HostState>,
         cx: f32,
         cy: f32,
         radius: f32,
         start_angle: f32,
         end_angle: f32,
         r: u32,
         g: u32,
         b: u32,
         a: u32,
         thickness: f32| {
            let color = rgba(r, g, b, a);
            draw(
                &caller,
                DrawCommand::Arc {
                    cx,
                    cy,
                    radius,
                    start_angle,
                    end_angle,
                    color,
                    thickness,
                },
            );
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_bezier",
        |caller: Caller<'_, HostState>,
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
         thickness: f32| {
            let color = rgba(r, g, b, a);
            draw(
                &caller,
                DrawCommand::Bezier {
                    x1,
                    y1,
                    cp1x,
                    cp1y,
                    cp2x,
                    cp2y,
                    x2,
                    y2,
                    color,
                    thickness,
                },
            );
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_canvas_gradient",
        |caller: Caller<'_, HostState>,
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
         stops_len: u32| {
            let bytes = guest_bytes(&caller, stops_ptr, stops_len).unwrap_or_default();
            // Each stop is 8 bytes: f32 offset + u8 r + u8 g + u8 b + u8 a (packed).
            let stops = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|s| GradientStop {
                    offset: f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                    color: [s[4], s[5], s[6], s[7]],
                })
                .collect();
            draw(
                &caller,
                DrawCommand::Gradient {
                    x,
                    y,
                    w,
                    h,
                    kind: kind as u8,
                    ax,
                    ay,
                    bx,
                    by,
                    stops,
                },
            );
        },
    )?;

    // ── Canvas State (transform / clip / opacity) ──────────────────────
    linker.func_wrap(
        "oxide",
        "api_canvas_save",
        |caller: Caller<'_, HostState>| draw(&caller, DrawCommand::Save),
    )?;
    linker.func_wrap(
        "oxide",
        "api_canvas_restore",
        |caller: Caller<'_, HostState>| draw(&caller, DrawCommand::Restore),
    )?;
    linker.func_wrap(
        "oxide",
        "api_canvas_transform",
        |caller: Caller<'_, HostState>, a: f32, b: f32, c: f32, d: f32, tx: f32, ty: f32| {
            draw(&caller, DrawCommand::Transform { a, b, c, d, tx, ty })
        },
    )?;
    linker.func_wrap(
        "oxide",
        "api_canvas_clip",
        |caller: Caller<'_, HostState>, x: f32, y: f32, w: f32, h: f32| {
            draw(&caller, DrawCommand::Clip { x, y, w, h })
        },
    )?;
    linker.func_wrap(
        "oxide",
        "api_canvas_opacity",
        |caller: Caller<'_, HostState>, alpha: f32| draw(&caller, DrawCommand::Opacity { alpha }),
    )?;

    // ── Canvas Image ─────────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_canvas_image",
        |caller: Caller<'_, HostState>,
         x: f32,
         y: f32,
         w: f32,
         h: f32,
         data_ptr: u32,
         data_len: u32| {
            let raw = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            let mut canvas = caller.data().canvas.lock().unwrap();
            match canvas.decode(&raw) {
                Ok(image) => canvas.push_image(image, x, y, w, h),
                Err(e) => {
                    drop(canvas);
                    caller
                        .data()
                        .log(ConsoleLevel::Error, format!("[IMAGE] {e}"));
                }
            }
        },
    )?;

    // ── Local Storage ────────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_storage_set",
        |caller: Caller<'_, HostState>, key_ptr: u32, key_len: u32, val_ptr: u32, val_len: u32| {
            let key = guest_str(&caller, key_ptr, key_len).unwrap_or_default();
            let val = guest_str(&caller, val_ptr, val_len).unwrap_or_default();
            caller.data().storage.lock().unwrap().insert(key, val);
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_storage_get",
        |mut caller: Caller<'_, HostState>,
         key_ptr: u32,
         key_len: u32,
         out_ptr: u32,
         out_cap: u32|
         -> u32 {
            let key = guest_str(&caller, key_ptr, key_len).unwrap_or_default();
            let val = caller.data().storage.lock().unwrap().get(&key).cloned();
            let val = val.unwrap_or_default();
            write_prefix(&mut caller, out_ptr, out_cap, val.as_bytes()).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_storage_remove",
        |caller: Caller<'_, HostState>, key_ptr: u32, key_len: u32| {
            let key = guest_str(&caller, key_ptr, key_len).unwrap_or_default();
            caller.data().storage.lock().unwrap().remove(&key);
        },
    )?;

    // ── Clipboard ────────────────────────────────────────────────────
    // Nothing grants clipboard access to a page, so both directions are refused.
    linker.func_wrap(
        "oxide",
        "api_clipboard_write",
        |caller: Caller<'_, HostState>, _ptr: u32, _len: u32| {
            caller.data().log(
                ConsoleLevel::Warn,
                "[CLIPBOARD] Write blocked — clipboard access not permitted",
            );
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_clipboard_read",
        |caller: Caller<'_, HostState>, _out_ptr: u32, _out_cap: u32| -> u32 {
            caller.data().log(
                ConsoleLevel::Warn,
                "[CLIPBOARD] Read blocked — clipboard access not permitted",
            );
            0
        },
    )?;

    // ── Time and timers ──────────────────────────────────────────────
    // Timers fire via the guest-exported `on_timer(callback_id)` function,
    // which the host calls from the frame loop for each expired timer.
    linker.func_wrap("oxide", "api_time_now_ms", || -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    })?;

    for (name, repeat) in [("api_set_timeout", false), ("api_set_interval", true)] {
        linker.func_wrap(
            "oxide",
            name,
            move |caller: Caller<'_, HostState>, callback_id: u32, delay_ms: u32| -> u32 {
                let id = next_timer_id(caller.data());
                let delay = Duration::from_millis(u64::from(delay_ms));
                caller.data().timers.lock().unwrap().push(TimerEntry {
                    id,
                    fire_at: Instant::now() + delay,
                    interval: repeat.then_some(delay),
                    callback_id,
                });
                id
            },
        )?;
    }

    linker.func_wrap(
        "oxide",
        "api_clear_timer",
        |caller: Caller<'_, HostState>, timer_id: u32| {
            let mut timers = caller.data().timers.lock().unwrap();
            timers.retain(|t| t.id != timer_id);
        },
    )?;

    // One-shot per request (call again from inside `on_timer` to continue).
    linker.func_wrap(
        "oxide",
        "api_request_animation_frame",
        |caller: Caller<'_, HostState>, callback_id: u32| -> u32 {
            let id = next_timer_id(caller.data());
            let mut requests = caller.data().animation_requests.lock().unwrap();
            requests.push(AnimationRequest { id, callback_id });
            id
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_cancel_animation_frame",
        |caller: Caller<'_, HostState>, request_id: u32| {
            let mut requests = caller.data().animation_requests.lock().unwrap();
            requests.retain(|r| r.id != request_id);
        },
    )?;

    // ── Random ───────────────────────────────────────────────────────
    linker.func_wrap("oxide", "api_random", || -> u64 {
        let mut buf = [0u8; 8];
        getrandom(&mut buf);
        u64::from_le_bytes(buf)
    })?;

    // ── Notification (writes to console as a "notification") ─────────
    linker.func_wrap(
        "oxide",
        "api_notify",
        |caller: Caller<'_, HostState>,
         title_ptr: u32,
         title_len: u32,
         body_ptr: u32,
         body_len: u32| {
            let title = guest_str(&caller, title_ptr, title_len).unwrap_or_default();
            let body = guest_str(&caller, body_ptr, body_len).unwrap_or_default();
            let message = format!("[NOTIFICATION] {title}: {body}");
            caller.data().log(ConsoleLevel::Log, message);
        },
    )?;

    // ── HTTP Fetch ───────────────────────────────────────────────────
    // Blocks the guest until the whole response is in; `api_fetch_begin` streams instead.
    linker.func_wrap(
        "oxide",
        "api_fetch",
        |mut caller: Caller<'_, HostState>,
         method_ptr: u32,
         method_len: u32,
         url_ptr: u32,
         url_len: u32,
         ct_ptr: u32,
         ct_len: u32,
         body_ptr: u32,
         body_len: u32,
         out_ptr: u32,
         out_cap: u32|
         -> i64 {
            let method = guest_str(&caller, method_ptr, method_len).unwrap_or_default();
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            let content_type = guest_str(&caller, ct_ptr, ct_len).unwrap_or_default();
            let body = guest_bytes(&caller, body_ptr, body_len).unwrap_or_default();
            caller
                .data()
                .log(ConsoleLevel::Log, format!("[FETCH] {method} {url}"));
            let result = (|| {
                let client = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(30))
                    .build()?;
                let method = method.parse().unwrap_or(reqwest::Method::GET);
                let mut request = client.request(method, &url);
                if !content_type.is_empty() {
                    request = request.header("Content-Type", &content_type);
                }
                if !body.is_empty() {
                    request = request.body(body);
                }
                let response = request.send()?;
                Ok::<_, reqwest::Error>((response.status().as_u16(), response.bytes()?))
            })();
            match result {
                Ok((status, response)) => {
                    let len = write_prefix(&mut caller, out_ptr, out_cap, &response);
                    (i64::from(status) << 32) | i64::from(len.unwrap_or(0))
                }
                Err(e) => {
                    let message = format!("[FETCH ERROR] {e}");
                    caller.data().log(ConsoleLevel::Error, message);
                    -1
                }
            }
        },
    )?;

    // ── Dynamic Module Loading ───────────────────────────────────────
    // Fetches another .wasm module and runs its `start_app` against the same canvas,
    // console, and storage — like a <script> tag loading code into the same page.
    linker.func_wrap(
        "oxide",
        "api_load_module",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> i32 {
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            let state = caller.data();
            let Some(engine) = state.engine.clone() else {
                return -1;
            };
            state.log(ConsoleLevel::Log, format!("[LOAD] Fetching module: {url}"));
            let (code, message) = load_module(&engine, state, &url);
            let level = if code == 0 {
                ConsoleLevel::Log
            } else {
                ConsoleLevel::Error
            };
            state.log(level, message);
            code
        },
    )?;

    // ── Hashing and randomness ───────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_hash_sha256",
        |mut caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32, out_ptr: u32| -> u32 {
            use sha2::{Digest, Sha256};
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            let hash = Sha256::digest(&data);
            write_guest(&mut caller, out_ptr, &hash);
            hash.len() as u32
        },
    )?;

    // Writes the 64-byte SHA-512 digest to out_ptr. Returns 64.
    linker.func_wrap(
        "oxide",
        "api_hash_sha512",
        |mut caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32, out_ptr: u32| -> u32 {
            use sha2::{Digest, Sha512};
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            let hash = Sha512::digest(&data);
            write_guest(&mut caller, out_ptr, &hash);
            hash.len() as u32
        },
    )?;

    // Writes the 32-byte HMAC-SHA256 tag to out_ptr. Returns 32.
    linker.func_wrap(
        "oxide",
        "api_hmac_sha256",
        |mut caller: Caller<'_, HostState>,
         key_ptr: u32,
         key_len: u32,
         data_ptr: u32,
         data_len: u32,
         out_ptr: u32|
         -> u32 {
            use hmac::{Hmac, Mac};
            let key = guest_bytes(&caller, key_ptr, key_len).unwrap_or_default();
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            let mut mac = Hmac::<sha2::Sha256>::new_from_slice(&key)
                .expect("HMAC accepts keys of any length");
            mac.update(&data);
            let tag = mac.finalize().into_bytes();
            write_guest(&mut caller, out_ptr, &tag);
            tag.len() as u32
        },
    )?;

    // Fills `len` bytes (capped at 64 KiB per call) with OS-grade randomness.
    // Returns the number of bytes written.
    linker.func_wrap(
        "oxide",
        "api_random_bytes",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, len: u32| -> u32 {
            const MAX_RANDOM_BYTES: u32 = 64 * 1024;
            let mut buf = vec![0u8; len.min(MAX_RANDOM_BYTES) as usize];
            getrandom(&mut buf);
            write_prefix(&mut caller, out_ptr, len, &buf).unwrap_or(0)
        },
    )?;

    // Writes a random RFC 4122 version-4 UUID as a 36-char lowercase
    // hyphenated string. Returns bytes written (36, or 0 if out_cap < 36).
    linker.func_wrap(
        "oxide",
        "api_uuid_v4",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let mut bytes = [0u8; 16];
            getrandom(&mut bytes);
            bytes[6] = (bytes[6] & 0x0F) | 0x40; // version 4
            bytes[8] = (bytes[8] & 0x3F) | 0x80; // RFC 4122 variant
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let uuid = [
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..],
            ]
            .join("-");
            if out_cap < 36 || !write_guest(&mut caller, out_ptr, uuid.as_bytes()) {
                return 0;
            }
            36
        },
    )?;

    // ── Base64 Encoding / Decoding ───────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_base64_encode",
        |mut caller: Caller<'_, HostState>,
         data_ptr: u32,
         data_len: u32,
         out_ptr: u32,
         out_cap: u32|
         -> u32 {
            use base64::Engine;
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            let encoded = base64::engine::general_purpose::STANDARD.encode(&data);
            write_prefix(&mut caller, out_ptr, out_cap, encoded.as_bytes()).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_base64_decode",
        |mut caller: Caller<'_, HostState>,
         data_ptr: u32,
         data_len: u32,
         out_ptr: u32,
         out_cap: u32|
         -> u32 {
            use base64::Engine;
            let encoded = guest_str(&caller, data_ptr, data_len).unwrap_or_default();
            match base64::engine::general_purpose::STANDARD.decode(&encoded) {
                Ok(decoded) => write_prefix(&mut caller, out_ptr, out_cap, &decoded).unwrap_or(0),
                Err(_) => 0,
            }
        },
    )?;

    // ── Persistent Key-Value Store ───────────────────────────────────
    // Files under the host's data directory, one directory per origin (see `crate::kv`).
    // The guest never sees them.
    linker.func_wrap(
        "oxide",
        "api_kv_store_set",
        |caller: Caller<'_, HostState>,
         key_ptr: u32,
         key_len: u32,
         val_ptr: u32,
         val_len: u32|
         -> i32 {
            let key = guest_str(&caller, key_ptr, key_len).unwrap_or_default();
            let val = guest_bytes(&caller, val_ptr, val_len).unwrap_or_default();
            match kv_store(caller.data()).set(&key, &val) {
                Ok(()) => 0,
                Err(e) => {
                    let message = format!("[KV] set failed: {e}");
                    caller.data().log(ConsoleLevel::Error, message);
                    -1
                }
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_kv_store_get",
        |mut caller: Caller<'_, HostState>,
         key_ptr: u32,
         key_len: u32,
         out_ptr: u32,
         out_cap: u32|
         -> i32 {
            let key = guest_str(&caller, key_ptr, key_len).unwrap_or_default();
            match kv_store(caller.data()).get(&key) {
                Ok(Some(val)) => {
                    write_prefix(&mut caller, out_ptr, out_cap, &val).unwrap_or(0) as i32
                }
                Ok(None) => -1,
                Err(e) => {
                    let message = format!("[KV] get failed: {e}");
                    caller.data().log(ConsoleLevel::Error, message);
                    -2
                }
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_kv_store_delete",
        |caller: Caller<'_, HostState>, key_ptr: u32, key_len: u32| -> i32 {
            let key = guest_str(&caller, key_ptr, key_len).unwrap_or_default();
            match kv_store(caller.data()).delete(&key) {
                Ok(()) => 0,
                Err(e) => {
                    let message = format!("[KV] delete failed: {e}");
                    caller.data().log(ConsoleLevel::Error, message);
                    -1
                }
            }
        },
    )?;

    // ── Navigation ──────────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_navigate",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> i32 {
            let raw_url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            let state = caller.data();
            let resolved = resolve_url(state, &raw_url);
            if AppUrl::parse(&resolved).is_err() {
                state.log(
                    ConsoleLevel::Error,
                    format!("[NAV] invalid URL: {resolved}"),
                );
                return -1;
            }
            state.log(ConsoleLevel::Log, format!("[NAV] navigate → {resolved}"));
            *state.pending_navigation.lock().unwrap() = Some(resolved);
            0
        },
    )?;

    // `push_state` and `replace_state` take (state, title, url); the title is not used.
    for (name, replace) in [("api_push_state", false), ("api_replace_state", true)] {
        linker.func_wrap(
            "oxide",
            name,
            move |caller: Caller<'_, HostState>,
                  state_ptr: u32,
                  state_len: u32,
                  _title_ptr: u32,
                  _title_len: u32,
                  url_ptr: u32,
                  url_len: u32| {
                let data = guest_bytes(&caller, state_ptr, state_len).unwrap_or_default();
                let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
                let state = caller.data();
                let url = if url.is_empty() {
                    state.current_url.lock().unwrap().clone()
                } else {
                    resolve_url(state, &url)
                };
                let entry = HistoryEntry::new(&url).with_state(data);
                let mut navigation = state.navigation.lock().unwrap();
                if replace {
                    navigation.replace_current(entry);
                } else {
                    navigation.push(entry);
                }
                *state.current_url.lock().unwrap() = url;
            },
        )?;
    }

    linker.func_wrap(
        "oxide",
        "api_get_url",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let url = caller.data().current_url.lock().unwrap().clone();
            write_prefix(&mut caller, out_ptr, out_cap, url.as_bytes()).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_get_state",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> i32 {
            let state = {
                let nav = caller.data().navigation.lock().unwrap();
                nav.current()
                    .filter(|entry| !entry.state.is_empty())
                    .map(|entry| entry.state.clone())
            };
            match state {
                Some(state) => {
                    write_prefix(&mut caller, out_ptr, out_cap, &state).unwrap_or(0) as i32
                }
                None => -1,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_history_length",
        |caller: Caller<'_, HostState>| -> u32 {
            caller.data().navigation.lock().unwrap().len() as u32
        },
    )?;

    // Every entry in `navigation` belongs to the current document (each load starts a fresh
    // stack), so moving through it is a same-document traversal: no module reload.
    for (name, forward) in [("api_history_back", false), ("api_history_forward", true)] {
        linker.func_wrap("oxide", name, move |caller: Caller<'_, HostState>| -> i32 {
            i32::from(traverse(caller.data(), forward))
        })?;
    }

    // ── Hyperlinks ──────────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_register_hyperlink",
        |caller: Caller<'_, HostState>,
         x: f32,
         y: f32,
         w: f32,
         h: f32,
         url_ptr: u32,
         url_len: u32|
         -> i32 {
            let raw_url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            let url = resolve_url(caller.data(), &raw_url);
            let link = Hyperlink { x, y, w, h, url };
            caller.data().hyperlinks.lock().unwrap().push(link);
            0
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_clear_hyperlinks",
        |caller: Caller<'_, HostState>| {
            caller.data().hyperlinks.lock().unwrap().clear();
        },
    )?;

    // ── URL Utilities ───────────────────────────────────────────────
    linker.func_wrap(
        "oxide",
        "api_url_resolve",
        |mut caller: Caller<'_, HostState>,
         base_ptr: u32,
         base_len: u32,
         rel_ptr: u32,
         rel_len: u32,
         out_ptr: u32,
         out_cap: u32|
         -> i32 {
            let base = guest_str(&caller, base_ptr, base_len).unwrap_or_default();
            let rel = guest_str(&caller, rel_ptr, rel_len).unwrap_or_default();
            let Ok(base) = AppUrl::parse(&base) else {
                return -1;
            };
            let Ok(resolved) = base.join(&rel) else {
                return -2;
            };
            let resolved = resolved.as_str().as_bytes();
            write_prefix(&mut caller, out_ptr, out_cap, resolved).unwrap_or(0) as i32
        },
    )?;

    for (name, convert) in [
        (
            "api_url_encode",
            app_url::percent_encode as fn(&str) -> String,
        ),
        ("api_url_decode", app_url::percent_decode),
    ] {
        linker.func_wrap(
            "oxide",
            name,
            move |mut caller: Caller<'_, HostState>,
                  input_ptr: u32,
                  input_len: u32,
                  out_ptr: u32,
                  out_cap: u32|
                  -> u32 {
                let input = guest_str(&caller, input_ptr, input_len).unwrap_or_default();
                let output = convert(&input);
                write_prefix(&mut caller, out_ptr, out_cap, output.as_bytes()).unwrap_or(0)
            },
        )?;
    }

    // ── Input Polling ────────────────────────────────────────────────
    fn input<R>(caller: &Caller<'_, HostState>, f: impl FnOnce(&InputState) -> R) -> R {
        f(&caller.data().input_state.lock().unwrap())
    }

    linker.func_wrap(
        "oxide",
        "api_mouse_position",
        |caller: Caller<'_, HostState>| -> u64 {
            input(&caller, |i| pack(i.mouse_x.to_bits(), i.mouse_y.to_bits()))
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_mouse_button_down",
        |caller: Caller<'_, HostState>, button: u32| -> u32 {
            input(&caller, |i| {
                u32::from(i.mouse_buttons_down.get(button as usize) == Some(&true))
            })
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_mouse_button_clicked",
        |caller: Caller<'_, HostState>, button: u32| -> u32 {
            input(&caller, |i| {
                u32::from(i.mouse_buttons_clicked.get(button as usize) == Some(&true))
            })
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_key_down",
        |caller: Caller<'_, HostState>, key: u32| -> u32 {
            input(&caller, |i| u32::from(i.keys_down.contains(&key)))
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_key_pressed",
        |caller: Caller<'_, HostState>, key: u32| -> u32 {
            input(&caller, |i| u32::from(i.keys_pressed.contains(&key)))
        },
    )?;

    // Writes the UTF-8 text typed since the previous frame, cut at a character boundary if it
    // does not fit in `out_cap`. Returns bytes written.
    linker.func_wrap(
        "oxide",
        "api_text_input",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let text = input(&caller, |i| i.text.clone());
            let len = text.floor_char_boundary(out_cap as usize);
            write_prefix(&mut caller, out_ptr, len as u32, text.as_bytes()).unwrap_or(0)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_scroll_delta",
        |caller: Caller<'_, HostState>| -> u64 {
            input(&caller, |i| {
                pack(i.scroll_x.to_bits(), i.scroll_y.to_bits())
            })
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_modifiers",
        |caller: Caller<'_, HostState>| -> u32 {
            input(&caller, |i| {
                u32::from(i.modifiers_shift)
                    | (u32::from(i.modifiers_ctrl) << 1)
                    | (u32::from(i.modifiers_alt) << 2)
            })
        },
    )?;

    // ── GPU ──────────────────────────────────────────────────────────
    // `sig-wasm` has no GPU: nothing a guest renders or computes there could be shown or read
    // back. Every call answers the way it does on a machine without a GPU adapter, which
    // guests already handle.
    linker.func_wrap(
        "oxide",
        "api_gpu_create_buffer",
        |caller: Caller<'_, HostState>, _: u32, _: u32, _: u32| -> u32 {
            let state = caller.data();
            if !state.gpu_warned.swap(true, Ordering::Relaxed) {
                state.log(ConsoleLevel::Error, "[GPU] No suitable GPU adapter found");
            }
            0
        },
    )?;
    linker.func_wrap("oxide", "api_gpu_create_texture", |_: u32, _: u32| 0u32)?;
    linker.func_wrap("oxide", "api_gpu_create_shader", |_: u32, _: u32| 0u32)?;
    linker.func_wrap(
        "oxide",
        "api_gpu_create_render_pipeline",
        |_: u32, _: u32, _: u32, _: u32, _: u32| 0u32,
    )?;
    linker.func_wrap(
        "oxide",
        "api_gpu_create_compute_pipeline",
        |_: u32, _: u32, _: u32| 0u32,
    )?;
    linker.func_wrap(
        "oxide",
        "api_gpu_write_buffer",
        |_: u32, _: u32, _: u32, _: u32, _: u32| 0u32,
    )?;
    linker.func_wrap("oxide", "api_gpu_draw", |_: u32, _: u32, _: u32, _: u32| {
        0u32
    })?;
    linker.func_wrap(
        "oxide",
        "api_gpu_dispatch_compute",
        |_: u32, _: u32, _: u32, _: u32| 0u32,
    )?;
    linker.func_wrap("oxide", "api_gpu_destroy_buffer", |_: u32| 0u32)?;
    linker.func_wrap("oxide", "api_gpu_destroy_texture", |_: u32| 0u32)?;

    crate::audio::register_audio_functions(linker)?;
    crate::video::register_video_functions(linker)?;
    crate::media_capture::register_media_capture_functions(linker)?;
    crate::rtc::register_rtc_functions(linker)?;
    crate::websocket::register_ws_functions(linker)?;
    crate::midi::register_midi_functions(linker)?;
    crate::fetch::register_fetch_functions(linker)?;
    crate::sse::register_sse_functions(linker)?;
    crate::events::register_event_functions(linker)?;
    crate::file_picker::register_file_picker_functions(linker)?;
    crate::worker::register_worker_functions(linker)?;
    crate::compression::register_compression_functions(linker)?;
    crate::system::register_system_functions(linker)?;
    crate::download::register_download_functions(linker)?;
    Ok(())
}

fn kv_store(state: &HostState) -> KvStore {
    KvStore::new(
        &crate::kv::default_root(),
        &state.module_origin.lock().unwrap(),
    )
}

/// Moves one same-document history entry back or forward. Returns false at either end.
pub fn traverse(state: &HostState, forward: bool) -> bool {
    let mut nav = state.navigation.lock().unwrap();
    let entry = if forward {
        nav.go_forward()
    } else {
        nav.go_back()
    };
    let Some(entry) = entry else {
        return false;
    };
    *state.current_url.lock().unwrap() = entry.url.clone();
    true
}

/// `api_load_module`: runs the module at `url` in a child store that shares `parent`'s canvas,
/// console, storage and the rest of its state. Returns the guest's result code and a line for
/// the console.
fn load_module(engine: &WasmEngine, parent: &HostState, url: &str) -> (i32, String) {
    let page_url = parent.current_url.lock().unwrap().clone();
    let bytes = match crate::runtime::fetch_module(url, &page_url) {
        Ok(bytes) => bytes,
        Err(e) => return (-1, format!("[LOAD ERROR] {e:#}")),
    };
    let module = match compile_cached(engine.engine(), &bytes) {
        Ok(module) => module,
        Err(e) => return (-2, format!("[LOAD ERROR] Compile: {e}")),
    };
    let child = HostState {
        memory: None,
        ..parent.clone()
    };
    let (mut store, instance) = match crate::runtime::instantiate(engine, child, &module) {
        Ok(instance) => instance,
        Err(e) => return (-6, format!("[LOAD ERROR] Instantiate: {e:#}")),
    };
    let Ok(start_app) = instance.get_typed_func::<(), ()>(&mut store, "start_app") else {
        return (-7, "[LOAD ERROR] Module missing start_app".into());
    };
    match start_app.call(&mut store, ()) {
        Ok(()) => (0, format!("[LOAD] Module {url} executed successfully")),
        Err(e) => (
            -8,
            format!(
                "[LOAD ERROR] {}",
                crate::runtime::guest_error("start_app", &e)
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_the_guest_api() {
        let engine = Engine::default();
        let mut linker = Linker::new(&engine);
        register_host_functions(&mut linker).unwrap();
        let mut store = Store::new(&engine, HostState::default());
        for name in [
            "api_canvas_rect",
            "api_text_input",
            "api_camera_open",
            "api_rtc_create_peer",
            "api_gpu_draw",
            "api_audio_play",
            "api_video_render",
            "api_video_set_pip",
            "api_download_url",
        ] {
            assert!(linker.get(&mut store, "oxide", name).is_some(), "{name}");
        }
        // Guests draw their own widgets.
        assert!(linker.get(&mut store, "oxide", "api_ui_button").is_none());
    }

    #[test]
    fn module_origin_ignores_path_changes() {
        let state = HostState::default();
        set_module_origin(&state, "https://example.com/apps/a.wasm");
        let first = state.module_origin.lock().unwrap().clone();
        // Same origin, different path (e.g. after navigating to a sibling module).
        set_module_origin(&state, "https://example.com/other/b.wasm");
        assert_eq!(*state.module_origin.lock().unwrap(), first);
    }

    #[test]
    fn session_storage_survives_same_origin_reload() {
        let state = HostState::default();
        set_module_origin(&state, "https://example.com/app.wasm");
        state
            .storage
            .lock()
            .unwrap()
            .insert("k".to_string(), "v".to_string());
        set_module_origin(&state, "https://example.com/app.wasm");
        assert_eq!(
            state.storage.lock().unwrap().get("k").map(String::as_str),
            Some("v")
        );
    }

    #[test]
    fn session_storage_cleared_on_cross_origin_navigation() {
        let state = HostState::default();
        set_module_origin(&state, "https://a.com/app.wasm");
        state
            .storage
            .lock()
            .unwrap()
            .insert("k".to_string(), "v".to_string());
        set_module_origin(&state, "https://b.com/app.wasm");
        assert!(state.storage.lock().unwrap().is_empty());
    }

    #[test]
    fn local_apps_in_different_directories_have_different_origins() {
        let state = HostState::default();
        set_module_origin(&state, "file:///tmp/app-one/index.wasm");
        let one = state.module_origin.lock().unwrap().clone();
        set_module_origin(&state, "file:///tmp/app-two/index.wasm");
        assert_ne!(*state.module_origin.lock().unwrap(), one);
    }

    #[test]
    fn links_resolve_against_the_current_url() {
        let state = HostState::default();
        assert_eq!(resolve_url(&state, "b.wasm"), "b.wasm");
        *state.current_url.lock().unwrap() = "https://a.test/apps/a.wasm".into();
        assert_eq!(resolve_url(&state, "b.wasm"), "https://a.test/apps/b.wasm");
        assert_eq!(resolve_url(&state, "b.wasm"), "https://a.test/apps/b.wasm");
        // `push_state` moved the document: the cached result must not be reused.
        *state.current_url.lock().unwrap() = "https://a.test/other/".into();
        assert_eq!(resolve_url(&state, "b.wasm"), "https://a.test/other/b.wasm");
        assert_eq!(resolve_url(&state, "ftp://x"), "ftp://x");
    }

    #[test]
    fn timers_fire_once_and_intervals_repeat() {
        let timers = Mutex::new(Vec::new());
        let now = Instant::now();
        let timer = |id, interval| TimerEntry {
            id,
            fire_at: now,
            interval,
            callback_id: id * 10,
        };
        timers.lock().unwrap().extend([
            timer(1, None),
            timer(2, Some(Duration::from_secs(60))),
            TimerEntry {
                fire_at: now + Duration::from_secs(60),
                ..timer(3, None)
            },
        ]);
        assert_eq!(drain_expired_timers(&timers), [10, 20]);
        assert_eq!(drain_expired_timers(&timers), Vec::<u32>::new());
        let ids: Vec<_> = timers.lock().unwrap().iter().map(|t| t.id).collect();
        assert_eq!(ids, [2, 3]);
    }

    fn rect(x: f32, w: f32, alpha: u8) -> DrawCommand {
        DrawCommand::Rect {
            x,
            y: 0.0,
            w,
            h: 600.0,
            color: [1, 2, 3, alpha],
        }
    }

    #[test]
    fn an_opaque_fill_of_the_canvas_replaces_what_it_hides() {
        let mut canvas = CanvasState::default();
        canvas.push(rect(10.0, 10.0, 255));
        // Translucent, or not covering the canvas: kept on top of what is there.
        canvas.push(rect(0.0, 800.0, 254));
        canvas.push(rect(1.0, 800.0, 255));
        assert_eq!(canvas.commands.len(), 3);
        canvas.push(rect(0.0, 800.0, 255));
        assert_eq!(canvas.commands, [rect(0.0, 800.0, 255)]);
        assert_eq!(canvas.generation, 1);

        // Under a transform the fill may not cover the canvas.
        canvas.push(DrawCommand::Transform {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            tx: 5.0,
            ty: 0.0,
        });
        canvas.push(rect(0.0, 800.0, 255));
        assert_eq!(canvas.commands.len(), 3);
        canvas.clear();
        canvas.push(rect(0.0, 800.0, 255));
        assert_eq!(canvas.commands.len(), 1);
    }

    #[test]
    fn images_are_decoded_once_while_they_are_drawn() {
        let mut png = Vec::new();
        image::RgbaImage::from_pixel(2, 1, image::Rgba([9, 8, 7, 255]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let mut canvas = CanvasState::default();
        let first = canvas.decode(&png).unwrap();
        assert_eq!((first.width, first.height), (2, 1));
        assert_eq!(first.pixels, [9, 8, 7, 255, 9, 8, 7, 255]);
        // Drawn twice in a frame: one image.
        canvas.push_image(first.clone(), 0.0, 0.0, 1.0, 1.0);
        let again = canvas.decode(&png).unwrap();
        canvas.push_image(again, 5.0, 0.0, 1.0, 1.0);
        assert_eq!(canvas.images.len(), 1);
        // The next frames reuse it; a frame without it forgets it.
        canvas.clear();
        assert!(Arc::ptr_eq(&canvas.decode(&png).unwrap(), &first));
        canvas.clear();
        canvas.clear();
        assert!(!Arc::ptr_eq(&canvas.decode(&png).unwrap(), &first));
        assert!(canvas.decode(b"not an image").is_err());
    }

    #[test]
    fn decoded_images_are_kept_up_to_a_limit() {
        let png = |shade: u8| {
            let mut png = Vec::new();
            image::RgbaImage::from_pixel(1, 1, image::Rgba([shade, 0, 0, 255]))
                .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .unwrap();
            png
        };
        let mut canvas = CanvasState::default();
        let first = canvas.decode(&png(0)).unwrap();
        for shade in 1..=MAX_DECODED_IMAGES as u8 {
            canvas.decode(&png(shade)).unwrap();
        }
        assert_eq!(canvas.decoded.len(), MAX_DECODED_IMAGES);
        // The oldest went first.
        assert!(!Arc::ptr_eq(&canvas.decode(&png(0)).unwrap(), &first));
    }
}
