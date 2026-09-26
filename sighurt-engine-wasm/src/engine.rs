//! The Wasmtime engine a runtime shares between all its guests, and the limits guests run under.
//!
//! Guests run under [Wasmtime](https://wasmtime.dev/) with bounded linear memory and reach
//! nothing but the host functions registered in [`crate::capabilities`] (no WASI). Every call
//! into a guest, instantiation included (a module's `start` function), goes through
//! [`WasmEngine::call`] or [`WasmEngine::start`], which give it a budget of instructions (fuel)
//! and of wall-clock time. The time is checked on every epoch tick by the store's
//! [`Budget`]: a call halts when it has run longer than its time limit, not counting what host
//! functions spent waiting (on the network, another module, a dialog); when host functions have
//! waited longer than [`SandboxPolicy::max_wait`] for the network and other modules within it;
//! or as soon as its document is closed or replaced. A guest that exceeds its fuel traps too.
//!
//! Default [`SandboxPolicy`]: 256 MiB of linear memory, 500M instructions for `start_app`, 50M
//! for each later callback, 10 s for `start_app` and 1 s for each callback, and 30 s of waiting
//! for the network in each.
//!
//! Owns: the Wasmtime [`Engine`], the epoch ticker, the compiled-module cache, the location of
//! the persistent key-value store and the count of worker threads. Must not own: any guest's
//! state, or process-wide choices such as where those directories are (the executable passes
//! them in [`WasmConfig`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use wasmtime::*;

use crate::capabilities::HostState;

const MAX_MEMORY_PAGES: u32 = 4096; // 4096 * 64KB = 256MB
const FUEL_LIMIT: u64 = 500_000_000; // ~500M instructions before forced halt
/// Fuel for each callback after `start_app`: `on_frame`, `on_timer`, `on_event`, `on_message`.
pub const FRAME_FUEL_LIMIT: u64 = 50_000_000;
/// How often the epoch advances while a guest runs, and so how often a running call's
/// [`Budget`] is checked.
const EPOCH_TICK: Duration = Duration::from_millis(10);

/// Bump when [`Config`] changes in a way that would invalidate cached artifacts.
const AOT_CACHE_FORMAT: &str = "v2";

/// The hard limits applied to a guest at runtime.
#[derive(Clone, Debug)]
pub struct SandboxPolicy {
    /// Maximum number of 64 KiB Wasm memory pages the guest may grow to (default: 4096 → 256 MiB).
    pub max_memory_pages: u32,
    /// Wasm "fuel" (instruction budget) for `start_app`, and for a new [`Store`].
    pub fuel_limit: u64,
    /// Fuel for each later callback.
    pub frame_fuel: u64,
    /// Wall-clock time `start_app` may take, not counting host functions that wait.
    pub start_time_limit: Duration,
    /// Wall-clock time each later callback may take, not counting host functions that wait.
    pub time_limit: Duration,
    /// Most time the host functions of one call may wait for the network or for other modules
    /// (`api_fetch`, `api_load_module`, `api_audio_play_url`) together. Waiting for the user in
    /// a file dialog is not counted.
    pub max_wait: Duration,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            max_memory_pages: MAX_MEMORY_PAGES,
            fuel_limit: FUEL_LIMIT,
            frame_fuel: FRAME_FUEL_LIMIT,
            start_time_limit: Duration::from_secs(10),
            time_limit: Duration::from_secs(1),
            // The timeout of one request.
            max_wait: Duration::from_secs(30),
        }
    }
}

/// How a Wasm runtime is set up. The executable chooses the directories; the engine never picks
/// process-wide locations itself.
#[derive(Clone, Debug)]
pub struct WasmConfig {
    /// Limits for every guest.
    pub policy: SandboxPolicy,
    /// Where `api_kv_store_*` keeps each origin's keys: [`crate::kv::default_root`] for `sig`.
    pub kv_root: PathBuf,
    /// Where compiled modules are cached ([`default_aot_cache`] for `sig`), or `None` to compile
    /// every module every time.
    pub aot_cache: Option<PathBuf>,
}

/// The shared engine: Wasmtime, the limits, the epoch ticker and the runtime's directories.
/// Cloning is cheap and shares all of it; the ticker thread ends when the last clone is gone.
#[derive(Clone)]
pub struct WasmEngine {
    engine: Engine,
    policy: SandboxPolicy,
    kv_root: Arc<Path>,
    aot_cache: Option<Arc<Path>>,
    ticker: Arc<TickerHandle>,
    /// Guest worker threads alive in this runtime (see [`crate::worker`]).
    worker_threads: Arc<AtomicUsize>,
}

