//! The Wasm engine's [`EngineRuntime`]: one runtime serves every Wasm view of the browser.
//!
//! The owner thread (the browser's event loop) calls the runtime; guests never run on it.
//! Loads (fetch, compile, `start_app`) run on a small loader pool and guest steps (`on_event`,
//! `on_timer`, `on_frame`) on a guest pool, both a fixed number of threads, with at most one
//! job per guest in flight, so a guest's calls stay in order and busy guests share the pool in
//! turn. A step job queues the guest's next step itself ([`crate::live`]), so a guest's
//! cadence costs the owner nothing. Every result the browser can see comes back through a
//! channel, tagged with its view and its navigation or document generation, and wakes the owner
//! through the [`EngineWaker`]; [`pump`] applies at most [`MAX_COMPLETIONS_PER_PUMP`] of them
//! and discards those whose view was closed or whose document was replaced. No guest callback,
//! and no wait for one, ever blocks the owner.
//!
//! What a closed or replaced document leaves (its guest, its host state with the connections,
//! audio and runtimes in it) is dropped on a thread of its own, since dropping some of it
//! waits; so is a closed view, before the owner drops its target.
//!
//! Presenting is "paint the latest display list into this buffer while the host's permit
//! allows it": [`present`] rasterizes into memory the view's [`CpuTarget`] lends for that one
//! frame, a large frame in bands on helper threads while the owner paints one of them and
//! waits for the rest (`raster.rs`). [`CpuTarget`] is
//! [`sighurt_api::present::CpuTarget`](sighurt_api::present::CpuTarget). `sig` gives views the
//! Wayland host's shared-memory target lease, which implements it in `sighurt-wayland`.
//!
//! Owns: the views, the pools and the completion channel. Must not own: browser policy (which
//! URL opens where, permissions beyond the guest's own prompt) or native surfaces.
//!
//! [`pump`]: EngineRuntime::pump
//! [`present`]: EngineRuntime::present

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use sighurt_api::engine::{
    EngineCapabilities, EngineCommand, EngineError, EngineRuntime, EngineWaker, PresentOutcome,
    PumpStatus, ViewOptions,
};
use sighurt_api::events::EngineEvent;
use sighurt_api::ids::{DocumentGeneration, NavigationId, ViewId};
use sighurt_api::present::CpuTarget;

use crate::capabilities::{ConsoleEntry, ConsoleLevel, HostState};
use crate::draw::DisplayList;
use crate::engine::{WasmConfig, WasmEngine};
use crate::live::{LiveGuest, Seen, Step};
use crate::pool::{Pool, Spawner};
use crate::runtime::{self, Loaded};
use crate::view::{Jobs, Outbox, Readiness, View};

/// Most finished loads and steps one [`EngineRuntime::pump`] applies.
pub const MAX_COMPLETIONS_PER_PUMP: usize = 64;
/// Threads that load documents. Loads wait on the network, so they don't share threads with
/// the guests that are running.
const LOADERS: usize = 2;

/// The Wasm engine's runtime, serving views that present into `T`.
///
/// `T` is a [`CpuTarget`]. `sig` uses the Wayland host's shared-memory target lease, which
/// implements it in `sighurt-wayland` with the host's frame permit; tests use an in-memory
/// target. The runtime drops a view's target on the owner thread when the view closes or the
/// runtime shuts down, after it has stopped the view's guest; the guest's state, which doesn't
/// refer to the target, is dropped on another thread.
pub struct WasmRuntime<T: CpuTarget> {
    engine: WasmEngine,
    views: HashMap<ViewId, (View, T)>,
    /// `None` once shut down.
    workers: Option<Workers>,
    done: Receiver<Done>,
    outbox: Outbox,
}

/// A finished job the owner must hear of.
enum Done {
    Loaded {
        view: ViewId,
        navigation: NavigationId,
        result: Result<Loaded, String>,
    },
    /// A step that changed what the browser shows.
    Stepped {
        view: ViewId,
        document: DocumentGeneration,
        list: Option<Arc<DisplayList>>,
        /// Why the step failed, ending the guest.
        error: Option<String>,
    },
}

/// The pools, and how their jobs report back.
struct Workers {
    engine: WasmEngine,
    loads: Pool,
    guests: Pool,
    /// Drops what closed and replaced documents leave.
    reaper: Pool,
    report: Report,
}

/// How jobs report to the owner.
#[derive(Clone)]
struct Report {
    done: Sender<Done>,
    waker: EngineWaker,
}

