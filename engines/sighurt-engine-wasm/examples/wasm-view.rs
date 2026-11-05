//! One Wasm view in a Wayland window: the engine mounted on the Wayland host the way `sig`
//! mounts it, without the chrome or the core.
//!
//! ```sh
//! tools/cargo build --target wasm32-unknown-unknown --release -p raf-demo
//! tools/cargo build -p sighurt-engine-wasm --example wasm-view
//! tools/headless-sway -- target/debug/examples/wasm-view \
//!     target/wasm32-unknown-unknown/release/raf_demo.wasm --exit-after 5
//! ```
//!
//! The window has one target, an SHM lease covering all of it, that the view presents into.
//! The host's events go to the view (the target's metrics as `SetMetrics`, input on it as
//! `Input`); the view's `FrameReady` invalidates the target, and each permit the host issues
//! goes to `present`. The runtime wakes the loop with a calloop ping; nothing runs on a timer,
//! so a page that doesn't change leaves the loop asleep. On exit it prints how long presenting
//! took on this (the owner) thread, and the host's counters.

use std::time::{Duration, Instant};

use calloop::ping::make_ping;
use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::{NavigationId, ViewId, WindowId};
use sighurt_engine_wasm::{WasmConfig, WasmRuntime};
use sighurt_wayland::{
    Host, HostEvent, InputRegion, Placement, PresentMode, TargetLease, TargetPlacement,
    WindowOptions,
};

struct App {
    host: Host,
    runtime: WasmRuntime<TargetLease>,
    waker: EngineWaker,
    window: WindowId,
    view: ViewId,
    target: sighurt_api::ids::TargetId,
    /// How long each `present` that submitted a frame took.
    presents: Vec<Duration>,
    unchanged: u32,
    not_ready: u32,
    wakeups: u64,
    running: bool,
}

impl App {
    fn handle(&mut self, event: HostEvent) {
        match event {
            HostEvent::WindowConfigured { size, .. } => self.place(size),
            HostEvent::MetricsChanged { target, metrics } if target == self.target => {
                self.command(EngineCommand::SetMetrics(metrics))
            }
            HostEvent::Input {
                target: Some(target),
                event,
                ..
            } if target == self.target => self.command(EngineCommand::Input(event)),
            HostEvent::CloseRequested { .. } => self.running = false,
            HostEvent::ConnectionLost { error } => {
                eprintln!("connection lost: {error}");
                self.running = false;
            }
            _ => {}
        }
    }

    /// The page fills the window.
    fn place(&mut self, size: LogicalSize) {
        let placement = Placement {
            window: self.window,
            size,
            targets: vec![TargetPlacement {
                target: self.target,
                rect: WindowRect {
                    x: 0,
                    y: 0,
                    width: size.width,
                    height: size.height,
                },
                visible: true,
                input: InputRegion::Whole,
            }],
        };
        if let Err(error) = self.host.apply_placement(placement) {
            eprintln!("placement refused: {error}");
        }
    }

    fn command(&mut self, command: EngineCommand) {
        if let Err(error) = self.runtime.command(self.view, command) {
            eprintln!("command refused: {error:?}");
        }
    }

    /// Pumps the runtime and acts on what it reports. Returns whether more is pending.
    fn pump(&mut self) -> bool {
        self.waker.take();
        let mut events = Vec::new();
        let status = self.runtime.pump(&mut events);
        for event in events {
            match event {
                EngineEvent::FrameReady { .. } => self.host.invalidate(self.target),
                EngineEvent::CursorChanged { cursor, .. } => {
                    self.host.set_cursor(self.target, cursor)
                }
                EngineEvent::TitleChanged { title, .. } => println!("title: {title:?}"),
                EngineEvent::NavigationCommitted { url, .. } => println!("committed {url}"),
                EngineEvent::NavigationFailed { error, .. } => eprintln!("load failed: {error}"),
                EngineEvent::ViewFailed { message, .. } => eprintln!("view failed: {message}"),
                EngineEvent::RuntimeFailed { message } => {
                    eprintln!("runtime failed: {message}");
                    self.running = false;
                }
                EngineEvent::OpenRequested { url, .. } => println!("the page asks to open {url}"),
                _ => {}
            }
        }
        matches!(status, PumpStatus::Pending { .. })
    }

