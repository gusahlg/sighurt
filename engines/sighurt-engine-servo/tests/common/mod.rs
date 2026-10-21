//! A small owner loop for the Servo adapter's tests and example, playing the executable: one
//! window, one GL page target per view, the host's input and metrics routed to the views,
//! frames presented on the host's permits, the engine's service requests answered (clipboard
//! through the host, everything else refused) and the page's navigations allowed, unless a test
//! answers them itself ([`State::navigations`]). Every engine event is kept, with the time since
//! the start, and printed.
//!
//! It does what `sig` will do for Servo (decision 26): installs rustls's aws-lc-rs provider and
//! a panic hook that hands panics to Servo's logger, builds the runtime and installs its logger.

#![allow(dead_code)] // Each test binary uses part of it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::{NavigationId, RequestId, TargetId, ViewId, WindowId};
use sighurt_api::services::{Service, ServiceError, ServiceReply};
use sighurt_engine_servo::{ServoConfig, ServoRuntime};
use sighurt_wayland::{
    Host, HostEvent, InputRegion, Placement, PresentMode, TargetPlacement, WindowOptions,
};

/// How the views share the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Each view covers the window; hidden ones are unmapped (tabs).
    Stack,
    /// Side by side, all visible at once.
    Split,
}

pub struct Page {
    pub view: ViewId,
    pub target: TargetId,
    pub visible: bool,
}

pub struct State {
    pub host: Host,
    pub runtime: ServoRuntime,
    pub waker: EngineWaker,
    pub window: WindowId,
    pub size: Option<LogicalSize>,
    pub layout: Layout,
    pub pages: Vec<Page>,
    /// The view with the keyboard.
    pub focus: Option<ViewId>,
    /// Every engine event so far, with the time since the start.
    pub events: Vec<(Duration, EngineEvent)>,
    /// Every present call's outcome.
    pub presents: Vec<(ViewId, PresentOutcome)>,
    pub start: Instant,
    pub running: bool,
    /// Scripted actions that came due (the example's), for the caller to carry out.
    pub actions: Vec<String>,
    /// Loop iterations so far: wakeups, to show an idle loop sleeps.
    pub iterations: u64,
    /// How the loop answers the page's navigation requests: `Some(allow)` at once, as a browser
    /// whose routing keeps them in Servo would; `None` not at all, for the test to answer.
    pub navigations: Option<bool>,
    /// Clipboard reads in flight, for the view that asked.
    reads: HashMap<RequestId, ViewId>,
    /// The runtime was called since the last pump.
    called: bool,
    /// When the runtime asked to be pumped again.
    due: Option<Instant>,
}

pub struct Driver {
    pub event_loop: EventLoop<'static, State>,
    pub state: State,
}

/// How long `/slow/...` pages of [`serve`] take to answer.
pub const SLOW: Duration = Duration::from_secs(3);

/// The path of every request [`serve`] got, in order.
static SERVED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Whether [`serve`] got a request for `path`.
pub fn served(path: &str) -> bool {
    SERVED.lock().unwrap().iter().any(|p| p == path)
}

/// Starts an HTTP server on 127.0.0.1 that runs until the process ends, and returns its
/// address (`http://127.0.0.1:PORT`). `/204` answers "204 No Content"; anything else is a page
/// titled with its path, which `/slow/...` sends after [`SLOW`]. A query makes the page act
/// 300 ms after it loads: `?go=PATH` navigates it there, `?back` calls `history.back()`,
/// `?open=PATH` opens PATH in a new browsing context; `?img=PATH` gives it an image from PATH,
/// `?link=PATH` a link to PATH that covers the page, and `?frames=N` N frames that each go to
/// `/landedI.html` when they have loaded, the page's title counting those that have (`landed
/// 3`). `/held` never answers, so a page with `?img=/held` keeps loading. [`served`] says what
/// was asked for.
pub fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("a local port");
    let base = format!("http://{}", listener.local_addr().expect("its address"));
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer(stream));
        }
    });
    base
}

