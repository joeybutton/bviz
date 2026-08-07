use std::sync::Arc;

use bviz_trace_profile::Profile;

use crate::flame::{FlameDirection, FlameGraph, FlameMode, TrackFlamer, invert};
use crate::selection::TimeSelection;
use crate::stats::{StatRow, default_order, stats_from_flame};

/// Everything the UI presents for one flame scope, derived together so a held
/// box, node, or row always resolves its name [`Str`](bviz_trace_profile::Str)
/// against the graph that produced it. The inputs it was built under travel
/// with it, so a view presenting it names what it is showing rather than what a
/// later rebuild is targeting.
pub struct ScopeAggregate {
    pub mode: FlameMode,
    /// The profile time its figures are restricted to.
    pub window: TimeSelection,
    pub flame: FlameGraph,
    /// One row per distinct frame name; unordered, indexed by `default_order`.
    /// A frame's self time is a property of the profile's own nesting, so the
    /// rows read the same whichever direction `flame` presents.
    pub stats: Vec<StatRow>,
    /// Indices into `stats` in the default display order (see
    /// [`default_order`]).
    pub default_order: Vec<u32>,
    /// Summed `total_us` over `stats` — the denominator of a row's share.
    pub grand_total: i64,
}

impl ScopeAggregate {
    /// Builds the aggregate over exactly the thread tracks `selected` admits (by
    /// index into [`Profile::threads`]), for a view driven by a lane selection
    /// rather than a fixed scope. Scans the whole profile and builds the per-lane
    /// node tables for a throwaway [`TrackFlamer`]; a caller rebuilding several
    /// selections should hold one [`TrackFlamer`] and go through
    /// [`Self::build_for_tracks_reusing`] instead, so that precompute is paid
    /// once.
    pub fn build_for_tracks(
        profile: &Arc<Profile>,
        mode: FlameMode,
        window: &TimeSelection,
        selected: impl Fn(usize) -> bool + Sync,
    ) -> ScopeAggregate {
        ScopeAggregate::from_graph(
            TrackFlamer::new(profile).build(mode.labels, window, selected),
            mode,
            window,
        )
    }

    /// Builds a selection's aggregate from an already-scanned [`TrackFlamer`] by
    /// merging its cached per-lane node tables (see [`TrackFlamer::build`]),
    /// skipping the whole-profile scan and per-lane reduction so a view can
    /// rebuild across selection changes paying only the merge.
    pub fn build_for_tracks_reusing(
        scan: &TrackFlamer,
        mode: FlameMode,
        window: &TimeSelection,
        selected: impl Fn(usize) -> bool + Sync,
    ) -> ScopeAggregate {
        ScopeAggregate::from_graph(scan.build(mode.labels, window, selected), mode, window)
    }

