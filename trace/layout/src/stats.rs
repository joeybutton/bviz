use std::collections::HashMap;

use bviz_trace_profile::{Profile, WorkClass};

use crate::flame::{leaf_label, mnemonic_index, GroupMode, Scope};

/// One aggregated group of slices, summed across the scope. `total_us` and
/// `max_us` sum/peak the durations of every slice (at any depth) sharing this
/// key. A group's share of the total is `total_us / sum(all groups' total_us)`,
/// which sums to 100% across groups — share of aggregated time, not wall-clock:
/// nested slices overlap in time, so the summed durations exceed the elapsed
/// wall time.
#[derive(Debug, Clone, PartialEq)]
pub struct StatRow {
    pub key: String,
    pub work_class: WorkClass,
    pub count: u32,
    pub total_us: i64,
    pub max_us: i64,
}

impl StatRow {
    /// Mean duration of the group's slices.
    pub fn avg_us(&self) -> i64 {
        if self.count > 0 {
            self.total_us / self.count as i64
        } else {
            0
        }
    }
}

/// Aggregate the profile's in-scope slices into one row per distinct leaf label
/// of `mode`'s path (count, summed and max duration, work class). Every slice in
/// the lanes `scope` selects counts, at all depths. Order is unspecified; the
/// caller sorts.
pub fn aggregate_stats(profile: &Profile, mode: GroupMode, scope: Scope) -> Vec<StatRow> {
    let idx = mnemonic_index(profile);
    let mut groups: HashMap<String, StatRow> = HashMap::new();
    for thread in &profile.threads {
        if !scope.includes(thread.lane_class) {
            continue;
        }
        for s in &thread.slices {
            let (label, work_class) = leaf_label(s, &idx, mode);
            let row = groups.entry(label.clone()).or_insert_with(|| StatRow {
                key: label,
                work_class,
                count: 0,
                total_us: 0,
                max_us: 0,
            });
            row.count += 1;
            row.total_us += s.dur_us;
            row.max_us = row.max_us.max(s.dur_us);
        }
    }
    groups.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_trace_profile::{LaneClass, Metadata, Slice, SliceArgs, ThreadTrack, TimeSpan};

    fn slice(name: &str, category: &str, start_us: i64, dur_us: i64) -> Slice {
        Slice {
            name: name.to_owned(),
            category: category.to_owned(),
            start_us,
            dur_us,
            depth: 0,
            args: SliceArgs::default(),
            work_class: bviz_trace_profile::work_class(category),
        }
    }

    fn action(mnemonic: &str, start_us: i64, dur_us: i64) -> Slice {
        Slice {
            args: SliceArgs {
                mnemonic: Some(mnemonic.to_owned()),
                ..SliceArgs::default()
            },
            ..slice(mnemonic, "local action execution", start_us, dur_us)
        }
    }

    fn track(tid: i64, lane_class: LaneClass, slices: Vec<Slice>) -> ThreadTrack {
        ThreadTrack {
            tid,
            name: format!("t{tid}"),
            sort_index: tid,
            slices,
            max_depth: 0,
            lane_class,
        }
    }

    fn profile(threads: Vec<ThreadTrack>) -> Profile {
        Profile {
            metadata: Metadata::default(),
            threads,
            counters: Vec::new(),
            phase_markers: Vec::new(),
            span: TimeSpan {
                start_us: 0,
                end_us: 1000,
            },
        }
    }

    fn row<'a>(rows: &'a [StatRow], key: &str) -> &'a StatRow {
        rows.iter()
            .find(|r| r.key == key)
            .unwrap_or_else(|| panic!("no row {key}"))
    }

    #[test]
    fn mnemonic_grouping_sums_count_total_and_max() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("CppCompile", 0, 100),
                action("CppCompile", 100, 50),
                action("CppLink", 200, 30),
            ],
        )]);
        let rows = aggregate_stats(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(rows.len(), 2);
        let c = row(&rows, "CppCompile");
        assert_eq!(c.count, 2);
        assert_eq!(c.total_us, 150);
        assert_eq!(c.max_us, 100);
        assert_eq!(c.avg_us(), 75);
        assert_eq!(c.work_class, WorkClass::Cpu);
        assert_eq!(row(&rows, "CppLink").total_us, 30);
    }

    #[test]
    fn scope_excludes_other_lanes() {
        let p = profile(vec![
            track(7, LaneClass::Analysis, vec![action("CppCompile", 0, 100)]),
            track(8, LaneClass::Execution, vec![action("Rustc", 0, 50)]),
        ]);
        let rows = aggregate_stats(&p, GroupMode::Mnemonic, Scope::Execution);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].key, "Rustc");
        assert_eq!(rows[0].total_us, 50);
    }

    #[test]
    fn mnemonicless_slice_keeps_its_name() {
        // Under Mnemonic grouping a slice with no mnemonic keys by its own name.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("CppCompile", 0, 100),
                slice("checkOutputs", "general information", 200, 30),
            ],
        )]);
        let rows = aggregate_stats(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(rows.len(), 2);
        assert_eq!(row(&rows, "checkOutputs").total_us, 30);
        assert_eq!(row(&rows, "CppCompile").count, 1);
    }

    #[test]
    fn work_class_mode_sums_per_class() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("CppCompile", 0, 100),
                action("Rustc", 100, 50),
                slice("remote", "remote action execution", 200, 30),
            ],
        )]);
        let rows = aggregate_stats(&p, GroupMode::WorkClass, Scope::All);
        assert_eq!(row(&rows, "cpu").total_us, 150);
        assert_eq!(row(&rows, "remote").total_us, 30);
    }

    #[test]
    fn aggregation_is_order_independent() {
        let ordered = vec![
            action("CppCompile", 0, 100),
            action("Rustc", 0, 40),
            action("CppCompile", 200, 60),
        ];
        let mut reversed = ordered.clone();
        reversed.reverse();
        let mut ra = aggregate_stats(
            &profile(vec![track(1, LaneClass::Execution, ordered)]),
            GroupMode::Mnemonic,
            Scope::All,
        );
        let mut rb = aggregate_stats(
            &profile(vec![track(1, LaneClass::Execution, reversed)]),
            GroupMode::Mnemonic,
            Scope::All,
        );
        ra.sort_by(|x, y| x.key.cmp(&y.key));
        rb.sort_by(|x, y| x.key.cmp(&y.key));
        assert_eq!(ra, rb);
        let a = row(&ra, "CppCompile");
        assert_eq!(a.count, 2);
        assert_eq!(a.total_us, 160);
        assert_eq!(a.max_us, 100);
    }
}
