//! WPE WebKit beside Servo 0.5 and Wasmtime 36 in one process, mounted as `sig` mounts its
//! engines: the WPE equivalent of M0 (docs/rewrite/WPE.md, "Coexistence").
//!
//! One window in a private compositor, three page targets side by side: a Servo view, a WPE view
//! and a Wasm engine view, each on its engine's runtime behind `EngineRuntime`, on one host and
//! one owner loop. In order, printing PASS, FAIL or INFO per observation:
//!
//! 1. Process setup as `sig` does it (TLS provider, panic hook to Servo's logger), and Wasmtime
//!    guests trapping on the owner thread (out of bounds, `unreachable`).
//! 2. The three runtimes start; signal dispositions are read (never changed) after each.
//! 3. Pages load: in Servo and in WebKit a page runs JavaScript and makes a Wasm module trap in
//!    the page's engine (SpiderMonkey, JavaScriptCore) 100 times each way; the Wasm engine runs a
//!    guest. Every view must present.
//! 4. JavaScript evaluated by WebKit returns its value into this process (JavaScriptCore runs in
//!    the UI process too, to hold the result).
//! 5. Everything at once: both pages trap 2000 more times each while a thread of ours makes a
//!    Wasmtime guest trap in a loop and the owner thread does too.
//! 6. A Wasm guest that traps fails its view only.
//! 7. WebKit's web process is killed: the WPE view fails, the other engines go on, and a reload
//!    brings it back.
//! 8. The shared libraries mapped: none twice under one name.
//! 9. Shutdown in `sig`'s order: every runtime, then the host.
//!
//! ```sh
//! tools/cargo build -p wpe-coexist
//! tools/heavy tools/headless-sway --renderer gles2 -- experiments/wpe-coexist/coexist-checks.sh OUT
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::{NavigationId, TargetId, ViewId, WindowId};
use sighurt_engine_servo::{ServoConfig, ServoRuntime};
use sighurt_engine_wasm::{SandboxPolicy, WasmConfig, WasmRuntime};
use sighurt_wayland::{
    FramePermit, Host, HostEvent, InputRegion, Placement, PresentMode, TargetLease,
    TargetPlacement, WindowOptions,
};
use wpe_engine::{WpeConfig, WpeRuntime};

type Runtime = dyn EngineRuntime<Target = TargetLease, FramePermit = FramePermit>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Kind {
    Servo,
    Wpe,
    Wasm,
}

const KINDS: [Kind; 3] = [Kind::Servo, Kind::Wpe, Kind::Wasm];

/// A Wasm module that traps both ways, for every engine's Wasm.
const TRAPS: &str = r#"(module
  (memory (export "memory") 1 1)
  (func (export "oob") (result i32) (i32.load (i32.const 65536)))
  (func (export "trap") unreachable))"#;

struct Engines {
    servo: ServoRuntime,
    wpe: WpeRuntime,
    wasm: WasmRuntime<TargetLease>,
    wakers: HashMap<Kind, EngineWaker>,
}

impl Engines {
    fn get(&mut self, kind: Kind) -> &mut Runtime {
        match kind {
            Kind::Servo => &mut self.servo,
            Kind::Wpe => &mut self.wpe,
            Kind::Wasm => &mut self.wasm,
        }
    }
}

struct View {
    kind: Kind,
    view: ViewId,
    target: TargetId,
    frames: u64,
    title: Option<String>,
    failed: Option<String>,
}

struct State {
    host: Host,
    engines: Engines,
    window: WindowId,
    size: Option<LogicalSize>,
    views: Vec<View>,
    called: HashMap<Kind, bool>,
    due: HashMap<Kind, Instant>,
    failures: u32,
    start: Instant,
}

