//! `sig --dump URL`: a session with no terminal that prints the page as reflowed text.
//!
//! Owns: the headless [`Frontend`] (it shows nothing and keeps the latest page snapshot), the
//! loop that opens the page, waits for it to be complete and prints [`textmode::dump`], and
//! which engine is used. A page is complete when the core says its load ended, and then the
//! snapshots the engine sends after being asked again have gone 300 ms without a newer one, or
//! 3 s have passed since the first of them (a page that keeps changing, such as a ticker, is
//! re-extracted about every 250 ms and is never quiet); the latest is printed (TUI.md §8.12).
//! It fails on the first navigation error, or after the timeout. Shutdown after
//! that is bounded ([`SHUTDOWN_BOUND`]): Servo's `Drop` sleeps until its script threads exit,
//! and a page that keeps one busy would otherwise never let `--dump` end. The session is
//! ephemeral: no session file is read or written, and no terminal, Wayland connection or
//! sighurt-ui is used. Must not own: the text layout ([`crate::textmode`]), browser policy (the
//! core) or the engines.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::IsTerminal;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::fs::OFlags;
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
use sighurt_runtime::session::{Session, SHUTDOWN_BOUND};
use sighurt_runtime::view::ViewEntry;

use crate::app::{chrome_checks, error_line, log_path, redirect_stderr, COLOR_NAMES};
use crate::sanitize::sanitize;
use crate::target::{TermPermit, TermTarget};
use crate::textmode;

/// How long the page's text must stay unchanged after the load ended: an extraction that was
/// already running when the load ended may still deliver an earlier state.
const SETTLE: Duration = Duration::from_millis(300);

/// The longest the snapshots are awaited to go quiet, from the first one after the engine was
/// asked again. Servo re-extracts a changing page about every 250 ms
/// (`sighurt_engine_servo::semantic::DEBOUNCE`), sooner than [`SETTLE`].
const MAX_SETTLE: Duration = Duration::from_secs(3);

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
/// text, is an error for the caller to print. `signals` is `sig`'s signal pipe: SIGTERM, SIGINT
/// and SIGHUP end the dump.
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
    // The core's parser: IDNA hosts and percent-encoded paths, the same ones a tab uses.
    let (name, matched) = match sighurt_core::router::routed(&fixed, config) {
        None => return Err(format!("{fixed} is not an address")),
        Some(sighurt_core::router::Routed::Internal) => {
            return Err("sig: pages are Sighurt's own and have no page text for --dump".into())
        }
        Some(sighurt_core::router::Routed::Error(message)) => return Err(message),
        Some(sighurt_core::router::Routed::Engine { engine, matched }) => (engine, matched),
    };
    if registry
        .engine(&name)
        .is_some_and(|engine| engine.capabilities.semantics)
    {
        return Ok(name);
    }
    // `matched` is false only when an http(s) or file URL fell through to the default
    // engine. That default may give no page text (a configured Wasm default); use one that
    // does.
    if !matched {
        return engine_with_text(registry);
    }
    match registry.engine(&name) {
        Some(_) => Err(format!(
            "the {name} engine gives no page text for --dump; pass --engine wpe"
        )),
        None => Err(format!("the {name} engine is not part of this build")),
    }
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

