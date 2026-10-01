//! The bviz window: one panel per document named on the command line.

use bviz_cli::{Cli, Source, Sources};
use bviz_ui_workspace::Document;
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
    Console(bviz_ui_console::Load),
}

impl Load {
    fn start(source: Source) -> Self {
        match source {
            Source::Trace(source) => Self::Trace(source.into()),
            Source::Console(source) => Self::Console(source.into()),
        }
    }

    /// Starts reading everything `sources` names: the document the window
    /// shows at the top, and those stacked below it.
    fn start_all(sources: Sources) -> (Self, Vec<Self>) {
        let start_each = |files: Vec<Source>| files.into_iter().map(Self::start).collect();
        match sources {
            Sources::Graph { graph, files } => (Self::Graph(graph.into()), start_each(files)),
            Sources::Files { first, rest } => (Self::start(first), start_each(rest)),
        }
    }

    fn open(self, window: &mut Window, cx: &mut App) -> Document {
        match self {
            Self::Graph(load) => bviz_ui_graph::open(load, window, cx).into(),
            Self::Trace(load) => bviz_ui_trace::open(load, window, cx).into(),
            Self::Console(load) => bviz_ui_console::open(load, window, cx).into(),
        }
    }
}

fn main() {
    bviz_alloc::retain_freed_memory_across_load();
    let sources = Cli::parse().sources().unwrap_or_else(|e| e.exit());
    let (first, rest) = Load::start_all(sources);
    bviz_ui_workspace::run_panels(
        "bviz",
        WINDOW_SIZE,
        first,
        rest,
        Load::open,
        &[
            bviz_ui_graph::register_keys,
            bviz_ui_trace::register_keys,
            bviz_ui_console::register_keys,
        ],
    );
}

#[cfg(test)]
mod tests {
    use bviz_ui_workspace::{Panel, ViewKind};
    use std::collections::HashMap;

    fn view_kind<T: Panel>() -> (&'static str, ViewKind) {
        (std::any::type_name::<T>(), T::kind().view)
    }

    #[test]
    fn no_two_panels_share_a_view_kind() {
        let panels = [
            view_kind::<bviz_ui_graph::GraphPanel>(),
            view_kind::<bviz_ui_graph::ScatterPanel>(),
            view_kind::<bviz_ui_trace::TracePanel>(),
            view_kind::<bviz_ui_trace::FlamePanel>(),
            view_kind::<bviz_ui_trace::StatsPanel>(),
            view_kind::<bviz_ui_console::ConsolePanel>(),
        ];
        let mut claimed: HashMap<ViewKind, &'static str> = HashMap::new();
        for (panel, view) in panels {
            if let Some(first) = claimed.insert(view, panel) {
                panic!("{first} and {panel} both call themselves {}", view.as_str());
            }
        }
    }
}
