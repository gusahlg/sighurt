//! A fixed number of threads that run queued jobs, oldest first, and jobs queued for a time,
//! when it comes.
//!
//! The adapter keeps at most one job per guest queued or running, so a pool of `n` threads runs
//! `n` guests at a time and takes the others in turn, however many views are open. A guest's
//! step job queues the guest's next step itself ([`Spawner::spawn_at`]), so a guest keeps its
//! cadence without the owner thread.
//!
//! A job that waits (a synchronous fetch, a file dialog: see [`blocking`]) hands its place to a
//! spare thread while it waits, so `n` guests waiting on a slow server don't stop every other
//! guest. Spares are bounded ([`MAX_SPARE_THREADS`] per pool); past that, waiting jobs hold
//! their threads until their own timeouts. A spare ends once the pool has its `n` running
//! threads again.
//!
//! Its threads have [`GUEST_THREAD_STACK`](crate::engine::GUEST_THREAD_STACK) of stack: guests
//! run on them, Wasm on the native stack.
//!
//! Owns: its threads, and the jobs queued on it: when the pool is dropped, its threads drop the
//! jobs still queued (and what they hold) rather than the thread that drops the pool. Must not
//! own: what the jobs do, or their results (jobs send those to the owner loop themselves).

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread;
use std::time::Instant;

type Job = Box<dyn FnOnce() + Send>;

/// Most threads a pool starts beyond its size for jobs that wait.
pub const MAX_SPARE_THREADS: usize = 16;

thread_local! {
    /// The pool this thread works for, if any.
    static POOL: RefCell<Weak<Shared>> = const { RefCell::new(Weak::new()) };
    /// Whether this thread is in [`blocking`].
    static BLOCKED: Cell<bool> = const { Cell::new(false) };
}

pub struct Pool {
    shared: Arc<Shared>,
}

/// Queues jobs on a pool from within its jobs; does nothing once the pool is gone.
#[derive(Clone)]
pub struct Spawner(Weak<Shared>);

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    name: String,
    /// Threads that run jobs, not counting those blocked in [`blocking`].
    size: usize,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    timed: BinaryHeap<Timed>,
    /// Orders jobs queued for the same time.
    serial: u64,
    closed: bool,
    /// Threads alive, and how many of them are in [`blocking`].
    threads: usize,
    blocked: usize,
}

/// A job queued for a time. The heap's greatest is the one due first.
struct Timed {
    at: Instant,
    serial: u64,
    job: Job,
}

impl Ord for Timed {
    fn cmp(&self, other: &Self) -> Ordering {
        (other.at, other.serial).cmp(&(self.at, self.serial))
    }
}

impl PartialOrd for Timed {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Timed {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Timed {}

impl Pool {
    /// Starts `threads` threads named `name`.
    pub fn new(name: &str, threads: usize) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::default(),
            ready: Condvar::new(),
            name: name.into(),
            size: threads,
        });
        for _ in 0..threads {
            shared.start()?;
            shared.lock().threads += 1;
        }
        Ok(Self { shared })
    }

    /// Queues `job` behind the jobs already queued.
    pub fn spawn(&self, job: impl FnOnce() + Send + 'static) {
        self.shared.push(None, Box::new(job));
    }

    pub fn spawner(&self) -> Spawner {
        Spawner(Arc::downgrade(&self.shared))
    }
}

impl Spawner {
    /// Queues `job` to run at `at`, or as soon after as a thread is free.
    pub fn spawn_at(&self, at: Instant, job: impl FnOnce() + Send + 'static) {
        if let Some(shared) = self.0.upgrade() {
            shared.push(Some(at), Box::new(job));
        }
    }
}

