//! A small priority thread pool, the equivalent of `QThreadPool.start(job,
//! priority)`: higher priority runs sooner, equal priorities run in
//! submission order, and queued-but-not-started jobs can be dropped by owner.

use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::thread;

use parking_lot::{Condvar, Mutex};

struct Entry<J> {
    priority: i32,
    seq: u64,
    owner: u32,
    job: J,
}

impl<J> PartialEq for Entry<J> {
    fn eq(&self, other: &Self) -> bool {
        self.priority == other.priority && self.seq == other.seq
    }
}
impl<J> Eq for Entry<J> {}
impl<J> PartialOrd for Entry<J> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl<J> Ord for Entry<J> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority.cmp(&other.priority).then_with(|| other.seq.cmp(&self.seq))
    }
}

struct Queue<J> {
    heap: BinaryHeap<Entry<J>>,
    seq: u64,
}

struct Shared<J> {
    queue: Mutex<Queue<J>>,
    ready: Condvar,
    stop: AtomicBool,
}

pub struct PriorityPool<J: Send + 'static> {
    shared: Arc<Shared<J>>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl<J: Send + 'static> PriorityPool<J> {
    pub fn new(name: &str, threads: usize, handler: impl Fn(J) + Send + Sync + 'static) -> Self {
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { heap: BinaryHeap::new(), seq: 0 }),
            ready: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let handler = Arc::new(handler);
        let workers = (0..threads.max(1))
            .map(|i| {
                let shared = shared.clone();
                let handler = handler.clone();
                thread::Builder::new()
                    .name(format!("{name}-{i}"))
                    .spawn(move || loop {
                        let job = {
                            let mut q = shared.queue.lock();
                            loop {
                                if shared.stop.load(AtomicOrdering::Relaxed) {
                                    return;
                                }
                                if let Some(e) = q.heap.pop() {
                                    break e.job;
                                }
                                shared.ready.wait(&mut q);
                            }
                        };
                        handler(job);
                    })
                    .expect("spawn worker")
            })
            .collect();
        Self { shared, workers }
    }

    pub fn submit(&self, owner: u32, priority: i32, job: J) {
        let mut q = self.shared.queue.lock();
        q.seq += 1;
        let seq = q.seq;
        q.heap.push(Entry { priority, seq, owner, job });
        drop(q);
        self.shared.ready.notify_one();
    }

    /// Drop every queued (not yet started) job belonging to `owner`.
    pub fn clear(&self, owner: u32) {
        let mut q = self.shared.queue.lock();
        let old = std::mem::take(&mut q.heap);
        q.heap = old.into_iter().filter(|e| e.owner != owner).collect();
    }

    pub fn clear_all(&self) {
        self.shared.queue.lock().heap.clear();
    }

}

impl<J: Send + 'static> Drop for PriorityPool<J> {
    fn drop(&mut self) {
        self.shared.stop.store(true, AtomicOrdering::Relaxed);
        self.shared.queue.lock().heap.clear();
        self.shared.ready.notify_all();
        // Running jobs finish on their own; don't block shutdown on them.
        for w in self.workers.drain(..) {
            drop(w);
        }
    }
}
