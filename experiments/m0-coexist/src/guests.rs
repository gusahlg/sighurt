//! The Wasmtime side: guests configured and instantiated the way the old `sig-wasm` did it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use wasmtime::{
    Config, Engine, Linker, Memory, MemoryType, Module, OptLevel, Store, Trap, TypedFunc,
};

/// One guest module for both engines: Wasmtime runs it here, SpiderMonkey in the test pages.
/// Like an Oxide app it may import `oxide.memory`, the bounded memory the host provides.
pub const GUEST_WAT: &str = r#"
(module
  (import "oxide" "memory" (memory 1 4096))
  ;; A value in the guest's own memory, to show that instances don't share it.
  (func (export "store") (param i32) (i32.store (i32.const 0) (local.get 0)))
  (func (export "load") (result i32) (i32.load (i32.const 0)))
  ;; Reads just past the single 64 KiB page: a guard-page fault, handled through SIGSEGV.
  (func (export "oob") (result i32) (i32.load (i32.const 65536)))
  ;; `ud2` on x86-64, handled through SIGILL.
  (func (export "trap") unreachable)
  (func (export "spin") (loop $forever (br $forever))))
"#;

// The old engine's limits (gpui-baseline:sighurt-engine-wasm/src/{engine,runtime}.rs).
const MAX_MEMORY_PAGES: u32 = 4096;
const FUEL_LIMIT: u64 = 500_000_000;
pub const FRAME_FUEL_LIMIT: u64 = 50_000_000;

/// How long the epoch test lets a guest spin before the epoch is incremented.
const EPOCH_TICK: Duration = Duration::from_millis(20);

/// Wasmtime's `Config` exactly as `gpui-baseline:sighurt-engine-wasm/src/engine.rs` builds it.
fn sig_wasm_config() -> Config {
    let mut config = Config::new();
    config.consume_fuel(true);
    config.cranelift_opt_level(OptLevel::Speed);
    config
}

/// The engines and compiled modules. Creating the first engine installs Wasmtime's signal
/// handlers for the whole process.
pub struct Wasm {
    /// The old engine's configuration.
    pub engine: Engine,
    pub module: Module,
    /// The same plus epoch interruption, the time budget §15 of the plan adds.
    epoch_engine: Engine,
    epoch_module: Module,
}

impl Wasm {
    pub fn new() -> wasmtime::Result<Self> {
        let engine = Engine::new(&sig_wasm_config())?;
        let module = Module::new(&engine, GUEST_WAT)?;
        let mut config = sig_wasm_config();
        config.epoch_interruption(true);
        let epoch_engine = Engine::new(&config)?;
        let epoch_module = Module::new(&epoch_engine, GUEST_WAT)?;
        Ok(Self {
            engine,
            module,
            epoch_engine,
            epoch_module,
        })
    }

    /// Runs a guest into an endless loop and stops it by incrementing the epoch from another
    /// thread, with fuel to spare. Returns how long the guest ran.
    pub fn stop_by_epoch(&self) -> Result<Duration, String> {
        let mut guest = Guest::new(&self.epoch_engine, &self.epoch_module, 0).map_err(text)?;
        guest.store.set_fuel(u64::MAX).map_err(text)?;
        guest.store.set_epoch_deadline(1);
        let engine = self.epoch_engine.clone();
        let ticker = thread::spawn(move || {
            thread::sleep(EPOCH_TICK);
            engine.increment_epoch();
        });
        let started = Instant::now();
        let result = guest.spin.call(&mut guest.store, ());
        let ran = started.elapsed();
        let _ = ticker.join();
        expect_trap(result, Trap::Interrupt)?;
        Ok(ran)
    }