fn main() {
    // `--exit`: only load the executable and its libraries (coexist-checks.sh reads the dynamic
    // linker's bindings from such a run).
    if std::env::args().any(|a| a == "--exit") {
        return;
    }
    let mut state = setup();
    let mut event_loop = state.1;
    let state = &mut state.0;
    run(state, &mut event_loop);
    let failures = state.failures;
    println!("{failures} failed");
    std::process::exit(failures.min(100) as i32);
}

fn check(state: &mut State, name: &str, ok: bool, detail: impl std::fmt::Display) {
    let detail = detail.to_string();
    let detail = if detail.is_empty() {
        detail
    } else {
        format!(" ({detail})")
    };
    if ok {
        println!("PASS {name}{detail}");
    } else {
        println!("FAIL {name}{detail}");
        state.failures += 1;
    }
}

/// The disposition of the signals engines care about: handler addresses, by signal name.
fn dispositions() -> String {
    let signals = [
        ("SEGV", libc::SIGSEGV),
        ("BUS", libc::SIGBUS),
        ("ILL", libc::SIGILL),
        ("FPE", libc::SIGFPE),
        ("USR1", libc::SIGUSR1),
        ("USR2", libc::SIGUSR2),
        ("PIPE", libc::SIGPIPE),
        ("CHLD", libc::SIGCHLD),
    ];
    let mut parts = Vec::new();
    for (name, signal) in signals {
        // SAFETY: reads the current action; a null new action changes nothing.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut action) };
        let handler = match action.sa_sigaction {
            libc::SIG_DFL => "default".to_string(),
            libc::SIG_IGN => "ignored".to_string(),
            address => format!("{address:#x}({})", object_of(address)),
        };
        parts.push(format!("{name}={handler}"));
    }
    parts.join(" ")
}

/// The file name of the object `address` is in: the executable or a shared library.
fn object_of(address: usize) -> String {
    // SAFETY: dladdr only reads the dynamic linker's tables.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    if unsafe { libc::dladdr(address as *const libc::c_void, &mut info) } == 0
        || info.dli_fname.is_null()
    {
        return "?".into();
    }
    let path = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy();
    path.rsplit('/').next().unwrap_or_default().to_string()
}

/// Wasmtime guests trapping on this thread: out of bounds and `unreachable`, `rounds` times each.
fn wasmtime_traps(engine: &wasmtime::Engine, module: &wasmtime::Module, rounds: u32) -> (u32, u32) {
    let mut store = wasmtime::Store::new(engine, ());
    let instance = wasmtime::Instance::new(&mut store, module, &[]).expect("an instance");
    let oob = instance
        .get_typed_func::<(), i32>(&mut store, "oob")
        .expect("oob");
    let trap = instance
        .get_typed_func::<(), ()>(&mut store, "trap")
        .expect("trap");
    let (mut oobs, mut unreachables) = (0, 0);
    for _ in 0..rounds {
        let error = oob.call(&mut store, ()).expect_err("out of bounds");
        if error.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::MemoryOutOfBounds) {
            oobs += 1;
        }
        let error = trap.call(&mut store, ()).expect_err("unreachable");
        if error.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::UnreachableCodeReached) {
            unreachables += 1;
        }
    }
    (oobs, unreachables)
}

/// A page that runs JavaScript and makes the module trap `rounds` times each way in its
/// engine's Wasm, reporting in its title.
fn trap_page(label: &str, rounds: u32) -> String {
    let bytes = wat::parse_str(TRAPS).expect("the module");
    let list: Vec<String> = bytes.iter().map(u8::to_string).collect();
    let script = format!(
        "const t0 = performance.now();\
         WebAssembly.instantiate(new Uint8Array([{}])).then(({{instance}}) => {{\
           let oob = 0, unreachable = 0;\
           for (let i = 0; i < {rounds}; i++) {{\
             try {{ instance.exports.oob(); }} catch (e) {{ if (e instanceof WebAssembly.RuntimeError) oob++; }}\
             try {{ instance.exports.trap(); }} catch (e) {{ if (e instanceof WebAssembly.RuntimeError) unreachable++; }}\
           }}\
           document.title = `{label} js=${{6 * 7}} oob=${{oob}}/{rounds} unreachable=${{unreachable}}/{rounds} ms=${{Math.round(performance.now() - t0)}}`;\
         }}, (e) => document.title = `{label} failed ${{e}}`);",
        list.join(",")
    );
    let html = format!(
        "<!DOCTYPE html><meta charset='utf-8'><title>{label}</title><body style='background:#36c;color:#fff;font:40px sans-serif'>\
         <h1>{label} åäö</h1><script>{script}</script>"
    );
    format!("data:text/html,{}", percent(&html))
}

