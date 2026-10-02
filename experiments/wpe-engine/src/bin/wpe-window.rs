//! A window with WPE WebKit views through the prototype runtime and the real host: the WPE
//! equivalent of M1's experiment. A chrome strip (a CPU-painted SHM target, painted only when
//! its size changes) sits above the page, which is a native target WebKit's frames are attached
//! to. Every engine event is printed; a report at the end names the transport and counts
//! frames, copies, wakeups and memory.
//!
//! ```sh
//! tools/cargo build -p wpe-engine
//! tools/heavy tools/headless-sway --renderer gles2 -- target/debug/wpe-window --exit-after 10 PAGE...
//! ```
//!
//! Each PAGE (a URL, or a file) opens a view; they are tabs, and only the first is shown.
//! Actions come from `--at SECONDS:ACTION` (repeatable) and standard input, one per line:
//! `hide`, `show`, `reload`, `back`, `forward`, `open=URL`, `eval=SCRIPT` (prints the result),
//! `zoom=FACTOR`, `select=N`, `new=URL`, `close=N`, `focus=chrome` / `focus=page` (where the
//! keyboard goes), `mark` (prints wakeups, diagnostics and memory so far), `quit`. `--shm` forces
//! the shared-memory path. `experiments/wpe-engine/window-checks.sh` drives it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use calloop::channel::{channel, Event};
use calloop::timer::{TimeoutAction, Timer};
use calloop::EventLoop;
use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::{NavigationId, TargetId, ViewId, WindowId};
use sighurt_api::input::{ButtonState, InputKind, PointerEvent};
use sighurt_wayland::{
    Host, HostEvent, InputRegion, Placement, PresentMode, TargetLease, TargetPlacement,
    WindowOptions,
};
use wpe_engine::{WpeConfig, WpeRuntime};

/// The chrome strip's height, in logical pixels.
const CHROME: u32 = 40;

struct Page {
    view: ViewId,
    target: TargetId,
    visible: bool,
    /// When each frame was submitted, for pacing.
    frames: Vec<Instant>,
}

