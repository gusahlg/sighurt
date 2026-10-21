//! Phase 0b: WPE WebKit shared-memory frames with no Wayland compositor
//! (docs/rewrite/TUI.md §0.3, §8.2, §8.6).
//!
//! The display is [`sighurt_engine_wpe::platform::Display`] with no DMA-BUF device, the same
//! `Display::new(None, ..)` the engine uses for its shared-memory fallback, but this process never
//! opens Wayland. WebKit is asked for `WPEBufferSHM` frames; each one is copied and then handed
//! back with `wpe_view_buffer_rendered` and `wpe_view_buffer_released`.
//!
//! Run it with `WAYLAND_DISPLAY` and `DISPLAY` unset, on `engines/sighurt-engine-wpe/tests/private-bus`,
//! so WebKit's sandbox talks only to a private session bus:
//!
//! ```text
//! tools/heavy env -u WAYLAND_DISPLAY -u DISPLAY \
//!   engines/sighurt-engine-wpe/tests/private-bus \
//!   target/debug/wpe-cpu --size 640x480 --seconds 3 --out /tmp/wpe-cpu-a \
//!   experiments/wpe-cpu/pages/animated.html
//! ```
//!
//! The second page defaults to `second.html` beside a `file:` URL. `--resize` defaults to about
//! two thirds of `--size`.

mod pngenc;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use glib::prelude::*;
use glib::translate::{from_glib, from_glib_full, from_glib_none, ToGlibPtr};
use sighurt_api::engine::EngineWaker;
use sighurt_engine_wpe::clipboard::{ClipboardCall, ClipboardSink};
use sighurt_engine_wpe::ffi;
use sighurt_engine_wpe::glib_loop::GlibLoop;
use sighurt_engine_wpe::platform::{Buffer, Call, Display, View};

/// `WebKitLoadEvent::WEBKIT_LOAD_FINISHED` (WebKitWebView.h: STARTED, REDIRECTED, COMMITTED, FINISHED).
const LOAD_FINISHED: u32 = 3;

/// How long to wait for the web process to paint the first frame.
const STARTUP_LIMIT: Duration = Duration::from_secs(45);

/// Throttle probe: stop acknowledging for this long (TUI.md §8.6).
const THROTTLE: Duration = Duration::from_secs(2);

/// Bound on unacknowledged frames, so a renderer that ignores the missing ack cannot fill memory.
const HOLD_CAP: usize = 48;

/// DOM extraction in the isolated world. The page under test is `pages/second.html`.
const DOM_SCRIPT: &str = r#"JSON.stringify({title:document.title,headings:document.querySelectorAll("h1,h2,h3,h4,h5,h6").length,links:document.querySelectorAll("a").length,inputs:document.querySelectorAll("input,textarea,select").length,first_link_href:(document.querySelector("a")&&document.querySelector("a").href)||""})"#;

const ISOLATED_PROBE: &str = r#"JSON.stringify({marker:(typeof window.__sighurtPage==="string")?window.__sighurtPage:null,dom_title:document.title})"#;

const MAIN_PROBE: &str = r#"JSON.stringify({marker:(typeof window.__sighurtPage==="string")?window.__sighurtPage:null})"#;

fn main() {
    let config = match Config::parse(std::env::args().skip(1)) {
        Ok(config) => config,
        Err(message) => {
            eprintln!("wpe-cpu: {message}");
            eprintln!(
                "usage: wpe-cpu [--size WxH] [--resize WxH] [--seconds S] [--out DIR] [--next URL] URL"
            );
            std::process::exit(2);
        }
    };
    match run(&config) {
        Ok(()) => {}
        Err(message) => {
            eprintln!("wpe-cpu: FAIL {message}");
            std::process::exit(1);
        }
    }
}

struct Config {
    url: String,
    next: String,
    size: (i32, i32),
    resize: (i32, i32),
    seconds: f64,
    out: PathBuf,
}

impl Config {
    fn parse(args: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut size = None;
        let mut resize = None;
        let mut seconds = 3.0;
        let mut out = PathBuf::from("wpe-cpu-out");
        let mut next = None;
        let mut urls = Vec::new();
        let args: Vec<String> = args.collect();
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].clone();
            index += 1;
            let mut value = |flag: &str| -> Result<String, String> {
                if index >= args.len() {
                    return Err(format!("{flag} needs a value"));
                }
                let next = args[index].clone();
                index += 1;
                Ok(next)
            };
            match arg.as_str() {
                "--size" => size = Some(parse_size(&value("--size")?)?),
                "--resize" => resize = Some(parse_size(&value("--resize")?)?),
                "--seconds" => {
                    seconds = value("--seconds")?
                        .parse()
                        .map_err(|_| "--seconds is not a number".to_string())?;
                }
                "--out" => out = PathBuf::from(value("--out")?),
                "--next" => next = Some(as_url(&value("--next")?)?),
                "--help" | "-h" => return Err("show usage".into()),
                other if other.starts_with('-') => return Err(format!("unknown argument {other}")),
                other => urls.push(as_url(other)?),
            }
        }
        if urls.len() != 1 {
            return Err("pass one page URL (a path is read as a file URL)".into());
        }
        if !(seconds > 0.0 && seconds <= 60.0) {
            return Err("--seconds must be in (0, 60]".into());
        }
        let size = size.unwrap_or((640, 480));
        let resize = resize.unwrap_or_else(|| default_resize(size.0, size.1));
        if resize == size {
            return Err("--resize must differ from --size".into());
        }
        let url = urls.remove(0);
        let next = match next {
            Some(next) => next,
            None => sibling_second(&url).ok_or("no --next, and no second.html beside the URL")?,
        };
        Ok(Self {
            url,
            next,
            size,
            resize,
            seconds,
            out,
        })
    }
}

fn parse_size(text: &str) -> Result<(i32, i32), String> {
    let (w, h) = text
        .split_once(['x', 'X'])
        .ok_or("--size expects WxH, for example 640x480")?;
    let w: i32 = w.parse().map_err(|_| format!("bad width in {text}"))?;
    let h: i32 = h.parse().map_err(|_| format!("bad height in {text}"))?;
    if !(16..=4096).contains(&w) || !(16..=4096).contains(&h) {
        return Err(format!("{text} is outside 16..=4096"));
    }
    Ok((w, h))
}

