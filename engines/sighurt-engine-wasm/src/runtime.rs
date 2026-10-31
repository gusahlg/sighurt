//! A guest's lifecycle: loading it, and running its callbacks one step at a time.
//!
//! [`load`] fetches a `.wasm` binary (HTTP/HTTPS or `file://`), compiles it under the sandbox
//! policy, links the `oxide` import module (memory, host capabilities) and runs `start_app()`.
//! A guest that exports `on_frame(dt_ms: u32)` becomes a [`Guest`], which runs one
//! [`step`](Guest::step) per frame: queued events to `on_event`, animation frames and expired
//! timers to `on_timer`, then `on_frame`.
//!
//! Both run on the runtime's worker threads, never on the owner thread, and one step at a time
//! per guest, so a guest's calls stay in order. A step does a bounded amount of work: each
//! callback has its fuel and time limit (see [`crate::engine`]), and past [`STEP_BUDGET`] or
//! [`MAX_CALLBACKS_PER_STEP`] the remaining events and timers wait for the next step, so a
//! flood of individually small callbacks cannot hold a worker indefinitely. A load or step of a
//! document that was closed or replaced stops at the next epoch tick, its store's budget
//! cancelled.
//!
//! Owns: calling into guests. Must not own: when steps run (the adapter schedules them) or what
//! the browser is told about them.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use wasmtime::*;

use crate::capabilities::{
    http_get, next_animation_frame, next_due_timer, register_host_functions, ConsoleLevel,
    HostState,
};
use crate::draw::{self, DisplayList};
use crate::engine::{Halt, WasmEngine};
use crate::events::{drain_pending_events, set_current_event};
use crate::url::AppUrl;

/// How long a step keeps delivering events and timers before it leaves the rest for the next
/// step. `on_frame` still runs.
pub const STEP_BUDGET: Duration = Duration::from_millis(50);
/// Most event and timer callbacks in one step.
pub const MAX_CALLBACKS_PER_STEP: usize = 1000;

/// A guest that exports `on_frame`: its [`Store`], its callbacks, and what it last drew.
///
/// The store holds [`HostState`] (canvas, console, timers, animation requests, etc.) for the
/// lifetime of the document.
pub struct Guest {
    engine: WasmEngine,
    store: Store<HostState>,
    on_frame_fn: TypedFunc<u32, ()>,
    on_timer_fn: Option<TypedFunc<u32, ()>>,
    on_event_fn: Option<TypedFunc<u32, ()>>,
    /// Events taken from the event queue that a step had no budget left to deliver.
    events: VecDeque<(u32, String, Vec<u8>)>,
    /// The display list last handed out, to tell whether the next frame differs.
    drawn: Option<Arc<DisplayList>>,
}

/// Input the owner collected since the last step, for the next one.
#[derive(Debug, Default)]
pub struct FrameInput {
    pub keys_pressed: Vec<u32>,
    pub text: String,
    pub clicked: [bool; 3],
    /// Positive up and left, as the guest reads it.
    pub scroll: (f32, f32),
}

/// A loaded document: its guest if it runs frames, and what `start_app` drew.
pub struct Loaded {
    pub guest: Option<Box<Guest>>,
    pub list: Option<Arc<DisplayList>>,
}