struct State {
    host: Host,
    runtime: WpeRuntime,
    waker: EngineWaker,
    window: WindowId,
    size: Option<LogicalSize>,
    chrome: TargetLease,
    chrome_paints: u64,
    pages: Vec<Page>,
    /// The tab actions apply to, shown or hidden.
    selected: Option<ViewId>,
    /// Who has the keyboard: a page, or the chrome (`None`).
    focus: Option<ViewId>,
    start: Instant,
    running: bool,
    actions: Vec<String>,
    iterations: u64,
    called: bool,
    due: Option<Instant>,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut actions, mut pages, mut config) = (Vec::new(), Vec::new(), WpeConfig::default());
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--exit-after" => actions.extend(args.next().map(|s| format!("{s}:quit"))),
            "--at" => actions.extend(args.next()),
            "--shm" => config.force_shm = true,
            page => pages.push(url_of(page)),
        }
    }
    if pages.is_empty() {
        eprintln!("usage: wpe-window [--shm] [--exit-after S] [--at S:ACTION]... PAGE...");
        std::process::exit(2);
    }
    let mut event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("an event loop");
    let handle = event_loop.handle();
    let mut host = Host::new(&handle, |state: &mut State| &mut state.host).expect("a compositor");
    let (ping, source) = calloop::ping::make_ping().expect("a ping");
    handle
        .insert_source(source, |_, _, _| {})
        .expect("the ping source");
    let waker = EngineWaker::new(move || ping.ping());
    let runtime = WpeRuntime::new(config, waker.clone()).expect("the WPE runtime");
    let window = WindowId::next();
    host.create_window(
        window,
        WindowOptions {
            title: "sighurt wpe".into(),
            app_id: "sighurt-wpe".into(),
            ..WindowOptions::default()
        },
    );
    let chrome = host
        .create_target(window, PresentMode::Shm, None)
        .expect("the window");
    for action in actions {
        let Some((at, what)) = action.split_once(':') else {
            eprintln!("--at {action}: expected SECONDS:ACTION");
            std::process::exit(2);
        };
        let (at, what) = (at.parse::<f64>().unwrap_or(0.0), what.to_string());
        let timer = Timer::from_duration(Duration::from_secs_f64(at));
        handle
            .insert_source(timer, move |_, _, state: &mut State| {
                state.actions.push(what.clone());
                TimeoutAction::Drop
            })
            .expect("a timer");
    }
    let (lines, source) = channel::<String>();
    handle
        .insert_source(source, |event, _, state: &mut State| {
            if let Event::Msg(line) = event {
                state.actions.push(line);
            }
        })
        .expect("the stdin channel");
    std::thread::spawn(move || {
        for line in std::io::stdin().lines().map_while(Result::ok) {
            if lines.send(line.trim().to_string()).is_err() {
                break;
            }
        }
    });
    let mut state = State {
        host,
        runtime,
        waker,
        window,
        size: None,
        chrome,
        chrome_paints: 0,
        pages: Vec::new(),
        selected: None,
        focus: None,
        start: Instant::now(),
        running: true,
        actions: Vec::new(),
        iterations: 0,
        called: false,
        due: None,
    };
    for (index, page) in pages.iter().enumerate() {
        state.open(page, index == 0);
    }
    while state.running {
        let timeout = state
            .due
            .map(|due| due.saturating_duration_since(Instant::now()));
        event_loop
            .dispatch(timeout, &mut state)
            .expect("the event loop");
        state.step();
        let actions = std::mem::take(&mut state.actions);
        for action in &actions {
            println!("[{:8.3}] action {action}", state.secs());
            state.act(action);
        }
        if actions.iter().any(|a| a != "mark") {
            event_loop
                .dispatch(Some(Duration::ZERO), &mut state)
                .expect("the event loop");
            state.step();
        }
    }
    state.report("end");
    let State {
        mut host,
        mut runtime,
        chrome,
        ..
    } = state;
    runtime.shutdown();
    chrome.retire();
    host.flush();
    println!("shut down");
}

fn url_of(page: &str) -> String {
    if page.contains("://") || page.starts_with("data:") || page.starts_with("about:") {
        return page.to_string();
    }
    let path = std::fs::canonicalize(page).unwrap_or_else(|e| panic!("{page}: {e}"));
    format!("file://{}", path.display())
}

impl State {
    fn secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn open(&mut self, url: &str, visible: bool) -> ViewId {
        let view = ViewId::next();
        let lease = self
            .host
            .create_target(self.window, PresentMode::Gl, Some(view))
            .expect("the window");
        let target = lease.id();
        let options = ViewOptions {
            visible,
            ..ViewOptions::default()
        };
        if let Err(error) = self.runtime.open_view(view, options, lease) {
            println!("[{:8.3}] open failed: {error}", self.secs());
            return view;
        }
        self.pages.push(Page {
            view,
            target,
            visible,
            frames: Vec::new(),
        });
        self.called = true;
        self.place();
        self.command(
            view,
            EngineCommand::Navigate {
                navigation: NavigationId::next(),
                url: url.into(),
            },
        );
        if visible {
            self.selected = Some(view);
        }
        if visible && self.focus.is_none() {
            self.set_focus(Some(view));
        }
        view
    }

    fn command(&mut self, view: ViewId, command: EngineCommand) {
        self.called = true;
        if let Err(error) = self.runtime.command(view, command) {
            println!("[{:8.3}] command for {view:?} failed: {error}", self.secs());
        }
    }

    fn set_focus(&mut self, view: Option<ViewId>) {
        if let Some(old) = self.focus.filter(|old| Some(*old) != view) {
            self.command(old, EngineCommand::SetFocus(false));
        }
        self.focus = view;
        let target = match view {
            Some(view) => self.pages.iter().find(|p| p.view == view).map(|p| p.target),
            None => Some(self.chrome.id()),
        };
        self.host.set_keyboard_focus(self.window, target);
        if let Some(view) = view {
            self.command(view, EngineCommand::SetFocus(true));
        }
    }

