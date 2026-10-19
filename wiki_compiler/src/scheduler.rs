use rayon::ScopeFifo;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::sync::{Arc, Mutex};

/// How urgent a job is. A bigger value runs first.
///
/// Fields are compared in the order they are written, so `unknown` outranks
/// `cost`, and `cost` outranks `size`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Priority {
    /// True for a job that has never been timed (new, renamed or edited).
    ///
    /// These run before every timed job. Their duration could be anything, so
    /// it is best to find out early, and their timing is then recorded for the
    /// next run.
    pub unknown: bool,
    /// For a timed job, the fastest it has ever taken, in milliseconds.
    ///
    /// For an untimed job, an estimate such as its file size or pixel area.
    pub cost: u64,
    /// Breaks ties between jobs with equal cost.
    pub size: u64,
}

type Job<'scope> = Box<dyn FnOnce(&ScopeFifo<'scope>) + Send + 'scope>;

struct Entry<'scope> {
    priority: Priority,
    /// The order of submission.
    ///
    /// Jobs with equal priority run in submission order, so every run is the
    /// same.
    seq: u64,
    job: Job<'scope>,
}

impl PartialEq for Entry<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}
impl Eq for Entry<'_> {}
impl PartialOrd for Entry<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Entry<'_> {
    fn cmp(&self, other: &Self) -> Ordering {
        // `BinaryHeap` pops the greatest entry first.
        //
        // A higher priority is greater. For equal priorities the `seq`
        // comparison is reversed, so the job submitted earlier is greater and
        // pops first.
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.seq.cmp(&self.seq))
    }
}

struct Inner<'scope> {
    heap: BinaryHeap<Entry<'scope>>,
    next_seq: u64,
}

/// Runs jobs on a rayon scope, always starting the most urgent job waiting at
/// that moment.
///
/// `spawn_fifo` starts tasks in the order they were spawned. That only works
/// when every task is known up front. Here a running job can create more jobs:
/// once an SVG is parsed, it produces one job per PNG. Those jobs must be
/// placed among everything already waiting, by how long they are expected to
/// take, instead of going to the back.
///
/// Every submitted job is pushed onto a priority heap, and one rayon task is
/// spawned for it. That task does not run its own job. It runs whichever job is
/// the most urgent when it starts. A job is always available, because a task is
/// only spawned after its job was queued.
///
/// Each task runs exactly one job and then ends. Nothing waits or loops, so a
/// worker that picks up other work while blocked inside oxipng cannot get
/// stuck.
pub struct Scheduler<'scope> {
    inner: Mutex<Inner<'scope>>,
}

impl<'scope> Scheduler<'scope> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                heap: BinaryHeap::new(),
                next_seq: 0,
            }),
        })
    }

    /// Queues `job`.
    ///
    /// The job receives the scope when it runs, so it can submit more jobs.
    pub fn submit(
        self: &Arc<Self>,
        scope: &ScopeFifo<'scope>,
        priority: Priority,
        job: impl FnOnce(&ScopeFifo<'scope>) + Send + 'scope,
    ) {
        {
            let mut inner = self.inner.lock().unwrap();
            let seq = inner.next_seq;
            inner.next_seq += 1;
            inner.heap.push(Entry {
                priority,
                seq,
                job: Box::new(job),
            });
        }

        let this = Arc::clone(self);
        scope.spawn_fifo(move |scope| {
            // The lock is released at the end of this statement, before the
            // job runs. Holding it during the job would block every other
            // worker from taking its own job.
            let entry = this
                .inner
                .lock()
                .unwrap()
                .heap
                .pop()
                .expect("a task is only spawned for a job that was queued");
            (entry.job)(scope);
        });
    }
}