fn answer(mut stream: TcpStream) {
    let mut reader = BufReader::new(&stream);
    let mut request = String::new();
    if reader.read_line(&mut request).is_err() {
        return;
    }
    let mut header = String::new();
    while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
        header.clear();
    }
    let target = request.split_whitespace().nth(1).unwrap_or("/");
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    SERVED.lock().unwrap().push(path.to_owned());
    if target == "/204" {
        let _ = stream.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n");
        return;
    }
    if target == "/held" {
        std::thread::sleep(Duration::from_secs(600));
        return;
    }
    if target.starts_with("/slow/") {
        std::thread::sleep(SLOW);
    }
    let (act, extra) = match query.split_once('=').unwrap_or((query, "")) {
        ("go", to) => (format!("location = {to:?}"), String::new()),
        ("back", _) => ("history.back()".into(), String::new()),
        ("open", to) => (format!("window.open({to:?})"), String::new()),
        ("img", src) => (String::new(), format!("<img src={src:?}>")),
        ("link", to) => {
            let cover = "position:fixed;left:0;top:0;width:100%;height:100%";
            (
                String::new(),
                format!("<a href={to:?} style={cover:?}>{to}</a>"),
            )
        }
        ("frames", n) => {
            let frames: String = (1..=n.parse().unwrap_or(0))
                .map(|i| format!("<iframe src='/frame{i}.html?go=/landed{i}.html'></iframe>"))
                .collect();
            let count = "const landed = () => [...document.querySelectorAll('iframe')].filter(\
                         f => f.contentWindow.location.pathname.startsWith('/landed')).length;\
                         setInterval(() => document.title = 'landed ' + landed(), 100)";
            (String::new(), format!("{frames}<script>{count}</script>"))
        }
        _ => (String::new(), String::new()),
    };
    let body = format!(
        "<!DOCTYPE html><title>{path}</title><h1>{path}</h1>{extra}\
         <script>addEventListener('load', () => setTimeout(() => {{ {act} }}, 300))</script>"
    );
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
}

/// What `sig` does before any engine (decision 26): the TLS provider and the panic hook that
/// hands panics on other threads to Servo's logger, which turns a script thread's into a crash
/// of its page. Never exits.
pub fn process_setup() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        if sighurt_engine_servo::should_panic_hook_suppress_termination() {
            return;
        }
        log::error!("{info}");
    }));
}

impl Driver {
    pub fn new(config: ServoConfig, layout: Layout) -> Self {
        process_setup();
        let event_loop: EventLoop<'static, State> = EventLoop::try_new().expect("an event loop");
        let handle = event_loop.handle();
        let mut host =
            Host::new(&handle, |state: &mut State| &mut state.host).expect("a compositor");
        let (ping, source) = calloop::ping::make_ping().expect("a ping");
        // Waking the loop is enough: every iteration pumps when the waker was taken.
        handle
            .insert_source(source, |_, _, _| {})
            .expect("the ping source");
        let waker = EngineWaker::new(move || ping.ping());
        let runtime = ServoRuntime::new(config, waker.clone());
        runtime.setup_logging();
        let window = WindowId::next();
        host.create_window(
            window,
            WindowOptions {
                title: "sighurt servo".into(),
                app_id: "sighurt-servo".into(),
                ..WindowOptions::default()
            },
        );
        let state = State {
            host,
            runtime,
            waker,
            window,
            size: None,
            layout,
            pages: Vec::new(),
            focus: None,
            events: Vec::new(),
            presents: Vec::new(),
            start: Instant::now(),
            running: true,
            actions: Vec::new(),
            iterations: 0,
            navigations: Some(true),
            reads: HashMap::new(),
            called: false,
            due: None,
        };
        Self { event_loop, state }
    }

    /// One loop iteration: waits up to `timeout` (or until something happens, with `None`), and
    /// no longer than the runtime's pump deadline, then handles what happened.
    pub fn iterate(&mut self, timeout: Option<Duration>) {
        let due = self
            .state
            .due
            .map(|due| due.saturating_duration_since(Instant::now()));
        let timeout = match (timeout, due) {
            (Some(timeout), Some(due)) => Some(timeout.min(due)),
            (timeout, due) => timeout.or(due),
        };
        self.event_loop
            .dispatch(timeout, &mut self.state)
            .expect("the event loop");
        self.state.step();
    }