impl WasmEngine {
    /// Builds the engine: fuel metering and epoch interruption on, Cranelift optimizing for
    /// speed, and the epoch ticker thread started (it sleeps while no guest runs).
    pub fn new(config: WasmConfig) -> Result<Self> {
        let mut wasmtime = Config::new();
        wasmtime.consume_fuel(true);
        wasmtime.epoch_interruption(true);
        wasmtime.cranelift_opt_level(OptLevel::Speed);
        let engine = Engine::new(&wasmtime).context("failed to create wasmtime engine")?;
        let ticker = Arc::new(Ticker::default());
        let weak = engine.weak();
        let thread_ticker = ticker.clone();
        thread::Builder::new()
            .name("sighurt-wasm-epoch".into())
            .spawn(move || thread_ticker.run(&weak))
            .context("cannot start the epoch ticker")?;
        Ok(Self {
            engine,
            policy: config.policy,
            kv_root: config.kv_root.into(),
            aot_cache: config.aot_cache.map(Into::into),
            ticker: Arc::new(TickerHandle(ticker)),
            worker_threads: Arc::default(),
        })
    }

    /// Returns the underlying Wasmtime [`Engine`] for compilation and linking.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The limits guests run under.
    pub fn policy(&self) -> &SandboxPolicy {
        &self.policy
    }

    /// Where the persistent key-value store keeps each origin's keys.
    pub fn kv_root(&self) -> &Path {
        &self.kv_root
    }

    /// Creates a new [`Store`] with `state` as host state and a full tank of fuel, whose calls
    /// are checked against `state`'s [`Budget`] on every epoch tick. Guest code that runs
    /// outside [`call`](Self::call) and [`start`](Self::start) halts at the first tick.
    pub fn create_store(&self, state: HostState) -> Result<Store<HostState>> {
        let mut store = Store::new(&self.engine, state);
        store
            .set_fuel(self.policy.fuel_limit)
            .context("failed to set fuel limit")?;
        store.epoch_deadline_callback(|store| {
            store.data().budget.check()?;
            Ok(UpdateDeadline::Continue(1))
        });
        store.set_epoch_deadline(1);
        Ok(store)
    }

    /// Allocates a new linear [`Memory`] with minimum 1 page and maximum `policy.max_memory_pages`.
    pub fn create_bounded_memory(&self, store: &mut Store<impl Send>) -> Result<Memory> {
        let mem_type = MemoryType::new(1, Some(self.policy.max_memory_pages));
        Memory::new(store, mem_type).context("failed to create bounded linear memory")
    }

    /// Compiles raw Wasm bytes into a [`Module`], reusing the compiled-module cache when there
    /// is one.
    pub fn compile_module(&self, wasm_bytes: &[u8]) -> Result<Module> {
        compile_cached_in(&self.engine, wasm_bytes, self.aot_cache.as_deref())
    }

    /// Runs `f`, a callback into the guest on `store` (`on_frame`, `on_timer`, ...), with the
    /// callback budget: [`SandboxPolicy::frame_fuel`] instructions and
    /// [`SandboxPolicy::time_limit`]. Past either, the guest traps.
    pub fn call<R>(
        &self,
        store: &mut Store<HostState>,
        f: impl FnOnce(&mut Store<HostState>) -> Result<R>,
    ) -> Result<R> {
        self.run(store, self.policy.frame_fuel, self.policy.time_limit, f)
    }

    /// Like [`call`](Self::call), with the budget of `start_app`. Instantiating a module, which
    /// runs its `start` function, gets this budget too.
    pub fn start<R>(
        &self,
        store: &mut Store<HostState>,
        f: impl FnOnce(&mut Store<HostState>) -> Result<R>,
    ) -> Result<R> {
        let policy = &self.policy;
        self.run(store, policy.fuel_limit, policy.start_time_limit, f)
    }

    fn run<R>(
        &self,
        store: &mut Store<HostState>,
        fuel: u64,
        time: Duration,
        f: impl FnOnce(&mut Store<HostState>) -> Result<R>,
    ) -> Result<R> {
        store.set_fuel(fuel)?;
        store.set_epoch_deadline(1);
        let budget = store.data().budget.clone();
        budget.begin(time, self.policy.max_wait);
        let result = {
            let _running = self.ticker.0.running();
            f(store)
        };
        budget.end();
        result
    }

