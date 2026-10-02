//! The engines compiled into this build, and their runtimes.
//!
//! Cargo features decide which engines exist (`engine-servo`, `engine-wasm`, `engine-demo`);
//! runtime configuration decides which of them may open new views (the core checks it). A
//! runtime is created on first use, one per engine kind, with the [`EngineWaker`] it needs, and
//! its `Target` and `FramePermit` bound to `sighurt_wayland`'s types. The registry also knows
//! what each engine supports before its runtime exists, since the core needs that from the
//! start, and how its views present (shared memory or GL), since the host creates their targets.
//!
//! Each runtime remembers why it must be pumped: it was called this iteration, its waker fired,
//! or its pump deadline passed. The registry also remembers a runtime that its `shutdown` had
//! to leave running (Servo after a page crash or a lost backend, decision 54), so that `sig`
//! knows to end with `_exit`.
//!
//! Servo is the exception to starting on first use (decision 26): when it is compiled in and
//! enabled, it starts with `sig`, because its logger must be the process's one `log` logger
//! (the only way script-thread panics reach Servo) and Servo can be built only once per
//! process. Without it, `sig` installs `env_logger`; a Servo started later, by a configuration
//! that enables it, then logs through that one ([`install_logger`]).
//!
//! Must not own: routing policy (the core decides which engine opens a URL).

use std::collections::BTreeMap;
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use sighurt_api::engine::{EngineCapabilities, EngineRuntime, EngineWaker, PumpStatus};
use sighurt_core::config::Config;
use sighurt_wayland::{FramePermit, PresentMode, TargetLease};

/// The contract every engine's runtime implements, bound to the host's types.
pub type Contract = dyn EngineRuntime<Target = TargetLease, FramePermit = FramePermit>;

/// A running engine: the contract, and the concrete runtime for what only it can tell
/// ([`Runtime::renderer`], [`Runtime::readbacks`]).
pub enum Runtime {
    #[cfg(feature = "engine-servo")]
    Servo(Box<sighurt_engine_servo::ServoRuntime>),
    #[cfg(feature = "engine-wasm")]
    Wasm(Box<sighurt_engine_wasm::WasmRuntime<TargetLease>>),
    #[cfg(feature = "engine-demo")]
    Demo(Box<sighurt_engine_demo::DemoRuntime>),
}

impl Deref for Runtime {
    type Target = Contract;

    fn deref(&self) -> &Contract {
        match *self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(ref runtime) => &**runtime,
            #[cfg(feature = "engine-wasm")]
            Self::Wasm(ref runtime) => &**runtime,
            #[cfg(feature = "engine-demo")]
            Self::Demo(ref runtime) => &**runtime,
        }
    }
}

impl DerefMut for Runtime {
    fn deref_mut(&mut self) -> &mut Contract {
        match *self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(ref mut runtime) => &mut **runtime,
            #[cfg(feature = "engine-wasm")]
            Self::Wasm(ref mut runtime) => &mut **runtime,
            #[cfg(feature = "engine-demo")]
            Self::Demo(ref mut runtime) => &mut **runtime,
        }
    }
}

