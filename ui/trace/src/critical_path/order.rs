//! The order the critical path's tables show their rows in.

use bviz_critical_path::{NearlyCritical, Step};
use bviz_ui_components::Sort;
use std::cmp::Ordering;
use std::time::Duration;

/// What the steps table can be sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StepKey {
    Start,
    Waited,
    Own,
    CouldSave,
    /// The step's own time as a share of the path, which orders as
    /// [`StepKey::Own`] does but stands for its own column.
    Share,
    /// The wait and own time together.
    Total,
}

/// What the table of actions nearly critical can be sorted by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NearlyCriticalKey {
    Took,
    Slack,
}

/// The order the steps open in, the order they ran.
pub(super) const STEPS_SORT: Sort<StepKey> = Sort {
    by: StepKey::Start,
    descending: false,
};

/// The order the actions nearly critical open in, least slack first.
pub(super) const NEARLY_CRITICAL_SORT: Sort<NearlyCriticalKey> = Sort {
    by: NearlyCriticalKey::Slack,
    descending: false,
};

/// The rows of `steps` in the order `sort` puts them, by index.
pub(super) fn steps(steps: &[Step], sort: Sort<StepKey>) -> Vec<usize> {
    let descending = sort.descending;
    match sort.by {
        StepKey::Start => sorted(steps, descending, |step| Some(step.start_us())),
        StepKey::Waited => sorted(steps, descending, |step| Some(step.gap())),
        StepKey::Own | StepKey::Share => sorted(steps, descending, |step| Some(step.own())),
        StepKey::CouldSave => sorted(steps, descending, |step| {
            step.model().map(|model| model.ceiling)
        }),
        StepKey::Total => sorted(steps, descending, |step| Some(step.gap() + step.own())),
    }
}

/// The rows of `actions` in the order `sort` puts them, by index.
pub(super) fn nearly_critical(
    actions: &[NearlyCritical],
    sort: Sort<NearlyCriticalKey>,
) -> Vec<usize> {
    let key = |action: &NearlyCritical| -> Option<Duration> {
        Some(match sort.by {
            NearlyCriticalKey::Took => action.duration,
            NearlyCriticalKey::Slack => action.slack,
        })
    };
    sorted(actions, sort.descending, key)
}

/// The indices of `rows` ordered by `key`, `descending` or not. A row with no
/// key goes last either way, and rows of equal key keep their order.
fn sorted<R, K: Ord>(rows: &[R], descending: bool, key: impl Fn(&R) -> Option<K>) -> Vec<usize> {
    let mut keyed: Vec<(Option<K>, usize)> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| (key(row), index))
        .collect();
    keyed.sort_by(|(a, a_index), (b, b_index)| {
        let by_key = match (a, b) {
            (Some(a), Some(b)) if descending => b.cmp(a),
            (Some(a), Some(b)) => a.cmp(b),
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
        };
        by_key.then(a_index.cmp(b_index))
    });
    keyed.into_iter().map(|(_, index)| index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::critical_path::words::tests::{MODELED_OUTPUTS, modeled_path};

    #[test]
    fn rows_without_a_key_go_last_either_way_and_ties_keep_their_order() {
        let rows = [Some(2), None, Some(1), Some(2)];
        assert_eq!(sorted(&rows, false, |row| *row), [2, 0, 3, 1]);
        assert_eq!(sorted(&rows, true, |row| *row), [0, 3, 2, 1]);
    }

    #[test]
    fn steps_open_in_the_order_they_ran_and_sort_by_any_column() {
        let path = modeled_path(&MODELED_OUTPUTS);
        let steps = path.steps();
        assert_eq!(super::steps(steps, STEPS_SORT), [0, 1, 2, 3]);
        let by_own = STEPS_SORT.clicked(StepKey::Own, true);
        assert_eq!(super::steps(steps, by_own), [0, 1, 2, 3]);
        assert_eq!(
            super::steps(steps, by_own.clicked(StepKey::Own, true)),
            [2, 3, 1, 0]
        );
        let by_saving = STEPS_SORT.clicked(StepKey::CouldSave, true);
        assert_eq!(super::steps(steps, by_saving), [0, 1, 2, 3]);
        assert_eq!(
            super::steps(steps, by_saving.clicked(StepKey::CouldSave, true)),
            [2, 1, 0, 3]
        );
        let by_wait = STEPS_SORT.clicked(StepKey::Waited, true);
        assert_eq!(super::steps(steps, by_wait), [1, 3, 0, 2]);
    }

    #[test]
    fn actions_nearly_critical_open_least_slack_first() {
        let path = modeled_path(&MODELED_OUTPUTS);
        let actions = path.modeled().expect("modeled").nearly_critical();
        assert_eq!(nearly_critical(actions, NEARLY_CRITICAL_SORT), [0, 1]);
        let by_took = NEARLY_CRITICAL_SORT.clicked(NearlyCriticalKey::Took, true);
        assert_eq!(nearly_critical(actions, by_took), [1, 0]);
    }
}
