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

/// One runner's spawns, in sum and by mnemonic.
#[derive(Clone)]
pub(crate) struct Runner {
    pub(crate) sum: GroupRow,
    pub(crate) mnemonics: Vec<GroupRow>,
}

#[derive(Clone)]
pub(crate) struct Summary {
    pub(crate) spawns: usize,
    pub(crate) timing: Timing,
    /// Longest first.
    pub(crate) runners: Vec<Runner>,
}

impl Summary {
    pub(crate) fn of(log: &ExecLog) -> Self {
        let runners = log
            .by_runner()
            .into_iter()
            .map(|group| Runner {
                mnemonics: log
                    .by_mnemonic(group.name)
                    .into_iter()
                    .map(GroupRow::from)
                    .collect(),
                sum: GroupRow::from(group),
            })
            .collect();
        Self {
            spawns: log.spawn_count(),
            timing: log.timing(),
            runners,
        }
    }
}
