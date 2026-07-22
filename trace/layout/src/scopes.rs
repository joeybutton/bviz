use std::sync::Arc;

use bviz_trace_profile::{LaneClass, Profile};

use crate::flame::{
    Builder, FlameGraph, Flamer, Scope, TrackFlamer, action_locations, build_critical_path,
    build_flame_graph, class_trees, fold_all, fold_into, seal,
};
use crate::stats::{StatRow, default_order, stats_from_flame};

/// Everything the UI presents for one flame scope, derived together so a held
/// box, node, or row always resolves its name [`Str`](bviz_trace_profile::Str)
/// against the graph that produced it.
pub struct ScopeAggregate {
    pub flame: FlameGraph,
    /// One row per distinct frame name; unordered, indexed by `default_order`.
    pub stats: Vec<StatRow>,
    /// Indices into `stats` in the default display order (see
    /// [`default_order`]).
    pub default_order: Vec<u32>,
    /// Summed `total_us` over `stats` — the denominator of a row's share.
    pub grand_total: i64,
}

impl ScopeAggregate {
    /// Builds the one scope's aggregate on its own, for a caller that needs a
    /// single scope before (or without) the full [`FlameScopes`] pass.
    pub fn build(profile: &Arc<Profile>, scope: Scope) -> ScopeAggregate {
        ScopeAggregate::from_graph(build_flame_graph(profile, scope))
    }

    /// Builds the aggregate over exactly the thread tracks `selected` admits (by
    /// index into [`Profile::threads`]), for a view driven by a lane selection
    /// rather than a fixed scope. Scans the whole profile for a throwaway
    /// [`TrackFlamer`]; a caller rebuilding several selections should hold one
    /// [`TrackFlamer`] and go through [`Self::build_for_tracks_reusing`] instead,
    /// so the scan is paid once.
    pub fn build_for_tracks(
        profile: &Arc<Profile>,
        selected: impl Fn(usize) -> bool + Sync,
    ) -> ScopeAggregate {
        ScopeAggregate::from_graph(TrackFlamer::new(profile).build(selected))
    }

    /// Builds a selection's aggregate from an already-scanned [`TrackFlamer`],
    /// skipping the whole-profile scan so a view can rebuild across selection
    /// changes without rescanning.
    pub fn build_for_tracks_reusing(
        scan: &TrackFlamer,
        selected: impl Fn(usize) -> bool + Sync,
    ) -> ScopeAggregate {
        ScopeAggregate::from_graph(scan.build(selected))
    }

    /// Builds a selection's aggregate from an already-scanned [`TrackFlamer`] by
    /// the path-sort accumulation of
    /// [`build_flame_graph_for_tracks_columnar`](crate::flame). It produces the
    /// same aggregate as [`Self::build_for_tracks_reusing`] by a different
    /// aggregation path.
    pub fn build_for_tracks_columnar(
        scan: &TrackFlamer,
        selected: impl Fn(usize) -> bool + Sync,
    ) -> ScopeAggregate {
        ScopeAggregate::from_graph(scan.build_columnar(selected))
    }

    /// Builds a selection's aggregate from an already-scanned [`TrackFlamer`] by
    /// merging the selection's cached per-lane node tables (see
    /// [`TrackFlamer::build_columnar_cached`](crate::flame)), so the per-span
    /// emission, keying, and per-lane sort are paid once when the scan is built
    /// rather than on every selection. Produces the same aggregate as
    /// [`Self::build_for_tracks_reusing`].
    pub fn build_for_tracks_columnar_cached(
        scan: &TrackFlamer,
        selected: impl Fn(usize) -> bool + Sync,
    ) -> ScopeAggregate {
        ScopeAggregate::from_graph(scan.build_columnar_cached(selected))
    }

