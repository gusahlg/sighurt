//! `sig-servo` — a Sighurt content engine backed by [Servo](https://servo.org).
//!
//! Sighurt spawns this process once per page and speaks the [`sighurt_ipc`] protocol over
//! stdin/stdout. Servo renders offscreen into a GL context (on the GPU when it can, see
//! [`rendering`]); every new frame is read back as RGBA and sent to the shell, which only has
//! to display it.
//!
//! Everything Servo-specific lives in this crate. The browser shell never links Servo.

mod delegate;
mod rendering;

use std::fs::{File, TryLockError};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use delegate::{Engine, Output};
use dpi::PhysicalSize;
use euclid::Scale;
use servo::{
    Code, DevicePoint, EventLoopWaker, InputEvent, Key, KeyState, KeyboardEvent, Location,
    Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent, MouseMoveEvent, Opts, PrefValue,
    Preferences, Servo, ServoBuilder, StorageType, WebView, WebViewBuilder, WebViewPoint,
    WheelDelta, WheelEvent, WheelMode,
};
use sighurt_ipc::{FromEngine, ToEngine, MOD_ALT, MOD_CTRL, MOD_META, MOD_SHIFT};
use url::Url;

enum Msg {
    Host(ToEngine),
    /// Servo asked for its event loop to be spun.
    Wake,
    /// The shell closed our stdin.
    Closed,
}

fn main() {
    let out: Output = Arc::new(Mutex::new(protocol_output()));
    report_panics(out.clone());
    let (tx, rx) = mpsc::channel();

    let stdin_tx = tx.clone();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        while let Ok(Some(msg)) = ToEngine::read_from(&mut stdin) {
            if stdin_tx.send(Msg::Host(msg)).is_err() {
                return;
            }
        }
        let _ = stdin_tx.send(Msg::Closed);
    });

    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let timing = std::env::var_os("SIG_SERVO_TIMING").is_some_and(|v| v != "0");

    let hello = FromEngine::Hello {
        version: sighurt_ipc::VERSION,
        name: "servo".into(),
    };
    let _ = hello.write_to(&mut *out.lock().unwrap());

    // The shell's first `Resize` gives the real size, before the page is created.
    let started = Instant::now();
    let context = match rendering::create(PhysicalSize::new(800, 600)) {
        Ok((context, kind)) => {
            if timing {
                let renderer = context.gleam_gl_api().get_string(gleam::gl::RENDERER);
                eprintln!(
                    "sig-servo: {kind} rendering ({renderer}), context created in {:.1} ms",
                    ms(started)
                );
            }
            context
        }
        Err(message) => {
            let _ = FromEngine::Error { message }.write_to(&mut *out.lock().unwrap());
            std::process::exit(1);
        }
    };
    let engine = Engine::new(out, context);

    let profile = profile_dir();
    let lock = profile.as_deref().and_then(lock_profile);
    let wake_pending = Arc::new(AtomicBool::new(false));
    let servo = ServoBuilder::default()
        .opts(Opts {
            // Report page crashes through `notify_crashed` instead of exiting the process.
            hard_fail: false,
            config_dir: profile.clone(),
            ..Opts::default()
        })
        .preferences(preferences())
        .event_loop_waker(Box::new(Waker {
            tx,
            pending: wake_pending.clone(),
        }))
        .build();
    if lock.is_some() {
        // Servo reads the profile on its own threads. Asking for the site data it holds waits
        // until they have.
        servo
            .site_data_manager()
            .site_data(StorageType::Cookies | StorageType::Local);
    }
    drop(lock);
    servo.setup_logging();
    servo.set_delegate(engine.clone());

    let mut page = Page {
        webview: None,
        servo,
        engine,
        scale: 1.0,
        focused: false,
        zoom: 1.0,
        pixels: Vec::new(),
        last_frame: FromEngine::Frame {
            width: 0,
            height: 0,
            rgba: Vec::new(),
        },
        timing,
        navigated: None,
    };

    'run: while let Ok(first) = rx.recv() {
        // Handle everything that is already queued before spinning Servo once.
        for msg in std::iter::once(first).chain(rx.try_iter()) {
            match msg {
                Msg::Wake => wake_pending.store(false, Ordering::SeqCst),
                Msg::Closed => break 'run,
                Msg::Host(msg) => page.handle(msg),
            }
        }
        page.servo.spin_event_loop();
        page.render();
        if page.engine.closed.get() {
            break;
        }
    }

    // Servo saves cookies and the rest of the profile while it shuts down, when `page` drops.
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(5));
        eprintln!("sig-servo: Servo did not shut down in time");
        // Not `process::exit`: running exit handlers while Servo's threads are busy crashes.
        // SAFETY: `_exit` ends the process without touching any Rust or C state.
        unsafe { libc::_exit(1) }
    });
    let _lock = profile.as_deref().and_then(lock_profile);
    drop(page);
}