fn default_resize(w: i32, h: i32) -> (i32, i32) {
    let mut rw = (w * 2 / 3).max(64);
    let mut rh = (h * 2 / 3).max(64);
    if rw >= w {
        rw = (w / 2).max(16);
    }
    if rh >= h {
        rh = (h / 2).max(16);
    }
    if (rw, rh) == (w, h) {
        ((w - 8).max(16), (h - 8).max(16))
    } else {
        (rw, rh)
    }
}

fn as_url(arg: &str) -> Result<String, String> {
    if arg.contains("://") {
        return Ok(arg.to_string());
    }
    let path = std::fs::canonicalize(arg).map_err(|e| format!("{arg}: {e}"))?;
    Ok(format!("file://{}", path.display()))
}

fn sibling_second(url: &str) -> Option<String> {
    let path = url.strip_prefix("file://")?;
    let next = Path::new(path).parent()?.join("second.html");
    next.is_file().then(|| format!("file://{}", next.display()))
}

fn run(config: &Config) -> Result<(), String> {
    let mut log = Log::open(&config.out)?;
    log.line(&format!(
        "wpe-cpu: config url={} next={} size={}x{} resize={}x{} seconds={} out={}",
        config.url,
        config.next,
        config.size.0,
        config.size.1,
        config.resize.0,
        config.resize.1,
        config.seconds,
        config.out.display()
    ));
    report_environment(&mut log);
    prepare_process()?;
    log.line(&format!(
        "wpe-cpu: process JSC_SIGNAL_FOR_GC={} WEBKIT_DEBUG={}",
        env_show("JSC_SIGNAL_FOR_GC"),
        env_show("WEBKIT_DEBUG")
    ));
    let mut session = Session::start(config, &mut log)?;
    let phases = run_phases(&mut session, config, &mut log);
    let tracked = webkit_pids(std::process::id());
    log.line(&format!(
        "wpe-cpu: processes-before {}",
        format_procs(&tracked)
    ));
    let dispatches = session.dispatches();
    session.shutdown();
    let reaped = reap_children();
    let gone = wait_until_gone(&tracked, Duration::from_secs(8));
    match &gone {
        Ok(()) => log.line(&format!(
            "wpe-cpu: shutdown web-and-network-gone=yes reaped={reaped} glib_dispatches={dispatches}"
        )),
        Err(message) => log.line(&format!(
            "wpe-cpu: shutdown web-and-network-gone=no reaped={reaped} glib_dispatches={dispatches} {message}"
        )),
    }
    match (phases, gone) {
        (Ok(()), Ok(())) => {
            log.line("wpe-cpu: PASS");
            Ok(())
        }
        (Err(phase), Ok(())) => Err(phase),
        (Ok(()), Err(gone)) => Err(gone),
        (Err(phase), Err(gone)) => Err(format!("{phase}; {gone}")),
    }
}

fn run_phases(session: &mut Session, config: &Config, log: &mut Log) -> Result<(), String> {
    phase_animate(session, config, log)?;
    report_renderer(log, std::process::id());
    phase_navigate_resize(session, config, log)?;
    phase_javascript(session, log)?;
    phase_throttle(session, log)?;
    Ok(())
}

fn phase_animate(session: &mut Session, config: &Config, log: &mut Log) -> Result<(), String> {
    let want = u32_size(config.size);
    let mut measure = Measure::wanting(want);
    let startup = Instant::now();
    let found = until(
        session,
        STARTUP_LIMIT,
        Mode::Copy,
        &mut measure,
        |_, seen| seen.copied >= 1,
    )?;
    if !found {
        return Err(format!(
            "no SHM frame of {}x{} within {}s ({})",
            want.0,
            want.1,
            STARTUP_LIMIT.as_secs(),
            session.status()
        ));
    }
    let startup_ms = startup.elapsed().as_secs_f64() * 1e3;
    let before = measure.copied;
    let window = Instant::now();
    until(
        session,
        Duration::from_secs_f64(config.seconds),
        Mode::Copy,
        &mut measure,
        |_, _| false,
    )?;
    let elapsed = window.elapsed().as_secs_f64().max(1e-6);
    let window_frames = measure.copied - before;
    let fps = window_frames as f64 / elapsed;
    log.line(&format!(
        "wpe-cpu: animate {} window_frames={window_frames} window_s={elapsed:.3} fps={fps:.2} startup_ms={startup_ms:.1}",
        measure.summary()
    ));
    let (Some(first), Some(last)) = (measure.first.as_ref(), measure.last.as_ref()) else {
        return Err("the capture kept no frame".into());
    };
    log.line(&format!(
        "wpe-cpu: animate-samples first={} last={}",
        sample(first),
        sample(last)
    ));
    save_png(log, &config.out.join("first.png"), first)?;
    save_png(log, &config.out.join("last.png"), last)?;
    if measure.copied < 2 {
        return Err(format!(
            "only {} SHM frame of the requested size; need two to compare",
            measure.copied
        ));
    }
    let different = differing_pixels(first, last)?;
    log.line(&format!("wpe-cpu: animate-diff pixels={different}"));
    if different == 0 {
        return Err("the first and last SHM frames are identical".into());
    }
    Ok(())
}

fn phase_navigate_resize(
    session: &mut Session,
    config: &Config,
    log: &mut Log,
) -> Result<(), String> {
    let before = session.finishes();
    session.navigate(&config.next)?;
    let mut during = Measure::default();
    let loaded = until(
        session,
        Duration::from_secs(30),
        Mode::Release,
        &mut during,
        |session, _| session.finishes() > before,
    )?;
    log.line(&format!(
        "wpe-cpu: navigate loaded={loaded} uri={} finishes={} frames_during={} sizes={}",
        session.uri(),
        session.finishes(),
        during.produced,
        format_sizes(&during.sizes)
    ));
    if !loaded {
        return Err(format!(
            "the second page did not finish loading ({})",
            session.status()
        ));
    }
    let leaf = url_leaf(&config.next);
    if !session.uri().contains(leaf) {
        return Err(format!(
            "finished on {}, which does not contain {leaf}",
            session.uri()
        ));
    }
    let want = u32_size(config.resize);
    session.resize(config.resize)?;
    let mut resized = Measure::wanting(want);
    let painted = until(
        session,
        Duration::from_secs(20),
        Mode::Copy,
        &mut resized,
        |_, seen| seen.copied >= 1,
    )?;
    log.line(&format!(
        "wpe-cpu: resize to={}x{} painted={painted} {}",
        want.0,
        want.1,
        resized.summary()
    ));
    if !painted {
        return Err(format!(
            "no SHM frame of {}x{} within 20s of the resize ({})",
            want.0,
            want.1,
            session.status()
        ));
    }
    let Some(frame) = resized.last.as_ref() else {
        return Err("resize kept no frame".into());
    };
    save_png(log, &config.out.join("resized.png"), frame)?;
    Ok(())
}

