//! The `tui-servo` process: one Servo 0.5 runtime, one offscreen WebView, N timed frames.
//!
//! Owns the command line, the Servo event loop, frame timing, the PNG and the kitty payload.
//! Does not own the GL context (that is [`context`]) and does not open a display connection.

mod context;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine;
use dpi::PhysicalSize;
use servo::{
    ConsoleLogLevel, EventLoopWaker, JSValue, JavaScriptEvaluationError, LoadStatus, Opts,
    Preferences, Servo, ServoBuilder, ServoDelegate, ServoError, WebView, WebViewBuilder,
    WebViewDelegate,
};
use url::Url;

use context::{AdapterChoice, ContextReport, OffscreenContext};

const USAGE: &str = "\
Usage: tui-servo <URL|path> --out DIR [--size WxH] [--frames N] [--adapter hw|sw] [--kitty FILE]

Renders the page with Servo 0.5 into an offscreen GL framebuffer (no Wayland or X11
display), paints N frames, and writes the last one as DIR/frame.png.
--adapter hw asks surfman for the hardware adapter; sw asks for the software one.
--kitty writes that frame as a kitty graphics escape sequence (RGBA, zlib, base64).";

const PAGE_SCRIPT: &str = "JSON.stringify({title: document.title, \
     headings: document.querySelectorAll('h1,h2,h3,h4,h5,h6').length, \
     links: document.querySelectorAll('a[href]').length, \
     inputs: document.querySelectorAll('input,textarea,select,button').length})";

const LOAD_TIMEOUT: Duration = Duration::from_secs(120);
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(60);
const JS_TIMEOUT: Duration = Duration::from_secs(20);
const BETWEEN_FRAMES: Duration = Duration::from_millis(250);
/// A frame whose most common color covers this fraction of the pixels is blank.
const BLANK_FRACTION: f64 = 0.99;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("tui-servo: {err}");
            ExitCode::from(1)
        }
    }
}