/// Where Servo keeps cookies, local storage and HSTS state between runs:
/// `{data_dir}/sighurt/servo`. `None` (nothing is kept) if it can't be created.
fn profile_dir() -> Option<PathBuf> {
    let dir = dirs::data_dir()?.join("sighurt").join("servo");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Some(dir),
        Err(e) => {
            eprintln!(
                "sig-servo: cannot create {}: {e}; nothing will be saved",
                dir.display()
            );
            None
        }
    }
}

/// Locks the profile shared by every page's `sig-servo`. Servo reads its files when it starts
/// and rewrites them when it shuts down, and the lock keeps other processes from doing either at
/// the same time. Gives up after a few seconds, so a stuck process can't block the others.
fn lock_profile(dir: &Path) -> Option<File> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("lock"))
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match file.try_lock() {
            Ok(()) => return Some(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                eprintln!("sig-servo: cannot lock the profile: {e}");
                return None;
            }
        }
    }
}

/// Servo's preferences: its defaults with a Firefox user agent and the changes below, then any
/// `--pref NAME=VALUE` arguments.
fn preferences() -> Preferences {
    let mut prefs = Preferences::default();
    prefs.user_agent = firefox_user_agent(&prefs.user_agent);
    // Web platform features that Servo 0.5 implements but leaves off, and that common sites use.
    // Without grid, sites like mozilla.org and theguardian.com lose their layout.
    prefs.dom_exec_command_enabled = true;
    prefs.dom_fontface_enabled = true;
    prefs.dom_indexeddb_enabled = true;
    prefs.dom_offscreen_canvas_enabled = true;
    prefs.dom_permissions_enabled = true;
    prefs.dom_storage_manager_api_enabled = true;
    prefs.dom_webgl2_enabled = true;
    prefs.layout_columns_enabled = true;
    prefs.layout_container_queries_enabled = true;
    prefs.layout_grid_enabled = true;
    prefs.layout_variable_fonts_enabled = true;
    // Left off because they break common sites: with `IntersectionObserver` script spins forever
    // and the page stays blank (reddit.com, microsoft.com), a Servo 0.5 layout bug fixed upstream
    // in servo/servo#47693, and with `adoptedStyleSheets` layout panics (microsoft.com).
    //
    // Turned off: a page's shared worker never exits, so Servo can't shut down (x.com) and the
    // engine lingers until its watchdog kills it, holding the profile lock. Sites fall back as
    // they do in browsers without `SharedWorker`.
    prefs.dom_sharedworker_enabled = false;
    // With Servo's default of 3, parallel styling races and panics a style thread on
    // developer.mozilla.org in ~40% of loads (14 of 36), and the page stops rendering. With 2 it
    // didn't happen in 30 loads.
    prefs.layout_threads = 2;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let pref = match arg.strip_prefix("--pref=") {
            Some(pref) => pref.to_string(),
            None if arg == "--pref" => args.next().unwrap_or_default(),
            None => {
                eprintln!("sig-servo: ignoring unknown argument {arg:?}");
                continue;
            }
        };
        if let Err(e) = set_pref(&mut prefs, &pref) {
            eprintln!("sig-servo: ignoring --pref {pref:?}: {e}");
        }
    }
    prefs
}

/// Servo's user agent with its `Servo/<version>` token replaced by Firefox's `Gecko/20100101`.
/// Servo already claims to be Firefox 140, but some sites turn away a user agent that isn't
/// exactly Firefox's: web.whatsapp.com asks for "Firefox 115+" instead of showing its login.
fn firefox_user_agent(servo: &str) -> String {
    let tokens = servo.split(' ').map(|token| {
        if token.starts_with("Servo/") {
            "Gecko/20100101"
        } else {
            token
        }
    });
    tokens.collect::<Vec<_>>().join(" ")
}