impl Runtime {
    /// How it renders, once it knows: "cpu" for the CPU engines; for Servo, after its first
    /// view has a context, "hardware" or "software" (a software rasterizer: the diagnosed
    /// fallback) and the GL renderer.
    pub fn renderer(&self) -> Option<(&'static str, Option<String>)> {
        match *self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(ref runtime) => runtime.diagnostics().gl.map(|gl| {
                let mode = if gl.software { "software" } else { "hardware" };
                (mode, Some(gl.renderer))
            }),
            #[allow(unreachable_patterns)]
            _ => Some(("cpu", None)),
        }
    }

    /// Page readbacks to the CPU the engine made: Servo counts `read_to_image` and, when
    /// auditing, GL reads. 0 for the CPU engines, which paint into the host's buffers.
    pub fn readbacks(&self) -> u64 {
        match *self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(ref runtime) => {
                let d = runtime.diagnostics();
                d.read_to_image + d.gl_reads.unwrap_or(0)
            }
            #[allow(unreachable_patterns)]
            _ => 0,
        }
    }

    /// How long the engine itself took to paint the frame it last submitted, when it times
    /// that apart from presenting: Servo's `WebView::paint` (WebRender drawing the frame),
    /// without its spin and swap. `None` for the CPU engines, whose `present` is the painting.
    pub fn paint_time(&self) -> Option<Duration> {
        match *self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(ref runtime) => Some(runtime.diagnostics().timing.paint),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// The GL reads among [`Runtime::readbacks`], when the engine audits its GL calls (Servo
    /// with `SIG_AUDIT_GL=1`).
    pub fn gl_reads(&self) -> Option<u64> {
        match *self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(ref runtime) => runtime.diagnostics().gl_reads,
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Whether `shutdown` had to leave the engine running: Servo after a page crash or a lost
    /// backend (decision 54). Its threads still use GL state on the host's connection, so the
    /// process must end without exit handlers.
    fn leaked(&self) -> bool {
        match *self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(ref runtime) => runtime.diagnostics().leaked,
            #[allow(unreachable_patterns)]
            _ => false,
        }
    }
}

/// Whether the process's `log` logger is installed.
static LOGGER: AtomicBool = AtomicBool::new(false);

/// Installs `env_logger` as the process's logger, unless Servo's is installed already.
pub fn install_logger() {
    if !LOGGER.swap(true, Ordering::SeqCst) {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    }
}

/// One compiled-in engine.
pub struct Engine {
    /// Its name in the configuration.
    pub name: &'static str,
    /// What it supports; the core needs this before the runtime exists.
    pub capabilities: EngineCapabilities,
    /// How its views present.
    pub mode: PresentMode,
    /// How it renders, for diagnostics and the home page.
    pub renderer: &'static str,
    start: fn(EngineWaker) -> Result<Runtime, String>,
    state: State,
    waker: Option<EngineWaker>,
    /// Called since its last pump.
    called: bool,
    /// When its last pump asked to be pumped again.
    due: Option<PumpDue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PumpDue {
    Now,
    At(Instant),
}

/// Where a runtime is in its life.
pub enum State {
    /// Not needed yet.
    #[cfg_attr(
        not(any(
            feature = "engine-servo",
            feature = "engine-wasm",
            feature = "engine-demo"
        )),
        allow(dead_code)
    )]
    NotStarted,
    /// Running.
    Running(Runtime),
    /// Starting it failed, or it reported `RuntimeFailed`: why.
    Failed(String),
    /// `EngineRuntime::shutdown` was called.
    ShutDown,
}

impl Engine {
    #[cfg_attr(
        not(any(
            feature = "engine-servo",
            feature = "engine-wasm",
            feature = "engine-demo"
        )),
        allow(dead_code)
    )]
    fn new(
        name: &'static str,
        capabilities: EngineCapabilities,
        mode: PresentMode,
        renderer: &'static str,
        start: fn(EngineWaker) -> Result<Runtime, String>,
    ) -> Self {
        Self {
            name,
            capabilities,
            mode,
            renderer,
            start,
            state: State::NotStarted,
            waker: None,
            called: false,
            due: None,
        }
    }

    /// Where the runtime is in its life.
    pub fn state(&self) -> &State {
        &self.state
    }
}

/// Every compiled-in engine.
pub struct Registry {
    engines: Vec<Engine>,
    /// A runtime that was shut down is still running ([`Runtime::leaked`]).
    leaked: bool,
}

impl Registry {
    /// The engines of this build, none started.
    pub fn compiled() -> Self {
        let engines = vec![
            #[cfg(feature = "engine-servo")]
            servo(),
            #[cfg(feature = "engine-wasm")]
            wasm(),
            #[cfg(feature = "engine-demo")]
            demo(),
        ];
        Self {
            engines,
            leaked: false,
        }
    }

