//! WebKit's GLib main context, run from the owner loop without busy polling.
//!
//! WebKit's UI-process side lives on the default `GMainContext` of the thread that first called
//! it: its IPC with the web and network processes, its timers and the fences it waits on are
//! GLib sources there. The owner thread acquires that context and runs GLib's own iteration
//! protocol in two halves: [`GlibLoop::iterate`] checks and dispatches what is ready, then
//! prepares and queries the next wait, and returns when GLib next needs a turn (a timeout). The
//! wait itself is a small thread, [`Watch`], that `poll`s the file descriptors GLib asked for
//! and wakes the owner through the engine's [`EngineWaker`] when one is ready; it never touches
//! GLib, and after a wake it waits for the next set instead of polling the same one again. The
//! owner re-polls with a zero timeout just before `check`, so GLib always sees fresh readiness.
//! Sources other threads attach signal GLib's wakeup descriptor, which is in every set.
//!
//! Must not own: what WebKit's sources do, or when the owner pumps (it pumps on the waker and
//! at the deadline this returns).

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use glib::ffi::{GMainContext, GPollFD, GFALSE};
use sighurt_api::engine::EngineWaker;

/// The owner's side: the acquired default context and the last query's descriptors.
pub struct GlibLoop {
    context: *mut GMainContext,
    fds: Vec<GPollFD>,
    max_priority: i32,
    queried: bool,
    watch: Watch,
    /// Dispatches so far, for diagnostics.
    pub dispatches: u64,
}

impl GlibLoop {
    /// Acquires the thread's default main context; fails if another thread owns it.
    pub fn new(waker: EngineWaker) -> Result<Self, String> {
        let context = unsafe { glib::ffi::g_main_context_default() };
        if unsafe { glib::ffi::g_main_context_acquire(context) } == GFALSE {
            return Err("GLib's default main context belongs to another thread".into());
        }
        Ok(Self {
            context,
            fds: Vec::new(),
            max_priority: 0,
            queried: false,
            watch: Watch::start(waker)?,
            dispatches: 0,
        })
    }

    /// Dispatches what is ready, then prepares the next wait. Returns when GLib needs another
    /// turn even if no descriptor becomes ready: `None` for never, `Some(now)` for at once.
    pub fn iterate(&mut self) -> Option<Instant> {
        if self.queried {
            // Fresh readiness for `check`: the watch thread may have seen it long ago, or not yet.
            for fd in &mut self.fds {
                fd.revents = 0;
            }
            unsafe { libc::poll(self.fds.as_mut_ptr().cast(), self.fds.len() as _, 0) };
            let ctx = self.context;
            let ready = unsafe {
                glib::ffi::g_main_context_check(
                    ctx,
                    self.max_priority,
                    self.fds.as_mut_ptr(),
                    self.fds.len() as i32,
                )
            };
            if ready != GFALSE {
                unsafe { glib::ffi::g_main_context_dispatch(ctx) };
                self.dispatches += 1;
            }
        }
        let ready_now =
            unsafe { glib::ffi::g_main_context_prepare(self.context, &mut self.max_priority) };
        let mut timeout_ms = -1;
        loop {
            let capacity = self.fds.capacity().max(8);
            self.fds.resize(
                capacity,
                GPollFD {
                    fd: -1,
                    events: 0,
                    revents: 0,
                },
            );
            let n = unsafe {
                glib::ffi::g_main_context_query(
                    self.context,
                    self.max_priority,
                    &mut timeout_ms,
                    self.fds.as_mut_ptr(),
                    capacity as i32,
                )
            } as usize;
            if n <= capacity {
                self.fds.truncate(n);
                break;
            }
            self.fds.reserve(n - capacity);
        }
        self.queried = true;
        self.watch.set(&self.fds);
        let now = Instant::now();
        match (ready_now != GFALSE, timeout_ms) {
            (true, _) | (_, 0) => Some(now),
            (false, t) if t < 0 => None,
            (false, t) => Some(now + Duration::from_millis(t as u64)),
        }
    }
}

impl Drop for GlibLoop {
    fn drop(&mut self) {
        self.watch.stop();
        unsafe { glib::ffi::g_main_context_release(self.context) };
    }
}