    /// Counts a new worker thread, unless the runtime already has `max` of them. The count goes
    /// down again when the returned guard is dropped.
    pub(crate) fn worker_thread(&self, max: usize) -> Option<ThreadCount> {
        ThreadCount::add(&self.worker_threads, max)
    }
}

/// One counted thread; dropping it counts the thread as ended.
pub(crate) struct ThreadCount(Arc<AtomicUsize>);

impl ThreadCount {
    /// Counts one more thread in `count`, unless it is at `max` already.
    pub(crate) fn add(count: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        count
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < max).then_some(n + 1)
            })
            .ok()?;
        Some(Self(count.clone()))
    }
}

impl Drop for ThreadCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Why a guest call was stopped before it returned, other than by its fuel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halt {
    /// It ran longer than its time limit, not counting host functions that waited.
    Time,
    /// Its host functions waited longer than [`SandboxPolicy::max_wait`] for the network or
    /// other modules.
    Waited,
    /// Its document was closed or replaced, or its worker terminated.
    Cancelled,
}

impl std::fmt::Display for Halt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Time => "time limit exceeded",
            Self::Waited => "host calls waited too long",
            Self::Cancelled => "cancelled",
        })
    }
}

impl std::error::Error for Halt {}

/// What the calls into one store may spend, and whether they may still run. Shared by the
/// store's epoch callback, the host functions that wait, and the owner of the document, which
/// cancels it. Cloning shares it.
#[derive(Clone)]
pub struct Budget(Arc<BudgetState>);

struct BudgetState {
    /// The call in progress.
    call: Mutex<Option<Call>>,
    /// This store's cancellation flag, then those of the stores it was started from (the page
    /// of a worker or of a module it loaded), any of which cancels it.
    cancelled: Vec<Arc<AtomicBool>>,
}

struct Call {
    started: Instant,
    limit: Duration,
    max_wait: Duration,
    /// Time host functions spent waiting, which the time limit doesn't count.
    waited: Duration,
    /// The part of it that counts against `max_wait`.
    counted: Duration,
}

/// What a host function waits for (see [`Budget::wait`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wait {
    /// The network or another module: counted against [`SandboxPolicy::max_wait`].
    Host,
    /// The user, in a dialog: not counted.
    User,
}

impl Default for Budget {
    fn default() -> Self {
        Self(Arc::new(BudgetState {
            call: Mutex::default(),
            cancelled: vec![Arc::default()],
        }))
    }
}

impl Budget {
    /// A budget for a store started from this one's: cancelling this one cancels it too.
    pub fn child(&self) -> Self {
        let mut cancelled = vec![Arc::default()];
        cancelled.extend(self.0.cancelled.iter().cloned());
        Self(Arc::new(BudgetState {
            call: Mutex::default(),
            cancelled,
        }))
    }

    /// Stops the store's calls: the one running halts at the next epoch tick, later ones at
    /// once, and host functions that would wait don't start.
    pub fn cancel(&self) {
        self.0.cancelled[0].store(true, Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0
            .cancelled
            .iter()
            .any(|flag| flag.load(Ordering::Relaxed))
    }

    /// Runs `f`, a host operation that waits for `what`, without charging the time to the
    /// call's time limit. `None`, without running `f`, when the store was cancelled.
    pub fn wait<R>(&self, what: Wait, f: impl FnOnce() -> R) -> Option<R> {
        if self.is_cancelled() {
            return None;
        }
        let started = Instant::now();
        let result = f();
        let took = started.elapsed();
        if let Some(call) = self.lock().as_mut() {
            call.waited += took;
            if what == Wait::Host {
                call.counted += took;
            }
        }
        Some(result)
    }

    fn lock(&self) -> MutexGuard<'_, Option<Call>> {
        self.0.call.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(&self, limit: Duration, max_wait: Duration) {
        *self.lock() = Some(Call {
            started: Instant::now(),
            limit,
            max_wait,
            waited: Duration::ZERO,
            counted: Duration::ZERO,
        });
    }

    fn end(&self) {
        *self.lock() = None;
    }

    /// Whether the call in progress may go on.
    fn check(&self) -> Result<(), Halt> {
        if self.is_cancelled() {
            return Err(Halt::Cancelled);
        }
        let call = self.lock();
        let Some(call) = call.as_ref() else {
            return Err(Halt::Time);
        };
        if call.counted > call.max_wait {
            return Err(Halt::Waited);
        }
        if call.started.elapsed().saturating_sub(call.waited) > call.limit {
            return Err(Halt::Time);
        }
        Ok(())
    }
}

/// Advances the engine's epoch every [`EPOCH_TICK`] while at least one guest call runs, and
/// sleeps otherwise, so an idle browser has no ticking thread.
#[derive(Default)]
struct Ticker {
    state: Mutex<TickerState>,
    wake: Condvar,
}

#[derive(Default)]
struct TickerState {
    running: usize,
    stopped: bool,
}

impl Ticker {
    fn run(&self, engine: &EngineWeak) {
        loop {
            {
                let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
                while state.running == 0 && !state.stopped {
                    state = self
                        .wake
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                }
                if state.stopped {
                    return;
                }
            }
            thread::sleep(EPOCH_TICK);
            match engine.upgrade() {
                Some(engine) => engine.increment_epoch(),
                None => return,
            }
        }
    }

