//! The bviz window: a document per file named on the command line, the files
//! of one Bazel invocation sharing one.

mod group;

use bviz_cli::{Cli, Request, Source, Sources};
use bviz_invocation::Session;
use bviz_ui_playback::DocumentClock;
use bviz_ui_workspace::{Document, Member};
#[cfg(not(target_family = "wasm"))]
use clap::Parser;
use gpui::{App, AppContext, Entity, Window};
use group::{File, Given, Kind, Opening};

#[global_allocator]
static GLOBAL: bviz_alloc::Allocator = bviz_alloc::Allocator;

/// A document being read, whichever viewer shows it.
enum Load {
    Graph(bviz_ui_graph::Load),
    Trace(bviz_ui_trace::Load),
    Console(bviz_ui_console::Load),
    ExecLog(bviz_ui_execlog::Load),
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
            Source::ExecLog(source) => File {
                kind: Kind::ExecLog,
                invocation: bviz_ui_execlog::invocation_id(&source),
                file: Self::ExecLog(source.into()),
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
    /// its invocation where it is opened as one of them, and following `clock`
    /// where its document plays back.
    fn open(
        self,
        session: Option<&Entity<Session>>,
        clock: Option<&DocumentClock>,
        window: &mut Window,
        cx: &mut App,
    ) -> Member {
        let session = session.cloned();
        match self {
            Self::Graph(load) => bviz_ui_graph::open(load, session, clock.cloned(), window, cx),
            Self::Trace(load) => bviz_ui_trace::open(load, session, clock.cloned(), window, cx),
            Self::Console(load) => bviz_ui_console::open(load, session, clock.cloned(), window, cx),
            Self::ExecLog(load) => bviz_ui_execlog::open(load, session, window, cx),
        }
    }
}

/// The document `opening` is shown as: a file's own, or an invocation's files
/// joined under a label naming it. A build's console replay plays back alone;
/// a profile or a graph alone has nothing to play back with. Every invocation
/// carries a clock; one whose files give it no time to play, such as an
/// execution log with a graph, shows no playback bar.
fn open(opening: Opening<Load>, window: &mut Window, cx: &mut App) -> Document {
    match opening {
        Opening::Alone(load @ Load::Console(_)) => {
            Document::playing_on(DocumentClock::alone(cx), |clock| {
                load.open(None, Some(clock), window, cx).into()
            })
        }
        Opening::Alone(load) => load.open(None, None, window, cx).into(),
        Opening::Invocation {
            id,
            first,
            second,
            rest,
        } => {
            let session = cx.new(|_| Session::default());
            Document::playing_on(DocumentClock::joined(cx), |clock| {
                let mut open = |load: Load| load.open(Some(&session), Some(clock), window, cx);
                let first = open(first);
                let second = open(second);
                let mut others = Vec::with_capacity(rest.len());
                for load in rest {
                    others.push(open(load));
                }
                Document::join(group::label(&id).into(), first, second, others)
            })
        }
    }
}

#[cfg(not(target_family = "wasm"))]
fn main() {
    // SAFETY: the first statement of main, before any other thread starts.
    unsafe { bviz_alloc::retain_freed_memory_across_load() };
    match Cli::parse().request().unwrap_or_else(|e| e.exit()) {
        Request::Bazelrc(args) => args.print(),
        Request::Open(sources) => open_window(sources),
    }
}

/// In a browser the page starts bviz, once it has handed over the files its
/// command line names: see [`web::start`].
#[cfg(target_family = "wasm")]
fn main() {}

#[cfg(target_family = "wasm")]
mod web;

fn open_window(sources: Sources) {
    let (first, rest) = group::by_invocation(Load::start_all(sources));
    bviz_ui_workspace::run_panels(
        "bviz",
        bviz_ui_workspace::WINDOW_SIZE,
        first,
        rest,
        open,
        &[
            bviz_ui_graph::register_keys,
            bviz_ui_trace::register_keys,
            bviz_ui_console::register_keys,
            bviz_ui_execlog::register_keys,
        ],
    );
}

#[cfg(test)]
mod tests {
    use bviz_ui_components::LABEL_MAX_CHARS;
    use bviz_ui_workspace::{Panel, ViewKind};
    use std::collections::HashMap;

    /// What the namespace of every action bviz declares begins with.
    const ACTION_NAMESPACE_PREFIX: &str = "bviz_";

    #[test]
    fn every_action_has_a_help_label_the_keys_help_fits() {
        let mut declared = 0;
        let mut unfit = Vec::new();
        for action in gpui::generate_list_of_all_registered_actions() {
            if !action.name.starts_with(ACTION_NAMESPACE_PREFIX) {
                continue;
            }
            declared += 1;
            let label = action.documentation.and_then(|doc| doc.lines().next());
            if label.is_none_or(|label| label.chars().count() > LABEL_MAX_CHARS) {
                unfit.push((action.name, label));
            }
        }
        assert_ne!(declared, 0, "no action of bviz's is registered");
        assert_eq!(unfit, []);
    }

    /// gpui registers every action by name as the application starts, and
    /// panics there on a name registered twice.
    #[test]
    fn no_two_actions_share_a_name() {
        let mut named: HashMap<&str, usize> = HashMap::new();
        for action in gpui::generate_list_of_all_registered_actions() {
            *named.entry(action.name).or_default() += 1;
        }
        let mut shared: Vec<&str> = named
            .into_iter()
            .filter(|&(_, count)| count > 1)
            .map(|(name, _)| name)
            .collect();
        shared.sort_unstable();
        assert_eq!(shared, Vec::<&str>::new());
    }

    fn view_kind<T: Panel>() -> (&'static str, ViewKind) {
        (std::any::type_name::<T>(), T::kind().view)
    }

    #[test]
    fn no_two_panels_share_a_view_kind() {
        let panels = [
            view_kind::<bviz_ui_graph::GraphPanel>(),
            view_kind::<bviz_ui_graph::ScatterPanel>(),
            view_kind::<bviz_ui_graph::ExclusivePanel>(),
            view_kind::<bviz_ui_trace::TracePanel>(),
            view_kind::<bviz_ui_trace::FlamePanel>(),
            view_kind::<bviz_ui_trace::StatsPanel>(),
            view_kind::<bviz_ui_console::ConsolePanel>(),
            view_kind::<bviz_ui_execlog::SpawnTimePanel>(),
        ];
        let mut claimed: HashMap<ViewKind, &'static str> = HashMap::new();
        for (panel, view) in panels {
            if let Some(first) = claimed.insert(view, panel) {
                panic!("{first} and {panel} both call themselves {}", view.as_str());
            }
        }
    }
}