    /// Starts a thread that makes a new guest trap over and over until the returned flag is set;
    /// joining it gives the number of rounds.
    pub fn trap_in_background(&self) -> (Arc<AtomicBool>, thread::JoinHandle<Result<u32, String>>) {
        let stop = Arc::new(AtomicBool::new(false));
        let (engine, module, flag) = (self.engine.clone(), self.module.clone(), stop.clone());
        let thread = thread::Builder::new()
            .name("wasm-guest".into())
            .spawn(move || {
                let mut guest = Guest::new(&engine, &module, 7).map_err(text)?;
                let mut rounds = 0;
                while !flag.load(Ordering::Relaxed) {
                    guest.trap_round()?;
                    rounds += 1;
                }
                Ok(rounds)
            })
            .expect("spawn a thread");
        (stop, thread)
    }
}

/// One instance in its own store, with its own bounded memory holding `value`.
pub struct Guest {
    store: Store<()>,
    value: i32,
    load: TypedFunc<(), i32>,
    oob: TypedFunc<(), i32>,
    trap: TypedFunc<(), ()>,
    spin: TypedFunc<(), ()>,
}

impl Guest {
    /// Instantiates `module` as `gpui-baseline:sighurt-engine-wasm/src/runtime.rs` does: a
    /// store with the full fuel limit and `oxide.memory` bounded to the policy's pages.
    pub fn new(engine: &Engine, module: &Module, value: i32) -> wasmtime::Result<Self> {
        let mut store = Store::new(engine, ());
        store.set_fuel(FUEL_LIMIT)?;
        // With epoch interruption, a store traps at once until it has a deadline; one tick
        // from now is set again, relative to the then current epoch, before each test. Without
        // epoch interruption this does nothing.
        store.set_epoch_deadline(1);
        let memory = Memory::new(&mut store, MemoryType::new(1, Some(MAX_MEMORY_PAGES)))?;
        let mut linker = Linker::new(engine);
        linker.define(&store, "oxide", "memory", memory)?;
        let instance = linker.instantiate(&mut store, module)?;
        instance
            .get_typed_func::<i32, ()>(&mut store, "store")?
            .call(&mut store, value)?;
        Ok(Self {
            value,
            load: instance.get_typed_func(&mut store, "load")?,
            oob: instance.get_typed_func(&mut store, "oob")?,
            trap: instance.get_typed_func(&mut store, "trap")?,
            spin: instance.get_typed_func(&mut store, "spin")?,
            store,
        })
    }

    /// Traps out of bounds and on `unreachable`, then checks the store is still usable and its
    /// memory still holds this guest's value. Every call gets a frame's fuel, as in the old
    /// engine's `call_logged`.
    pub fn trap_round(&mut self) -> Result<(), String> {
        self.store.set_fuel(FRAME_FUEL_LIMIT).map_err(text)?;
        expect_trap(
            self.oob.call(&mut self.store, ()).map(drop),
            Trap::MemoryOutOfBounds,
        )?;
        self.store.set_fuel(FRAME_FUEL_LIMIT).map_err(text)?;
        expect_trap(
            self.trap.call(&mut self.store, ()),
            Trap::UnreachableCodeReached,
        )?;
        self.store.set_fuel(FRAME_FUEL_LIMIT).map_err(text)?;
        match self.load.call(&mut self.store, ()) {
            Ok(value) if value == self.value => Ok(()),
            Ok(value) => Err(format!("memory holds {value}, not {}", self.value)),
            Err(e) => Err(format!("load failed after the traps: {e:#}")),
        }
    }

    /// Runs the endless loop with one frame's fuel. Returns how long it ran until halted.
    pub fn stop_by_fuel(&mut self) -> Result<Duration, String> {
        self.store.set_fuel(FRAME_FUEL_LIMIT).map_err(text)?;
        let started = Instant::now();
        let result = self.spin.call(&mut self.store, ());
        let ran = started.elapsed();
        expect_trap(result, Trap::OutOfFuel)?;
        Ok(ran)
    }
}

fn expect_trap(result: wasmtime::Result<()>, expected: Trap) -> Result<(), String> {
    match result {
        Ok(()) => Err(format!("returned instead of trapping with {expected:?}")),
        Err(e) if e.downcast_ref::<Trap>() == Some(&expected) => Ok(()),
        Err(e) => Err(format!("expected {expected:?}, got {e:#}")),
    }
}

fn text(e: wasmtime::Error) -> String {
    format!("{e:#}")
}