    fn view_of(&self, target: TargetId) -> Option<ViewId> {
        self.pages
            .iter()
            .find(|p| p.target == target)
            .map(|p| p.view)
    }

    fn nth(&self, argument: &str) -> Option<ViewId> {
        let index = argument.parse::<usize>().ok()?.checked_sub(1)?;
        Some(self.pages.get(index)?.view)
    }

    fn place(&mut self) {
        let Some(size) = self.size else {
            return;
        };
        let page_rect = WindowRect {
            x: 0,
            y: CHROME as i32,
            width: size.width,
            height: size.height.saturating_sub(CHROME),
        };
        let mut targets = vec![TargetPlacement {
            target: self.chrome.id(),
            rect: WindowRect {
                x: 0,
                y: 0,
                width: size.width,
                height: CHROME.min(size.height),
            },
            visible: true,
            input: InputRegion::Whole,
        }];
        targets.extend(self.pages.iter().map(|page| TargetPlacement {
            target: page.target,
            rect: page_rect,
            visible: page.visible,
            input: InputRegion::Whole,
        }));
        let placement = Placement {
            window: self.window,
            size,
            targets,
        };
        if let Err(error) = self.host.apply_placement(placement) {
            println!("[{:8.3}] placement refused: {error}", self.secs());
        }
    }

    fn step(&mut self) {
        self.iterations += 1;
        while let Some(event) = self.host.next_event() {
            self.host_event(event);
        }
        let due = self.due.is_some_and(|due| Instant::now() >= due);
        if std::mem::take(&mut self.called) | self.waker.take() | due {
            self.due = None;
            // A few slices, then input and presentation get their turn (the pump is bounded).
            for _ in 0..8 {
                let mut events = Vec::new();
                let status = self.runtime.pump(&mut events);
                for event in events {
                    self.engine_event(event);
                }
                self.due = match status {
                    PumpStatus::Pending { deadline: None } => Some(Instant::now()),
                    PumpStatus::Pending { deadline } => deadline,
                    PumpStatus::Idle => None,
                };
                if status != (PumpStatus::Pending { deadline: None }) {
                    break;
                }
            }
        }
        for (view, result) in self.runtime.take_script_results() {
            println!("[{:8.3}] script {view:?}: {result:?}", self.secs());
        }
        for permit in self.host.permits() {
            if permit.target() == self.chrome.id() {
                // The chrome repaints only for a new size: never because the page animated.
                if let Ok(mut frame) = permit.shm_frame() {
                    // ARGB8888, little-endian: B, G, R, A.
                    frame
                        .pixels()
                        .as_chunks_mut::<4>()
                        .0
                        .fill([0x60, 0x40, 0x30, 0xff]);
                    frame.submit();
                    self.chrome_paints += 1;
                }
                continue;
            }
            let Some(view) = self.view_of(permit.target()) else {
                continue;
            };
            match self.runtime.present(view, permit) {
                Ok(PresentOutcome::Submitted) => {
                    if let Some(page) = self.pages.iter_mut().find(|p| p.view == view) {
                        page.frames.push(Instant::now());
                    }
                }
                Ok(_) => {}
                Err(error) => println!("[{:8.3}] present failed: {error}", self.secs()),
            }
        }
        self.host.flush();
    }

