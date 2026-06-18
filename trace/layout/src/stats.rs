use std::collections::HashMap;

use bviz_trace_profile::{Profile, WorkClass};

use crate::flame::{build_flame_graph, FlameNode, GroupMode, Scope};

/// One aggregated group of frames, summed across the flame tree. `total_us` is
/// the summed self time of every frame keyed here, so the rows partition the
/// root's total and a group's share `total_us / Σ(all groups' total_us)` sums to
/// 100% without double-counting nested time. `max_us` is the peak single-frame
/// self time, `count` the number of merged span occurrences.
#[derive(Debug, Clone, PartialEq)]
pub struct StatRow {
    pub key: String,
    pub work_class: WorkClass,
    pub count: u32,
    pub total_us: i64,
    pub max_us: i64,
}

impl StatRow {
    /// Mean self time per merged span occurrence in the group.
    pub fn avg_us(&self) -> i64 {
        if self.count > 0 {
            self.total_us / self.count as i64
        } else {
            0
        }
    }
}

/// Aggregate the profile into one row per distinct frame name of the flame tree
/// built under `mode` and `scope`, summing each frame's self time. Self time
/// sums to the root's total, so the rows partition the aggregated time without
/// the double-count of summing a span and its sub-spans. Order is unspecified;
/// the caller sorts.
pub fn aggregate_stats(profile: &Profile, mode: GroupMode, scope: Scope) -> Vec<StatRow> {
    let root = build_flame_graph(profile, mode, scope);
    let mut groups: HashMap<String, StatRow> = HashMap::new();
    accumulate(&root, &mut groups);
    // The synthetic root carries no slices of its own; drop its zero-self frame.
    groups.remove(&root.name);
    groups.into_values().collect()
}

/// Fold every frame's self time into its name's row, recursing into children.
fn accumulate(node: &FlameNode, groups: &mut HashMap<String, StatRow>) {
    let row = groups.entry(node.name.clone()).or_insert_with(|| StatRow {
        key: node.name.clone(),
        work_class: node.work_class,
        count: 0,
        total_us: 0,
        max_us: 0,
    });
    row.count += node.count;
    row.total_us += node.self_us;
    row.max_us = row.max_us.max(node.self_us);
    for c in &node.children {
        accumulate(c, groups);
    }
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

    /// Sum of every row's self-time total — must equal the root's total.
    fn total(rows: &[StatRow]) -> i64 {
        rows.iter().map(|r| r.total_us).sum()
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
        // Two disjoint CppCompile actions merge into one frame (no nesting), so
        // its self time sums both durations and the row counts both occurrences.
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
        // One merged frame; its peak self time is the merged total.
        assert_eq!(c.max_us, 150);
        assert_eq!(c.work_class, WorkClass::Cpu);
        assert_eq!(row(&rows, "CppLink").total_us, 30);
        // Self time partitions the root total (180), no double-count.
        assert_eq!(total(&rows), 180);
    }

    #[test]
    fn nested_spans_count_self_time_not_inclusive() {
        // Rustc(0..100) contains subprocess.run(0..80): the table credits Rustc
        // with its self time (20) and subprocess.run with its own (80), summing to
        // the action's 100 rather than the double-counted 180.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("Rustc", 0, 100),
                slice("subprocess.run", "subprocess run", 0, 80),
            ],
        )]);
        let rows = aggregate_stats(&p, GroupMode::Mnemonic, Scope::All);
        assert_eq!(row(&rows, "Rustc").total_us, 20);
        assert_eq!(row(&rows, "subprocess.run").total_us, 80);
        assert_eq!(total(&rows), 100);
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
            action("Rustc", 100, 40),
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
        // Both disjoint CppCompile spans merge into one frame: self time 160.
        assert_eq!(a.total_us, 160);
        assert_eq!(a.max_us, 160);
    }
}