    fn from_graph(flame: FlameGraph) -> ScopeAggregate {
        let stats = stats_from_flame(&flame);
        let default_order = default_order(&stats);
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
/// the label rank table and mnemonic index are scope-independent and shared,
/// and each lane is walked once into its lane class's tree, from which the
/// class-scoped graphs and their union `All` all derive. The critical-path
/// aggregate exists only when the profile has a critical-path lane.
/// Each aggregate is held as its own [`Arc`] so a caller can serve one scope's
/// handle on its own, keyed by identity, without cloning the aggregate.
pub struct FlameScopes {
    critical_path: Option<Arc<ScopeAggregate>>,
    loading: Arc<ScopeAggregate>,
    analysis: Arc<ScopeAggregate>,
    execution: Arc<ScopeAggregate>,
    all: Arc<ScopeAggregate>,
}

/// The three class scopes sealed, each paired with its [`Builder`] tree, plus
/// the critical path when it was sealed alongside them.
struct SealedScopes {
    loading: (ScopeAggregate, Builder),
    analysis: (ScopeAggregate, Builder),
    execution: (ScopeAggregate, Builder),
    critical_path: Option<ScopeAggregate>,
}

impl FlameScopes {
    pub fn build(profile: &Arc<Profile>) -> FlameScopes {
        FlameScopes::build_with_workers(profile, bviz_trace_profile::worker_count(), None, None)
    }

    /// Builds the set while reusing an already-computed aggregate for `scope` —
    /// the scope a caller has just served on demand, which is always the profile
    /// default ([`Scope::All`], or [`Scope::CriticalPath`] when the profile has a
    /// critical path). Reusing `All` also lets the set pass skip building and
    /// sealing the union entirely; reusing `CriticalPath` skips the graft. Any
    /// other scope is rebuilt normally.
    pub fn build_reusing(
        profile: &Arc<Profile>,
        scope: Scope,
        aggregate: Arc<ScopeAggregate>,
    ) -> FlameScopes {
        let workers = bviz_trace_profile::worker_count();
        match scope {
            Scope::All => FlameScopes::build_with_workers(profile, workers, Some(aggregate), None),
            Scope::CriticalPath => {
                FlameScopes::build_with_workers(profile, workers, None, Some(aggregate))
            }
            _ => FlameScopes::build_with_workers(profile, workers, None, None),
        }
    }

    /// Parallelism override, so the sharded build can be asserted equal to the
    /// single-worker one; [`FlameScopes::build`] derives the count from the
    /// parallelism granted to the process. `prebuilt_all` / `prebuilt_cp` supply
    /// those scopes when the caller already holds them (see [`build_reusing`]).
    fn build_with_workers(
        profile: &Arc<Profile>,
        workers: usize,
        prebuilt_all: Option<Arc<ScopeAggregate>>,
        prebuilt_cp: Option<Arc<ScopeAggregate>>,
    ) -> FlameScopes {
        let flamer = Flamer::new(profile, workers);
        let reuse_all = prebuilt_all.is_some();

        let mut loading = None;
        let mut analysis = None;
        let mut execution = None;
        let mut rest = Vec::new();
        // With the union already built, the other classes exist only to fold
        // into it, so a reusing build walks just the three class scopes' lanes.
        let include = |class: LaneClass| {
            !reuse_all
                || matches!(
                    class,
                    LaneClass::RepoFetch | LaneClass::Analysis | LaneClass::Execution
                )
        };
        for (class, tree) in class_trees(profile, &flamer, include, workers) {
            match class {
                LaneClass::RepoFetch => loading = Some(tree),
                LaneClass::Analysis => analysis = Some(tree),
                LaneClass::Execution => execution = Some(tree),
                _ => rest.push(tree),
            }
        }

        let include_critical_path = prebuilt_cp.is_none() && profile.has_critical_path();
        let sealed = Self::seal_scopes(
            profile,
            &flamer,
            loading,
            analysis,
            execution,
            include_critical_path,
        );
        let (loading, loading_tree) = sealed.loading;
        let (analysis, analysis_tree) = sealed.analysis;
        let (execution, execution_tree) = sealed.execution;

        let all = prebuilt_all.unwrap_or_else(|| {
            Self::fold_union(profile, rest, [loading_tree, analysis_tree, execution_tree])
        });
        FlameScopes {
            critical_path: prebuilt_cp.or_else(|| sealed.critical_path.map(Arc::new)),
            loading: Arc::new(loading),
            analysis: Arc::new(analysis),
            execution: Arc::new(execution),
            all,
        }
    }

    /// Seals the three class scopes concurrently, the critical path alongside
    /// when `include_critical_path`. Each class scope keeps its now-sorted tree
    /// so the union folds it by move rather than deep-copying: the sort only
    /// reorders children, which the union re-sorts, so the merge is unchanged.
    fn seal_scopes(
        profile: &Arc<Profile>,
        flamer: &Flamer,
        loading: Option<Builder>,
        analysis: Option<Builder>,
        execution: Option<Builder>,
        include_critical_path: bool,
    ) -> SealedScopes {
        let seal_scope = |tree: Option<Builder>| -> (ScopeAggregate, Builder) {
            let mut root = tree.unwrap_or_else(Builder::new_root);
            let graph = seal(&mut root, profile);
            (ScopeAggregate::from_graph(graph), root)
        };
        let mut cp_slot = None;
        let mut loading_slot = None;
        let mut analysis_slot = None;
        let mut execution_slot = None;
        std::thread::scope(|scope| {
            if include_critical_path {
                scope.spawn(|| {
                    let actions = action_locations(profile);
                    let mut root = Builder::new_root();
                    build_critical_path(profile, flamer, &actions, &mut root);
                    cp_slot = Some(ScopeAggregate::from_graph(seal(&mut root, profile)));
                });
            }
            scope.spawn(|| loading_slot = Some(seal_scope(loading)));
            scope.spawn(|| analysis_slot = Some(seal_scope(analysis)));
            scope.spawn(|| execution_slot = Some(seal_scope(execution)));
        });
        SealedScopes {
            loading: loading_slot.unwrap(),
            analysis: analysis_slot.unwrap(),
            execution: execution_slot.unwrap(),
            critical_path: cp_slot,
        }
    }

    /// Folds the unclassed trees and the three sealed class trees by move into
    /// the union, then seals it.
    fn fold_union(
        profile: &Arc<Profile>,
        rest: Vec<Builder>,
        class_trees: [Builder; 3],
    ) -> Arc<ScopeAggregate> {
        let mut all = fold_all(rest);
        for tree in class_trees {
            fold_into(&mut all, tree);
        }
        Arc::new(ScopeAggregate::from_graph(seal(&mut all, profile)))
    }

    /// The aggregate presented under `scope`; `None` only for
    /// [`Scope::CriticalPath`] on a profile without a critical-path lane.
    pub fn get(&self, scope: Scope) -> Option<&Arc<ScopeAggregate>> {
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
    fn tracks_selection_matches_all_scope() {
        // Selecting every non-critical-path lane is the same membership
        // Scope::All uses, so both builds must yield the same graph and stats.
        let p = full_profile();
        let threads = p.threads();
        let select = |i: usize| threads[i].lane_class != LaneClass::CriticalPath;
        let by_tracks = ScopeAggregate::build_for_tracks(&p, select);
        let all = ScopeAggregate::build(&p, Scope::All);
        assert_eq!(resolved(&by_tracks.flame), resolved(&all.flame));
        assert_eq!(
            resolved_rows(&by_tracks.flame, &by_tracks.stats),
            resolved_rows(&all.flame, &all.stats),
        );
        assert_eq!(by_tracks.grand_total, all.grand_total);
    }

    #[test]
    fn empty_track_selection_yields_a_bare_root() {
        let p = full_profile();
        let agg = ScopeAggregate::build_for_tracks(&p, |_| false);
        // Only the synthetic root, no frames and no rows.
        assert_eq!(agg.flame.nodes().len(), 1);
        assert!(agg.stats.is_empty());
        assert_eq!(agg.grand_total, 0);
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
            let reused = ScopeAggregate::build_for_tracks_reusing(&scan, sel);
            let fresh = ScopeAggregate::build_for_tracks(&p, sel);
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
    fn columnar_matches_trie_across_selections() {
        // Path-sort accumulation must reproduce the merge-trie graph and stats
        // byte-for-byte for every selection, built from one shared scan so both
        // paths rank labels identically and the graphs compare by handle.
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
            Box::new(|_: usize| true),
            Box::new(|i: usize| i % 2 == 0),
        ];
        for sel in &selections {
            let columnar = ScopeAggregate::build_for_tracks_columnar(&scan, sel);
            let cached = ScopeAggregate::build_for_tracks_columnar_cached(&scan, sel);
            let trie = ScopeAggregate::build_for_tracks_reusing(&scan, sel);
            assert_eq!(columnar.flame, trie.flame);
            assert_eq!(cached.flame, trie.flame);
            assert_eq!(
                resolved_rows(&columnar.flame, &columnar.stats),
                resolved_rows(&trie.flame, &trie.stats),
            );
            assert_eq!(
                resolved_rows(&cached.flame, &cached.stats),
                resolved_rows(&trie.flame, &trie.stats),
            );
            assert_eq!(columnar.grand_total, trie.grand_total);
            assert_eq!(cached.grand_total, trie.grand_total);
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
        // classes, so the sharded merge must reproduce the serial one.
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
        // One name under two mnemonics plus a mnemonic-less carrier: the
        // smallest-text resolution must not depend on which shard scans which
        // lane.
        tracks.push(track(
            50,
            LaneClass::Execution,
            vec![action_named("Compiling foo", "Javac", 5000, 60)],
        ));
        tracks.push(track(
            51,
            LaneClass::Execution,
            vec![slice("Compiling foo", "action processing", 5100, 30)],
        ));
        let p = profile(tracks);

        let serial = FlameScopes::build_with_workers(&p, 1, None, None);
        for workers in [2, 5, 16] {
            let sharded = FlameScopes::build_with_workers(&p, workers, None, None);
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
    fn reusing_a_scope_matches_full_build() {
        // Handing back an already-built default scope must reproduce a
        // from-scratch set: every other scope is unchanged and the reused one is
        // the very object passed in. Both possible initial scopes are covered.
        let p = full_profile();
        let full = FlameScopes::build(&p);
        for reused_scope in [Scope::All, Scope::CriticalPath] {
            let held = full.get(reused_scope).unwrap().clone();
            let reused = FlameScopes::build_reusing(&p, reused_scope, held);
            for scope in ALL_SCOPES {
                match (full.get(scope), reused.get(scope)) {
                    (Some(a), Some(b)) => {
                        assert_eq!(resolved(&a.flame), resolved(&b.flame), "{scope:?}");
                        assert_eq!(
                            resolved_rows(&a.flame, &a.stats),
                            resolved_rows(&b.flame, &b.stats),
                            "{scope:?}"
                        );
                        assert_eq!(a.default_order, b.default_order, "{scope:?}");
                        assert_eq!(a.grand_total, b.grand_total, "{scope:?}");
                    }
                    (None, None) => {}
                    _ => panic!("critical-path presence differs for {scope:?}"),
                }
            }
            assert!(Arc::ptr_eq(
                full.get(reused_scope).unwrap(),
                reused.get(reused_scope).unwrap()
            ));
        }
    }

    #[test]
    fn restricted_single_scope_matches_the_set() {
        // The single-scope path scans only the lanes its build walks, while the
        // set pass scans the whole profile; both must produce the same graphs,
        // rows, and default order. The fixture stresses the restricted scan's
        // coverage: the mnemonic-less "Compiling foo" carriers on the analysis
        // and repo-fetch lanes resolve their labels to mnemonics carried only
        // on execution lanes — lanes outside those scopes' scans.
        let tracks = vec![
            track(
                0,
                LaneClass::CriticalPath,
                vec![
                    slice("action 'Compiling foo'", "critical path component", 500, 100),
                    slice("Writing file out", "critical path component", 620, 10),
                ],
            ),
            track(
                1,
                LaneClass::Execution,
                vec![
                    action_named("Compiling foo", "Rustc", 0, 100),
                    slice("subprocess.run", "subprocess run", 0, 80),
                ],
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
            track(
                4,
                LaneClass::RepoFetch,
                vec![
                    slice("@rules_rs//:fetch", "generic", 0, 80),
                    slice("Compiling foo", "generic", 90, 5),
                ],
            ),
        ];
        let p = profile(tracks);
        let set = FlameScopes::build(&p);
        // Row keys in default display order, resolved: ranks differ between a
        // restricted and a full scan (each is dense over its own scanned
        // texts), so the order must be compared through the resolved keys.
        let order = |a: &ScopeAggregate| -> Vec<String> {
            a.default_order
                .iter()
                .map(|&i| a.flame.str(a.stats[i as usize].key).to_owned())
                .collect()
        };
        for scope in ALL_SCOPES {
            let single = ScopeAggregate::build(&p, scope);
            let agg = set.get(scope).expect("scope present");
            assert_eq!(resolved(&agg.flame), resolved(&single.flame), "{scope:?}");
            assert_eq!(
                resolved_rows(&agg.flame, &agg.stats),
                resolved_rows(&single.flame, &single.stats),
                "{scope:?}"
            );
            assert_eq!(order(agg), order(&single), "{scope:?}");
            assert_eq!(agg.grand_total, single.grand_total, "{scope:?}");
        }
        // The cross-lane resolution really happened: the analysis scope's rows
        // label the mnemonic-less carrier by the smallest foreign mnemonic.
        let analysis = ScopeAggregate::build(&p, Scope::Analysis);
        assert!(
            analysis
                .stats
                .iter()
                .any(|r| analysis.flame.str(r.key) == "Javac"),
            "the foreign-lane mnemonic must label the analysis carrier"
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
        let set = FlameScopes::build(&p);
        let agg = set.get(Scope::All).unwrap();
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
