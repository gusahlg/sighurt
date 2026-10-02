//! Milestone M0: can Servo 0.5 and Wasmtime 36 live in one process? (`sighurt-rewrite-plan.md`
//! §23 M0, §27; results in `docs/rewrite/M0.md`.)
//!
//! `m0-coexist wasmtime-first` and `m0-coexist servo-first` each run the whole experiment in
//! this process, starting the engines in that order: both install SIGSEGV and SIGILL handlers,
//! and the one installed last must pass on the other's faults. Servo initializes once per
//! process, so each order needs its own process; without an argument the program runs both as
//! child processes and reports how they exited, which also catches a crash at exit.
//!
//! Prints one `PASS`, `FAIL` or `INFO` line per observation and exits non-zero on a failure.

mod guests;
mod process;
mod wasm_engine;
mod web;

use std::fmt::Display;
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use guests::{Guest, Wasm, FRAME_FUEL_LIMIT};
use servo::{JSValue, ServoBuilder};
use sighurt_api::engine::EngineRuntime;
use url::Url;
use web::Web;

/// Time allowed for pages to load and scripts to run: generous, for a loaded build machine.
const TIMEOUT: Duration = Duration::from_secs(60);
/// Time allowed for one child process.
const CHILD_TIMEOUT: Duration = Duration::from_secs(300);
/// Wasm traps each page makes while loading, and again while Wasmtime guests trap alongside.
const LOAD_ROUNDS: u32 = 100;
const CONCURRENT_ROUNDS: u32 = 2000;

#[derive(Clone, Copy, PartialEq)]
enum Order {
    WasmtimeFirst,
    ServoFirst,
}

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        None => run_both_orders(),
        Some("wasmtime-first") => run(Order::WasmtimeFirst),
        Some("servo-first") => run(Order::ServoFirst),
        Some(_) => {
            eprintln!("usage: m0-coexist [wasmtime-first | servo-first]");
            ExitCode::from(2)
        }
    }
}

/// Runs each order in a child process of its own.
fn run_both_orders() -> ExitCode {
    let exe = std::env::current_exe().expect("own executable");
    let mut failed = false;
    let mut summary = Vec::new();
    for order in ["wasmtime-first", "servo-first"] {
        println!("==== m0-coexist {order}");
        let outcome = run_child(&exe, order);
        failed |= outcome.is_err();
        summary.push(match outcome {
            Ok(()) => format!("PASS {order}: every check passed and the process exited with 0"),
            Err(e) => format!("FAIL {order}: {e}"),
        });
    }
    println!("==== summary");
    summary.iter().for_each(|line| println!("{line}"));
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn run_child(exe: &Path, order: &str) -> Result<(), String> {
    let mut child = Command::new(exe)
        .arg(order)
        .spawn()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok(()),
            Some(status) => return Err(format!("exited with {status}")),
            None if started.elapsed() > CHILD_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("still running after {CHILD_TIMEOUT:?}; killed"));
            }
            None => thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// What the run found.
#[derive(Default)]
struct Report {
    failures: u32,
}

impl Report {
    fn check(&mut self, what: &str, result: Result<impl Display, impl Display>) {
        match result {
            Ok(detail) => println!("PASS {what}: {detail}"),
            Err(problem) => {
                println!("FAIL {what}: {problem}");
                self.failures += 1;
            }
        }
    }

    fn fail(&mut self, what: &str, problem: impl Display) {
        self.check(what, Err::<&str, _>(problem));
    }

    fn info(&self, what: &str, detail: impl Display) {
        println!("INFO {what}: {detail}");
    }
}