    /// Derives the aggregate from the top-down graph `mode.labels` builds,
    /// inverting it afterwards where `mode.direction` asks for the bottom-up
    /// presentation.
    fn from_graph(flame: FlameGraph, mode: FlameMode, window: &TimeSelection) -> ScopeAggregate {
        let stats = stats_from_flame(&flame);
        let default_order = default_order(&stats);
        let grand_total = stats.iter().map(|r| r.total_us).sum();
        let flame = match mode.direction {
            FlameDirection::TopDown => flame,
            FlameDirection::BottomUp => invert(flame),
        };
        ScopeAggregate {
            mode,
            window: window.clone(),
            flame,
            stats,
            default_order,
            grand_total,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::span_between;
    use crate::test_profile::{action, action_named, profile, slice, track};
    use bviz_trace_profile::LaneClass;

    /// A profile touching every lane class plus a critical path, with nesting
    /// and cross-lane merges.
    fn full_profile() -> Arc<Profile> {
        profile(vec![
            track(
                1,
                LaneClass::Main,
                vec![slice("runAnalysisPhase", "generic", 0, 300)],
            ),
            track(
                2,
                LaneClass::Gc,
                vec![slice("minor GC", "gc notification", 10, 5)],
            ),
            track(
                3,
                LaneClass::RepoFetch,
                vec![slice("@rules_rs//:fetch", "generic", 0, 80)],
            ),
            track(4, LaneClass::Analysis, vec![action("CppCompile", 0, 40)]),
            track(
                5,
                LaneClass::Execution,
                vec![
                    action_named("Compiling foo", "Rustc", 0, 100),
                    slice("subprocess.run", "subprocess run", 0, 80),
                    action("Rustc", 200, 60),
                ],
            ),
            track(6, LaneClass::Execution, vec![action("Rustc", 0, 50)]),
            track(7, LaneClass::Other, vec![slice("idle", "generic", 0, 20)]),
            track(
                0,
                LaneClass::CriticalPath,
                vec![slice(
                    "action 'Compiling foo'",
                    "critical path component",
                    500,
                    100,
                )],
            ),
        ])
    }

    /// A graph's nodes with names resolved, so two graphs over one profile
    /// compare structurally.
    fn resolved(g: &FlameGraph) -> Vec<(String, i64, i64, i64, u32, u32)> {
        g.nodes()
            .iter()
            .map(|n| {
                (
                    g.str(n.name).to_owned(),
                    n.total_us,
                    n.self_us,
                    n.width_us,
                    n.count,
                    n.depth,
                )
            })
            .collect()
    }

    fn resolved_rows(g: &FlameGraph, rows: &[StatRow]) -> Vec<(String, u32, i64, i64)> {
        let mut rows: Vec<_> = rows
            .iter()
            .map(|r| (g.str(r.key).to_owned(), r.count, r.total_us, r.max_us))
            .collect();
        rows.sort();
        rows
    }

    /// Summed `total_us` of every span frame named `name` in the graph. Robust
    /// frames occur once, but summing folds any repeat so the check never
    /// depends on a name being unique. Detail frames re-label part of their
    /// parent's time, so they are not spans of their own and are left out.
    fn frame_total(g: &FlameGraph, name: &str) -> i64 {
        span_frames(g)
            .filter(|&i| g.str(g.name(i)) == name)
            .map(|i| g.total_us(i))
            .sum()
    }

    /// The graph's non-detail node ids.
    fn span_frames(g: &FlameGraph) -> impl Iterator<Item = usize> + '_ {
        (0..g.len()).filter(|&i| !g.is_detail(i))
    }

    #[test]
    fn non_cp_selection_partitions_the_root() {
        let p = full_profile();
        let t = p.threads();
        let select = |i: usize| t[i].lane_class != LaneClass::CriticalPath;
        let agg = ScopeAggregate::build_for_tracks(
            &p,
            FlameMode::default(),
            &TimeSelection::default(),
            select,
        );
        let g = &agg.flame;
        let root_total = g.root().total_us;

        // Self time partitions the root: summed once per node it equals the root
        // total, so no time-contained span is counted at both itself and its
        // parent.
        let self_sum: i64 = g.nodes().iter().map(|n| n.self_us).sum();
        assert_eq!(self_sum, root_total);
        // The stats rows are that same self-time partition, grouped by name.
        let rows_total: i64 = agg.stats.iter().map(|r| r.total_us).sum();
        assert_eq!(rows_total, root_total);
        assert_eq!(agg.grand_total, root_total);

        // Known frames from full_profile's slice durations. Leaves carry their own
        // duration; subprocess.run nests under a Rustc action; the three
        // execution "Rustc" spans (two lanes, mnemonic-tagged and bare) merge into
        // one frame summing 100 + 60 + 50.
        assert_eq!(frame_total(g, "runAnalysisPhase"), 300);
        assert_eq!(frame_total(g, "@rules_rs//:fetch"), 80);
        assert_eq!(frame_total(g, "CppCompile"), 40);
        assert_eq!(frame_total(g, "subprocess.run"), 80);
        assert_eq!(frame_total(g, "minor GC"), 5);
        assert_eq!(frame_total(g, "idle"), 20);
        assert_eq!(frame_total(g, "Rustc"), 210);
        assert_eq!(root_total, 655);

        // A second build of the same selection is byte-identical.
        assert_eq!(
            g.nodes(),
            ScopeAggregate::build_for_tracks(
                &p,
                FlameMode::default(),
                &TimeSelection::default(),
                select
            )
            .flame
            .nodes()
        );
    }

    #[test]
    fn empty_track_selection_yields_a_bare_root() {
        let p = full_profile();
        // Either direction: there is no frame for the inversion to walk out of.
        for direction in [FlameDirection::TopDown, FlameDirection::BottomUp] {
            let mode = FlameMode { direction, ..FlameMode::default() };
            let agg =
                ScopeAggregate::build_for_tracks(&p, mode, &TimeSelection::default(), |_| false);
            // Only the synthetic root, no frames and no rows.
            assert_eq!(agg.flame.nodes().len(), 1);
            assert_eq!(agg.flame.root().total_us, 0);
            assert!(agg.stats.is_empty());
            assert_eq!(agg.grand_total, 0);
        }
    }

    #[test]
    fn reusing_a_track_flamer_matches_fresh_builds() {
        // One whole-profile scan, reused across selections, must reproduce for
        // each selection the graph and stats a fresh per-selection build gives.
        let p = full_profile();
        let threads = p.threads();
        let scan = TrackFlamer::new(&p);
        let one_execution = threads
            .iter()
            .position(|t| t.lane_class == LaneClass::Execution)
            .unwrap();
        let selections: Vec<Box<dyn Fn(usize) -> bool + Sync>> = vec![
            Box::new(|i: usize| threads[i].lane_class != LaneClass::CriticalPath),
            Box::new(move |i: usize| i == one_execution),
            Box::new(|_: usize| false),
            // Every lane, the critical-path lane included: its components resolve
            // their labels through the same whole-profile mnemonic index, so the
            // reused scan must match a fresh build here too.
            Box::new(|_: usize| true),
            Box::new(|i: usize| i % 2 == 0),
        ];
        // Ranks are dense over each build's own scanned texts, so the global
        // scan and a per-selection scan number names differently; the default
        // order is compared through resolved keys, never raw rank values.
        let order = |a: &ScopeAggregate| -> Vec<String> {
            a.default_order
                .iter()
                .map(|&i| a.flame.str(a.stats[i as usize].key).to_owned())
                .collect()
        };
        for sel in &selections {
            let reused = ScopeAggregate::build_for_tracks_reusing(
                &scan,
                FlameMode::default(),
                &TimeSelection::default(),
                sel,
            );
            let fresh = ScopeAggregate::build_for_tracks(
                &p,
                FlameMode::default(),
                &TimeSelection::default(),
                sel,
            );
            assert_eq!(resolved(&reused.flame), resolved(&fresh.flame));
            assert_eq!(
                resolved_rows(&reused.flame, &reused.stats),
                resolved_rows(&fresh.flame, &fresh.stats),
            );
            assert_eq!(order(&reused), order(&fresh));
            assert_eq!(reused.grand_total, fresh.grand_total);
        }
    }

    #[test]
    fn an_aggregate_carries_the_window_its_figures_cover() {
        let p = full_profile();
        let mut window = TimeSelection::default();
        window.insert(span_between(0, 100));
        let agg = ScopeAggregate::build_for_tracks(&p, FlameMode::default(), &window, |_| true);
        assert_eq!(agg.window, window);
        let unrestricted = ScopeAggregate::build_for_tracks(
            &p,
            FlameMode::default(),
            &TimeSelection::default(),
            |_| true,
        );
        assert!(unrestricted.window.is_empty());
        // The window bites: the profile runs past it, so the two differ.
        assert!(agg.grand_total < unrestricted.grand_total);
    }

    #[test]
    fn class_selections_sum_to_their_union() {
        let p = full_profile();
        let t = p.threads();
        let class_total = |c: LaneClass| {
            ScopeAggregate::build_for_tracks(
                &p,
                FlameMode::default(),
                &TimeSelection::default(),
                move |i| t[i].lane_class == c,
            )
            .flame
            .root()
            .total_us
        };
        let loading = class_total(LaneClass::RepoFetch);
        let analysis = class_total(LaneClass::Analysis);
        let execution = class_total(LaneClass::Execution);
        assert_eq!(loading, 80);
        assert_eq!(analysis, 40);
        // Two execution lanes, three Rustc spans merging into one frame.
        assert_eq!(execution, 210);
        // A selection's root total is additive over disjoint lane sets: the three
        // classes unioned total exactly the sum of the three.
        let union = ScopeAggregate::build_for_tracks(
            &p,
            FlameMode::default(),
            &TimeSelection::default(),
            |i| {
                matches!(
                    t[i].lane_class,
                    LaneClass::RepoFetch | LaneClass::Analysis | LaneClass::Execution
                )
            },
        )
        .flame
        .root()
        .total_us;
        assert_eq!(union, loading + analysis + execution);
        assert_eq!(union, 330);
    }

    #[test]
    fn carrier_resolves_to_a_foreign_lanes_smallest_mnemonic() {
        // A mnemonic-less "Compiling foo" carrier on the analysis lane; the same
        // action name carries Rustc and Javac mnemonics on execution lanes. The
        // whole-profile scan resolves the carrier's label to the byte-wise
        // smallest mnemonic (Javac), even when only the analysis lane is selected.
        let p = profile(vec![
            track(
                1,
                LaneClass::Execution,
                vec![action_named("Compiling foo", "Rustc", 0, 100)],
            ),
            track(
                2,
                LaneClass::Execution,
                vec![action_named("Compiling foo", "Javac", 0, 60)],
            ),
            track(
                3,
                LaneClass::Analysis,
                vec![
                    action("CppCompile", 0, 40),
                    slice("Compiling foo", "action processing", 50, 20),
                ],
            ),
        ]);
        let t = p.threads();
        let agg = ScopeAggregate::build_for_tracks(
            &p,
            FlameMode::default(),
            &TimeSelection::default(),
            |i| t[i].lane_class == LaneClass::Analysis,
        );
        let g = &agg.flame;
        // The carrier joined the foreign smallest-mnemonic frame carrying its own
        // 20us; no span frame is left under the carrier's own name.
        assert_eq!(frame_total(g, "Javac"), 20);
        assert!(span_frames(g).all(|i| g.str(g.name(i)) != "Compiling foo"));
        assert!(
            agg.stats.iter().any(|r| g.str(r.key) == "Javac"),
            "the foreign-lane mnemonic must label the carrier"
        );
    }

    #[test]
    fn ranks_mirror_byte_wise_text_order() {
        // Tricky keys: empty, shared prefixes, case splits, and non-ASCII.
        // `&str` ordering is byte-wise, so "é" (0xC3 0xA9) sorts after "z"
        // rather than near "e"; ranks must reproduce exactly that order.
        let keys = [
            "", "A", "Z", "a", "a!", "aB", "aa", "ab", "b", "e", "z", "\u{7f}", "é", "😀",
        ];
        let slices = keys
            .iter()
            .enumerate()
            .map(|(i, k)| slice(k, "generic", i as i64 * 10, 5))
            .collect();
        let p = profile(vec![track(1, LaneClass::Execution, slices)]);
        let agg = ScopeAggregate::build_for_tracks(
            &p,
            FlameMode::default(),
            &TimeSelection::default(),
            |_| true,
        );
        assert_eq!(agg.stats.len(), keys.len());
        let text_of = |r: &StatRow| agg.flame.str(r.key).to_owned();
        let mut by_rank: Vec<String> = {
            let mut rows: Vec<&StatRow> = agg.stats.iter().collect();
            rows.sort_by_key(|r| r.rank);
            rows.into_iter().map(text_of).collect()
        };
        let by_text = {
            let mut texts = by_rank.clone();
            texts.sort_unstable();
            texts
        };
        assert_eq!(by_rank, by_text);
        // All rows tie on total, so the default order is pure key order:
        // strictly ascending by text.
        let ordered: Vec<String> = agg
            .default_order
            .iter()
            .map(|&i| text_of(&agg.stats[i as usize]))
            .collect();
        assert_eq!(ordered, by_text);
        by_rank.dedup();
        assert_eq!(by_rank.len(), keys.len(), "keys must be distinct");
    }

    #[test]
    fn default_order_is_total_desc_then_key_text() {
        let p = full_profile();
        let t = p.threads();
        let selections: Vec<Box<dyn Fn(usize) -> bool + Sync>> = vec![
            Box::new(|i: usize| t[i].lane_class != LaneClass::CriticalPath),
            Box::new(|i: usize| t[i].lane_class == LaneClass::Execution),
        ];
        for sel in &selections {
            let agg = ScopeAggregate::build_for_tracks(
                &p,
                FlameMode::default(),
                &TimeSelection::default(),
                sel,
            );
            assert_eq!(agg.default_order.len(), agg.stats.len());
            let keys: Vec<(i64, &str)> = agg
                .default_order
                .iter()
                .map(|&i| {
                    let r = &agg.stats[i as usize];
                    (-r.total_us, agg.flame.str(r.key))
                })
                .collect();
            assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{keys:?}");
            assert_eq!(
                agg.grand_total,
                agg.stats.iter().map(|r| r.total_us).sum::<i64>()
            );
        }
    }
}