fn percent(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn guest(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("guests")
        .join(name);
    format!("file://{}", path.display())
}

fn setup() -> (State, EventLoop<'static, State>) {
    println!("INFO signals at start: {}", dispositions());
    // What `sig` does before any engine (M0, decision 26).
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        if sighurt_engine_servo::should_panic_hook_suppress_termination() {
            return;
        }
        log::error!("{info}");
    }));

    let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("an event loop");
    let handle = event_loop.handle();
    let mut wakers = HashMap::new();
    for kind in KINDS {
        let (ping, source) = calloop::ping::make_ping().expect("a ping");
        handle
            .insert_source(source, |_, _, _| {})
            .expect("the ping");
        wakers.insert(kind, EngineWaker::new(move || ping.ping()));
    }
    let servo = ServoRuntime::new(ServoConfig::default(), wakers[&Kind::Servo].clone());
    servo.setup_logging();
    println!("INFO signals after Servo: {}", dispositions());
    let config = WasmConfig {
        policy: SandboxPolicy::default(),
        kv_root: std::env::temp_dir().join(format!("wpe-coexist-kv-{}", std::process::id())),
        aot_cache: None,
    };
    let wasm = WasmRuntime::new(config, wakers[&Kind::Wasm].clone()).expect("the Wasm runtime");
    println!("INFO signals after the Wasm engine: {}", dispositions());
    // WPE_COEXIST_JSC_SIGNALS=1 lets JavaScriptCore install its signal handlers here, as WebKit
    // does by default, to reproduce the clash the runtime avoids (WPE.md, "Signals").
    let config = WpeConfig {
        jsc_signal_handlers: std::env::var_os("WPE_COEXIST_JSC_SIGNALS").is_some(),
        ..WpeConfig::default()
    };
    let wpe = WpeRuntime::new(config, wakers[&Kind::Wpe].clone()).expect("WPE");
    let mut host = Host::new(&handle, |state: &mut State| &mut state.host).expect("a compositor");
    let window = WindowId::next();
    host.create_window(
        window,
        WindowOptions {
            title: "sighurt wpe+servo+wasm".into(),
            app_id: "sighurt-coexist".into(),
            ..WindowOptions::default()
        },
    );
    let state = State {
        host,
        engines: Engines {
            servo,
            wpe,
            wasm,
            wakers,
        },
        window,
        size: None,
        views: Vec::new(),
        called: HashMap::new(),
        due: HashMap::new(),
        failures: 0,
        start: Instant::now(),
    };
    (state, event_loop)
}

impl State {
    fn open(&mut self, kind: Kind, url: &str) -> ViewId {
        let view = ViewId::next();
        let mode = match kind {
            Kind::Wasm => PresentMode::Shm,
            Kind::Servo | Kind::Wpe => PresentMode::Gl,
        };
        let lease = self
            .host
            .create_target(self.window, mode, Some(view))
            .expect("the window");
        let target = lease.id();
        self.engines
            .get(kind)
            .open_view(view, ViewOptions::default(), lease)
            .unwrap_or_else(|e| panic!("{kind:?} view: {e}"));
        self.views.push(View {
            kind,
            view,
            target,
            frames: 0,
            title: None,
            failed: None,
        });
        self.place();
        self.navigate(kind, url);
        view
    }

