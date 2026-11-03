//! Dedicated preview threads fed newest-first from a bounded queue, so preview requests
//! never park the async runtime's blocking threads (which Tauri commands also need).

use std::collections::VecDeque;
use std::panic::AssertUnwindSafe;
use std::sync::{Condvar, Mutex, MutexGuard, Once};

/// Each thread loads an attachment, hashes it, looks up the disk cache and on a miss
/// renders it, waiting for a slot of the renderer it needs: enough threads for every
/// renderer slot plus two that keep serving cache hits meanwhile.
const THREADS: usize = if cfg!(target_os = "macos") { 16 } else { 8 };
/// Requests waiting beyond this are turned away oldest first: the grid scrolled past them.
/// Waiting requests hold no attachment bytes, and newer ones run first anyway, so this
/// only bounds the work spent on tiles nobody looks at any more.
const MAX_WAITING: usize = 256;

/// Called with `true` to do the work, or `false` when turned away (answer "busy").
type Job = Box<dyn FnOnce(bool) + Send>;

/// Waiting jobs, oldest first.
struct Queue {
    jobs: VecDeque<Job>,
}

impl Queue {
    /// Adds `job`; returns the job turned away to make room, if any.
    fn push(&mut self, job: Job) -> Option<Job> {
        self.jobs.push_back(job);
        if self.jobs.len() > MAX_WAITING {
            self.jobs.pop_front()
        } else {
            None
        }
    }

    /// The newest waiting job.
    fn pop(&mut self) -> Option<Job> {
        self.jobs.pop_back()
    }
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { jobs: VecDeque::new() });
static READY: Condvar = Condvar::new();

fn lock() -> MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|p| p.into_inner())
}

/// Runs `job(true)` on a preview thread, newest submission first, so the tiles on screen
/// go before those scrolled past; the caller does not wait. When more than
/// [`MAX_WAITING`] jobs wait, the oldest is called with `false` at once instead.
pub fn submit(job: impl FnOnce(bool) + Send + 'static) {
    static START: Once = Once::new();
    START.call_once(|| {
        for n in 0..THREADS {
            // Fewer threads only means fewer renders in parallel.
            let _ = std::thread::Builder::new().name(format!("stv-preview-{n}")).spawn(serve);
        }
    });
    let turned_away = lock().push(Box::new(job));
    READY.notify_one();
    if let Some(job) = turned_away {
        job(false);
    }
}

fn serve() {
    loop {
        let job = {
            let mut queue = lock();
            loop {
                match queue.pop() {
                    Some(job) => break job,
                    None => queue = READY.wait(queue).unwrap_or_else(|p| p.into_inner()),
                }
            }
        };
        // A panicking decoder must not take the thread with it.
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| job(true)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn newest_runs_first_and_the_oldest_is_turned_away() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let mut queue = Queue { jobs: VecDeque::new() };
        let job = |n: usize| -> Job {
            let log = log.clone();
            Box::new(move |run| log.lock().unwrap().push((n, run)))
        };
        for n in 0..MAX_WAITING {
            assert!(queue.push(job(n)).is_none());
        }
        queue.push(job(MAX_WAITING)).expect("queue is full")(false);
        queue.pop().unwrap()(true);
        assert_eq!(*log.lock().unwrap(), [(0, false), (MAX_WAITING, true)]);
    }
}