impl Report {
    fn send(&self, done: Done) {
        let _ = self.done.send(done);
        self.waker.wake();
    }
}

impl<T: CpuTarget> WasmRuntime<T> {
    /// Starts the runtime: the shared Wasmtime engine, the epoch ticker and the worker pools.
    /// `waker` is how finished work gets the owner loop to pump.
    pub fn new(config: WasmConfig, waker: EngineWaker) -> Result<Self, EngineError> {
        let failed = |e: &dyn std::fmt::Display| EngineError::Runtime(format!("{e:#}"));
        let engine = WasmEngine::new(config).map_err(|e| failed(&e))?;
        // Leave the owner and the compositor a core; a few guests at a time are enough.
        let threads = std::thread::available_parallelism()
            .map_or(2, |n| n.get().saturating_sub(1))
            .clamp(1, 4);
        let (done, done_rx) = mpsc::channel();
        let workers = Workers {
            engine: engine.clone(),
            loads: Pool::new("sighurt-wasm-load", LOADERS).map_err(|e| failed(&e))?,
            guests: Pool::new("sighurt-wasm-guest", threads).map_err(|e| failed(&e))?,
            reaper: Pool::new("sighurt-wasm-drop", 1).map_err(|e| failed(&e))?,
            report: Report { done, waker },
        };
        Ok(Self {
            engine,
            views: HashMap::new(),
            workers: Some(workers),
            done: done_rx,
            outbox: Outbox::default(),
        })
    }

    fn running(&self) -> Result<&Workers, EngineError> {
        self.workers
            .as_ref()
            .ok_or_else(|| EngineError::Runtime("the Wasm runtime was shut down".into()))
    }
}

/// Applies a finished job to its view, if the view is still open.
fn apply<T>(
    views: &mut HashMap<ViewId, (View, T)>,
    done: Done,
    workers: &Workers,
    out: &mut Outbox,
) {
    match done {
        Done::Loaded {
            view,
            navigation,
            result,
        } => match views.get_mut(&view) {
            Some((view, _)) => view.loaded(navigation, result, workers, out),
            None => workers.retire(Box::new(result)),
        },
        Done::Stepped {
            view,
            document,
            list,
            error,
        } => {
            if let Some((view, _)) = views.get_mut(&view) {
                view.stepped(document, list, error, workers, out);
            }
        }
    }
}

impl<T: CpuTarget> EngineRuntime for WasmRuntime<T> {
    type Target = T;
    type FramePermit = T::Permit;

    fn capabilities(&self) -> EngineCapabilities {
        EngineCapabilities {
            history_traversal: true,
            page_zoom: true,
            ..EngineCapabilities::default()
        }
    }

    fn open_view(
        &mut self,
        view: ViewId,
        options: ViewOptions,
        target: T,
    ) -> Result<(), EngineError> {
        self.running()?;
        if self.views.contains_key(&view) {
            let message = "a view with this id is already open".into();
            return Err(EngineError::View { view, message });
        }
        let state = View::new(view, self.engine.clone(), target.metrics(), options);
        self.views.insert(view, (state, target));
        // A view shows nothing until it navigates, and can present that right away.
        self.outbox.push(EngineEvent::ViewReady { view });
        Ok(())
    }

    fn command(&mut self, view: ViewId, command: EngineCommand) -> Result<(), EngineError> {
        let workers = self
            .workers
            .as_ref()
            .ok_or_else(|| EngineError::Runtime("the Wasm runtime was shut down".into()))?;
        let (state, _) = self
            .views
            .get_mut(&view)
            .ok_or(EngineError::UnknownView(view))?;
        state.command(command, workers, &mut self.outbox)
    }

    fn pump(&mut self, events: &mut Vec<EngineEvent>) -> PumpStatus {
        let Some(workers) = &self.workers else {
            return PumpStatus::Idle;
        };
        let mut applied = 0;
        while applied < MAX_COMPLETIONS_PER_PUMP {
            let Ok(done) = self.done.try_recv() else {
                break;
            };
            apply(&mut self.views, done, workers, &mut self.outbox);
            applied += 1;
        }
        events.extend(self.outbox.take());
        if applied == MAX_COMPLETIONS_PER_PUMP {
            PumpStatus::Pending { deadline: None }
        } else {
            PumpStatus::Idle
        }
    }

