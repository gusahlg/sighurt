//! The page a `sig-wasm` process shows: it loads Oxide apps, runs their frames and turns what
//! they draw and ask for into protocol messages.
//!
//! Every load gets a fresh [`HostState`] (see [`document_state`]): the loader thread fetches,
//! compiles and starts the module in it (`start_app` runs there) and hands the resulting
//! [`LiveModule`] back to the main thread, which swaps it in. Until then the previous document
//! keeps running untouched, and a superseded or stopped load is dropped together with everything
//! it started.
//!
//! Input arrives in physical pixels. The guest sees canvas coordinates: logical pixels divided by
//! the page zoom. The draw list goes out in logical pixels, zoomed.
//!
//! ## History
//!
//! Each document's history starts as just its URL. Guest `push_state` adds same-document entries
//! on top, and Back / Forward (like the guest's `history_back` / `history_forward`) walk them
//! without re-fetching the module: they move the stack and the URL the guest reads back through
//! `get_url` / `get_state`. The guest ABI has no popstate event, so guests notice by polling
//! those.

use std::io::{self, BufWriter, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use sighurt_engine_wasm::capabilities::{traverse, ConsoleEntry, ConsoleLevel, HostState};
use sighurt_engine_wasm::engine::{SandboxPolicy, WasmEngine};
use sighurt_engine_wasm::navigation::{HistoryEntry, NavigationStack};
use sighurt_engine_wasm::permissions;
use sighurt_engine_wasm::runtime::{self, LiveModule};
use sighurt_ipc::{DrawOp, FromEngine, ToEngine, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT};

use crate::draw::Presenter;
use crate::keys;

/// The protocol stream to the browser. Shared with the panic hook.
pub type Output = Arc<Mutex<BufWriter<Box<dyn Write + Send>>>>;

/// Name of the thread that loads apps.
pub const LOADER_THREAD: &str = "sig-wasm-loader";

/// Guest frames run at about 60 Hz while a module is live.
const FRAME: Duration = Duration::from_micros(16_667);

/// What the main thread waits for.
pub enum Msg {
    Host(ToEngine),
    Loaded(Box<Loaded>),
    /// The browser closed our stdin.
    Closed,
}

/// A finished load, tagged with the navigation that asked for it.
pub struct Loaded {
    generation: u64,
    result: Result<Option<LiveModule>, String>,
}

/// A load for the loader thread.
struct Request {
    generation: u64,
    url: String,
    /// The new document's state (see [`document_state`]).
    host_state: HostState,
}

pub struct Page {
    out: Out,
    loader: mpsc::Sender<Request>,
    /// Generation of the newest navigation. Results of older ones are dropped, and the loader
    /// skips their requests if they are still queued.
    generation: Arc<AtomicU64>,
    /// State of the document on screen.
    host_state: HostState,
    /// URL and state of the document being loaded; they replace `document_url` and `host_state`
    /// when its load finishes.
    loading: Option<(String, HostState)>,
    /// URL the document on screen was loaded from; Reload re-runs it even after `push_state`.
    document_url: String,
    live: Option<LiveModule>,
    last_tick: Instant,
    next_tick: Instant,
    /// Viewport size in physical pixels and its scale factor, as last sent by the browser.
    viewport: (u32, u32, f32),
    zoom: f32,
    /// Pointer position in logical pixels.
    pointer: (f32, f32),
    /// Link under the pointer when a button went down; releasing over the same link opens it.
    pressed_link: Option<String>,
    /// The permission prompt took the current left click.
    prompt_grab: bool,
    presenter: Presenter,
    /// The draw list must be rebuilt although no frame ran (zoom, prompt, a new document).
    dirty: bool,
    /// What the browser was last told.
    shown: Shown,
}

#[derive(Default)]
struct Shown {
    url: String,
    title: String,
    history: (bool, bool),
    status: String,
    cursor: &'static str,
}

impl Page {
    /// Creates the sandbox and starts the loader thread, which reports back through `msgs`.
    pub fn new(out: Output, msgs: mpsc::Sender<Msg>) -> anyhow::Result<Self> {
        let engine = WasmEngine::new(SandboxPolicy::default())?;
        let generation = Arc::new(AtomicU64::new(0));
        let loader = spawn_loader(engine.clone(), generation.clone(), msgs)?;
        let now = Instant::now();
        Ok(Self {
            out: Out {
                stream: out,
                closed: false,
            },
            loader,
            generation,
            host_state: HostState {
                engine: Some(engine),
                focused: Arc::new(AtomicBool::new(true)),
                ..Default::default()
            },
            loading: None,
            document_url: String::new(),
            live: None,
            last_tick: now,
            next_tick: now,
            viewport: (800, 600, 1.0),
            zoom: 1.0,
            pointer: (0.0, 0.0),
            pressed_link: None,
            prompt_grab: false,
            presenter: Presenter::default(),
            dirty: false,
            shown: Shown {
                cursor: "default",
                ..Default::default()
            },
        })
    }

    /// When the next guest frame is due, while a module is live.
    pub fn next_tick(&self) -> Option<Instant> {
        self.live.as_ref().map(|_| self.next_tick)
    }

    /// Whether the browser stopped reading.
    pub fn closed(&self) -> bool {
        self.out.closed
    }

    pub fn handle(&mut self, msg: ToEngine) {
        match msg {
            ToEngine::Navigate { url } => self.load(url),
            ToEngine::Reload => {
                // Restarts the load in progress, or else reloads the document on screen.
                let url = match &self.loading {
                    Some((url, _)) => url.clone(),
                    None => self.document_url.clone(),
                };
                if !url.is_empty() {
                    self.load(url);
                }
            }
            ToEngine::Stop => {
                // Abandons the load in progress; the document on screen stays.
                if self.loading.take().is_some() {
                    self.generation.fetch_add(1, Ordering::SeqCst);
                    self.out.send(&FromEngine::Loading { loading: false });
                }
            }
            ToEngine::Back => {
                traverse(&self.host_state, false);
            }
            ToEngine::Forward => {
                traverse(&self.host_state, true);
            }
            ToEngine::Resize {
                width,
                height,
                scale,
            } => {
                self.viewport = (width, height, if scale > 0.0 { scale } else { 1.0 });
                self.layout();
            }
            ToEngine::Zoom { factor } => {
                if factor > 0.0 {
                    self.zoom = factor;
                    self.layout();
                    self.dirty = true;
                }
            }
            ToEngine::Focus { focused } => {
                self.host_state.focused.store(focused, Ordering::Relaxed);
                if !focused {
                    // Keys released after focus moved elsewhere never reach the page.
                    let mut input = self.host_state.input_state.lock().unwrap();
                    input.keys_down.clear();
                    input.modifiers_shift = false;
                    input.modifiers_ctrl = false;
                    input.modifiers_alt = false;
                }
            }
            ToEngine::MouseMove { x, y } => self.pointer_moved(x, y),
            ToEngine::MouseButton { button, down, x, y } => self.mouse_button(button, down, x, y),
            ToEngine::Wheel { dx, dy, x, y } => {
                self.pointer_moved(x, y);
                self.wheel(dx, dy);
            }
            ToEngine::Key {
                down,
                key,
                code,
                modifiers,
                repeat: _,
            } => self.key(down, &key, &code, modifiers),
        }
    }

    /// Starts loading `url` as a new document, superseding any load in progress.
    fn load(&mut self, url: String) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let host_state = document_state(&self.host_state, &url);
        let _ = self.loader.send(Request {
            generation,
            url: url.clone(),
            host_state: host_state.clone(),
        });
        self.loading = Some((url, host_state));
        self.out.send(&FromEngine::Loading { loading: true });
    }

    /// Swaps in the document of the newest load, failed or not. The previous document's module
    /// and state are dropped here, which ends everything it had running.
    pub fn finish_load(&mut self, loaded: Loaded) {
        if loaded.generation != self.generation.load(Ordering::SeqCst) {
            return;
        }
        let Some((url, host_state)) = self.loading.take() else {
            return;
        };
        self.document_url = url;
        self.host_state = host_state;
        // A prompt the previous document raised no longer concerns anyone.
        self.host_state.permissions.lock().unwrap().pending = None;
        match loaded.result {
            Ok(live) => {
                self.live = live;
                let now = Instant::now();
                (self.last_tick, self.next_tick) = (now, now);
            }
            Err(message) => {
                self.live = None;
                self.out.send(&FromEngine::Error { message });
            }
        }
        self.presenter.reset();
        self.dirty = true;
        self.flush();
        self.out.send(&FromEngine::Loading { loading: false });
    }

    /// Runs a guest frame if one is due, then sends whatever changed.
    pub fn update(&mut self) {
        let now = Instant::now();
        let ticked = self.live.is_some() && now >= self.next_tick;
        if ticked {
            self.tick(now);
        }
        // Per-frame input is for the frame that just ran, or for nobody.
        if ticked || self.live.is_none() {
            let mut input = self.host_state.input_state.lock().unwrap();
            input.keys_pressed.clear();
            input.text.clear();
            input.mouse_buttons_clicked = [false; 3];
            input.scroll_x = 0.0;
            input.scroll_y = 0.0;
        }
        self.flush();
    }

    /// Runs one guest frame. A trap stops the module.
    fn tick(&mut self, now: Instant) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let dt_ms = (now - self.last_tick).as_millis().min(100) as u32;
        self.last_tick = now;
        // Keep the cadence, unless the guest fell behind it.
        self.next_tick += FRAME;
        if self.next_tick < now {
            self.next_tick = now + FRAME;
        }
        self.dirty = true;
        let Err(e) = live.tick(dt_ms) else {
            return;
        };
        let message = runtime::guest_error("on_frame", &e);
        self.host_state.log(ConsoleLevel::Error, message.clone());
        self.out.send(&FromEngine::Error { message });
        self.live = None;
    }

    /// Sends the draw list if it may have changed, and any changes to the URL, title, history,
    /// console and hover state.
    fn flush(&mut self) {
        if std::mem::take(&mut self.dirty) {
            let prompt = self.prompt_ops();
            let canvas = self.host_state.canvas.lock().unwrap();
            let out = &mut self.out;
            self.presenter
                .present(&canvas, self.zoom, &prompt, &mut |msg| out.send(msg));
            drop(canvas);
            // What is under the pointer may have changed too.
            self.update_hover();
        }
        self.sync_info();

        let lines = std::mem::take(&mut *self.host_state.console.lock().unwrap());
        for ConsoleEntry { level, message } in lines {
            let level = match level {
                ConsoleLevel::Log => 0,
                ConsoleLevel::Warn => 2,
                ConsoleLevel::Error => 3,
            };
            self.out.send(&FromEngine::Console { level, message });
        }
        let navigation = self.host_state.pending_navigation.lock().unwrap().take();
        if let Some(url) = navigation {
            self.out.send(&FromEngine::Open {
                url,
                new_tab: false,
            });
        }
    }

    /// Reports changes to the URL, title and history flags. While a load is in progress they
    /// describe the incoming document.
    fn sync_info(&mut self) {
        let state = self
            .loading
            .as_ref()
            .map_or(&self.host_state, |(_, state)| state);
        let url = state.current_url.lock().unwrap();
        if *url != self.shown.url {
            self.shown.url.clone_from(&url);
            self.out.send(&FromEngine::Url { url: url.clone() });
        }
        drop(url);
        let manifest = state.manifest.lock().unwrap();
        let title = manifest.as_ref().map_or("", |m| m.name.trim());
        if title != self.shown.title {
            self.shown.title = title.to_string();
            self.out.send(&FromEngine::Title {
                title: title.to_string(),
            });
        }
        drop(manifest);
        let history = {
            let nav = state.navigation.lock().unwrap();
            (nav.can_go_back(), nav.can_go_forward())
        };
        if history != self.shown.history {
            let (can_back, can_forward) = history;
            self.out.send(&FromEngine::History {
                can_back,
                can_forward,
            });
            self.shown.history = history;
        }
    }

    /// Sizes the canvases of the document on screen and the one loading to the viewport.
    fn layout(&self) {
        let (width, height, scale) = self.viewport;
        let size = |px: u32| (px as f32 / scale / self.zoom) as u32;
        let loading = self.loading.as_ref().map(|(_, state)| state);
        for state in std::iter::once(&self.host_state).chain(loading) {
            let mut canvas = state.canvas.lock().unwrap();
            canvas.width = size(width);
            canvas.height = size(height);
        }
    }

    /// The pointer position in canvas coordinates.
    fn canvas_pointer(&self) -> (f32, f32) {
        (self.pointer.0 / self.zoom, self.pointer.1 / self.zoom)
    }

    fn pointer_moved(&mut self, x: f32, y: f32) {
        let scale = self.viewport.2;
        self.pointer = (x / scale, y / scale);
        let (x, y) = self.canvas_pointer();
        {
            let mut input = self.host_state.input_state.lock().unwrap();
            input.mouse_x = x;
            input.mouse_y = y;
        }
        self.update_hover();
    }

    /// Shows the target of the link under the pointer, and a pointer cursor over links and the
    /// prompt's buttons.
    fn update_hover(&mut self) {
        let link = self.link_at_pointer();
        let cursor = if link.is_some() || self.prompt_button_at_pointer().is_some() {
            "pointer"
        } else {
            "default"
        };
        let status = link.unwrap_or_default();
        if status != self.shown.status {
            self.out.send(&FromEngine::Status {
                text: status.clone(),
            });
            self.shown.status = status;
        }
        if cursor != self.shown.cursor {
            self.out.send(&FromEngine::Cursor {
                name: cursor.into(),
            });
            self.shown.cursor = cursor;
        }
    }

    /// URL of the topmost guest hyperlink under the pointer.
    fn link_at_pointer(&self) -> Option<String> {
        if self.prompt_at_pointer(PROMPT) {
            return None;
        }
        let (x, y) = self.canvas_pointer();
        self.host_state
            .hyperlinks
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|link| x >= link.x && y >= link.y && x <= link.x + link.w && y <= link.y + link.h)
            .map(|link| link.url.clone())
    }

    fn mouse_button(&mut self, button: u8, down: bool, x: f32, y: f32) {
        self.pointer_moved(x, y);
        // The prompt answers left clicks on it; the guest doesn't see them.
        if button == 0 && (self.prompt_grab || (down && self.prompt_at_pointer(PROMPT))) {
            self.prompt_grab = down;
            if let Some(allow) = self.prompt_button_at_pointer().filter(|_| !down) {
                permissions::resolve_pending(&self.host_state.permissions, allow);
                self.dirty = true;
            }
            return;
        }
        // DOM button numbers to the guest's: 0 left, 1 right, 2 middle.
        let index = match button {
            0 => 0,
            2 => 1,
            1 => 2,
            _ => return,
        };
        {
            let mut input = self.host_state.input_state.lock().unwrap();
            input.mouse_buttons_down[index] = down;
            if !down {
                input.mouse_buttons_clicked[index] = true;
            }
        }
        // Clicking a link opens it; with the middle button, in a new tab.
        if button <= 1 {
            let link = self.link_at_pointer();
            if down {
                self.pressed_link = link;
            } else if let Some(url) = link.filter(|url| self.pressed_link.as_ref() == Some(url)) {
                self.out.send(&FromEngine::Open {
                    url,
                    new_tab: button == 1,
                });
            }
        }
    }

    /// Scrolls by a wheel delta in physical pixels with DOM signs (positive scrolls down).
    fn wheel(&mut self, dx: f32, dy: f32) {
        let scale = self.viewport.2 * self.zoom;
        let (dx, dy) = (dx / scale, dy / scale);
        let state = &self.host_state;
        {
            // The guest's delta is positive when scrolling up or left.
            let mut input = state.input_state.lock().unwrap();
            input.scroll_x -= dx;
            input.scroll_y -= dy;
        }
        let viewport = {
            let canvas = state.canvas.lock().unwrap();
            (canvas.width, canvas.height)
        };
        let mut scroll = state.scroll.lock().unwrap();
        let (x, y) = (scroll.x + dx, scroll.y + dy);
        scroll.set(x, y, viewport);
    }

    fn key(&mut self, down: bool, key: &str, code: &str, modifiers: u8) {
        {
            let mut input = self.host_state.input_state.lock().unwrap();
            input.modifiers_shift = modifiers & MOD_SHIFT != 0;
            input.modifiers_ctrl = modifiers & (MOD_CTRL | MOD_META) != 0;
            input.modifiers_alt = modifiers & MOD_ALT != 0;
        }
        // Enter / Escape answer the permission prompt; the guest never sees them.
        if down && modifiers == 0 && self.prompt_showing() {
            let allow = match key {
                "Enter" => Some(true),
                "Escape" => Some(false),
                _ => None,
            };
            if let Some(allow) = allow {
                permissions::resolve_pending(&self.host_state.permissions, allow);
                self.dirty = true;
                return;
            }
        }
        let code = keys::key_code(key, code);
        let mut input = self.host_state.input_state.lock().unwrap();
        if down {
            if let Some(code) = code {
                if !input.keys_down.contains(&code) {
                    input.keys_down.push(code);
                }
                input.keys_pressed.push(code);
            }
            if let Some(text) = keys::typed_text(key, modifiers) {
                input.text.push_str(text);
            }
        } else if let Some(code) = code {
            input.keys_down.retain(|&held| held != code);
        }
    }

    /// Whether a permission request is waiting for the user.
    fn prompt_showing(&self) -> bool {
        self.host_state
            .permissions
            .lock()
            .unwrap()
            .pending
            .is_some()
    }

    /// Whether the prompt is showing and the pointer is in `rect` of it.
    fn prompt_at_pointer(&self, [x, y, w, h]: Rect) -> bool {
        let (px, py) = self.pointer;
        (x..=x + w).contains(&px) && (y..=y + h).contains(&py) && self.prompt_showing()
    }

    /// The decision of the prompt button under the pointer.
    fn prompt_button_at_pointer(&self) -> Option<bool> {
        if self.prompt_at_pointer(PROMPT_ALLOW) {
            Some(true)
        } else if self.prompt_at_pointer(PROMPT_BLOCK) {
            Some(false)
        } else {
            None
        }
    }

    /// The permission prompt, drawn over the page's top-left corner while a request waits.
    fn prompt_ops(&self) -> Vec<DrawOp> {
        let request = self.host_state.permissions.lock().unwrap().pending.clone();
        let Some(request) = request else {
            return Vec::new();
        };
        let [x, y, w, h] = PROMPT;
        let button = |[bx, by, bw, bh]: Rect, fill, fg, weight, text: &str| {
            [
                rect([bx, by, bw, bh], 4.0, fill),
                label(
                    bx + bw / 2.0,
                    by + (bh - 16.8) / 2.0,
                    14.0,
                    fg,
                    weight,
                    1,
                    text,
                ),
            ]
        };
        let mut ops = vec![
            rect([x - 1.0, y - 1.0, w + 2.0, h + 2.0], 7.0, 0x3f3f46ff),
            rect(PROMPT, 6.0, 0x18181bff),
            DrawOp::PushClip { x, y, w, h },
            label(
                x + 12.0,
                y + 12.0,
                12.0,
                0xa1a1aaff,
                400,
                0,
                &request.origin,
            ),
            label(
                x + 12.0,
                y + 32.0,
                14.0,
                0xfafafaff,
                400,
                0,
                &format!("wants to: {}", request.kind.description()),
            ),
        ];
        ops.extend(button(PROMPT_BLOCK, 0x27272aff, 0xfafafaff, 400, "Block"));
        ops.extend(button(PROMPT_ALLOW, 0xfafafaff, 0x18181bff, 600, "Allow"));
        ops.push(label(
            x + 12.0,
            y + 92.0,
            11.0,
            0x71717aff,
            400,
            0,
            "Enter to allow \u{00b7} Esc to block",
        ));
        ops.push(DrawOp::PopClip);
        ops
    }
}