fn phase_javascript(session: &mut Session, log: &mut Log) -> Result<(), String> {
    let json = session.evaluate(DOM_SCRIPT, Some("sighurt"))?;
    log.line(&format!("wpe-cpu: js {json}"));
    check_dom_json(&json)?;
    let isolated = session.evaluate(ISOLATED_PROBE, Some("sighurt"))?;
    let main = session.evaluate(MAIN_PROBE, None)?;
    log.line(&format!("wpe-cpu: js-isolated {isolated}"));
    log.line(&format!("wpe-cpu: js-main {main}"));
    if !isolated.contains("\"marker\":null") {
        return Err(format!(
            "the isolated world can see the page's window.__sighurtPage: {isolated}"
        ));
    }
    if !isolated.contains("\"dom_title\":\"wpe-cpu second\"") {
        return Err(format!(
            "the isolated world did not read the DOM title: {isolated}"
        ));
    }
    if !main.contains("\"marker\":\"page-world\"") {
        return Err(format!(
            "the main world did not see the page global (the page script did not run): {main}"
        ));
    }
    Ok(())
}

fn check_dom_json(json: &str) -> Result<(), String> {
    let title = json_string(json, "title").ok_or("js result has no title")?;
    let headings = json_number(json, "headings").ok_or("js result has no headings")?;
    let links = json_number(json, "links").ok_or("js result has no links")?;
    let inputs = json_number(json, "inputs").ok_or("js result has no inputs")?;
    let href = json_string(json, "first_link_href").ok_or("js result has no first_link_href")?;
    if title != "wpe-cpu second" || headings != 3 || links != 2 || inputs != 2 {
        return Err(format!(
            "js counts title={title:?} headings={headings} links={links} inputs={inputs}, want title \"wpe-cpu second\", 3, 2, 2"
        ));
    }
    if !href.contains("animated.html") {
        return Err(format!(
            "first_link_href {href} does not point at animated.html"
        ));
    }
    Ok(())
}

fn phase_throttle(session: &mut Session, log: &mut Log) -> Result<(), String> {
    // Drop anything already queued, acknowledged, so the probe counts only frames that arrive
    // while we are not calling the rendered/released callbacks.
    session.drain(Mode::Release, &mut Measure::default())?;
    let start = Instant::now();
    let end = start + THROTTLE;
    let mut produced = 0u64;
    let mut last = None;
    while Instant::now() < end && session.held.len() < HOLD_CAP {
        if let Some(err) = session.fatal() {
            return Err(err);
        }
        let next = session.pump_once();
        let arrived = session.drain(Mode::Hold, &mut Measure::default())?;
        if arrived > 0 {
            produced += arrived;
            last = Some(Instant::now());
        }
        if session.held.len() >= HOLD_CAP {
            break;
        }
        session.pace(next, end);
    }
    let elapsed = start.elapsed().as_secs_f64();
    let last_ms = last
        .map(|at| format!("{:.1}", at.duration_since(start).as_secs_f64() * 1e3))
        .unwrap_or_else(|| "none".into());
    let held = session.held.len();
    log.line(&format!(
        "wpe-cpu: throttle seconds={elapsed:.3} produced={produced} acknowledged=0 held={held} last_produced_ms={last_ms} hit_cap={}",
        held >= HOLD_CAP
    ));
    session.release_held();
    Ok(())
}

/// What to do with a frame WebKit has handed over.
#[derive(Clone, Copy)]
enum Mode {
    /// Copy pixels of frames at [`Measure::want`] (any size when `want` is unset). Acknowledge.
    Copy,
    /// Acknowledge without copying. Sizes are still recorded.
    Release,
    /// Keep the buffer and do not acknowledge.
    Hold,
}

struct Measure {
    produced: u64,
    copied: u64,
    copy_total: Duration,
    copy_max: Duration,
    stale: u64,
    first: Option<Image>,
    last: Option<Image>,
    sizes: BTreeMap<(u32, u32), u64>,
    want: Option<(u32, u32)>,
}

impl Default for Measure {
    fn default() -> Self {
        Self {
            produced: 0,
            copied: 0,
            copy_total: Duration::ZERO,
            copy_max: Duration::ZERO,
            stale: 0,
            first: None,
            last: None,
            sizes: BTreeMap::new(),
            want: None,
        }
    }
}

impl Measure {
    fn wanting(size: (u32, u32)) -> Self {
        Self {
            want: Some(size),
            ..Self::default()
        }
    }

    fn summary(&self) -> String {
        let mean = if self.copied == 0 {
            0.0
        } else {
            self.copy_total.as_secs_f64() * 1e3 / self.copied as f64
        };
        format!(
            "frames={} produced={} copy_ms_mean={mean:.3} copy_ms_max={:.3} copy_ms_total={:.3} stale={} sizes={}",
            self.copied,
            self.produced,
            self.copy_max.as_secs_f64() * 1e3,
            self.copy_total.as_secs_f64() * 1e3,
            self.stale,
            format_sizes(&self.sizes)
        )
    }
}

#[derive(Clone)]
struct Image {
    width: u32,
    height: u32,
    /// Tightly packed premultiplied B, G, R, A as `WPE_PIXEL_FORMAT_ARGB8888` stores it.
    bgra: Vec<u8>,
}

#[derive(Default)]
struct Shared {
    frames: Vec<Buffer>,
    load_event: u32,
    load_finished: u32,
    uri: String,
    load_error: Option<String>,
    terminated: Option<String>,
}

struct ScriptSlot(RefCell<Option<Result<String, String>>>);

/// The GLib watch thread's wake, so the owner sleeps until WebKit's next timeout or a ready fd.
///
/// [`EngineWaker`] only calls its notify on the first wake after [`EngineWaker::take`], and the
/// watch thread calls it. `flag` is that notify. The owner takes the waker before each pump.
struct Alarm {
    pair: Arc<(Mutex<bool>, Condvar)>,
}

impl Alarm {
    fn new() -> Self {
        Self {
            pair: Arc::new((Mutex::new(false), Condvar::new())),
        }
    }