impl Guest {
    /// Runs one step with `input`: queued events to `on_event`, animation frames and expired
    /// timers to `on_timer`, then `on_frame(dt_ms)`. Returns the new display list if the frame
    /// drew something different from the last one.
    ///
    /// A failing event or timer callback is logged to the console; a failing `on_frame` is
    /// returned, and ends the document.
    pub fn step(&mut self, dt_ms: u32, input: FrameInput) -> Result<Option<Arc<DisplayList>>> {
        {
            let mut state = self.store.data().input_state.lock().unwrap();
            state.keys_pressed = input.keys_pressed;
            state.text = input.text;
            state.mouse_buttons_clicked = input.clicked;
            (state.scroll_x, state.scroll_y) = input.scroll;
        }
        let started = Instant::now();
        let mut callbacks = 0;
        let mut budget_left = || {
            callbacks += 1;
            callbacks <= MAX_CALLBACKS_PER_STEP && started.elapsed() < STEP_BUDGET
        };

        // Events first: the guest reacts to resize, input and custom events before it draws.
        if let Some(on_event) = self.on_event_fn.clone() {
            if self.events.is_empty() {
                let data = self.store.data();
                let canvas_size = {
                    let c = data.canvas.lock().unwrap();
                    (c.width, c.height)
                };
                let focused = data.focused.load(Ordering::Relaxed);
                let (mouse_down, mouse_pos) = {
                    let i = data.input_state.lock().unwrap();
                    (i.mouse_buttons_down[0], (i.mouse_x, i.mouse_y))
                };
                self.events = drain_pending_events(
                    &data.events,
                    canvas_size,
                    focused,
                    mouse_down,
                    mouse_pos,
                    MAX_CALLBACKS_PER_STEP,
                )
                .into();
            }
            while let Some((callback_id, evt_type, evt_data)) = self.events.pop_front() {
                if !budget_left() {
                    self.events.push_front((callback_id, evt_type, evt_data));
                    break;
                }
                set_current_event(&self.store.data().events, evt_type.clone(), evt_data);
                self.call_logged(&on_event, callback_id, || {
                    format!("on_event({evt_type}:{callback_id})")
                });
            }
        }

        if let Some(on_timer) = self.on_timer_fn.clone() {
            // Animation frames first (one-shot, requested before this step), then timers that
            // were due when the step began.
            let data = self.store.data();
            let mut frames = data.animation_requests.lock().unwrap().len();
            let (requests, timers) = (data.animation_requests.clone(), data.timers.clone());
            while frames > 0 && budget_left() {
                frames -= 1;
                let Some(callback_id) = next_animation_frame(&requests) else {
                    break;
                };
                self.call_logged(&on_timer, callback_id, || {
                    format!("on_timer(raf:{callback_id})")
                });
            }
            while budget_left() {
                let Some(callback_id) = next_due_timer(&timers, started) else {
                    break;
                };
                self.call_logged(&on_timer, callback_id, || {
                    format!("on_timer({callback_id})")
                });
            }
        }

        let on_frame = self.on_frame_fn.clone();
        self.engine
            .call(&mut self.store, |store| on_frame.call(store, dt_ms))
            .map_err(|e| anyhow::anyhow!(guest_error("on_frame", &e)))?;
        Ok(self.redrawn())
    }

    /// Calls a callback export, logging a trap to the console as a failure of `what`.
    fn call_logged(&mut self, f: &TypedFunc<u32, ()>, arg: u32, what: impl FnOnce() -> String) {
        if let Err(e) = self
            .engine
            .call(&mut self.store, |store| f.call(store, arg))
        {
            self.store
                .data()
                .log(ConsoleLevel::Error, guest_error(&what(), &e));
        }
    }

    /// The canvas as a new display list, if it changed since the last one.
    fn redrawn(&mut self) -> Option<Arc<DisplayList>> {
        let list = {
            let canvas = self.store.data().canvas.lock().unwrap();
            draw::display_list(&canvas, self.drawn.as_deref())?
        };
        let list = Arc::new(list);
        self.drawn = Some(list.clone());
        Some(list)
    }

    /// The state the guest's host functions use.
    pub fn host_state(&self) -> &HostState {
        self.store.data()
    }
}

/// Describes a failed guest call to `what`: that it ran out of fuel or time, was cancelled, or
/// how it trapped.
pub fn guest_error(what: &str, e: &anyhow::Error) -> String {
    if let Some(halt) = e.downcast_ref::<Halt>() {
        return format!("{what} halted: {halt}");
    }
    match e.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel) => format!("{what} halted: fuel limit exceeded"),
        Some(Trap::Interrupt) => format!("{what} halted: time limit exceeded"),
        // `{:#}` includes the trap itself, not just the outermost context.
        _ => format!("{what} trapped: {e:#}"),
    }
}

