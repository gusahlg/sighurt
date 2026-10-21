//! `sig --dump URL`: a session with no terminal that prints the page as reflowed text.
//!
//! Owns: the headless [`Frontend`] (it shows nothing and keeps the latest page snapshot), the
//! loop that opens the page, waits for it to be complete and prints [`textmode::dump`], and
//! which engine is used. A page is complete when the core says its load ended, and then the
//! first snapshot the engine sends after being asked again has gone 300 ms without a newer one
//! (TUI.md §8.12); it fails on the first navigation error, or after the timeout. The session is
//! ephemeral: no session file is read or written, and no terminal, Wayland connection or
//! sighurt-ui is used. Must not own: the text layout ([`crate::textmode`]), browser policy (the
//! core) or the engines.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::IsTerminal;
use std::os::fd::{AsFd, OwnedFd};
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::io::Errno;
use std::time::{Duration, Instant};

use sighurt_api::events::Cursor;
use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::{DocumentGeneration, LayoutEpoch, RequestId, TargetId, ViewId};
use sighurt_api::input::{InputEvent, KeyboardFocus, TextInputState};
use sighurt_api::semantic::SemanticSnapshot;
use sighurt_api::services::{ClipboardData, Selection, ServiceError};
use sighurt_core::tabs::Content;
use sighurt_core::{BrowserEvent, BrowserState, Frontend as FrontendKind, OpenRequest};
use sighurt_runtime::diagnostics::Diagnostics;
use sighurt_runtime::frontend::Frontend;
use sighurt_runtime::persistence::{self, Paths};
use sighurt_runtime::services::{Clipboard, Services};
use sighurt_runtime::session::Session;
use sighurt_runtime::view::ViewEntry;

use crate::app::{chrome_checks, error_line, log_path, redirect_stderr, COLOR_NAMES};
use crate::sanitize::sanitize;
use crate::target::{TermPermit, TermTarget};
use crate::textmode;

/// How long the page's text must stay unchanged after the load ended: an extraction that was
/// already running when the load ended may still deliver an earlier state.
const SETTLE: Duration = Duration::from_millis(300);

/// A page that builds itself with scripts after its load ended (most of today's sites) is
/// given this long, from the end of the load, before a snapshot may be taken for it.
const GRACE: Duration = Duration::from_millis(1500);

/// A snapshot with fewer words than [`THIN`] is probably still being built: it is awaited for
/// this long from the end of the load before it is printed as it is.
const GRACE_THIN: Duration = Duration::from_secs(6);

/// The page area the engine is given. No one sees it, but pages that lay out nothing at zero
/// size (YouTube shows no content at all) or by media queries need a window of a desktop's size.
const VIEWPORT: WindowRect = WindowRect {
    x: 0,
    y: 0,
    width: 1280,
    height: 800,
};

/// Words below which a snapshot looks unfinished.
const THIN: usize = 30;

/// What `--dump` is asked for.
pub struct DumpOptions {
    /// The page to print.
    pub open: OpenRequest,
    /// Columns to wrap to.
    pub width: u16,
    /// How long to wait for the page before giving up (exit code 2).
    pub timeout: Duration,
}

/// Wakes the loop from engine and service threads.
#[derive(Default)]
struct Wake {
    woken: Mutex<bool>,
    signal: Condvar,
}

impl Wake {
    fn wake(&self) {
        *self.woken.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.signal.notify_one();
    }