    fn waker(&self) -> EngineWaker {
        let pair = Arc::clone(&self.pair);
        EngineWaker::new(move || {
            let (lock, condvar) = &*pair;
            *lock.lock().expect("wpe-cpu wake lock") = true;
            condvar.notify_one();
        })
    }

    fn wait_until(&self, deadline: Instant) {
        let (lock, condvar) = &*self.pair;
        let mut flagged = lock.lock().expect("wpe-cpu wake lock");
        while !*flagged {
            let now = Instant::now();
            if now >= deadline {
                return;
            }
            let (guard, status) = condvar
                .wait_timeout(flagged, deadline.saturating_duration_since(now))
                .expect("wpe-cpu wake lock");
            flagged = guard;
            if status.timed_out() {
                break;
            }
        }
        *flagged = false;
    }
}

struct Session {
    shared: Rc<RefCell<Shared>>,
    glib: Option<GlibLoop>,
    waker: EngineWaker,
    alarm: Alarm,
    display: Option<Display>,
    context: Option<glib::Object>,
    network: Option<glib::Object>,
    web_view: Option<glib::Object>,
    view: Option<View>,
    held: Vec<Buffer>,
    shut_down: bool,
}

impl Session {
    fn start(config: &Config, log: &mut Log) -> Result<Self, String> {
        ffi::load()?;
        keep_jsc_off_signals()?;
        log.line("wpe-cpu: jsc usePollingTraps=true useWasmFaultSignalHandler=false");
        let shared = Rc::new(RefCell::new(Shared::default()));
        let alarm = Alarm::new();
        let waker = alarm.waker();
        let glib = GlibLoop::new(waker.clone()).map_err(|e| format!("glib: {e}"))?;
        let clipboard: ClipboardSink = Rc::new(|_: ClipboardCall| {});
        let display = Display::new(None, clipboard).map_err(|e| format!("WPE display: {e}"))?;
        log.line("wpe-cpu: display SighurtWPEDisplay primary dmabuf=none (WPEBufferSHM)");
        let context = web_context();
        let network = network_session();
        let web_view =
            glib::Object::builder_with_type(unsafe { from_glib(ffi::webkit_web_view_get_type()) })
                .property("display", display.clone())
                .property("web-context", &context)
                .property("network-session", &network)
                .build();
        connect_signals(&web_view, &shared);
        let view = wpe_view(&web_view)?;
        let sink_shared = Rc::clone(&shared);
        view.set_sink(Some(Rc::new(move |call| {
            if let Call::Frame(buffer, _) = call {
                sink_shared.borrow_mut().frames.push(buffer);
            }
        })));
        let mut session = Self {
            shared,
            glib: Some(glib),
            waker,
            alarm,
            display: Some(display),
            context: Some(context),
            network: Some(network),
            web_view: Some(web_view),
            view: Some(view),
            held: Vec::new(),
            shut_down: false,
        };
        session.resize(config.size)?;
        session.map_and_focus()?;
        session.navigate(&config.url)?;
        log.line(&format!("wpe-cpu: load {}", config.url));
        Ok(session)
    }

    fn resize(&mut self, size: (i32, i32)) -> Result<(), String> {
        let view = self.view_ptr()?;
        let object: glib::Object =
            unsafe { from_glib_none(view.cast::<glib::gobject_ffi::GObject>()) };
        let toplevel: glib::Object = object.property("toplevel");
        let top = toplevel.as_ptr().cast();
        unsafe {
            ffi::wpe_toplevel_scale_changed(top, 1.0);
            ffi::wpe_toplevel_resized(top, size.0, size.1);
            ffi::wpe_view_resized(view, size.0, size.1);
        }
        Ok(())
    }

    fn map_and_focus(&mut self) -> Result<(), String> {
        let view = self.view_ptr()?;
        unsafe {
            ffi::wpe_view_map(view);
            ffi::wpe_view_focus_in(view);
        }
        Ok(())
    }

    fn navigate(&mut self, url: &str) -> Result<(), String> {
        self.shared.borrow_mut().load_error = None;
        let url = CString::new(url).map_err(|_| "NUL in a URL".to_string())?;
        unsafe { ffi::webkit_web_view_load_uri(self.web_ptr()?, url.as_ptr()) };
        Ok(())
    }

