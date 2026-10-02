use bviz_parallel::workers;
use rayon::slice::ParallelSliceMut;

use bviz_trace_profile::{SpanRole, Str};

use crate::flame::FlameGraph;

/// One aggregated group of frames, summed across the flame graph. The rows
/// partition the root's total, so a group's share `self_us / Σ(all groups'
/// self_us)` sums to 100% without double-counting nested time. `max_us` is the
/// peak single-frame self time, `count` the number of merged span occurrences.
/// `key` is a [`Str`] handle into the profile held by the [`FlameGraph`] the row
/// was derived from; resolve it with that graph's [`str`](FlameGraph::str).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatRow {
    pub key: Str,
    /// Lexicographic rank of `key`'s text among the names scanned for the
    /// producing graph's build ([`FlameNode::rank`]); rank order equals
    /// byte-wise text order, so sorting rows by key compares integers without
    /// resolving text. Ranks only order rows within one aggregate, never
    /// across aggregates.
    pub rank: u32,
    pub role: SpanRole,
    pub count: u32,
    pub self_us: i64,
    pub max_us: i64,
}

impl StatRow {
    /// Mean self time per merged span occurrence in the group.
    pub fn avg_us(&self) -> i64 {
        if self.count > 0 {
            self.self_us / self.count as i64
        } else {
            0
        }
    }
}

/// Aggregate an already-built flame graph into its stats rows, summing each
/// frame's self time. Lets a caller that already holds the graph derive the table
/// without rebuilding it. Keys resolve with `graph`'s [`str`](FlameGraph::str).
///
/// A detail frame carries part of its parent's own self time under another
/// label, so a row of its own would double-count that time and split it away
/// from the label it belongs to. Each is skipped, and its parent contributes
/// [`FlameGraph::span_self_us_by`] instead.
pub fn stats_from_flame(graph: &FlameGraph) -> Vec<StatRow> {
    stats_by(graph, |i| graph.total_us(i))
}

/// [`stats_from_flame`]'s rows for a graph weighed by something other than
/// duration, from `duration_us`: each node's summed span duration,
/// index-parallel to the graph's nodes.
pub(crate) fn stats_from_durations(graph: &FlameGraph, duration_us: &[i64]) -> Vec<StatRow> {
    stats_by(graph, |i| duration_us[i])
}

/// The stats rows of `graph`'s frames, timed by `total_us`, each node's total.
fn stats_by(graph: &FlameGraph, total_us: impl Fn(usize) -> i64) -> Vec<StatRow> {
    // A node's rank is a dense `u32` bijection with its name within one build,
    // so a table indexed by rank groups by name without hashing. Node 0 is the
    // synthetic root, whose placeholder rank 0 aliases the real
    // lexicographically-smallest name; skipping it drops its zero-self frame
    // without evicting that name's real rank-0 row.
    let cap = (0..graph.len()).map(|i| graph.rank(i)).max().unwrap_or(0) as usize + 1;
    let mut rows: Vec<Option<StatRow>> = vec![None; cap];
    for i in 1..graph.len() {
        if graph.is_detail(i) {
            continue;
        }
        let rank = graph.rank(i);
        let self_us = graph.span_self_us_by(i, &total_us);
        match &mut rows[rank as usize] {
            Some(row) => {
                row.count += graph.count(i);
                row.self_us += self_us;
                row.max_us = row.max_us.max(self_us);
            }
            None => {
                rows[rank as usize] = Some(StatRow {
                    key: graph.name(i),
                    rank,
                    role: graph.role(i),
                    count: graph.count(i),
                    self_us,
                    max_us: self_us,
                });
            }
        }
    }
    rows.into_iter().flatten().collect()
}

