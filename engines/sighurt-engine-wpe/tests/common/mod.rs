//! A small owner loop for the WPE adapter's tests and example, playing the executable: one
//! window with a CPU-painted chrome strip above the pages, one native page target per view, the
//! host's input and metrics routed to the views, frames presented on the host's permits, and the
//! engine's service requests answered (the clipboard through the host's, permissions as
//! [`State::grant`] says, file pickers with [`State::files`], downloads recorded, anything else
//! refused) and the page's navigations allowed, unless a test answers them itself
//! ([`State::navigations`]). Every engine event is kept with the time since the start, and
//! printed (frame notices only counted).

#![allow(dead_code)] // Each test binary uses part of it.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use calloop::EventLoop;
use sighurt_api::engine::{
    EngineCommand, EngineRuntime, EngineWaker, PresentOutcome, PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::geometry::{LogicalSize, WindowRect};
use sighurt_api::ids::{NavigationId, RequestId, TargetId, ViewId, WindowId};
use sighurt_api::input::{ButtonState, InputKind, PointerEvent};
use sighurt_api::services::{Service, ServiceError, ServiceReply, ServiceRequest, ServiceResult};
use sighurt_engine_wpe::{WpeConfig, WpeRuntime};
use sighurt_wayland::{
    Host, HostEvent, InputRegion, Placement, PresentMode, TargetLease, TargetPlacement,
    WindowOptions,
};

/// The chrome strip's height, in logical pixels.
pub const CHROME: u32 = 40;

/// How the views share the page area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// Each view covers the page area; hidden ones are unmapped (tabs).
    Stack,
    /// Side by side, all visible at once.
    Split,
}

pub struct Page {
    pub view: ViewId,
    pub target: TargetId,
    pub visible: bool,
    /// When each frame was submitted.
    pub frames: Vec<Instant>,
}

pub struct State {
    pub host: Host,
    pub runtime: WpeRuntime,
    pub waker: EngineWaker,
    pub window: WindowId,
    pub size: Option<LogicalSize>,
    pub layout: Layout,
    pub chrome: Option<TargetLease>,
    pub chrome_paints: u64,
    pub pages: Vec<Page>,
    /// The view with the keyboard, or `None` for the chrome.
    pub focus: Option<ViewId>,
    /// Every engine event so far, with the time since the start.
    pub events: Vec<(Duration, EngineEvent)>,
    pub start: Instant,
    pub running: bool,
    /// Scripted actions that came due, for the caller to carry out.
    pub actions: Vec<String>,
    /// Loop iterations so far: wakeups, to show an idle loop sleeps.
    pub iterations: u64,
    /// Whether permission requests are granted.
    pub grant: bool,
    /// What file pickers answer; empty: the user cancels.
    pub files: Vec<PathBuf>,
    /// Download requests, as asked.
    pub downloads: Vec<ServiceRequest>,
    /// Every service request, with its answer once given.
    pub requests: Vec<(ServiceRequest, Option<ServiceResult>)>,
    /// How the navigations pages ask for are answered: allowed, denied, or (`None`) by the test.
    pub navigations: Option<bool>,
    /// Clipboard reads in flight, for the view that asked.
    reads: HashMap<RequestId, ViewId>,
    called: bool,
    due: Option<Instant>,
}

pub struct Driver {
    pub event_loop: EventLoop<'static, State>,
    pub state: State,
}

/// How long `/slow/...` pages of [`serve`] take to answer.
pub const SLOW: Duration = Duration::from_secs(3);

/// Every request the server answered, as its path (with the query) and its User-Agent header.
static SERVED: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

/// Whether the server answered a request for `path`, whatever its query.
pub fn served(path: &str) -> bool {
    agent_of(path).is_some()
}

/// The User-Agent header of the latest request for `path`, whatever its query.
pub fn agent_of(path: &str) -> Option<String> {
    let served = SERVED.lock().unwrap();
    let mut requests = served.iter().rev();
    let found = requests.find(|(p, _)| p.split('?').next() == Some(path));
    found.map(|(_, agent)| agent.clone())
}

