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
//!
//! Workers run in the browser process, so they are bounded: a guest has at most
//! [`MAX_WORKERS`] worker threads alive, terminated ones still finishing included, and the
//! runtime at most [`MAX_WORKER_THREADS`]; each direction between a guest and a worker holds at
//! most [`MAX_QUEUED_MESSAGES`] undelivered messages of [`MAX_QUEUED_BYTES`] together, and
//! posting to a full queue fails. Each call into a worker has the same fuel and time limits as a
//! page's, and stops at the next epoch tick once the worker is terminated or its page's document
//! is closed or replaced. A worker's memory counts in the runtime's total
//! ([`crate::engine::Limits`]), and its connections follow its page's network policy within its
//! page's budget of buffered bytes ([`crate::net`]). A worker opens no file dialog.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    guest_bytes, guest_str, next_handle, with, write_prefix, ConsoleLevel, HostState,
};
use crate::engine::{Budget, ThreadCount, WasmEngine};

/// Most worker threads one guest may have alive at once.
pub const MAX_WORKERS: usize = 16;
/// Most worker threads alive in a runtime, over all its guests.
pub const MAX_WORKER_THREADS: usize = 32;
/// Most messages, and most bytes of them, waiting in either direction between a guest and one
/// of its workers.
pub const MAX_QUEUED_MESSAGES: usize = 1024;
pub const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;

/// Message handed to a worker thread over its inbox channel.
enum Inbox {
    /// A byte payload to deliver to the worker's `on_message` export.
    Message(Vec<u8>),
    /// Request the worker thread to stop.
    Terminate,
}

/// Messages one side of a worker posted and the other hasn't read yet.
#[derive(Debug, Default)]
pub struct Mailbox {
    messages: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl Mailbox {
    /// Queues `message`, unless the mailbox is full; returns whether it did.
    pub fn post(&mut self, message: Vec<u8>) -> bool {
        let bytes = self.bytes + message.len();
        if self.messages.len() >= MAX_QUEUED_MESSAGES || bytes > MAX_QUEUED_BYTES {
            return false;
        }
        self.bytes = bytes;
        self.messages.push_back(message);
        true
    }

    pub fn take(&mut self) -> Option<Vec<u8>> {
        let message = self.messages.pop_front()?;
        self.bytes -= message.len();
        Some(message)
    }
}

/// Parent-side handle to a single running worker.
struct Worker {
    /// Sender for the worker's inbox (parent → worker).
    inbox_tx: SyncSender<Inbox>,
    /// Bytes of messages in the inbox.
    inbox_bytes: Arc<AtomicUsize>,
    /// Messages the worker posted back (worker → parent), drained by `api_worker_recv`.
    outbox: Arc<Mutex<Mailbox>>,
    /// The worker's budget, cancelled on terminate.
    budget: Budget,
}

/// Registry of workers spawned by one guest. Created on first spawn.
#[derive(Default)]
pub struct WorkerState {
    workers: HashMap<u32, Worker>,
    last_id: u32,
    /// This guest's worker threads alive, terminated ones included.
    threads: Arc<AtomicUsize>,
}

impl WorkerState {
    /// Spawn a worker for `url`, deriving its sandbox state from `parent`.
    ///
    /// Returns a handle (`> 0`) or `0` if there is no engine to run it on, the guest already
    /// has [`MAX_WORKERS`] worker threads alive or the runtime [`MAX_WORKER_THREADS`], or no
    /// thread can be started. The worker shares only the console, the persistent KV store of the
    /// parent's origin, and its network policy and byte budget; canvas, input, timers, and
    /// memory are its own.
    fn spawn(&mut self, url: String, parent: &HostState) -> u32 {
        let Some(engine) = parent.engine.clone() else {
            return 0;
        };
        let Some(mine) = ThreadCount::add(&self.threads, MAX_WORKERS) else {
            return 0;
        };
        let Some(all) = engine.worker_thread(MAX_WORKER_THREADS) else {
            return 0;
        };
        let (inbox_tx, inbox_rx) = std::sync::mpsc::sync_channel::<Inbox>(MAX_QUEUED_MESSAGES);
        let inbox_bytes = Arc::new(AtomicUsize::new(0));
        let outbox: Arc<Mutex<Mailbox>> = Default::default();
        let budget = parent.budget.child();
        let child = HostState {
            engine: Some(engine.clone()),
            budget: budget.clone(),
            console: parent.console.clone(),
            current_url: Arc::new(Mutex::new(url.clone())),
            module_origin: Arc::new(Mutex::new(parent.module_origin.lock().unwrap().clone())),
            // The page's network policy, and its budget of buffered bytes.
            net: parent.net.clone(),
            worker_outbox: Some(outbox.clone()),
            ..Default::default()
        };
        let page_url = parent.current_url.lock().unwrap().clone();
        let bytes = inbox_bytes.clone();
        let started = std::thread::Builder::new()
            .name("sighurt-wasm-worker".into())
            .stack_size(crate::engine::GUEST_THREAD_STACK)
            .spawn(move || {
                let _counted = (mine, all);
                let inbox = (inbox_rx, bytes);
                let result = worker_main(&url, &page_url, &engine, child.clone(), inbox);
                if let Err(message) = result {
                    child.log(ConsoleLevel::Error, format!("[WORKER] {message}"));
                }
            });
        if started.is_err() {
            return 0;
        }

        let id = next_handle(&mut self.last_id);
        let worker = Worker {
            inbox_tx,
            inbox_bytes,
            outbox,
            budget,
        };
        self.workers.insert(id, worker);
        id
    }

