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
use std::io::{IsTerminal, Write};
use std::os::fd::OwnedFd;
use std::process::ExitCode;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use sighurt_api::events::Cursor;
use sighurt_api::geometry::{Metrics, Scale, WindowRect};
use sighurt_api::ids::{LayoutEpoch, RequestId, TargetId, ViewId};
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
use crate::target::{TermPermit, TermTarget};
use crate::textmode;

/// How long the page's text must stay unchanged after the load ended: an extraction that was
/// already running when the load ended may still deliver an earlier state.
const SETTLE: Duration = Duration::from_millis(300);

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

    fn open_target(&mut self, _: ViewId, _: &str) -> Result<(TargetId, TermTarget), String> {
        let (target, _) = TermTarget::new(self.metrics);
        Ok((TargetId::next(), target))
    }

    fn place(&mut self, _: &BTreeMap<ViewId, ViewEntry>, _: &mut Diagnostics) -> bool {
        false
    }

    fn forget_target(&mut self, _: TargetId) {}

    fn focus(&mut self, _: KeyboardFocus, _: &mut VecDeque<BrowserEvent>) {}

    fn chrome_input(&mut self, _: InputEvent, _: &mut VecDeque<BrowserEvent>) {}

    fn chrome_command(&mut self, _: String, _: &mut VecDeque<BrowserEvent>) {}

    fn page_cursor(&mut self, _: Option<TargetId>, _: Cursor) {}

    fn page_text_input(&mut self, _: Option<TargetId>, _: Option<TextInputState>) {}

    fn notice(&mut self, _: String) {}

    fn redraw(&mut self) {}

    fn show(&mut self, _: ViewId, _: bool) {}

    fn frame_ready(&mut self, _: TargetId) {}

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
    let result = dump(options, signals);
    if let (Some(saved), Err(_) | Ok(ExitCode::FAILURE)) = (&saved, &result) {
        let _ = rustix::stdio::dup2_stderr(saved);
    }
    result
}

fn dump(options: DumpOptions, signals: &OwnedFd) -> Result<ExitCode, String> {
    let paths = Paths {
        config: Paths::from_env().config,
        session: None,
    };
    let mut registry = crate::engines::build();
    let engine = match options.open.engine.clone() {
        Some(name) => match registry.engine(&name) {
            Some(engine) if engine.capabilities.semantics => name,
            Some(_) => return Err(format!("the {name} engine gives no page text for --dump")),
            None => return Err(format!("the {name} engine is not part of this build")),
        },
        None => {
            let mut with_text = registry.engines().filter(|e| e.capabilities.semantics);
            let first = with_text.next().map(|e| e.name);
            let wpe = registry.engine("wpe").filter(|e| e.capabilities.semantics);
            wpe.map(|e| e.name)
                .or(first)
                .ok_or("this sig has no engine that gives page text (WPE WebKit does)")?
                .to_owned()
        }
    };
    let mut env = registry.environment(chrome_checks(), COLOR_NAMES);
    env.frontend = FrontendKind::Terminal;
    let config = match persistence::load_config(paths.config.as_deref(), &env) {
        Ok(config) => config,
        Err(error) => {
            log::warn!("the configuration can't be used: {error}");
            persistence::load_config(None, &env).map_err(|e| e.to_string())?
        }
    };
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
        metrics: Metrics::new(LayoutEpoch::next(), WindowRect::default(), Scale::ONE),
        snapshot: None,
        count: 0,
        last: Instant::now(),
        reads: Vec::new(),
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
        Ok(Some(snapshot)) => Some(textmode::dump(snapshot, options.width)),
        _ => None,
    };
    let code = match &outcome {
        Ok(Some(_)) => ExitCode::SUCCESS,
        Ok(None) => ExitCode::from(2),
        Err(_) => ExitCode::FAILURE,
    };
    // The text is out before the engines shut down, which may end the process.
    if let Some(text) = text {
        let mut out = std::io::stdout().lock();
        out.write_all(text.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|error| format!("cannot write the page: {error}"))?;
    }
    session.set_quit(code);
    let _ = session.shutdown(&mut headless, |_, _| {});
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
    // Once the load ended: the count of snapshots then, to see one that came after.
    let mut loaded_at: Option<u64> = None;
    loop {
        let mut byte = [0u8; 1];
        if rustix::io::read(signals, &mut byte).is_ok_and(|n| n > 0)
            && [libc::SIGTERM as u8, libc::SIGINT as u8].contains(&byte[0])
        {
            return Err("interrupted".into());
        }
        for request in std::mem::take(&mut headless.reads) {
            session.clipboard_done(request, Err(ServiceError::Unsupported));
        }
        session.service_completions(headless);
        session.pump(headless);
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
        match (loaded, loaded_at) {
            (true, None) => {
                // Ask again: whatever arrives now was extracted after the load ended.
                if let Some(view) = view {
                    session.set_semantics(view, true);
                }
                loaded_at = Some(headless.count);
            }
            (true, Some(seen)) if headless.count > seen && headless.last.elapsed() >= SETTLE => {
                if let Some((_, snapshot)) = headless.snapshot.take() {
                    return Ok(Some(snapshot));
                }
            }
            _ => {}
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        let mut until = deadline;
        if loaded_at.is_some_and(|seen| headless.count > seen) {
            until = until.min(headless.last + SETTLE);
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