    /// Runs the loop for `duration`.
    pub fn run_for(&mut self, duration: Duration) {
        let end = Instant::now() + duration;
        while self.state.running {
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                break;
            };
            self.iterate(Some(left.min(Duration::from_millis(50))));
        }
    }

    /// Runs the loop until `done` holds or `timeout` passes; returns whether it held.
    pub fn run_until(&mut self, timeout: Duration, done: impl Fn(&State) -> bool) -> bool {
        let end = Instant::now() + timeout;
        while self.state.running && !done(&self.state) {
            let Some(left) = end.checked_duration_since(Instant::now()) else {
                return false;
            };
            self.iterate(Some(left.min(Duration::from_millis(50))));
        }
        done(&self.state)
    }

    /// Shuts the runtime down, then the host: the order `sig` uses.
    pub fn shutdown(self) -> State {
        let mut state = self.state;
        state.runtime.shutdown();
        state.host.flush();
        state
    }
}

impl State {
    /// Opens a view loading `url`.
    pub fn open(&mut self, url: &str, visible: bool) -> ViewId {
        let view = ViewId::next();
        let lease = self
            .host
            .create_target(self.window, PresentMode::Gl, Some(view))
            .expect("the window exists");
        let target = lease.id();
        let options = ViewOptions {
            visible,
            ..ViewOptions::default()
        };
        self.runtime
            .open_view(view, options, lease)
            .expect("the view opens");
        self.pages.push(Page {
            view,
            target,
            visible,
        });
        self.called = true;
        self.place();
        self.navigate(view, url);
        if self.focus.is_none() && visible {
            self.set_focus(Some(view));
        }
        view
    }

    pub fn navigate(&mut self, view: ViewId, url: &str) -> NavigationId {
        let navigation = NavigationId::next();
        self.command(
            view,
            EngineCommand::Navigate {
                navigation,
                url: url.into(),
            },
        );
        navigation
    }

    pub fn traverse(&mut self, view: ViewId, delta: i32) -> NavigationId {
        let navigation = NavigationId::next();
        self.command(view, EngineCommand::Traverse { navigation, delta });
        navigation
    }

    pub fn command(&mut self, view: ViewId, command: EngineCommand) {
        self.called = true;
        if let Err(error) = self.runtime.command(view, command) {
            println!("[{:8.3}] command for {view:?} failed: {error}", self.secs());
        }
    }

    /// Shows or hides `view`, as switching tabs does.
    pub fn set_visible(&mut self, view: ViewId, visible: bool) {
        if let Some(page) = self.pages.iter_mut().find(|p| p.view == view) {
            page.visible = visible;
        }
        self.command(view, EngineCommand::SetVisibility(visible));
        self.place();
    }

    /// Gives `view` the keyboard, taking it from the view that had it.
    pub fn set_focus(&mut self, view: Option<ViewId>) {
        if let Some(old) = self.focus.filter(|old| Some(*old) != view) {
            self.command(old, EngineCommand::SetFocus(false));
        }
        self.focus = view;
        let target = view.and_then(|v| self.target(v));
        self.host.set_keyboard_focus(self.window, target);
        if let Some(view) = view {
            self.command(view, EngineCommand::SetFocus(true));
        }
    }

    pub fn close(&mut self, view: ViewId) {
        self.runtime.close_view(view);
        self.called = true;
        self.pages.retain(|p| p.view != view);
        if self.focus == Some(view) {
            self.focus = None;
        }
        self.place();
    }

    pub fn target(&self, view: ViewId) -> Option<TargetId> {
        self.pages.iter().find(|p| p.view == view).map(|p| p.target)
    }

    fn view_of(&self, target: TargetId) -> Option<ViewId> {
        self.pages
            .iter()
            .find(|p| p.target == target)
            .map(|p| p.view)
    }