    /// Sleeps until woken or `timeout` passes.
    fn wait(&self, timeout: Duration) {
        let mut woken = self.woken.lock().unwrap_or_else(PoisonError::into_inner);
        if !*woken {
            woken = self
                .signal
                .wait_timeout(woken, timeout)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        *woken = false;
    }
}

/// The frontend of a session nobody looks at: pages get empty targets and the latest snapshot
/// is kept.
struct Headless {
    metrics: Metrics,
    /// The latest snapshot, and when and how many have arrived.
    snapshot: Option<(ViewId, Box<SemanticSnapshot>)>,
    count: u64,
    last: Instant,
    reads: Vec<RequestId>,
    /// The view of each target, and the targets whose engine has a frame to present.
    views: BTreeMap<TargetId, ViewId>,
    ready: Vec<TargetId>,
}

impl Clipboard for Headless {
    fn read(&mut self, request: RequestId, _: Selection, _: &[String], _: usize) {
        self.reads.push(request);
    }

    fn write(&mut self, _: Selection, _: Vec<ClipboardData>) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported)
    }

    fn cancel(&mut self, request: RequestId) {
        self.reads.retain(|&read| read != request);
    }
}

impl Frontend for Headless {
    type Target = TermTarget;
    type Permit = TermPermit;

    fn open_target(&mut self, view: ViewId, _: &str) -> Result<(TargetId, TermTarget), String> {
        let (target, _) = TermTarget::new(self.metrics);
        let id = TargetId::next();
        self.views.insert(id, view);
        Ok((id, target))
    }

    fn place(&mut self, _: &BTreeMap<ViewId, ViewEntry>, _: &mut Diagnostics) -> bool {
        false
    }

    fn forget_target(&mut self, target: TargetId) {
        self.views.remove(&target);
        self.ready.retain(|&ready| ready != target);
    }

    fn focus(&mut self, _: KeyboardFocus, _: &mut VecDeque<BrowserEvent>) {}

    fn chrome_input(&mut self, _: InputEvent, _: &mut VecDeque<BrowserEvent>) {}

    fn chrome_command(&mut self, _: String, _: &mut VecDeque<BrowserEvent>) {}

    fn page_cursor(&mut self, _: Option<TargetId>, _: Cursor) {}

    fn page_text_input(&mut self, _: Option<TargetId>, _: Option<TextInputState>) {}

    fn notice(&mut self, _: String) {}

    fn redraw(&mut self) {}

    fn show(&mut self, _: ViewId, _: bool) {}

    fn frame_ready(&mut self, target: TargetId) {
        if !self.ready.contains(&target) {
            self.ready.push(target);
        }
    }

    fn before_forced_exit(&mut self) {}

    fn semantics_changed(&mut self, view: ViewId, snapshot: Box<SemanticSnapshot>) {
        self.snapshot = Some((view, snapshot));
        self.count += 1;
        self.last = Instant::now();
    }
}

/// Opens `options.open`, waits for the page and prints it to stdout. Returns the exit code: 0
/// printed, 2 timed out. A page that fails to load, or a build with no engine that gives page
/// text, is an error for the caller to print. `signals` is `sig`'s signal pipe: SIGTERM and
/// SIGINT end the dump.
pub fn run(options: DumpOptions, signals: &OwnedFd) -> Result<ExitCode, String> {
    // Library and engine logging goes to the log file when stderr is a terminal.
    let saved = match std::io::stderr().is_terminal() {
        true => Some(redirect_stderr(&log_path())?),
        false => None,
    };
    // Every diagnostic is page data or the user's address: contained before it is printed.
    let result = dump(options, signals).map_err(|error| sanitize(&error));
    if let (Some(saved), Err(_) | Ok(ExitCode::FAILURE)) = (&saved, &result) {
        let _ = rustix::stdio::dup2_stderr(saved);
    }
    result
}

