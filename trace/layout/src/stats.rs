use std::sync::Arc;

use bviz_trace_profile::{Profile, SpanRole, Str};

use crate::flame::{build_flame_graph, FlameGraph, Scope};

/// One aggregated group of frames, summed across the flame graph. `total_us` is
/// the summed self time of every frame keyed here, so the rows partition the
/// root's total and a group's share `total_us / Σ(all groups' total_us)` sums to
/// 100% without double-counting nested time. `max_us` is the peak single-frame
/// self time, `count` the number of merged span occurrences. `key` is a [`Str`]
/// handle into the profile held by the [`FlameGraph`] the row was derived from;
/// resolve it with that graph's [`str`](FlameGraph::str).
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

/// Aggregate the profile into one row per distinct frame name of the flame graph
/// built under `scope`, summing each frame's self time. Self time sums
/// to the root's total, so the rows partition the aggregated time without the
/// double-count of summing a span and its sub-spans. Order is unspecified; the
/// caller sorts. The returned [`FlameGraph`] resolves the rows' keys — it holds
/// the profile whose text they index — so it must be kept alongside them.
pub fn aggregate_stats(profile: &Arc<Profile>, scope: Scope) -> (FlameGraph, Vec<StatRow>) {
    let graph = build_flame_graph(profile, scope);
    let rows = stats_from_flame(&graph);
    (graph, rows)
}

/// Aggregate an already-built flame graph into its stats rows, summing each
/// frame's self time. Lets a caller that already holds the graph derive the table
/// without rebuilding it. Keys resolve with `graph`'s [`str`](FlameGraph::str).
pub fn stats_from_flame(graph: &FlameGraph) -> Vec<StatRow> {
    // A node's rank is a dense `u32` bijection with its name within one build,
    // so a table indexed by rank groups by name without hashing. Node 0 is the
    // synthetic root, whose placeholder rank 0 aliases the real
    // lexicographically-smallest name; skipping it drops its zero-self frame
    // without evicting that name's real rank-0 row.
    let nodes = graph.nodes();
    let cap = nodes.iter().map(|n| n.rank).max().unwrap_or(0) as usize + 1;
    let mut rows: Vec<Option<StatRow>> = vec![None; cap];
    for node in &nodes[1..] {
        match &mut rows[node.rank as usize] {
            Some(row) => {
                row.count += node.count;
                row.total_us += node.self_us;
                row.max_us = row.max_us.max(node.self_us);
            }
            None => {
                rows[node.rank as usize] = Some(StatRow {
                    key: node.name,
                    rank: node.rank,
                    role: node.role,
                    count: node.count,
                    total_us: node.self_us,
                    max_us: node.self_us,
                });
            }
        }
    }
    rows.into_iter().flatten().collect()
}

/// Indices into `stats` in the default display order: `total_us` descending,
/// ties broken by key rank ascending — which orders exactly as the key text
/// does — then by row index. Each row decorate-sorts on one packed integer key:
/// the total in the high bits as an order-preserving `i64`→`u64` bias inverted
/// for descending, above the rank, above the index. No two rows share an index,
/// so the order is total, and no comparison resolves a string or chases a row
/// pointer.
pub(crate) fn default_order(stats: &[StatRow]) -> Vec<u32> {
    const RANK_SHIFT: u32 = u32::BITS;
    const TOTAL_SHIFT: u32 = 2 * u32::BITS;
    const SIGN_BIT: u64 = 1 << (u64::BITS - 1);
    let mut keyed: Vec<u128> = stats
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let total_desc = !((r.total_us as u64) ^ SIGN_BIT);
            ((total_desc as u128) << TOTAL_SHIFT) | ((r.rank as u128) << RANK_SHIFT) | i as u128
        })
        .collect();
    keyed.sort_unstable();
    keyed.into_iter().map(|k| k as u32).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_profile::{action, profile, slice, track};
    use bviz_trace_profile::{LaneClass, Workload};

    /// Sum of every row's self-time total — must equal the root's total.
    fn total(rows: &[StatRow]) -> i64 {
        rows.iter().map(|r| r.total_us).sum()
    }

    fn row<'a>(graph: &FlameGraph, rows: &'a [StatRow], key: &str) -> &'a StatRow {
        rows.iter()
            .find(|r| graph.str(r.key) == key)
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
        let (g, rows) = aggregate_stats(&p, Scope::All);
        assert_eq!(rows.len(), 2);
        let c = row(&g, &rows, "CppCompile");
        assert_eq!(c.count, 2);
        assert_eq!(c.total_us, 150);
        // One merged frame; its peak self time is the merged total.
        assert_eq!(c.max_us, 150);
        assert_eq!(
            c.role,
            SpanRole { workload: Workload::Subprocess, payload: true }
        );
        assert_eq!(row(&g, &rows, "CppLink").total_us, 30);
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
        let (g, rows) = aggregate_stats(&p, Scope::All);
        assert_eq!(row(&g, &rows, "Rustc").total_us, 20);
        assert_eq!(row(&g, &rows, "subprocess.run").total_us, 80);
        assert_eq!(total(&rows), 100);
    }

    #[test]
    fn scope_excludes_other_lanes() {
        let p = profile(vec![
            track(7, LaneClass::Analysis, vec![action("CppCompile", 0, 100)]),
            track(8, LaneClass::Execution, vec![action("Rustc", 0, 50)]),
        ]);
        let (g, rows) = aggregate_stats(&p, Scope::Execution);
        assert_eq!(rows.len(), 1);
        assert_eq!(g.str(rows[0].key), "Rustc");
        assert_eq!(rows[0].total_us, 50);
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
        let (g, rows) = aggregate_stats(&p, Scope::All);
        assert_eq!(rows.len(), 2);
        assert_eq!(row(&g, &rows, "checkOutputs").total_us, 30);
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
        let (ga, mut ra) =
            aggregate_stats(&profile(vec![track(1, LaneClass::Execution, ordered)]), Scope::All);
        let (gb, mut rb) =
            aggregate_stats(&profile(vec![track(1, LaneClass::Execution, reversed)]), Scope::All);
        ra.sort_by(|x, y| ga.str(x.key).cmp(ga.str(y.key)));
        rb.sort_by(|x, y| gb.str(x.key).cmp(gb.str(y.key)));
        // Compare by resolved key: the graphs hold two independently interned
        // profiles, so equal names need not share a handle across them.
        let resolved = |g: &FlameGraph, rows: &[StatRow]| -> Vec<(String, u32, i64, i64)> {
            rows.iter()
                .map(|r| (g.str(r.key).to_owned(), r.count, r.total_us, r.max_us))
                .collect()
        };
        assert_eq!(resolved(&ga, &ra), resolved(&gb, &rb));
        let a = row(&ga, &ra, "CppCompile");
        assert_eq!(a.count, 2);
        // Both disjoint CppCompile spans merge into one frame: self time 160.
        assert_eq!(a.total_us, 160);
        assert_eq!(a.max_us, 160);
    }
}