    fn host_event(&mut self, event: HostEvent) {
        match event {
            HostEvent::WindowConfigured { size, scale, .. } => {
                println!(
                    "[{:8.3}] window {}x{} at {}/120",
                    self.secs(),
                    size.width,
                    size.height,
                    scale.numerator()
                );
                self.size = Some(size);
                self.place();
            }
            HostEvent::CloseRequested { .. } => self.running = false,
            HostEvent::MetricsChanged { target, metrics } => {
                if target == self.chrome.id() {
                    self.host.invalidate(target);
                } else if let Some(view) = self.view_of(target) {
                    println!(
                        "[{:8.3}] {view:?} metrics {}x{} buffer {}x{} at {}/120",
                        self.secs(),
                        metrics.rect().width,
                        metrics.rect().height,
                        metrics.buffer().width,
                        metrics.buffer().height,
                        metrics.scale().numerator()
                    );
                    self.command(view, EngineCommand::SetMetrics(metrics));
                }
            }
            HostEvent::Input { target, event, .. } => {
                let pressed = matches!(
                    event.kind,
                    InputKind::Pointer(PointerEvent::Button {
                        state: ButtonState::Pressed,
                        ..
                    })
                );
                if target == Some(self.chrome.id()) && pressed {
                    self.set_focus(None);
                }
                if let Some(view) = target.and_then(|t| self.view_of(t)) {
                    if pressed && self.focus != Some(view) {
                        self.set_focus(Some(view));
                    }
                    self.command(view, EngineCommand::Input(event));
                }
            }
            HostEvent::ClipboardRead { .. } => {}
            HostEvent::ConnectionLost { error } => {
                println!("[{:8.3}] connection lost: {error}", self.secs());
                self.running = false;
            }
        }
    }

    fn engine_event(&mut self, event: EngineEvent) {
        if !matches!(event, EngineEvent::FrameReady { .. }) {
            println!("[{:8.3}] {event:?}", self.secs());
        }
        let target = event
            .view()
            .and_then(|view| self.pages.iter().find(|p| p.view == view).map(|p| p.target));
        match (&event, target) {
            (EngineEvent::FrameReady { .. }, Some(target)) => self.host.invalidate(target),
            (EngineEvent::CursorChanged { cursor, .. }, Some(target)) => {
                self.host.set_cursor(target, *cursor)
            }
            _ => {}
        }
    }

    fn act(&mut self, action: &str) {
        let (verb, argument) = action.split_once('=').unwrap_or((action, ""));
        match (verb, self.selected) {
            ("quit", _) => self.running = false,
            ("mark", _) => self.report("mark"),
            ("new", _) => {
                self.open(&url_of(argument), false);
            }
            ("select", _) => {
                let Some(view) = self.nth(argument) else {
                    return;
                };
                let changes: Vec<(ViewId, bool)> = self
                    .pages
                    .iter_mut()
                    .filter_map(|p| {
                        let visible = p.view == view;
                        (p.visible != visible).then(|| {
                            p.visible = visible;
                            (p.view, visible)
                        })
                    })
                    .collect();
                for (view, visible) in changes {
                    self.command(view, EngineCommand::SetVisibility(visible));
                }
                self.place();
                self.selected = Some(view);
                self.set_focus(Some(view));
            }
            ("close", _) => {
                if let Some(view) = self.nth(argument) {
                    self.runtime.close_view(view);
                    self.called = true;
                    self.pages.retain(|p| p.view != view);
                    if self.focus == Some(view) {
                        self.focus = None;
                    }
                    if self.selected == Some(view) {
                        self.selected = None;
                    }
                    self.place();
                }
            }
            ("focus", Some(view)) => {
                self.set_focus((argument == "page").then_some(view));
            }
            (_, None) => println!("[{:8.3}] no page is selected", self.secs()),
            (verb, Some(view)) => {
                let command = match verb {
                    "hide" | "show" => {
                        let visible = verb == "show";
                        // Like switching tabs: hidden, the page keeps its view and metrics.
                        if let Some(page) = self.pages.iter_mut().find(|p| p.view == view) {
                            page.visible = visible;
                        }
                        self.place();
                        EngineCommand::SetVisibility(visible)
                    }
                    "reload" => EngineCommand::Reload {
                        navigation: NavigationId::next(),
                    },
                    "back" | "forward" => EngineCommand::Traverse {
                        navigation: NavigationId::next(),
                        delta: if verb == "back" { -1 } else { 1 },
                    },
                    "open" => EngineCommand::Navigate {
                        navigation: NavigationId::next(),
                        url: url_of(argument),
                    },
                    "zoom" => EngineCommand::SetZoom(argument.parse().unwrap_or(1.0)),
                    "eval" => {
                        if let Err(error) = self.runtime.evaluate(view, argument) {
                            println!("[{:8.3}] eval failed: {error}", self.secs());
                        }
                        self.called = true;
                        return;
                    }
                    other => {
                        println!("[{:8.3}] unknown action {other}", self.secs());
                        return;
                    }
                };
                self.command(view, command);
            }
        }
    }

