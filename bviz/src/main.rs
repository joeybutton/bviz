//! The bviz window: one panel per document named on the command line.

use bviz_cli::{Cli, Source};
use bviz_ui_workspace::AnyPanel;
use clap::Parser;
use gpui::{App, Pixels, Size, Window, px};

#[global_allocator]
static GLOBAL: bviz_alloc::MiMalloc = bviz_alloc::MiMalloc;

/// Opening size of the window, roomy enough for a graph or a timeline to be
/// worth looking at before it is resized.
const WINDOW_SIZE: Size<Pixels> = Size {
    width: px(1280.),
    height: px(820.),
};

/// A document being read, whichever viewer shows it.
enum Load {
    Graph(bviz_ui_graph::Load),
    Trace(bviz_ui_trace::Load),
    Console(bviz_ui_terminal::Load),
}

impl Load {
    fn start(source: Source) -> Self {
        match source {
            Source::Graph(source) => Self::Graph(source.into()),
            Source::Trace(source) => Self::Trace(source.into()),
            Source::Console(source) => Self::Console(source.into()),
        }
    }

    fn open(self, window: &mut Window, cx: &mut App) -> AnyPanel {
        match self {
            Self::Graph(load) => bviz_ui_graph::open(load, window, cx),
            Self::Trace(load) => bviz_ui_trace::open(load, window, cx),
            Self::Console(load) => bviz_ui_terminal::open(load, window, cx),
        }
    }
}

fn main() {
    bviz_alloc::retain_freed_memory_across_load();
    let sources = Cli::parse().sources().unwrap_or_else(|e| e.exit());
    let (first, rest) = sources.into_parts();
    let first = Load::start(first);
    let rest: Vec<Load> = rest.into_iter().map(Load::start).collect();
    bviz_ui_workspace::run_panels("bviz", WINDOW_SIZE, first, rest, Load::open);
}
