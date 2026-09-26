//! Host-side WebRTC: peer connections, data channels, media tracks, and signaling.
//!
//! Guests call [`register_rtc_functions`] imports from the `oxide` module to create
//! peer-to-peer connections with SDP offer/answer exchange, ICE candidate trickle,
//! data channel messaging, and media track attachment. A lightweight HTTP-based
//! signaling client is included for bootstrapping connections.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use tokio::runtime::Runtime;
use wasmtime::{Caller, Linker};
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::data_channel::data_channel_init::RTCDataChannelInit;
use webrtc::data_channel::data_channel_message::DataChannelMessage;
use webrtc::data_channel::RTCDataChannel;
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::RTCRtpCodecCapability;
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::TrackLocal;

use crate::capabilities::{
    guest_bytes, guest_str, next_handle, with, with_started, write_prefix, ConsoleLevel, HostState,
};

/// Incoming message queued by a data channel's on_message callback.
struct IncomingMessage {
    channel_id: u32,
    is_binary: bool,
    data: Vec<u8>,
}

/// Metadata about a remotely-created data channel that the guest hasn't accepted yet.
struct PendingChannel {
    channel_id: u32,
    label: String,
}

/// Metadata about a remote media track received via `on_track`.
struct PendingTrack {
    kind: u32,
    id: String,
    stream_id: String,
}

/// Per-peer state: the connection object plus event queues polled by the guest.
struct PeerState {
    conn: Arc<RTCPeerConnection>,
    data_channels: Arc<Mutex<HashMap<u32, Arc<RTCDataChannel>>>>,
    incoming_messages: Arc<Mutex<VecDeque<IncomingMessage>>>,
    pending_channels: Arc<Mutex<VecDeque<PendingChannel>>>,
    pending_tracks: Arc<Mutex<VecDeque<PendingTrack>>>,
    ice_candidates: Arc<Mutex<VecDeque<String>>>,
    connection_state: Arc<Mutex<u32>>,
    /// Next id for a data channel (ours or the remote peer's) or track.
    next_channel_id: Arc<AtomicU32>,
}

/// HTTP-based signaling session for bootstrapping peer connections.
struct SignalingSession {
    base_url: String,
    room: String,
    client: reqwest::blocking::Client,
}

/// All RTC state for a tab. Lazily initialised on first `api_rtc_*` call.
pub struct RtcState {
    runtime: Runtime,
    peers: HashMap<u32, PeerState>,
    last_peer_id: u32,
    signaling: Option<SignalingSession>,
}

/// Connection state constants exposed to guests.
const STATE_NEW: u32 = 0;
const STATE_CONNECTING: u32 = 1;
const STATE_CONNECTED: u32 = 2;
const STATE_DISCONNECTED: u32 = 3;
const STATE_FAILED: u32 = 4;
const STATE_CLOSED: u32 = 5;

fn map_connection_state(s: RTCPeerConnectionState) -> u32 {
    match s {
        RTCPeerConnectionState::New => STATE_NEW,
        RTCPeerConnectionState::Connecting => STATE_CONNECTING,
        RTCPeerConnectionState::Connected => STATE_CONNECTED,
        RTCPeerConnectionState::Disconnected => STATE_DISCONNECTED,
        RTCPeerConnectionState::Failed => STATE_FAILED,
        RTCPeerConnectionState::Closed => STATE_CLOSED,
        _ => STATE_NEW,
    }
}

/// Queues the messages arriving on `dc` for the guest as coming from `channel_id`.
fn deliver_messages(
    dc: &RTCDataChannel,
    channel_id: u32,
    queue: Arc<Mutex<VecDeque<IncomingMessage>>>,
) {
    dc.on_message(Box::new(move |msg: DataChannelMessage| {
        queue.lock().unwrap().push_back(IncomingMessage {
            channel_id,
            is_binary: !msg.is_string,
            data: msg.data.to_vec(),
        });
        Box::pin(async {})
    }));
}

impl RtcState {
    pub fn new() -> Option<Self> {
        let runtime = Runtime::new().ok()?;
        Some(Self {
            runtime,
            peers: HashMap::new(),
            last_peer_id: 0,
            signaling: None,
        })
    }