/// The engine `--dump` opens with. An explicit `--engine` must give page text. Otherwise the
/// engine is the one [`sighurt_core::router`] would open the address with. A specific rule that
/// names an engine without page text is an error (pass `--engine wpe`). An http(s) or file URL
/// that only falls through to the default engine uses WPE, or the first compiled-in engine with
/// page text, when that default has none: TUI.md's WPE default for ordinary pages.
fn choose_dump_engine(
    registry: &sighurt_runtime::registry::Registry<TermTarget, TermPermit>,
    config: &sighurt_core::config::Config,
    home: Option<&std::path::Path>,
    open: &OpenRequest,
) -> Result<String, String> {
    if let Some(name) = &open.engine {
        return match registry.engine(name) {
            Some(engine) if engine.capabilities.semantics => Ok(name.clone()),
            Some(_) => Err(format!("the {name} engine gives no page text for --dump")),
            None => Err(format!("the {name} engine is not part of this build")),
        };
    }
    let fixed = sighurt_core::router::fixup(&open.input, config.search(), config.keywords(), home);
    if fixed.is_empty() {
        return Err("the address is empty".into());
    }
    let Some(address) = Address::parse(&fixed) else {
        return Err(format!("{fixed} is not an address"));
    };
    let name = match routed(&address, config) {
        Routed::Internal => {
            return Err("sig: pages are Sighurt's own and have no page text for --dump".into())
        }
        Routed::Error(message) => return Err(message),
        Routed::Engine(name) => name,
    };
    if registry
        .engine(&name)
        .is_some_and(|engine| engine.capabilities.semantics)
    {
        return Ok(name);
    }
    if default_without_a_rule(&address, config, &name) {
        return engine_with_text(registry);
    }
    match registry.engine(&name) {
        Some(_) => Err(format!(
            "the {name} engine gives no page text for --dump; pass --engine wpe"
        )),
        None => Err(format!("the {name} engine is not part of this build")),
    }
}

/// Where [`sighurt_core::router::route`] would send `address` with no explicit engine.
enum Routed {
    Engine(String),
    Internal,
    Error(String),
}

fn routed(address: &Address, config: &sighurt_core::config::Config) -> Routed {
    let routes = config.routes();
    let scheme = address.scheme.as_str();
    let engine = if scheme == "sig" {
        return Routed::Internal;
    } else if !matches!(scheme, "http" | "https" | "file") {
        match routes
            .iter()
            .find(|route| route.schemes.iter().any(|listed| listed == scheme))
        {
            Some(route) => route.engine.clone(),
            None => return Routed::Error(format!("no engine opens {scheme}: URLs")),
        }
    } else {
        match web_rule(routes, address) {
            Some(route) => route.engine.clone(),
            None => config.default_engine().to_owned(),
        }
    };
    if !config.is_enabled(&engine) {
        return Routed::Error(format!(
            "the {engine} engine is not enabled in the configuration"
        ));
    }
    Routed::Engine(engine)
}

/// The extension or host rule that claims `address`, if one does. Same order as the core:
/// a file by its extension; an http(s) URL by origin and extension, else the most specific host.
fn web_rule<'a>(
    routes: &'a [sighurt_core::config::EngineRoutes],
    address: &Address,
) -> Option<&'a sighurt_core::config::EngineRoutes> {
    let claims = |route: &&sighurt_core::config::EngineRoutes| {
        address
            .extension
            .as_ref()
            .is_some_and(|extension| route.extensions.contains(extension))
    };
    if address.scheme == "file" {
        return routes.iter().find(claims);
    }
    routes
        .iter()
        .filter(claims)
        .find(|route| route.origins.contains(&address.origin))
        .or_else(|| claiming_host(routes, &address.host))
}

/// True when `engine` is the default and no extension or host rule claimed the URL.
fn default_without_a_rule(
    address: &Address,
    config: &sighurt_core::config::Config,
    engine: &str,
) -> bool {
    engine == config.default_engine()
        && matches!(address.scheme.as_str(), "http" | "https" | "file")
        && web_rule(config.routes(), address).is_none()
}