/// The protocol stream, and whether the browser stopped reading it.
struct Out {
    stream: Output,
    closed: bool,
}

impl Out {
    fn send(&mut self, msg: &FromEngine) {
        if self.closed {
            return;
        }
        let mut stream = self.stream.lock().unwrap_or_else(PoisonError::into_inner);
        match msg.write_to(&mut *stream) {
            Ok(()) => {}
            // Too large for the protocol (a huge image, say); nothing was written.
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                eprintln!("sig-wasm: dropped a message: {e}");
            }
            Err(_) => self.closed = true,
        }
    }
}

/// `[x, y, w, h]` in logical pixels.
type Rect = [f32; 4];

/// The permission prompt's card and buttons.
const PROMPT: Rect = [8.0, 8.0, 320.0, 116.0];
const PROMPT_BLOCK: Rect = [180.0, 64.0, 64.0, 28.0];
const PROMPT_ALLOW: Rect = [252.0, 64.0, 64.0, 28.0];

fn rect([x, y, w, h]: Rect, radius: f32, color: u32) -> DrawOp {
    DrawOp::Rect {
        x,
        y,
        w,
        h,
        radius,
        color,
    }
}

fn label(x: f32, y: f32, size: f32, color: u32, weight: u16, align: u8, text: &str) -> DrawOp {
    DrawOp::Text {
        x,
        y,
        size,
        color,
        family: String::new(),
        weight,
        italic: false,
        align,
        text: text.into(),
    }
}

