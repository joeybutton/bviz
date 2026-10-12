//! Shows where the spawns of a Bazel build spent their time, read from a
//! compact execution log (`--execution_log_compact_file`): the time Bazel's
//! spawn metrics report for each phase of a spawn's run, summed by runner and
//! by mnemonic, and the spawns that spent longest in one phase.

mod format;
mod model;
mod palette;
mod spawn_time;
mod summary;

pub use spawn_time::SpawnTimePanel;

use bviz_execlog::ExecLog;
use bviz_invocation::{InvocationId, Session};
use bviz_ui_components::{Pending, declare_scopes, legend_key_bindings};
use bviz_ui_execlog_args::{EXEC_LOG_LONG, ExecLogArgs, ExecLogSource};
use bviz_ui_workspace::{Member, ViewSpec, document_label};
use clap::Parser;
use gpui::{App, Entity, SharedString, Window, prelude::*};
use model::{ExecLogModel, Read};
use summary::Summary;

pub fn register_keys(cx: &mut App) {
    declare_scopes(cx, [spawn_time::KEY_CONTEXT]);
    cx.bind_keys(legend_key_bindings(spawn_time::KEY_CONTEXT));
}

/// An execution log being read.
pub struct Load {
    log: Pending<Read>,
    title: SharedString,
}

/// The invocation that wrote the execution log at `source`, read from the head
/// of the file alone.
pub fn invocation_id(source: &ExecLogSource) -> Option<InvocationId> {
    bviz_execlog::probe_invocation_id(&source.path)
}

impl From<ExecLogSource> for Load {
    fn from(source: ExecLogSource) -> Self {
        let title = document_label(&source.path);
        Self {
            log: Pending::start(move || {
                ExecLog::from_path(&source.path)
                    .map(|log| Read {
                        summary: log.as_ref().map(Summary::of),
                        spawns: log.into_inner().into_spawns(),
                    })
                    .map_err(|e| format!("cannot read {}: {e}", source.path.display()))
            }),
            title,
        }
    }
}

/// The view of the spawn time the log `load` reads. Where it is opened as
/// part of an invocation's `session`, the log's spawns are reported to it.
pub fn open(
    load: impl Into<Load>,
    session: Option<Entity<Session>>,
    _window: &mut Window,
    cx: &mut App,
) -> Member {
    let load = load.into();
    let session = session.as_ref().map(Entity::downgrade);
    let model = cx.new(|cx| ExecLogModel::reading(load.log, session, cx));
    let view = ViewSpec::new(move |cx| cx.new(|cx| SpawnTimePanel::new(model.clone(), cx)));
    Member::new(load.title, view, Vec::new())
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
    bviz_ui_workspace::run_viewer(
        "bviz execlog",
        bviz_ui_workspace::TEXT_WINDOW_SIZE,
        |cli: Cli| cli.exec_log.sources().map(Load::from),
        &format!("--{EXEC_LOG_LONG}"),
        |load, window, cx| open(load, None, window, cx).into(),
        &[register_keys],
    );
}