struct Args {
    url: Url,
    size: PhysicalSize<u32>,
    frames: u32,
    adapter: AdapterChoice,
    out: PathBuf,
    kitty: Option<PathBuf>,
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out)
        .map_err(|err| format!("create {}: {err}", args.out.display()))?;
    let profile = args.out.join("profile");
    std::fs::create_dir_all(&profile)
        .map_err(|err| format!("create {}: {err}", profile.display()))?;

    let wayland = env_value("WAYLAND_DISPLAY");
    let display = env_value("DISPLAY");
    eprintln!(
        "tui-servo: url {} size {}x{} frames {} adapter {:?} out {}",
        args.url,
        args.size.width,
        args.size.height,
        args.frames,
        args.adapter,
        args.out.display()
    );
    eprintln!("tui-servo: WAYLAND_DISPLAY={wayland} DISPLAY={display}");

    // Declared first so it drops last: Servo's shutdown still makes the context current.
    let context = Rc::new(OffscreenContext::new(args.size, args.adapter)?);
    let report = context.report().clone();
    eprintln!(
        "tui-servo: backend {} adapter {} egl {:?} gl {} / {} / {}",
        report.backend,
        report.adapter,
        report.egl_vendor,
        report.gl_vendor,
        report.gl_renderer,
        report.gl_version
    );
    eprintln!(
        "tui-servo: LIBGL_ALWAYS_SOFTWARE {} -> {}  DRI_PRIME {} -> {}",
        report.libgl_before, report.libgl_after, report.dri_prime_before, report.dri_prime_after
    );

    // Servo's network stack refuses to start without a process-wide TLS provider.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let woken = Arc::new(AtomicBool::new(false));
    let preferences = Preferences {
        // Servo 0.5's parallel styling raced with 3 threads (see experiments/m1-mixed). Kept.
        layout_threads: 2,
        ..Preferences::default()
    };
    let servo = ServoBuilder::default()
        .opts(Opts {
            hard_fail: false,
            config_dir: Some(profile),
            ..Opts::default()
        })
        .preferences(preferences)
        .event_loop_waker(Box::new(Waker(woken)))
        .build();
    servo.setup_logging();
    let delegate = Rc::new(Delegate::default());
    servo.set_delegate(delegate.clone());
    let webview = WebViewBuilder::new(&servo, context.clone())
        .url(args.url.clone())
        .delegate(delegate.clone())
        .build();
    webview.set_focused(true);
    webview.show();

    let loaded = Instant::now();
    if !pump_for(&servo, &delegate, LOAD_TIMEOUT, || {
        webview.load_status() == LoadStatus::Complete
    })? {
        return Err(format!(
            "load did not complete within {LOAD_TIMEOUT:?} (status {:?})",
            webview.load_status()
        ));
    }
    eprintln!("tui-servo: load complete in {:.1?}", loaded.elapsed());

    if !pump_for(&servo, &delegate, FIRST_FRAME_TIMEOUT, || {
        delegate.frame_ready.get()
    })? {
        eprintln!(
            "tui-servo: no notify_new_frame_ready within {FIRST_FRAME_TIMEOUT:?}; painting anyway"
        );
    }

    let mut pixels = vec![0u8; frame_bytes(args.size.width, args.size.height)?];
    let mut paint_ms = Vec::with_capacity(args.frames as usize);
    let mut read_ms = Vec::with_capacity(args.frames as usize);
    for index in 0..args.frames {
        delegate.frame_ready.set(false);
        let _new_frame = pump_for(&servo, &delegate, BETWEEN_FRAMES, || {
            delegate.frame_ready.get()
        })?;
        let paint_at = Instant::now();
        webview.paint();
        let paint = duration_ms(paint_at.elapsed());
        let read_at = Instant::now();
        context.read_rgba(&mut pixels)?;
        let read = duration_ms(read_at.elapsed());
        eprintln!("tui-servo: frame {index} paint_ms={paint:.3} read_ms={read:.3}");
        paint_ms.push(paint);
        read_ms.push(read);
    }

    let png = args.out.join("frame.png");
    image::save_buffer(
        &png,
        &pixels,
        args.size.width,
        args.size.height,
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|err| format!("write {}: {err}", png.display()))?;
    eprintln!("tui-servo: wrote {}", png.display());
    if let Some(kitty) = &args.kitty {
        write_kitty(kitty, args.size.width, args.size.height, &pixels)?;
        eprintln!("tui-servo: wrote kitty {}", kitty.display());
    }

    let js = evaluate_page(&servo, &webview, &delegate);
    let js_text = match &js {
        Ok(value) => value.clone(),
        Err(err) => format!("error: {err}"),
    };
    let (mode_color, mode_fraction) = mode_of(&pixels);
    let blank = mode_fraction >= BLANK_FRACTION;
    let text = result_text(&ResultView {
        args: &args,
        report: &report,
        wayland: &wayland,
        display: &display,
        paint_ms: &paint_ms,
        read_ms: &read_ms,
        png: &png,
        js: &js_text,
        mode_color: &mode_color,
        mode_fraction,
        blank,
        samples: &samples(&pixels, args.size.width, args.size.height),
        animating: delegate.animating.get(),
    });
    let summary = args.out.join("summary.txt");
    std::fs::write(&summary, &text).map_err(|err| format!("write {}: {err}", summary.display()))?;
    print!("{text}");

    let mut failures = Vec::new();
    if let Err(err) = js {
        failures.push(err);
    }
    if blank {
        failures.push(format!(
            "frame is blank ({:.1}% of pixels are {mode_color}); wrote {}",
            mode_fraction * 100.0,
            png.display()
        ));
    }
    if !failures.is_empty() {
        return Err(failures.join("; "));
    }
    Ok(())
}

struct ResultView<'a> {
    args: &'a Args,
    report: &'a ContextReport,
    wayland: &'a str,
    display: &'a str,
    paint_ms: &'a [f64],
    read_ms: &'a [f64],
    png: &'a std::path::Path,
    js: &'a str,
    mode_color: &'a str,
    mode_fraction: f64,
    blank: bool,
    samples: &'a str,
    animating: bool,
}