    /// Counts a guest call as running until the guard is dropped.
    fn running(&self) -> Running<'_> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.running += 1;
        if state.running == 1 {
            self.wake.notify_one();
        }
        Running(self)
    }
}

struct Running<'a>(&'a Ticker);

impl Drop for Running<'_> {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.running -= 1;
    }
}

/// Stops the ticker thread when the last [`WasmEngine`] clone is dropped.
struct TickerHandle(Arc<Ticker>);

impl Drop for TickerHandle {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.stopped = true;
        self.0.wake.notify_one();
    }
}

/// `{cache_dir}/sighurt/wasm-aot`, or what `SIGHURT_AOT_CACHE` says: `off` (or `0`) for no
/// cache, a path for another root. For the executable to put in [`WasmConfig::aot_cache`].
pub fn default_aot_cache() -> Option<PathBuf> {
    match std::env::var("SIGHURT_AOT_CACHE") {
        Ok(v) if v == "0" || v.eq_ignore_ascii_case("off") => return None,
        Ok(v) if !v.is_empty() => return Some(PathBuf::from(v)),
        _ => {}
    }
    Some(dirs::cache_dir()?.join("sighurt").join("wasm-aot"))
}

/// Compiles `wasm_bytes` with `engine`, loading a serialized artifact from the compiled-module
/// cache under `cache_dir` on later visits. Stale or corrupt entries are discarded and the
/// module is compiled from source.
fn compile_cached_in(
    engine: &Engine,
    wasm_bytes: &[u8],
    cache_dir: Option<&Path>,
) -> Result<Module> {
    let cache_dir = cache_dir.map(|dir| dir.join(AOT_CACHE_FORMAT));
    let hash = module_hash(wasm_bytes);
    if let Some(dir) = &cache_dir {
        if let Some(module) = load_cached(engine, dir, &hash) {
            return Ok(module);
        }
    }

    let module = Module::new(engine, wasm_bytes).context("failed to compile wasm module")?;
    if let Some(dir) = &cache_dir {
        // The cache only saves time; a failure to write it changes nothing else.
        let _ = store_cached(dir, &hash, &module);
    }
    Ok(module)
}

fn module_hash(wasm_bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(wasm_bytes))
}

fn cache_path(dir: &Path, hash: &str) -> PathBuf {
    dir.join(format!("{hash}.cwasm"))
}

fn load_cached(engine: &Engine, dir: &Path, hash: &str) -> Option<Module> {
    let path = cache_path(dir, hash);
    if !path.is_file() {
        return None;
    }
    // SAFETY: `path` is only created by `store_cached` via `Module::serialize`.
    // A corrupt or cross-version file fails deserialize and we fall back.
    match unsafe { Module::deserialize_file(engine, &path) } {
        Ok(module) => Some(module),
        Err(_) => {
            let _ = std::fs::remove_file(&path);
            None
        }
    }
}