/// Starts an HTTP server on 127.0.0.1 that runs until the process ends, and returns its
/// address (`http://127.0.0.1:PORT`), a secure context for WebKit. `/204` answers "204 No
/// Content"; `/attachment` is a text file to save; `/data.bin` is content WebKit can't show;
/// `/geolocation` asks for the position, saying the outcome in its title; `/close` calls `window.close()` and `/mailto` goes to a `mailto:`
/// address; `/agent` puts what its script sees as `navigator.userAgent` in its title. Pages
/// that cover themselves with one thing to click, going to the path after `?`: `/link?` a link,
/// `/script?` a button whose script navigates, `/get?` and `/post?` a form's button.
/// `/frames?N` stacks N frames 30 pixels tall, frame i (from 1) a `/link?/landedi`. Anything
/// else is a page titled with its path, which `/slow/...` sends after [`SLOW`].
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
    let mut agent = String::new();
    while reader.read_line(&mut header).is_ok_and(|n| n > 2) {
        if let Some(value) = header.strip_prefix("User-Agent:") {
            agent = value.trim().to_owned();
        }
        header.clear();
    }
    let path = request.split_whitespace().nth(1).unwrap_or("/").to_owned();
    SERVED.lock().unwrap().push((path.clone(), agent));
    let (route, to) = path.split_once('?').unwrap_or((&path, ""));
    let cover = "position: fixed; inset: 0; width: 100%; height: 100%";
    let frames: String = (1..=to.parse().unwrap_or(0))
        .map(|i| format!("<iframe src='/link?/landed{i}' style='display: block; border: 0; width: 100%; height: 30px'></iframe>"))
        .collect();
    if path.starts_with("/slow/") {
        std::thread::sleep(SLOW);
    }
    let script = |act: &str| {
        format!(
            "<script>addEventListener('load', () => setTimeout(() => {{ {act} }}, 300))</script>"
        )
    };
    let say = "const say = (m) => { document.title = m; };";
    let (status, headers, body) = match route {
        "/204" => ("204 No Content", "", String::new()),
        "/attachment" => (
            "200 OK",
            "Content-Type: text/plain\r\nContent-Disposition: attachment; filename=\"report.txt\"\r\n",
            "a report\n".into(),
        ),
        "/data.bin" => ("200 OK", "Content-Type: application/x-sighurt-test\r\n", "\x01\x02".into()),
        "/geolocation" => ("200 OK", "", script(&format!(
            "{say} navigator.geolocation.getCurrentPosition(() => say('geolocation granted'), \
             (e) => say('geolocation error ' + e.code))"
        ))),
        "/close" => ("200 OK", "", script("window.close()")),
        "/mailto" => ("200 OK", "", script("location = 'mailto:someone@example.test'")),
        "/agent" => ("200 OK", "", "<script>document.title = navigator.userAgent</script>".into()),
        "/link" => ("200 OK", "", format!("<body style='margin: 0'><a href='{to}' style='display: block; {cover}'>link</a>")),
        "/script" => ("200 OK", "", format!("<button onclick=\"location = '{to}'\" style='{cover}'>script</button>")),
        "/get" | "/post" => ("200 OK", "", format!("<form method='{}' action='{to}'><button style='{cover}'>form</button></form>", &route[1..])),
        "/frames" => ("200 OK", "", format!("<body style='margin: 0'>{frames}")),
        _ => ("200 OK", "", format!("<!DOCTYPE html><title>{path}</title><h1>{path}</h1>")),
    };
    let headers = match headers {
        "" => "Content-Type: text/html\r\n",
        headers => headers,
    };
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