/// Applies one `NAME=VALUE` preference, parsing the value as the preference's type.
fn set_pref(prefs: &mut Preferences, pref: &str) -> Result<(), String> {
    let (name, value) = pref.split_once('=').ok_or("expected NAME=VALUE")?;
    if !Preferences::exists(name) {
        return Err("no such preference".into());
    }
    let bad = |e: &dyn std::fmt::Display| format!("bad value: {e}");
    let value = match Preferences::type_of(name) {
        "bool" => PrefValue::Bool(value.parse().map_err(|e| bad(&e))?),
        "i64" => PrefValue::Int(value.parse().map_err(|e| bad(&e))?),
        "u64" => PrefValue::UInt(value.parse().map_err(|e| bad(&e))?),
        "f64" => PrefValue::Float(value.parse().map_err(|e| bad(&e))?),
        "alloc::string::String" => PrefValue::Str(value.into()),
        other => return Err(format!("preferences of type {other} are not supported")),
    };
    prefs.set_value(name, value);
    Ok(())
}

/// The page the shell asked for, and what is needed to apply its messages.
struct Page {
    /// Created by the first `Navigate`. Dropped before `servo`.
    webview: Option<WebView>,
    servo: Servo,
    engine: Rc<Engine>,
    /// Viewport scale, focus and zoom as last sent by the shell, also applied to a new webview.
    scale: f32,
    focused: bool,
    zoom: f32,
    /// Readback buffer, recycled between frames.
    pixels: Vec<u8>,
    /// The last frame sent, so frames that changed nothing are not sent again.
    last_frame: FromEngine,
    /// `SIG_SERVO_TIMING`: log per-frame timings to stderr.
    timing: bool,
    /// When the last navigation started, until its first frame is logged.
    navigated: Option<Instant>,
}

impl Page {
    fn handle(&mut self, msg: ToEngine) {
        let webview = match msg {
            ToEngine::Navigate { url } => return self.navigate(&url),
            ToEngine::Resize {
                width,
                height,
                scale,
            } => {
                let size = PhysicalSize::new(width.max(1), height.max(1));
                self.scale = if scale > 0.0 { scale } else { 1.0 };
                return match &self.webview {
                    // This resizes the rendering context too. Resizing the context first would
                    // make Servo skip updating the viewport.
                    Some(webview) => {
                        webview.resize(size);
                        webview.set_hidpi_scale_factor(Scale::new(self.scale));
                    }
                    // The webview will be created at the context's size.
                    None => self.engine.context.resize(size),
                };
            }
            ToEngine::Focus { focused } => {
                self.focused = focused;
                if let Some(webview) = &self.webview {
                    set_focus(webview, focused);
                }
                return;
            }
            ToEngine::Zoom { factor } => {
                self.zoom = factor;
                if let Some(webview) = &self.webview {
                    webview.set_page_zoom(factor);
                }
                return;
            }
            _ => match &self.webview {
                Some(webview) => webview,
                None => return,
            },
        };
        let point = |x: f32, y: f32| WebViewPoint::Device(DevicePoint::new(x, y));
        let input = match msg {
            ToEngine::Reload => {
                webview.reload();
                // Servo reports a reload only once the new document's head is parsed.
                return self.engine.send(&FromEngine::Loading { loading: true });
            }
            ToEngine::Stop => {
                // Servo 0.5 has no stop API. `window.stop()` aborts the document's parsing and
                // pending fetches, but can't cancel a navigation whose response hasn't arrived
                // yet. Servo reports the load complete only if the parser was still running.
                let engine = self.engine.clone();
                return webview.evaluate_javascript("window.stop()", move |result| {
                    if result.is_ok() {
                        engine.send(&FromEngine::Loading { loading: false });
                    }
                });
            }
            ToEngine::Back => {
                webview.go_back(1);
                return;
            }
            ToEngine::Forward => {
                webview.go_forward(1);
                return;
            }
            ToEngine::MouseMove { x, y } => InputEvent::MouseMove(MouseMoveEvent::new(point(x, y))),
            ToEngine::MouseButton { button, down, x, y } => {
                let action = if down {
                    MouseButtonAction::Down
                } else {
                    MouseButtonAction::Up
                };
                let button = MouseButton::from(button);
                InputEvent::MouseButton(MouseButtonEvent::new(action, button, point(x, y)))
            }
            ToEngine::Wheel { dx, dy, x, y } => {
                // The protocol uses DOM wheel semantics (positive = scroll down/right); Servo's
                // delta is the opposite.
                let delta = WheelDelta {
                    x: -dx as f64,
                    y: -dy as f64,
                    z: 0.0,
                    mode: WheelMode::DeltaPixel,
                };
                InputEvent::Wheel(WheelEvent::new(delta, point(x, y)))
            }
            ToEngine::Key {
                down,
                key,
                code,
                modifiers,
                repeat,
            } => {
                let Ok(key) = key.parse::<Key>() else {
                    return;
                };
                let state = if down { KeyState::Down } else { KeyState::Up };
                InputEvent::Keyboard(KeyboardEvent::new_without_event(
                    state,
                    key,
                    code.parse().unwrap_or(Code::Unidentified),
                    Location::Standard,
                    to_servo_modifiers(modifiers),
                    repeat,
                    false,
                ))
            }
            // Handled above, whether or not there is a webview yet.
            _ => return,
        };
        webview.notify_input_event(input);
    }