fn result_text(view: &ResultView<'_>) -> String {
    let frame_ms: Vec<f64> = view
        .paint_ms
        .iter()
        .zip(view.read_ms)
        .map(|(paint, read)| paint + read)
        .collect();
    let sum_ms: f64 = frame_ms.iter().sum();
    let kitty = match &view.args.kitty {
        Some(path) => format!("kitty={} kitty_compression=zlib", path.display()),
        None => "kitty=none".to_string(),
    };
    let mut text = String::new();
    let _ = writeln!(text, "----- tui-servo result -----");
    let _ = writeln!(text, "url={}", view.args.url);
    let _ = writeln!(
        text,
        "size={}x{}",
        view.args.size.width, view.args.size.height
    );
    let _ = writeln!(text, "frames={}", view.args.frames);
    let _ = writeln!(text, "backend={}", view.report.backend);
    let _ = writeln!(text, "adapter={}", view.report.adapter);
    let _ = writeln!(text, "adapter_choice={:?}", view.report.choice);
    let _ = writeln!(text, "egl_vendor={}", view.report.egl_vendor);
    let _ = writeln!(text, "egl_version={}", view.report.egl_version);
    let _ = writeln!(text, "gl_vendor={}", view.report.gl_vendor);
    let _ = writeln!(text, "gl_renderer={}", view.report.gl_renderer);
    let _ = writeln!(text, "gl_version={}", view.report.gl_version);
    let _ = writeln!(
        text,
        "surfman_connection_type={}",
        view.report.surfman_connection_type
    );
    let _ = writeln!(text, "WAYLAND_DISPLAY={}", view.wayland);
    let _ = writeln!(text, "DISPLAY={}", view.display);
    let _ = writeln!(
        text,
        "LIBGL_ALWAYS_SOFTWARE_before={}",
        view.report.libgl_before
    );
    let _ = writeln!(
        text,
        "LIBGL_ALWAYS_SOFTWARE_after={}",
        view.report.libgl_after
    );
    let _ = writeln!(text, "DRI_PRIME_before={}", view.report.dri_prime_before);
    let _ = writeln!(text, "DRI_PRIME_after={}", view.report.dri_prime_after);
    let _ = writeln!(text, "animating={}", view.animating);
    let _ = writeln!(
        text,
        "timing=paint_is_webview_paint readback_includes_gpu_wait=true"
    );
    let _ = writeln!(text, "paint_first_ms={:.3}", view.paint_ms[0]);
    let _ = writeln!(text, "readback_first_ms={:.3}", view.read_ms[0]);
    let _ = writeln!(text, "paint_median_ms={:.3}", quantile(view.paint_ms, 0.5));
    let _ = writeln!(text, "paint_p95_ms={:.3}", quantile(view.paint_ms, 0.95));
    let _ = writeln!(
        text,
        "readback_median_ms={:.3}",
        quantile(view.read_ms, 0.5)
    );
    let _ = writeln!(text, "readback_p95_ms={:.3}", quantile(view.read_ms, 0.95));
    let _ = writeln!(text, "frame_median_ms={:.3}", quantile(&frame_ms, 0.5));
    let _ = writeln!(text, "frame_p95_ms={:.3}", quantile(&frame_ms, 0.95));
    let _ = writeln!(text, "sum_ms={sum_ms:.3}");
    let _ = writeln!(
        text,
        "mode_color={} mode_fraction={:.4} blank={}",
        view.mode_color, view.mode_fraction, view.blank
    );
    let _ = writeln!(text, "samples={}", view.samples);
    let _ = writeln!(text, "js={}", view.js);
    let _ = writeln!(text, "png={}", view.png.display());
    let _ = writeln!(text, "{kitty}");
    for (index, (paint, read)) in view.paint_ms.iter().zip(view.read_ms).enumerate() {
        let _ = writeln!(text, "frame {index} paint_ms={paint:.3} read_ms={read:.3}");
    }
    text
}

/// Spin Servo until `done`, the budget runs out, or the page crashes.
///
/// There is no display connection and no OS event loop. Servo wakes [`Waker`], and this
/// sleeps about a millisecond between spins, the same wait Servo's own tests use.
/// `Ok(false)` is the budget running out with the page still alive.
fn pump_for(
    servo: &Servo,
    delegate: &Delegate,
    budget: Duration,
    mut done: impl FnMut() -> bool,
) -> Result<bool, String> {
    let start = Instant::now();
    loop {
        if let Some(err) = delegate.failure() {
            return Err(err);
        }
        if done() {
            return Ok(true);
        }
        if start.elapsed() >= budget {
            return Ok(false);
        }
        servo.spin_event_loop();
        thread::sleep(Duration::from_millis(1));
    }
}