/// Starts the loader thread. It runs one load at a time and skips requests that a newer
/// navigation superseded while they were queued.
fn spawn_loader(
    engine: WasmEngine,
    generation: Arc<AtomicU64>,
    results: mpsc::Sender<Msg>,
) -> anyhow::Result<mpsc::Sender<Request>> {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name(LOADER_THREAD.into())
        .spawn(move || {
            for request in rx {
                if request.generation != generation.load(Ordering::SeqCst) {
                    continue;
                }
                let result = runtime::load(&engine, &request.host_state, &request.url)
                    .map_err(|e| format!("{e:#}"));
                let loaded = Loaded {
                    generation: request.generation,
                    result,
                };
                if results.send(Msg::Loaded(Box::new(loaded))).is_err() {
                    break;
                }
            }
        })
        .context("cannot start the loader thread")?;
    Ok(tx)
}

/// Guest state for a new document at `url`, derived from the page's current one. Everything
/// the previous document's module could start (timers, event listeners, connections, audio,
/// workers, canvas, console, scroll) begins empty, so none of it outlives that document. The
/// page-level parts carry over: the module loader, input, canvas size, focus and permission
/// decisions, plus the session storage when `url` has the same origin.
fn document_state(page: &HostState, url: &str) -> HostState {
    let mut state = HostState {
        engine: page.engine.clone(),
        permissions: page.permissions.clone(),
        input_state: page.input_state.clone(),
        focused: page.focused.clone(),
        ..Default::default()
    };
    {
        let viewport = page.canvas.lock().unwrap();
        let mut canvas = state.canvas.lock().unwrap();
        canvas.width = viewport.width;
        canvas.height = viewport.height;
    }
    let origin = sighurt_engine_wasm::url::app_origin_of(url);
    if *page.module_origin.lock().unwrap() == origin {
        state.storage = page.storage.clone();
        *state.module_origin.lock().unwrap() = origin;
    }
    let mut nav = state.navigation.lock().unwrap();
    *nav = NavigationStack::new();
    nav.push(HistoryEntry::new(url));
    drop(nav);
    *state.current_url.lock().unwrap() = url.to_string();
    state
}