    fn navigate(&mut self, url: &str) {
        let url = match Url::parse(url) {
            Ok(url) => url,
            Err(e) => {
                let message = format!("invalid URL {url:?}: {e}");
                return self.engine.send(&FromEngine::Error { message });
            }
        };
        if self.timing {
            self.navigated = Some(Instant::now());
        }
        if let Some(webview) = &self.webview {
            return webview.load(url);
        }
        let webview = WebViewBuilder::new(&self.servo, self.engine.context.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(self.scale))
            .delegate(self.engine.clone())
            .build();
        if self.focused {
            set_focus(&webview, true);
        }
        if self.zoom != 1.0 {
            webview.set_page_zoom(self.zoom);
        }
        self.webview = Some(webview);
    }

    /// Paints and sends a frame if Servo has a new one and it differs from the last one sent.
    fn render(&mut self) {
        let Some(webview) = &self.webview else {
            return;
        };
        if !self.engine.frame_ready.take() {
            return;
        }
        let start = Instant::now();
        webview.paint();
        let painted = Instant::now();
        let context = &*self.engine.context;
        rendering::read_frame(context, &mut self.pixels);
        let read = Instant::now();

        let size = context.size();
        let mut frame = FromEngine::Frame {
            width: size.width,
            height: size.height,
            rgba: std::mem::take(&mut self.pixels),
        };
        // Servo also reports frames for changes that aren't visible, such as hover styles that
        // look the same.
        let changed = frame != self.last_frame;
        if changed {
            self.engine.send(&frame);
            std::mem::swap(&mut frame, &mut self.last_frame);
        }
        // Recycle the buffer that isn't kept for the next comparison.
        if let FromEngine::Frame { rgba, .. } = frame {
            self.pixels = rgba;
        }

        if self.timing {
            let outcome = if changed {
                format!("sent in {:.1} ms", ms(read))
            } else {
                "unchanged, not sent".into()
            };
            eprintln!(
                "sig-servo: frame {}x{}: paint {:.1} ms, read {:.1} ms, {outcome}",
                size.width,
                size.height,
                (painted - start).as_secs_f64() * 1e3,
                (read - painted).as_secs_f64() * 1e3,
            );
            if let Some(navigated) = self.navigated.take() {
                eprintln!(
                    "sig-servo: first frame {:.0} ms after navigating",
                    ms(navigated)
                );
            }
        }
    }
}

fn set_focus(webview: &WebView, focused: bool) {
    if focused {
        webview.focus();
    } else {
        webview.blur();
    }
}

fn to_servo_modifiers(bits: u8) -> Modifiers {
    let mut modifiers = Modifiers::empty();
    for (bit, modifier) in [
        (MOD_SHIFT, Modifiers::SHIFT),
        (MOD_CTRL, Modifiers::CONTROL),
        (MOD_ALT, Modifiers::ALT),
        (MOD_META, Modifiers::META),
    ] {
        if bits & bit != 0 {
            modifiers |= modifier;
        }
    }
    modifiers
}

/// Milliseconds since `since`.
fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}

struct Waker {
    tx: Sender<Msg>,
    /// Collapses bursts of wake-ups into a single queued message.
    pending: Arc<AtomicBool>,
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(Waker {
            tx: self.tx.clone(),
            pending: self.pending.clone(),
        })
    }

    fn wake(&self) {
        if !self.pending.swap(true, Ordering::SeqCst) {
            let _ = self.tx.send(Msg::Wake);
        }
    }
}