fn dump(options: DumpOptions, signals: &OwnedFd) -> Result<ExitCode, String> {
    let paths = Paths {
        config: Paths::from_env().config,
        session: None,
    };
    let mut registry = crate::engines::build();
    let mut env = registry.environment(chrome_checks(), COLOR_NAMES);
    env.frontend = FrontendKind::Terminal;
    // A configuration file that can't be used stops the dump; its lines become one.
    let config = persistence::startup_config(paths.config.as_deref(), &env)
        .map_err(|error| error.replace('\n', " "))?;
    // Routing, not "WPE first": a demo: URL configured for the demo engine must not be handed
    // to WPE. TUI.md still asks for WPE when the page is an ordinary http(s) or file URL whose
    // only claim is the default engine and that engine gives no page text.
    let engine = choose_dump_engine(&registry, &config, env.home_dir.as_deref(), &options.open)?;
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
    // One deadline for the load and for writing the page. The write used to start
    // `options.timeout` over again once the text was ready.
    let deadline = Instant::now() + options.timeout;
    let outcome = wait(
        &mut session,
        &mut headless,
        &wake,
        &options,
        signals,
        deadline,
    );
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
        written = write_out(text.as_bytes(), signals, deadline);
        match written {
            Ok(true) => {}
            Ok(false) => code = ExitCode::from(2),
            Err(_) => code = ExitCode::FAILURE,
        }
    }
    // Before shutdown: it can `_exit` when the bound passes, and these lines would not run.
    match &written {
        Ok(false) => eprintln!("sig: timed out writing the page: nothing reads stdout"),
        Ok(true) => {
            if matches!(outcome, Ok(None)) {
                eprintln!(
                    "sig: timed out after {} s waiting for the page's text",
                    options.timeout.as_secs()
                );
            }
        }
        Err(_) => {}
    }
    let _ = std::io::Write::flush(&mut std::io::stdout());
    let _ = std::io::Write::flush(&mut std::io::stderr());
    session.set_quit(code);
    session.set_shutdown_bound(SHUTDOWN_BOUND);
    let _ = session.shutdown(&mut headless, |_, _| {});
    match written {
        Ok(_) => match outcome {
            Ok(_) => Ok(code),
            Err(error) => Err(error),
        },
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

/// Writes `bytes` to stdout without ever blocking on it. Gives up at `deadline`
/// (`Ok(false)`) or on SIGTERM, SIGINT and SIGHUP. `deadline` is the dump's one timeout, not a
/// fresh one for the write.
fn write_out(bytes: &[u8], signals: &OwnedFd, deadline: Instant) -> Result<bool, String> {
    let stdout = std::io::stdout();
    write_fd(stdout.as_fd(), bytes, signals, deadline)
}

/// Writes `bytes` to `fd`. The fd is non-blocking for the loop and its previous flags are
/// put back afterwards, including when the write fails. `EAGAIN` means poll again: another
/// writer can fill a shared pipe after `POLLOUT` and before `write`, and a blocking write
/// of 4096 bytes then waits forever.
fn write_fd(
    fd: BorrowedFd<'_>,
    bytes: &[u8],
    signals: &OwnedFd,
    deadline: Instant,
) -> Result<bool, String> {
    if bytes.is_empty() {
        return Ok(true);
    }
    if Instant::now() >= deadline {
        return Ok(false);
    }
    let original = rustix::fs::fcntl_getfl(fd)
        .map_err(|error| format!("cannot read stdout flags: {error}"))?;
    rustix::fs::fcntl_setfl(fd, original | OFlags::NONBLOCK)
        .map_err(|error| format!("cannot set stdout non-blocking: {error}"))?;
    let mut restore = RestoreFlags {
        fd,
        flags: original,
        pending: true,
    };
    let written = write_nonblocking(fd, bytes, signals, deadline);
    match (written, restore.restore()) {
        (Ok(done), Ok(())) => Ok(done),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(write), Err(flags)) => Err(format!("{write} ({flags})")),
    }
}

/// Puts `fd`'s status flags back. [`Drop`] covers a panic or an early return.
struct RestoreFlags<'a> {
    fd: BorrowedFd<'a>,
    flags: OFlags,
    pending: bool,
}

impl RestoreFlags<'_> {
    fn restore(&mut self) -> Result<(), String> {
        if !self.pending {
            return Ok(());
        }
        self.pending = false;
        rustix::fs::fcntl_setfl(self.fd, self.flags)
            .map_err(|error| format!("cannot restore stdout flags: {error}"))
    }
}

impl Drop for RestoreFlags<'_> {
    fn drop(&mut self) {
        if self.pending {
            let _ = rustix::fs::fcntl_setfl(self.fd, self.flags);
            self.pending = false;
        }
    }
}

/// `CHUNK` is `PIPE_BUF`. A pipe reports `POLLOUT` when any byte fits, so a write of that
/// size can still return `EAGAIN`; the next try is one byte, which fits whenever `POLLOUT`
/// is set, and `poll` blocks once the pipe is actually full.
fn write_nonblocking(
    fd: BorrowedFd<'_>,
    bytes: &[u8],
    signals: &OwnedFd,
    deadline: Instant,
) -> Result<bool, String> {
    const CHUNK: usize = 4096;
    let mut rest = bytes;
    let mut limit = CHUNK;
    while !rest.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(false);
        }
        let mut fds = [
            PollFd::new(&fd, PollFlags::OUT),
            PollFd::new(signals, PollFlags::IN),
        ];
        let spec = Timespec::try_from(left).map_err(|error| error.to_string())?;
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
        let chunk = &rest[..rest.len().min(limit)];
        match rustix::io::write(fd, chunk) {
            Ok(0) => {}
            Ok(n) => {
                rest = &rest[n..];
                limit = CHUNK;
            }
            Err(Errno::AGAIN) if limit > 1 => limit = 1,
            Err(Errno::AGAIN | Errno::INTR) => {}
            Err(error) => return Err(format!("cannot write the page: {error}")),
        }
    }
    Ok(true)
}