    /// What each engine supports, by name, for the core's `Environment`.
    pub fn capabilities(&self) -> BTreeMap<String, EngineCapabilities> {
        self.engines
            .iter()
            .map(|e| (e.name.to_owned(), e.capabilities))
            .collect()
    }

    /// Every engine, in build order.
    pub fn engines(&self) -> impl Iterator<Item = &Engine> {
        self.engines.iter()
    }

    /// `name`'s engine.
    pub fn engine(&self, name: &str) -> Option<&Engine> {
        self.engines.iter().find(|e| e.name == name)
    }

    /// Starts the engines that start with `sig` rather than on first use: Servo, when it is
    /// compiled in and `config` enables it (decision 26).
    #[cfg_attr(not(feature = "engine-servo"), allow(unused_variables))]
    pub fn start_at_launch(&mut self, config: &Config, waker: impl FnOnce() -> EngineWaker) {
        #[cfg(feature = "engine-servo")]
        if config.is_enabled("servo") {
            if let Err(error) = self.start("servo", waker) {
                log::error!("Servo could not start: {error}");
            }
            self.engines
                .iter_mut()
                .filter(|e| e.name == "servo")
                .for_each(|e| e.called = false);
        }
    }

    /// `name`'s runtime, started now if it wasn't, with a waker from `waker`. Counts as a call:
    /// the runtime is pumped in this iteration.
    pub fn start(
        &mut self,
        name: &str,
        waker: impl FnOnce() -> EngineWaker,
    ) -> Result<&mut Runtime, String> {
        let engine = self
            .engines
            .iter_mut()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("the {name} engine is not part of this build"))?;
        if matches!(engine.state, State::NotStarted) {
            let waker = waker();
            engine.state = match (engine.start)(waker.clone()) {
                Ok(runtime) => {
                    if runtime.capabilities() != engine.capabilities {
                        log::warn!(
                            "the {} engine supports {:?}, not {:?} as the registry says",
                            engine.name,
                            runtime.capabilities(),
                            engine.capabilities
                        );
                    }
                    engine.waker = Some(waker);
                    State::Running(runtime)
                }
                Err(error) => State::Failed(error),
            };
        }
        engine.called = true;
        match &mut engine.state {
            State::Running(runtime) => Ok(runtime),
            State::Failed(error) => Err(error.clone()),
            State::NotStarted | State::ShutDown => Err(format!("the {name} engine is shut down")),
        }
    }

    /// `name`'s runtime if it is running, marked as called.
    pub fn running(&mut self, name: &str) -> Option<&mut Runtime> {
        let engine = self.engines.iter_mut().find(|e| e.name == name)?;
        match &mut engine.state {
            State::Running(runtime) => {
                engine.called = true;
                Some(runtime)
            }
            _ => None,
        }
    }

    /// `name`'s runtime failed; it opens nothing more.
    pub fn failed(&mut self, name: &str, why: &str) {
        if let Some(engine) = self.engines.iter_mut().find(|e| e.name == name) {
            if let State::Running(runtime) = &mut engine.state {
                runtime.shutdown();
                self.leaked |= runtime.leaked();
            }
            engine.state = State::Failed(why.to_owned());
        }
    }

    /// The engines to pump now: called since their last pump, woken, or past their deadline.
    /// Takes their wakers' requests first, so a wake during the pump that follows is not lost.
    pub fn due(&mut self, now: Instant) -> Vec<&'static str> {
        let mut due = Vec::new();
        for engine in &mut self.engines {
            if !matches!(engine.state, State::Running(_)) {
                continue;
            }
            let woken = engine.waker.as_ref().is_some_and(EngineWaker::take);
            let deadline = match engine.due {
                Some(PumpDue::Now) => true,
                Some(PumpDue::At(at)) => at <= now,
                None => false,
            };
            if engine.called || woken || deadline {
                due.push(engine.name);
            }
        }
        due
    }

    /// Pumps `name`'s runtime into `events`, and notes when it wants to be pumped again.
    pub fn pump(&mut self, name: &str, events: &mut Vec<sighurt_api::events::EngineEvent>) {
        let Some(engine) = self.engines.iter_mut().find(|e| e.name == name) else {
            return;
        };
        let State::Running(runtime) = &mut engine.state else {
            return;
        };
        engine.called = false;
        engine.due = match runtime.pump(events) {
            PumpStatus::Idle => None,
            PumpStatus::Pending { deadline: None } => Some(PumpDue::Now),
            PumpStatus::Pending { deadline: Some(at) } => Some(PumpDue::At(at)),
        };
    }

    /// Whether a runtime must be pumped without waiting: it was called since its last pump or
    /// has work pending now. Otherwise the earliest pump deadline, if any.
    pub fn next_pump(&self) -> Option<Option<Instant>> {
        let mut earliest: Option<Instant> = None;
        for engine in &self.engines {
            if !matches!(engine.state, State::Running(_)) {
                continue;
            }
            match engine.due {
                _ if engine.called => return Some(None),
                Some(PumpDue::Now) => return Some(None),
                Some(PumpDue::At(at)) => earliest = Some(earliest.map_or(at, |e| e.min(at))),
                None => {}
            }
        }
        earliest.map(Some)
    }

    /// Shuts every running runtime down, in build order. Afterwards nothing runs, unless
    /// [`Registry::leaked`].
    pub fn shutdown(&mut self) {
        for engine in &mut self.engines {
            if let State::Running(runtime) = &mut engine.state {
                runtime.shutdown();
                self.leaked |= runtime.leaked();
                engine.state = State::ShutDown;
            }
        }
    }

    /// Whether a runtime that was shut down, or failed, had to be left running (decision 54):
    /// then `sig` must end with `_exit` once its own state is saved.
    pub fn leaked(&self) -> bool {
        self.leaked
    }
}

