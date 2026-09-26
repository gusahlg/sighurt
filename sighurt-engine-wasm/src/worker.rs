//! Background WebAssembly workers for Oxide guest modules.
//!
//! A worker is a separate `.wasm` module instantiated on its own OS thread with
//! its own Wasmtime [`Store`], fuel budget, and linear memory. It never shares
//! Rust types or memory with the spawning guest — communication is pure message
//! passing of byte slices, mirroring the rest of the FFI boundary.
//!
//! The spawning ("parent") guest calls:
//! - `api_spawn_worker(url)` → handle
//! - `api_worker_post_message(handle, bytes)` — parent → worker inbox
//! - `api_worker_recv(handle)` — drain one message from the worker's outbox
//! - `api_worker_terminate(handle)`
//!
//! The worker module exports `start_app()` (run once on spawn) and optionally
//! `on_message(len)` (run for each inbound message). Inside `on_message` it reads
//! the payload with `api_worker_message_read` and replies with `api_worker_post`.
//!
//! All of this is driven by polling from the guest's frame loop, matching the
//! WebSocket / fetch / RTC subsystems.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    guest_bytes, guest_str, next_handle, with, write_prefix, ConsoleLevel, HostState,
};
use crate::engine::{compile_cached, WasmEngine};

/// Message handed to a worker thread over its inbox channel.
enum Inbox {
    /// A byte payload to deliver to the worker's `on_message` export.
    Message(Vec<u8>),
    /// Request the worker thread to stop.
    Terminate,
}

/// Parent-side handle to a single running worker.
struct Worker {
    /// Sender for the worker's inbox (parent → worker).
    inbox_tx: Sender<Inbox>,
    /// Messages the worker posted back (worker → parent), drained by `api_worker_recv`.
    outbox: Arc<Mutex<VecDeque<Vec<u8>>>>,
    /// Cleared on terminate so the worker loop exits after its current callback.
    alive: Arc<AtomicBool>,
}

/// Registry of workers spawned by one guest. Created on first spawn.
#[derive(Default)]
pub struct WorkerState {
    workers: HashMap<u32, Worker>,
    last_id: u32,
}

impl WorkerState {
    /// Spawn a worker for `url`, deriving its sandbox state from `parent`.
    ///
    /// Returns a handle (`> 0`) or `0` if there is no engine to run it on. The worker shares
    /// only the console and the persistent KV store of the parent's origin; canvas, input,
    /// timers, and memory are its own.
    fn spawn(&mut self, url: String, parent: &HostState) -> u32 {
        let Some(engine) = parent.engine.clone() else {
            return 0;
        };
        let (inbox_tx, inbox_rx) = std::sync::mpsc::channel::<Inbox>();
        let outbox: Arc<Mutex<VecDeque<Vec<u8>>>> = Default::default();
        let alive = Arc::new(AtomicBool::new(true));
        let child = HostState {
            engine: Some(engine.clone()),
            console: parent.console.clone(),
            current_url: Arc::new(Mutex::new(url.clone())),
            module_origin: Arc::new(Mutex::new(parent.module_origin.lock().unwrap().clone())),
            worker_outbox: Some(outbox.clone()),
            ..Default::default()
        };
        let page_url = parent.current_url.lock().unwrap().clone();
        let alive_thread = alive.clone();
        std::thread::spawn(move || {
            let result = worker_main(
                &url,
                &page_url,
                &engine,
                child.clone(),
                inbox_rx,
                &alive_thread,
            );
            if let Err(message) = result {
                child.log(ConsoleLevel::Error, format!("[WORKER] {message}"));
            }
        });

        let id = next_handle(&mut self.last_id);
        let worker = Worker {
            inbox_tx,
            outbox,
            alive,
        };
        self.workers.insert(id, worker);
        id
    }

    fn post(&self, id: u32, data: Vec<u8>) -> bool {
        self.workers
            .get(&id)
            .is_some_and(|w| w.inbox_tx.send(Inbox::Message(data)).is_ok())
    }

    fn recv(&self, id: u32) -> Option<Vec<u8>> {
        self.workers.get(&id)?.outbox.lock().unwrap().pop_front()
    }

    fn terminate(&mut self, id: u32) -> bool {
        let Some(worker) = self.workers.remove(&id) else {
            return false;
        };
        worker.alive.store(false, Ordering::Relaxed);
        let _ = worker.inbox_tx.send(Inbox::Terminate);
        true
    }
}

