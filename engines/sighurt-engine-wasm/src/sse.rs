//! Server-Sent Events for Oxide guest modules.
//!
//! Guests call `api_sse_open` to start an EventSource-style stream. The host
//! drives the HTTP connection on a background tokio task, parses the SSE
//! wire format, and queues events for `api_sse_recv`. The stream reconnects
//! automatically with `Last-Event-ID`, matching the web `EventSource` contract.
//!
//! Streams follow the document's [`NetPolicy`] (refused URLs fail `api_sse_open`), carry its
//! `Origin`, and run on the runtime's shared network runtime ([`crate::net::Net`]). A
//! connection that sends nothing, not even a comment, for [`IDLE_TIMEOUT`] is dropped and
//! reconnected. Removing a stream, or dropping the document, stops its task.
//!
//! What a guest can make the host keep is bounded: at most [`MAX_STREAMS`] streams (until it
//! removes them), [`MAX_QUEUE`] events per stream (the oldest go first) within the document's
//! budget of buffered bytes ([`NetBytes`]; past it the stream stops reading until the guest
//! catches up), and events and lines of at most [`MAX_EVENT_BYTES`]; longer ones are dropped as
//! they arrive.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use tokio::task::JoinHandle;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    guest_str, next_handle, with, with_started, write_guest, ConsoleLevel, HostState,
};
use crate::net::{Held, Net, NetBytes, NetPolicy, HTTP, ROOM_POLL};

/// Connection is being established (or reconnecting).
pub const SSE_CONNECTING: u32 = 0;
/// Stream is open; events may be queued.
pub const SSE_OPEN: u32 = 1;
/// Stream was closed by the guest or a terminal HTTP status (e.g. 204).
pub const SSE_CLOSED: u32 = 2;
/// Last attempt failed. Use `api_sse_error` for the message; reconnect may still run.
pub const SSE_ERROR: u32 = 3;

const RECV_PENDING: i64 = -1;
const RECV_CLOSED: i64 = -2;
const RECV_ERROR: i64 = -3;
const RECV_UNKNOWN: i64 = -4;

/// Most streams a guest may have.
pub const MAX_STREAMS: usize = 256;
/// Most events waiting in one stream.
pub const MAX_QUEUE: usize = 128;
/// Largest event, and longest line, kept.
pub const MAX_EVENT_BYTES: usize = 256 * 1024;
/// Longest a stream may go without receiving anything before it is dropped and reconnected.
/// Servers send comments as keep-alives far more often.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_RETRY_MS: u64 = 3_000;
const MIN_RETRY_MS: u64 = 500;
const MAX_RETRY_MS: u64 = 60_000;

/// One parsed SSE event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub name: String,
    pub id: String,
    pub data: String,
}

/// Incremental SSE parser (WHATWG HTML Living Standard, event stream). Each byte is looked at
/// once: the line in progress is kept, at most [`MAX_EVENT_BYTES`] of it.
pub struct SseParser {
    /// The line in progress.
    buf: Vec<u8>,
    /// The rest of the line in progress is dropped: it is too long.
    skip_line: bool,
    /// The last chunk ended in a CR: a LF that starts the next one ends no other line.
    after_cr: bool,
    name: String,
    id: String,
    data_lines: Vec<String>,
    /// Bytes in `data_lines`.
    data_bytes: usize,
    /// The event in progress is dropped: it is too large.
    oversized: bool,
    /// Last `retry:` value seen, if any.
    pub retry_ms: Option<u64>,
}

