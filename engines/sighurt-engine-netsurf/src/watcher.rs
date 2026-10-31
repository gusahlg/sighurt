//! The thread that waits for the fetchers' descriptors, so the owner loop can sleep.
//!
//! NetSurf's fetchers are driven by curl, which reports the descriptors it waits on after each
//! poll. The owner hands the newest set to this thread; the thread `poll`s exactly that set, wakes
//! the owner through the engine's [`EngineWaker`] when something in it is ready, and then waits
//! for the owner's next set instead of polling a ready set again, so it cannot spin. It never
//! touches NetSurf. The shape is the WPE engine's `glib_loop.rs` watch.
//!
//! Owns: the descriptor wait. Must not own: any deadline (the runtime's `pump` returns those).

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::{Arc, Condvar, Mutex};

use sighurt_api::engine::EngineWaker;

#[derive(Default)]
struct State {
    fds: Vec<(i32, i16)>,
    /// Bumped with each new set; the thread polls a set once.
    generation: u64,
    /// The thread waits on the condition variable, not in `poll`.
    parked: bool,
    stop: bool,
}

/// The descriptor-watching thread and its handle.
pub struct Watcher {
    shared: Arc<(Mutex<State>, Condvar)>,
    interrupt: Arc<OwnedFd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn poke(fd: &OwnedFd) {
    let one = 1u64;
    // SAFETY: an eventfd write of eight bytes from a live local.
    unsafe { libc::write(fd.as_raw_fd(), (&one as *const u64).cast(), 8) };
}

impl Watcher {
    /// Starts the thread, with no set: it sleeps until [`set`](Self::set).
    pub fn start(waker: EngineWaker) -> Result<Self, String> {
        // SAFETY: a plain syscall.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return Err(format!("eventfd: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: a new descriptor, owned here.
        let interrupt = Arc::new(unsafe { OwnedFd::from_raw_fd(raw) });
        let shared = Arc::new((Mutex::new(State::default()), Condvar::new()));
        let (thread_shared, thread_interrupt) = (Arc::clone(&shared), Arc::clone(&interrupt));
        let thread = std::thread::Builder::new()
            .name("sighurt-netsurf-fds".into())
            .spawn(move || watch(&thread_shared, &thread_interrupt, &waker))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            interrupt,
            thread: Some(thread),
        })
    }

    /// Replaces the watched set. `fds` is `(descriptor, poll events)`.
    pub fn set(&self, fds: &[(i32, i16)]) {
        let (state, wake) = &*self.shared;
        let mut state = state.lock().expect("the watch state");
        state.fds.clear();
        state.fds.extend_from_slice(fds);
        state.generation += 1;
        let parked = state.parked;
        drop(state);
        if parked {
            wake.notify_one();
        } else {
            poke(&self.interrupt);
        }
    }

    /// Stops and joins the thread.
    pub fn stop(&mut self) {
        let (state, wake) = &*self.shared;
        state.lock().expect("the watch state").stop = true;
        wake.notify_one();
        poke(&self.interrupt);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

fn watch(shared: &(Mutex<State>, Condvar), interrupt: &OwnedFd, waker: &EngineWaker) {
    let (state, wake) = shared;
    let mut polled = 0;
    let mut woke_owner = true; // There is no set yet.
    let mut fds: Vec<libc::pollfd> = Vec::new();
    loop {
        {
            let mut guard = state.lock().expect("the watch state");
            while woke_owner && !guard.stop && guard.generation == polled {
                guard.parked = true;
                guard = wake.wait(guard).expect("the watch state");
                guard.parked = false;
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
        // An empty set (only the interrupt) sleeps until the owner has something to watch.
        // SAFETY: `fds` is a live, correctly sized array.
        unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, -1) };
        let (interrupted, set) = fds.split_last().expect("the interrupt is in every set");
        if interrupted.revents != 0 {
            let mut drained = 0u64;
            // SAFETY: reading eight bytes of a nonblocking eventfd into a local.
            unsafe { libc::read(interrupt.as_raw_fd(), (&mut drained as *mut u64).cast(), 8) };
        }
        woke_owner = set.iter().any(|fd| fd.revents != 0);
        if woke_owner {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use sighurt_api::engine::EngineWaker;

    use super::Watcher;

    #[test]
    fn a_ready_descriptor_wakes_the_owner_once_and_an_idle_set_sleeps() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&wakes);
        let waker = EngineWaker::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        let watcher = Watcher::start(waker.clone()).expect("the thread");
        let mut pipe = [0i32; 2];
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);
        watcher.set(&[(pipe[0], libc::POLLIN)]);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            wakes.load(Ordering::SeqCst),
            0,
            "an idle set woke the owner"
        );
        unsafe { libc::write(pipe[1], b"x".as_ptr().cast(), 1) };
        let end = Instant::now() + Duration::from_secs(2);
        while !waker.take() {
            assert!(Instant::now() < end, "no wake for a readable descriptor");
            std::thread::sleep(Duration::from_millis(5));
        }
        // The thread waits for the next set: the still-readable pipe does not wake it again.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(wakes.load(Ordering::SeqCst), 1, "the watcher spun");
        drop(watcher);
        let _ = pipe.map(|fd| unsafe { libc::close(fd) });
    }
}
