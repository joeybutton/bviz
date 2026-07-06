use std::sync::Arc;

use bviz_trace_profile::{LaneClass, Profile};

use crate::flame::{
    Builder, FlameGraph, Flamer, Scope, build_critical_path, class_trees, fold_all, fold_ref, seal,
    worker_count,
};
use crate::stats::{StatRow, stats_from_flame};

/// Everything the UI presents for one flame scope, derived together so a held
/// box, node, or row always resolves its name [`Str`](bviz_trace_profile::Str)
/// against the graph that produced it.
pub struct ScopeAggregate {
    pub flame: FlameGraph,
    /// One row per distinct frame name; unordered, indexed by `default_order`.
    pub stats: Vec<StatRow>,
    /// Indices into `stats` in the default display order: total descending,
    /// ties by key text ascending.
    pub default_order: Vec<u32>,
    /// Summed `total_us` over `stats` — the denominator of a row's share.
    pub grand_total: i64,
}

impl ScopeAggregate {
    fn from_graph(flame: FlameGraph) -> ScopeAggregate {
        let stats = stats_from_flame(&flame);
        let mut default_order: Vec<u32> = (0..stats.len() as u32).collect();
        default_order.sort_by(|&ai, &bi| {
            let a = &stats[ai as usize];
            let b = &stats[bi as usize];
            b.total_us
                .cmp(&a.total_us)
                .then_with(|| flame.str(a.key).cmp(flame.str(b.key)))
        });
        let grand_total = stats.iter().map(|r| r.total_us).sum();
        ScopeAggregate {
            flame,
            stats,
            default_order,
            grand_total,
        }
    }
}

/// The aggregates of every flame scope, built from one pass over the profile:
/// the canonical string map and mnemonic index are scope-independent and shared,
/// and each lane is walked once into its lane class's tree, from which the
/// class-scoped graphs and their union `All` all derive. The critical-path
/// aggregate exists only when the profile has a critical-path lane.
pub struct FlameScopes {
    critical_path: Option<ScopeAggregate>,
    loading: ScopeAggregate,
    analysis: ScopeAggregate,
    execution: ScopeAggregate,
    all: ScopeAggregate,
}

impl FlameScopes {
    pub fn build(profile: &Arc<Profile>) -> FlameScopes {
        FlameScopes::build_with_workers(profile, worker_count())
    }

    /// Parallelism override, so the sharded build can be asserted equal to the
    /// single-worker one; [`FlameScopes::build`] derives the count from the
    /// parallelism granted to the process.
    #[doc(hidden)]
    pub fn build_with_workers(profile: &Arc<Profile>, workers: usize) -> FlameScopes {
        let flamer = Flamer::new(profile, workers);

        let mut loading = None;
        let mut analysis = None;
        let mut execution = None;
        let mut rest = Vec::new();
        for (class, tree) in class_trees(profile, &flamer, |_| true, workers) {
            match class {
                LaneClass::RepoFetch => loading = Some(tree),
                LaneClass::Analysis => analysis = Some(tree),
                LaneClass::Execution => execution = Some(tree),
                _ => rest.push(tree),
            }
        }
        // The union tree folds copies of the scoped trees (they still seal into
        // graphs of their own) and consumes the rest.
        let mut all = fold_all(rest);
        for tree in [&loading, &analysis, &execution].into_iter().flatten() {
            fold_ref(&mut all, tree);
        }

        let aggregate = |tree: Option<Builder>| {
            let mut root = tree.unwrap_or_else(Builder::new_root);
            ScopeAggregate::from_graph(seal(&mut root, profile))
        };
        // The five scopes now own disjoint trees, so they seal, aggregate, and
        // sort concurrently.
        let mut cp_slot = None;
        let mut loading_slot = None;
        let mut analysis_slot = None;
        let mut execution_slot = None;
        let mut all_slot = None;
        std::thread::scope(|scope| {
            if profile.has_critical_path() {
                scope.spawn(|| {
                    let mut root = Builder::new_root();
                    build_critical_path(profile, &flamer, &mut root);
                    cp_slot = Some(ScopeAggregate::from_graph(seal(&mut root, profile)));
                });
            }
            scope.spawn(|| loading_slot = Some(aggregate(loading)));
            scope.spawn(|| analysis_slot = Some(aggregate(analysis)));
            scope.spawn(|| execution_slot = Some(aggregate(execution)));
            scope.spawn(|| all_slot = Some(aggregate(Some(all))));
        });
        FlameScopes {
            critical_path: cp_slot,
            loading: loading_slot.unwrap(),
            analysis: analysis_slot.unwrap(),
            execution: execution_slot.unwrap(),
            all: all_slot.unwrap(),
        }
    }

