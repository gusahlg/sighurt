//! Host-side WebSocket connections for Oxide guest modules.
//!
//! Guests call the `api_ws_*` imports to open connections, send messages, poll
//! for incoming messages, query connection state, and close connections.
//! All I/O is non-blocking from the guest's perspective: the host spins up a
//! tokio task per connection that drives the underlying `tokio-tungstenite`
//! stream, pushes received frames into a `VecDeque`, and forwards outgoing
//! frames from an mpsc channel.
//!
//! What a guest can make the host keep is bounded: at most [`MAX_CONNECTIONS`] connections
//! (until it removes them), messages of at most [`MAX_MESSAGE`] bytes, and per connection at
//! most [`MAX_QUEUED_BYTES`] of received messages it hasn't read (and one message more), past
//! which the task stops reading from the network until the guest catches up, and as much of
//! messages waiting to be sent, past which sending fails.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::sync::Notify;
use tokio_tungstenite::connect_async_with_config;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    guest_bytes, guest_str, next_handle, with, with_started, write_prefix, ConsoleLevel, HostState,
    Runtime,
};

// ── Ready-state constants (mirrors browser WebSocket.readyState) ──────────

/// Connection is being established.
pub const WS_CONNECTING: u32 = 0;
/// Connection is open and ready to communicate.
pub const WS_OPEN: u32 = 1;
/// Connection is in the process of closing.
pub const WS_CLOSING: u32 = 2;
/// Connection is closed or could not be opened.
pub const WS_CLOSED: u32 = 3;

/// Most connections a guest may have.
pub const MAX_CONNECTIONS: usize = 256;
/// Largest message received.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// Most bytes of received messages held for one connection before reading stops, and of
/// messages waiting to be sent.
pub const MAX_QUEUED_BYTES: usize = 16 * 1024 * 1024;

/// A queued incoming message (text or binary frame).
struct RecvMsg {
    is_binary: bool,
    data: Vec<u8>,
}

/// An outgoing message queued by the guest for the writer task.
enum SendMsg {
    Data { is_binary: bool, data: Vec<u8> },
    Close,
}

/// Per-connection state shared between the host API and the background task.
struct WsConn {
    /// Sender half of the outgoing message channel consumed by the writer task.
    send_tx: mpsc::UnboundedSender<SendMsg>,
    /// Bytes of messages in `send_tx`.
    sending: Arc<AtomicUsize>,
    /// Incoming frames pushed by the reader task, drained by `api_ws_recv`.
    recv_queue: Arc<Mutex<Received>>,
    /// Signalled when the guest reads, for a reader waiting for room.
    drained: Arc<Notify>,
    /// Current connection state (one of the `WS_*` constants above).
    ready_state: Arc<Mutex<u32>>,
}

/// Received messages the guest hasn't read.
#[derive(Default)]
struct Received {
    messages: VecDeque<RecvMsg>,
    bytes: usize,
}

impl Received {
    fn push(&mut self, message: RecvMsg) {
        self.bytes += message.data.len();
        self.messages.push_back(message);
    }
}

/// All WebSocket state for a tab. Lazily initialised on the first `api_ws_*` call.
pub struct WsState {
    runtime: Runtime,
    connections: HashMap<u32, WsConn>,
    last_id: u32,
}

impl WsState {
    pub fn new() -> Option<Self> {
        let runtime = Runtime::new()?;
        Some(Self {
            runtime,
            connections: HashMap::new(),
            last_id: 0,
        })
    }