fn evaluate_page(servo: &Servo, webview: &WebView, delegate: &Delegate) -> Result<String, String> {
    let slot: Rc<RefCell<Option<Result<JSValue, JavaScriptEvaluationError>>>> =
        Rc::new(RefCell::new(None));
    let callback_slot = slot.clone();
    webview.evaluate_javascript(PAGE_SCRIPT, move |result| {
        *callback_slot.borrow_mut() = Some(result);
    });
    // `borrow` ends when this closure returns, before the next `spin_event_loop`.
    if !pump_for(servo, delegate, JS_TIMEOUT, || slot.borrow().is_some())? {
        return Err("javascript timed out".into());
    }
    let value = slot
        .borrow()
        .clone()
        .expect("pump waited until the callback ran");
    match value {
        Ok(JSValue::String(value)) => Ok(value),
        Ok(other) => Err(format!("javascript returned {other:?}, expected a string")),
        Err(err) => Err(format!("javascript failed: {err:?}")),
    }
}

/// Direct kitty graphics transmission of one RGBA frame, zlib-compressed, base64 in 4096-byte chunks.
fn write_kitty(path: &std::path::Path, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(rgba)
        .map_err(|err| format!("zlib: {err}"))?;
    let compressed = encoder.finish().map_err(|err| format!("zlib: {err}"))?;
    let payload = base64::engine::general_purpose::STANDARD.encode(compressed);
    if payload.is_empty() {
        return Err("kitty payload is empty".into());
    }
    let mut file = String::new();
    let mut start = 0;
    while start < payload.len() {
        let end = (start + 4096).min(payload.len());
        let more = if end == payload.len() { 0 } else { 1 };
        let chunk = &payload[start..end];
        if start == 0 {
            write!(
                file,
                "\x1b_Ga=T,f=32,s={width},v={height},o=z,m={more};{chunk}\x1b\\"
            )
            .expect("writing a String");
        } else {
            write!(file, "\x1b_Gm={more};{chunk}\x1b\\").expect("writing a String");
        }
        start = end;
    }
    file.push('\n');
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create {}: {err}", parent.display()))?;
        }
    }
    std::fs::write(path, file).map_err(|err| format!("write {}: {err}", path.display()))
}

fn samples(buf: &[u8], width: u32, height: u32) -> String {
    let at = |x: u32, y: u32| {
        let x = x.min(width - 1);
        let y = y.min(height - 1);
        let i = ((y as usize * width as usize) + x as usize) * 4;
        format!("({x},{y})={}", hex(&buf[i..i + 4]))
    };
    format!(
        "{} {} {} {}",
        at(0, 0),
        at(width / 2, 40),
        at(width / 2, height / 2),
        at(width.saturating_sub(8), height.saturating_sub(8))
    )
}

fn mode_of(buf: &[u8]) -> (String, f64) {
    let mut counts: HashMap<[u8; 3], u64> = HashMap::new();
    let mut pixels = 0u64;
    for px in buf.as_chunks::<4>().0 {
        let [r, g, b, _] = *px;
        *counts.entry([r, g, b]).or_insert(0) += 1;
        pixels += 1;
    }
    let Some((color, count)) = counts.into_iter().max_by_key(|(_, count)| *count) else {
        return ("#000000".to_string(), 1.0);
    };
    let fraction = if pixels == 0 {
        1.0
    } else {
        count as f64 / pixels as f64
    };
    (
        format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2]),
        fraction,
    )
}

fn hex(px: &[u8]) -> String {
    format!("#{:02x}{:02x}{:02x}{:02x}", px[0], px[1], px[2], px[3])
}

/// Linear quantile at `q` of `q * (n - 1)`, after sorting. `n == 1` returns that sample.
fn quantile(values: &[f64], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() {
        return f64::NAN;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let pos = q * (sorted.len() - 1) as f64;
    let lo = pos.floor() as usize;
    let hi = pos.ceil() as usize;
    let frac = pos - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn frame_bytes(width: u32, height: u32) -> Result<usize, String> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("frame {width}x{height} is too large"))
}

fn env_value(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "unset".to_string())
}

