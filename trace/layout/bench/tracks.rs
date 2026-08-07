//! Benchmarks the UI's real flame-graph path: `ScopeAggregate::build_for_tracks`
//! is rebuilt from scratch on every track-selection change. Times a single-track
//! selection (the largest lane, walked serially by one worker) and the all-track
//! selection (walked in parallel), the two ends the interactive profile exercised,
//! plus the all-track rebuild from a reused whole-profile scan (`TrackFlamer`),
//! which is what the view does for every selection after the first, in both the
//! top-down and the bottom-up presentation.

use std::sync::Arc;

use bviz_trace_layout::{FlameDirection, FlameMode, ScopeAggregate, TimeSelection, TrackFlamer};
use bviz_trace_profile::{LaneClass, Profile};

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

fn min_secs(iters: usize, mut f: impl FnMut()) -> f64 {
    (0..iters)
        .map(|_| {
            let t = std::time::Instant::now();
            f();
            t.elapsed().as_secs_f64()
        })
        .fold(f64::INFINITY, f64::min)
}

fn main() {
    bviz_alloc::retain_freed_memory_across_load();
    bviz_trace_layout::init_thread_pool();
    let path = std::env::args().nth(1).expect("usage: bench_tracks <profile>");
    let iters = std::env::var("BVIZ_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let profile = Arc::new(Profile::from_path(std::path::Path::new(&path)).expect("load profile"));
    let threads = profile.threads();

    // The largest non-critical-path lane by slice count — a single-track
    // selection lands its whole merge on one worker.
    let (biggest, big_slices) = threads
        .iter()
        .enumerate()
        .filter(|(_, t)| t.lane_class != LaneClass::CriticalPath)
        .map(|(i, t)| (i, profile.slices(t).len()))
        .max_by_key(|&(_, n)| n)
        .expect("a real lane");
    let real_lanes = threads
        .iter()
        .filter(|t| t.lane_class != LaneClass::CriticalPath)
        .count();
    let total_slices: usize = threads
        .iter()
        .filter(|t| t.lane_class != LaneClass::CriticalPath)
        .map(|t| profile.slices(t).len())
        .sum();

    // The presentation the view opens on, over the whole timeline.
    let mode = FlameMode::default();
    let window = TimeSelection::default();
    let single = min_secs(iters, || {
        std::hint::black_box(ScopeAggregate::build_for_tracks(
            &profile,
            mode,
            &window,
            |i| i == biggest,
        ));
    });
    let all = min_secs(iters, || {
        std::hint::black_box(ScopeAggregate::build_for_tracks(
            &profile,
            mode,
            &window,
            |i| threads[i].lane_class != LaneClass::CriticalPath,
        ));
    });
    // The whole-profile scan the view holds after the first build; every later
    // selection rebuilds from it rather than rescanning.
    let scan_cold = min_secs(iters, || {
        std::hint::black_box(TrackFlamer::new(&profile));
    });
    // The first rebuild off a scan pays that labelling's per-lane reduction, so
    // this timing is warm: the pure merge every selection change after it costs.
    let scan = TrackFlamer::new(&profile);
    let all_reused = min_secs(iters, || {
        std::hint::black_box(ScopeAggregate::build_for_tracks_reusing(
            &scan,
            mode,
            &window,
            |i| threads[i].lane_class != LaneClass::CriticalPath,
        ));
    });

    // The same rebuild presented bottom-up, so the gap against `all_reused` is
    // what inverting a built graph costs.
    let all_inverted = min_secs(iters, || {
        std::hint::black_box(ScopeAggregate::build_for_tracks_reusing(
            &scan,
            FlameMode { direction: FlameDirection::BottomUp, ..mode },
            &window,
            |i| threads[i].lane_class != LaneClass::CriticalPath,
        ));
    });

    let bottom_up_agg = ScopeAggregate::build_for_tracks_reusing(
        &scan,
        FlameMode { direction: FlameDirection::BottomUp, ..mode },
        &window,
        |i| threads[i].lane_class != LaneClass::CriticalPath,
    );
    let single_agg = ScopeAggregate::build_for_tracks(&profile, mode, &window, |i| i == biggest);
    let all_agg = ScopeAggregate::build_for_tracks(&profile, mode, &window, |i| {
        threads[i].lane_class != LaneClass::CriticalPath
    });
    eprintln!(
        "[bench] lanes: {real_lanes} real, {total_slices} slices; \
         biggest lane #{biggest} = {big_slices} slices"
    );
    eprintln!(
        "[bench] build_for_tracks(1 biggest): {single:.3}s  \
         build_for_tracks(all {real_lanes}): {all:.3}s  (min of {iters})"
    );
    eprintln!(
        "[bench] scan once: {scan_cold:.3}s  \
         build_for_tracks_reusing(all): {all_reused:.3}s  \
         (rebuild saves {:.3}s vs one-shot {all:.3}s)",
        all - all_reused,
    );
    eprintln!(
        "[bench] build_for_tracks_reusing(all, bottom-up): {all_inverted:.3}s  \
         (inversion adds {:.3}s)",
        all_inverted - all_reused,
    );
    eprintln!(
        "[bench] single: {} nodes / {} rows;  all: {} nodes / {} rows;  \
         all bottom-up: {} nodes",
        single_agg.flame.nodes().len(),
        single_agg.stats.len(),
        all_agg.flame.nodes().len(),
        all_agg.stats.len(),
        bottom_up_agg.flame.nodes().len(),
    );
}
