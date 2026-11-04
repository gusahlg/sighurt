//! Non-blocking / streaming HTTP fetch for Oxide guest modules.
//!
//! The legacy `api_fetch` import (in [`crate::capabilities`]) blocks the guest
//! until the entire response body has been downloaded. That prevents guests
//! from rendering frames during large downloads and makes LLM-style
//! token-streaming, chunked feeds, or progressive image loads impossible.
//!
//! This module exposes a second fetch API that is fully async from the guest's
//! perspective. The shape mirrors [`crate::websocket`]:
//!
//! * `api_fetch_begin` dispatches a request and returns a handle immediately.
//! * `api_fetch_state` / `api_fetch_status` report progress.
//! * `api_fetch_recv` pulls the next body chunk from an in-memory queue.
//! * `api_fetch_abort` cancels an in-flight request.
//! * `api_fetch_remove` frees host-side resources once the guest is done.
//!
//! A background tokio task per request, on the runtime's shared network runtime
//! ([`crate::net::Net`]), drives `reqwest`'s `bytes_stream()`, pushing chunks into a
//! `VecDeque<Vec<u8>>` drained by the guest. Requests follow the document's
//! [`NetPolicy`](crate::net::NetPolicy); a refused URL or method fails `api_fetch_begin`.
//!
//! What a guest can make the host keep is bounded: at most [`MAX_REQUESTS`] handles, and per
//! request at most [`MAX_QUEUED_BYTES`] of body the guest hasn't read (and one chunk more),
//! within the document's budget of buffered bytes ([`NetBytes`]); past either the task stops
//! reading from the network until the guest catches up. Removing a handle, or dropping the
//! document, stops its task at once.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use tokio::task::JoinHandle;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    guest_bytes, guest_str, next_handle, request_method, with, with_started, write_guest,
    write_prefix, ConsoleLevel, HostState,
};
use crate::net::{Held, Net, NetBytes, NetPolicy, HTTP, ROOM_POLL};

/// Most requests a guest may have (until it removes them).
pub const MAX_REQUESTS: usize = 256;
/// Most bytes of unread body the host holds for one request before it stops reading.
pub const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;
/// How long one request may take, reading its body included.
const TIMEOUT: Duration = Duration::from_secs(60);

// ── Ready-state constants (mirrored in sighurt-sdk) ────────────────────────

/// Request dispatched; waiting for response headers.
pub const FETCH_PENDING: u32 = 0;
/// Headers received; body chunks may be streaming in.
pub const FETCH_STREAMING: u32 = 1;
/// Body fully delivered and the queue may still contain trailing chunks.
pub const FETCH_DONE: u32 = 2;
/// Request failed. Use `api_fetch_error` to retrieve the message.
pub const FETCH_ERROR: u32 = 3;
/// Request was aborted by the guest.
pub const FETCH_ABORTED: u32 = 4;

// ── Recv return-code sentinels (i64, negative) ─────────────────────────────

const RECV_PENDING: i64 = -1;
const RECV_EOF: i64 = -2;
const RECV_ERROR: i64 = -3;
const RECV_UNKNOWN: i64 = -4;

/// Per-request state shared between the host API and the background driver task.
struct FetchInner {
    /// One of the `FETCH_*` constants.
    state: AtomicU32,
    /// HTTP status code once headers arrive, 0 until then.
    status: AtomicU32,
    /// Guest-side signal to stop streaming. Polled by the driver task between chunks.
    aborted: AtomicBool,
    /// Queued body chunks. Each `Vec<u8>` is one network chunk; the guest may
    /// drain them in smaller pieces — see [`FetchState::recv`] for splitting.
    body: Mutex<Body>,
    /// Signalled when the guest reads, or stops the request, for a driver waiting for room.
    drained: tokio::sync::Notify,
    /// Last error message (set once before transitioning to [`FETCH_ERROR`]).
    error: Mutex<Option<String>>,
}

impl FetchInner {
    fn new(bytes: NetBytes) -> Self {
        Self {
            state: AtomicU32::new(FETCH_PENDING),
            status: AtomicU32::new(0),
            aborted: AtomicBool::new(false),
            body: Mutex::new(Body {
                chunks: VecDeque::new(),
                held: Held::new(bytes),
            }),
            drained: tokio::sync::Notify::new(),
            error: Mutex::new(None),
        }
    }

    fn stop(&self) {
        self.aborted.store(true, Ordering::SeqCst);
        self.drained.notify_one();
    }

