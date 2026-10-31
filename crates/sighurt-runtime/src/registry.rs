//! The engines compiled into this build, and their runtimes.
//!
//! Cargo features decide which engines exist (`engine-servo`, `engine-wasm`, `engine-wpe`,
//! `engine-demo`); runtime configuration decides which of them may open new views (the core
//! checks it). A runtime is created on first use, one per engine kind, with the [`EngineWaker`]
//! it needs. Its `Target` and `FramePermit` are the frontend's: the registry holds
//! `Box<dyn Running<T, P>>`, and the frontend supplies the value when it starts the engine.
//! The registry knows what each engine supports before its runtime exists, since the core needs
//! that from the start.
//!
//! Each runtime remembers why it must be pumped: it was called this iteration, its waker fired,
//! or its pump deadline passed. The registry also remembers a runtime that its `shutdown` had
//! to leave running (Servo after a page crash or a lost backend, decision 54), so the process
//! knows to end with [`crate::exit_now`].
//!
//! Servo is the exception to starting on first use (decision 26): when it is compiled in and
//! enabled, it starts with the process, because its logger must be the process's one `log`
//! logger (the only way script-thread panics reach Servo) and Servo can be built only once per
//! process. Without it, [`install_logger`] installs `env_logger`; a Servo started later, by a
//! configuration that enables it, then logs through that one. The frontend's start function
//! calls [`claim_logger`] and, when it wins, Servo's `setup_logging`.
//!
//! The keymaps of the build are listed here ([`KEYMAPS`]), as engines are: compiled in, chosen
//! by `[input] keymap`. Chromes are the frontend's.
//!
//! What each engine supports, and which settings of `[engines.<name>]` it honors, is the
//! build's [`crate::engines::descriptors`], so every frontend's [`Environment`] says the same
//! thing. A frontend binds each descriptor with a renderer label and a start function
//! ([`Registry::bind`]) and makes the engine's own configuration (`ServoConfig`, `WasmConfig`,
//! `WpeConfig`) from the helpers here. Every runtime starts with the settings of the
//! configuration the process started with, whenever it starts ([`Launch`]). An engine started
//! with `storage = "ephemeral"` that has to write while it runs gets a directory of its own
//! under `$XDG_RUNTIME_DIR` ([`Launch::scratch`]),
//! which [`Registry::remove_scratch`] deletes at the end of shutdown, after everything that
//! could still write there has stopped. Persistent engine profiles and `{state_dir}/sighurt`
//! are mode 0700 ([`crate::persistence::secure_private_dir`]).
//!
//! Must not own: routing policy (the core decides which engine opens a URL), windows, or the
//! concrete target and permit types.

use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sighurt_api::engine::{EngineCapabilities, EngineRuntime, EngineWaker, PumpStatus};
use sighurt_api::keymap::KeymapKind;
use sighurt_core::config::{Config, EngineSettings};
use sighurt_core::{ChromeChecks, Environment, Frontend};
use tempfile::TempDir;

/// What a running engine adds to the contract, for diagnostics and shutdown.
///
/// The frontend implements this for each concrete runtime and stores it in [`Runtime`]. The
/// defaults are a CPU engine that counts no readbacks and shuts down cleanly.
pub trait Running<T, P>: EngineRuntime<Target = T, FramePermit = P> {
    /// How it renders, once it knows. `None` until a GL engine has a context. The default is
    /// a CPU engine.
    fn renderer(&self) -> Option<(&'static str, Option<String>)> {
        Some(("cpu", None))
    }

    /// Page readbacks to the CPU the engine made. 0 for the CPU engines.
    fn readbacks(&self) -> u64 {
        0
    }

    /// How long the engine itself took to paint the frame it last submitted, when it times
    /// that apart from presenting. `None` for the CPU engines, whose `present` is the painting.
    fn paint_time(&self) -> Option<std::time::Duration> {
        None
    }

    /// The GL reads among [`Running::readbacks`], when the engine audits its GL calls.
    fn gl_reads(&self) -> Option<u64> {
        None
    }

