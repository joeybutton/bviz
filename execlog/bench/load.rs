//! Headless load benchmark: reads an execution log and prints the wall time,
//! with no UI so the read can be measured in isolation and repeatedly.

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: bench_load <execution log>");
    let start = std::time::Instant::now();
    let log =
        bviz_execlog::ExecLog::from_path(std::path::Path::new(&path)).expect("load execution log");
    let elapsed = start.elapsed().as_secs_f64();
    eprintln!(
        "[bench] load: {elapsed:.3}s  ({} spawns, {} runners)",
        log.spawn_count(),
        log.by_runner().len()
    );
}