    fn present(&mut self) {
        for permit in self.host.permits() {
            if permit.target() != self.target {
                continue;
            }
            let started = Instant::now();
            match self.runtime.present(self.view, permit) {
                Ok(PresentOutcome::Submitted) => self.presents.push(started.elapsed()),
                Ok(PresentOutcome::Unchanged) => self.unchanged += 1,
                Ok(PresentOutcome::NotReady) => self.not_ready += 1,
                Err(error) => eprintln!("present failed: {error:?}"),
            }
        }
    }

    fn report(&mut self) {
        self.presents.sort();
        let n = self.presents.len();
        if n > 0 {
            println!(
                "{n} frames submitted; present took median {:?}, 95th percentile {:?}, slowest {:?}",
                self.presents[n / 2],
                self.presents[(n * 95 / 100).min(n - 1)],
                self.presents[n - 1],
            );
        } else {
            println!("no frame submitted");
        }
        println!(
            "{} unchanged, {} not ready, {} loop wakeups",
            self.unchanged, self.not_ready, self.wakeups
        );
        for target in self.host.diagnostics().targets {
            let size = target.metrics.buffer();
            println!(
                "page: buffer {}x{}, {:?}, {:?}",
                size.width, size.height, target.stats, target.buffers
            );
        }
    }
}

/// `arg` as a URL: a URL as it is, anything else as a path to a local `.wasm` file.
fn url_of(arg: &str) -> String {
    if arg.contains("://") {
        return arg.to_string();
    }
    let path = std::fs::canonicalize(arg).unwrap_or_else(|e| panic!("{arg}: {e}"));
    url::Url::from_file_path(path).unwrap().to_string()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exit_after = args
        .iter()
        .position(|a| a == "--exit-after")
        .and_then(|i| args.get(i + 1)?.parse::<f64>().ok());
    let Some(page) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("usage: wasm-view <app.wasm or URL> [--exit-after SECONDS]");
        std::process::exit(2);
    };
    let url = url_of(page);

    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new().expect("an event loop");
    let handle = event_loop.handle();
    let mut host = Host::new(&handle, |app: &mut App| &mut app.host).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(1);
    });
    let window = WindowId::next();
    host.create_window(
        window,
        WindowOptions {
            title: "sighurt wasm view".into(),
            app_id: "sighurt-wasm-view".into(),
            ..WindowOptions::default()
        },
    );
    let lease = host
        .create_target(window, PresentMode::Shm, None)
        .expect("the window exists");
    let target = lease.id();
    host.set_keyboard_focus(window, Some(target));

    // Worker threads wake the loop through this ping; the loop pumps on every iteration anyway.
    let (ping, ping_source) = make_ping().expect("a ping");
    handle
        .insert_source(ping_source, |_, _, _| {})
        .expect("the ping source");
    let waker = EngineWaker::new(move || ping.ping());
    let kv = tempfile::tempdir().expect("a directory for the key-value store");
    let config = WasmConfig {
        policy: Default::default(),
        kv_root: kv.path().to_path_buf(),
        aot_cache: None,
        pages: Vec::new(),
    };
    let mut runtime = WasmRuntime::new(config, waker.clone()).expect("the Wasm runtime");
    let view = ViewId::next();
    runtime
        .open_view(view, ViewOptions::default(), lease)
        .expect("a view");
    let navigation = NavigationId::next();
    runtime
        .command(view, EngineCommand::Navigate { navigation, url })
        .expect("a navigation");
    runtime
        .command(view, EngineCommand::SetFocus(true))
        .expect("focus");

    if let Some(seconds) = exit_after {
        let timer = Timer::from_duration(Duration::from_secs_f64(seconds));
        handle
            .insert_source(timer, |_, _, app: &mut App| {
                app.running = false;
                TimeoutAction::Drop
            })
            .expect("a timer");
    }
    let mut app = App {
        host,
        runtime,
        waker,
        window,
        view,
        target,
        presents: Vec::new(),
        unchanged: 0,
        not_ready: 0,
        wakeups: 0,
        running: true,
    };
    let signal = event_loop.get_signal();
    let again = event_loop.get_signal();
    event_loop
        .run(None, &mut app, |app| {
            app.wakeups += 1;
            while let Some(event) = app.host.next_event() {
                app.handle(event);
            }
            let pending = app.pump();
            app.present();
            app.host.flush();
            if !app.running {
                signal.stop();
            } else if pending {
                // More finished work than one pump applies: come round again at once.
                again.wakeup();
            }
        })
        .expect("the event loop");
    app.report();
    // The runtime lets go of its views' leases before the host goes.
    app.runtime.shutdown();
    drop(app);
}