    /// Whether `shutdown` had to leave the engine running (decision 54).
    fn leaked(&self) -> bool {
        false
    }
}

/// A running engine, distinguished so each kind can be named, holding the frontend's target
/// and permit types.
pub enum Runtime<T: 'static, P: 'static> {
    #[cfg(feature = "engine-servo")]
    Servo(Box<dyn Running<T, P>>),
    #[cfg(feature = "engine-wasm")]
    Wasm(Box<dyn Running<T, P>>),
    #[cfg(feature = "engine-wpe")]
    Wpe(Box<dyn Running<T, P>>),
    #[cfg(feature = "engine-cef")]
    Cef(Box<dyn Running<T, P>>),
    #[cfg(feature = "engine-demo")]
    Demo(Box<dyn Running<T, P>>),
    /// Present only when no engine is compiled in, so the type still names `T` and `P`.
    /// Never constructed.
    #[cfg(not(any(
        feature = "engine-servo",
        feature = "engine-wasm",
        feature = "engine-wpe",
        feature = "engine-cef",
        feature = "engine-demo"
    )))]
    #[allow(dead_code)]
    None(std::marker::PhantomData<fn(T, P)>),
}

impl<T: 'static, P: 'static> Runtime<T, P> {
    fn as_running(&self) -> &(dyn Running<T, P> + 'static) {
        match self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(engine) => engine.as_ref(),
            #[cfg(feature = "engine-wasm")]
            Self::Wasm(engine) => engine.as_ref(),
            #[cfg(feature = "engine-wpe")]
            Self::Wpe(engine) => engine.as_ref(),
            #[cfg(feature = "engine-cef")]
            Self::Cef(engine) => engine.as_ref(),
            #[cfg(feature = "engine-demo")]
            Self::Demo(engine) => engine.as_ref(),
            #[cfg(not(any(
                feature = "engine-servo",
                feature = "engine-wasm",
                feature = "engine-wpe",
                feature = "engine-cef",
                feature = "engine-demo"
            )))]
            Self::None(_) => unreachable!("no engine is compiled in"),
        }
    }

    fn as_running_mut(&mut self) -> &mut (dyn Running<T, P> + 'static) {
        match self {
            #[cfg(feature = "engine-servo")]
            Self::Servo(engine) => engine.as_mut(),
            #[cfg(feature = "engine-wasm")]
            Self::Wasm(engine) => engine.as_mut(),
            #[cfg(feature = "engine-wpe")]
            Self::Wpe(engine) => engine.as_mut(),
            #[cfg(feature = "engine-cef")]
            Self::Cef(engine) => engine.as_mut(),
            #[cfg(feature = "engine-demo")]
            Self::Demo(engine) => engine.as_mut(),
            #[cfg(not(any(
                feature = "engine-servo",
                feature = "engine-wasm",
                feature = "engine-wpe",
                feature = "engine-cef",
                feature = "engine-demo"
            )))]
            Self::None(_) => unreachable!("no engine is compiled in"),
        }
    }
}

impl<T: 'static, P: 'static> Deref for Runtime<T, P> {
    type Target = dyn Running<T, P>;

    fn deref(&self) -> &Self::Target {
        self.as_running()
    }
}

impl<T: 'static, P: 'static> DerefMut for Runtime<T, P> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.as_running_mut()
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

/// Marks the process logger as taken. Returns whether it was taken already. The frontend
/// calls this when it starts Servo: the first caller installs Servo's logger.
pub fn claim_logger() -> bool {
    LOGGER.swap(true, Ordering::SeqCst)
}

/// The keymaps compiled into this build besides the core's own `bindings`.
pub const KEYMAPS: &[KeymapKind] = &[
    #[cfg(feature = "keymap-modal")]
    sighurt_keymap_modal::KIND,
];