/// Indices into `stats` in the default display order: `self_us` descending,
/// ties broken by key rank ascending — which orders exactly as the key text
/// does — then by row index. Each row decorate-sorts on one packed integer key:
/// the self time in the high bits as an order-preserving `i64`→`u64` bias
/// inverted for descending, above the rank, above the index. No two rows share
/// an index, so the order is total, and no comparison resolves a string or
/// chases a row pointer.
pub(crate) fn default_order(stats: &[StatRow]) -> Vec<u32> {
    const RANK_SHIFT: u32 = u32::BITS;
    const SELF_SHIFT: u32 = 2 * u32::BITS;
    const SIGN_BIT: u64 = 1 << (u64::BITS - 1);
    let mut keyed: Vec<u128> = stats
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let self_desc = !((r.self_us as u64) ^ SIGN_BIT);
            ((self_desc as u128) << SELF_SHIFT) | ((r.rank as u128) << RANK_SHIFT) | i as u128
        })
        .collect();
    workers().install(|| keyed.par_sort_unstable());
    keyed.into_iter().map(|k| k as u32).collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::flame::{FlameLabels, TrackFlamer};
    use crate::selection::TimeSelection;
    use crate::test_profile::{action, action_named, profile, slice, track};
    use bviz_trace_profile::{LaneClass, Profile, Workload};

    /// The all-lanes flame graph, whose stats every all-scope test aggregates.
    fn flame(p: &Arc<Profile>) -> FlameGraph {
        let t = p.threads();
        TrackFlamer::new(p).build(FlameLabels::Kind, &TimeSelection::default(), |i| {
            t[i].lane_class != LaneClass::CriticalPath
        })
    }

    /// Sum of every row's self time — must equal the root's total.
    fn self_sum(rows: &[StatRow]) -> i64 {
        rows.iter().map(|r| r.self_us).sum()
    }

    fn row<'a>(graph: &FlameGraph, rows: &'a [StatRow], key: &str) -> &'a StatRow {
        rows.iter()
            .find(|r| graph.str(r.key) == key)
            .unwrap_or_else(|| panic!("no row {key}"))
    }

    #[test]
    fn mnemonic_grouping_sums_count_self_and_max() {
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
        let g = flame(&p);
        let rows = stats_from_flame(&g);
        assert_eq!(rows.len(), 2);
        let c = row(&g, &rows, "CppCompile");
        assert_eq!(c.count, 2);
        assert_eq!(c.self_us, 150);
        // One merged frame; its peak self time is the merged total.
        assert_eq!(c.max_us, 150);
        assert_eq!(
            c.role,
            SpanRole {
                workload: Workload::Subprocess,
                payload: true
            }
        );
        assert_eq!(row(&g, &rows, "CppLink").self_us, 30);
        // Self time partitions the root total (180), no double-count.
        assert_eq!(self_sum(&rows), 180);
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
        let g = flame(&p);
        let rows = stats_from_flame(&g);
        assert_eq!(row(&g, &rows, "Rustc").self_us, 20);
        assert_eq!(row(&g, &rows, "subprocess.run").self_us, 80);
        assert_eq!(self_sum(&rows), 100);
    }

    #[test]
    fn detail_frames_leave_the_rows_alone() {
        // A Rustc action on one target containing subprocess.run: the graph
        // breaks each frame's self time out under the target's name, and the
        // rows stay the self time per mnemonic — 20 for the action's own work,
        // 80 for the subprocess — with no row for the target.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action_named("Compiling //x:y", "Rustc", 0, 100),
                slice("subprocess.run", "subprocess run", 0, 80),
            ],
        )]);
        let g = flame(&p);
        assert!(
            (0..g.len()).any(|i| g.is_detail(i)),
            "no detail frame to fold"
        );
        let rows = stats_from_flame(&g);
        assert_eq!(rows.len(), 2);
        let rustc = row(&g, &rows, "Rustc");
        assert_eq!((rustc.count, rustc.self_us, rustc.max_us), (1, 20, 20));
        assert_eq!(row(&g, &rows, "subprocess.run").self_us, 80);
        assert_eq!(self_sum(&rows), 100);
    }

    #[test]
    fn a_kind_is_one_row_and_its_instances_none() {
        // Two repository fetches, one holding a span named for what it does
        // that in turn holds a download named for its repository: each kind is
        // one row of its spans' own time, the span between them keeps a row of
        // its own, and no repository has one.
        let p = profile(vec![track(
            1,
            LaneClass::Analysis,
            vec![
                slice("@@a", "Fetching repository", 0, 100),
                slice("download", "generic", 0, 70),
                slice("fetching: repository @@a", "general information", 0, 50),
                slice("@@b", "Fetching repository", 100, 40),
                slice("fetching: repository @@b", "general information", 100, 10),
            ],
        )]);
        let g = flame(&p);
        let rows = stats_from_flame(&g);
        assert_eq!(rows.len(), 3);
        let figures = |key| {
            let r = row(&g, &rows, key);
            (r.count, r.self_us, r.max_us)
        };
        assert_eq!(figures("Fetching repository"), (2, 60, 60));
        assert_eq!(figures("download"), (1, 20, 20));
        // The two downloads sit at different places in the tree, so they are
        // two frames of one row.
        assert_eq!(figures("fetching"), (2, 60, 50));
        assert_eq!(self_sum(&rows), 140);
    }

    #[test]
    fn overlapping_enclosed_siblings_fold_back_to_the_undetailed_rows() {
        // Under //b:b two c1 spans overlap each other, so the c1 frame they
        // merge into (210) overruns the Rustc frame the two actions merge into
        // (200). The rows must read exactly as they do for the same tree built
        // without detail frames — the same lane carrying no mnemonic, so no
        // action resolves and nothing is broken out by target.
        let c1 = |start, dur| slice("c1", "general information", start, dur);
        let detailed = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action_named("Compiling //a:a", "Rustc", 0, 100),
                c1(0, 50),
                action_named("Compiling //b:b", "Rustc", 200, 100),
                c1(200, 80),
                c1(220, 80),
            ],
        )]);
        let plain = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                slice("Rustc", "action processing", 0, 100),
                c1(0, 50),
                slice("Rustc", "action processing", 200, 100),
                c1(200, 80),
                c1(220, 80),
            ],
        )]);
        let gd = flame(&detailed);
        let gp = flame(&plain);
        assert!(
            (0..gd.len()).any(|i| gd.is_detail(i)),
            "no detail frame to fold"
        );
        assert!(
            (0..gp.len()).all(|i| !gp.is_detail(i)),
            "the reference must have none"
        );
        let resolved = |g: &FlameGraph| -> Vec<(String, u32, i64, i64)> {
            let mut rows: Vec<_> = stats_from_flame(g)
                .iter()
                .map(|r| (g.str(r.key).to_owned(), r.count, r.self_us, r.max_us))
                .collect();
            rows.sort();
            rows
        };
        assert_eq!(resolved(&gd), resolved(&gp));
        // The overrun clamps the Rustc frame's self time to nothing, leaving c1
        // holding the whole 210.
        assert_eq!(
            resolved(&gd),
            [
                ("Rustc".to_owned(), 2, 0, 0),
                ("c1".to_owned(), 3, 210, 210),
            ]
        );
    }

    #[test]
    fn scope_excludes_other_lanes() {
        let p = profile(vec![
            track(7, LaneClass::Analysis, vec![action("CppCompile", 0, 100)]),
            track(8, LaneClass::Execution, vec![action("Rustc", 0, 50)]),
        ]);
        let t = p.threads();
        let g = TrackFlamer::new(&p).build(FlameLabels::Kind, &TimeSelection::default(), |i| {
            t[i].lane_class == LaneClass::Execution
        });
        let rows = stats_from_flame(&g);
        assert_eq!(rows.len(), 1);
        assert_eq!(g.str(rows[0].key), "Rustc");
        assert_eq!(rows[0].self_us, 50);
    }

    #[test]
    fn mnemonicless_slice_keeps_its_name() {
        // A slice with no mnemonic keys by its own name.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action("CppCompile", 0, 100),
                slice("checkOutputs", "general information", 200, 30),
            ],
        )]);
        let g = flame(&p);
        let rows = stats_from_flame(&g);
        assert_eq!(rows.len(), 2);
        assert_eq!(row(&g, &rows, "checkOutputs").self_us, 30);
        assert_eq!(row(&g, &rows, "CppCompile").count, 1);
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
        let ga = flame(&profile(vec![track(1, LaneClass::Execution, ordered)]));
        let mut ra = stats_from_flame(&ga);
        let gb = flame(&profile(vec![track(1, LaneClass::Execution, reversed)]));
        let mut rb = stats_from_flame(&gb);
        ra.sort_by(|x, y| ga.str(x.key).cmp(ga.str(y.key)));
        rb.sort_by(|x, y| gb.str(x.key).cmp(gb.str(y.key)));
        // Compare by resolved key: the graphs hold two independently interned
        // profiles, so equal names need not share a handle across them.
        let resolved = |g: &FlameGraph, rows: &[StatRow]| -> Vec<(String, u32, i64, i64)> {
            rows.iter()
                .map(|r| (g.str(r.key).to_owned(), r.count, r.self_us, r.max_us))
                .collect()
        };
        assert_eq!(resolved(&ga, &ra), resolved(&gb, &rb));
        let a = row(&ga, &ra, "CppCompile");
        assert_eq!(a.count, 2);
        // Both disjoint CppCompile spans merge into one frame: self time 160.
        assert_eq!(a.self_us, 160);
        assert_eq!(a.max_us, 160);
    }
}
