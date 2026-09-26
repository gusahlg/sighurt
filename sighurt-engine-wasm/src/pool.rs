//! A fixed number of threads that run queued jobs, oldest first, and jobs queued for a time,
//! when it comes.
//!
//! The adapter keeps at most one job per guest queued or running, so a pool of `n` threads runs
//! `n` guests at a time and takes the others in turn, however many views are open. A guest's
//! step job queues the guest's next step itself ([`Spawner::spawn_at`]), so a guest keeps its
//! cadence without the owner thread.
//!
//! Owns: its threads, and the jobs queued on it: when the pool is dropped, its threads drop the
//! jobs still queued (and what they hold) rather than the thread that drops the pool. Must not
//! own: what the jobs do, or their results (jobs send those to the owner loop themselves).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::thread;
use std::time::Instant;

type Job = Box<dyn FnOnce() + Send>;

pub struct Pool {
    shared: Arc<Shared>,
}

/// Queues jobs on a pool from within its jobs; does nothing once the pool is gone.
#[derive(Clone)]
pub struct Spawner(Weak<Shared>);

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    timed: BinaryHeap<Timed>,
    /// Orders jobs queued for the same time.
    serial: u64,
    closed: bool,
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
        let shared = Arc::new(Shared::default());
        for _ in 0..threads {
            let shared = shared.clone();
            thread::Builder::new()
                .name(name.into())
                .spawn(move || shared.work())?;
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

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
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