impl Default for SseParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            skip_line: false,
            after_cr: false,
            name: String::new(),
            id: String::new(),
            data_lines: Vec::new(),
            data_bytes: 0,
            oversized: false,
            retry_ms: None,
        }
    }

    /// Feed bytes and return every complete event dispatched.
    pub fn push(&mut self, mut bytes: &[u8]) -> Vec<SseEvent> {
        let mut events = Vec::new();
        if self.after_cr && !bytes.is_empty() {
            self.after_cr = false;
            bytes = bytes.strip_prefix(b"\n").unwrap_or(bytes);
        }
        while let Some(end) = bytes.iter().position(|&b| b == b'\n' || b == b'\r') {
            let (part, rest) = bytes.split_at(end);
            let crlf = rest.starts_with(b"\r\n");
            bytes = &rest[1 + usize::from(crlf)..];
            self.after_cr = rest[0] == b'\r' && !crlf && bytes.is_empty();
            let too_long = self.buf.len() + part.len() > MAX_EVENT_BYTES;
            if std::mem::take(&mut self.skip_line) || too_long {
                self.buf.clear();
                continue;
            }
            self.buf.extend_from_slice(part);
            let line = std::mem::take(&mut self.buf);
            if let Ok(line) = String::from_utf8(line) {
                self.line(&line, &mut events);
            }
        }
        // A line that doesn't end within the limit is dropped, up to its end.
        if !self.skip_line {
            if self.buf.len() + bytes.len() > MAX_EVENT_BYTES {
                self.buf.clear();
                self.skip_line = true;
            } else {
                self.buf.extend_from_slice(bytes);
            }
        }
        events
    }

    /// Handles one whole line.
    fn line(&mut self, line: &str, events: &mut Vec<SseEvent>) {
        {
            if line.is_empty() {
                if let Some(ev) = self.dispatch() {
                    events.push(ev);
                }
                return;
            }
            if line.starts_with(':') {
                return;
            }
            let (field, value) = split_field(line);
            match field {
                "event" => self.name = value.to_string(),
                "data" if !self.oversized => {
                    self.data_bytes += value.len() + 1;
                    if self.data_bytes > MAX_EVENT_BYTES + 1 {
                        self.oversized = true;
                        self.data_lines.clear();
                    } else {
                        self.data_lines.push(value.to_string());
                    }
                }
                "id" if !value.contains('\0') => self.id = value.to_string(),
                "retry" if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) => {
                    if let Ok(ms) = value.parse::<u64>() {
                        self.retry_ms = Some(ms.clamp(MIN_RETRY_MS, MAX_RETRY_MS));
                    }
                }
                _ => {}
            }
        }
    }

    fn dispatch(&mut self) -> Option<SseEvent> {
        self.data_bytes = 0;
        if std::mem::take(&mut self.oversized) {
            self.data_lines.clear();
            self.name.clear();
            return None;
        }
        if self.data_lines.is_empty() {
            self.name.clear();
            return None;
        }
        let data = self.data_lines.join("\n");
        self.data_lines.clear();
        let name = if self.name.is_empty() {
            "message".to_string()
        } else {
            std::mem::take(&mut self.name)
        };
        let id = self.id.clone();
        if data.len() > MAX_EVENT_BYTES {
            return None;
        }
        Some(SseEvent { name, id, data })
    }
}

fn split_field(line: &str) -> (&str, &str) {
    match line.split_once(':') {
        Some((field, rest)) => {
            let value = rest.strip_prefix(' ').unwrap_or(rest);
            (field, value)
        }
        None => (line, ""),
    }
}

/// Encode an event for the guest: `u16le name | name | u16le id | id | u32le data | data`.
pub fn encode_event(ev: &SseEvent) -> Vec<u8> {
    let name = ev.name.as_bytes();
    let id = ev.id.as_bytes();
    let data = ev.data.as_bytes();
    let name_len = (name.len().min(u16::MAX as usize)) as u16;
    let id_len = (id.len().min(u16::MAX as usize)) as u16;
    let data_len = data.len() as u32;
    let mut out = Vec::with_capacity(8 + name.len() + id.len() + data.len());
    out.extend_from_slice(&name_len.to_le_bytes());
    out.extend_from_slice(&name[..name_len as usize]);
    out.extend_from_slice(&id_len.to_le_bytes());
    out.extend_from_slice(&id[..id_len as usize]);
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(data);
    out
}

struct SseInner {
    state: AtomicU32,
    aborted: AtomicBool,
    events: Mutex<Events>,
    error: Mutex<Option<String>>,
    last_id: Mutex<String>,
    retry_ms: AtomicU64,
}

/// Events the guest hasn't read, and the bytes they hold of the document's budget.
struct Events {
    queue: VecDeque<SseEvent>,
    held: Held,
}