    /// The aggregate presented under `scope`; `None` only for
    /// [`Scope::CriticalPath`] on a profile without a critical-path lane.
    pub fn get(&self, scope: Scope) -> Option<&ScopeAggregate> {
        match scope {
            Scope::CriticalPath => self.critical_path.as_ref(),
            Scope::Loading => Some(&self.loading),
            Scope::Analysis => Some(&self.analysis),
            Scope::Execution => Some(&self.execution),
            Scope::All => Some(&self.all),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flame::build_flame_graph;
    use crate::stats::aggregate_stats;
    use crate::test_profile::{action, action_named, profile, slice, track};

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

    const ALL_SCOPES: [Scope; 5] = [
        Scope::CriticalPath,
        Scope::Loading,
        Scope::Analysis,
        Scope::Execution,
        Scope::All,
    ];

    #[test]
    fn set_matches_single_scope_builds() {
        let p = full_profile();
        let set = FlameScopes::build(&p);
        for scope in ALL_SCOPES {
            let agg = set.get(scope).expect("scope present");
            let (single_graph, single_rows) = aggregate_stats(&p, scope);
            assert_eq!(resolved(&agg.flame), resolved(&single_graph), "{scope:?}");
            assert_eq!(
                resolved_rows(&agg.flame, &agg.stats),
                resolved_rows(&single_graph, &single_rows),
                "{scope:?}"
            );
        }
    }

    #[test]
    fn all_scope_unions_the_classes() {
        let p = full_profile();
        let set = FlameScopes::build(&p);
        let all = set.get(Scope::All).unwrap();
        // Every real lane's top-level time lands in All exactly once.
        assert_eq!(
            all.flame.root().total_us,
            300 + 5 + 80 + 40 + 100 + 60 + 50 + 20
        );
        // The scoped graphs partition their classes out of the same walk.
        assert_eq!(set.get(Scope::Loading).unwrap().flame.root().total_us, 80);
        assert_eq!(set.get(Scope::Analysis).unwrap().flame.root().total_us, 40);
        assert_eq!(
            set.get(Scope::Execution).unwrap().flame.root().total_us,
            210
        );
    }

    #[test]
    fn critical_path_absent_yields_none_and_empty_scopes_bare_roots() {
        let p = profile(vec![track(
            5,
            LaneClass::Execution,
            vec![action("Rustc", 0, 50)],
        )]);
        let set = FlameScopes::build(&p);
        assert!(set.get(Scope::CriticalPath).is_none());
        let loading = set.get(Scope::Loading).unwrap();
        assert_eq!(loading.flame.nodes().len(), 1);
        assert_eq!(loading.flame.root().total_us, 0);
        assert!(loading.stats.is_empty());
        assert_eq!(loading.grand_total, 0);
    }

    #[test]
    fn critical_path_present_matches_single_build() {
        let p = full_profile();
        let set = FlameScopes::build(&p);
        let cp = set.get(Scope::CriticalPath).expect("critical path");
        // The graft pulls the worker action's subtree, not the bare component.
        assert_eq!(
            resolved(&cp.flame),
            resolved(&build_flame_graph(&p, Scope::CriticalPath))
        );
    }

    #[test]
    fn sharded_build_matches_serial() {
        // Enough lanes that several workers really shard, spread over the
        // classes, with duplicated handles so the shards must agree on the
        // canonical handle of a text they see under different raw handles.
        let mut tracks = vec![track(
            0,
            LaneClass::CriticalPath,
            vec![slice(
                "action 'Compiling foo'",
                "critical path component",
                900,
                40,
            )],
        )];
        for i in 0..4i64 {
            let base = i * 1000;
            tracks.push(track(
                10 + i,
                LaneClass::Execution,
                vec![
                    action_named("Compiling foo", "Rustc", base, 100),
                    slice("subprocess.run", "subprocess run", base, 80),
                    action("CppCompile", base + 200, 50),
                ],
            ));
            tracks.push(track(
                20 + i,
                LaneClass::Analysis,
                vec![action("CppCompile", base, 30)],
            ));
            tracks.push(track(
                30 + i,
                LaneClass::RepoFetch,
                vec![slice("fetch", "generic", base, 10)],
            ));
            tracks.push(track(
                40 + i,
                LaneClass::Other,
                vec![slice("idle", "generic", base, 5)],
            ));
        }
        let mut p = profile(tracks);
        let p_mut = Arc::get_mut(&mut p).unwrap();
        p_mut.duplicate_str_for_test("Rustc");
        p_mut.duplicate_str_for_test("CppCompile");

        let serial = FlameScopes::build_with_workers(&p, 1);
        for workers in [2, 5, 16] {
            let sharded = FlameScopes::build_with_workers(&p, workers);
            for scope in ALL_SCOPES {
                match (serial.get(scope), sharded.get(scope)) {
                    (Some(a), Some(b)) => {
                        assert_eq!(a.flame.nodes(), b.flame.nodes(), "{scope:?} x{workers}");
                        assert_eq!(a.stats, b.stats, "{scope:?} x{workers}");
                        assert_eq!(a.default_order, b.default_order, "{scope:?} x{workers}");
                        assert_eq!(a.grand_total, b.grand_total, "{scope:?} x{workers}");
                    }
                    (None, None) => {}
                    _ => panic!("critical-path presence differs at {workers} workers"),
                }
            }
        }
    }

    #[test]
    fn default_order_is_total_desc_then_key_text() {
        let p = full_profile();
        let set = FlameScopes::build(&p);
        for scope in ALL_SCOPES {
            let agg = set.get(scope).unwrap();
            assert_eq!(agg.default_order.len(), agg.stats.len());
            let keys: Vec<(i64, &str)> = agg
                .default_order
                .iter()
                .map(|&i| {
                    let r = &agg.stats[i as usize];
                    (-r.total_us, agg.flame.str(r.key))
                })
                .collect();
            assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{scope:?}: {keys:?}");
            assert_eq!(
                agg.grand_total,
                agg.stats.iter().map(|r| r.total_us).sum::<i64>()
            );
        }
    }
}