    /// Runs `script` and returns its string result. `world` `Some` is an isolated world of that
    /// name; `None` is the page's main world. Frames that arrive while waiting are acknowledged
    /// so the web process is not stuck on a buffer.
    fn evaluate(&mut self, script: &str, world: Option<&str>) -> Result<String, String> {
        let script = CString::new(script).map_err(|_| "NUL in a script".to_string())?;
        let world = world
            .map(CString::new)
            .transpose()
            .map_err(|_| "NUL in a world name".to_string())?;
        let slot = Rc::new(ScriptSlot(RefCell::new(None)));
        let raw = Rc::into_raw(Rc::clone(&slot));
        let web = self.web_ptr()?;
        unsafe {
            ffi::webkit_web_view_evaluate_javascript(
                web,
                script.as_ptr(),
                -1,
                world.as_ref().map_or(std::ptr::null(), |w| w.as_ptr()),
                std::ptr::null(),
                std::ptr::null_mut(),
                Some(script_done),
                raw.cast_mut().cast(),
            );
        }
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if slot.0.borrow().is_some() {
                break;
            }
            if Instant::now() >= end {
                return Err("javascript timed out after 15s".into());
            }
            if let Some(err) = self.fatal() {
                return Err(err);
            }
            let next = self.pump_once();
            self.drain(Mode::Release, &mut Measure::default())?;
            if slot.0.borrow().is_some() {
                break;
            }
            self.pace(next, end);
        }
        let outcome = slot.0.borrow_mut().take();
        match outcome {
            Some(outcome) => outcome,
            None => Err("javascript produced nothing".to_string()),
        }
    }

    /// Acknowledges buffers kept by [`Mode::Hold`].
    fn release_held(&mut self) {
        let Ok(view) = self.view_ptr() else {
            self.held.clear();
            return;
        };
        for buffer in self.held.drain(..) {
            ack(view, &buffer);
        }
    }

    /// Takes queued frames. Returns how many were held or acknowledged.
    fn drain(&mut self, mode: Mode, measure: &mut Measure) -> Result<u64, String> {
        let view = self.view_ptr()?;
        let mut incoming: VecDeque<Buffer> =
            std::mem::take(&mut self.shared.borrow_mut().frames).into();
        let mut handled = 0u64;
        while let Some(buffer) = incoming.pop_front() {
            handled += 1;
            let size = match classify(&buffer) {
                Ok(size) => size,
                Err(error) => {
                    ack(view, &buffer);
                    for rest in incoming {
                        ack(view, &rest);
                    }
                    return Err(error);
                }
            };
            *measure.sizes.entry(size).or_insert(0) += 1;
            measure.produced += 1;
            let keep = match mode {
                Mode::Hold => {
                    self.held.push(buffer);
                    continue;
                }
                Mode::Release => false,
                Mode::Copy => measure.want.is_none_or(|want| want == size),
            };
            if !keep {
                measure.stale += 1;
                ack(view, &buffer);
                continue;
            }
            let started = Instant::now();
            let image = match copy_image(&buffer, size) {
                Ok(image) => image,
                Err(error) => {
                    ack(view, &buffer);
                    for rest in incoming {
                        ack(view, &rest);
                    }
                    return Err(error);
                }
            };
            let took = started.elapsed();
            measure.copy_total += took;
            measure.copy_max = measure.copy_max.max(took);
            measure.copied += 1;
            if measure.first.is_none() {
                measure.first = Some(image);
                measure.last = measure.first.clone();
            } else {
                measure.last = Some(image);
            }
            ack(view, &buffer);
        }
        Ok(handled)
    }

    fn pump_once(&mut self) -> Option<Instant> {
        // `take` before the pump: a wake that arrives during it notifies again.
        self.waker.take();
        self.glib.as_mut().and_then(GlibLoop::iterate)
    }

    /// Sleeps until `next` (GLib's timeout), `end`, or the watch thread's wake, whichever is first.
    fn pace(&self, next: Option<Instant>, end: Instant) {
        let now = Instant::now();
        if now >= end {
            return;
        }
        let target = match next {
            Some(at) if at <= now => return,
            Some(at) => at.min(end),
            None => (now + Duration::from_millis(15)).min(end),
        };
        self.alarm.wait_until(target);
    }

    fn fatal(&self) -> Option<String> {
        let shared = self.shared.borrow();
        shared
            .terminated
            .clone()
            .or_else(|| shared.load_error.clone())
    }

    fn finishes(&self) -> u32 {
        self.shared.borrow().load_finished
    }

    fn uri(&self) -> String {
        self.shared.borrow().uri.clone()
    }

    fn status(&self) -> String {
        let shared = self.shared.borrow();
        format!(
            "uri={} load_event={} finishes={} error={} terminated={} queued={} held={}",
            shared.uri,
            shared.load_event,
            shared.load_finished,
            shared.load_error.as_deref().unwrap_or("-"),
            shared.terminated.as_deref().unwrap_or("-"),
            shared.frames.len(),
            self.held.len()
        )
    }

    fn dispatches(&self) -> u64 {
        self.glib.as_ref().map(|glib| glib.dispatches).unwrap_or(0)
    }

    fn view_ptr(&self) -> Result<*mut ffi::WPEView, String> {
        self.view
            .as_ref()
            .map(View::as_ptr)
            .ok_or_else(|| "the WPE view is gone".to_string())
    }

    fn web_ptr(&self) -> Result<*mut ffi::WebKitWebView, String> {
        self.web_view
            .as_ref()
            .map(|web| web.as_ptr().cast())
            .ok_or_else(|| "the web view is gone".to_string())
    }

    /// The engine's order (`WpeRuntime::close`, then `shutdown`): acknowledge buffers we hold,
    /// drop the web view, let GLib deliver the close, drop the web context (that ends spare web
    /// processes), drop the network session and the display, then release GLib's main context.
    fn shutdown(&mut self) {
        if self.shut_down {
            return;
        }
        self.shut_down = true;
        if let Some(view) = self.view.as_ref().map(View::as_ptr) {
            for _ in 0..8 {
                let queued = std::mem::take(&mut self.shared.borrow_mut().frames);
                let held = std::mem::take(&mut self.held);
                if queued.is_empty() && held.is_empty() {
                    break;
                }
                for buffer in queued.iter().chain(&held) {
                    ack(view, buffer);
                }
            }
        }
        if let Some(view) = self.view.as_ref() {
            view.set_sink(None);
        }
        // A frame queued by the last ack, before the sink was cleared, still has to be returned.
        if let Some(view) = self.view.as_ref().map(View::as_ptr) {
            let queued = std::mem::take(&mut self.shared.borrow_mut().frames);
            let held = std::mem::take(&mut self.held);
            for buffer in queued.iter().chain(&held) {
                ack(view, buffer);
            }
        }
        self.held.clear();
        self.shared.borrow_mut().frames.clear();
        self.view = None;
        self.web_view = None;
        self.turn_for(Duration::from_millis(400));
        self.context = None;
        self.turn_for(Duration::from_millis(400));
        self.network = None;
        self.display = None;
        self.turn_for(Duration::from_millis(200));
        self.glib = None;
    }

    fn turn_for(&mut self, total: Duration) {
        let end = Instant::now() + total;
        while Instant::now() < end {
            if self.glib.is_none() {
                return;
            }
            let next = self.pump_once();
            self.pace(next, end);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn until(
    session: &mut Session,
    timeout: Duration,
    mode: Mode,
    measure: &mut Measure,
    mut done: impl FnMut(&Session, &Measure) -> bool,
) -> Result<bool, String> {
    let end = Instant::now() + timeout;
    loop {
        if let Some(err) = session.fatal() {
            return Err(err);
        }
        let next = session.pump_once();
        session.drain(mode, measure)?;
        if done(session, measure) {
            return Ok(true);
        }
        if Instant::now() >= end {
            session.pump_once();
            session.drain(mode, measure)?;
            return Ok(done(session, measure));
        }
        session.pace(next, end);
    }
}

fn connect_signals(web: &glib::Object, shared: &Rc<RefCell<Shared>>) {
    let slot = Rc::clone(shared);
    web.connect_local("load-changed", false, move |args| {
        let event = enum_arg(&args[1]);
        let web: glib::Object = args[0].get().ok()?;
        let uri = unsafe { ffi::string(ffi::webkit_web_view_get_uri(web.as_ptr().cast())) }
            .unwrap_or_default();
        let mut shared = slot.borrow_mut();
        shared.load_event = event;
        shared.uri = uri;
        if event == LOAD_FINISHED {
            shared.load_finished += 1;
        }
        None
    });
    let slot = Rc::clone(shared);
    web.connect_local("load-failed", false, move |args| {
        let message = args
            .get(3)
            .and_then(|value| value.get::<glib::Error>().ok())
            .map(|error| error.to_string())
            .unwrap_or_else(|| "load failed".to_string());
        slot.borrow_mut().load_error = Some(message);
        Some(true.to_value())
    });
    let slot = Rc::clone(shared);
    web.connect_local("web-process-terminated", false, move |args| {
        let reason = enum_arg(&args[1]);
        slot.borrow_mut().terminated = Some(format!("web process terminated (reason {reason})"));
        None
    });
}

fn enum_arg(value: &glib::Value) -> u32 {
    unsafe { glib::gobject_ffi::g_value_get_enum(value.to_glib_none().0) as u32 }
}

fn wpe_view(web: &glib::Object) -> Result<View, String> {
    let raw = unsafe { ffi::webkit_web_view_get_wpe_view(web.as_ptr().cast()) };
    if raw.is_null() {
        return Err("WebKit did not make a WPE view".into());
    }
    let object: glib::Object = unsafe { from_glib_none(raw.cast::<glib::gobject_ffi::GObject>()) };
    object
        .downcast::<View>()
        .map_err(|_| "WebKit's view is not Sighurt's WPE view".to_string())
}

/// A document-browser web context, so WebKit does not keep a closed page's process for reuse.
/// Copied from `sighurt-engine-wpe`'s private `WebContext::new`: the cache model is read when the
/// context is created.
fn web_context() -> glib::Object {
    unsafe {
        ffi::webkit_web_context_set_cache_model(
            ffi::webkit_web_context_get_default(),
            ffi::WEBKIT_CACHE_MODEL_DOCUMENT_BROWSER,
        );
        from_glib_full(ffi::webkit_web_context_new())
    }
}

fn network_session() -> glib::Object {
    unsafe { from_glib_full(ffi::webkit_network_session_new_ephemeral()) }
}

fn keep_jsc_off_signals() -> Result<(), String> {
    for (option, value) in [
        (c"usePollingTraps", true),
        (c"useWasmFaultSignalHandler", false),
    ] {
        let mut now = 0;
        let ok = unsafe {
            ffi::jsc_options_set_boolean(option.as_ptr(), i32::from(value)) != 0
                && ffi::jsc_options_get_boolean(option.as_ptr(), &mut now) != 0
        };
        if !ok || (now != 0) != value {
            return Err(format!("JavaScriptCore option {option:?} is not {value}"));
        }
    }
    Ok(())
}

fn classify(buffer: &Buffer) -> Result<(u32, u32), String> {
    let name = unsafe { ffi::type_name(buffer.as_ptr()) };
    if buffer.is_dma_buf() || !(name.contains("SHM") || name.contains("Shm")) {
        return Err(format!(
            "frame is {name}, size {:?}, not WPEBufferSHM",
            buffer.size()
        ));
    }
    let (width, height) = buffer.size();
    if width == 0 || height == 0 {
        return Err(format!("SHM frame has a zero dimension ({width}x{height})"));
    }
    Ok((width, height))
}

fn copy_image(buffer: &Buffer, size: (u32, u32)) -> Result<Image, String> {
    // SAFETY: `buffer` is a live `WPEBufferSHM` this process holds. The bytes pointer is only
    // used to copy out of, before the buffer is released back to WebKit.
    unsafe {
        let raw = buffer.as_ptr();
        let format = ffi::wpe_buffer_shm_get_format(raw);
        if format != ffi::WPE_PIXEL_FORMAT_ARGB8888 {
            return Err(format!("SHM format {format} is not ARGB8888"));
        }
        let stride = ffi::wpe_buffer_shm_get_stride(raw) as usize;
        let bytes = ffi::wpe_buffer_shm_get_data(raw);
        if bytes.is_null() {
            return Err("SHM frame has no GBytes".into());
        }
        let mut len = 0usize;
        let data = glib::ffi::g_bytes_get_data(bytes, &mut len);
        if data.is_null() {
            return Err("SHM frame data is null".into());
        }
        let row = size.0 as usize * 4;
        if stride < row || (size.1 > 0 && len < stride * (size.1 as usize)) {
            return Err(format!(
                "SHM frame is {len} bytes, stride {stride}, for {}x{}",
                size.0, size.1
            ));
        }
        let source = std::slice::from_raw_parts(data.cast::<u8>(), len);
        let mut bgra = vec![0u8; row * size.1 as usize];
        for y in 0..size.1 as usize {
            let from = y * stride;
            bgra[y * row..(y + 1) * row].copy_from_slice(&source[from..from + row]);
        }
        Ok(Image {
            width: size.0,
            height: size.1,
            bgra,
        })
    }
}

fn ack(view: *mut ffi::WPEView, buffer: &Buffer) {
    // SAFETY: `view` is the live view WebKit delivered `buffer` to. These two calls are how the
    // engine returns an SHM frame: rendered, then released, so WebKit paints another and reuses
    // the buffer.
    unsafe {
        ffi::wpe_view_buffer_rendered(view, buffer.as_ptr());
        ffi::wpe_view_buffer_released(view, buffer.as_ptr());
    }
}

unsafe extern "C" fn script_done(
    source: *mut glib::gobject_ffi::GObject,
    result: *mut ffi::GAsyncResult,
    data: glib::ffi::gpointer,
) {
    // SAFETY: `data` is the `Rc::into_raw` in `evaluate`, and WebKit calls this once.
    let slot = unsafe { Rc::from_raw(data.cast::<ScriptSlot>()) };
    let mut error = std::ptr::null_mut();
    let value = unsafe {
        ffi::webkit_web_view_evaluate_javascript_finish(source.cast(), result, &mut error)
    };
    let outcome = if value.is_null() {
        if error.is_null() {
            Err("javascript returned nothing".to_string())
        } else {
            let error: glib::Error = unsafe { from_glib_full(error) };
            Err(error.to_string())
        }
    } else {
        let raw = unsafe { ffi::jsc_value_to_string(value) };
        unsafe { glib::gobject_ffi::g_object_unref(value.cast()) };
        if raw.is_null() {
            Err("javascript result has no string form".to_string())
        } else {
            let text: glib::GString = unsafe { from_glib_full(raw) };
            Ok(text.to_string())
        }
    };
    *slot.0.borrow_mut() = Some(outcome);
}

fn differing_pixels(first: &Image, last: &Image) -> Result<usize, String> {
    if (first.width, first.height) != (last.width, last.height)
        || first.bgra.len() != last.bgra.len()
    {
        return Err(format!(
            "frame sizes {}x{} and {}x{}",
            first.width, first.height, last.width, last.height
        ));
    }
    Ok(first
        .bgra
        .as_chunks::<4>()
        .0
        .iter()
        .zip(last.bgra.as_chunks::<4>().0)
        .filter(|(a, b)| *a != *b)
        .count())
}

fn sample(image: &Image) -> String {
    let pixel = |x: u32, y: u32| {
        let index = (y as usize * image.width as usize + x as usize) * 4;
        let pixel = &image.bgra[index..index + 4];
        format!(
            "#{:02x}{:02x}{:02x}{:02x}",
            pixel[2], pixel[1], pixel[0], pixel[3]
        )
    };
    format!(
        "{}x{} tl={} center={} br={}",
        image.width,
        image.height,
        pixel(0, 0),
        pixel(image.width / 2, image.height / 2),
        pixel(image.width - 1, image.height - 1)
    )
}

fn to_rgba(image: &Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.bgra.len());
    for pixel in image.bgra.as_chunks::<4>().0 {
        let (b, g, r, a) = (pixel[0], pixel[1], pixel[2], pixel[3]);
        if a == 0 || a == 255 {
            out.extend_from_slice(&[r, g, b, a]);
        } else {
            out.push(((u16::from(r) * 255) / u16::from(a)) as u8);
            out.push(((u16::from(g) * 255) / u16::from(a)) as u8);
            out.push(((u16::from(b) * 255) / u16::from(a)) as u8);
            out.push(a);
        }
    }
    out
}

fn save_png(log: &mut Log, path: &Path, image: &Image) -> Result<(), String> {
    pngenc::write_rgba(path, image.width, image.height, &to_rgba(image))?;
    let bytes = std::fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    log.line(&format!(
        "wpe-cpu: png {} bytes={bytes} {}",
        path.display(),
        sample(image)
    ));
    Ok(())
}

fn u32_size(size: (i32, i32)) -> (u32, u32) {
    (size.0 as u32, size.1 as u32)
}

fn format_sizes(sizes: &BTreeMap<(u32, u32), u64>) -> String {
    if sizes.is_empty() {
        return "-".into();
    }
    sizes
        .iter()
        .map(|((w, h), n)| format!("{w}x{h}:{n}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn json_string(json: &str, key: &str) -> Option<String> {
    let rest = json.split_once(&format!("\"{key}\":\""))?.1;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

fn json_number(json: &str, key: &str) -> Option<u64> {
    let rest = json.split_once(&format!("\"{key}\":"))?.1;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

fn url_leaf(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

struct Log {
    file: std::io::BufWriter<std::fs::File>,
}

impl Log {
    fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| format!("out dir: {e}"))?;
        let file = std::fs::File::create(dir.join("report.txt"))
            .map_err(|error| format!("report: {error}"))?;
        Ok(Self {
            file: std::io::BufWriter::new(file),
        })
    }

    fn line(&mut self, text: &str) {
        println!("{text}");
        use std::io::Write;
        let _ = writeln!(self.file, "{text}");
        let _ = self.file.flush();
    }
}

fn prepare_process() -> Result<(), String> {
    for key in ["WAYLAND_DISPLAY", "DISPLAY"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                return Err(format!(
                    "{key} is set to {value}; this run refuses a display"
                ));
            }
        }
    }
    // SAFETY: `main` has not started another thread yet. JavaScriptCore reads `JSC_SIGNAL_FOR_GC`
    // when its VM is created, which is after this, and WebKit's debug channels likewise.
    unsafe {
        if std::env::var_os("JSC_SIGNAL_FOR_GC").is_none() {
            std::env::set_var("JSC_SIGNAL_FOR_GC", "12");
        }
        if std::env::var_os("WEBKIT_DEBUG").is_none() {
            std::env::set_var("WEBKIT_DEBUG", "PlatformDisplay,Compositing");
        }
    }
    Ok(())
}

fn report_environment(log: &mut Log) {
    log.line(&format!(
        "wpe-cpu: host hostname={} wayland={} display={} xdg_runtime={} libgl={} dri_prime={} webkit_debug={}",
        hostname(),
        env_show("WAYLAND_DISPLAY"),
        env_show("DISPLAY"),
        env_show("XDG_RUNTIME_DIR"),
        env_show("LIBGL_ALWAYS_SOFTWARE"),
        env_show("DRI_PRIME"),
        env_show("WEBKIT_DEBUG")
    ));
    log.line(&format!(
        "wpe-cpu: devices drm=[{}] renderD128=[{}] card0=[{}] nvidia0=[{}] nvidiactl=[{}]",
        drm_cards(),
        try_open("/dev/dri/renderD128"),
        try_open("/dev/dri/card0"),
        try_open("/dev/nvidia0"),
        try_open("/dev/nvidiactl")
    ));
}

fn report_renderer(log: &mut Log, root: u32) {
    let unix = unix_socket_paths();
    for proc in process_tree(root) {
        if proc.pid != root && !is_webkit(&proc) && !is_helper(&proc) {
            continue;
        }
        let libs = mapped_libs(proc.pid);
        let fds = device_fds(proc.pid, &unix);
        let env = proc_env(
            proc.pid,
            &[
                "LIBGL_ALWAYS_SOFTWARE",
                "DRI_PRIME",
                "WEBKIT_DEBUG",
                "LD_LIBRARY_PATH",
            ],
        );
        log.line(&format!(
            "wpe-cpu: proc pid={} ppid={} comm={} cmd={} libs=[{}] fds=[{}] env=[{}]",
            proc.pid,
            proc.ppid,
            proc.comm,
            proc.cmdline,
            libs.join(" "),
            fds.join(" "),
            env.join(" ")
        ));
    }
}

fn env_show(key: &str) -> String {
    match std::env::var(key) {
        Ok(value) if !value.is_empty() => value,
        _ => "<unset>".into(),
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn drm_cards() -> String {
    let mut names = Vec::new();
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return "unreadable".into();
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let driver = std::fs::read_link(entry.path().join("device/driver"))
            .ok()
            .and_then(|path| path.file_name().map(|s| s.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "?".into());
        names.push(format!("{name}:{driver}"));
    }
    if names.is_empty() {
        "-".into()
    } else {
        names.join(",")
    }
}

fn try_open(path: &str) -> String {
    match std::fs::File::open(path) {
        Ok(_) => "open-ok".into(),
        Err(error) => error.to_string(),
    }
}

struct Proc {
    pid: u32,
    ppid: u32,
    comm: String,
    cmdline: String,
    state: char,
}

fn processes() -> Vec<Proc> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        let Some((ppid, state)) = stat_ppid_state(&stat) else {
            continue;
        };
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let cmdline = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        out.push(Proc {
            pid,
            ppid,
            comm,
            cmdline: cmdline.trim().to_string(),
            state,
        });
    }
    out
}

fn stat_ppid_state(stat: &str) -> Option<(u32, char)> {
    let rest = stat.rsplit_once(')')?.1.trim_start();
    let mut parts = rest.split_whitespace();
    let state = parts.next()?.chars().next()?;
    let ppid = parts.next()?.parse().ok()?;
    Some((ppid, state))
}

fn process_tree(root: u32) -> Vec<Proc> {
    let all = processes();
    let mut pids = vec![root];
    let mut index = 0;
    while index < pids.len() {
        let parent = pids[index];
        for proc in &all {
            if proc.ppid == parent && !pids.contains(&proc.pid) {
                pids.push(proc.pid);
            }
        }
        index += 1;
    }
    all.into_iter()
        .filter(|proc| pids.contains(&proc.pid))
        .collect()
}

fn is_webkit(proc: &Proc) -> bool {
    let text = format!("{} {}", proc.comm, proc.cmdline);
    text.contains("WPEWebProcess")
        || text.contains("WPENetworkProcess")
        || proc.comm.starts_with("WPEWeb")
        || proc.comm.starts_with("WPENetwork")
}

fn is_helper(proc: &Proc) -> bool {
    let text = format!("{} {}", proc.comm, proc.cmdline);
    text.contains("bwrap") || text.contains("xdg-dbus-proxy")
}

fn webkit_pids(root: u32) -> Vec<Proc> {
    process_tree(root).into_iter().filter(is_webkit).collect()
}

fn format_procs(procs: &[Proc]) -> String {
    if procs.is_empty() {
        return "none".into();
    }
    procs
        .iter()
        .map(|proc| format!("{}:{}:{}", proc.pid, proc.state, proc.comm))
        .collect::<Vec<_>>()
        .join(",")
}

fn wait_until_gone(tracked: &[Proc], timeout: Duration) -> Result<(), String> {
    let end = Instant::now() + timeout;
    let root = std::process::id();
    loop {
        reap_children();
        let alive: Vec<String> = tracked
            .iter()
            .filter(|proc| process_running(proc.pid))
            .map(|proc| format!("{}:{}", proc.pid, proc.comm))
            .collect();
        let descendants: Vec<String> = webkit_pids(root)
            .iter()
            .filter(|proc| process_running(proc.pid))
            .map(|proc| format!("{}:{}:{}", proc.pid, proc.state, proc.comm))
            .collect();
        if alive.is_empty() && descendants.is_empty() {
            return Ok(());
        }
        if Instant::now() >= end {
            return Err(format!(
                "webkit processes still running tracked=[{}] descendants=[{}]",
                alive.join(","),
                descendants.join(",")
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn process_running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    match stat_ppid_state(&stat) {
        Some((_, 'Z')) | None => false,
        Some(_) => true,
    }
}

fn mapped_libs(pid: u32) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/maps")) else {
        return vec!["<unreadable>".into()];
    };
    let mut found = BTreeSet::new();
    for line in text.lines() {
        let Some(path) = line.split_whitespace().last() else {
            continue;
        };
        if !path.starts_with('/') {
            continue;
        }
        let base = path.rsplit('/').next().unwrap_or(path);
        let interesting = base.contains("libEGL")
            || base.contains("libGL")
            || base.contains("libgbm")
            || base.contains("libdrm")
            || base.contains("nvidia")
            || base.contains("llvmpipe")
            || base.contains("swrast")
            || base.contains("iris")
            || base.contains("zink")
            || base.contains("nouveau")
            || base.contains("radeon")
            || base.contains("libglapi")
            || path.contains("/dri/");
        if interesting {
            found.insert(path.to_string());
        }
    }
    found.into_iter().take(24).collect()
}

fn device_fds(pid: u32, unix: &HashMap<String, String>) -> Vec<String> {
    let mut found = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return vec!["<unreadable>".into()];
    };
    for entry in entries.flatten() {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        let text = target.display().to_string();
        if let Some(inode) = text
            .strip_prefix("socket:[")
            .and_then(|s| s.strip_suffix(']'))
        {
            if let Some(path) = unix.get(inode) {
                if path.contains("wayland") || path.contains("/dev/dri") || path.contains("nvidia")
                {
                    found.insert(path.clone());
                }
            }
            continue;
        }
        if text.contains("/dev/dri") || text.contains("/dev/nvidia") || text.contains("wayland") {
            found.insert(text);
        }
    }
    found.into_iter().collect()
}

fn proc_env(pid: u32, keys: &[&str]) -> Vec<String> {
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return vec!["<unreadable>".into()];
    };
    bytes
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let text = String::from_utf8_lossy(entry);
            let (key, _) = text.split_once('=')?;
            if !keys.contains(&key) {
                return None;
            }
            if key == "LD_LIBRARY_PATH" && text.len() > 180 {
                Some(format!("{key}=<{} chars>", text.len()))
            } else {
                Some(text.into_owned())
            }
        })
        .collect()
}

fn unix_socket_paths() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string("/proc/net/unix") else {
        return map;
    };
    for line in text.lines().skip(1) {
        let mut parts = line.split_whitespace();
        let Some(inode) = parts.nth(6) else {
            continue;
        };
        if let Some(path) = parts.next() {
            map.insert(inode.to_string(), path.to_string());
        }
    }
    map
}

fn reap_children() -> u32 {
    let mut reaped = 0u32;
    loop {
        // SAFETY: WNOHANG on this process's children; the status is discarded and a return of -1
        // or 0 ends the loop.
        let pid = unsafe { waitpid(-1, std::ptr::null_mut(), 1) };
        if pid <= 0 {
            break;
        }
        reaped += 1;
    }
    reaped
}

unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}