    fn navigate(&mut self, kind: Kind, url: &str) {
        let view = self.view(kind).view;
        let navigation = NavigationId::next();
        self.command(
            kind,
            view,
            EngineCommand::Navigate {
                navigation,
                url: url.into(),
            },
        );
    }

    fn command(&mut self, kind: Kind, view: ViewId, command: EngineCommand) {
        self.called.insert(kind, true);
        if let Err(error) = self.engines.get(kind).command(view, command) {
            println!("INFO {kind:?} command failed: {error}");
        }
    }

    fn view(&mut self, kind: Kind) -> &mut View {
        self.views
            .iter_mut()
            .find(|v| v.kind == kind)
            .expect("a view of that kind")
    }

    fn place(&mut self) {
        let Some(size) = self.size else {
            return;
        };
        let width = size.width / KINDS.len() as u32;
        let targets = self
            .views
            .iter()
            .map(|v| {
                let column = KINDS.iter().position(|k| *k == v.kind).expect("a kind") as u32;
                TargetPlacement {
                    target: v.target,
                    rect: WindowRect {
                        x: (column * width) as i32,
                        y: 0,
                        width,
                        height: size.height,
                    },
                    visible: true,
                    input: InputRegion::Whole,
                }
            })
            .collect();
        let placement = Placement {
            window: self.window,
            size,
            targets,
        };
        if let Err(error) = self.host.apply_placement(placement) {
            println!("INFO placement refused: {error}");
        }
    }

    /// One loop iteration's work after dispatching: host events, pumps, presents.
    fn step(&mut self) {
        while let Some(event) = self.host.next_event() {
            match event {
                HostEvent::WindowConfigured { size, .. } => {
                    self.size = Some(size);
                    self.place();
                }
                HostEvent::MetricsChanged { target, metrics } => {
                    if let Some(v) = self.views.iter().find(|v| v.target == target) {
                        let (kind, view) = (v.kind, v.view);
                        self.command(kind, view, EngineCommand::SetMetrics(metrics));
                    }
                }
                HostEvent::ConnectionLost { error } => panic!("the compositor: {error}"),
                _ => {}
            }
        }
        for kind in KINDS {
            let called = self.called.remove(&kind).unwrap_or(false);
            let woken = self.engines.wakers[&kind].take();
            let due = self.due.get(&kind).is_some_and(|d| Instant::now() >= *d);
            if !(called || woken || due) {
                continue;
            }
            self.due.remove(&kind);
            // A few slices, then the other engines and the host get their turn.
            for _ in 0..8 {
                let mut events = Vec::new();
                let status = self.engines.get(kind).pump(&mut events);
                for event in events {
                    self.engine_event(kind, event);
                }
                match status {
                    PumpStatus::Pending { deadline: None } => {
                        self.due.insert(kind, Instant::now());
                    }
                    PumpStatus::Pending { deadline: Some(at) } => {
                        self.due.insert(kind, at);
                        break;
                    }
                    PumpStatus::Idle => {
                        self.due.remove(&kind);
                        break;
                    }
                }
            }
        }
        for permit in self.host.permits() {
            let Some(v) = self.views.iter().find(|v| v.target == permit.target()) else {
                continue;
            };
            let (kind, view) = (v.kind, v.view);
            if let Ok(PresentOutcome::Submitted) = self.engines.get(kind).present(view, permit) {
                self.view(kind).frames += 1;
            }
        }
        self.host.flush();
    }

