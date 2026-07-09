//! Headless flame-build benchmark mirroring the UI's load sequence: it loads a
//! profile once, then times the pieces that decide how long the user waits for
//! the first flame graph and for the full scope set. Each build stage runs
//! `iters` times (env `BVIZ_BENCH_ITERS`, default 5) and reports the minimum, so
//! a background bazel build on the box perturbs the figure as little as possible.

use std::sync::Arc;

use bviz_trace_layout::{FlameScopes, Scope, ScopeAggregate};
use bviz_trace_profile::Profile;

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
    let path = std::env::args().nth(1).expect("usage: bench_flame <profile>");
    let iters = std::env::var("BVIZ_BENCH_ITERS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);

    let load_start = std::time::Instant::now();
    let profile =
        Arc::new(Profile::from_path(std::path::Path::new(&path)).expect("load profile"));
    let load = load_start.elapsed().as_secs_f64();

    // What the user waits for before the first graph appears: the on-demand
    // build of the default (All) scope.
    let agg_all = min_secs(iters, || {
        std::hint::black_box(ScopeAggregate::build(&profile, Scope::All));
    });
    // The background set pass that follows, reusing the served All.
    let served = Arc::new(ScopeAggregate::build(&profile, Scope::All));
    let set_reuse = min_secs(iters, || {
        std::hint::black_box(FlameScopes::build_reusing(&profile, Scope::All, served.clone()));
    });
    // The set pass with no reuse, for comparison.
    let set_full = min_secs(iters, || {
        std::hint::black_box(FlameScopes::build(&profile));
    });

    let scopes = FlameScopes::build(&profile);
    let all = scopes.get(Scope::All).expect("All scope present");
    eprintln!(
        "[bench] load: {load:.3}s  agg(All): {agg_all:.3}s  set(reuse): {set_reuse:.3}s  \
         set(full): {set_full:.3}s  (min of {iters})"
    );
    eprintln!(
        "[bench] time-to-first-graph: {agg_all:.3}s   full set from scratch: {set_full:.3}s   \
         served-then-reuse set: {:.3}s",
        agg_all + set_reuse,
    );
    eprintln!(
        "[bench] All: {} nodes, {} rows",
        all.flame.nodes().len(),
        all.stats.len()
    );
}
