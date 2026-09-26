//! Guest WebAssembly lifecycle for the WASM engine.
//!
//! [`load`] fetches a `.wasm` binary (HTTP/HTTPS or `file://`), compiles it with Wasmtime under
//! the sandbox policy, links the `oxide` import module (memory, host capabilities) and runs
//! `start_app()`. After that, interactive guests export `on_frame(dt_ms: u32)` for a per-frame
//! render loop driven through [`LiveModule::tick`]; optional `on_timer(callback_id: u32)`
//! callbacks run when timers expire, immediately before each frame.

use anyhow::{Context, Result};
use wasmtime::*;

use crate::capabilities::{
    drain_animation_frame_requests, drain_expired_timers, http_get, register_host_functions,
    ConsoleLevel, HostState,
};
use crate::engine::WasmEngine;
use crate::events::{drain_pending_events, set_current_event};
use crate::url::AppUrl;

const FRAME_FUEL_LIMIT: u64 = 50_000_000;

/// A Wasmtime [`Store`] and typed guest exports kept alive across frames for interactive apps.
///
/// Constructed when the module exports `on_frame`. The store holds [`HostState`] (canvas, console,
/// timers, animation requests, etc.) for the lifetime of the document.
pub struct LiveModule {
    store: Store<HostState>,
    on_frame_fn: TypedFunc<u32, ()>,
    on_timer_fn: Option<TypedFunc<u32, ()>>,
    on_event_fn: Option<TypedFunc<u32, ()>>,
}

impl LiveModule {
    /// Advances one frame: delivers queued events to `on_event`, animation frame requests and
    /// expired timers to `on_timer`, then calls `on_frame(dt_ms)`.
    ///
    /// Every call gets a fresh frame's worth of fuel. A failing event or timer callback is
    /// logged to the console; a failing `on_frame` is returned.
    pub fn tick(&mut self, dt_ms: u32) -> Result<()> {
        let store = &mut self.store;
        // Events first: the guest reacts to resize, input and custom events before it draws.
        if let Some(on_event) = &self.on_event_fn {
            let data = store.data();
            let canvas_size = {
                let c = data.canvas.lock().unwrap();
                (c.width, c.height)
            };
            let focused = data.focused.load(std::sync::atomic::Ordering::Relaxed);
            let (mouse_down, mouse_pos) = {
                let i = data.input_state.lock().unwrap();
                (i.mouse_buttons_down[0], (i.mouse_x, i.mouse_y))
            };
            let events = data.events.clone();
            let pending =
                drain_pending_events(&events, canvas_size, focused, mouse_down, mouse_pos);
            for (callback_id, evt_type, evt_data) in pending {
                set_current_event(&events, evt_type.clone(), evt_data);
                call_logged(store, on_event, callback_id, || {
                    format!("on_event({evt_type}:{callback_id})")
                })?;
            }
        }

        if let Some(on_timer) = &self.on_timer_fn {
            // Animation frames first (vsync-aligned, one-shot), then regular timers.
            let animation_frames = drain_animation_frame_requests(&store.data().animation_requests);
            for callback_id in animation_frames {
                call_logged(store, on_timer, callback_id, || {
                    format!("on_timer(raf:{callback_id})")
                })?;
            }
            let timers = drain_expired_timers(&store.data().timers);
            for callback_id in timers {
                call_logged(store, on_timer, callback_id, || {
                    format!("on_timer({callback_id})")
                })?;
            }
        }

        store.set_fuel(FRAME_FUEL_LIMIT)?;
        self.on_frame_fn.call(store, dt_ms)
    }
}

/// Calls a callback export with a frame's worth of fuel, logging a trap to the console as a
/// failure of `what`.
fn call_logged(
    store: &mut Store<HostState>,
    f: &TypedFunc<u32, ()>,
    arg: u32,
    what: impl FnOnce() -> String,
) -> Result<()> {
    store.set_fuel(FRAME_FUEL_LIMIT)?;
    if let Err(e) = f.call(&mut *store, arg) {
        store
            .data()
            .log(ConsoleLevel::Error, guest_error(&what(), &e));
    }
    Ok(())
}

/// Describes a failed guest call to `what`: that it ran out of fuel, or how it trapped.
pub fn guest_error(what: &str, e: &anyhow::Error) -> String {
    if e.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
        format!("{what} halted: fuel limit exceeded")
    } else {
        // `{:#}` includes the trap itself, not just the outermost context.
        format!("{what} trapped: {e:#}")
    }
}

/// Instantiates `module` with the host API, bounded memory and a full tank of fuel, in a new
/// store holding `host_state`. The state's `memory` is the module's own exported memory if it
/// has one, or else the `oxide.memory` the host provides.
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
    let instance = linker
        .instantiate(&mut store, module)
        .context("failed to instantiate wasm module")?;
    if let Some(own) = instance.get_memory(&mut store, "memory") {
        store.data_mut().memory = Some(own);
    }
    Ok((store, instance))
}

/// Loads the app at `url` into `host_state`, the state of a new document.
///
/// Fetches the module and its optional sibling manifest (`app.wasm` → `app.toml`), then
/// compiles and instantiates it and runs `start_app()`, all on the calling thread (the page's
/// loader). Supports `http`/`https` and `file://` URLs; `url` is fetched as given. Returns a
/// [`LiveModule`] if the guest exports `on_frame`.
pub fn load(engine: &WasmEngine, host_state: &HostState, url: &str) -> Result<Option<LiveModule>> {
    let parsed = AppUrl::parse(url).map_err(|e| anyhow::anyhow!("{e}"))?;
    let wasm_bytes = fetch_module(url, url)?;

    // Optional sibling manifest: metadata + declared permissions.
    match crate::manifest::fetch_manifest(&parsed) {
        Ok(manifest) => *host_state.manifest.lock().unwrap() = manifest,
        Err(e) => host_state.log(
            ConsoleLevel::Warn,
            format!("[MANIFEST] {e} — loading app without a manifest"),
        ),
    }

    // Capture the app origin for storage/permission scoping. Done once per load so guest
    // `push_state` calls (which mutate `current_url`) can't shift the origin afterwards.
    let url = host_state.current_url.lock().unwrap().clone();
    crate::capabilities::set_module_origin(host_state, &url);

    let module = engine.compile_module(&wasm_bytes)?;
    let (mut store, instance) = instantiate(engine, host_state.clone(), &module)?;
    let start_app = instance
        .get_typed_func::<(), ()>(&mut store, "start_app")
        .context("module must export `start_app` as extern \"C\" fn()")?;
    if let Err(e) = start_app.call(&mut store, ()) {
        anyhow::bail!(guest_error("start_app", &e));
    }
    let Ok(on_frame_fn) = instance.get_typed_func::<u32, ()>(&mut store, "on_frame") else {
        return Ok(None);
    };
    let on_timer_fn = instance
        .get_typed_func::<u32, ()>(&mut store, "on_timer")
        .ok();
    let on_event_fn = instance
        .get_typed_func::<u32, ()>(&mut store, "on_event")
        .ok();
    Ok(Some(LiveModule {
        store,
        on_frame_fn,
        on_timer_fn,
        on_event_fn,
    }))
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
        anyhow::bail!("sig:// pages are served by the shell, not the WASM engine");
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