/// Instantiates `module` with the host API and bounded memory, in a new store holding
/// `host_state`. The state's `memory` is the module's own exported memory if it has one, or
/// else the `oxide.memory` the host provides. The module's `start` function, if it has one,
/// runs with the budget of `start_app`.
pub fn instantiate(
    engine: &WasmEngine,
    host_state: HostState,
    module: &Module,
) -> Result<(Store<HostState>, Instance)> {
    let mut linker = Linker::new(engine.engine());
    register_host_functions(&mut linker)?;
    let mut store = engine.create_store(host_state)?;
    let memory = engine.create_bounded_memory(&mut store)?;
    linker.define(&store, "oxide", "memory", memory)?;
    store.data_mut().memory = Some(memory);
    let instance = engine
        .start(&mut store, |store| linker.instantiate(store, module))
        .map_err(|e| {
            if e.downcast_ref::<Halt>().is_some() || e.downcast_ref::<Trap>().is_some() {
                anyhow::anyhow!(guest_error("the module's start function", &e))
            } else {
                e.context("failed to instantiate wasm module")
            }
        })?;
    if let Some(own) = instance.get_memory(&mut store, "memory") {
        store.data_mut().memory = Some(own);
    }
    Ok((store, instance))
}

/// Loads the app at `url` into `host_state`, the state of a new document.
///
/// Fetches the module and its optional sibling manifest (`app.wasm` → `app.toml`), then
/// compiles and instantiates it and runs `start_app()`, all on the calling thread (a worker).
/// Supports `http`/`https` and `file://` URLs; `url` is fetched as given. The document's origin
/// is `url`'s, whatever URL its history shows. Once `host_state`'s budget is cancelled (the
/// navigation was superseded, stopped or closed) it gives up between these steps, and guest
/// code stops at the next epoch tick.
pub fn load(engine: &WasmEngine, host_state: &HostState, url: &str) -> Result<Loaded> {
    let check = || {
        anyhow::ensure!(!host_state.budget.is_cancelled(), "cancelled");
        Ok(())
    };
    let parsed = AppUrl::parse(url).map_err(|e| anyhow::anyhow!("{e}"))?;
    check()?;
    // Fetching waits on the network: other loads go on meanwhile.
    let wasm_bytes = crate::pool::blocking(|| fetch_module(url, url))?;

    // Optional sibling manifest: metadata + declared permissions.
    match crate::pool::blocking(|| crate::manifest::fetch_manifest(&parsed)) {
        Ok(manifest) => *host_state.manifest.lock().unwrap() = manifest,
        Err(e) => host_state.log(
            ConsoleLevel::Warn,
            format!("[MANIFEST] {e} — loading app without a manifest"),
        ),
    }

    // Capture the app origin for storage/permission scoping, from the module's own URL: not
    // from the entry its history restores, and once per load so guest `push_state` calls
    // (which mutate `current_url`) can't shift the origin afterwards.
    crate::capabilities::set_module_origin(host_state, url);

    check()?;
    let module = engine.compile_module(&wasm_bytes)?;
    let (mut store, instance) = instantiate(engine, host_state.clone(), &module)?;
    let start_app = instance
        .get_typed_func::<(), ()>(&mut store, "start_app")
        .context("module must export `start_app` as extern \"C\" fn()")?;
    check()?;
    if let Err(e) = engine.start(&mut store, |store| start_app.call(store, ())) {
        anyhow::bail!(guest_error("start_app", &e));
    }
    let mut list = {
        let canvas = host_state.canvas.lock().unwrap();
        draw::display_list(&canvas, None).map(Arc::new)
    };
    let Ok(on_frame_fn) = instance.get_typed_func::<u32, ()>(&mut store, "on_frame") else {
        return Ok(Loaded { guest: None, list });
    };
    // start_app drew nothing. This guest paints from on_frame, so the empty list is not a
    // frame yet: publishing it makes FrameReady fire before the first draw, and a present in
    // that window fills the buffer with transparent pixels. The next step's on_frame is the
    // first frame, including one that deliberately draws nothing (`drawn` stays `None`, so an
    // empty canvas still differs from "no frame"). A start_app that did draw is a real frame
    // and is published now.
    if list.as_ref().is_some_and(|list| list.ops.is_empty()) {
        list = None;
    }
    let on_timer_fn = instance
        .get_typed_func::<u32, ()>(&mut store, "on_timer")
        .ok();
    let on_event_fn = instance
        .get_typed_func::<u32, ()>(&mut store, "on_event")
        .ok();
    let guest = Guest {
        engine: engine.clone(),
        store,
        on_frame_fn,
        on_timer_fn,
        on_event_fn,
        events: VecDeque::new(),
        drawn: list.clone(),
    };
    Ok(Loaded {
        guest: Some(Box::new(guest)),
        list,
    })
}