    fn set_error(&self, msg: impl Into<String>) {
        *self.error.lock().unwrap() = Some(msg.into());
        self.state.store(FETCH_ERROR, Ordering::SeqCst);
    }
}

/// Body chunks the guest hasn't read, and the bytes they hold of the document's budget.
struct Body {
    chunks: VecDeque<Vec<u8>>,
    held: Held,
}

/// A request and the task that drives it.
struct Request {
    inner: Arc<FetchInner>,
    task: JoinHandle<()>,
}

/// All streaming-fetch state for a host. Lazily initialised on the first
/// `api_fetch_begin` call.
pub struct FetchState {
    net: Arc<Net>,
    policy: NetPolicy,
    bytes: NetBytes,
    handles: HashMap<u32, Request>,
    last_id: u32,
}

impl FetchState {
    pub fn new(net: Arc<Net>, policy: NetPolicy, bytes: NetBytes) -> Self {
        Self {
            net,
            policy,
            bytes,
            handles: HashMap::new(),
            last_id: 0,
        }
    }

    /// The state of `host`'s document; `None` when the network can't be used.
    pub fn of(host: &HostState) -> Option<Self> {
        Some(Self::new(
            host.shared_net()?,
            host.net_policy(),
            host.net_bytes(),
        ))
    }

    /// Dispatch a new request. Returns a handle (`> 0`), or why it was refused: the guest
    /// already has [`MAX_REQUESTS`], or the policy refuses its URL or method.
    fn begin(
        &mut self,
        method: String,
        url: String,
        content_type: String,
        body: Vec<u8>,
    ) -> Result<u32, String> {
        if self.handles.len() >= MAX_REQUESTS {
            return Err(format!("{MAX_REQUESTS} requests open"));
        }
        let url = self.policy.check(&url, HTTP)?;
        let mut request = self
            .net
            .client(&self.policy)
            .request(request_method(&method)?, url)
            .timeout(TIMEOUT);
        if let Some(origin) = self.policy.origin() {
            request = request.header("Origin", origin);
        }
        if !content_type.is_empty() {
            request = request.header("Content-Type", content_type);
        }
        if !body.is_empty() {
            request = request.body(body);
        }
        let id = next_handle(&mut self.last_id);
        let inner = Arc::new(FetchInner::new(self.bytes.clone()));
        let task = self
            .net
            .runtime()
            .spawn(drive_request(inner.clone(), request));
        self.handles.insert(id, Request { inner, task });
        Ok(id)
    }

    fn inner(&self, id: u32) -> Option<&FetchInner> {
        self.handles.get(&id).map(|request| &*request.inner)
    }

    fn state(&self, id: u32) -> u32 {
        self.inner(id)
            .map(|h| h.state.load(Ordering::SeqCst))
            .unwrap_or(FETCH_ERROR)
    }

    fn status(&self, id: u32) -> u32 {
        self.inner(id)
            .map(|h| h.status.load(Ordering::SeqCst))
            .unwrap_or(0)
    }

    fn error(&self, id: u32) -> Option<String> {
        self.inner(id).and_then(|h| h.error.lock().unwrap().clone())
    }

    /// Pop up to `cap` bytes from the head of the body queue.
    ///
    /// Returns one of the negative sentinels (`RECV_*`) or a non-negative byte
    /// count. When a single queued chunk is larger than `cap`, the extra bytes
    /// are re-queued at the front so the next call returns them — no data is
    /// dropped.
    fn recv(&self, id: u32, cap: usize) -> RecvResult {
        let Some(inner) = self.inner(id) else {
            return RecvResult::Sentinel(RECV_UNKNOWN);
        };

        let mut body = inner.body.lock().unwrap();
        if let Some(mut chunk) = body.chunks.pop_front() {
            if chunk.len() > cap {
                let remainder = chunk.split_off(cap);
                body.chunks.push_front(remainder);
            }
            body.held.remove(chunk.len());
            inner.drained.notify_one();
            return RecvResult::Data(chunk);
        }
        drop(body);

        match inner.state.load(Ordering::SeqCst) {
            FETCH_DONE => RecvResult::Sentinel(RECV_EOF),
            FETCH_ERROR | FETCH_ABORTED => RecvResult::Sentinel(RECV_ERROR),
            _ => RecvResult::Sentinel(RECV_PENDING),
        }
    }

