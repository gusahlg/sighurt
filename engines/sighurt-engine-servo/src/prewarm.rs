//! Servo's TLS prewarm thread, which [`ServoRuntime::shutdown`](crate::ServoRuntime) waits for.
//!
//! `Servo::new` (0.7, `servo.rs`) calls `net::connector::prewarm_tls`, which starts a thread
//! named `Net-TLS-prewarm` that draws 32 bytes from aws-lc's random generator, and never joins
//! it. That first draw makes the thread's generator state and gathers jitter entropy for it:
//! about 20 ms on an idle machine, several hundred on a loaded one. When the process exits
//! during it, aws-lc's exit destructor (`rand_thread_local_state_clear_all`) marks the
//! generator shut down; the thread then write-locks its new state against itself, its read
//! lock fails with `EDEADLK`, and aws-lc calls `abort()`, which mozjs redirects to
//! `mozalloc_abort`: `sig` dies with SIGSEGV instead of exiting 0. A quit within the first
//! fraction of a second after Servo is built is enough (the terminal tests' SIGTERM).
//!
//! [`Started`] notes the threads that appeared while Servo was built; [`Started::wait`] waits
//! until none of them is the prewarm thread, or a thread not yet named (a new thread has its
//! creator's name until it names itself), so the process can exit with its destructors. Threads
//! are found in `/proc/self/task`: Servo keeps no handle to the thread, and this avoids patching
//! servo-net. Every long-lived thread Servo starts names itself.
//!
//! Must not own: when the process exits (the executable), or waiting for any other thread:
//! the rest of Servo's threads end in `Servo`'s `Drop` or stay parked until the exit.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// The name `net::connector::prewarm_tls` gives its thread (15 bytes, the kernel's limit).
pub const PREWARM_THREAD: &str = "Net-TLS-prewarm";

/// The threads that appeared while Servo was built, and the name the building thread had then.
#[derive(Debug)]
pub struct Started {
    threads: Vec<u32>,
    creator: Option<String>,
}

impl Started {
    /// The threads of this process now. Call it just before building Servo and pass it to
    /// [`Self::since`] just after.
    pub fn threads() -> HashSet<u32> {
        thread_ids()
    }

    /// The threads that exist now and not in `before`.
    pub fn since(before: &HashSet<u32>) -> Self {
        let mut threads: Vec<u32> = thread_ids().difference(before).copied().collect();
        threads.sort_unstable();
        Self {
            threads,
            creator: std::fs::read_to_string("/proc/thread-self/comm")
                .ok()
                .map(|name| name.trim_end().to_owned()),
        }
    }

    /// The started threads that may still draw from the random generator for the first time:
    /// the prewarm thread, and any not named yet.
    fn pending(&self) -> Vec<u32> {
        self.threads
            .iter()
            .copied()
            .filter(|&tid| match thread_name(tid) {
                Some(name) => name == PREWARM_THREAD || Some(&name) == self.creator.as_ref(),
                None => false,
            })
            .collect()
    }

    /// Waits until [`Self::pending`] is empty, at most `bound`. Whether it is.
    pub fn wait(&self, bound: Duration) -> bool {
        let start = Instant::now();
        loop {
            let pending = self.pending();
            if pending.is_empty() {
                return true;
            }
            if start.elapsed() >= bound {
                log::warn!(
                    "servo: {} of the threads Servo started at launch still running after {:?}",
                    pending.len(),
                    bound
                );
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// This process's thread ids, from `/proc/self/task`; empty if it can't be read.
fn thread_ids() -> HashSet<u32> {
    let Ok(dir) = std::fs::read_dir("/proc/self/task") else {
        return HashSet::new();
    };
    dir.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .collect()
}

/// Thread `tid`'s name, or `None` once it has exited.
fn thread_name(tid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/self/task/{tid}/comm"))
        .ok()
        .map(|name| name.trim_end().to_owned())
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::sync::mpsc;

    use super::*;

    /// Names the calling thread, as std does for a named thread when it starts.
    fn name_this_thread(name: &str) {
        let name = CString::new(name).unwrap();
        // SAFETY: a NUL-terminated name of at most 15 bytes, for the calling thread.
        unsafe { libc::prctl(libc::PR_SET_NAME, name.as_ptr()) };
    }

    /// One test, so that no other test of this module starts threads with the same creator
    /// name while one of these scenarios looks at the threads that appeared.
    #[test]
    fn waits_for_the_prewarm_thread_and_unnamed_ones_but_no_other() {
        waits_for_the_prewarm_thread_until_it_exits();
        a_thread_not_named_yet_is_waited_for();
        named_threads_that_stay_are_not_waited_for();
        a_bound_ends_the_wait();
    }

    fn waits_for_the_prewarm_thread_until_it_exits() {
        let before = Started::threads();
        let (named, wait_named) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(PREWARM_THREAD.into())
            .spawn(move || {
                named.send(()).unwrap();
                std::thread::sleep(Duration::from_millis(200));
            })
            .unwrap();
        let started = Started::since(&before);
        wait_named.recv().unwrap();
        assert!(!started.pending().is_empty());
        let at = Instant::now();
        assert!(started.wait(Duration::from_secs(10)));
        assert!(at.elapsed() >= Duration::from_millis(100));
        assert!(started.pending().is_empty());
        thread.join().unwrap();
    }

    fn a_thread_not_named_yet_is_waited_for() {
        let before = Started::threads();
        let (go, wait_go) = mpsc::channel::<()>();
        let (done, wait_done) = mpsc::channel();
        // Unnamed: it has its creator's name, as a new thread has before it names itself.
        let thread = std::thread::spawn(move || {
            wait_go.recv().unwrap();
            name_this_thread(PREWARM_THREAD);
            std::thread::sleep(Duration::from_millis(100));
            done.send(()).unwrap();
        });
        let started = Started::since(&before);
        assert_eq!(started.pending().len(), 1, "{started:?}");
        go.send(()).unwrap();
        assert!(started.wait(Duration::from_secs(10)));
        // It exited, so it had finished its work.
        wait_done.try_recv().unwrap();
        thread.join().unwrap();
    }

    fn named_threads_that_stay_are_not_waited_for() {
        let before = Started::threads();
        let (stop, wait_stop) = mpsc::channel::<()>();
        let (named, wait_named) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("Constellation".into())
            .spawn(move || {
                named.send(()).unwrap();
                let _ = wait_stop.recv();
            })
            .unwrap();
        wait_named.recv().unwrap();
        let started = Started::since(&before);
        let at = Instant::now();
        assert!(started.wait(Duration::from_secs(10)));
        assert!(at.elapsed() < Duration::from_secs(1));
        drop(stop);
        thread.join().unwrap();
    }

    fn a_bound_ends_the_wait() {
        let before = Started::threads();
        let (stop, wait_stop) = mpsc::channel::<()>();
        let (named, wait_named) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name(PREWARM_THREAD.into())
            .spawn(move || {
                named.send(()).unwrap();
                let _ = wait_stop.recv();
            })
            .unwrap();
        wait_named.recv().unwrap();
        let started = Started::since(&before);
        assert!(!started.wait(Duration::from_millis(50)));
        drop(stop);
        thread.join().unwrap();
    }
}
