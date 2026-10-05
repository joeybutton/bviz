//! Shows where the spawns of a Bazel build spent their time, read from a
//! compact execution log (`--execution_log_compact_file`) or an execution graph
//! log (`--experimental_enable_execution_graph_log`): the time Bazel's spawn
//! metrics report for each phase of a spawn's run, summed by runner and by
//! mnemonic, and the spawns that spent longest in one phase.

mod format;
mod model;
mod palette;
mod spawn_time;
mod summary;

pub use spawn_time::SpawnTimePanel;

use bviz_execlog::ExecLog;
use bviz_execlog::graph::GraphLog;
use bviz_invocation::{InvocationId, Session};
use bviz_ui_components::Pending;
use bviz_ui_execlog_args::{ExecLogArgs, ExecLogSource};
use bviz_ui_playback::DocumentClock;
use bviz_ui_workspace::{Member, PanelGroup, ViewSpec, document_label};
use clap::error::ErrorKind;
use clap::{CommandFactory, Parser};
use gpui::{App, Entity, SharedString, Window, prelude::*, px, size};
use model::{ExecLogModel, GraphRead, Read};
use summary::Summary;

/// A log being read.
pub struct Load {
    log: Reading,
    title: SharedString,
}

enum Reading {
    Spawns(Pending<Read>),
    Graph(Pending<GraphRead>),
}

/// The invocation that wrote the log at `source`, read from the head of the
/// file alone; `None` for an execution graph log, which names none.
pub fn invocation_id(source: &ExecLogSource) -> Option<InvocationId> {
    match source {
        ExecLogSource::Spawns(path) => bviz_execlog::probe_invocation_id(path),
        ExecLogSource::Graph(_) => None,
    }
}

impl From<ExecLogSource> for Load {
    fn from(source: ExecLogSource) -> Self {
        let title = document_label(source.path());
        let cannot_read =
            |path: &std::path::Path, e| format!("cannot read {}: {e}", path.display());
        let log = match source {
            ExecLogSource::Spawns(path) => Reading::Spawns(Pending::start(move || {
                ExecLog::from_path(&path)
                    .map(|log| Read {
                        summary: Summary::of(&log),
                        spawns: log.into_spawns(),
                    })
                    .map_err(|e| cannot_read(&path, e))
            })),
            ExecLogSource::Graph(path) => Reading::Graph(Pending::start(move || {
                GraphLog::from_path(&path)
                    .map(|log| GraphRead {
                        summary: Summary::of(log.spawns()),
                        log,
                    })
                    .map_err(|e| cannot_read(&path, e))
            })),
        };
        Self { log, title }
    }
}

/// The view of the spawn time the log `load` reads. Where it is opened as
/// part of an invocation's `session`, what the log tells of the invocation's
/// spawns and attempts is reported to it; where its document plays back on
/// `clock`, the log plays there.
pub fn open(
    load: impl Into<Load>,
    session: Option<Entity<Session>>,
    clock: Option<DocumentClock>,
    _window: &mut Window,
    cx: &mut App,
) -> Member {
    let load = load.into();
    let model = cx.new(|cx| match load.log {
        Reading::Spawns(log) => {
            ExecLogModel::reading(log, session.as_ref().map(Entity::downgrade), clock, cx)
        }
        Reading::Graph(log) => ExecLogModel::reading_graph(log, session, clock, cx),
    });
    let view = ViewSpec::new(move |cx| cx.new(|cx| SpawnTimePanel::new(model.clone(), cx)));
    Member::new(load.title, PanelGroup::one(view))
}

/// Shows the spawn time of Bazel builds by phase. Each file given opens a view
/// of its own.
#[derive(Parser)]
#[command(name = "execlog", version, about)]
struct Cli {
    #[command(flatten)]
    exec_log: ExecLogArgs,
}

/// Reads the execution logs and opens the spawn-time window.
pub fn run() {
    let mut loads = Cli::parse().exec_log.sources().map(Load::from);
    let Some(first) = loads.next() else {
        Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "--exec_log or --exec_graph_log is required",
            )
            .exit()
    };
    bviz_ui_workspace::run_panels(
        "bviz execlog",
        size(px(1100.), px(720.)),
        first,
        loads.collect(),
        |load, window, cx| open(load, None, None, window, cx).into(),
        &[],
    );
}