fn claiming_host<'a>(
    routes: &'a [sighurt_core::config::EngineRoutes],
    host: &str,
) -> Option<&'a sighurt_core::config::EngineRoutes> {
    let specificity = |pattern: &String| match pattern.strip_prefix("*.") {
        Some(suffix) => host
            .strip_suffix(suffix)
            .is_some_and(|under| under.ends_with('.'))
            .then_some(suffix.len()),
        None => (*pattern == host).then_some(usize::MAX),
    };
    routes
        .iter()
        .flat_map(|route| route.hosts.iter().map(move |pattern| (pattern, route)))
        .filter_map(|(pattern, route)| Some((specificity(pattern)?, route)))
        .max_by_key(|&(specificity, _)| specificity)
        .map(|(_, route)| route)
}

fn engine_with_text(
    registry: &sighurt_runtime::registry::Registry<TermTarget, TermPermit>,
) -> Result<String, String> {
    if let Some(engine) = registry.engine("wpe").filter(|e| e.capabilities.semantics) {
        return Ok(engine.name.to_owned());
    }
    registry
        .engines()
        .find(|engine| engine.capabilities.semantics)
        .map(|engine| engine.name.to_owned())
        .ok_or_else(|| "this sig has no engine that gives page text (WPE WebKit does)".into())
}

/// The parts of an address [`sighurt_core::router::route`] looks at.
///
/// Hierarchical URLs are split the way the `url` crate reports them for ASCII hosts: the host
/// without brackets, lower case, default ports left out of the origin. Non-ASCII hosts are
/// lowercased but not punycoded.
struct Address {
    scheme: String,
    host: String,
    origin: String,
    extension: Option<String>,
}

impl Address {
    fn parse(input: &str) -> Option<Self> {
        let (scheme, rest) = input.split_once(':')?;
        if scheme.is_empty()
            || !scheme
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'.' | b'-'))
        {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        if !rest.starts_with("//") {
            let path = rest.split(['?', '#']).next().unwrap_or("");
            return Some(Self {
                scheme,
                host: String::new(),
                origin: String::new(),
                extension: extension_of(path),
            });
        }
        let rest = &rest[2..];
        let (authority, after) = match rest.find(['/', '?', '#']) {
            Some(at) => (&rest[..at], &rest[at..]),
            None => (rest, ""),
        };
        let hostport = match authority.rsplit_once('@') {
            Some((_, hostport)) => hostport,
            None => authority,
        };
        let (host, port) = split_host_port(hostport)?;
        let path = after.split(['?', '#']).next().unwrap_or("");
        let origin = http_origin(&scheme, &host, port);
        Some(Self {
            scheme,
            host,
            origin,
            extension: extension_of(path),
        })
    }
}

fn split_host_port(hostport: &str) -> Option<(String, Option<u16>)> {
    if let Some(host) = hostport.strip_prefix('[') {
        let (host, rest) = host.split_once(']')?;
        let port = match rest.strip_prefix(':') {
            Some(port) => Some(port.parse().ok()?),
            None if rest.is_empty() => None,
            None => return None,
        };
        // `host_str` keeps the brackets the URL serialization uses.
        return Some((format!("[{}]", host.to_ascii_lowercase()), port));
    }
    match hostport.rsplit_once(':') {
        Some((host, port))
            if !host.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()) =>
        {
            Some((host.to_ascii_lowercase(), Some(port.parse().ok()?)))
        }
        _ => Some((hostport.to_ascii_lowercase(), None)),
    }
}

fn http_origin(scheme: &str, host: &str, port: Option<u16>) -> String {
    if host.is_empty() {
        return "null".into();
    }
    let default = match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    };
    match (default, port) {
        (Some(default), Some(port)) if port == default => format!("{scheme}://{host}"),
        (Some(_), None) | (None, None) => format!("{scheme}://{host}"),
        (_, Some(port)) => format!("{scheme}://{host}:{port}"),
    }
}

fn extension_of(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or("");
    let (_, extension) = name.rsplit_once('.')?;
    let extension = percent_decode(extension);
    (!extension.is_empty()).then(|| extension.to_ascii_lowercase())
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_owned())
}

