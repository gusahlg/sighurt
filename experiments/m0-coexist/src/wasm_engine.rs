//! The Wasm engine itself (`sighurt-engine-wasm`), mounted the way `sig` mounts it: its runtime
//! opens a guest whose `start_app` connects a WebSocket over TLS. That brings in OpenSSL, the
//! one native library in the process that only the Wasm engine uses (tokio-tungstenite's
//! `native-tls`; Servo and reqwest use rustls).

use std::io::Read;
use std::net::TcpListener;
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCommand, EngineRuntime, EngineWaker, ViewOptions};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, NavigationId, ViewId};
use sighurt_engine_wasm::{CpuTarget, Frame, SandboxPolicy, WasmConfig, WasmRuntime};
use url::Url;

/// The Wasm engine's runtime, serving views that present nowhere.
pub type Runtime = WasmRuntime<NoTarget>;

/// A target that is never presented to: the experiment shows nothing, so it never asks.
pub struct NoTarget(Metrics);

impl CpuTarget for NoTarget {
    type Permit = LayoutEpoch;

    fn metrics(&self) -> Metrics {
        self.0
    }

    fn permit_epoch(permit: &LayoutEpoch) -> LayoutEpoch {
        *permit
    }

    fn submit(
        &mut self,
        _: LayoutEpoch,
        _: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
    ) -> Result<(), String> {
        Err("m0-coexist presents nothing".into())
    }
}

/// Starts the runtime: its Wasmtime engine, epoch ticker and worker pools. Its key-value store
/// lives in `dir`, and it caches no compiled modules.
pub fn start(dir: &Path) -> Result<Runtime, String> {
    let config = WasmConfig {
        policy: SandboxPolicy::default(),
        kv_root: dir.join("kv"),
        aot_cache: None,
    };
    // The loop below polls, so the waker has nothing to wake.
    WasmRuntime::new(config, EngineWaker::new(|| {})).map_err(|e| format!("{e:?}"))
}

/// Opens a guest (written to `dir`) that connects to `wss://` on a loopback server, and waits
/// until the server receives the first bytes of a TLS handshake. Returns what it received.
pub fn open_tls_guest(
    runtime: &mut Runtime,
    dir: &Path,
    timeout: Duration,
) -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let (sent, received) = mpsc::channel();
    // Left behind if nothing connects; it holds only the listener.
    thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.set_read_timeout(Some(timeout));
            let mut record = [0; 3];
            let _ = sent.send(stream.read_exact(&mut record).map(|()| record));
        }
    });

    let url = format!("wss://{address}/");
    let wat = format!(
        r#"(module
  (import "oxide" "api_ws_connect" (func $connect (param i32 i32) (result i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "{url}")
  (func (export "start_app") (drop (call $connect (i32.const 0) (i32.const {})))))"#,
        url.len()
    );
    let path = dir.join("tls-guest.wasm");
    let bytes = wat::parse_str(&wat).map_err(|e| e.to_string())?;
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let page = Url::from_file_path(&path).map_err(|()| "not an absolute path".to_string())?;

    let view = ViewId::next();
    let rect = WindowRect {
        x: 0,
        y: 0,
        width: 320,
        height: 240,
    };
    let scale = Scale::from_fractional(120).expect("scale 1");
    let target = NoTarget(Metrics::new(LayoutEpoch::next(), rect, scale));
    runtime
        .open_view(view, ViewOptions::default(), target)
        .map_err(|e| format!("{e:?}"))?;
    let navigate = EngineCommand::Navigate {
        navigation: NavigationId::next(),
        url: page.to_string(),
    };
    runtime
        .command(view, navigate)
        .map_err(|e| format!("{e:?}"))?;

    let deadline = Instant::now() + timeout;
    let mut events = Vec::new();
    let result = loop {
        runtime.pump(&mut events);
        let failed = events.iter().find(|event| {
            matches!(
                event,
                EngineEvent::NavigationFailed { .. } | EngineEvent::ViewFailed { .. }
            )
        });
        if let Some(event) = failed {
            break Err(format!("{event:?}"));
        }
        match received.recv_timeout(Duration::from_millis(10)) {
            // A TLS record header: content type 22 (handshake), then the protocol version.
            Ok(Ok([0x16, 3, minor])) => {
                break Ok(format!(
                    "the guest's wss:// connection sent a TLS handshake record (version 3.{minor})"
                ))
            }
            Ok(Ok(other)) => break Err(format!("not a TLS handshake: {other:02x?}")),
            Ok(Err(e)) => break Err(format!("the server read nothing: {e}")),
            Err(RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            Err(_) => break Err(format!("no connection within {timeout:?}")),
        }
    };
    runtime.close_view(view);
    result
}