    fn engine_event(&mut self, kind: Kind, event: EngineEvent) {
        let Some(v) = self.views.iter_mut().find(|v| Some(v.view) == event.view()) else {
            return;
        };
        match event {
            EngineEvent::FrameReady { .. } => {
                let target = v.target;
                self.host.invalidate(target);
            }
            EngineEvent::TitleChanged { title, .. } => {
                println!(
                    "INFO [{:6.3}] {kind:?} title {title:?}",
                    self.start.elapsed().as_secs_f64()
                );
                v.title = title;
            }
            EngineEvent::ViewFailed { message, .. } => {
                println!(
                    "INFO [{:6.3}] {kind:?} failed: {message}",
                    self.start.elapsed().as_secs_f64()
                );
                v.failed = Some(message);
            }
            _ => {}
        }
    }

    fn title(&self, kind: Kind) -> String {
        let v = self.views.iter().find(|v| v.kind == kind).expect("a view");
        v.title.clone().unwrap_or_default()
    }

    fn frames(&self, kind: Kind) -> u64 {
        self.views
            .iter()
            .find(|v| v.kind == kind)
            .map_or(0, |v| v.frames)
    }

    fn failed(&self, kind: Kind) -> bool {
        self.views
            .iter()
            .any(|v| v.kind == kind && v.failed.is_some())
    }
}

/// Runs the loop until `done` holds or `timeout` passes; returns whether it held.
fn until(
    state: &mut State,
    event_loop: &mut EventLoop<'static, State>,
    timeout: Duration,
    done: impl Fn(&State) -> bool,
) -> bool {
    let end = Instant::now() + timeout;
    while !done(state) {
        let Some(left) = end.checked_duration_since(Instant::now()) else {
            return false;
        };
        let wait = state
            .due
            .values()
            .map(|d| d.saturating_duration_since(Instant::now()))
            .min()
            .unwrap_or(left)
            .min(left)
            .min(Duration::from_millis(50));
        event_loop.dispatch(wait, state).expect("the event loop");
        state.step();
    }
    true
}

