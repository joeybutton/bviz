//! What the spawn-time view shows of a log, derived once off the UI thread.

use bviz_execlog::{ExecLog, Group, Timing};
use gpui::SharedString;

/// How the spawns of a log are summed for one row of a table.
#[derive(Clone)]
pub(crate) struct GroupRow {
    pub(crate) name: SharedString,
    pub(crate) spawns: usize,
    pub(crate) timing: Timing,
}

impl From<Group<'_>> for GroupRow {
    fn from(group: Group<'_>) -> Self {
        Self {
            name: group.name.to_owned().into(),
            spawns: group.spawns,
            timing: group.timing,
        }
    }
}

#[derive(Clone)]
pub(crate) struct Summary {
    pub(crate) spawns: usize,
    pub(crate) timing: Timing,
    /// Longest first.
    pub(crate) runners: Vec<GroupRow>,
}

impl Summary {
    pub(crate) fn of(log: &ExecLog) -> Self {
        Self {
            spawns: log.spawn_count(),
            timing: log.timing(),
            runners: log.by_runner().into_iter().map(GroupRow::from).collect(),
        }
    }
}
