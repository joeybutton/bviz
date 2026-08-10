mod concurrent_interner;
mod model;
mod parse;

pub use model::{
    encloses_end, lane_class, occupied_span, span_role, CounterLane, CounterPoint, CounterSeries,
    LaneClass, Metadata, OptStr, PhaseMarker, Profile, Slice, SliceDetail, Span32, SpanRole, Str,
    ThreadTrack, TimeSpan, Workload, TIME_RESOLUTION_US,
};
pub use parse::LoadError;

/// Worker count for the parallel passes over profile data: the machine's
/// physical core count rather than its logical (SMT) thread count, because the
/// passes are memory-bandwidth bound, so threads past the cores that own the
/// memory channels mostly stall on bandwidth, burning CPU for a wall-time gain
/// that has flattened. Clamped to the parallelism the process is actually
/// granted, so a CPU quota or affinity mask is honored.
pub fn worker_count() -> usize {
    let granted = std::thread::available_parallelism().map_or(1, |n| n.get());
    num_cpus::get_physical().min(granted)
}
