//! The thread that turns "Ladybird's loop descriptor is readable" into an owner-loop wake.
//!
//! It polls one descriptor (the shim's epoll descriptor) and a stop descriptor, never touches
//! Ladybird, and after waking the owner it sleeps until the owner has pumped, so a descriptor that
//! stays readable until the pump handles it cannot make it spin.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use sighurt_api::engine::EngineWaker;

struct Shared {
    /// Pumps done so far.
    pumps: Mutex<u64>,
    pumped: Condvar,
    stop: AtomicBool,
}

/// The waiting thread, and its handle for the pump and for shutdown.
pub struct Waiter {
    shared: Arc<Shared>,
    stop_fd: libc::c_int,
    thread: Option<JoinHandle<()>>,
}

impl Waiter {
    /// Starts watching `fd` (not owned: the shim's).
    pub fn start(fd: libc::c_int, waker: EngineWaker) -> Result<Self, String> {
        let stop_fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if stop_fd < 0 {
            return Err(format!("eventfd: {}", std::io::Error::last_os_error()));
        }
        let shared = Arc::new(Shared {
            pumps: Mutex::new(0),
            pumped: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let thread_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("ladybird-wait".into())
            .spawn(move || watch(fd, stop_fd, &thread_shared, &waker))
            .map_err(|e| format!("cannot start the Ladybird wait thread: {e}"))?;
        Ok(Self {
            shared,
            stop_fd,
            thread: Some(thread),
        })
    }

    /// The owner is about to pump: whatever made the descriptor readable is being handled.
    pub fn pumping(&self) {
        *self.shared.pumps.lock().expect("not poisoned") += 1;
        self.shared.pumped.notify_all();
    }

    /// Stops the thread and waits for it.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        let one: u64 = 1;
        unsafe { libc::write(self.stop_fd, (&raw const one).cast(), 8) };
        self.shared.pumped.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Waiter {
    fn drop(&mut self) {
        self.stop();
        unsafe { libc::close(self.stop_fd) };
    }
}

fn watch(fd: libc::c_int, stop_fd: libc::c_int, shared: &Shared, waker: &EngineWaker) {
    loop {
        let mut fds = [
            libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stop_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return;
        }
        if shared.stop.load(Ordering::SeqCst) || fds[1].revents != 0 {
            return;
        }
        if fds[0].revents & libc::POLLIN != 0 {
            let seen = *shared.pumps.lock().expect("not poisoned");
            waker.wake();
            let mut pumps = shared.pumps.lock().expect("not poisoned");
            while *pumps == seen && !shared.stop.load(Ordering::SeqCst) {
                pumps = shared
                    .pumped
                    .wait_timeout(pumps, Duration::from_millis(500))
                    .expect("not poisoned")
                    .0;
            }
        }
    }
}