#[cfg(feature = "engine-demo")]
fn demo() -> Engine {
    use sighurt_engine_demo::{DemoRuntime, CAPABILITIES, NAME};
    Engine::new(
        NAME,
        CAPABILITIES,
        PresentMode::Shm,
        "cpu, wl_shm",
        |_waker| Ok(Runtime::Demo(Box::new(DemoRuntime::new()))),
    )
}

#[cfg(feature = "engine-wasm")]
fn wasm() -> Engine {
    use sighurt_engine_wasm::{default_aot_cache, kv, SandboxPolicy, WasmConfig, WasmRuntime};
    // What `WasmRuntime::capabilities` reports; `Registry::start` warns if they ever differ.
    let capabilities = EngineCapabilities {
        history_traversal: true,
        page_zoom: true,
        ..EngineCapabilities::default()
    };
    Engine::new(
        "wasm",
        capabilities,
        PresentMode::Shm,
        "cpu (tiny-skia), wl_shm",
        |waker| {
            let config = WasmConfig {
                policy: SandboxPolicy::default(),
                kv_root: kv::default_root(),
                aot_cache: default_aot_cache(),
            };
            WasmRuntime::<TargetLease>::new(config, waker)
                .map(|runtime| Runtime::Wasm(Box::new(runtime)))
                .map_err(|e| e.to_string())
        },
    )
}

#[cfg(feature = "engine-servo")]
fn servo() -> Engine {
    use sighurt_engine_servo::{ServoConfig, ServoRuntime, CAPABILITIES};
    Engine::new(
        "servo",
        CAPABILITIES,
        PresentMode::Gl,
        "gl, straight into the page's subsurface",
        |waker| {
            let config = ServoConfig {
                profile: dirs::data_dir().map(|d| d.join("sighurt").join("servo")),
                // Times every GL call: for tests and measurements only.
                audit_gl: std::env::var_os("SIG_AUDIT_GL").is_some_and(|v| v == "1"),
            };
            let runtime = ServoRuntime::new(config, waker);
            if !LOGGER.swap(true, Ordering::SeqCst) {
                runtime.setup_logging();
            } else {
                log::warn!(
                    "Servo started after the logger: its script-thread panics are not caught"
                );
            }
            Ok(Runtime::Servo(Box::new(runtime)))
        },
    )
}