fn run(state: &mut State, event_loop: &mut EventLoop<'static, State>) {
    let engine = wasmtime::Engine::default();
    let module = wasmtime::Module::new(&engine, TRAPS).expect("the trap module");
    let (oob, unreachable) = wasmtime_traps(&engine, &module, 100);
    check(
        state,
        "Wasmtime guests trap before WebKit starts",
        (oob, unreachable) == (100, 100),
        format!("oob {oob}/100, unreachable {unreachable}/100"),
    );
    println!("INFO signals after Wasmtime: {}", dispositions());

    // 3. Pages in all three engines.
    let configured = until(state, event_loop, Duration::from_secs(5), |s| {
        s.size.is_some()
    });
    check(state, "the window is configured", configured, "");
    state.open(Kind::Servo, &trap_page("servo", 100));
    state.open(Kind::Wpe, &trap_page("wpe", 100));
    state.open(Kind::Wasm, &guest("green.wat"));
    println!("INFO signals after WebKit's first view: {}", dispositions());
    let want = "js=42 oob=100/100 unreachable=100/100";
    let ok = until(state, event_loop, Duration::from_secs(20), |s| {
        [Kind::Servo, Kind::Wpe]
            .iter()
            .all(|k| s.title(*k).contains(want))
            && KINDS.iter().all(|k| s.frames(*k) > 0)
    });
    let titles = format!(
        "servo: {:?}, wpe: {:?}",
        state.title(Kind::Servo),
        state.title(Kind::Wpe)
    );
    check(
        state,
        "Servo and WebKit run JavaScript and trap in their Wasm",
        ok,
        titles,
    );
    let frames: Vec<String> = KINDS
        .iter()
        .map(|k| format!("{k:?} {}", state.frames(*k)))
        .collect();
    check(
        state,
        "every engine presents",
        KINDS.iter().all(|k| state.frames(*k) > 0),
        frames.join(", "),
    );
    // WPE_COEXIST_HOLD=SECONDS keeps the three views on screen for a screenshot.
    if let Some(hold) = std::env::var("WPE_COEXIST_HOLD")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        println!("INFO holding for {hold} s");
        until(state, event_loop, Duration::from_secs_f64(hold), |_| false);
    }
    let servo = state.engines.servo.diagnostics();
    let wpe = state.engines.wpe.diagnostics();
    println!(
        "INFO Servo GL: {:?}, read_to_image {}; WPE transport {:?} on {:?}, shm copies {}",
        servo.gl.map(|g| g.renderer),
        servo.read_to_image,
        wpe.transport,
        wpe.device,
        wpe.present.shm_copies
    );

    // 4. JavaScriptCore in this process.
    let wpe_view = state.view(Kind::Wpe).view;
    state
        .engines
        .wpe
        .evaluate(
            wpe_view,
            "[6 * 7, navigator.userAgent.includes('WebKit')].join(' ')",
        )
        .expect("evaluate");
    state.called.insert(Kind::Wpe, true);
    let mut result = None;
    let end = Instant::now() + Duration::from_secs(5);
    while result.is_none() && Instant::now() < end {
        until(state, event_loop, Duration::from_millis(100), |_| false);
        result = state
            .engines
            .wpe
            .take_script_results()
            .pop()
            .map(|(_, r)| r);
    }
    check(
        state,
        "WebKit returns a script's value into this process",
        result == Some(Ok("42 true".into())),
        format!("{result:?}"),
    );
    println!(
        "INFO signals after JavaScriptCore in this process: {}",
        dispositions()
    );
    wasmtime_traps(&engine, &module, 1);
    let blocked = blocked_signals();
    check(
        state,
        "a Wasmtime trap leaves this thread's signal mask as it was",
        blocked.is_empty(),
        format!("blocked after the trap: {blocked:?}"),
    );

    // 5. Everything at once.
    let stop = Arc::new(AtomicBool::new(false));
    let rounds = Arc::new(AtomicU64::new(0));
    let thread = {
        let (engine, module, stop, rounds) =
            (engine.clone(), module.clone(), stop.clone(), rounds.clone());
        std::thread::spawn(move || {
            let mut bad = 0;
            while !stop.load(Ordering::Relaxed) {
                let (o, u) = wasmtime_traps(&engine, &module, 10);
                bad += 20 - o - u;
                rounds.fetch_add(10, Ordering::Relaxed);
            }
            bad
        })
    };
    state.navigate(Kind::Servo, &trap_page("servo2", 2000));
    state.navigate(Kind::Wpe, &trap_page("wpe2", 2000));
    let mut owner_bad = 0;
    let want = "js=42 oob=2000/2000 unreachable=2000/2000";
    let ok = until(state, event_loop, Duration::from_secs(30), |s| {
        [Kind::Servo, Kind::Wpe]
            .iter()
            .all(|k| s.title(*k).contains(want))
    });
    for _ in 0..20 {
        let (o, u) = wasmtime_traps(&engine, &module, 10);
        owner_bad += 20 - o - u;
    }
    stop.store(true, Ordering::Relaxed);
    let thread_bad = thread.join().expect("the trap thread");
    let titles = format!(
        "servo: {:?}, wpe: {:?}",
        state.title(Kind::Servo),
        state.title(Kind::Wpe)
    );
    check(
        state,
        "the pages trap 2000 times each way while Wasmtime traps on two threads",
        ok && owner_bad == 0 && thread_bad == 0,
        format!(
            "{titles}; thread rounds {}, wrong traps {thread_bad} + {owner_bad}",
            rounds.load(Ordering::Relaxed)
        ),
    );

    // 6. A trapping Wasm guest.
    state.navigate(Kind::Wasm, &guest("trap.wat"));
    let ok = until(state, event_loop, Duration::from_secs(10), |s| {
        s.failed(Kind::Wasm)
    });
    let others = !state.failed(Kind::Servo) && !state.failed(Kind::Wpe);
    check(
        state,
        "a trapping Wasm guest fails its view only",
        ok && others,
        "",
    );

    // 7. WebKit's web process killed.
    let web = web_processes();
    println!("INFO WebKit web processes: {web:?}");
    for pid in &web {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    let ok = until(state, event_loop, Duration::from_secs(10), |s| {
        s.failed(Kind::Wpe)
    });
    check(
        state,
        "a killed web process fails the WPE view",
        ok && !web.is_empty(),
        "",
    );
    state.navigate(Kind::Servo, &trap_page("servo3", 10));
    let ok = until(state, event_loop, Duration::from_secs(10), |s| {
        s.title(Kind::Servo).contains("servo3 js=42")
    });
    check(
        state,
        "Servo goes on after WebKit's crash",
        ok,
        state.title(Kind::Servo),
    );
    let wpe_view = state.view(Kind::Wpe).view;
    state.view(Kind::Wpe).failed = None;
    state.command(
        Kind::Wpe,
        wpe_view,
        EngineCommand::Reload {
            navigation: NavigationId::next(),
        },
    );
    let ok = until(state, event_loop, Duration::from_secs(15), |s| {
        s.title(Kind::Wpe).starts_with("wpe2 js=42")
    });
    check(
        state,
        "a reload brings the WPE view back",
        ok,
        state.title(Kind::Wpe),
    );

    // 8. Libraries.
    let duplicates = duplicate_libraries();
    check(
        state,
        "no shared library is mapped twice under one name",
        duplicates.is_empty(),
        format!("{duplicates:?}"),
    );

    // 9. Shutdown in sig's order.
    let shutdown = Instant::now();
    state.engines.wpe.shutdown();
    state.engines.wasm.shutdown();
    state.engines.servo.shutdown();
    state.host.flush();
    let leaked = state.engines.servo.diagnostics().leaked;
    println!(
        "INFO shut down in {:?}; Servo left running: {leaked}",
        shutdown.elapsed()
    );
}