    fn peer(&self, peer_id: u32) -> Result<&PeerState> {
        self.peers
            .get(&peer_id)
            .ok_or_else(|| anyhow::anyhow!("unknown peer"))
    }

    fn create_peer(&mut self, stun_urls: Vec<String>) -> Result<u32> {
        let urls = if stun_urls.is_empty() {
            vec!["stun:stun.l.google.com:19302".to_string()]
        } else {
            stun_urls
        };
        let config = RTCConfiguration {
            ice_servers: vec![RTCIceServer {
                urls,
                ..Default::default()
            }],
            ..Default::default()
        };

        let conn = Arc::new(self.runtime.block_on(async {
            let mut me = MediaEngine::default();
            me.register_default_codecs()?;
            let registry = register_default_interceptors(Registry::new(), &mut me)?;
            let api = APIBuilder::new()
                .with_media_engine(me)
                .with_interceptor_registry(registry)
                .build();
            api.new_peer_connection(config).await
        })?);
        let peer = PeerState {
            conn: conn.clone(),
            data_channels: Default::default(),
            incoming_messages: Default::default(),
            pending_channels: Default::default(),
            pending_tracks: Default::default(),
            ice_candidates: Default::default(),
            connection_state: Arc::new(Mutex::new(STATE_NEW)),
            next_channel_id: Arc::new(AtomicU32::new(1)),
        };

        let state = peer.connection_state.clone();
        conn.on_peer_connection_state_change(Box::new(move |s| {
            *state.lock().unwrap() = map_connection_state(s);
            Box::pin(async {})
        }));

        let ice = peer.ice_candidates.clone();
        conn.on_ice_candidate(Box::new(move |c| {
            if let Some(candidate) = c {
                if let Ok(json) = serde_json::to_string(&candidate.to_json().unwrap_or_default()) {
                    ice.lock().unwrap().push_back(json);
                }
            }
            Box::pin(async {})
        }));

        // Data channels the remote peer opens.
        let pending = peer.pending_channels.clone();
        let messages = peer.incoming_messages.clone();
        let channels = peer.data_channels.clone();
        let next_id = peer.next_channel_id.clone();
        conn.on_data_channel(Box::new(move |dc| {
            let channel_id = next_id.fetch_add(1, Ordering::Relaxed);
            pending.lock().unwrap().push_back(PendingChannel {
                channel_id,
                label: dc.label().to_string(),
            });
            deliver_messages(&dc, channel_id, messages.clone());
            channels.lock().unwrap().insert(channel_id, dc);
            Box::pin(async {})
        }));

        let tracks = peer.pending_tracks.clone();
        conn.on_track(Box::new(move |track, _receiver, _transceiver| {
            let kind = match track.kind() {
                webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Audio => 0,
                webrtc::rtp_transceiver::rtp_codec::RTPCodecType::Video => 1,
                _ => 2,
            };
            tracks.lock().unwrap().push_back(PendingTrack {
                kind,
                id: track.id().to_string(),
                stream_id: track.stream_id().to_string(),
            });
            Box::pin(async {})
        }));

        let peer_id = next_handle(&mut self.last_peer_id);
        self.peers.insert(peer_id, peer);
        Ok(peer_id)
    }

    fn close_peer(&mut self, peer_id: u32) -> bool {
        let Some(peer) = self.peers.remove(&peer_id) else {
            return false;
        };
        let _ = self.runtime.block_on(peer.conn.close());
        true
    }

    /// Creates an offer, or an answer, and makes it the local description.
    fn create_sdp(&self, peer_id: u32, answer: bool) -> Result<String> {
        let conn = &self.peer(peer_id)?.conn;
        let sdp = self.runtime.block_on(async {
            if answer {
                conn.create_answer(None).await
            } else {
                conn.create_offer(None).await
            }
        })?;
        self.runtime
            .block_on(conn.set_local_description(sdp.clone()))?;
        Ok(sdp.sdp)
    }