fn dump(options: DumpOptions, signals: &OwnedFd) -> Result<ExitCode, String> {
    let paths = Paths {
        config: Paths::from_env().config,
        session: None,
    };
    let mut registry = crate::engines::build();
    let mut env = registry.environment(chrome_checks(), COLOR_NAMES);
    env.frontend = FrontendKind::Terminal;
    let config = match persistence::load_config(paths.config.as_deref(), &env) {
        Ok(config) => config,
        Err(error) => {
            log::warn!("the configuration can't be used: {error}");
            persistence::load_config(None, &env).map_err(|e| e.to_string())?
        }
    };
    // Routing, not "WPE first": a demo: URL configured for the demo engine must not be handed
    // to WPE. TUI.md still asks for WPE when the page is an ordinary http(s) or file URL whose
    // only claim is the default engine and that engine gives no page text (Servo).
    let engine = choose_dump_engine(
        &registry,
        &config,
        env.home_dir.as_deref(),
        &options.open,
    )?;
    registry.configure(&config);
    sighurt_runtime::registry::install_logger();
    let wake = Arc::new(Wake::default());
    let services_wake = Arc::clone(&wake);
    let services = Services::new(move || services_wake.wake());
    let session_wake = Arc::clone(&wake);
    let mut session = Session::new(
        BrowserState::new(config, env.clone()),
        env,
        registry,
        services,
        paths,
        None,
        Diagnostics::new(None),
        Arc::new(move || session_wake.wake()),
    );
    let mut headless = Headless {
        metrics: Metrics::new(LayoutEpoch::next(), VIEWPORT, Scale::ONE),
        snapshot: None,
        count: 0,
        last: Instant::now(),
        reads: Vec::new(),
        views: BTreeMap::new(),
        ready: Vec::new(),
    };
    let open = OpenRequest {
        input: options.open.input.clone(),
        engine: Some(engine),
    };
    session.queue_mut().push_back(BrowserEvent::Start {
        session: None,
        open: vec![open],
    });
    session.run_core(&mut headless);
    let outcome = wait(&mut session, &mut headless, &wake, &options, signals);
    let text = match &outcome {
        Ok(Some(snapshot)) => {
            if std::env::var_os("SIG_DUMP_STATS").is_some() {
                eprintln!("{}", stats(snapshot));
            }
            Some(textmode::dump(snapshot, options.width))
        }
        _ => None,
    };
    let code = match &outcome {
        Ok(Some(_)) => ExitCode::SUCCESS,
        Ok(None) => ExitCode::from(2),
        Err(_) => ExitCode::FAILURE,
    };
    // The text is out before the engines shut down, which may end the process.
    let mut code = code;
    let mut written = Ok(true);
    if let Some(text) = text {
        written = write_out(text.as_bytes(), signals, options.timeout);
        match written {
            Ok(true) => {}
            Ok(false) => code = ExitCode::from(2),
            Err(_) => code = ExitCode::FAILURE,
        }
    }
    session.set_quit(code);
    let _ = session.shutdown(&mut headless, |_, _| {});
    match written {
        Ok(true) => {}
        Ok(false) => {
            eprintln!("sig: timed out writing the page: nothing reads stdout");
            return Ok(code);
        }
        Err(error) => return Err(error),
    }
    match outcome {
        Ok(None) => {
            eprintln!(
                "sig: timed out after {} s waiting for the page's text",
                options.timeout.as_secs()
            );
            Ok(code)
        }
        Ok(Some(_)) => Ok(code),
        Err(error) => Err(error),
    }
}

/// How many words the snapshot's nodes hold.
fn snapshot_words(snapshot: &SemanticSnapshot) -> usize {
    snapshot
        .nodes
        .iter()
        .map(|node| {
            node.text
                .as_deref()
                .map_or(0, |t| t.split_whitespace().count())
        })
        .sum()
}