/// Whether the byte `sig`'s signal pipe carries is a request to quit.
fn is_quit(byte: u8) -> bool {
    [libc::SIGTERM as u8, libc::SIGINT as u8, libc::SIGHUP as u8].contains(&byte)
}

/// Where a document is on its way to being printed: which one the snapshot count was taken at
/// when the engine was asked again after its load ended, so that a snapshot of an earlier
/// document, or from before the load ended, is never taken for it.
#[derive(Default)]
struct Settle {
    asked: Option<(DocumentGeneration, u64)>,
    /// When the first snapshot after asking was seen.
    first: Option<Instant>,
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
            self.first = None;
            return Step::Wait;
        };
        match self.asked {
            Some((asked, _)) if asked == document => {}
            _ => {
                self.asked = Some((document, count));
                self.first = None;
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

    /// When the snapshots will count as quiet: [`SETTLE`] after the latest (`last`), or
    /// [`MAX_SETTLE`] after the first one after asking, whichever is sooner. `None` until such
    /// a snapshot has arrived; the first time one has, `now` is taken as its arrival.
    fn quiet_at(&mut self, count: u64, last: Instant, now: Instant) -> Option<Instant> {
        if !self.settling(count) {
            return None;
        }
        let first = *self.first.get_or_insert(now);
        Some((last + SETTLE).min(first + MAX_SETTLE))
    }
}

/// How long to sleep. `None` means a pump is already due, so the loop runs again without
/// sleeping. Otherwise the soonest deadline that is still ahead of `now`, and at most 500 ms
/// so the signal pipe is checked. A deadline that has passed does not count: once the quiet
/// period has expired, waiting out [`GRACE`] or [`GRACE_THIN`] must not spin.
fn sleep_for(
    now: Instant,
    deadline: Instant,
    quiet_at: Option<Instant>,
    grace_from: Option<Instant>,
    pump_at: Option<Option<Instant>>,
) -> Option<Duration> {
    match pump_at {
        Some(None) => return None,
        Some(Some(at)) if at <= now => return None,
        _ => {}
    }
    let mut until = deadline;
    for at in quiet_at.into_iter().chain(pump_at.flatten()) {
        if at > now {
            until = until.min(at);
        }
    }
    if let Some(at) = grace_from {
        for at in [at + GRACE, at + GRACE_THIN] {
            if at > now {
                until = until.min(at);
            }
        }
    }
    Some(
        until
            .saturating_duration_since(now)
            .min(Duration::from_millis(500)),
    )
}

/// Drives the session until the page's snapshot is in: `Some`, or `None` at `deadline`.
fn wait(
    session: &mut Session<Headless>,
    headless: &mut Headless,
    wake: &Wake,
    options: &DumpOptions,
    signals: &OwnedFd,
    deadline: Instant,
) -> Result<Option<Box<SemanticSnapshot>>, String> {
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
        let checked = Instant::now();
        let quiet = settle
            .quiet_at(headless.count, headless.last, checked)
            .is_some_and(|at| at <= checked);
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
        let quiet_at = settle.quiet_at(headless.count, headless.last, now);
        let grace_from = loaded_at.filter(|_| document.is_some());
        let Some(pause) = sleep_for(
            now,
            deadline,
            quiet_at,
            grace_from,
            session.registry().next_pump(),
        ) else {
            continue;
        };
        wake.wait(pause);
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

    /// A page whose snapshots never stop (a ticker, re-extracted every 250 ms) still settles:
    /// [`MAX_SETTLE`] after the first snapshot that followed the question.
    #[test]
    fn snapshots_that_never_go_quiet_settle_after_the_cap() {
        let a = DocumentGeneration::next();
        let mut settle = Settle::default();
        let start = Instant::now();
        assert_eq!(settle.step(Some(a), 1, false, true, Some(a)), Step::Ask);
        assert_eq!(settle.quiet_at(1, start, start), None);
        // Snapshots every 250 ms from the first: SETTLE after the latest is never reached.
        let mut count = 1;
        let mut now = start;
        let mut done_at = None;
        while now < start + Duration::from_secs(10) {
            count += 1;
            let last = now;
            let quiet = settle
                .quiet_at(count, last, now)
                .is_some_and(|at| at <= now);
            if settle.step(Some(a), count, quiet, true, Some(a)) == Step::Done {
                done_at = Some(now);
                break;
            }
            now += Duration::from_millis(250);
        }
        let done_at = done_at.expect("never settled");
        assert_eq!(done_at - start, MAX_SETTLE);
        // A page that goes quiet settles SETTLE after its latest snapshot, before the cap.
        let mut settle = Settle::default();
        assert_eq!(settle.step(Some(a), 1, false, true, Some(a)), Step::Ask);
        let first = start;
        assert_eq!(settle.quiet_at(2, first, first), Some(first + SETTLE));
        let last = first + Duration::from_millis(200);
        assert_eq!(settle.quiet_at(3, last, last), Some(last + SETTLE));
        // A new document starts over.
        let b = DocumentGeneration::next();
        assert_eq!(settle.step(Some(b), 3, false, true, Some(b)), Step::Ask);
        let later = start + Duration::from_secs(5);
        assert_eq!(settle.quiet_at(3, later, later), None);
        assert_eq!(settle.quiet_at(4, later, later), Some(later + SETTLE));
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

    #[test]
    fn an_expired_quiet_period_does_not_busy_loop_through_grace() {
        let now = Instant::now();
        let loaded = now - Duration::from_millis(400);
        // Quiet expired at this instant. Grace still has 1.1 s; the wait is the 500 ms cap.
        let pause = sleep_for(
            now,
            now + Duration::from_secs(20),
            Some(now),
            Some(loaded),
            None,
        )
        .expect("grace is still ahead");
        assert_eq!(pause, Duration::from_millis(500), "{pause:?}");

        // Quiet still ahead: wait for it, not the longer grace.
        let pause = sleep_for(
            now,
            now + Duration::from_secs(20),
            Some(now + Duration::from_millis(200)),
            Some(loaded),
            None,
        )
        .unwrap();
        assert_eq!(pause, Duration::from_millis(200));

        // Grace sooner than the cap, quiet already over.
        let loaded = now - GRACE + Duration::from_millis(120);
        let pause = sleep_for(
            now,
            now + Duration::from_secs(20),
            Some(now - Duration::from_secs(1)),
            Some(loaded),
            None,
        )
        .unwrap();
        assert_eq!(pause, Duration::from_millis(120));

        let pause = sleep_for(now, now + Duration::from_millis(80), None, None, None).unwrap();
        assert_eq!(pause, Duration::from_millis(80));

        // A pump that is already due is not a zero-length sleep.
        assert!(sleep_for(
            now,
            now + Duration::from_secs(20),
            Some(now - Duration::from_millis(1)),
            Some(loaded),
            Some(None),
        )
        .is_none());
        assert!(sleep_for(
            now,
            now + Duration::from_secs(20),
            None,
            None,
            Some(Some(now))
        )
        .is_none());
        let pause = sleep_for(
            now,
            now + Duration::from_secs(20),
            None,
            None,
            Some(Some(now + Duration::from_millis(40))),
        )
        .unwrap();
        assert_eq!(pause, Duration::from_millis(40));
    }

    /// A pipe with one byte of room: `poll` reports `POLLOUT`, and a blocking write of
    /// 4096 bytes never returns.
    fn leave_one_byte_free(reader: &OwnedFd, writer: &OwnedFd) {
        let flags = rustix::fs::fcntl_getfl(writer).unwrap();
        rustix::fs::fcntl_setfl(writer, flags | OFlags::NONBLOCK).unwrap();
        let byte = [0u8; 1];
        loop {
            match rustix::io::write(writer, &byte) {
                Ok(1) => {}
                Err(Errno::AGAIN) => break,
                other => panic!("filling the pipe: {other:?}"),
            }
        }
        let mut one = [0u8; 1];
        assert_eq!(rustix::io::read(reader, &mut one).unwrap(), 1);
        rustix::fs::fcntl_setfl(writer, flags).unwrap();
    }

    fn signal_pipe() -> (OwnedFd, std::io::PipeWriter) {
        let (read, write) = std::io::pipe().unwrap();
        (read.into(), write)
    }

    #[test]
    fn a_4096_byte_write_hits_the_deadline_when_the_pipe_has_room_for_less() {
        let (reader, writer) = std::io::pipe().unwrap();
        let reader = OwnedFd::from(reader);
        let writer = OwnedFd::from(writer);
        let flags = rustix::fs::fcntl_getfl(&writer).unwrap();
        assert!(!flags.contains(OFlags::NONBLOCK), "{flags:?}");
        leave_one_byte_free(&reader, &writer);
        let (signals, _keep) = signal_pipe();
        let (tx, rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        std::thread::spawn(move || {
            let result = write_fd(
                writer.as_fd(),
                &[0u8; 4096],
                &signals,
                Instant::now() + Duration::from_millis(200),
            );
            let after = rustix::fs::fcntl_getfl(&writer).unwrap();
            let _ = tx.send((result, after));
        });
        let (result, after) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("the write blocked; stdout was still blocking");
        assert!(!result.unwrap());
        assert_eq!(after, flags);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn stdout_flags_are_restored_when_the_write_fails() {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let writer = OwnedFd::from(writer);
        let flags = rustix::fs::fcntl_getfl(&writer).unwrap();
        let (signals, _keep) = signal_pipe();
        let error = write_fd(
            writer.as_fd(),
            b"hello",
            &signals,
            Instant::now() + Duration::from_secs(2),
        )
        .unwrap_err();
        assert!(error.contains("cannot write"), "{error}");
        assert_eq!(rustix::fs::fcntl_getfl(&writer).unwrap(), flags);
    }

    #[test]
    fn an_expired_deadline_writes_nothing() {
        let (reader, writer) = std::io::pipe().unwrap();
        let writer = OwnedFd::from(writer);
        let (signals, _keep) = signal_pipe();
        let start = Instant::now();
        let wrote = write_fd(
            writer.as_fd(),
            b"hello",
            &signals,
            start - Duration::from_millis(1),
        )
        .unwrap();
        assert!(!wrote);
        assert!(start.elapsed() < Duration::from_millis(200));
        let reader = OwnedFd::from(reader);
        let flags = rustix::fs::fcntl_getfl(&reader).unwrap();
        rustix::fs::fcntl_setfl(&reader, flags | OFlags::NONBLOCK).unwrap();
        let mut buf = [0u8; 8];
        assert_eq!(
            rustix::io::read(&reader, &mut buf).unwrap_err(),
            Errno::AGAIN
        );
    }

    #[cfg(all(feature = "engine-demo", feature = "engine-wasm"))]
    #[test]
    fn dump_routing_uses_the_cores_url_parser() {
        let registry = crate::engines::build();
        let mut env = registry.environment(chrome_checks(), COLOR_NAMES);
        env.frontend = FrontendKind::Terminal;
        let config = sighurt_core::config::Config::parse(
            Some("version = 1\n[routing.wasm]\nhosts = [\"bücher.de\"]\n"),
            &env,
        )
        .unwrap_or_else(|error| panic!("{error}"));
        let choose = |input: &str| {
            choose_dump_engine(
                &registry,
                &config,
                None,
                &OpenRequest {
                    input: input.into(),
                    engine: None,
                },
            )
        };
        // IDNA: the host rule matches, and wasm gives no page text.
        let unicode = choose("https://Bücher.de/").unwrap_err();
        assert!(
            unicode.contains("wasm") && unicode.contains("page text"),
            "{unicode}"
        );
        let puny = choose("https://xn--bcher-kva.de/a").unwrap_err();
        assert!(
            puny.contains("wasm") && puny.contains("page text"),
            "{puny}"
        );
        // `%77` is `w`, but the core keeps the encoding, so this is not a wasm file.
        let encoded = choose("file:///tmp/app.%77asm").unwrap();
        assert_eq!(encoded, engine_with_text(&registry).unwrap());
        let wasm = choose("file:///tmp/app.wasm").unwrap_err();
        assert!(
            wasm.contains("wasm") && wasm.contains("page text"),
            "{wasm}"
        );
    }
}