    fn set_description(&self, peer_id: u32, sdp: &str, is_offer: bool, remote: bool) -> Result<()> {
        let conn = &self.peer(peer_id)?.conn;
        let desc = if is_offer {
            RTCSessionDescription::offer(sdp.to_string())?
        } else {
            RTCSessionDescription::answer(sdp.to_string())?
        };
        if remote {
            self.runtime.block_on(conn.set_remote_description(desc))?;
        } else {
            self.runtime.block_on(conn.set_local_description(desc))?;
        }
        Ok(())
    }

    fn add_ice_candidate(&self, peer_id: u32, candidate_json: &str) -> Result<()> {
        let conn = &self.peer(peer_id)?.conn;
        let init: RTCIceCandidateInit = serde_json::from_str(candidate_json)?;
        self.runtime.block_on(conn.add_ice_candidate(init))?;
        Ok(())
    }

    fn connection_state(&self, peer_id: u32) -> u32 {
        self.peers
            .get(&peer_id)
            .map(|p| *p.connection_state.lock().unwrap())
            .unwrap_or(STATE_CLOSED)
    }

    fn poll_ice_candidate(&self, peer_id: u32) -> Option<String> {
        let peer = self.peers.get(&peer_id)?;
        peer.ice_candidates.lock().unwrap().pop_front()
    }

    fn create_data_channel(&self, peer_id: u32, label: &str, ordered: bool) -> Result<u32> {
        let peer = self.peer(peer_id)?;
        let options = (!ordered).then(|| RTCDataChannelInit {
            ordered: Some(false),
            ..Default::default()
        });
        let dc = self
            .runtime
            .block_on(peer.conn.create_data_channel(label, options))?;
        let channel_id = peer.next_channel_id.fetch_add(1, Ordering::Relaxed);
        deliver_messages(&dc, channel_id, peer.incoming_messages.clone());
        peer.data_channels.lock().unwrap().insert(channel_id, dc);
        Ok(channel_id)
    }

    fn send_data(&self, peer_id: u32, channel_id: u32, data: &[u8], is_binary: bool) -> Result<()> {
        let dc = self
            .peer(peer_id)?
            .data_channels
            .lock()
            .unwrap()
            .get(&channel_id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("unknown channel"))?;
        if is_binary {
            self.runtime
                .block_on(dc.send(&bytes::Bytes::copy_from_slice(data)))?;
        } else {
            let text = String::from_utf8_lossy(data).to_string();
            self.runtime.block_on(dc.send_text(text))?;
        }
        Ok(())
    }

    /// The next message from `channel_id`, or from any channel when it is 0.
    fn recv(&self, peer_id: u32, channel_id: u32) -> Option<IncomingMessage> {
        let peer = self.peers.get(&peer_id)?;
        let mut queue = peer.incoming_messages.lock().unwrap();
        let pos = queue
            .iter()
            .position(|m| channel_id == 0 || m.channel_id == channel_id)?;
        queue.remove(pos)
    }

    fn poll_new_channel(&self, peer_id: u32) -> Option<PendingChannel> {
        let peer = self.peers.get(&peer_id)?;
        peer.pending_channels.lock().unwrap().pop_front()
    }

    fn poll_track(&self, peer_id: u32) -> Option<PendingTrack> {
        let peer = self.peers.get(&peer_id)?;
        peer.pending_tracks.lock().unwrap().pop_front()
    }

    fn add_track(&self, peer_id: u32, kind: u32) -> Result<u32> {
        let peer = self.peer(peer_id)?;
        let handle = peer.next_channel_id.fetch_add(1, Ordering::Relaxed);
        let (mime, name) = if kind == 0 {
            (webrtc::api::media_engine::MIME_TYPE_OPUS, "audio")
        } else {
            (webrtc::api::media_engine::MIME_TYPE_VP8, "video")
        };
        let track = Arc::new(TrackLocalStaticRTP::new(
            RTCRtpCodecCapability {
                mime_type: mime.to_string(),
                ..Default::default()
            },
            format!("track-{kind}-{handle}"),
            format!("sighurt-{name}"),
        ));
        self.runtime.block_on(
            peer.conn
                .add_track(track as Arc<dyn TrackLocal + Send + Sync>),
        )?;
        Ok(handle)
    }