/// The waiting thread, and what it waits on.
struct Watch {
    shared: Arc<(Mutex<WatchState>, Condvar)>,
    /// Interrupts the thread's `poll` when the set changes.
    interrupt: Arc<OwnedFd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct WatchState {
    /// Descriptors and the events GLib asked for.
    fds: Vec<(i32, i16)>,
    /// Bumped with each new set; the thread polls a set once.
    generation: u64,
    stop: bool,
}

impl Watch {
    fn start(waker: EngineWaker) -> Result<Self, String> {
        let interrupt = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if interrupt < 0 {
            return Err(format!("eventfd: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: a new descriptor, owned here.
        let interrupt =
            Arc::new(unsafe { <OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(interrupt) });
        let shared = Arc::new((Mutex::new(WatchState::default()), Condvar::new()));
        let (thread_shared, thread_interrupt) = (Arc::clone(&shared), Arc::clone(&interrupt));
        let thread = std::thread::Builder::new()
            .name("sighurt-wpe-glib".into())
            .spawn(move || watch(&thread_shared, &thread_interrupt, &waker))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            interrupt,
            thread: Some(thread),
        })
    }

    fn set(&self, fds: &[GPollFD]) {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("the watch state");
        state.fds.clear();
        state
            .fds
            .extend(fds.iter().map(|fd| (fd.fd, fd.events as i16)));
        state.generation += 1;
        drop(state);
        wake.notify_one();
        let one = 1u64;
        unsafe { libc::write(self.interrupt.as_raw_fd(), (&one as *const u64).cast(), 8) };
    }

    fn stop(&mut self) {
        let (state, wake) = &*self.shared;
        state.lock().expect("the watch state").stop = true;
        wake.notify_one();
        let one = 1u64;
        unsafe { libc::write(self.interrupt.as_raw_fd(), (&one as *const u64).cast(), 8) };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The thread: polls the newest set until something in it is ready, wakes the owner, and then
/// waits for the owner's next set rather than polling a ready set again (no spinning).
fn watch(shared: &(Mutex<WatchState>, Condvar), interrupt: &OwnedFd, waker: &EngineWaker) {
    let (state, wake) = shared;
    let mut polled = 0;
    let mut woke_owner = true; // There is no set yet.
    let mut fds: Vec<libc::pollfd> = Vec::new();
    loop {
        {
            let mut guard = state.lock().expect("the watch state");
            while woke_owner && !guard.stop && guard.generation == polled {
                guard = wake.wait(guard).expect("the watch state");
            }
            if guard.stop {
                return;
            }
            if guard.generation != polled {
                polled = guard.generation;
                fds.clear();
                fds.extend(guard.fds.iter().map(|&(fd, events)| libc::pollfd {
                    fd,
                    events,
                    revents: 0,
                }));
                fds.push(libc::pollfd {
                    fd: interrupt.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                });
            }
        }
        for fd in &mut fds {
            fd.revents = 0;
        }
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        let mut drained = 0u64;
        unsafe { libc::read(interrupt.as_raw_fd(), (&mut drained as *mut u64).cast(), 8) };
        woke_owner = fds[..fds.len() - 1].iter().any(|fd| fd.revents != 0);
        if woke_owner {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sighurt_api::engine::EngineWaker;

    use super::GlibLoop;

    /// One test owns the process's default context; GLib allows one owner thread at a time.
    #[test]
    fn sources_wake_the_owner_and_an_idle_context_sleeps() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let waker = EngineWaker::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let mut glib = GlibLoop::new(waker.clone()).expect("the default context");
        let wait_for_wake = |what: &str| {
            let end = Instant::now() + Duration::from_secs(2);
            while !waker.take() {
                assert!(Instant::now() < end, "no wake for {what}");
                std::thread::sleep(Duration::from_millis(5));
            }
        };

        // Nothing attached: no deadline, and the watch thread doesn't wake anyone.
        assert_eq!(glib.iterate(), None);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            0,
            "an idle context woke the owner"
        );

        // A source attached from another thread wakes the owner through GLib's wakeup fd, and
        // the next iteration dispatches it.
        let ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&ran);
        std::thread::spawn(move || {
            glib::MainContext::default().invoke(move || flag.store(true, Ordering::SeqCst));
        })
        .join()
        .expect("the other thread");
        wait_for_wake("a source from another thread");
        while glib.iterate().is_some_and(|at| at <= Instant::now()) {}
        assert!(ran.load(Ordering::SeqCst), "the source was not dispatched");

        // A timeout is a deadline for the owner, not a wake.
        let fired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&fired);
        glib::timeout_add_once(Duration::from_millis(50), move || {
            flag.store(true, Ordering::SeqCst)
        });
        let deadline = glib.iterate().expect("a deadline for the timeout");
        assert!(deadline > Instant::now() + Duration::from_millis(20));
        let woken = wakes.load(Ordering::SeqCst);
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            woken,
            "the timeout woke the owner"
        );
        while !fired.load(Ordering::SeqCst) {
            glib.iterate();
        }
        assert_eq!(glib.iterate(), None);
    }
}