/// Runs `f`, which waits for something other than the CPU (the network, the user), on a pool
/// thread's behalf: while it waits, a spare thread runs the pool's other jobs. Outside a pool,
/// or already within `blocking`, it just runs `f`.
pub fn blocking<R>(f: impl FnOnce() -> R) -> R {
    let pool = POOL.with(|pool| pool.borrow().upgrade());
    let Some(shared) = pool.filter(|_| !BLOCKED.get()) else {
        return f();
    };
    /// Unblocks the thread however `f` ends.
    struct Unblock(Arc<Shared>);
    impl Drop for Unblock {
        fn drop(&mut self) {
            BLOCKED.set(false);
            self.0.lock().blocked -= 1;
            // Any thread that looks now finds the pool a thread over, and a spare ends.
            self.0.ready.notify_one();
        }
    }
    shared.block();
    BLOCKED.set(true);
    let _unblock = Unblock(shared);
    f()
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a thread working for the pool; the caller counts it.
    fn start(self: &Arc<Self>) -> io::Result<()> {
        let shared = self.clone();
        thread::Builder::new()
            .name(self.name.clone())
            .stack_size(crate::engine::GUEST_THREAD_STACK)
            .spawn(move || {
                POOL.with(|pool| *pool.borrow_mut() = Arc::downgrade(&shared));
                shared.work()
            })
            .map(drop)
    }

    /// The calling thread starts waiting: a spare takes its place if the pool has room.
    fn block(self: &Arc<Self>) {
        let mut queue = self.lock();
        queue.blocked += 1;
        let running = queue.threads - queue.blocked;
        if !queue.closed && running < self.size && queue.threads < self.size + MAX_SPARE_THREADS {
            // A thread that can't be started leaves the pool a thread short while this waits.
            if self.start().is_ok() {
                queue.threads += 1;
            }
        }
    }

    fn push(&self, at: Option<Instant>, job: Job) {
        let mut queue = self.lock();
        if queue.closed {
            drop(queue);
            return drop(job);
        }
        match at {
            None => queue.jobs.push_back(job),
            Some(at) => {
                queue.serial += 1;
                let serial = queue.serial;
                queue.timed.push(Timed { at, serial, job });
            }
        }
        // A thread waiting for a later job must look again.
        self.ready.notify_one();
    }

    fn work(&self) {
        loop {
            let job = {
                let mut queue = self.lock();
                loop {
                    if queue.threads - queue.blocked > self.size {
                        // A spare, and the thread it stood in for is back.
                        queue.threads -= 1;
                        return;
                    }
                    if queue.closed {
                        let left = (
                            std::mem::take(&mut queue.jobs),
                            std::mem::take(&mut queue.timed),
                        );
                        drop(queue);
                        return drop(left);
                    }
                    let now = Instant::now();
                    while queue.timed.peek().is_some_and(|timed| timed.at <= now) {
                        let timed = queue.timed.pop().expect("peeked");
                        queue.jobs.push_back(timed.job);
                    }
                    if let Some(job) = queue.jobs.pop_front() {
                        break job;
                    }
                    queue = match queue.timed.peek() {
                        Some(next) => {
                            let wait = next.at - now;
                            let ready = self.ready.wait_timeout(queue, wait);
                            ready.unwrap_or_else(PoisonError::into_inner).0
                        }
                        None => self
                            .ready
                            .wait(queue)
                            .unwrap_or_else(PoisonError::into_inner),
                    };
                }
            };
            job();
        }
    }
}