    // ── Signaling helpers ───────────────────────────────────────────

    fn signal_connect(&mut self, url: &str) {
        self.signaling = Some(SignalingSession {
            base_url: url.trim_end_matches('/').to_string(),
            room: String::new(),
            client: reqwest::blocking::Client::new(),
        });
    }

    fn signal_join_room(&mut self, room: &str) -> bool {
        let Some(sig) = &mut self.signaling else {
            return false;
        };
        sig.room = room.to_string();
        let _ = sig
            .client
            .post(format!("{}/rooms/{room}/join", sig.base_url))
            .send();
        true
    }

    /// The signaling session and the URL to exchange messages at.
    fn signal_endpoint(&self) -> Option<(&SignalingSession, String)> {
        let sig = self.signaling.as_ref()?;
        let url = if sig.room.is_empty() {
            format!("{}/signal", sig.base_url)
        } else {
            format!("{}/rooms/{}/signal", sig.base_url, sig.room)
        };
        Some((sig, url))
    }

    fn signal_send(&self, data: &[u8]) -> bool {
        self.signal_endpoint().is_some_and(|(sig, url)| {
            sig.client
                .post(url)
                .header("Content-Type", "application/json")
                .body(data.to_vec())
                .send()
                .is_ok()
        })
    }

    fn signal_recv(&self) -> Option<Vec<u8>> {
        let (sig, url) = self.signal_endpoint()?;
        let resp = sig.client.get(url).send().ok()?;
        if !resp.status().is_success() {
            return None;
        }
        resp.bytes().ok().map(|b| b.to_vec())
    }
}

/// The guest's code for an RTC call without a result: 0 on success, -1 before any peer was
/// created, -2 on failure (logged as `what`).
fn status(state: &HostState, what: &str, result: Option<Result<()>>) -> i32 {
    match result {
        Some(Ok(())) => 0,
        Some(Err(e)) => {
            state.log(ConsoleLevel::Error, format!("[RTC] {what}: {e}"));
            -2
        }
        None => -1,
    }
}

/// A handle from an RTC call, or 0 (logged as `what` when the call failed).
fn handle(state: &HostState, what: &str, result: Option<Result<u32>>) -> u32 {
    match result {
        Some(Ok(id)) => id,
        Some(Err(e)) => {
            state.log(ConsoleLevel::Error, format!("[RTC] {what}: {e}"));
            0
        }
        None => 0,
    }
}

/// Writes the answer of a polling RTC call: its length, 0 when there was nothing, -1 before
/// any peer was created, -4 when the buffer is outside guest memory.
fn write_polled(
    caller: &mut Caller<'_, HostState>,
    out_ptr: u32,
    out_cap: u32,
    item: Option<Option<Vec<u8>>>,
) -> i32 {
    match item {
        None => -1,
        Some(None) => 0,
        Some(Some(bytes)) => {
            write_prefix(caller, out_ptr, out_cap, &bytes).map_or(-4, |n| n as i32)
        }
    }
}