/// Body of a worker thread: fetch, compile, instantiate, run `start_app`, then
/// loop delivering inbound messages to `on_message` until terminated.
fn worker_main(
    url: &str,
    page_url: &str,
    engine: &WasmEngine,
    host_state: HostState,
    inbox_rx: Receiver<Inbox>,
    alive: &AtomicBool,
) -> Result<(), String> {
    let wasm_bytes = crate::runtime::fetch_module(url, page_url)
        .map_err(|e| format!("fetch failed for {url}: {e:#}"))?;
    let module =
        compile_cached(engine.engine(), &wasm_bytes).map_err(|e| format!("compile failed: {e}"))?;
    let (mut store, instance) = crate::runtime::instantiate(engine, host_state, &module)
        .map_err(|e| format!("instantiate failed: {e:#}"))?;
    if let Ok(start_app) = instance.get_typed_func::<(), ()>(&mut store, "start_app") {
        start_app
            .call(&mut store, ())
            .map_err(|e| crate::runtime::guest_error("start_app", &e))?;
    }
    let on_message = instance
        .get_typed_func::<u32, ()>(&mut store, "on_message")
        .ok();
    while alive.load(Ordering::Relaxed) {
        let bytes = match inbox_rx.recv() {
            Ok(Inbox::Message(bytes)) => bytes,
            Ok(Inbox::Terminate) | Err(_) => break,
        };
        let Some(on_message) = &on_message else {
            continue;
        };
        let len = bytes.len() as u32;
        *store.data().worker_current_msg.lock().unwrap() = Some(bytes);
        store
            .set_fuel(engine.fuel_limit())
            .map_err(|e| e.to_string())?;
        on_message
            .call(&mut store, len)
            .map_err(|e| crate::runtime::guest_error("on_message", &e))?;
        *store.data().worker_current_msg.lock().unwrap() = None;
    }
    Ok(())
}

/// Register all `api_worker_*` / `api_spawn_worker` host functions.
pub fn register_worker_functions(linker: &mut Linker<HostState>) -> Result<()> {
    // api_spawn_worker(url_ptr: u32, url_len: u32) -> i32
    //   Returns a worker handle (> 0), or -1 on error.
    linker.func_wrap(
        "oxide",
        "api_spawn_worker",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> i32 {
            let Some(url) = guest_str(&caller, url_ptr, url_len) else {
                return -1;
            };
            let state = caller.data();
            let id = state
                .workers
                .lock()
                .unwrap()
                .get_or_insert_default()
                .spawn(url.clone(), state);
            if id == 0 {
                state.log(
                    ConsoleLevel::Error,
                    "[WORKER] spawn failed (no module loader)",
                );
                return -1;
            }
            state.log(
                ConsoleLevel::Log,
                format!("[WORKER] spawned {url} (handle={id})"),
            );
            id as i32
        },
    )?;

    // api_worker_post_message(handle: u32, ptr: u32, len: u32) -> i32
    //   Parent → worker. Returns 0 on success, -1 if the handle is unknown.
    linker.func_wrap(
        "oxide",
        "api_worker_post_message",
        |caller: Caller<'_, HostState>, handle: u32, ptr: u32, len: u32| -> i32 {
            let Some(data) = guest_bytes(&caller, ptr, len) else {
                return -1;
            };
            if with(&caller.data().workers, |w| w.post(handle, data)) == Some(true) {
                0
            } else {
                -1
            }
        },
    )?;

    // api_worker_recv(handle: u32, out_ptr: u32, out_cap: u32) -> i64
    //   Worker → parent. -1 if no message is queued, else the byte length written.
    linker.func_wrap(
        "oxide",
        "api_worker_recv",
        |mut caller: Caller<'_, HostState>, handle: u32, out_ptr: u32, out_cap: u32| -> i64 {
            let Some(msg) = with(&caller.data().workers, |w| w.recv(handle)).flatten() else {
                return -1;
            };
            write_prefix(&mut caller, out_ptr, out_cap, &msg).map_or(-1, i64::from)
        },
    )?;

    // api_worker_terminate(handle: u32) -> i32
    //   Returns 1 if the worker was running, 0 if the handle is unknown.
    linker.func_wrap(
        "oxide",
        "api_worker_terminate",
        |caller: Caller<'_, HostState>, handle: u32| -> i32 {
            i32::from(with(&caller.data().workers, |w| w.terminate(handle)) == Some(true))
        },
    )?;

    // api_worker_post(ptr: u32, len: u32) -> i32
    //   Worker → its spawning parent. -1 if not running inside a worker.
    linker.func_wrap(
        "oxide",
        "api_worker_post",
        |caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
            let (Some(data), Some(outbox)) =
                (guest_bytes(&caller, ptr, len), &caller.data().worker_outbox)
            else {
                return -1;
            };
            outbox.lock().unwrap().push_back(data);
            0
        },
    )?;

    // api_worker_message_read(out_ptr: u32, out_cap: u32) -> u32
    //   Copies the current inbound message into guest memory during on_message.
    //   Returns the number of bytes written (0 if no message is active).
    linker.func_wrap(
        "oxide",
        "api_worker_message_read",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> u32 {
            let msg = caller.data().worker_current_msg.lock().unwrap().clone();
            let msg = msg.unwrap_or_default();
            write_prefix(&mut caller, out_ptr, out_cap, &msg).unwrap_or(0)
        },
    )?;

    Ok(())
}