    fn abort(&self, id: u32) -> bool {
        if let Some(inner) = self.inner(id) {
            inner.stop();
            // Flip state immediately so `state()` reports the intent even
            // before the driver task observes the flag.
            let _ = inner.state.compare_exchange(
                FETCH_PENDING,
                FETCH_ABORTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            let _ = inner.state.compare_exchange(
                FETCH_STREAMING,
                FETCH_ABORTED,
                Ordering::SeqCst,
                Ordering::SeqCst,
            );
            true
        } else {
            false
        }
    }

    /// Forgets request `id`, stopping its task wherever it is (connecting, sending, reading).
    fn remove(&mut self, id: u32) {
        if let Some(request) = self.handles.remove(&id) {
            request.inner.stop();
            request.task.abort();
        }
    }
}

impl Drop for FetchState {
    /// The document is gone: its requests stop.
    fn drop(&mut self) {
        for request in self.handles.values() {
            request.task.abort();
        }
    }
}

enum RecvResult {
    Data(Vec<u8>),
    Sentinel(i64),
}

/// Background task body: run the request and feed chunks into `inner`.
async fn drive_request(inner: Arc<FetchInner>, request: reqwest::RequestBuilder) {
    if inner.aborted.load(Ordering::SeqCst) {
        inner.state.store(FETCH_ABORTED, Ordering::SeqCst);
        return;
    }

    let resp = match request.send().await {
        Ok(r) => r,
        Err(e) => {
            inner.set_error(e.to_string());
            return;
        }
    };

    if inner.aborted.load(Ordering::SeqCst) {
        inner.state.store(FETCH_ABORTED, Ordering::SeqCst);
        return;
    }

    inner
        .status
        .store(resp.status().as_u16() as u32, Ordering::SeqCst);
    inner.state.store(FETCH_STREAMING, Ordering::SeqCst);

    let mut stream = resp.bytes_stream();
    while let Some(next) = stream.next().await {
        if inner.aborted.load(Ordering::SeqCst) {
            inner.state.store(FETCH_ABORTED, Ordering::SeqCst);
            return;
        }
        match next {
            Ok(chunk) => {
                if !chunk.is_empty() && !queue(&inner, chunk.to_vec()).await {
                    inner.state.store(FETCH_ABORTED, Ordering::SeqCst);
                    return;
                }
            }
            Err(e) => {
                inner.set_error(e.to_string());
                return;
            }
        }
    }

    // Preserve ABORTED if the guest aborted between the final chunk and here.
    let _ = inner.state.compare_exchange(
        FETCH_STREAMING,
        FETCH_DONE,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}

/// Queues `chunk` for the guest once there is room for it, reading no further meanwhile: room
/// in the request's queue ([`MAX_QUEUED_BYTES`], and one chunk more) and in the document's
/// budget. False if the guest stopped the request first.
async fn queue(inner: &FetchInner, chunk: Vec<u8>) -> bool {
    let mut chunk = Some(chunk);
    loop {
        if inner.aborted.load(Ordering::SeqCst) {
            return false;
        }
        let queue_full = {
            let mut body = inner.body.lock().unwrap();
            let queue_full = body.held.get() > MAX_QUEUED_BYTES;
            let len = chunk.as_ref().map_or(0, Vec::len);
            if !queue_full && body.held.try_add(len) {
                body.chunks.extend(chunk.take());
                return true;
            }
            queue_full
        };
        if queue_full {
            inner.drained.notified().await;
        } else {
            // The document's budget is full: another connection's reader frees it.
            tokio::select! {
                _ = inner.drained.notified() => {}
                _ = tokio::time::sleep(ROOM_POLL) => {}
            }
        }
    }
}

/// Register all `api_fetch_*` streaming host functions on the given linker.
pub fn register_fetch_functions(linker: &mut Linker<HostState>) -> Result<()> {
    // api_fetch_begin(method_ptr, method_len, url_ptr, url_len,
    //                 ct_ptr, ct_len, body_ptr, body_len) -> u32
    //   Returns a handle (> 0), or 0 on error.
    linker.func_wrap(
        "oxide",
        "api_fetch_begin",
        |caller: Caller<'_, HostState>,
         method_ptr: u32,
         method_len: u32,
         url_ptr: u32,
         url_len: u32,
         ct_ptr: u32,
         ct_len: u32,
         body_ptr: u32,
         body_len: u32|
         -> u32 {
            let method = guest_str(&caller, method_ptr, method_len).unwrap_or_default();
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            let content_type = guest_str(&caller, ct_ptr, ct_len).unwrap_or_default();
            let body = guest_bytes(&caller, body_ptr, body_len).unwrap_or_default();
            let state = caller.data();
            let line = format!("[FETCH] {method} {url}");
            match with_started(
                &state.fetch,
                || FetchState::of(state),
                |f| f.begin(method, url, content_type, body),
            ) {
                Some(Err(e)) => {
                    state.log(ConsoleLevel::Error, format!("[FETCH] Refused: {e}"));
                    0
                }
                Some(Ok(id)) => {
                    state.log(ConsoleLevel::Log, format!("{line} (id={id})"));
                    id
                }
                None => {
                    state.log(ConsoleLevel::Error, "[FETCH] Init failed");
                    0
                }
            }
        },
    )?;

    // api_fetch_state(id) -> u32  (one of FETCH_* constants)
    linker.func_wrap(
        "oxide",
        "api_fetch_state",
        |caller: Caller<'_, HostState>, id: u32| -> u32 {
            with(&caller.data().fetch, |f| f.state(id)).unwrap_or(FETCH_ERROR)
        },
    )?;

    // api_fetch_status(id) -> u32  (HTTP status, 0 before headers)
    linker.func_wrap(
        "oxide",
        "api_fetch_status",
        |caller: Caller<'_, HostState>, id: u32| -> u32 {
            with(&caller.data().fetch, |f| f.status(id)).unwrap_or(0)
        },
    )?;