/// What a runtime is started with.
// Only the Wasm engine needs a scratch directory, and the demo engine reads nothing.
#[cfg_attr(
    not(any(feature = "engine-wasm", feature = "engine-cef")),
    allow(dead_code)
)]
pub struct Launch<'a> {
    /// `[engines.<name>]` of the configuration the process started with.
    pub settings: &'a EngineSettings,
    scratch: &'a mut Option<TempDir>,
    name: &'static str,
}

#[cfg_attr(
    not(any(feature = "engine-wasm", feature = "engine-cef")),
    allow(dead_code)
)]
impl Launch<'_> {
    /// A directory of the engine's own for what it keeps only while the process runs, made
    /// now: under `$XDG_RUNTIME_DIR` (or the temporary directory), readable by the user alone,
    /// and deleted at the end of shutdown.
    pub fn scratch(&mut self) -> Result<PathBuf, String> {
        if self.scratch.is_none() {
            use std::os::unix::fs::PermissionsExt;
            let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
            let dir = tempfile::Builder::new()
                .prefix(&format!("sighurt-{}-", self.name))
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in(&base)
                .map_err(|e| format!("cannot make a directory in {}: {e}", base.display()))?;
            *self.scratch = Some(dir);
        }
        Ok(self.scratch.as_ref().expect("made above").path().to_owned())
    }
}

/// What a frontend adds to one compiled-in engine: which one, how its views present, and how
/// to start its runtime bound to the frontend's target and permit. The name, capabilities and
/// honored settings come from [`crate::engines::descriptors`].
pub struct Binding<T: 'static, P: 'static> {
    /// The engine's name. One of [`crate::engines::descriptors`], bound once.
    pub name: &'static str,
    /// How it renders, for diagnostics and the home page.
    pub renderer: &'static str,
    /// Builds the runtime. Called on first use, and for Servo at launch when it is enabled.
    pub start: fn(EngineWaker, Launch<'_>) -> Result<Runtime<T, P>, String>,
}

/// One compiled-in engine.
pub struct Engine<T: 'static, P: 'static> {
    /// Its name in the configuration.
    pub name: &'static str,
    /// What it supports; the core needs this before the runtime exists.
    pub capabilities: EngineCapabilities,
    /// How it renders, for diagnostics and the home page.
    pub renderer: &'static str,
    /// The settings of `[engines.<name>]` it honors.
    pub honors: &'static [&'static str],
    start: fn(EngineWaker, Launch<'_>) -> Result<Runtime<T, P>, String>,
    /// What it starts with: the configuration's when the process started.
    settings: EngineSettings,
    /// Its directory for what it keeps only while the process runs, once it has one.
    scratch: Option<TempDir>,
    state: State<T, P>,
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
pub enum State<T: 'static, P: 'static> {
    /// Not needed yet.
    #[cfg_attr(
        not(any(
            feature = "engine-servo",
            feature = "engine-wasm",
            feature = "engine-wpe",
            feature = "engine-cef",
            feature = "engine-demo"
        )),
        allow(dead_code)
    )]
    NotStarted,
    /// Running.
    Running(Runtime<T, P>),
    /// Starting it failed, or it reported `RuntimeFailed`: why.
    Failed(String),
    /// `EngineRuntime::shutdown` was called.
    ShutDown,
}

impl<T: 'static, P: 'static> Engine<T, P> {
    /// An engine that is not started yet. `start` builds its runtime, bound to `T` and `P`.
    #[cfg_attr(
        not(any(
            feature = "engine-servo",
            feature = "engine-wasm",
            feature = "engine-wpe",
            feature = "engine-cef",
            feature = "engine-demo"
        )),
        allow(dead_code)
    )]
    pub fn new(
        name: &'static str,
        capabilities: EngineCapabilities,
        renderer: &'static str,
        honors: &'static [&'static str],
        start: fn(EngineWaker, Launch<'_>) -> Result<Runtime<T, P>, String>,
    ) -> Self {
        Self {
            name,
            capabilities,
            renderer,
            honors,
            start,
            settings: EngineSettings::default(),
            scratch: None,
            state: State::NotStarted,
            waker: None,
            called: false,
            due: None,
        }
    }

    /// Where the runtime is in its life.
    pub fn state(&self) -> &State<T, P> {
        &self.state
    }
}