    fn report(&self, label: &str) {
        let d = self.runtime.diagnostics();
        println!(
            "[{:8.3}] {label}: wakeups {} chrome-paints {} transport {:?} device {:?} formats {} \
             glib-dispatches {} stale-frames {} rounded-frames {} failed-imports {}",
            self.secs(),
            self.iterations,
            self.chrome_paints,
            d.transport,
            d.device,
            d.formats,
            d.glib_dispatches,
            d.stale_frames,
            d.rounded_frames,
            d.failed_imports,
        );
        let p = d.present;
        println!(
            "[{:8.3}] {label}: dmabuf-frames {} dmabuf-imports {} shm-copies {} shm-bytes {} \
             dropped {} releases {}",
            self.secs(),
            p.dmabuf_frames,
            p.dmabuf_imports,
            p.shm_copies,
            p.shm_bytes,
            p.dropped,
            p.releases,
        );
        for page in &self.pages {
            println!(
                "[{:8.3}] {label}: {:?} frames {} pacing {}",
                self.secs(),
                page.view,
                page.frames.len(),
                pacing(&page.frames)
            );
        }
        let memory = memory();
        let total: u64 = memory.values().sum();
        let parts: Vec<String> = memory
            .iter()
            .map(|(name, kib)| format!("{name} {kib}"))
            .collect();
        println!(
            "[{:8.3}] {label}: pss-kib {total} ({})",
            self.secs(),
            parts.join(", ")
        );
    }
}

/// Frame intervals in milliseconds: median, 95th percentile and maximum, over the frames that
/// came within a second of the one before (animation, not idle gaps).
fn pacing(frames: &[Instant]) -> String {
    let mut gaps: Vec<f64> = frames
        .windows(2)
        .map(|w| (w[1] - w[0]).as_secs_f64() * 1000.0)
        .filter(|&ms| ms < 1000.0)
        .collect();
    if gaps.is_empty() {
        return "-".into();
    }
    gaps.sort_by(f64::total_cmp);
    let at = |q: f64| gaps[((gaps.len() - 1) as f64 * q).round() as usize];
    format!(
        "n={} median={:.2}ms p95={:.2}ms max={:.2}ms",
        gaps.len(),
        at(0.5),
        at(0.95),
        gaps[gaps.len() - 1]
    )
}

/// Proportional set size of this process and its descendants (WebKit's web, network and
/// sandbox processes), by command name, in KiB.
fn memory() -> HashMap<String, u64> {
    let mut by_name = HashMap::new();
    let mut pids = vec![std::process::id()];
    while let Some(pid) = pids.pop() {
        let name = std::fs::read_to_string(format!("/proc/{pid}/comm")).unwrap_or_default();
        let pss = std::fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("Pss:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
            })
            .unwrap_or(0);
        *by_name.entry(name.trim().to_string()).or_insert(0) += pss;
        let tasks = std::fs::read_dir(format!("/proc/{pid}/task"))
            .into_iter()
            .flatten();
        for task in tasks.flatten() {
            let path = task.path().join("children");
            let children = std::fs::read_to_string(path).unwrap_or_default();
            pids.extend(
                children
                    .split_whitespace()
                    .filter_map(|c| c.parse::<u32>().ok()),
            );
        }
    }
    by_name
}
