//! The bviz window: a document per file named on the command line, the files
//! of one Bazel invocation sharing one.

mod group;

use bviz_cli::{Cli, Source, Sources};
use bviz_invocation::Session;
use bviz_ui_workspace::{Document, Member};
use clap::Parser;
use gpui::{App, AppContext, Entity, Pixels, Size, Window, px};
use group::{File, Given, Kind, Opening};

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
    /// Starts reading `source`, having first asked its head which invocation
    /// wrote it.
    fn start(source: Source) -> File<Self> {
        match source {
            Source::Trace(source) => File {
                kind: Kind::Profile,
                invocation: bviz_ui_trace::invocation_id(&source),
                file: Self::Trace(source.into()),
            },
            Source::Console(source) => File {
                kind: Kind::Bep,
                invocation: bviz_ui_console::invocation_id(&source),
                file: Self::Console(source.into()),
            },
        }
    }

    fn start_all(sources: Sources) -> Given<Self> {
        let start_each = |files: Vec<Source>| files.into_iter().map(Self::start).collect();
        match sources {
            Sources::Graph { graph, files } => Given::Graph {
                graph: Self::Graph(graph.into()),
                files: start_each(files),
            },
            Sources::Files { first, rest } => Given::Files {
                first: Self::start(first),
                rest: start_each(rest),
            },
        }
    }

    /// The views of what this reads, sharing `session` with the other files of
    /// its invocation where it is opened as one of them.
    fn open(self, session: Option<&Entity<Session>>, window: &mut Window, cx: &mut App) -> Member {
        let session = session.cloned();
        match self {
            Self::Graph(load) => bviz_ui_graph::open(load, session, window, cx),
            Self::Trace(load) => bviz_ui_trace::open(load, session, window, cx),
            Self::Console(load) => bviz_ui_console::open(load, session, window, cx),
        }
    }
}

/// The document `opening` is shown as: a file's own, or an invocation's files
/// joined under a label naming it.
fn open(opening: Opening<Load>, window: &mut Window, cx: &mut App) -> Document {
    match opening {
        Opening::Alone(load) => load.open(None, window, cx).into(),
        Opening::Invocation {
            id,
            first,
            second,
            rest,
        } => {
            let session = cx.new(|_| Session::default());
            let first = first.open(Some(&session), window, cx);
            let second = second.open(Some(&session), window, cx);
            let mut others = Vec::with_capacity(rest.len());
            for load in rest {
                others.push(load.open(Some(&session), window, cx));
            }
            Document::join(group::label(&id).into(), first, second, others)
        }
    }
}

fn main() {
    bviz_alloc::retain_freed_memory_across_load();
    let sources = Cli::parse().sources().unwrap_or_else(|e| e.exit());
    let (first, rest) = group::by_invocation(Load::start_all(sources));
    bviz_ui_workspace::run_panels(
        "bviz",
        WINDOW_SIZE,
        first,
        rest,
        open,
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
            view_kind::<bviz_ui_graph::DominatorsPanel>(),
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