/// One line of counts for the corpus harness (`SIG_DUMP_STATS=1`, bench/compat/run --dump):
/// what the snapshot holds, whatever the layout prints of it.
fn stats(snapshot: &SemanticSnapshot) -> String {
    use sighurt_api::semantic::NodeKind;
    let (mut headings, mut links, mut controls, mut words) = (0, 0, 0, 0);
    for node in &snapshot.nodes {
        match node.kind {
            NodeKind::Heading(_) => headings += 1,
            NodeKind::Link => links += 1,
            NodeKind::Button
            | NodeKind::TextField
            | NodeKind::TextArea
            | NodeKind::Checkbox
            | NodeKind::Radio
            | NodeKind::Select => controls += 1,
            _ => {}
        }
        words += node
            .text
            .as_deref()
            .map_or(0, |t| t.split_whitespace().count());
    }
    format!(
        "sig-stats nodes={} headings={headings} links={links} controls={controls} words={words} truncated={}",
        snapshot.nodes.len(),
        snapshot.truncated
    )
}

/// Writes `bytes` to stdout without ever blocking on it: it waits for the pipe to take more, a
/// chunk at a time, and gives up at `timeout` (`Ok(false)`) or on SIGTERM and SIGINT.
fn write_out(bytes: &[u8], signals: &OwnedFd, timeout: Duration) -> Result<bool, String> {
    // A pipe takes a write of this size whole when it reports room.
    const CHUNK: usize = 4096;
    let deadline = Instant::now() + timeout;
    let stdout = std::io::stdout();
    let mut rest = bytes;
    while !rest.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        let mut fds = [
            PollFd::new(&stdout, PollFlags::OUT),
            PollFd::new(signals, PollFlags::IN),
        ];
        let spec = Timespec::try_from(left).map_err(|e| e.to_string())?;
        match rustix::event::poll(&mut fds, Some(&spec)) {
            Ok(_) | Err(Errno::INTR) => {}
            Err(error) => return Err(format!("cannot wait for stdout: {error}")),
        }
        if fds[1].revents().contains(PollFlags::IN) {
            let mut byte = [0u8; 1];
            if rustix::io::read(signals, &mut byte).is_ok_and(|n| n > 0) && is_quit(byte[0]) {
                return Err("interrupted".into());
            }
        }
        // Hung up or in error: the write says which.
        if fds[0].revents().is_empty() {
            continue;
        }
        let chunk = &rest[..rest.len().min(CHUNK)];
        match rustix::io::write(stdout.as_fd(), chunk) {
            Ok(n) => rest = &rest[n..],
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(error) => return Err(format!("cannot write the page: {error}")),
        }
    }
    Ok(true)
}

/// Whether the byte `sig`'s signal pipe carries is a request to quit.
fn is_quit(byte: u8) -> bool {
    [libc::SIGTERM as u8, libc::SIGINT as u8].contains(&byte)
}

/// Where a document is on its way to being printed: which one the snapshot count was taken at
/// when the engine was asked again after its load ended, so that a snapshot of an earlier
/// document, or from before the load ended, is never taken for it.
#[derive(Default)]
struct Settle {
    asked: Option<(DocumentGeneration, u64)>,
}

/// What [`Settle::step`] wants done.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// Nothing yet.
    Wait,
    /// The load of the document ended: ask the engine for its content again.
    Ask,
    /// A snapshot of the document, after it loaded, has settled.
    Done,
}