fn parse_args() -> Result<Args, String> {
    let mut args = std::env::args().skip(1);
    let mut url = None;
    let mut size = PhysicalSize::new(1280, 720);
    let mut frames = 30u32;
    let mut adapter = AdapterChoice::Hardware;
    let mut out = None;
    let mut kitty = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Err(USAGE.into()),
            "--size" => size = parse_size(&arg, args.next())?,
            "--frames" => frames = parse_frames(&arg, args.next())?,
            "--adapter" => adapter = parse_adapter(&arg, args.next())?,
            "--out" => out = Some(PathBuf::from(required(&arg, args.next())?)),
            "--kitty" => kitty = Some(PathBuf::from(required(&arg, args.next())?)),
            _ if arg.starts_with('-') => return Err(format!("unknown option {arg}\n\n{USAGE}")),
            _ if url.is_some() => {
                return Err(format!("unexpected extra argument {arg}\n\n{USAGE}"))
            }
            _ => url = Some(parse_url(&arg)?),
        }
    }
    Ok(Args {
        url: url.ok_or_else(|| format!("missing URL or path\n\n{USAGE}"))?,
        size,
        frames,
        adapter,
        out: out.ok_or_else(|| format!("missing --out DIR\n\n{USAGE}"))?,
        kitty,
    })
}

fn parse_url(page: &str) -> Result<Url, String> {
    if page.contains("://") {
        Url::parse(page).map_err(|err| format!("{page}: {err}"))
    } else {
        let path = std::fs::canonicalize(page).map_err(|err| format!("{page}: {err}"))?;
        Url::from_file_path(&path).map_err(|()| format!("{}: not a file", path.display()))
    }
}

fn required(flag: &str, value: Option<String>) -> Result<String, String> {
    value.ok_or_else(|| format!("{flag} needs a value\n\n{USAGE}"))
}

fn parse_size(flag: &str, value: Option<String>) -> Result<PhysicalSize<u32>, String> {
    let value = required(flag, value)?;
    let Some((width, height)) = value.split_once('x') else {
        return Err(format!("{flag} wants WxH, got {value}"));
    };
    let width: u32 = width
        .parse()
        .map_err(|_| format!("{flag}: bad width {width}"))?;
    let height: u32 = height
        .parse()
        .map_err(|_| format!("{flag}: bad height {height}"))?;
    if width == 0 || height == 0 {
        return Err(format!("{flag}: size must be at least 1x1, got {value}"));
    }
    Ok(PhysicalSize::new(width, height))
}

fn parse_frames(flag: &str, value: Option<String>) -> Result<u32, String> {
    let value = required(flag, value)?;
    let frames: u32 = value
        .parse()
        .map_err(|_| format!("{flag}: bad count {value}"))?;
    if frames == 0 {
        return Err(format!("{flag}: frame count must be at least 1"));
    }
    Ok(frames)
}

fn parse_adapter(flag: &str, value: Option<String>) -> Result<AdapterChoice, String> {
    match required(flag, value)?.as_str() {
        "hw" => Ok(AdapterChoice::Hardware),
        "sw" => Ok(AdapterChoice::Software),
        other => Err(format!("{flag}: expected hw or sw, got {other}")),
    }
}

/// Servo asks the embedder to spin. This process has no OS loop, so the flag is only a record
/// that a wake arrived; [`pump_for`] spins on a short sleep either way.
#[derive(Clone)]
struct Waker(Arc<AtomicBool>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct Delegate {
    frame_ready: Cell<bool>,
    animating: Cell<bool>,
    crashed: RefCell<Option<String>>,
    servo_error: RefCell<Option<String>>,
}

impl Delegate {
    fn failure(&self) -> Option<String> {
        if let Some(reason) = self.crashed.borrow().clone() {
            return Some(format!("page crashed: {reason}"));
        }
        self.servo_error
            .borrow()
            .clone()
            .map(|err| format!("servo error: {err}"))
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.frame_ready.set(true);
    }

    fn notify_page_title_changed(&self, _: WebView, title: Option<String>) {
        eprintln!("tui-servo: title {title:?}");
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        eprintln!("tui-servo: load {status:?}");
    }

    fn notify_animating_changed(&self, _: WebView, animating: bool) {
        self.animating.set(animating);
        eprintln!("tui-servo: animating {animating}");
    }

    fn notify_crashed(&self, _: WebView, reason: String, _: Option<String>) {
        eprintln!("tui-servo: crashed: {reason}");
        *self.crashed.borrow_mut() = Some(reason);
    }

    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        eprintln!("tui-servo: console {level:?}: {message}");
    }
}

impl ServoDelegate for Delegate {
    fn notify_error(&self, error: ServoError) {
        eprintln!("tui-servo: servo error {error:?}");
        *self.servo_error.borrow_mut() = Some(format!("{error:?}"));
    }
}
