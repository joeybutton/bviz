//! How wide to fan work out across the machine.

/// Worker count for a parallel pass over bulk data: the machine's physical core
/// count rather than its logical (SMT) thread count, because such a pass is
/// memory-bandwidth bound, so threads past the cores that own the memory
/// channels mostly stall on bandwidth, burning CPU for a wall-time gain that has
/// flattened. Clamped to the parallelism the process is actually granted, so a
/// CPU quota or affinity mask is honored.
pub fn worker_count() -> usize {
    let granted = std::thread::available_parallelism().map_or(1, |n| n.get());
    num_cpus::get_physical().min(granted)
}