    // api_fetch_recv(id, out_ptr, out_cap) -> i64
    //   >= 0 : bytes written into `out_ptr` (one chunk, possibly partial)
    //   -1   : pending (no chunk available, more may arrive)
    //   -2   : end of stream (body fully delivered)
    //   -3   : error (see api_fetch_error)
    //   -4   : unknown handle
    linker.func_wrap(
        "oxide",
        "api_fetch_recv",
        |mut caller: Caller<'_, HostState>, id: u32, out_ptr: u32, out_cap: u32| -> i64 {
            match with(&caller.data().fetch, |f| f.recv(id, out_cap as usize)) {
                None => RECV_UNKNOWN,
                Some(RecvResult::Sentinel(code)) => code,
                Some(RecvResult::Data(bytes)) => {
                    if write_guest(&mut caller, out_ptr, &bytes) {
                        bytes.len() as i64
                    } else {
                        RECV_ERROR
                    }
                }
            }
        },
    )?;

    // api_fetch_error(id, out_ptr, out_cap) -> i32
    //   >= 0 : number of UTF-8 bytes written (possibly truncated)
    //   -1   : no error set
    linker.func_wrap(
        "oxide",
        "api_fetch_error",
        |mut caller: Caller<'_, HostState>, id: u32, out_ptr: u32, out_cap: u32| -> i32 {
            let Some(message) = with(&caller.data().fetch, |f| f.error(id)).flatten() else {
                return -1;
            };
            match write_prefix(&mut caller, out_ptr, out_cap, message.as_bytes()) {
                Some(len) => len as i32,
                None => -1,
            }
        },
    )?;

    // api_fetch_abort(id) -> i32  (1 if aborted, 0 if unknown handle)
    linker.func_wrap(
        "oxide",
        "api_fetch_abort",
        |caller: Caller<'_, HostState>, id: u32| -> i32 {
            i32::from(with(&caller.data().fetch, |f| f.abort(id)) == Some(true))
        },
    )?;

    // api_fetch_remove(id)  — free host-side resources.
    linker.func_wrap(
        "oxide",
        "api_fetch_remove",
        |caller: Caller<'_, HostState>, id: u32| {
            with(&caller.data().fetch, |f| f.remove(id));
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::time::Instant;

    use super::*;

    const MIB: usize = 1024 * 1024;

    /// Serves one response of `total` bytes, a MiB at a time, on a local port.
    fn serve(total: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request);
            let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {total}\r\n\r\n");
            let _ = socket.write_all(head.as_bytes());
            for _ in 0..total / MIB {
                if socket.write_all(&[b'a'; MIB]).is_err() {
                    return;
                }
            }
        });
        format!("http://{address}/")
    }

    /// A fetch state of a local page, with a budget of `limit` bytes.
    fn local(limit: usize) -> FetchState {
        let policy = NetPolicy::for_page("file:///app.wasm", None);
        let net = Arc::new(Net::new().unwrap());
        FetchState::new(net, policy, NetBytes::with_limit(limit))
    }

    fn get(fetch: &mut FetchState, url: String) -> u32 {
        fetch
            .begin("GET".into(), url, String::new(), Vec::new())
            .unwrap()
    }

    fn queued(fetch: &FetchState, id: u32) -> usize {
        fetch.handles[&id].inner.body.lock().unwrap().held.get()
    }

    #[test]
    fn a_body_nobody_reads_is_held_back_not_kept() {
        let total = 64 * MIB;
        let mut fetch = local(crate::net::MAX_DOCUMENT_BYTES);
        let id = get(&mut fetch, serve(total));
        let queued = || queued(&fetch, id);
        let started = Instant::now();
        while queued() < MAX_QUEUED_BYTES && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(300));
        let held = queued();
        // Reqwest hands over at most a few hundred KiB at a time.
        let bound = MAX_QUEUED_BYTES..=MAX_QUEUED_BYTES + MIB;
        assert!(bound.contains(&held), "{held}");
        // Reading lets the rest through.
        let mut read = 0;
        let started = Instant::now();
        loop {
            match fetch.recv(id, MIB) {
                RecvResult::Data(data) => read += data.len(),
                RecvResult::Sentinel(RECV_EOF) => break,
                RecvResult::Sentinel(RECV_PENDING) => std::thread::sleep(Duration::from_millis(1)),
                RecvResult::Sentinel(code) => panic!("{code}"),
            }
            assert!(started.elapsed() < Duration::from_secs(20));
        }
        assert_eq!(read, total);
    }

    #[test]
    fn a_guest_has_a_bounded_number_of_requests() {
        let mut fetch = local(crate::net::MAX_DOCUMENT_BYTES);
        // Nothing listens there: each request fails at once.
        let url = || "http://127.0.0.1:1/".to_string();
        for _ in 0..MAX_REQUESTS {
            get(&mut fetch, url());
        }
        let refused = fetch.begin("GET".into(), url(), String::new(), Vec::new());
        assert!(refused.is_err());
        fetch.remove(1);
        get(&mut fetch, url());
    }

    /// Several requests together hold no more than the document's budget.
    #[test]
    fn requests_share_the_documents_budget() {
        let budget = 6 * MIB;
        let mut fetch = local(budget);
        let ids: Vec<u32> = (0..3).map(|_| get(&mut fetch, serve(16 * MIB))).collect();
        let total = |fetch: &FetchState| ids.iter().map(|&id| queued(fetch, id)).sum::<usize>();
        let started = Instant::now();
        while total(&fetch) + MIB < budget && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(300));
        let held = total(&fetch);
        assert!((budget - MIB..=budget).contains(&held), "{held}");
        // Removing one gives its share back to the others.
        fetch.remove(ids[0]);
        let rest = |fetch: &FetchState| ids[1..].iter().map(|&id| queued(fetch, id)).sum::<usize>();
        let started = Instant::now();
        while rest(&fetch) + MIB < budget && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        let rest = rest(&fetch);
        assert!((budget - MIB..=budget).contains(&rest), "{rest}");
    }

    #[test]
    fn a_page_from_the_web_cannot_fetch_local_addresses() {
        let net = Arc::new(Net::new().unwrap());
        let policy = NetPolicy::for_page("https://a.test/app.wasm", Some([1, 1, 1, 1].into()));
        let mut fetch = FetchState::new(net, policy, NetBytes::with_limit(MIB));
        for url in ["http://127.0.0.1:1/", "file:///etc/passwd", "http://[::1]/"] {
            let refused = fetch.begin("GET".into(), url.into(), String::new(), Vec::new());
            assert!(refused.is_err(), "{url}");
        }
        assert!(fetch
            .begin(
                "TRACE".into(),
                "https://a.test/".into(),
                String::new(),
                Vec::new()
            )
            .is_err());
    }

    /// A removed request's task ends: the server sees its connection close.
    #[test]
    fn removing_a_request_stops_it() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = socket.read(&mut request);
            // Never answer; tell when the client hangs up.
            let _ = socket.read(&mut request);
            let _ = tx.send(());
        });
        let mut fetch = local(MIB);
        let id = get(&mut fetch, format!("http://{address}/"));
        std::thread::sleep(Duration::from_millis(200));
        fetch.remove(id);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the request was not stopped");
    }
}