impl Drop for Pool {
    /// Lets each thread end after its current job; the first to see it drops the jobs still
    /// queued.
    fn drop(&mut self) {
        self.shared.lock().closed = true;
        self.shared.ready.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    #[test]
    fn jobs_run_in_order_on_one_thread() {
        let pool = Pool::new("test", 1).unwrap();
        let (tx, rx) = mpsc::channel();
        for i in 0..10 {
            let tx = tx.clone();
            pool.spawn(move || tx.send(i).unwrap());
        }
        let got: Vec<_> = (0..10)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(got, (0..10).collect::<Vec<_>>());
    }

    #[test]
    fn a_busy_thread_does_not_hold_up_the_others() {
        let pool = Pool::new("test", 2).unwrap();
        let (block, blocked) = mpsc::channel::<()>();
        pool.spawn(move || {
            let _ = blocked.recv();
        });
        let (tx, rx) = mpsc::channel();
        pool.spawn(move || tx.send(()).unwrap());
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(block);
    }

    /// Waits until `shared` has `threads` threads, or panics.
    fn wait_for_threads(shared: &Shared, threads: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while shared.lock().threads != threads {
            assert!(
                Instant::now() < deadline,
                "{} threads",
                shared.lock().threads
            );
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_waiting_job_hands_its_place_to_a_spare() {
        let pool = Pool::new("test", 1).unwrap();
        let (release, released) = mpsc::channel::<()>();
        let (tx, rx) = mpsc::channel();
        let waiting = tx.clone();
        pool.spawn(move || {
            blocking(|| released.recv()).unwrap();
            waiting.send("waited").unwrap();
        });
        pool.spawn(move || tx.send("ran").unwrap());
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok("ran"));
        assert_eq!(pool.shared.lock().threads, 2);
        release.send(()).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok("waited"));
        // The spare ends once the thread it stood in for is back.
        wait_for_threads(&pool.shared, 1);
    }

    #[test]
    fn spares_are_bounded() {
        let pool = Pool::new("test", 1).unwrap();
        let (release, released) = mpsc::channel::<()>();
        let released = Arc::new(Mutex::new(released));
        let (tx, rx) = mpsc::channel();
        let jobs = 1 + MAX_SPARE_THREADS + 3;
        for _ in 0..jobs {
            let (tx, released) = (tx.clone(), released.clone());
            pool.spawn(move || {
                tx.send(()).unwrap();
                blocking(|| released.lock().unwrap().recv()).unwrap();
            });
        }
        // Every thread the pool may have is waiting; the other jobs stay queued.
        for _ in 0..1 + MAX_SPARE_THREADS {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(pool.shared.lock().threads, 1 + MAX_SPARE_THREADS);
        for _ in 0..jobs {
            release.send(()).unwrap();
        }
        for _ in 0..3 {
            rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        wait_for_threads(&pool.shared, 1);
    }

    #[test]
    fn blocking_outside_a_pool_just_runs() {
        assert_eq!(blocking(|| blocking(|| 7)), 7);
    }

    #[test]
    fn timed_jobs_run_when_they_are_due_in_order() {
        let pool = Pool::new("test", 1).unwrap();
        let (tx, rx) = mpsc::channel();
        let now = Instant::now();
        for (i, ms) in [(1, 60), (0, 30), (2, 60)] {
            let tx = tx.clone();
            let at = now + Duration::from_millis(ms);
            pool.spawner()
                .spawn_at(at, move || tx.send((i, Instant::now() >= at)).unwrap());
        }
        let got: Vec<_> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(got, [(0, true), (1, true), (2, true)]);
    }

    #[test]
    fn a_dropped_pool_drops_its_jobs_on_its_threads() {
        struct Where(mpsc::Sender<thread::ThreadId>);
        impl Drop for Where {
            fn drop(&mut self) {
                let _ = self.0.send(thread::current().id());
            }
        }
        let pool = Pool::new("test", 1).unwrap();
        let (tx, rx) = mpsc::channel();
        let (block, blocked) = mpsc::channel::<()>();
        pool.spawn(move || {
            let _ = blocked.recv();
        });
        let queued = Where(tx.clone());
        pool.spawn(move || drop(queued));
        let timed = Where(tx);
        pool.spawner()
            .spawn_at(Instant::now() + Duration::from_secs(60), move || {
                drop(timed)
            });
        drop(pool);
        drop(block);
        for _ in 0..2 {
            let dropped_on = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert_ne!(dropped_on, thread::current().id());
        }
    }
}