    /// Open a new WebSocket connection to `url`.
    ///
    /// Returns a handle (`> 0`), or `0` if the guest already has [`MAX_CONNECTIONS`].
    /// The actual TCP/TLS handshake happens asynchronously; poll
    /// [`WsState::ready_state`] until it reaches [`WS_OPEN`].
    fn connect(&mut self, url: &str) -> u32 {
        if self.connections.len() >= MAX_CONNECTIONS {
            return 0;
        }
        let url = url.to_string();
        let id = next_handle(&mut self.last_id);

        let ready_state = Arc::new(Mutex::new(WS_CONNECTING));
        let recv_queue: Arc<Mutex<Received>> = Arc::default();
        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<SendMsg>();
        let (sending, drained) = (Arc::new(AtomicUsize::new(0)), Arc::new(Notify::new()));

        let rs = ready_state.clone();
        let rq = recv_queue.clone();
        let (sent, read) = (sending.clone(), drained.clone());
        let config = WebSocketConfig {
            max_message_size: Some(MAX_MESSAGE),
            max_frame_size: Some(MAX_MESSAGE),
            ..WebSocketConfig::default()
        };

        self.runtime.spawn(async move {
            let ws_stream = match connect_async_with_config(&url, Some(config), false).await {
                Ok((stream, _)) => stream,
                Err(_) => {
                    *rs.lock().unwrap() = WS_CLOSED;
                    return;
                }
            };

            *rs.lock().unwrap() = WS_OPEN;
            let (mut writer, mut reader) = ws_stream.split();

            // Drive reading and writing concurrently.
            loop {
                // Read no further while the guest is behind.
                let full = rq.lock().unwrap().bytes > MAX_QUEUED_BYTES;
                tokio::select! {
                    _ = read.notified(), if full => {}
                    // Incoming frame from the remote server.
                    msg = reader.next(), if !full => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                rq.lock().unwrap().push(RecvMsg {
                                    is_binary: false,
                                    data: text.into_bytes(),
                                });
                            }
                            Some(Ok(Message::Binary(bytes))) => {
                                rq.lock().unwrap().push(RecvMsg {
                                    is_binary: true,
                                    data: bytes.to_vec(),
                                });
                            }
                            Some(Ok(Message::Close(_))) | None => {
                                *rs.lock().unwrap() = WS_CLOSED;
                                break;
                            }
                            Some(Ok(Message::Ping(payload))) => {
                                let _ = writer.send(Message::Pong(payload)).await;
                            }
                            _ => {}
                        }
                    }
                    // Outgoing frame queued by the guest.
                    outgoing = send_rx.recv() => {
                        match outgoing {
                            Some(SendMsg::Data { is_binary, data }) => {
                                sent.fetch_sub(data.len(), Ordering::AcqRel);
                                let msg = if is_binary {
                                    Message::Binary(data)
                                } else {
                                    match String::from_utf8(data) {
                                        Ok(text) => Message::Text(text),
                                        Err(e) => Message::Binary(e.into_bytes()),
                                    }
                                };
                                if writer.send(msg).await.is_err() {
                                    *rs.lock().unwrap() = WS_CLOSED;
                                    break;
                                }
                            }
                            Some(SendMsg::Close) => {
                                *rs.lock().unwrap() = WS_CLOSING;
                                let _ = writer.send(Message::Close(None)).await;
                                *rs.lock().unwrap() = WS_CLOSED;
                                break;
                            }
                            None => {
                                // Channel closed — host dropped the connection handle.
                                *rs.lock().unwrap() = WS_CLOSED;
                                break;
                            }
                        }
                    }
                }
            }
        });

        self.connections.insert(
            id,
            WsConn {
                send_tx,
                sending,
                recv_queue,
                drained,
                ready_state,
            },
        );

        id
    }

    /// Queues `data` to send, unless [`MAX_QUEUED_BYTES`] are waiting already.
    fn send(&self, id: u32, data: Vec<u8>, is_binary: bool) -> bool {
        let Some(conn) = self.connections.get(&id) else {
            return false;
        };
        let len = data.len();
        let queued = conn.sending.fetch_add(len, Ordering::AcqRel);
        let sent = queued + len <= MAX_QUEUED_BYTES
            && conn.send_tx.send(SendMsg::Data { is_binary, data }).is_ok();
        if !sent {
            conn.sending.fetch_sub(len, Ordering::AcqRel);
        }
        sent
    }

    fn recv(&self, id: u32) -> Option<RecvMsg> {
        let conn = self.connections.get(&id)?;
        let mut received = conn.recv_queue.lock().unwrap();
        let message = received.messages.pop_front()?;
        received.bytes -= message.data.len();
        conn.drained.notify_one();
        Some(message)
    }

    fn ready_state(&self, id: u32) -> u32 {
        self.connections
            .get(&id)
            .map(|c| *c.ready_state.lock().unwrap())
            .unwrap_or(WS_CLOSED)
    }

    fn close(&mut self, id: u32) -> bool {
        if let Some(conn) = self.connections.get(&id) {
            *conn.ready_state.lock().unwrap() = WS_CLOSING;
            let _ = conn.send_tx.send(SendMsg::Close);
            true
        } else {
            false
        }
    }

    fn remove(&mut self, id: u32) {
        self.connections.remove(&id);
    }
}