    fn post(&self, id: u32, data: Vec<u8>) -> bool {
        let Some(worker) = self.workers.get(&id) else {
            return false;
        };
        let len = data.len();
        let queued = worker.inbox_bytes.fetch_add(len, Ordering::AcqRel);
        if queued + len > MAX_QUEUED_BYTES
            || worker.inbox_tx.try_send(Inbox::Message(data)).is_err()
        {
            worker.inbox_bytes.fetch_sub(len, Ordering::AcqRel);
            return false;
        }
        true
    }

    fn recv(&self, id: u32) -> Option<Vec<u8>> {
        self.workers.get(&id)?.outbox.lock().unwrap().take()
    }

    fn terminate(&mut self, id: u32) -> bool {
        let Some(worker) = self.workers.remove(&id) else {
            return false;
        };
        // Its callback in progress stops at the next epoch tick; dropping the sender ends its
        // wait for a message.
        worker.budget.cancel();
        let _ = worker.inbox_tx.try_send(Inbox::Terminate);
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
    (inbox_rx, inbox_bytes): (Receiver<Inbox>, Arc<AtomicUsize>),
) -> Result<(), String> {
    let budget = host_state.budget.clone();
    let (wasm_bytes, _) = crate::runtime::fetch_module(url, page_url, &host_state.net_policy())
        .map_err(|e| format!("fetch failed for {url}: {e:#}"))?;
    let module = engine
        .compile_module(&wasm_bytes)
        .map_err(|e| format!("compile failed: {e}"))?;
    let (mut store, instance) = crate::runtime::instantiate(engine, host_state, &module)
        .map_err(|e| format!("instantiate failed: {e:#}"))?;
    if let Ok(start_app) = instance.get_typed_func::<(), ()>(&mut store, "start_app") {
        engine
            .start(&mut store, |store| start_app.call(store, ()))
            .map_err(|e| crate::runtime::guest_error("start_app", &e))?;
    }
    let on_message = instance
        .get_typed_func::<u32, ()>(&mut store, "on_message")
        .ok();
    while !budget.is_cancelled() {
        let bytes = match inbox_rx.recv() {
            Ok(Inbox::Message(bytes)) => bytes,
            Ok(Inbox::Terminate) | Err(_) => break,
        };
        inbox_bytes.fetch_sub(bytes.len(), Ordering::AcqRel);
        let Some(on_message) = &on_message else {
            continue;
        };
        let len = bytes.len() as u32;
        *store.data().worker_current_msg.lock().unwrap() = Some(bytes);
        engine
            .call(&mut store, |store| on_message.call(store, len))
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
                    "[WORKER] spawn refused: too many workers, or no module loader",
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
    //   Parent → worker. Returns 0 on success, -1 if the handle is unknown or its inbox is full.
    linker.func_wrap(
        "oxide",
        "api_worker_post_message",
        |caller: Caller<'_, HostState>, handle: u32, ptr: u32, len: u32| -> i32 {
            if len as usize > MAX_QUEUED_BYTES {
                return -1;
            }
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
    //   Worker → its spawning parent. -1 if not running inside a worker, or its outbox is full.
    linker.func_wrap(
        "oxide",
        "api_worker_post",
        |caller: Caller<'_, HostState>, ptr: u32, len: u32| -> i32 {
            if len as usize > MAX_QUEUED_BYTES {
                return -1;
            }
            let (Some(data), Some(outbox)) =
                (guest_bytes(&caller, ptr, len), &caller.data().worker_outbox)
            else {
                return -1;
            };
            if !outbox.lock().unwrap().post(data) {
                return -1;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mailbox_holds_a_bounded_number_of_bytes_and_messages() {
        let mut mailbox = Mailbox::default();
        assert!(mailbox.post(vec![0; MAX_QUEUED_BYTES]));
        assert!(!mailbox.post(vec![0]));
        assert_eq!(mailbox.take().map(|m| m.len()), Some(MAX_QUEUED_BYTES));
        for _ in 0..MAX_QUEUED_MESSAGES {
            assert!(mailbox.post(Vec::new()));
        }
        assert!(!mailbox.post(Vec::new()));
        assert_eq!(mailbox.take(), Some(Vec::new()));
        assert!(mailbox.post(Vec::new()));
    }
}
