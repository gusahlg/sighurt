//! Shutting Servo down right after it is built, as `sig` does when it is told to quit during
//! its first fraction of a second.
//!
//! One test: Servo initializes once per process. `Servo::new` starts a thread that seeds aws-lc's
//! random generator and never joins it; a process that exits while it still runs is aborted by
//! aws-lc's exit destructor (`src/prewarm.rs`). `shutdown` must return only once that thread has
//! ended, so the process can then exit with its destructors.

use std::time::Instant;

use sighurt_api::engine::{EngineRuntime, EngineWaker};
use sighurt_engine_servo::{ServoConfig, ServoRuntime};

/// The thread `net::connector::prewarm_tls` starts.
const PREWARM_THREAD: &str = "Net-TLS-prewarm";

/// The names of this process's threads.
fn thread_names() -> Vec<String> {
    std::fs::read_dir("/proc/self/task")
        .expect("/proc/self/task")
        .filter_map(|entry| {
            let path = entry.ok()?.path().join("comm");
            Some(std::fs::read_to_string(path).ok()?.trim_end().to_owned())
        })
        .collect()
}

#[test]
fn shutdown_at_launch_returns_after_the_tls_prewarm_thread() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let built = Instant::now();
    let mut runtime: ServoRuntime =
        ServoRuntime::new(ServoConfig::default(), EngineWaker::new(|| {}));
    let running = thread_names();
    runtime.shutdown();
    let after = thread_names();
    eprintln!(
        "prewarm thread at build: {}; shutdown done {:?} after the build",
        running.iter().any(|name| name == PREWARM_THREAD),
        built.elapsed()
    );
    assert!(
        !after.iter().any(|name| name == PREWARM_THREAD),
        "Servo's TLS prewarm thread outlived shutdown: {after:?}"
    );
    assert!(!runtime.diagnostics().leaked);
}