fn run(order: Order) -> ExitCode {
    let mut report = Report::default();
    let style_panics = Arc::new(AtomicUsize::new(0));
    process::install_panic_hook(style_panics.clone());
    report.check("tls provider", process::install_tls_provider());
    report.check("wasm engine libraries", process::wasm_engine_libraries());
    report.info("resources at start", process::resources());
    let handlers_at_start = process::trap_handlers();

    let profile = std::env::temp_dir().join(format!("m0-coexist-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&profile);
    let wasm_dir = std::env::temp_dir().join(format!("m0-coexist-wasm-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&wasm_dir);

    let mut wasm = None;
    let mut handlers_after_wasmtime = [0; 2];
    if order == Order::WasmtimeFirst {
        wasm = start_wasmtime(&mut report, &wasm_dir);
        handlers_after_wasmtime = process::trap_handlers();
    }

    let started = Instant::now();
    let mut web = Web::new(profile.clone());
    // The one `log` logger of the process; M0.md, "Logging", says why it is Servo's.
    web.servo.setup_logging();
    report.info("servo built", format!("in {:.1} ms", ms(started)));

    // Two views, whose pages run JavaScript and JavaScript Wasm that traps. Guests trap in
    // Wasmtime on this thread in between, if it has started.
    let guest_bytes = wat::parse_str(guests::GUEST_WAT).expect("the guest is valid");
    let software_gl_before = std::env::var("LIBGL_ALWAYS_SOFTWARE");
    for view in 0..2 {
        if let Err(e) = web.open(test_page(view, &guest_bytes)) {
            report.fail("software rendering context", e);
            return finish(report);
        }
    }
    report.info(
        "environment",
        format!(
            "LIBGL_ALWAYS_SOFTWARE was {software_gl_before:?} before the rendering contexts, \
             {:?} after (surfman sets it)",
            std::env::var("LIBGL_ALWAYS_SOFTWARE")
        ),
    );
    for (i, view) in web.views.iter().enumerate() {
        report.info(
            &format!("view {i} renderer"),
            web::gl_renderer(&*view.context),
        );
    }
    let mut owner_rounds = 0;
    let loaded = web.spin_until(
        TIMEOUT,
        || {
            if let Some((_, guests, _)) = &mut wasm {
                for guest in guests.iter_mut() {
                    if guest.trap_round().is_ok() {
                        owner_rounds += 1;
                    }
                }
            }
        },
        |web| (0..2).all(|view| title(web, view).starts_with("m0 ")),
    );
    for view in 0..2 {
        report.check(
            &format!("view {view} ran JavaScript and trapping Wasm"),
            expect_probe(&title(&web, view), view, LOAD_ROUNDS),
        );
    }
    if !loaded {
        report.info("load", format!("gave up after {TIMEOUT:?}"));
    }
    report.info(
        "pages loaded",
        format!(
            "in {:.1} ms; frames painted {:?}; guest trap rounds on this thread meanwhile: {}",
            ms(started),
            web.views
                .iter()
                .map(|view| view.state.frames.get())
                .collect::<Vec<_>>(),
            if wasm.is_some() {
                owner_rounds.to_string()
            } else {
                "none, not started".into()
            },
        ),
    );
    let handlers_after_servo = process::trap_handlers();

    if order == Order::ServoFirst {
        wasm = start_wasmtime(&mut report, &wasm_dir);
        handlers_after_wasmtime = process::trap_handlers();
    }
    report.check(
        "both runtimes installed trap handlers, in this order",
        handler_order(
            order,
            handlers_at_start,
            handlers_after_wasmtime,
            handlers_after_servo,
        ),
    );
    let Some((wasm, mut guests, mut runtime)) = wasm else {
        return finish(report);
    };

    concurrent_traps(&mut report, &web, &wasm, &mut guests);
    panics_elsewhere(&mut report, &web, &style_panics);

    for (i, view) in web.views.iter().enumerate() {
        if let Some(reason) = view.state.crashed.borrow().as_ref() {
            report.fail(&format!("view {i}"), format!("crashed: {reason}"));
        }
    }
    script_thread_panic(&mut report, &web);
    // Closing one view leaves the other working.
    drop(web.views.remove(0));
    report.check(
        "view 1 after view 0 closed",
        web.evaluate(0, "probe(10)", TIMEOUT)
            .and_then(|value| expect_probe(&js_string(value), 1, 10)),
    );
    if let Some(error) = web.failed() {
        report.fail("servo runtime", error);
    }
    report.info("resources with both engines", process::resources());

    shut_down(&mut report, web, &profile);
    runtime.shutdown();
    drop((runtime, guests, wasm));
    report.info(
        "wasmtime",
        "the Wasm engine's runtime shut down; stores and engines dropped",
    );
    let _ = std::fs::remove_dir_all(&profile);
    let _ = std::fs::remove_dir_all(&wasm_dir);

    report.info(
        "second servo initialization",
        match panic::catch_unwind(AssertUnwindSafe(|| drop(ServoBuilder::default().build()))) {
            Ok(()) => "succeeded".to_string(),
            Err(payload) => {
                let message = panic_message(&*payload);
                let message = message
                    .split_once(": Opts")
                    .map_or(&*message, |(start, _)| start);
                format!("panicked: {message}")
            }
        },
    );
    report.info("resources at the end", process::resources());
    finish(report)
}

/// Creates the engines and two guests, and makes both trap once; then starts the Wasm engine's
/// runtime, which opens a guest that connects over TLS, in `dir`.
fn start_wasmtime(
    report: &mut Report,
    dir: &Path,
) -> Option<(Wasm, [Guest; 2], wasm_engine::Runtime)> {
    let started = Instant::now();
    let result = Wasm::new().and_then(|wasm| {
        let guests = [
            Guest::new(&wasm.engine, &wasm.module, 1)?,
            Guest::new(&wasm.engine, &wasm.module, 2)?,
        ];
        Ok((wasm, guests))
    });
    let (wasm, mut guests) = match result {
        Ok(engines) => engines,
        Err(e) => {
            report.fail("wasmtime", format!("{e:#}"));
            return None;
        }
    };
    report.info("wasmtime started", format!("in {:.1} ms", ms(started)));
    for (i, guest) in guests.iter_mut().enumerate() {
        report.check(
            &format!("guest {i} traps out of bounds and on unreachable, keeps its memory"),
            guest
                .trap_round()
                .map(|()| "MemoryOutOfBounds, UnreachableCodeReached"),
        );
    }
    report.info("threads after wasmtime compiled", process::thread_names());
    let mut runtime = match wasm_engine::start(dir) {
        Ok(runtime) => runtime,
        Err(e) => {
            report.fail("wasm engine runtime", e);
            return None;
        }
    };
    openssl(report, &mut runtime, dir);
    Some((wasm, guests, runtime))
}

/// A guest of the Wasm engine connects to `wss://`, which initializes OpenSSL (through
/// tokio-tungstenite's `native-tls`): it must change no signal handler and none of the
/// variables OpenSSL reads.
fn openssl(report: &mut Report, runtime: &mut wasm_engine::Runtime, dir: &Path) {
    let signals = [
        libc::SIGSEGV,
        libc::SIGILL,
        libc::SIGFPE,
        libc::SIGBUS,
        libc::SIGPIPE,
    ];
    let variables = ["SSL_CERT_FILE", "SSL_CERT_DIR", "OPENSSL_CONF"];
    let state = || {
        (
            signals.map(process::handler),
            variables.map(std::env::var_os),
        )
    };
    let before = state();
    let result = wasm_engine::open_tls_guest(runtime, dir, TIMEOUT);
    let after = state();
    let library = process::mapped_library("libssl").unwrap_or_else(|| "libssl not mapped".into());
    report.check(
        "wasm engine: a guest's wss:// connection initializes OpenSSL",
        result.and_then(|detail| {
            if before == after {
                Ok(format!(
                    "{detail}; {library}; no signal handler and no SSL_CERT_FILE, \
                     SSL_CERT_DIR or OPENSSL_CONF changed"
                ))
            } else {
                Err(format!(
                    "signal handlers or environment changed: {before:?} → {after:?}"
                ))
            }
        }),
    );
}

/// Pages trap in SpiderMonkey on Servo's script threads while this thread stops guests by fuel
/// and by epoch and another thread makes a guest trap from before the scripts are sent until
/// after their results arrive.
fn concurrent_traps(report: &mut Report, web: &Web, wasm: &Wasm, guests: &mut [Guest; 2]) {
    let (stop, background) = wasm.trap_in_background();
    let started = unix_ms();
    let results: Vec<web::Evaluation> = (0..2)
        .map(|view| {
            let result = web::Evaluation::default();
            web.start_evaluation(view, &format!("probe({CONCURRENT_ROUNDS})"), result.clone());
            result
        })
        .collect();
    for (i, guest) in guests.iter_mut().enumerate() {
        report.check(
            &format!("guest {i} stopped by fuel while pages run"),
            guest.stop_by_fuel().map(|ran| {
                format!(
                    "OutOfFuel after {:.1} ms ({FRAME_FUEL_LIMIT} fuel)",
                    ran.as_secs_f64() * 1e3
                )
            }),
        );
    }
    report.check(
        "guest stopped by epoch while pages run",
        wasm.stop_by_epoch()
            .map(|ran| format!("Interrupt after {:.1} ms", ran.as_secs_f64() * 1e3)),
    );
    let owner_ran = (started, unix_ms());
    web.spin_until(
        TIMEOUT,
        || {},
        |_| results.iter().all(|result| result.borrow().is_some()),
    );
    stop.store(true, Ordering::Relaxed);
    let mut timeline = vec![format!(
        "this thread ran guests {}",
        span(owner_ran, started)
    )];
    for (view, result) in results.iter().enumerate() {
        let result = web::finish_evaluation(result).map(js_string);
        if let Some(ran) = result.as_deref().ok().and_then(probe_span) {
            timeline.push(format!("view {view} trapped {}", span(ran, started)));
        }
        report.check(
            &format!("view {view} Wasm traps alongside Wasmtime's"),
            result.and_then(|result| expect_probe(&result, view, CONCURRENT_ROUNDS)),
        );
    }
    report.check(
        "a guest trapped on another thread throughout",
        background
            .join()
            .unwrap_or_else(|_| Err("the guest thread panicked".into()))
            .map(|rounds| format!("{rounds} rounds")),
    );
    report.info(
        "timeline, ms after the scripts were sent",
        timeline.join("; "),
    );
}

/// Milliseconds since the Unix epoch, the clock of JavaScript's `Date.now()`.
fn unix_ms() -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    now.as_millis() as u64
}

/// The `ran=<from>-<to>` a probe reports.
fn probe_span(result: &str) -> Option<(u64, u64)> {
    let ran = result
        .split(' ')
        .find_map(|field| field.strip_prefix("ran="))?;
    let (from, to) = ran.split_once('-')?;
    Some((from.parse().ok()?, to.parse().ok()?))
}

fn span((from, to): (u64, u64), origin: u64) -> String {
    format!(
        "{}–{}",
        from.saturating_sub(origin),
        to.saturating_sub(origin)
    )
}

/// Panics on other threads, a style thread's included, leave Servo working.
fn panics_elsewhere(report: &mut Report, web: &Web, style_panics: &AtomicUsize) {
    for name in ["StyleThread#m0", "wasm-worker"] {
        let thread = thread::Builder::new().name(name.into());
        let _ = thread
            .spawn(|| panic!("m0: a deliberate panic"))
            .map(|t| t.join());
    }
    report.check(
        "panics on other threads",
        match (
            style_panics.load(Ordering::SeqCst),
            web.evaluate(1, "document.title.length", TIMEOUT),
        ) {
            (1, Ok(JSValue::Number(_))) => {
                Ok("the hook counted the style thread's; Servo still evaluates JavaScript")
            }
            (count, result) => Err(format!("{count} style-thread panics counted; {result:?}")),
        },
    );
}

/// A panic on a page's script thread: Servo fails every view with a document on that thread
/// (`notify_crashed`, then a crash page), and views on other script threads go on. The pages
/// are `data:` URLs, which have no site, so each view has a script thread of its own.
fn script_thread_panic(report: &mut Report, web: &Web) {
    // Panics on view 0's script thread; its result never arrives.
    web.start_evaluation(0, "ServoTestUtils.panic()", web::Evaluation::default());
    web.spin_until(
        TIMEOUT,
        || {},
        |web| web.views[0].state.crashed.borrow().is_some(),
    );
    let crashed = |view: usize| web.views[view].state.crashed.borrow().clone();
    let probe = web
        .evaluate(1, "probe(10)", TIMEOUT)
        .and_then(|value| expect_probe(&js_string(value), 1, 10));
    report.check(
        "a panic on view 0's script thread",
        match (crashed(0), crashed(1), probe) {
            (Some(reason), None, Ok(_)) => Ok(format!(
                "view 0 crashed ({:?}); view 1, on another script thread, didn't and still runs \
                 its script",
                reason.lines().last().unwrap_or_default()
            )),
            (view0, view1, probe) => Err(format!(
                "view 0 crashed: {view0:?}; view 1 crashed: {view1:?}; view 1's probe: {probe:?}"
            )),
        },
    );
}

/// Closes the remaining views and Servo, and checks Servo wrote its profile on the way out.
fn shut_down(report: &mut Report, web: Web, profile: &Path) {
    let Web { servo, views, .. } = web;
    let contexts: Vec<_> = views.iter().map(|view| view.context.clone()).collect();
    drop(views);
    let started = Instant::now();
    // The last handle: `Drop for ServoInner` asks the constellation to exit, spins until it has,
    // then shuts SpiderMonkey down.
    drop(servo);
    let took = ms(started);
    drop(contexts);
    report.check(
        "servo shut down",
        if profile.join("cookie_jar.json").is_file() {
            Ok(format!(
                "in {took:.1} ms; the profile was written on the way"
            ))
        } else {
            Err(format!(
                "after {took:.1} ms, but cookie_jar.json was not written"
            ))
        },
    );
    report.info("resources after servo shut down", process::resources());
    report.info(
        "threads left after servo shut down",
        process::thread_names(),
    );
}

fn finish(report: Report) -> ExitCode {
    if report.failures == 0 {
        println!("RESULT pass");
        ExitCode::SUCCESS
    } else {
        println!("RESULT {} failed", report.failures);
        ExitCode::FAILURE
    }
}

/// Which handler each step left in place, compared with the step before.
fn handler_order(
    order: Order,
    start: [usize; 2],
    after_wasmtime: [usize; 2],
    after_servo: [usize; 2],
) -> Result<String, String> {
    let steps = match order {
        Order::WasmtimeFirst => [
            ("Wasmtime", start, after_wasmtime),
            ("SpiderMonkey", after_wasmtime, after_servo),
        ],
        Order::ServoFirst => [
            ("SpiderMonkey", start, after_servo),
            ("Wasmtime", after_servo, after_wasmtime),
        ],
    };
    let describe =
        |handlers: [usize; 2]| format!("SIGSEGV {:#x}, SIGILL {:#x}", handlers[0], handlers[1]);
    let text = steps
        .iter()
        .map(|(who, _, after)| format!("after {who}: {}", describe(*after)))
        .collect::<Vec<_>>()
        .join("; ");
    if steps
        .iter()
        .all(|(_, before, after)| before[0] != after[0] && before[1] != after[1])
    {
        Ok(text)
    } else {
        Err(format!(
            "a step didn't replace both handlers: start {}; {text}",
            describe(start)
        ))
    }
}

/// A page whose script defines `probe(rounds)` and puts `probe(100)` in its title. `probe`
/// instantiates the guest in SpiderMonkey, makes it trap out of bounds and on `unreachable`
/// `rounds` times each, catching the `RuntimeError`s, and reports the counts and when it ran.
fn test_page(view: usize, guest: &[u8]) -> Url {
    let bytes = guest
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let html = format!(
        r#"<!doctype html><title>loading</title><script>
const MODULE = new WebAssembly.Module(new Uint8Array([{bytes}]));
function probe(rounds) {{
  const memory = new WebAssembly.Memory({{initial: 1, maximum: 4096}});
  const guest = new WebAssembly.Instance(MODULE, {{oxide: {{memory}}}}).exports;
  guest.store({view});
  const started = Date.now();
  let oob = 0, unreachable = 0, last = "";
  const traps = (f) => {{
    try {{ f(); }} catch (e) {{
      last = String(e);
      return e instanceof WebAssembly.RuntimeError;
    }}
    return false;
  }};
  for (let i = 0; i < rounds; i++) {{
    if (traps(guest.oob)) oob++;
    if (traps(guest.trap)) unreachable++;
  }}
  const kept = guest.load() === {view};
  return `m0 view={view} oob=${{oob}}/${{rounds}} unreachable=${{unreachable}}/${{rounds}} ` +
    `kept=${{kept}} ran=${{started}}-${{Date.now()}} last=${{last}}`;
}}
document.title = probe({LOAD_ROUNDS});
</script>"#
    );
    let encoded: String = html
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => char::from(b).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    Url::parse(&format!("data:text/html,{encoded}")).expect("a valid data URL")
}

fn expect_probe(result: &str, view: usize, rounds: u32) -> Result<String, String> {
    let expected =
        format!("m0 view={view} oob={rounds}/{rounds} unreachable={rounds}/{rounds} kept=true ");
    if result.starts_with(&expected) {
        Ok(result.to_string())
    } else {
        Err(format!("expected {expected:?}…, got {result:?}"))
    }
}

fn title(web: &Web, view: usize) -> String {
    web.views[view]
        .state
        .title
        .borrow()
        .clone()
        .unwrap_or_default()
}

fn js_string(value: JSValue) -> String {
    match value {
        JSValue::String(text) => text,
        other => format!("{other:?}"),
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "(not a string)".into())
}

fn ms(since: Instant) -> f64 {
    since.elapsed().as_secs_f64() * 1e3
}