#[cfg(test)]
mod tests {
    use super::*;
    use sighurt_engine_wasm::capabilities::{set_module_origin, TimerEntry};

    fn push_state(host_state: &HostState, url: &str) {
        host_state
            .navigation
            .lock()
            .unwrap()
            .push(HistoryEntry::new(url));
        *host_state.current_url.lock().unwrap() = url.to_string();
    }

    fn current_url(host_state: &HostState) -> String {
        host_state.current_url.lock().unwrap().clone()
    }

    #[test]
    fn a_new_document_has_no_history() {
        let hs = document_state(&HostState::default(), "https://a.test/app.wasm");
        let nav = hs.navigation.lock().unwrap();
        assert!(!nav.can_go_back());
        assert!(!nav.can_go_forward());
        assert_eq!(nav.len(), 1);
        drop(nav);
        assert_eq!(current_url(&hs), "https://a.test/app.wasm");
        assert!(!traverse(&hs, false));
    }

    #[test]
    fn traverse_walks_push_state_entries() {
        let hs = document_state(&HostState::default(), "https://a.test/app.wasm");
        push_state(&hs, "https://a.test/about");
        push_state(&hs, "https://a.test/contact");

        assert!(traverse(&hs, false));
        assert_eq!(current_url(&hs), "https://a.test/about");
        assert!(traverse(&hs, false));
        assert_eq!(current_url(&hs), "https://a.test/app.wasm");
        assert!(!traverse(&hs, false));

        assert!(traverse(&hs, true));
        assert_eq!(current_url(&hs), "https://a.test/about");
        assert!(hs.navigation.lock().unwrap().can_go_forward());
    }