    pub fn secs(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    /// The events of `view`, oldest first.
    pub fn events_of(&self, view: ViewId) -> impl DoubleEndedIterator<Item = &EngineEvent> {
        self.events
            .iter()
            .map(|(_, e)| e)
            .filter(move |e| e.view() == Some(view))
    }

    /// Submitted frames of `view` so far.
    pub fn submitted(&self, view: ViewId) -> usize {
        self.presents
            .iter()
            .filter(|(v, outcome)| *v == view && *outcome == PresentOutcome::Submitted)
            .count()
    }

    /// Places every page for the latest configured size.
    pub fn place(&mut self) {
        let Some(size) = self.size else {
            return;
        };
        let count = self.pages.len().max(1) as u32;
        let targets = self
            .pages
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let rect = match self.layout {
                    Layout::Stack => WindowRect {
                        x: 0,
                        y: 0,
                        width: size.width,
                        height: size.height,
                    },
                    Layout::Split => {
                        let width = size.width / count;
                        WindowRect {
                            x: (width * index as u32) as i32,
                            y: 0,
                            width,
                            height: size.height,
                        }
                    }
                };
                TargetPlacement {
                    target: page.target,
                    rect,
                    visible: page.visible,
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
            println!("[{:8.3}] placement refused: {error}", self.secs());
        }
    }

    /// Handles what the host and the engine have, and presents on the host's permits.
    fn step(&mut self) {
        self.iterations += 1;
        while let Some(event) = self.host.next_event() {
            self.host_event(event);
        }
        let due = self.due.is_some_and(|due| Instant::now() >= due);
        if std::mem::take(&mut self.called) | self.waker.take() | due {
            self.due = None;
            loop {
                let mut events = Vec::new();
                let status = self.runtime.pump(&mut events);
                for event in events {
                    self.engine_event(event);
                }
                match status {
                    PumpStatus::Pending { deadline: None } => {}
                    PumpStatus::Pending { deadline } => break self.due = deadline,
                    PumpStatus::Idle => break,
                }
            }
        }
        for permit in self.host.permits() {
            let Some(view) = self.view_of(permit.target()) else {
                continue;
            };
            match self.runtime.present(view, permit) {
                Ok(outcome) => self.presents.push((view, outcome)),
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
                if let Some(view) = self.view_of(target) {
                    self.command(view, EngineCommand::SetMetrics(metrics));
                }
            }
            HostEvent::Input { target, event, .. } => {
                if let Some(view) = target.and_then(|t| self.view_of(t)) {
                    if matches!(
                        event.kind,
                        sighurt_api::input::InputKind::Pointer(
                            sighurt_api::input::PointerEvent::Button { .. }
                        )
                    ) && self.focus != Some(view)
                    {
                        self.set_focus(Some(view));
                    }
                    self.command(view, EngineCommand::Input(event));
                }
            }
            HostEvent::ClipboardRead { request, result } => {
                if let Some(view) = self.reads.remove(&request) {
                    let result = result.map(ServiceReply::Clipboard);
                    self.command(
                        view,
                        EngineCommand::ResolveServiceRequest { request, result },
                    );
                }
            }
            HostEvent::ConnectionLost { error } => {
                println!("[{:8.3}] connection lost: {error}", self.secs());
                self.running = false;
            }
        }
    }

    fn engine_event(&mut self, event: EngineEvent) {
        println!("[{:8.3}] {event:?}", self.secs());
        let target = event.view().and_then(|view| self.target(view));
        match (&event, target) {
            (EngineEvent::FrameReady { .. }, Some(target)) => self.host.invalidate(target),
            (EngineEvent::CursorChanged { cursor, .. }, Some(target)) => {
                self.host.set_cursor(target, *cursor)
            }
            (EngineEvent::TextInputChanged { state, .. }, Some(target)) => {
                self.host.set_text_input(self.window, target, state.clone())
            }
            (EngineEvent::ServiceRequested(request), _) => {
                let result = match &request.service {
                    Service::ClipboardRead {
                        selection,
                        mime_types,
                        limit,
                    } => {
                        self.reads.insert(request.id, request.view);
                        self.host
                            .clipboard_read(request.id, *selection, mime_types, *limit);
                        None
                    }
                    Service::ClipboardWrite { selection, items } => Some(
                        self.host
                            .clipboard_write(*selection, items.clone())
                            .map(|()| ServiceReply::Done),
                    ),
                    Service::Permission(_) => Some(Err(ServiceError::Denied)),
                    _ => Some(Err(ServiceError::Unsupported)),
                };
                if let Some(result) = result {
                    let (request, view) = (request.id, request.view);
                    self.command(
                        view,
                        EngineCommand::ResolveServiceRequest { request, result },
                    );
                }
            }
            (EngineEvent::NavigationRequested { view, request, .. }, _) => {
                if let Some(allow) = self.navigations {
                    let (view, request) = (*view, *request);
                    self.command(view, EngineCommand::AnswerNavigation { request, allow });
                }
            }
            (EngineEvent::ViewClosed { view }, Some(_)) => {
                // Closed by the engine: its target is gone from the window.
                let view = *view;
                self.pages.retain(|p| p.view != view);
                self.place();
            }
            _ => {}
        }
        self.events.push((self.start.elapsed(), event));
    }
}
