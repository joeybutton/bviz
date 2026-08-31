//! The bviz window: the document named on the command line, shown by whichever
//! viewer reads its kind.

use bviz_cli::{Cli, Source};
use clap::Parser;

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

fn main() {
    bviz_alloc::retain_freed_memory_across_load();
    match Cli::parse().source().unwrap_or_else(|e| e.exit()) {
        Source::Graph(source) => bviz_ui_graph::open_window(source),
        Source::Trace(source) => bviz_ui_trace::open_window(source),
        Source::Console(source) => bviz_ui_terminal::open_window(source),
    }
}