/// Register all `api_ws_*` host functions on the given linker.
pub fn register_ws_functions(linker: &mut Linker<HostState>) -> Result<()> {
    // api_ws_connect(url_ptr: u32, url_len: u32) -> u32
    //   Returns a connection handle (> 0), or 0 on error.
    linker.func_wrap(
        "oxide",
        "api_ws_connect",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> u32 {
            let Some(url) = guest_str(&caller, url_ptr, url_len) else {
                return 0;
            };
            let state = caller.data();
            match with_started(&state.ws, WsState::new, |ws| ws.connect(&url)) {
                Some(0) => {
                    let message = format!("[WS] Refused: {MAX_CONNECTIONS} connections open");
                    state.log(ConsoleLevel::Error, message);
                    0
                }
                Some(id) => {
                    let message = format!("[WS] Connecting to {url} (id={id})");
                    state.log(ConsoleLevel::Log, message);
                    id
                }
                None => {
                    state.log(ConsoleLevel::Error, "[WS] Init failed");
                    0
                }
            }
        },
    )?;

    // api_ws_send_text / api_ws_send_binary(id: u32, data_ptr: u32, data_len: u32) -> i32
    //   Returns 0 on success, -1 if the connection is unknown or closed, or too much is
    //   waiting to be sent.
    for (name, is_binary) in [("api_ws_send_text", false), ("api_ws_send_binary", true)] {
        linker.func_wrap(
            "oxide",
            name,
            move |caller: Caller<'_, HostState>, id: u32, ptr: u32, len: u32| -> i32 {
                if len as usize > MAX_QUEUED_BYTES {
                    return -1;
                }
                let Some(data) = guest_bytes(&caller, ptr, len) else {
                    return -1;
                };
                if with(&caller.data().ws, |ws| ws.send(id, data, is_binary)) == Some(true) {
                    0
                } else {
                    -1
                }
            },
        )?;
    }

    // api_ws_recv(id: u32, out_ptr: u32, out_cap: u32) -> i64
    //
    // Dequeues one frame and writes its bytes into guest memory at `out_ptr`.
    //   -1          : no message available (queue is empty)
    //   >= 0        : low 32 bits = byte length written;
    //                 bit 32 set   = frame is binary (bit 32 = 0 → text)
    linker.func_wrap(
        "oxide",
        "api_ws_recv",
        |mut caller: Caller<'_, HostState>, id: u32, out_ptr: u32, out_cap: u32| -> i64 {
            let Some(msg) = with(&caller.data().ws, |ws| ws.recv(id)).flatten() else {
                return -1;
            };
            match write_prefix(&mut caller, out_ptr, out_cap, &msg.data) {
                Some(len) => i64::from(len) | (i64::from(msg.is_binary) << 32),
                None => -1,
            }
        },
    )?;

    // api_ws_ready_state(id: u32) -> u32
    //   0=CONNECTING  1=OPEN  2=CLOSING  3=CLOSED
    linker.func_wrap(
        "oxide",
        "api_ws_ready_state",
        |caller: Caller<'_, HostState>, id: u32| -> u32 {
            with(&caller.data().ws, |ws| ws.ready_state(id)).unwrap_or(WS_CLOSED)
        },
    )?;

    // api_ws_close(id: u32) -> i32
    //   Returns 1 if the close was initiated, 0 if the id is unknown.
    linker.func_wrap(
        "oxide",
        "api_ws_close",
        |caller: Caller<'_, HostState>, id: u32| -> i32 {
            i32::from(with(&caller.data().ws, |ws| ws.close(id)) == Some(true))
        },
    )?;

    // api_ws_remove(id: u32)
    //   Frees host-side resources for a closed connection.
    linker.func_wrap(
        "oxide",
        "api_ws_remove",
        |caller: Caller<'_, HostState>, id: u32| {
            with(&caller.data().ws, |ws| ws.remove(id));
        },
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    const MIB: usize = 1024 * 1024;

    /// A WebSocket server on a local port that sends `count` binary messages of a MiB, as fast
    /// as the connection takes them, then waits for the client to go.
    fn serve(count: usize) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                let (socket, _) = listener.accept().await.unwrap();
                let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
                for _ in 0..count {
                    if ws.send(Message::Binary(vec![7; MIB])).await.is_err() {
                        return;
                    }
                }
                while ws.next().await.is_some() {}
            });
        });
        format!("ws://{address}/")
    }

    #[test]
    fn messages_nobody_reads_are_held_back_not_kept() {
        let count = 64;
        let mut ws = WsState::new().unwrap();
        let id = ws.connect(&serve(count));
        let queued = || ws.connections[&id].recv_queue.lock().unwrap().bytes;
        let started = Instant::now();
        while queued() <= MAX_QUEUED_BYTES && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(300));
        let held = queued();
        let bound = MAX_QUEUED_BYTES + 1..=MAX_QUEUED_BYTES + MIB;
        assert!(bound.contains(&held), "{held}");
        // Reading lets the rest through.
        let started = Instant::now();
        let mut read = 0;
        while read < count {
            match ws.recv(id) {
                Some(message) => {
                    assert_eq!(message.data.len(), MIB);
                    read += 1;
                }
                None => std::thread::sleep(Duration::from_millis(1)),
            }
            assert!(started.elapsed() < Duration::from_secs(20), "{read} read");
        }
    }

    #[test]
    fn sending_more_than_the_queue_holds_fails() {
        // A port that takes the connection and never answers the handshake, so what is sent
        // waits.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mut ws = WsState::new().unwrap();
        let id = ws.connect(&format!("ws://{address}/"));
        let sent = (0..20)
            .take_while(|_| ws.send(id, vec![0; MIB], true))
            .count();
        assert_eq!(sent, MAX_QUEUED_BYTES / MIB);
        drop(listener);
    }
}