impl Settle {
    /// `loaded` is the shown document once its load ended (`None` while it loads, or it is
    /// replaced), `count` how many snapshots have arrived, `quiet` that none came for
    /// [`SETTLE`], `grace` that the page has had the time [`GRACE`] and [`GRACE_THIN`] give
    /// it, and `latest` the document of the latest one.
    fn step(
        &mut self,
        loaded: Option<DocumentGeneration>,
        count: u64,
        quiet: bool,
        grace: bool,
        latest: Option<DocumentGeneration>,
    ) -> Step {
        let Some(document) = loaded else {
            self.asked = None;
            return Step::Wait;
        };
        match self.asked {
            Some((asked, _)) if asked == document => {}
            _ => {
                self.asked = Some((document, count));
                return Step::Ask;
            }
        }
        match self.asked {
            Some((_, seen)) if count > seen && quiet && grace && latest == Some(document) => {
                Step::Done
            }
            _ => Step::Wait,
        }
    }

    /// A snapshot arrived after the engine was asked, so only quiet is awaited.
    fn settling(&self, count: u64) -> bool {
        self.asked.is_some_and(|(_, seen)| count > seen)
    }
}

/// Drives the session until the page's snapshot is in: `Some`, or `None` at the timeout.
fn wait(
    session: &mut Session<Headless>,
    headless: &mut Headless,
    wake: &Wake,
    options: &DumpOptions,
    signals: &OwnedFd,
) -> Result<Option<Box<SemanticSnapshot>>, String> {
    let deadline = Instant::now() + options.timeout;
    let mut asked = HashSet::new();
    let mut settle = Settle::default();
    // When the load of the shown document ended.
    let mut loaded_at: Option<Instant> = None;
    loop {
        let mut byte = [0u8; 1];
        if rustix::io::read(signals, &mut byte).is_ok_and(|n| n > 0) && is_quit(byte[0]) {
            return Err("interrupted".into());
        }
        for request in std::mem::take(&mut headless.reads) {
            session.clipboard_done(request, Err(ServiceError::Unsupported));
        }
        session.service_completions(headless);
        session.pump(headless);
        // Pages draw and run their animation frames only for a view whose frames are taken.
        for target in std::mem::take(&mut headless.ready) {
            if let Some(&view) = headless.views.get(&target) {
                let permit = TermPermit {
                    epoch: headless.metrics.epoch(),
                    size: headless.metrics.buffer(),
                };
                let _ = session.present(view, permit, headless.metrics.buffer());
            }
        }
        let views: Vec<ViewId> = session.views().keys().copied().collect();
        for view in views {
            if asked.insert(view) {
                session.set_semantics(view, true);
            }
        }
        let core = session.core();
        let tab = core.tabs().nth(core.active());
        let Some(tab) = tab else {
            return Err("the page did not open".into());
        };
        if let Some(error) = tab.error {
            return Err(error_line(error));
        }
        let view = match tab.content {
            Content::View(view) => Some(view),
            Content::Internal(_) | Content::Blank => None,
        };
        let loaded = view.is_some() && tab.url.is_some() && !tab.loading;
        // WPE WebKit ends a load it could not do on a blank page without a navigation error.
        if loaded && tab.url == Some("about:blank") && options.open.input != "about:blank" {
            return Err(format!("cannot load {}", options.open.input));
        }
        let document = view
            .and_then(|view| session.core().view(view))
            .and_then(|view| view.document())
            .filter(|_| loaded);
        let latest = headless.snapshot.as_ref().map(|(_, s)| s.document);
        let quiet = headless.last.elapsed() >= SETTLE;
        if document.is_none() {
            loaded_at = None;
        }
        let since = loaded_at.get_or_insert_with(Instant::now).elapsed();
        let thin = headless
            .snapshot
            .as_ref()
            .is_none_or(|(_, snapshot)| snapshot_words(snapshot) < THIN);
        let grace = since >= GRACE && (!thin || since >= GRACE_THIN);
        match settle.step(document, headless.count, quiet, grace, latest) {
            Step::Ask => {
                loaded_at = Some(Instant::now());
                // Whatever arrives now was extracted after the load ended.
                if let Some(view) = view {
                    session.set_semantics(view, true);
                }
            }
            Step::Done => {
                if let Some((_, snapshot)) = headless.snapshot.take() {
                    return Ok(Some(snapshot));
                }
            }
            Step::Wait => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        let mut until = deadline;
        if settle.settling(headless.count) {
            until = until.min(headless.last + SETTLE);
        }
        if let (Some(at), true) = (loaded_at, document.is_some()) {
            for wait in [GRACE, GRACE_THIN] {
                if at + wait > now {
                    until = until.min(at + wait);
                }
            }
        }
        match session.registry().next_pump() {
            Some(None) => continue,
            Some(Some(at)) => until = until.min(at),
            None => {}
        }
        // The signal pipe has no waker: look at it at least twice a second.
        wake.wait(
            until
                .saturating_duration_since(now)
                .min(Duration::from_millis(500)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_counts_only_for_its_document_after_the_load_ended() {
        let (a, b) = (DocumentGeneration::next(), DocumentGeneration::next());
        let mut settle = Settle::default();
        // While loading nothing is asked or taken.
        assert_eq!(settle.step(None, 1, true, true, Some(a)), Step::Wait);
        // A's load ended: ask. A's earlier snapshot (count 1) does not count.
        assert_eq!(settle.step(Some(a), 1, true, true, Some(a)), Step::Ask);
        assert_eq!(settle.step(Some(a), 1, true, true, Some(a)), Step::Wait);
        // A's answer arrives but is not yet quiet.
        assert_eq!(settle.step(Some(a), 2, false, true, Some(a)), Step::Wait);
        // B replaces A (a redirect) before A settles: start over, and A's snapshot is not B's.
        assert_eq!(settle.step(None, 2, true, true, Some(a)), Step::Wait);
        assert_eq!(settle.step(Some(b), 2, true, true, Some(a)), Step::Ask);
        assert_eq!(settle.step(Some(b), 2, true, true, Some(a)), Step::Wait);
        assert_eq!(settle.step(Some(b), 3, true, true, Some(a)), Step::Wait);
        // Quiet is not enough before the page has had its grace time.
        assert_eq!(settle.step(Some(b), 3, true, false, Some(b)), Step::Wait);
        assert_eq!(settle.step(Some(b), 3, true, true, Some(b)), Step::Done);
    }

    #[test]
    fn errors_are_contained_before_they_are_printed() {
        let hostile = "cannot load https://x.test/\x1b]52;c;AAAA\x07\u{202e}evil\n";
        let clean = sanitize(hostile);
        assert!(
            !clean.chars().any(|c| c.is_control() || c == '\u{202e}'),
            "{clean:?}"
        );
        assert!(clean.starts_with("cannot load https://x.test/"));
    }

    #[test]
    fn the_printed_page_has_no_control_or_bidi_characters_even_in_a_link() {
        use sighurt_api::semantic::{NodeId, NodeKind, SemanticNode};
        let mut root = SemanticNode {
            id: NodeId(1),
            kind: NodeKind::Document,
            children: vec![NodeId(2)],
            ..SemanticNode::default()
        };
        root.text = Some("t\u{202e}x\x1b[2J".into());
        let link = SemanticNode {
            id: NodeId(2),
            kind: NodeKind::Link,
            text: Some("go\x1b]52;c;QQ\x07\u{2066}".into()),
            href: Some("https://x.test/\x1b]52;c;QQ\x07\u{202e}evil".into()),
            ..SemanticNode::default()
        };
        let snapshot = SemanticSnapshot {
            document: DocumentGeneration::next(),
            root: NodeId(1),
            nodes: vec![root, link],
            focus: None,
            truncated: false,
        };
        let out = textmode::dump(&snapshot, 40);
        let bad = |c: char| {
            (c.is_control() && c != '\n')
                || ('\u{202a}'..='\u{202e}').contains(&c)
                || ('\u{2066}'..='\u{2069}').contains(&c)
        };
        assert!(!out.chars().any(bad), "{out:?}");
        assert!(out.contains("https://x.test/"));
    }
}