impl Events {
    fn pop(&mut self) -> Option<SseEvent> {
        let event = self.queue.pop_front()?;
        self.held.remove(event_bytes(&event));
        Some(event)
    }
}

/// The bytes an event holds.
fn event_bytes(event: &SseEvent) -> usize {
    event.name.len() + event.id.len() + event.data.len()
}

impl SseInner {
    fn new(bytes: NetBytes) -> Self {
        Self {
            state: AtomicU32::new(SSE_CONNECTING),
            aborted: AtomicBool::new(false),
            events: Mutex::new(Events {
                queue: VecDeque::new(),
                held: Held::new(bytes),
            }),
            error: Mutex::new(None),
            last_id: Mutex::new(String::new()),
            retry_ms: AtomicU64::new(DEFAULT_RETRY_MS),
        }
    }

    /// Queues `events`, the oldest of the queue going first past [`MAX_QUEUE`], while the
    /// document's budget has room; returns those it had none for, in order.
    fn push_events(&self, mut events: VecDeque<SseEvent>) -> VecDeque<SseEvent> {
        let mut q = self.events.lock().unwrap();
        while let Some(ev) = events.pop_front() {
            if q.queue.len() >= MAX_QUEUE {
                q.pop();
            }
            if !q.held.try_add(event_bytes(&ev)) {
                events.push_front(ev);
                break;
            }
            if !ev.id.is_empty() {
                *self.last_id.lock().unwrap() = ev.id.clone();
            }
            q.queue.push_back(ev);
        }
        events
    }

    /// Queues `events`, waiting for room in the document's budget as long as it takes. False
    /// if the guest closed the stream first.
    async fn deliver(&self, events: Vec<SseEvent>) -> bool {
        let mut events = VecDeque::from(events);
        loop {
            events = self.push_events(events);
            if events.is_empty() {
                return true;
            }
            if self.aborted.load(Ordering::SeqCst) {
                return false;
            }
            tokio::time::sleep(ROOM_POLL).await;
        }
    }

    fn set_error(&self, msg: impl Into<String>) {
        *self.error.lock().unwrap() = Some(msg.into());
        self.state.store(SSE_ERROR, Ordering::SeqCst);
    }
}

/// A stream and the task that drives it.
struct Stream {
    inner: Arc<SseInner>,
    task: JoinHandle<()>,
}

/// All SSE streams for a tab. Lazily initialised on the first `api_sse_*` call.
pub struct SseState {
    net: Arc<Net>,
    policy: NetPolicy,
    bytes: NetBytes,
    streams: HashMap<u32, Stream>,
    last_id: u32,
}