/// Register all `api_rtc_*` host functions on the given linker.
pub fn register_rtc_functions(linker: &mut Linker<HostState>) -> Result<()> {
    // ── Peer Connection ──────────────────────────────────────────

    linker.func_wrap(
        "oxide",
        "api_rtc_create_peer",
        |caller: Caller<'_, HostState>, stun_ptr: u32, stun_len: u32| -> u32 {
            let stun = guest_str(&caller, stun_ptr, stun_len).unwrap_or_default();
            let stun_urls: Vec<String> = stun
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|_| !stun.is_empty())
                .collect();
            let state = caller.data();
            match with_started(&state.rtc, RtcState::new, |r| r.create_peer(stun_urls)) {
                Some(Ok(id)) => {
                    state.log(ConsoleLevel::Log, format!("[RTC] Peer {id} created"));
                    id
                }
                Some(Err(e)) => {
                    state.log(ConsoleLevel::Error, format!("[RTC] Create peer: {e}"));
                    0
                }
                None => {
                    state.log(ConsoleLevel::Error, "[RTC] Init failed");
                    0
                }
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_close_peer",
        |caller: Caller<'_, HostState>, peer_id: u32| -> u32 {
            u32::from(with(&caller.data().rtc, |r| r.close_peer(peer_id)) == Some(true))
        },
    )?;

    // ── SDP Offer / Answer ───────────────────────────────────────

    for (name, what, answer) in [
        ("api_rtc_create_offer", "Offer", false),
        ("api_rtc_create_answer", "Answer", true),
    ] {
        linker.func_wrap(
            "oxide",
            name,
            move |mut caller: Caller<'_, HostState>,
                  peer_id: u32,
                  out_ptr: u32,
                  out_cap: u32|
                  -> i32 {
                let sdp = with(&caller.data().rtc, |r| r.create_sdp(peer_id, answer));
                match sdp {
                    Some(Ok(sdp)) => {
                        write_polled(&mut caller, out_ptr, out_cap, Some(Some(sdp.into_bytes())))
                    }
                    Some(Err(e)) => {
                        caller
                            .data()
                            .log(ConsoleLevel::Error, format!("[RTC] {what}: {e}"));
                        -2
                    }
                    None => -1,
                }
            },
        )?;
    }

    // ── SDP set local/remote ─────────────────────────────────────

    for (name, what, remote) in [
        ("api_rtc_set_local_description", "Set local desc", false),
        ("api_rtc_set_remote_description", "Set remote desc", true),
    ] {
        linker.func_wrap(
            "oxide",
            name,
            move |caller: Caller<'_, HostState>,
                  peer_id: u32,
                  sdp_ptr: u32,
                  sdp_len: u32,
                  is_offer: u32|
                  -> i32 {
                let sdp = guest_str(&caller, sdp_ptr, sdp_len).unwrap_or_default();
                let result = with(&caller.data().rtc, |r| {
                    r.set_description(peer_id, &sdp, is_offer != 0, remote)
                });
                status(caller.data(), what, result)
            },
        )?;
    }

    // ── ICE Candidates ───────────────────────────────────────────

    linker.func_wrap(
        "oxide",
        "api_rtc_add_ice_candidate",
        |caller: Caller<'_, HostState>, peer_id: u32, cand_ptr: u32, cand_len: u32| -> i32 {
            let candidate = guest_str(&caller, cand_ptr, cand_len).unwrap_or_default();
            let result = with(&caller.data().rtc, |r| {
                r.add_ice_candidate(peer_id, &candidate)
            });
            status(caller.data(), "Add ICE candidate", result)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_connection_state",
        |caller: Caller<'_, HostState>, peer_id: u32| -> u32 {
            with(&caller.data().rtc, |r| r.connection_state(peer_id)).unwrap_or(STATE_CLOSED)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_poll_ice_candidate",
        |mut caller: Caller<'_, HostState>, peer_id: u32, out_ptr: u32, out_cap: u32| -> i32 {
            let candidate = with(&caller.data().rtc, |r| {
                r.poll_ice_candidate(peer_id).map(String::into_bytes)
            });
            write_polled(&mut caller, out_ptr, out_cap, candidate)
        },
    )?;

    // ── Data Channels ────────────────────────────────────────────

    linker.func_wrap(
        "oxide",
        "api_rtc_create_data_channel",
        |caller: Caller<'_, HostState>,
         peer_id: u32,
         label_ptr: u32,
         label_len: u32,
         ordered: u32|
         -> u32 {
            let label = guest_str(&caller, label_ptr, label_len).unwrap_or_default();
            let result = with(&caller.data().rtc, |r| {
                r.create_data_channel(peer_id, &label, ordered != 0)
            });
            handle(caller.data(), "Create data channel", result)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_send",
        |caller: Caller<'_, HostState>,
         peer_id: u32,
         channel_id: u32,
         data_ptr: u32,
         data_len: u32,
         is_binary: u32|
         -> i32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            let result = with(&caller.data().rtc, |r| {
                r.send_data(peer_id, channel_id, &data, is_binary != 0)
            });
            match status(caller.data(), "Send", result) {
                0 => data.len() as i32,
                code => code,
            }
        },
    )?;

    // api_rtc_recv(peer, channel, out_ptr, out_cap) -> i64: the next message (from any
    // channel when `channel` is 0) as `channel << 48 | is_binary << 32 | length`; 0 when there
    // is none, -1 before any peer was created, -4 when the buffer is outside guest memory.
    linker.func_wrap(
        "oxide",
        "api_rtc_recv",
        |mut caller: Caller<'_, HostState>,
         peer_id: u32,
         channel_id: u32,
         out_ptr: u32,
         out_cap: u32|
         -> i64 {
            let msg = with(&caller.data().rtc, |r| r.recv(peer_id, channel_id));
            match msg {
                None => -1,
                Some(None) => 0,
                Some(Some(m)) => match write_prefix(&mut caller, out_ptr, out_cap, &m.data) {
                    Some(len) => {
                        ((u64::from(m.channel_id) << 48)
                            | (u64::from(m.is_binary) << 32)
                            | u64::from(len)) as i64
                    }
                    None => -4,
                },
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_poll_data_channel",
        |mut caller: Caller<'_, HostState>, peer_id: u32, out_ptr: u32, out_cap: u32| -> i32 {
            let channel = with(&caller.data().rtc, |r| {
                let ch = r.poll_new_channel(peer_id)?;
                Some(format!("{}:{}", ch.channel_id, ch.label).into_bytes())
            });
            write_polled(&mut caller, out_ptr, out_cap, channel)
        },
    )?;

    // ── Media Tracks ─────────────────────────────────────────────

    linker.func_wrap(
        "oxide",
        "api_rtc_add_track",
        |caller: Caller<'_, HostState>, peer_id: u32, kind: u32| -> u32 {
            let result = with(&caller.data().rtc, |r| r.add_track(peer_id, kind));
            handle(caller.data(), "Add track", result)
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_poll_track",
        |mut caller: Caller<'_, HostState>, peer_id: u32, out_ptr: u32, out_cap: u32| -> i32 {
            let track = with(&caller.data().rtc, |r| {
                let t = r.poll_track(peer_id)?;
                Some(format!("{}:{}:{}", t.kind, t.id, t.stream_id).into_bytes())
            });
            write_polled(&mut caller, out_ptr, out_cap, track)
        },
    )?;

    // ── Signaling ────────────────────────────────────────────────

    linker.func_wrap(
        "oxide",
        "api_rtc_signal_connect",
        |caller: Caller<'_, HostState>, url_ptr: u32, url_len: u32| -> u32 {
            let url = guest_str(&caller, url_ptr, url_len).unwrap_or_default();
            let state = caller.data();
            if with_started(&state.rtc, RtcState::new, |r| r.signal_connect(&url)).is_none() {
                state.log(ConsoleLevel::Error, "[RTC] Init failed");
                return 0;
            }
            let message = format!("[RTC] Signaling connected to {url}");
            state.log(ConsoleLevel::Log, message);
            1
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_signal_join_room",
        |caller: Caller<'_, HostState>, room_ptr: u32, room_len: u32| -> i32 {
            let room = guest_str(&caller, room_ptr, room_len).unwrap_or_default();
            match with(&caller.data().rtc, |r| r.signal_join_room(&room)) {
                Some(true) => 0,
                Some(false) => -2,
                None => -1,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_signal_send",
        |caller: Caller<'_, HostState>, data_ptr: u32, data_len: u32| -> i32 {
            let data = guest_bytes(&caller, data_ptr, data_len).unwrap_or_default();
            match with(&caller.data().rtc, |r| r.signal_send(&data)) {
                Some(true) => 0,
                Some(false) => -2,
                None => -1,
            }
        },
    )?;

    linker.func_wrap(
        "oxide",
        "api_rtc_signal_recv",
        |mut caller: Caller<'_, HostState>, out_ptr: u32, out_cap: u32| -> i32 {
            let data = with(&caller.data().rtc, |r| r.signal_recv());
            write_polled(&mut caller, out_ptr, out_cap, data)
        },
    )?;

    Ok(())
}