impl Driver {
    pub fn new(config: WpeConfig, layout: Layout) -> Self {
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
        let state = State {
            host,
            runtime,
            waker,
            window,
            size: None,
            layout,
            chrome: Some(chrome),
            chrome_paints: 0,
            pages: Vec::new(),
            focus: None,
            events: Vec::new(),
            start: Instant::now(),
            running: true,
            actions: Vec::new(),
            iterations: 0,
            grant: false,
            files: Vec::new(),
            downloads: Vec::new(),
            requests: Vec::new(),
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

    /// Runs `script` in `view` and returns its result.
    pub fn eval(&mut self, view: ViewId, script: &str) -> Result<String, String> {
        self.state.runtime.evaluate(view, script).expect("a view");
        self.state.called = true;
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            let results = self.state.runtime.take_script_results();
            if let Some((_, result)) = results.into_iter().find(|(v, _)| *v == view) {
                return result;
            }
            assert!(Instant::now() < end, "no script result");
            self.iterate(Some(Duration::from_millis(20)));
        }
    }

    /// Shuts the runtime down, then the chrome and the host: the order `sig` uses.
    pub fn shutdown(self) -> State {
        let mut state = self.state;
        state.runtime.shutdown();
        if let Some(chrome) = state.chrome.take() {
            chrome.retire();
        }
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
            .create_target(self.window, PresentMode::Native, Some(view))
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
            frames: Vec::new(),
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

    pub fn reload(&mut self, view: ViewId) -> NavigationId {
        let navigation = NavigationId::next();
        self.command(view, EngineCommand::Reload { navigation });
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

    /// Gives `view` the keyboard (`None`: the chrome), taking it from the view that had it.
    pub fn set_focus(&mut self, view: Option<ViewId>) {
        if let Some(old) = self.focus.filter(|old| Some(*old) != view) {
            self.command(old, EngineCommand::SetFocus(false));
        }
        self.focus = view;
        let target = match view {
            Some(view) => self.target(view),
            None => self.chrome.as_ref().map(|c| c.id()),
        };
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
        self.pages
            .iter()
            .find(|p| p.view == view)
            .map_or(0, |p| p.frames.len())
    }

    /// The latest title of `view`.
    pub fn title(&self, view: ViewId) -> Option<String> {
        self.events_of(view).rev().find_map(|e| match e {
            EngineEvent::TitleChanged { title, .. } => Some(title.clone().unwrap_or_default()),
            _ => None,
        })
    }

    /// Places the chrome and every page for the latest configured size.
    pub fn place(&mut self) {
        let Some(size) = self.size else {
            return;
        };
        let area = WindowRect {
            x: 0,
            y: CHROME as i32,
            width: size.width,
            height: size.height.saturating_sub(CHROME),
        };
        let count = self.pages.len().max(1) as u32;
        let mut targets: Vec<TargetPlacement> = self
            .chrome
            .iter()
            .map(|chrome| TargetPlacement {
                target: chrome.id(),
                rect: WindowRect {
                    x: 0,
                    y: 0,
                    width: size.width,
                    height: CHROME.min(size.height),
                },
                visible: true,
                input: InputRegion::Whole,
            })
            .collect();
        targets.extend(self.pages.iter().enumerate().map(|(index, page)| {
            let rect = match self.layout {
                Layout::Stack => area,
                Layout::Split => {
                    let width = size.width / count;
                    WindowRect {
                        x: (width * index as u32) as i32,
                        width,
                        ..area
                    }
                }
            };
            TargetPlacement {
                target: page.target,
                rect,
                visible: page.visible,
                input: InputRegion::Whole,
            }
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

    /// Handles what the host and the engine have, and presents on the host's permits.
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
        let chrome = self.chrome.as_ref().map(|c| c.id());
        for permit in self.host.permits() {
            if Some(permit.target()) == chrome {
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
                if self.chrome.as_ref().is_some_and(|c| c.id() == target) {
                    self.host.invalidate(target);
                } else if let Some(view) = self.view_of(target) {
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
                let chrome = self.chrome.as_ref().map(|c| c.id());
                if target.is_some() && target == chrome && pressed {
                    self.set_focus(None);
                }
                if let Some(view) = target.and_then(|t| self.view_of(t)) {
                    if pressed && self.focus != Some(view) {
                        self.set_focus(Some(view));
                    }
                    self.command(view, EngineCommand::Input(event));
                }
            }
            HostEvent::ClipboardRead { request, result } => {
                if let Some(view) = self.reads.remove(&request) {
                    let result = result.map(ServiceReply::Clipboard);
                    self.answer(view, request, result);
                }
            }
            HostEvent::ConnectionLost { error } => {
                println!("[{:8.3}] connection lost: {error}", self.secs());
                self.running = false;
            }
        }
    }

    fn answer(&mut self, view: ViewId, request: RequestId, result: ServiceResult) {
        if let Some((_, answer)) = self.requests.iter_mut().find(|(r, _)| r.id == request) {
            *answer = Some(result.clone());
        }
        self.command(
            view,
            EngineCommand::ResolveServiceRequest { request, result },
        );
    }

    fn engine_event(&mut self, event: EngineEvent) {
        if !matches!(event, EngineEvent::FrameReady { .. }) {
            println!("[{:8.3}] {event:?}", self.secs());
        }
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
                self.requests.push((request.clone(), None));
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
                    Service::Permission(_) if self.grant => Some(Ok(ServiceReply::Done)),
                    Service::Permission(_) => Some(Err(ServiceError::Denied)),
                    Service::PickFiles { .. } if self.files.is_empty() => {
                        Some(Err(ServiceError::Cancelled))
                    }
                    Service::PickFiles { .. } => Some(Ok(ServiceReply::Files(self.files.clone()))),
                    Service::Download { .. } => {
                        self.downloads.push(request.clone());
                        Some(Ok(ServiceReply::Done))
                    }
                    _ => Some(Err(ServiceError::Unsupported)),
                };
                if let Some(result) = result {
                    self.answer(request.view, request.id, result);
                }
            }
            (EngineEvent::NavigationRequested { view, request, .. }, _) => {
                if let Some(allow) = self.navigations {
                    let (view, request) = (*view, *request);
                    self.command(view, EngineCommand::AnswerNavigation { request, allow });
                }
            }
            (EngineEvent::ViewClosed { view }, Some(_)) => {
                let view = *view;
                self.pages.retain(|p| p.view != view);
                self.place();
            }
            _ => {}
        }
        self.events.push((self.start.elapsed(), event));
    }
}
