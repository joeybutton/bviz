//! Headless flame-set benchmark: loads a profile, then builds every flame
//! scope, printing the load wall time, the [`FlameScopes::build`] wall time, and
//! their sum — the time until the All flame graph is available. No UI, so the
//! two stages measure in isolation and repeatedly.

use std::sync::Arc;

use bviz_trace_layout::{FlameScopes, Scope};
use bviz_trace_profile::Profile;

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

fn main() {
    bviz_alloc::retain_freed_memory_across_load();
    let path = std::env::args().nth(1).expect("usage: bench_flame <profile>");

    let load_start = std::time::Instant::now();
    let profile = Arc::new(
        Profile::from_path(std::path::Path::new(&path)).expect("load profile"),
    );
    let load = load_start.elapsed().as_secs_f64();

    let build_start = std::time::Instant::now();
    let scopes = FlameScopes::build(&profile);
    let build = build_start.elapsed().as_secs_f64();

    let all = scopes.get(Scope::All).expect("All scope present");
    eprintln!("[bench] load: {load:.3}s  build: {build:.3}s  sum: {:.3}s", load + build);
    eprintln!(
        "[bench] All: {} nodes, {} rows",
        all.flame.nodes().len(),
        all.stats.len()
    );
}