    #[test]
    fn a_new_document_does_not_inherit_guest_state() {
        let page = HostState::default();
        set_module_origin(&page, "https://a.test/app.wasm");
        page.timers.lock().unwrap().push(TimerEntry {
            id: 1,
            fire_at: Instant::now(),
            interval: Some(Duration::from_millis(10)),
            callback_id: 7,
        });
        page.storage.lock().unwrap().insert("k".into(), "v".into());
        push_state(&page, "https://a.test/about");
        {
            let mut canvas = page.canvas.lock().unwrap();
            canvas.width = 640;
            canvas.height = 480;
        }

        let doc = document_state(&page, "https://b.test/other.wasm");
        assert!(doc.timers.lock().unwrap().is_empty());
        assert!(doc.storage.lock().unwrap().is_empty());
        assert!(!Arc::ptr_eq(&doc.events, &page.events));
        let canvas = doc.canvas.lock().unwrap();
        assert_eq!((canvas.width, canvas.height), (640, 480));
        assert!(canvas.commands.is_empty());
        drop(canvas);
        assert!(Arc::ptr_eq(&doc.permissions, &page.permissions));
        assert!(Arc::ptr_eq(&doc.input_state, &page.input_state));
        assert!(Arc::ptr_eq(&doc.focused, &page.focused));
        assert_eq!(current_url(&doc), "https://b.test/other.wasm");
        assert_eq!(doc.navigation.lock().unwrap().len(), 1);
    }

    #[test]
    fn session_storage_survives_a_same_origin_document() {
        let page = HostState::default();
        set_module_origin(&page, "https://a.test/app.wasm");
        page.storage.lock().unwrap().insert("k".into(), "v".into());

        let doc = document_state(&page, "https://a.test/app.wasm");
        // The loader sets the origin again before the module runs; it must not clear anything.
        set_module_origin(&doc, "https://a.test/app.wasm");
        assert_eq!(
            doc.storage.lock().unwrap().get("k").map(String::as_str),
            Some("v")
        );
    }
}