/// Every compiled-in engine.
pub struct Registry<T: 'static, P: 'static> {
    engines: Vec<Engine<T, P>>,
    /// A runtime that was shut down is still running ([`Running::leaked`]).
    leaked: bool,
}

impl<T: 'static, P: 'static> Registry<T, P> {
    /// `engines`, none started. [`Registry::bind`] is how a frontend builds this: the order is
    /// the descriptor order, which is the shutdown order.
    pub fn new(engines: Vec<Engine<T, P>>) -> Self {
        Self {
            engines,
            leaked: false,
        }
    }

    /// The build's engines, each bound once, in descriptor order (the shutdown order). `bindings`
    /// names each engine of [`crate::engines::descriptors`] and no other.
    pub fn bind(bindings: Vec<Binding<T, P>>) -> Self {
        let descriptors = crate::engines::descriptors();
        let engines = descriptors
            .iter()
            .map(|desc| {
                let found: Vec<_> = bindings.iter().filter(|b| b.name == desc.name).collect();
                assert!(
                    found.len() == 1,
                    "the {} engine is compiled in: the frontend must bind it once (it bound it {} times)",
                    desc.name,
                    found.len()
                );
                let binding = found[0];
                Engine::new(
                    desc.name,
                    desc.capabilities,
                    binding.renderer,
                    desc.honors,
                    binding.start,
                )
            })
            .collect();
        assert!(
            bindings.len() == descriptors.len(),
            "the frontend bound an engine this build does not have"
        );
        Self::new(engines)
    }