fn store_cached(dir: &Path, hash: &str, module: &Module) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create AOT cache dir {}", dir.display()))?;
    let dest = cache_path(dir, hash);
    if dest.exists() {
        return Ok(());
    }
    let bytes = module.serialize().context("serialize compiled module")?;
    let tmp = dir.join(format!("{hash}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &bytes).with_context(|| format!("write {}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename into {}", dest.display()));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::time::Instant;

    use super::*;

    /// An engine with `policy`, no module cache and a key-value store nobody reads.
    pub(crate) fn engine_with(policy: SandboxPolicy) -> WasmEngine {
        WasmEngine::new(WasmConfig {
            policy,
            kv_root: std::env::temp_dir().join("sighurt-wasm-unit-test-kv"),
            aot_cache: None,
        })
        .unwrap()
    }

    fn empty_wasm() -> Vec<u8> {
        wat::parse_str("(module)").expect("empty module")
    }

    fn run_export(engine: &WasmEngine, wat: &str, name: &str) -> anyhow::Error {
        let module =
            compile_cached_in(engine.engine(), &wat::parse_str(wat).unwrap(), None).unwrap();
        let mut store = engine.create_store(HostState::default()).unwrap();
        let instance = Instance::new(&mut store, &module, &[]).unwrap();
        let run = instance.get_typed_func::<(), ()>(&mut store, name).unwrap();
        engine
            .call(&mut store, |store| run.call(store, ()))
            .unwrap_err()
    }

    /// Instantiates `wat`, whose import `env.wait` waits `wait` for `what`, and runs its export
    /// `run` as a callback. Cancels the store after `cancel`, if given.
    fn run_waiting(
        engine: &WasmEngine,
        wat: &str,
        what: Wait,
        wait: Duration,
        cancel: Option<Duration>,
    ) -> Result<()> {
        let module =
            compile_cached_in(engine.engine(), &wat::parse_str(wat).unwrap(), None).unwrap();
        let mut store = engine.create_store(HostState::default()).unwrap();
        let waiting = Func::wrap(&mut store, move |caller: Caller<'_, HostState>| {
            caller.data().budget.wait(what, || thread::sleep(wait));
        });
        let instance = Instance::new(&mut store, &module, &[waiting.into()]).unwrap();
        let run = instance
            .get_typed_func::<(), ()>(&mut store, "run")
            .unwrap();
        if let Some(after) = cancel {
            let budget = store.data().budget.clone();
            thread::spawn(move || {
                thread::sleep(after);
                budget.cancel();
            });
        }
        engine.call(&mut store, |store| run.call(store, ()))
    }

    #[test]
    fn default_policy_limits() {
        let policy = SandboxPolicy::default();
        assert_eq!(policy.max_memory_pages, 4096);
        assert_eq!(policy.fuel_limit, 500_000_000);
        assert_eq!(policy.frame_fuel, 50_000_000);
    }

    #[test]
    fn compile_minimal_module() {
        let engine = engine_with(SandboxPolicy::default());
        compile_cached_in(engine.engine(), &empty_wasm(), None).expect("compile");
    }

    #[test]
    fn compile_invalid_wasm_errors() {
        let engine = engine_with(SandboxPolicy::default());
        assert!(compile_cached_in(engine.engine(), b"not a wasm module", None).is_err());
    }

    #[test]
    fn fuel_exhaustion_traps_infinite_loop() {
        let engine = engine_with(SandboxPolicy {
            frame_fuel: 10_000,
            ..SandboxPolicy::default()
        });
        let err = run_export(
            &engine,
            r#"(module (func (export "run") (loop $l (br $l))))"#,
            "run",
        );
        assert_eq!(
            crate::runtime::guest_error("run", &err),
            "run halted: fuel limit exceeded"
        );
    }

    #[test]
    fn the_time_limit_stops_a_loop_fuel_would_allow() {
        let engine = engine_with(SandboxPolicy {
            frame_fuel: u64::MAX,
            time_limit: Duration::from_millis(50),
            ..SandboxPolicy::default()
        });
        let started = Instant::now();
        let err = run_export(
            &engine,
            r#"(module (func (export "run") (loop $l (br $l))))"#,
            "run",
        );
        assert_eq!(
            crate::runtime::guest_error("run", &err),
            "run halted: time limit exceeded"
        );
        // 50 ms of budget, ticks of 10 ms, and slack for a loaded machine.
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    /// Waits five times in a row.
    const WAITS: &str = r#"(module (import "env" "wait" (func $wait))
        (func (export "run") (call $wait) (call $wait) (call $wait) (call $wait) (call $wait)))"#;

    /// Waits forever.
    const WAITS_IN_A_LOOP: &str = r#"(module (import "env" "wait" (func $wait))
        (func (export "run") (loop $l (call $wait) (br $l))))"#;

    #[test]
    fn waiting_is_not_charged_to_the_time_limit() {
        let engine = engine_with(SandboxPolicy {
            time_limit: Duration::from_millis(50),
            ..SandboxPolicy::default()
        });
        // 5 × 30 ms of waiting in a call that may run for 50 ms.
        run_waiting(&engine, WAITS, Wait::Host, Duration::from_millis(30), None).unwrap();
    }

    #[test]
    fn a_guest_that_keeps_waiting_is_stopped() {
        let engine = engine_with(SandboxPolicy {
            max_wait: Duration::from_millis(200),
            ..SandboxPolicy::default()
        });
        let started = Instant::now();
        let wait = Duration::from_millis(5);
        let err = run_waiting(&engine, WAITS_IN_A_LOOP, Wait::Host, wait, None).unwrap_err();
        assert_eq!(
            crate::runtime::guest_error("run", &err),
            "run halted: host calls waited too long"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // Waiting for the user isn't counted, but cancelling still stops it.
        let wait = Duration::from_millis(5);
        let cancel = Some(Duration::from_millis(300));
        let err = run_waiting(&engine, WAITS_IN_A_LOOP, Wait::User, wait, cancel).unwrap_err();
        assert_eq!(
            crate::runtime::guest_error("run", &err),
            "run halted: cancelled"
        );
    }

    #[test]
    fn cancelling_stops_a_running_call() {
        let engine = engine_with(SandboxPolicy {
            frame_fuel: u64::MAX,
            time_limit: Duration::from_secs(60),
            ..SandboxPolicy::default()
        });
        let started = Instant::now();
        let loops = r#"(module (import "env" "wait" (func $wait)) (func (export "run") (loop $l (br $l))))"#;
        let cancel = Some(Duration::from_millis(50));
        let err = run_waiting(&engine, loops, Wait::Host, Duration::ZERO, cancel).unwrap_err();
        assert_eq!(
            crate::runtime::guest_error("run", &err),
            "run halted: cancelled"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
    }

    #[test]
    fn a_child_is_cancelled_with_its_parent() {
        let parent = Budget::default();
        let child = parent.child();
        child.cancel();
        assert!(!parent.is_cancelled());
        let other = parent.child();
        parent.cancel();
        assert!(other.is_cancelled());
        assert_eq!(other.wait(Wait::Host, || 1), None);
    }

    #[test]
    fn other_traps_are_not_taken_for_limits() {
        let engine = engine_with(SandboxPolicy::default());
        // The export's name mentions fuel; only the trap code counts.
        let err = run_export(
            &engine,
            r#"(module (func (export "fuel") unreachable))"#,
            "fuel",
        );
        let message = crate::runtime::guest_error("fuel", &err);
        assert!(message.starts_with("fuel trapped: "), "{message}");
    }

    #[test]
    fn memory_rejects_grow_past_policy_ceiling() {
        let engine = engine_with(SandboxPolicy {
            max_memory_pages: 4,
            ..SandboxPolicy::default()
        });
        let mut store = engine.create_store(HostState::default()).unwrap();
        let mem = engine.create_bounded_memory(&mut store).unwrap();
        assert_eq!(mem.ty(&store).minimum(), 1);
        assert_eq!(mem.ty(&store).maximum(), Some(4));
        assert!(mem.grow(&mut store, 8).is_err());
    }

    #[test]
    fn aot_cache_writes_and_reloads() {
        let engine = engine_with(SandboxPolicy::default());
        let dir = tempfile::tempdir().unwrap();
        let wasm = empty_wasm();
        compile_cached_in(engine.engine(), &wasm, Some(dir.path())).unwrap();
        let path = cache_path(&dir.path().join(AOT_CACHE_FORMAT), &module_hash(&wasm));
        assert!(
            path.is_file(),
            "expected cached artifact at {}",
            path.display()
        );
        compile_cached_in(engine.engine(), &wasm, Some(dir.path())).expect("cache hit compile");
    }

    #[test]
    fn aot_cache_corrupt_entry_falls_back() {
        let engine = engine_with(SandboxPolicy::default());
        let dir = tempfile::tempdir().unwrap();
        let wasm = empty_wasm();
        let versioned = dir.path().join(AOT_CACHE_FORMAT);
        std::fs::create_dir_all(&versioned).unwrap();
        let path = cache_path(&versioned, &module_hash(&wasm));
        std::fs::write(&path, b"not a compiled module").unwrap();
        compile_cached_in(engine.engine(), &wasm, Some(dir.path())).expect("fallback compile");
        let cached = std::fs::read(&path).unwrap();
        assert_ne!(cached, b"not a compiled module");
    }
}
