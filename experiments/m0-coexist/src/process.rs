//! Process-wide state: the panic hook, the TLS provider, signal handlers, the libraries the
//! Wasm engine brings along, and resource and library figures for the record.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::panic;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

use cpal::traits::HostTrait;
use rodio::Source;

/// A panic hook of the kind `sig` needs (M0.md, "Panic hook"): it reports and never exits.
/// Counts panics on Servo's style threads in `style_panics`: Servo doesn't notice those itself,
/// and in one process they can't be pinned on a page, since every view shares the pool.
pub fn install_panic_hook(style_panics: Arc<AtomicUsize>) {
    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        default_hook(info);
        // Servo catches these itself (SVG rasterization).
        if servo::should_panic_hook_suppress_termination() {
            return;
        }
        if thread::current()
            .name()
            .is_some_and(|name| name.starts_with("StyleThread"))
        {
            style_panics.fetch_add(1, Ordering::SeqCst);
        }
        // On a script thread, Servo's logger turns this record into a crash page and
        // `notify_crashed` for every view with a document on that thread (step 9); elsewhere
        // its constellation ignores it.
        log::error!("{info}");
    }));
}

/// Shows that rustls can't choose a provider in this build graph, then installs aws-lc-rs as
/// `sig` must. Returns what happened, or why it didn't.
pub fn install_tls_provider() -> Result<String, String> {
    use rustls::crypto::CryptoProvider;
    if CryptoProvider::get_default().is_some() {
        return Err("a provider was installed before sig chose one".into());
    }
    println!("INFO tls: the next panic, from rustls, is expected");
    if panic::catch_unwind(|| drop(rustls::ClientConfig::builder())).is_ok() {
        return Err("rustls picked a provider from its crate features by itself".into());
    }
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| "another provider was installed first".to_string())?;
    Ok(
        "without one, ClientConfig::builder() panics (ring and aws-lc-rs are both enabled); \
        aws-lc-rs installed"
            .into(),
    )
}

/// The current SIGSEGV and SIGILL handlers, to show which runtime installed its own last.
pub fn trap_handlers() -> [usize; 2] {
    [libc::SIGSEGV, libc::SIGILL].map(handler)
}

/// The current handler of `signal`: an address, or `SIG_DFL` (0) or `SIG_IGN` (1).
pub fn handler(signal: libc::c_int) -> usize {
    // SAFETY: with a null new action, `sigaction` only reads the current one into `current`,
    // a valid, writable `sigaction`. No handler changes.
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        libc::sigaction(signal, std::ptr::null(), &mut current);
        current.sa_sigaction
    }
}

/// The file of the first mapping in this process whose path contains `name`.
pub fn mapped_library(name: &str) -> Option<String> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    maps.lines()
        .filter_map(|line| line.split_whitespace().nth(5))
        .find(|path| path.contains(name))
        .map(str::to_string)
}

/// Initializes the libraries the Wasm engine links that could clash with Servo's: FFmpeg,
/// the audio stack, Tokio and reqwest with rustls. Returns what was found.
pub fn wasm_engine_libraries() -> Result<String, String> {
    // As `gpui-baseline:sighurt-engine-wasm/src/video.rs` does it.
    ffmpeg_next::init().map_err(|e| format!("ffmpeg: {e}"))?;
    ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
    let avutil = ffmpeg_next::util::version();
    let h264 = ffmpeg_next::codec::decoder::find(ffmpeg_next::codec::Id::H264).is_some();
    let sine = rodio::source::SineWave::new(440.0).sample_rate();
    let host = cpal::default_host();
    let devices = match host.output_devices() {
        Ok(devices) => format!("{} output devices", devices.count()),
        Err(e) => format!("no output devices ({e})"),
    };
    let body = fetch_from_local_server()?;
    Ok(format!(
        "libavutil {}.{}.{}, H.264 decoder: {h264}; rodio sine at {sine} Hz; cpal {}: {devices}; \
         reqwest over Tokio fetched {body:?}",
        avutil >> 16,
        (avutil >> 8) & 0xff,
        avutil & 0xff,
        host.id().name(),
    ))
}

/// Fetches a page from a one-shot HTTP server on the loopback with reqwest, on a new
/// Tokio runtime, next to Servo's own. Building the client builds its rustls configuration.
fn fetch_from_local_server() -> Result<String, String> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let server = thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
        }
    });
    let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
    let body = runtime.block_on(async {
        reqwest::get(format!("http://{address}/"))
            .await?
            .text()
            .await
    });
    let _ = server.join();
    body.map_err(|e| format!("reqwest: {e}"))
}

/// Resident and virtual memory and the thread count, from /proc.
pub fn resources() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map_or("?".into(), |value| value.trim().to_string())
    };
    format!(
        "RSS {}, virtual {}, {} threads",
        field("VmRSS:"),
        field("VmSize:"),
        field("Threads:")
    )
}

/// The process's threads by name, with their numbers left out ("StyleThread × 2").
pub fn thread_names() -> String {
    let mut names = std::collections::BTreeMap::<String, usize>::new();
    for task in std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        let name = name
            .trim()
            .trim_end_matches(|c: char| c.is_ascii_digit() || c == '#');
        *names.entry(name.to_string()).or_default() += 1;
    }
    let names: Vec<_> = names
        .iter()
        .map(|(name, count)| format!("{name} × {count}"))
        .collect();
    names.join(", ")
}