/// Makes panics visible to the shell.
///
/// A panicking page thread is reported to Servo's constellation through an `error!` log, as
/// servoshell does; with `hard_fail` off it then replaces the page with a crash page and calls
/// `notify_crashed`. Servo doesn't notice a panic on one of its style threads, and the page
/// stops rendering (a race in Servo 0.5 does this to developer.mozilla.org now and then), so
/// that goes to the shell as an error directly. So does a panic on the main thread, where
/// Servo's embedding API and this engine run, which ends the process.
fn report_panics(out: Output) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        // Servo catches these itself (image decoding).
        if servo::should_panic_hook_suppress_termination() {
            return;
        }
        let thread = std::thread::current();
        let name = thread.name().unwrap_or_default();
        if name != "main" {
            log::error!("{info}");
            if name.starts_with("StyleThread") {
                let message = format!("page crashed: {info}");
                let mut out = out.lock().unwrap_or_else(PoisonError::into_inner);
                let _ = FromEngine::Error { message }.write_to(&mut *out);
            }
            return;
        }
        // `try_lock`, so a panic in the middle of sending a message can't deadlock here or
        // interleave with it.
        if let Ok(mut out) = out.try_lock() {
            let message = format!("sig-servo crashed: {info}");
            let _ = FromEngine::Error { message }.write_to(&mut *out);
        }
    }));
}

/// Takes ownership of the real stdout for the protocol and points file descriptor 1 at stderr,
/// so stray `println!`s inside Servo or its dependencies cannot corrupt the message stream.
#[cfg(unix)]
fn protocol_output() -> BufWriter<Box<dyn Write + Send>> {
    use std::os::fd::{AsRawFd, FromRawFd};
    // SAFETY: plain descriptor duplication on this process' own stdio; `dup` hands us a fresh
    // descriptor that nothing else owns.
    let file = unsafe {
        let fd = libc::dup(std::io::stdout().as_raw_fd());
        assert!(fd >= 0, "failed to duplicate stdout");
        libc::dup2(std::io::stderr().as_raw_fd(), libc::STDOUT_FILENO);
        File::from_raw_fd(fd)
    };
    BufWriter::new(Box::new(file))
}

#[cfg(not(unix))]
fn protocol_output() -> BufWriter<Box<dyn Write + Send>> {
    BufWriter::new(Box::new(std::io::stdout()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_is_firefox() {
        assert_eq!(
            firefox_user_agent(
                "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Servo/0.5.0 Firefox/140.0"
            ),
            "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"
        );
        let default = firefox_user_agent(&Preferences::default().user_agent);
        assert!(default.contains(" Gecko/20100101 Firefox/"), "{default}");
    }

    #[test]
    fn style_thread_panics_are_reported() {
        struct Shared(Arc<Mutex<Vec<u8>>>);
        impl Write for Shared {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sent = Arc::new(Mutex::new(Vec::new()));
        let out: Output = Arc::new(Mutex::new(BufWriter::new(Box::new(Shared(sent.clone())))));
        report_panics(out.clone());
        // Servo recovers from a script thread's panic by itself.
        for name in ["StyleThread#1", "Script#1"] {
            let thread = std::thread::Builder::new().name(name.into());
            assert!(thread.spawn(|| panic!("boom")).unwrap().join().is_err());
        }
        drop(std::panic::take_hook());
        out.lock().unwrap().flush().unwrap();
        let sent = sent.lock().unwrap();
        let mut sent = &sent[..];
        let Ok(Some(FromEngine::Error { message })) = FromEngine::read_from(&mut sent) else {
            panic!("no error was sent");
        };
        assert!(message.starts_with("page crashed: "), "{message}");
        assert!(message.contains("boom"), "{message}");
        assert!(sent.is_empty(), "more than one message was sent");
    }

    #[test]
    fn sets_preferences_by_type() {
        let mut prefs = Preferences::default();
        set_pref(&mut prefs, "layout_grid_enabled=false").unwrap();
        assert!(!prefs.layout_grid_enabled);
        set_pref(&mut prefs, "user_agent=Test/1.0 (a=b)").unwrap();
        assert_eq!(prefs.user_agent, "Test/1.0 (a=b)");
        set_pref(&mut prefs, "fonts_default_size=20").unwrap();
        assert_eq!(prefs.fonts_default_size, 20);
        set_pref(&mut prefs, "network_http_cache_size=10").unwrap();
        assert_eq!(prefs.network_http_cache_size, 10);
        assert!(set_pref(&mut prefs, "no_such_pref=1").is_err());
        assert!(set_pref(&mut prefs, "layout_grid_enabled=yes").is_err());
        assert!(set_pref(&mut prefs, "layout_grid_enabled").is_err());
        assert!(set_pref(&mut prefs, "shell_background_color_rgba=1").is_err());
    }
}