    /// What the core checks configurations against: the build's engines, with what each
    /// supports and which settings it honors, its keymaps, and the chromes and color names the
    /// frontend passes.
    pub fn environment(
        &self,
        chromes: Vec<ChromeChecks>,
        color_names: &'static [&'static str],
    ) -> Environment {
        let web_engines = crate::engines::descriptors()
            .into_iter()
            .filter(|desc| desc.web && self.engine(desc.name).is_some())
            .map(|desc| desc.name.to_owned())
            .collect();
        let engines = self.engines.iter();
        Environment {
            engines: engines
                .clone()
                .map(|e| (e.name.into(), e.capabilities))
                .collect(),
            web_engines,
            engine_settings: engines.map(|e| (e.name.into(), e.honors)).collect(),
            home_dir: dirs::home_dir(),
            keymaps: KEYMAPS.to_vec(),
            chromes,
            color_names,
            frontend: Frontend::Gui,
            starting_engine: None,
        }
    }

    /// Every engine, in build order.
    pub fn engines(&self) -> impl Iterator<Item = &Engine<T, P>> {
        self.engines.iter()
    }

    /// `name`'s engine.
    pub fn engine(&self, name: &str) -> Option<&Engine<T, P>> {
        self.engines.iter().find(|e| e.name == name)
    }

    /// Takes the settings every runtime starts with from `config`, the configuration the
    /// process starts with. Nothing starts.
    pub fn configure(&mut self, config: &Config) {
        for engine in &mut self.engines {
            engine.settings = config.engine_settings(engine.name);
        }
    }

    /// [`Self::configure`], then starts the engines that start with the process rather than on
    /// first use: Servo, when it is compiled in and `config` enables it (decision 26).
    #[cfg_attr(not(feature = "engine-servo"), allow(unused_variables))]
    pub fn start_at_launch(&mut self, config: &Config, waker: impl FnOnce() -> EngineWaker) {
        self.configure(config);
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
    ) -> Result<&mut Runtime<T, P>, String> {
        let engine = self
            .engines
            .iter_mut()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("the {name} engine is not part of this build"))?;
        if matches!(engine.state, State::NotStarted) {
            let waker = waker();
            let launch = Launch {
                settings: &engine.settings,
                scratch: &mut engine.scratch,
                name: engine.name,
            };
            engine.state = match (engine.start)(waker.clone(), launch) {
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
    pub fn running(&mut self, name: &str) -> Option<&mut Runtime<T, P>> {
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
    /// then the process must end with [`crate::exit_now`] once its own state is saved.
    pub fn leaked(&self) -> bool {
        self.leaked
    }

    /// Deletes the engines' directories for what they keep only while the process runs. Called
    /// at the end of shutdown: a guest still finishing its last call when its runtime shut
    /// down could otherwise write there again.
    pub fn remove_scratch(&mut self) {
        for engine in &mut self.engines {
            let Some(dir) = engine.scratch.take() else {
                continue;
            };
            let path = dir.path().to_owned();
            if let Err(error) = dir.close() {
                log::warn!("cannot delete {}: {error}", path.display());
            }
        }
    }
}

/// The Wasm runtime's directories: the key-value store in the user's data directory and
/// compiled modules in the cache, or with `storage = "ephemeral"` the store in the engine's
/// scratch directory and no cache, so nothing outlives the process.
#[cfg(feature = "engine-wasm")]
pub fn wasm_config(mut launch: Launch<'_>) -> Result<sighurt_engine_wasm::WasmConfig, String> {
    use sighurt_core::config::Storage;
    use sighurt_engine_wasm::{default_aot_cache, kv, SandboxPolicy, WasmConfig};
    let (kv_root, aot_cache) = match launch.settings.storage {
        Storage::Profile => (kv::default_root(), default_aot_cache()),
        Storage::Ephemeral => (launch.scratch()?.join("kv"), None),
    };
    Ok(WasmConfig {
        policy: SandboxPolicy::default(),
        kv_root,
        aot_cache,
    })
}

/// WebKit's configuration: its profile in the user's data directory, or with
/// `storage = "ephemeral"` none, which keeps everything in memory; the user agent, if one is set.
/// A persistent profile, and `{data_dir}/sighurt` above it, are mode 0700.
#[cfg(feature = "engine-wpe")]
pub fn wpe_config(settings: &EngineSettings) -> sighurt_engine_wpe::WpeConfig {
    use sighurt_core::config::Storage;
    let profile = match settings.storage {
        Storage::Profile => dirs::data_dir().map(|d| d.join("sighurt").join("wpe")),
        Storage::Ephemeral => None,
    };
    if let Some(profile) = &profile {
        secure_profile(profile);
    }
    sighurt_engine_wpe::WpeConfig {
        profile,
        user_agent: settings.user_agent.clone(),
        ..Default::default()
    }
}

/// Chromium's configuration: its profile in the user's data directory, or with
/// `storage = "ephemeral"` none (an in-memory session whose bookkeeping lives in the engine's
/// scratch directory); the user agent, if one is set. Errors say CEF is not installed.
#[cfg(feature = "engine-cef")]
pub fn cef_config(mut launch: Launch<'_>) -> Result<sighurt_engine_cef::CefConfig, String> {
    use sighurt_core::config::Storage;
    let mut config = sighurt_engine_cef::CefConfig::installed()
        .ok_or("CEF is not installed: run nix/build-cef.sh (docs/rewrite/CEF.md)")?;
    match launch.settings.storage {
        Storage::Profile => {
            let profile = dirs::data_dir().map(|d| d.join("sighurt").join("cef"));
            if let Some(profile) = &profile {
                secure_profile(profile);
            }
            config.profile = profile;
        }
        Storage::Ephemeral => {}
    }
    if config.profile.is_none() {
        config.scratch = Some(launch.scratch()?);
    }
    config.user_agent = launch.settings.user_agent.clone();
    Ok(config)
}

/// Servo's configuration: its profile in the user's data directory, or with
/// `storage = "ephemeral"` none, which keeps nothing; the user agent, if one is set;
/// `experimental-features` and `preferences`, applied when Servo is built.
/// A persistent profile, and `{data_dir}/sighurt` above it, are mode 0700.
#[cfg(feature = "engine-servo")]
pub fn servo_config(settings: &EngineSettings) -> sighurt_engine_servo::ServoConfig {
    use sighurt_core::config::{ServoPrefValue, Storage};
    use sighurt_engine_servo::ServoPreference;
    let profile = match settings.storage {
        Storage::Profile => dirs::data_dir().map(|d| d.join("sighurt").join("servo")),
        Storage::Ephemeral => None,
    };
    if let Some(profile) = &profile {
        secure_profile(profile);
    }
    let preferences = settings
        .preferences
        .iter()
        .map(|(name, value)| {
            let value = match value {
                ServoPrefValue::Bool(value) => ServoPreference::Bool(*value),
                ServoPrefValue::Int(value) => ServoPreference::Int(*value),
                ServoPrefValue::Uint(value) => ServoPreference::Uint(*value),
                ServoPrefValue::String(value) => ServoPreference::String(value.clone()),
                ServoPrefValue::Rgba(value) => ServoPreference::Rgba(*value),
            };
            (name.clone(), value)
        })
        .collect();
    sighurt_engine_servo::ServoConfig {
        profile,
        user_agent: settings.user_agent.clone(),
        // Times every GL call: for tests and measurements only.
        audit_gl: std::env::var_os("SIG_AUDIT_GL").is_some_and(|v| v == "1"),
        experimental_features: settings.experimental_features,
        preferences,
    }
}

/// Mode 0700 on `profile` and on the Sighurt directory above it. A failure is logged: the
/// engine then keeps nothing if it cannot use the directory.
#[cfg(any(
    feature = "engine-wpe",
    feature = "engine-servo",
    feature = "engine-cef"
))]
fn secure_profile(profile: &std::path::Path) {
    if let Some(root) = profile.parent() {
        if let Err(error) = crate::persistence::secure_private_dir(root) {
            log::warn!("cannot make {} private: {error}", root.display());
        }
    }
    if let Err(error) = crate::persistence::secure_private_dir(profile) {
        log::warn!("cannot make {} private: {error}", profile.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keymaps_follow_the_build() {
        let registry = Registry::<(), ()>::new(Vec::new());
        let env = registry.environment(Vec::new(), &[]);
        assert!(env.engines.is_empty());
        assert_eq!(env.keymaps, KEYMAPS.to_vec());
        assert!(env.chromes.is_empty());
    }

    #[cfg(feature = "engine-servo")]
    #[test]
    fn servos_configuration_follows_its_settings() {
        use sighurt_core::config::ServoPrefValue;
        use sighurt_core::config::Storage;
        use sighurt_engine_servo::ServoPreference;
        let mut ephemeral = EngineSettings {
            user_agent: Some("Sighurt/1".into()),
            storage: Storage::Ephemeral,
            experimental_features: true,
            ..EngineSettings::default()
        };
        ephemeral.preferences.insert(
            "dom_adoptedstylesheet_enabled".into(),
            ServoPrefValue::Bool(true),
        );
        let config = servo_config(&ephemeral);
        assert_eq!(
            (config.profile, config.user_agent.as_deref()),
            (None, Some("Sighurt/1"))
        );
        assert!(config.experimental_features);
        assert_eq!(
            config.preferences,
            vec![(
                "dom_adoptedstylesheet_enabled".into(),
                ServoPreference::Bool(true)
            )]
        );
        let config = servo_config(&EngineSettings::default());
        let profile = dirs::data_dir().map(|d| d.join("sighurt").join("servo"));
        assert_eq!((config.profile, config.user_agent), (profile, None));
        assert!(!config.experimental_features);
        assert!(config.preferences.is_empty());
    }

    #[cfg(feature = "engine-wpe")]
    #[test]
    fn webkits_configuration_follows_its_settings() {
        use sighurt_core::config::Storage;
        let ephemeral = EngineSettings {
            user_agent: Some("Sighurt/1".into()),
            storage: Storage::Ephemeral,
            ..EngineSettings::default()
        };
        let config = wpe_config(&ephemeral);
        assert_eq!(
            (config.profile, config.user_agent.as_deref()),
            (None, Some("Sighurt/1"))
        );
        let config = wpe_config(&EngineSettings::default());
        let profile = dirs::data_dir().map(|d| d.join("sighurt").join("wpe"));
        assert_eq!((config.profile, config.user_agent), (profile, None));
        assert!(!config.force_shm && !config.jsc_signal_handlers);
    }

    #[cfg(feature = "engine-wasm")]
    #[test]
    fn the_wasm_engines_directories_follow_its_storage() {
        use sighurt_core::config::Storage;
        use sighurt_engine_wasm::{default_aot_cache, kv};
        let mut scratch = None;
        let ephemeral = EngineSettings {
            storage: Storage::Ephemeral,
            ..EngineSettings::default()
        };
        let config = wasm_config(Launch {
            settings: &ephemeral,
            scratch: &mut scratch,
            name: "wasm",
        });
        let config = config.unwrap();
        let dir = scratch.as_ref().expect("made").path();
        assert_eq!((config.kv_root, config.aot_cache), (dir.join("kv"), None));
        let mut none = None;
        let config = wasm_config(Launch {
            settings: &EngineSettings::default(),
            scratch: &mut none,
            name: "wasm",
        });
        let config = config.unwrap();
        assert_eq!(
            (config.kv_root, config.aot_cache),
            (kv::default_root(), default_aot_cache())
        );
        assert!(none.is_none(), "no directory for nothing");
    }

    #[cfg(feature = "engine-wasm")]
    #[test]
    fn an_ephemeral_engines_directory_lasts_until_the_end_of_shutdown() {
        use std::os::unix::fs::PermissionsExt;

        use sighurt_api::geometry::Metrics;
        use sighurt_api::ids::LayoutEpoch;
        use sighurt_engine_wasm::{CpuTarget, Frame, WasmRuntime};

        /// An in-memory target, so this test starts the real Wasm runtime without a display.
        struct MemTarget;
        struct MemPermit;

        impl CpuTarget for MemTarget {
            type Permit = MemPermit;

            fn metrics(&self) -> Metrics {
                unimplemented!("the test never presents")
            }

            fn permit_epoch(_permit: &MemPermit) -> LayoutEpoch {
                unimplemented!("the test never presents")
            }

            fn submit(
                &mut self,
                _permit: MemPermit,
                _paint: &mut dyn FnMut(Frame<'_>) -> Result<(), String>,
            ) -> Result<(), String> {
                Ok(())
            }
        }

        impl Running<MemTarget, MemPermit> for WasmRuntime<MemTarget> {}

        let engine = Engine::new(
            "wasm",
            EngineCapabilities {
                history_traversal: true,
                page_zoom: true,
                ..EngineCapabilities::default()
            },
            "cpu (tiny-skia), wl_shm",
            &["storage"],
            |waker, launch| {
                WasmRuntime::new(wasm_config(launch)?, waker)
                    .map(|runtime| Runtime::Wasm(Box::new(runtime)))
                    .map_err(|e| e.to_string())
            },
        );

        let mut registry = Registry::new(vec![engine]);
        let text = "version = 1\n[engines]\ndefault = \"wasm\"\nenabled = [\"wasm\"]\n\
                    [engines.wasm]\nstorage = \"ephemeral\"";
        let config = Config::parse(Some(text), &registry.environment(Vec::new(), &[])).unwrap();
        registry.start_at_launch(&config, || -> EngineWaker { unreachable!("no Servo") });
        assert!(registry.start("wasm", || EngineWaker::new(|| {})).is_ok());
        let wasm = registry.engine("wasm").unwrap();
        let scratch = wasm
            .scratch
            .as_ref()
            .expect("made at start")
            .path()
            .to_owned();
        let base = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir);
        assert!(
            scratch.starts_with(&base) && scratch.is_dir(),
            "{scratch:?}"
        );
        let mode = std::fs::metadata(&scratch).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "the user's alone");
        registry.shutdown();
        assert!(scratch.is_dir(), "kept while guests may still finish");
        registry.remove_scratch();
        assert!(!scratch.exists());
    }
}