/// Which of the synchronous trap signals this thread blocks now.
fn blocked_signals() -> Vec<&'static str> {
    // SAFETY: reads the mask; a null new set changes nothing.
    let mut mask: libc::sigset_t = unsafe { std::mem::zeroed() };
    unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut mask) };
    [
        ("SEGV", libc::SIGSEGV),
        ("BUS", libc::SIGBUS),
        ("ILL", libc::SIGILL),
        ("FPE", libc::SIGFPE),
    ]
    .into_iter()
    .filter(|(_, signal)| unsafe { libc::sigismember(&mask, *signal) } == 1)
    .map(|(name, _)| name)
    .collect()
}

/// WebKit's web processes among this process's descendants.
fn web_processes() -> Vec<i32> {
    let mut found = Vec::new();
    let mut pids = vec![std::process::id() as i32];
    while let Some(pid) = pids.pop() {
        if pid != std::process::id() as i32 {
            let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
            if name.trim() == "WPEWebProcess" {
                found.push(pid);
            }
        }
        let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten();
        for task in tasks.flatten() {
            let children =
                std::fs::read_to_string(task.path().join("children")).unwrap_or_default();
            pids.extend(
                children
                    .split_whitespace()
                    .filter_map(|c| c.parse::<i32>().ok()),
            );
        }
    }
    found
}

/// Shared objects mapped from more than one path under the same file name.
fn duplicate_libraries() -> Vec<String> {
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let mut by_name: HashMap<String, Vec<String>> = HashMap::new();
    for path in maps.lines().filter_map(|l| l.split_whitespace().nth(5)) {
        if !path.contains(".so") {
            continue;
        }
        let name = path.rsplit('/').next().unwrap_or(path).to_string();
        let paths = by_name.entry(name).or_default();
        if !paths.iter().any(|p| p == path) {
            paths.push(path.to_string());
        }
    }
    println!("INFO shared libraries mapped: {}", by_name.len());
    by_name
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .map(|(name, paths)| format!("{name}: {}", paths.join(" ")))
        .collect()
}
