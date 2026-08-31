//! How wide to fan work out across the machine.

use std::sync::OnceLock;

/// Worker count for a parallel pass over bulk data: the machine's physical core
/// count rather than its logical (SMT) thread count, because such a pass is
/// memory-bandwidth bound, so threads past the cores that own the memory
/// channels mostly stall on bandwidth, burning CPU for a wall-time gain that has
/// flattened. Clamped to the parallelism the process is actually granted, so a
/// CPU quota or affinity mask is honored, and to at least one worker, so a
/// caller may size, multiply or divide by it unguarded.
pub fn worker_count() -> usize {
    let granted = std::thread::available_parallelism().map_or(1, |n| n.get());
    num_cpus::get_physical().min(granted).max(1)
}

/// Stack for a worker: the graph layout engine recurses over the frame it is
/// given, so the depth follows the frame's size, and placing a whole graph hands
/// it far larger frames than laying out one view does. Sized well clear of what
/// the largest frames of a whole-repository build graph reach.
const WORKER_STACK: usize = 8 << 20;

/// The process's one pool for parallel passes over bulk data, [`worker_count`]
/// wide. Every rayon entry point runs inside an `install` or `scope` on it, so
/// however many passes are in flight the process fans out that wide and no
/// wider: a parallel iterator left outside runs instead on rayon's implicit
/// global pool, a second set of workers sized by logical threads.
pub fn workers() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(worker_count())
            .stack_size(WORKER_STACK)
            .build()
            .expect("build the worker pool")
    })
}