/// Largest `.wasm` module that is fetched or read.
const MAX_WASM_MODULE_SIZE: u64 = 50 * 1024 * 1024; // 50 MB

/// Fetches the `.wasm` module at `url` for the page at `page_url`: the page itself, a module it
/// loads with `api_load_module`, or a worker. `http(s)` modules must come with a wasm (or
/// untyped) `Content-Type`. `file://` modules are read only for pages that are local files
/// themselves, so pages from the web can't read the disk. Either way, modules are at most
/// [`MAX_WASM_MODULE_SIZE`] bytes.
pub fn fetch_module(url: &str, page_url: &str) -> Result<Vec<u8>> {
    use std::io::Read;
    let parsed = AppUrl::parse(url).map_err(|e| anyhow::anyhow!("{e}"))?;
    let page_is_local = AppUrl::parse(page_url).is_ok_and(|page| page.is_local_file());
    let bytes = if parsed.is_fetchable() {
        let (bytes, content_type) =
            http_get(parsed.as_str(), "application/wasm", MAX_WASM_MODULE_SIZE)
                .map_err(anyhow::Error::msg)?;
        let content_type = content_type.unwrap_or_default();
        anyhow::ensure!(
            content_type.is_empty()
                || content_type.contains("application/wasm")
                || content_type.contains("application/octet-stream"),
            "unexpected Content-Type for .wasm module: {content_type}"
        );
        bytes
    } else if parsed.is_local_file() && page_is_local {
        let path = parsed
            .to_file_path()
            .ok_or_else(|| anyhow::anyhow!("cannot convert file URL to path: {url}"))?;
        let mut bytes = Vec::new();
        std::fs::File::open(&path)
            .and_then(|file| file.take(MAX_WASM_MODULE_SIZE + 1).read_to_end(&mut bytes))
            .with_context(|| format!("failed to read local file: {}", path.display()))?;
        anyhow::ensure!(
            (bytes.len() as u64) <= MAX_WASM_MODULE_SIZE,
            "module exceeds size limit ({MAX_WASM_MODULE_SIZE} bytes)"
        );
        bytes
    } else if parsed.is_local_file() {
        anyhow::bail!("only local pages can load local modules: {url}");
    } else if parsed.is_internal() {
        anyhow::bail!("sig:// pages are served by the browser, not the Wasm engine");
    } else {
        anyhow::bail!("unsupported URL scheme: {}", parsed.scheme());
    };
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_pages_load_local_modules() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("worker.wasm");
        std::fs::write(&path, b"\0asm").unwrap();
        let module = url::Url::from_file_path(&path).unwrap().to_string();
        let page = url::Url::from_file_path(dir.path().join("app.wasm")).unwrap();
        assert_eq!(fetch_module(&module, page.as_str()).unwrap(), b"\0asm");
        assert_eq!(fetch_module(&module, &module).unwrap(), b"\0asm");
        let err = fetch_module(&module, "https://example.com/app.wasm").unwrap_err();
        assert!(err.to_string().contains("only local pages"), "{err}");
        assert!(fetch_module("sig://home", page.as_str()).is_err());
    }
}