impl SseState {
    pub fn new(net: Arc<Net>, policy: NetPolicy, bytes: NetBytes) -> Self {
        Self {
            net,
            policy,
            bytes,
            streams: HashMap::new(),
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

    /// Opens a stream; returns its handle, or why it was refused: the guest has
    /// [`MAX_STREAMS`], or the policy refuses `url`.
    fn open(&mut self, url: String) -> Result<u32, String> {
        if self.streams.len() >= MAX_STREAMS {
            return Err(format!("{MAX_STREAMS} streams open"));
        }
        let url = self.policy.check(&url, HTTP)?;
        let mut request = self
            .net
            .client(&self.policy)
            .get(url)
            .header("Accept", "text/event-stream")
            .header("Cache-Control", "no-cache");
        if let Some(origin) = self.policy.origin() {
            request = request.header("Origin", origin);
        }
        let id = next_handle(&mut self.last_id);
        let inner = Arc::new(SseInner::new(self.bytes.clone()));
        let task = self
            .net
            .runtime()
            .spawn(drive_stream(inner.clone(), request));
        self.streams.insert(id, Stream { inner, task });
        Ok(id)
    }

    fn inner(&self, id: u32) -> Option<&SseInner> {
        self.streams.get(&id).map(|stream| &*stream.inner)
    }

    fn state(&self, id: u32) -> u32 {
        self.inner(id)
            .map(|s| s.state.load(Ordering::SeqCst))
            .unwrap_or(SSE_CLOSED)
    }

    fn pop(&self, id: u32) -> Option<SseEvent> {
        self.inner(id).and_then(|s| s.events.lock().unwrap().pop())
    }

    fn peek_front(&self, id: u32) -> Option<SseEvent> {
        self.inner(id)
            .and_then(|s| s.events.lock().unwrap().queue.front().cloned())
    }

    fn empty_recv_code(&self, id: u32) -> i64 {
        let Some(inner) = self.inner(id) else {
            return RECV_UNKNOWN;
        };
        match inner.state.load(Ordering::SeqCst) {
            SSE_CLOSED => RECV_CLOSED,
            SSE_ERROR => RECV_ERROR,
            _ => RECV_PENDING,
        }
    }

    /// Closes the stream: its task stops, and its events stay for the guest to read.
    fn close(&self, id: u32) -> bool {
        let Some(stream) = self.streams.get(&id) else {
            return false;
        };
        stream.inner.aborted.store(true, Ordering::SeqCst);
        stream.inner.state.store(SSE_CLOSED, Ordering::SeqCst);
        stream.task.abort();
        true
    }

    /// Forgets stream `id`, stopping its task wherever it is.
    fn remove(&mut self, id: u32) {
        if let Some(stream) = self.streams.remove(&id) {
            stream.inner.aborted.store(true, Ordering::SeqCst);
            stream.task.abort();
        }
    }

    fn error(&self, id: u32) -> Option<String> {
        self.inner(id).and_then(|s| s.error.lock().unwrap().clone())
    }
}

impl Drop for SseState {
    /// The document is gone: its streams stop.
    fn drop(&mut self) {
        for stream in self.streams.values() {
            stream.task.abort();
        }
    }
}

async fn drive_stream(inner: Arc<SseInner>, request: reqwest::RequestBuilder) {
    loop {
        if inner.aborted.load(Ordering::SeqCst) {
            inner.state.store(SSE_CLOSED, Ordering::SeqCst);
            return;
        }

        inner.state.store(SSE_CONNECTING, Ordering::SeqCst);
        let last_id = inner.last_id.lock().unwrap().clone();
        let Some(mut req) = request.try_clone() else {
            inner.set_error("the request can't be repeated");
            return;
        };
        if !last_id.is_empty() {
            req = req.header("Last-Event-ID", &last_id);
        }

        let resp = match tokio::time::timeout(IDLE_TIMEOUT, req.send()).await {
            Err(_) => {
                inner.set_error("no response");
                sleep_retry(&inner).await;
                continue;
            }
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                if inner.aborted.load(Ordering::SeqCst) {
                    inner.state.store(SSE_CLOSED, Ordering::SeqCst);
                    return;
                }
                inner.set_error(e.to_string());
                sleep_retry(&inner).await;
                continue;
            }
        };

        let status = resp.status();
        if status.as_u16() == 204 {
            inner.state.store(SSE_CLOSED, Ordering::SeqCst);
            return;
        }
        if !status.is_success() {
            inner.set_error(format!("HTTP {status}"));
            if inner.aborted.load(Ordering::SeqCst) {
                inner.state.store(SSE_CLOSED, Ordering::SeqCst);
                return;
            }
            sleep_retry(&inner).await;
            continue;
        }

        inner.state.store(SSE_OPEN, Ordering::SeqCst);
        let mut parser = SseParser::new();
        let mut stream = resp.bytes_stream();
        loop {
            let Ok(next) = tokio::time::timeout(IDLE_TIMEOUT, stream.next()).await else {
                inner.set_error(format!("nothing received for {} s", IDLE_TIMEOUT.as_secs()));
                break;
            };
            let Some(next) = next else {
                break;
            };
            if inner.aborted.load(Ordering::SeqCst) {
                inner.state.store(SSE_CLOSED, Ordering::SeqCst);
                return;
            }
            match next {
                Ok(chunk) => {
                    let events = parser.push(&chunk);
                    if let Some(ms) = parser.retry_ms.take() {
                        inner.retry_ms.store(ms, Ordering::SeqCst);
                    }
                    if !inner.deliver(events).await {
                        inner.state.store(SSE_CLOSED, Ordering::SeqCst);
                        return;
                    }
                }
                Err(e) => {
                    if inner.aborted.load(Ordering::SeqCst) {
                        inner.state.store(SSE_CLOSED, Ordering::SeqCst);
                        return;
                    }
                    inner.set_error(e.to_string());
                    break;
                }
            }
        }

        if inner.aborted.load(Ordering::SeqCst) {
            inner.state.store(SSE_CLOSED, Ordering::SeqCst);
            return;
        }
        // Stream ended — reconnect unless the guest closed it.
        sleep_retry(&inner).await;
    }
}

async fn sleep_retry(inner: &SseInner) {
    let ms = inner.retry_ms.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Register `api_sse_*` host functions.
pub fn register_sse_functions(linker: &mut Linker<HostState>) -> Result<()> {
    linker.func_wrap(
        "oxide",
        "api_sse_open",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> u32 {
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            if url.is_empty() {
                return 0;
            }
            let state = caller.data();
            match with_started(
                &state.sse,
                || SseState::of(state),
                |sse| sse.open(url.clone()),
            ) {
                Some(Err(e)) => {
                    state.log(ConsoleLevel::Error, format!("[SSE] Refused: {e}"));
                    0
                }
                Some(Ok(id)) => {
                    state.log(ConsoleLevel::Log, format!("[SSE] Opening {url} (id={id})"));
                    id
                }
                None => {
                    state.log(ConsoleLevel::Error, "[SSE] Init failed");
                    0
                }
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_sse_state",
        |caller: Caller<'_, HostState>, id: u32| -> u32 {
            with(&caller.data().sse, |sse| sse.state(id)).unwrap_or(SSE_CLOSED)
        },
    )?;

    // Writes the next event (see `encode_event`) and returns its size, or returns the size it
    // needs without taking it when it doesn't fit in `out_cap`, or a negative `RECV_*` code.
    linker.func_wrap(
        "oxide",
        "api_sse_recv",
        |mut caller: Caller<'_, HostState>, id: u32, out_ptr: u32, out_cap: u32| -> i64 {
            let next = with(&caller.data().sse, |sse| match sse.peek_front(id) {
                Some(event) => Ok(encode_event(&event)),
                None => Err(sse.empty_recv_code(id)),
            });
            let encoded = match next {
                None => return RECV_UNKNOWN,
                Some(Err(code)) => return code,
                Some(Ok(encoded)) => encoded,
            };
            if encoded.len() > out_cap as usize {
                return encoded.len() as i64;
            }
            // Consume now that we know it fits.
            with(&caller.data().sse, |sse| sse.pop(id));
            if !write_guest(&mut caller, out_ptr, &encoded) {
                return RECV_ERROR;
            }
            encoded.len() as i64
        },
    )?;

    // Writes the last error and returns its length; 0 when there is none, or its length
    // without writing it when it doesn't fit in `out_cap`.
    linker.func_wrap(
        "oxide",
        "api_sse_error",
        |mut caller: Caller<'_, HostState>, id: u32, out_ptr: u32, out_cap: u32| -> i32 {
            let message = with(&caller.data().sse, |sse| sse.error(id))
                .flatten()
                .unwrap_or_default();
            if message.len() > out_cap as usize {
                return message.len() as i32;
            }
            if !write_guest(&mut caller, out_ptr, message.as_bytes()) {
                return 0;
            }
            message.len() as i32
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_sse_close",
        |caller: Caller<'_, HostState>, id: u32| -> i32 {
            i32::from(with(&caller.data().sse, |sse| sse.close(id)) == Some(true))
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_sse_remove",
        |caller: Caller<'_, HostState>, id: u32| {
            with(&caller.data().sse, |sse| sse.remove(id));
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_message() {
        let mut p = SseParser::new();
        let ev = p.push(b"data: hello\n\n");
        assert_eq!(
            ev,
            vec![SseEvent {
                name: "message".into(),
                id: String::new(),
                data: "hello".into(),
            }]
        );
    }

    #[test]
    fn parse_multiline_data_and_name() {
        let mut p = SseParser::new();
        let ev = p.push(b"event: ping\ndata: one\ndata: two\nid: 7\n\n");
        assert_eq!(
            ev,
            vec![SseEvent {
                name: "ping".into(),
                id: "7".into(),
                data: "one\ntwo".into(),
            }]
        );
    }

    #[test]
    fn comments_and_retry_are_handled() {
        let mut p = SseParser::new();
        let ev = p.push(b": keep-alive\nretry: 2500\ndata: x\n\n");
        assert_eq!(p.retry_ms, Some(2500));
        assert_eq!(ev[0].data, "x");
    }

    #[test]
    fn empty_dispatch_emits_nothing() {
        let mut p = SseParser::new();
        assert!(p.push(b"event: ignored\n\n").is_empty());
    }

    #[test]
    fn incremental_chunks_across_lines() {
        let mut p = SseParser::new();
        assert!(p.push(b"data: hel").is_empty());
        assert!(p.push(b"lo\n").is_empty());
        let ev = p.push(b"\n");
        assert_eq!(ev[0].data, "hello");
    }

    #[test]
    fn overlong_lines_and_events_are_dropped_as_they_arrive() {
        let mut p = SseParser::new();
        // A line that never ends keeps at most a line's worth.
        let chunk = vec![b'x'; 64 * 1024];
        for _ in 0..64 {
            assert!(p.push(&chunk).is_empty());
            assert!(p.buf.len() <= MAX_EVENT_BYTES + chunk.len());
        }
        // Its end is dropped with it, and the stream goes on.
        let ev = p.push(b"yyy\ndata: after\n\n");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].data, "after");
        // An event of many short lines is dropped once it grows too large.
        let line = format!("data: {}\n", "z".repeat(1000));
        for _ in 0..1000 {
            assert!(p.push(line.as_bytes()).is_empty());
            assert!(p.data_bytes <= MAX_EVENT_BYTES + 1 + line.len());
        }
        assert!(p.push(b"\n").is_empty());
        assert_eq!(p.push(b"data: next\n\n")[0].data, "next");
    }

    #[test]
    fn crlf_lines() {
        let mut p = SseParser::new();
        let ev = p.push(b"data: crlf\r\n\r\n");
        assert_eq!(ev[0].data, "crlf");
        // A CRLF split between two chunks is one line end, not two.
        assert!(p.push(b"data: split\r").is_empty());
        assert!(p.push(b"\ndata: more\r").is_empty());
        let ev = p.push(b"\n\r\n");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].data, "split\nmore");
    }

    /// A chunk of many lines is parsed in time proportional to its size.
    #[test]
    fn many_lines_in_one_chunk_are_parsed_in_linear_time() {
        let chunk = ": keep-alive\n".repeat(400_000) + "data: last\n\n";
        let mut p = SseParser::new();
        let started = std::time::Instant::now();
        let ev = p.push(chunk.as_bytes());
        assert_eq!(ev.len(), 1);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_page_from_the_web_cannot_open_local_streams() {
        let policy = NetPolicy::for_page("https://a.test/app.wasm", Some([1, 1, 1, 1].into()));
        let net = Arc::new(Net::new().unwrap());
        let mut sse = SseState::new(net, policy, NetBytes::with_limit(1024));
        assert!(sse.open("http://127.0.0.1:1/".into()).is_err());
        assert!(sse.open("file:///etc/passwd".into()).is_err());
    }

    #[test]
    fn events_wait_for_room_in_the_documents_budget() {
        let inner = SseInner::new(NetBytes::with_limit(9));
        let event = |data: &str| SseEvent {
            name: String::new(),
            id: String::new(),
            data: data.into(),
        };
        let left = inner.push_events(VecDeque::from([event("12345"), event("678"), event("90")]));
        assert_eq!(left, [event("90")]);
        inner.events.lock().unwrap().pop();
        assert!(inner.push_events(left).is_empty());
    }

    #[test]
    fn encode_roundtrip_layout() {
        let ev = SseEvent {
            name: "update".into(),
            id: "42".into(),
            data: "abc".into(),
        };
        let bytes = encode_event(&ev);
        assert_eq!(&bytes[0..2], &6u16.to_le_bytes());
        assert_eq!(&bytes[2..8], b"update");
    }
}
