//! Host-side WebSocket connections for Oxide guest modules.
//!
//! Guests call the `api_ws_*` imports to open connections, send messages, poll
//! for incoming messages, query connection state, and close connections.
//! All I/O is non-blocking from the guest's perspective: the host spins up a
//! tokio task per connection that drives the underlying `tokio-tungstenite`
//! stream, pushes received frames into a `VecDeque`, and forwards outgoing
//! frames from an mpsc channel.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use wasmtime::{Caller, Linker};

use crate::capabilities::{
    guest_bytes, guest_str, next_handle, with, with_started, write_prefix, ConsoleLevel, HostState,
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
    /// Incoming frames pushed by the reader task, drained by `api_ws_recv`.
    recv_queue: Arc<Mutex<VecDeque<RecvMsg>>>,
    /// Current connection state (one of the `WS_*` constants above).
    ready_state: Arc<Mutex<u32>>,
}

/// All WebSocket state for a tab. Lazily initialised on the first `api_ws_*` call.
pub struct WsState {
    runtime: Runtime,
    connections: HashMap<u32, WsConn>,
    last_id: u32,
}

impl WsState {
    pub fn new() -> Option<Self> {
        let runtime = Runtime::new().ok()?;
        Some(Self {
            runtime,
            connections: HashMap::new(),
            last_id: 0,
        })
    }

    /// Open a new WebSocket connection to `url`.
    ///
    /// Returns a handle (`> 0`) on success or `0` if the URL is invalid.
    /// The actual TCP/TLS handshake happens asynchronously; poll
    /// [`WsState::ready_state`] until it reaches [`WS_OPEN`].
    fn connect(&mut self, url: &str) -> u32 {
        let url = url.to_string();
        let id = next_handle(&mut self.last_id);

        let ready_state = Arc::new(Mutex::new(WS_CONNECTING));
        let recv_queue: Arc<Mutex<VecDeque<RecvMsg>>> = Arc::new(Mutex::new(VecDeque::new()));
        let (send_tx, mut send_rx) = mpsc::unbounded_channel::<SendMsg>();

        let rs = ready_state.clone();
        let rq = recv_queue.clone();

        self.runtime.spawn(async move {
            let ws_stream = match connect_async(&url).await {
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
                tokio::select! {
                    // Incoming frame from the remote server.
                    msg = reader.next() => {
                        match msg {
                            Some(Ok(Message::Text(text))) => {
                                rq.lock().unwrap().push_back(RecvMsg {
                                    is_binary: false,
                                    data: text.into_bytes(),
                                });
                            }
                            Some(Ok(Message::Binary(bytes))) => {
                                rq.lock().unwrap().push_back(RecvMsg {
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
                recv_queue,
                ready_state,
            },
        );

        id
    }

    fn send(&self, id: u32, data: Vec<u8>, is_binary: bool) -> bool {
        if let Some(conn) = self.connections.get(&id) {
            conn.send_tx.send(SendMsg::Data { is_binary, data }).is_ok()
        } else {
            false
        }
    }

    fn recv(&self, id: u32) -> Option<RecvMsg> {
        self.connections
            .get(&id)?
            .recv_queue
            .lock()
            .unwrap()
            .pop_front()
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
    //   Returns 0 on success, -1 if the connection is unknown or closed.
    for (name, is_binary) in [("api_ws_send_text", false), ("api_ws_send_binary", true)] {
        linker.func_wrap(
            "oxide",
            name,
            move |caller: Caller<'_, HostState>, id: u32, ptr: u32, len: u32| -> i32 {
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