    fn present(&mut self, view: ViewId, permit: T::Permit) -> Result<PresentOutcome, EngineError> {
        self.running()?;
        let (state, target) = self
            .views
            .get_mut(&view)
            .ok_or(EngineError::UnknownView(view))?;
        match state.readiness(T::permit_epoch(&permit)) {
            Readiness::NotReady => Ok(PresentOutcome::NotReady),
            Readiness::Unchanged => Ok(PresentOutcome::Unchanged),
            Readiness::Paint => {
                target
                    .submit(permit, &mut |frame| state.paint(frame))
                    .map_err(|message| EngineError::View { view, message })?;
                Ok(PresentOutcome::Submitted)
            }
        }
    }

    fn close_view(&mut self, view: ViewId) {
        let Some((state, target)) = self.views.remove(&view) else {
            return;
        };
        match &self.workers {
            // The guest's state stops, and is dropped off this thread; nothing of it refers
            // to the target, which goes now.
            Some(workers) => state.close(workers),
            None => drop(state),
        }
        drop(target);
        self.outbox.push(EngineEvent::ViewClosed { view });
    }

    fn shutdown(&mut self) {
        let Some(workers) = self.workers.take() else {
            return;
        };
        for (_, (state, target)) in self.views.drain() {
            state.close(&workers);
            drop(target);
        }
        self.outbox.take();
        // The pools' threads drop the jobs still queued, and the reaper what it was given;
        // running jobs end within their guest's limits, their guests cancelled, and their
        // results are dropped with the channel. Workers hold no native resources.
        drop(workers);
    }
}

impl Jobs for Workers {
    fn load(&self, view: ViewId, navigation: NavigationId, state: HostState, url: String) {
        let (engine, report) = (self.engine.clone(), self.report.clone());
        self.loads.spawn(move || {
            if state.budget.is_cancelled() {
                return;
            }
            let result =
                catch(|| runtime::load(&engine, &state, &url).map_err(|e| format!("{e:#}")))
                    .and_then(|result| result);
            log_console(view, &state.console);
            // A superseded load is dropped here, off the owner thread.
            if !state.budget.is_cancelled() {
                report.send(Done::Loaded {
                    view,
                    navigation,
                    result,
                });
            }
        });
    }

    fn run(&self, live: &Arc<LiveGuest>, at: Instant, serial: u64) {
        let (live, report, guests) = (live.clone(), self.report.clone(), self.guests.spawner());
        self.guests
            .spawner()
            .spawn_at(at, move || step(live, serial, report, guests));
    }

    fn retire(&self, garbage: Box<dyn Send>) {
        self.reaper.spawn(move || drop(garbage));
    }
}

/// Runs the step of `live` with `serial`, tells the owner if it changed anything the browser
/// shows, and queues the next one.
fn step(live: Arc<LiveGuest>, serial: u64, report: Report, guests: Spawner) {
    let Some(Step {
        mut guest,
        input,
        dt_ms,
    }) = live.begin(serial)
    else {
        return;
    };
    let console = guest.host_state().console.clone();
    let result = catch(|| guest.step(dt_ms, input).map_err(|e| format!("{e:#}")));
    log_console(live.view, &console);
    let (guest, list, error) = match result.and_then(|result| result) {
        Ok(list) => (Some(guest), list, None),
        // The guest is dropped here, off the owner thread.
        Err(error) => (None, None, Some(error)),
    };
    let seen = guest.as_deref().map(Seen::of).unwrap_or_default();
    let navigates = guest.as_deref().is_some_and(|guest| {
        let requested = guest.host_state().pending_navigation.lock();
        requested.unwrap_or_else(PoisonError::into_inner).is_some()
    });
    let changed = list.is_some() || error.is_some() || navigates;
    let after = live.finish(guest, seen, changed);
    if after.notify {
        report.send(Done::Stepped {
            view: live.view,
            document: live.document,
            list,
            error,
        });
    }
    if let Some((at, serial)) = after.next {
        let next = guests.clone();
        guests.spawn_at(at, move || step(live, serial, report, next));
    }
}

/// Runs `f`, turning a panic (a bug in a host function) into an error for its view.
fn catch<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| panic.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        format!("the Wasm engine failed: {message}")
    })
}

/// Hands the guest's console lines to the log. They are page data, so they go to the debug
/// level only.
fn log_console(view: ViewId, console: &Mutex<Vec<ConsoleEntry>>) {
    let lines = std::mem::take(&mut *console.lock().unwrap_or_else(PoisonError::into_inner));
    for ConsoleEntry { level, message } in lines {
        let level = match level {
            ConsoleLevel::Log => "log",
            ConsoleLevel::Warn => "warn",
            ConsoleLevel::Error => "error",
        };
        tracing::debug!(target: "sighurt::wasm::console", ?view, level, "{message}");
    }
}
