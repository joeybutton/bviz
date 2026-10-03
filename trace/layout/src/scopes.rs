use std::sync::Arc;

use bviz_trace_profile::Profile;

use crate::flame::{
    FlameDirection, FlameGraph, FlameMode, FlameWeight, TrackFlamer, WallShareFlame, invert,
};
use crate::selection::TimeSelection;
use crate::stats::{StatRow, default_order, stats_from_durations, stats_from_flame};

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
    /// rows read the same whichever direction `flame` presents, and they sum
    /// span durations whichever [`FlameWeight`] it carries.
    pub stats: Vec<StatRow>,
    /// Indices into `stats` in the default display order (see
    /// [`default_order`]).
    pub default_order: Vec<u32>,
    /// Summed `self_us` over `stats` — the denominator of a row's share.
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
        ScopeAggregate::build_for_tracks_reusing(&TrackFlamer::new(profile), mode, window, selected)
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
        let (flame, stats) = match mode.weight {
            FlameWeight::Duration => {
                let flame = scan.build(mode.labels, window, selected);
                let stats = stats_from_flame(&flame);
                (flame, stats)
            }
            FlameWeight::WallShare => {
                let WallShareFlame { flame, duration_us } =
                    scan.build_wall_share(mode.labels, window, selected);
                let stats = stats_from_durations(&flame, &duration_us);
                (flame, stats)
            }
        };
        let default_order = default_order(&stats);
        let grand_total = stats.iter().map(|r| r.self_us).sum();
        // The rows are read off the top-down graph, so inverting it comes last.
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
    use crate::flame::{FlameLabels, FlameNode};
    use crate::selection::span_between;
    use crate::test_profile::{
        TestSlice, action, action_named, fetch_host, fetch_worker, profile, random_profile, slice,
        track, wait,
    };
    use bviz_trace_profile::LaneClass;
    use fastrand::Rng;

    fn child(graph: &FlameGraph, node: &FlameNode, name: &str) -> FlameNode {
        graph
            .children(node)
            .into_iter()
            .find(|c| graph.str(c.name) == name)
            .unwrap_or_else(|| panic!("no child {name}"))
    }

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
            .map(|r| (g.str(r.key).to_owned(), r.count, r.self_us, r.max_us))
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

    /// A selection of the given regions.
    fn window(regions: &[(i64, i64)]) -> TimeSelection {
        let mut sel = TimeSelection::default();
        for &(a, b) in regions {
            sel.insert(span_between(a, b).expect("a region of non-zero width"));
        }
        sel
    }

    /// Frames named by their own span, weighed by wall share.
    fn wall_share() -> FlameMode {
        FlameMode {
            labels: FlameLabels::Name,
            weight: FlameWeight::WallShare,
            ..FlameMode::default()
        }
    }

    /// The wall-share aggregate of every lane, over the whole timeline.
    fn wall_share_of(p: &Arc<Profile>) -> ScopeAggregate {
        ScopeAggregate::build_for_tracks(p, wall_share(), &TimeSelection::default(), |_| true)
    }

    /// Lanes that overlap, nest, park and pause, with no two sibling spans
    /// overlapping, so every frame's children fit inside it.
    fn sharing_profile() -> Arc<Profile> {
        profile(vec![
            track(
                1,
                LaneClass::Main,
                vec![wait(0, 150), slice("glue", "generic", 10, 20)],
            ),
            track(
                2,
                LaneClass::Execution,
                vec![
                    slice("a", "generic", 0, 70),
                    slice("a1", "generic", 5, 50),
                    slice("b", "generic", 80, 60),
                ],
            ),
            track(3, LaneClass::Execution, vec![slice("a", "generic", 30, 90)]),
            track(
                4,
                LaneClass::Gc,
                vec![slice("minor GC", "gc notification", 40, 10)],
            ),
        ])
    }

    /// A fetch restarted once, its worker's span open from inside the first
    /// host span to inside the second, beside a lane working throughout and a
    /// host with no worker.
    fn fetching_profile() -> Arc<Profile> {
        profile(vec![
            track(
                1,
                LaneClass::Analysis,
                vec![fetch_host("r", 0, 60), fetch_host("r", 70, 80)],
            ),
            track(
                2,
                LaneClass::RepoFetch,
                vec![
                    fetch_worker("r", 20, 100),
                    slice("download", "generic", 30, 40),
                ],
            ),
            track(3, LaneClass::Analysis, vec![fetch_host("r+s", 0, 150)]),
            track(
                4,
                LaneClass::Gc,
                vec![slice("minor GC", "gc notification", 40, 10)],
            ),
        ])
    }

    /// The profiles whose wall shares are checked to add up, each with the
    /// names of its frames.
    fn additive_profiles() -> [(Arc<Profile>, &'static [&'static str]); 2] {
        [
            (
                sharing_profile(),
                &[
                    "Parallel Evaluator evaluation",
                    "glue",
                    "a",
                    "a1",
                    "b",
                    "minor GC",
                ],
            ),
            (
                fetching_profile(),
                &["@@r", "r", "download", "@@r+s", "minor GC"],
            ),
        ]
    }

    #[test]
    fn a_fetch_host_frame_is_credited_only_what_its_worker_leaves_it() {
        let g = wall_share_of(&fetching_profile()).flame;
        // The restarted host halves with the other host from 0 to 20 and from
        // 120 to 150; its worker does from 20 to 120, less the pause.
        assert_eq!(frame_total(&g, "@@r"), 25);
        assert_eq!(frame_total(&g, "r"), 45);
        assert_eq!(frame_total(&g, "download"), 15);
        assert_eq!(frame_total(&g, "@@r+s"), 70);
        assert_eq!(frame_total(&g, "minor GC"), 10);
        assert_eq!(g.root().total_us, 150);
        // Weighed by duration, every span counts in full.
        let by_duration = FlameMode {
            weight: FlameWeight::Duration,
            ..wall_share()
        };
        let g = ScopeAggregate::build_for_tracks(
            &fetching_profile(),
            by_duration,
            &TimeSelection::default(),
            |_| true,
        )
        .flame;
        assert_eq!(frame_total(&g, "@@r"), 140);
        assert_eq!(frame_total(&g, "r"), 100);
    }

    #[test]
    fn wall_share_splits_the_time_two_lanes_overlap_and_totals_the_time_covered() {
        let p = profile(vec![
            track(1, LaneClass::Execution, vec![slice("a", "generic", 0, 100)]),
            track(
                2,
                LaneClass::Execution,
                vec![slice("b", "generic", 50, 100)],
            ),
        ]);
        let g = wall_share_of(&p).flame;
        assert_eq!(frame_total(&g, "a"), 75);
        assert_eq!(frame_total(&g, "b"), 75);
        // Some lane is open from 0 to 150, where durations sum to 200.
        assert_eq!(g.root().total_us, 150);
    }

    #[test]
    fn a_parking_span_is_credited_the_time_no_lane_works() {
        let p = profile(vec![
            track(1, LaneClass::Main, vec![wait(0, 300)]),
            track(
                2,
                LaneClass::Execution,
                vec![slice("compile", "generic", 100, 100)],
            ),
        ]);
        let g = wall_share_of(&p).flame;
        // Alone from 0 to 100 and from 200 to 300, the wait names the phase;
        // while the other lane works it is credited nothing.
        assert_eq!(frame_total(&g, "Parallel Evaluator evaluation"), 200);
        assert_eq!(frame_total(&g, "compile"), 100);
        assert_eq!(g.root().total_us, 300);
    }

    #[test]
    fn work_nested_in_a_parking_span_shares_with_the_other_lanes() {
        let p = profile(vec![
            track(
                1,
                LaneClass::Main,
                vec![wait(0, 300), slice("glue", "generic", 50, 50)],
            ),
            track(
                2,
                LaneClass::Execution,
                vec![slice("compile", "generic", 50, 100)],
            ),
        ]);
        let g = wall_share_of(&p).flame;
        let wait = child(&g, &g.root(), "Parallel Evaluator evaluation");
        // 50 alone, 25 as its child shares with the other lane, nothing while
        // that lane works on, then 150 alone.
        assert_eq!(wait.total_us, 225);
        assert_eq!(child(&g, &wait, "glue").total_us, 25);
        assert_eq!(wait.self_us, 200);
        assert_eq!(frame_total(&g, "compile"), 75);
        assert_eq!(g.root().total_us, 300);
    }

    #[test]
    fn a_collector_pause_is_credited_its_whole_extent() {
        let p = profile(vec![
            track(
                1,
                LaneClass::Gc,
                vec![slice("minor GC", "gc notification", 40, 20)],
            ),
            track(2, LaneClass::Execution, vec![slice("x", "generic", 0, 100)]),
            track(3, LaneClass::Execution, vec![slice("x", "generic", 0, 100)]),
        ]);
        let g = wall_share_of(&p).flame;
        assert_eq!(frame_total(&g, "minor GC"), 20);
        // The two lanes split the 80 left over between them.
        let x = child(&g, &g.root(), "x");
        assert_eq!((x.total_us, x.count), (80, 2));
        assert_eq!(g.root().total_us, 100);
    }

    #[test]
    fn the_critical_path_lane_shares_nothing_and_weighs_its_own_duration() {
        let p = profile(vec![
            track(
                0,
                LaneClass::CriticalPath,
                vec![slice(
                    "action 'Compiling foo'",
                    "critical path component",
                    0,
                    100,
                )],
            ),
            track(
                1,
                LaneClass::Execution,
                vec![slice("Compiling foo", "generic", 0, 100)],
            ),
        ]);
        let t = p.threads();
        let scan = TrackFlamer::new(&p);
        let built = |mode: FlameMode, critical_path: bool| {
            ScopeAggregate::build_for_tracks_reusing(&scan, mode, &TimeSelection::default(), |i| {
                (t[i].lane_class == LaneClass::CriticalPath) == critical_path
            })
            .flame
        };
        // The replay does not halve the lane it replays.
        assert_eq!(built(wall_share(), false).root().total_us, 100);
        // Shown on its own, the lane reads as it does weighed by duration.
        let by_duration = FlameMode {
            weight: FlameWeight::Duration,
            ..wall_share()
        };
        assert_eq!(
            resolved(&built(wall_share(), true)),
            resolved(&built(by_duration, true))
        );
    }

    #[test]
    fn wall_share_of_abutting_windows_sums_to_that_of_the_window_they_make_up() {
        for (p, names) in additive_profiles() {
            let scan = TrackFlamer::new(&p);
            let within = |regions: &[(i64, i64)]| {
                ScopeAggregate::build_for_tracks_reusing(
                    &scan,
                    wall_share(),
                    &window(regions),
                    |_| true,
                )
                .flame
            };
            let whole = wall_share_of(&p).flame;
            // Each profile ends at 150, so this window holds all of it.
            assert_eq!(resolved(&within(&[(0, 150)])), resolved(&whole));
            for cut in [1, 37, 45, 60, 85, 149] {
                let (before, after) = (within(&[(0, cut)]), within(&[(cut, 150)]));
                for name in names {
                    assert_eq!(
                        frame_total(&before, name) + frame_total(&after, name),
                        frame_total(&whole, name),
                        "{name} cut at {cut}"
                    );
                }
                assert_eq!(
                    before.root().total_us + after.root().total_us,
                    whole.root().total_us
                );
            }
            // Regions apart from each other weigh what each weighs alone.
            let (early, late) = (within(&[(10, 45)]), within(&[(70, 120)]));
            let both = within(&[(10, 45), (70, 120)]);
            for name in names {
                assert_eq!(
                    frame_total(&both, name),
                    frame_total(&early, name) + frame_total(&late, name),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn wall_shares_of_disjoint_lane_sets_sum_to_their_unions() {
        for (p, names) in additive_profiles() {
            let scan = TrackFlamer::new(&p);
            let of = |lanes: &[usize]| {
                ScopeAggregate::build_for_tracks_reusing(
                    &scan,
                    wall_share(),
                    &TimeSelection::default(),
                    |i| lanes.contains(&i),
                )
                .flame
            };
            let (some, rest, all) = (of(&[0, 2]), of(&[1, 3]), of(&[0, 1, 2, 3]));
            // A lane's share does not depend on which lanes are shown beside it.
            for name in names {
                assert_eq!(
                    frame_total(&some, name) + frame_total(&rest, name),
                    frame_total(&all, name),
                    "{name}"
                );
            }
            assert_eq!(
                some.root().total_us + rest.root().total_us,
                all.root().total_us
            );
            // Some lane is open from 0 to 150 throughout, and every change of
            // who shares falls where the shares are whole.
            assert_eq!(all.root().total_us, 150);
        }
    }

    #[test]
    fn a_frames_wall_share_is_its_own_plus_its_childrens() {
        // Three lanes share every microsecond, so no span's share is whole, and
        // the two children abut where the thirds do not divide.
        let lane = || {
            vec![
                slice("p", "generic", 0, 100),
                slice("c1", "generic", 0, 37),
                slice("c2", "generic", 37, 63),
            ]
        };
        let p = profile(vec![
            track(1, LaneClass::Execution, lane()),
            track(2, LaneClass::Execution, lane()),
            track(3, LaneClass::Execution, lane()),
        ]);
        let g = wall_share_of(&p).flame;
        let parent = child(&g, &g.root(), "p");
        assert_eq!(parent.total_us, 99);
        assert_eq!(child(&g, &parent, "c1").total_us, 36);
        assert_eq!(child(&g, &parent, "c2").total_us, 63);
        assert_eq!(parent.self_us, 0);

        let g = wall_share_of(&sharing_profile()).flame;
        for i in 0..g.len() {
            let nested: i64 = (0..g.child_count(i))
                .map(|k| g.total_us(g.child_id(i, k)))
                .sum();
            assert_eq!(g.total_us(i), g.self_us(i) + nested, "{}", g.str(g.name(i)));
        }
    }

    #[test]
    fn a_zero_duration_span_keeps_its_frame_at_no_wall_share() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                slice("p", "generic", 0, 100),
                slice("tick", "generic", 50, 0),
            ],
        )]);
        let g = wall_share_of(&p).flame;
        let parent = child(&g, &g.root(), "p");
        let tick = child(&g, &parent, "tick");
        assert_eq!((tick.total_us, tick.count), (0, 1));
        assert_eq!(parent.self_us, 100);
    }

    #[test]
    fn detail_frames_carry_the_wall_share_of_their_frames_self_time() {
        // The action shares every microsecond with the other lane.
        let p = profile(vec![
            track(
                1,
                LaneClass::Execution,
                vec![
                    action_named("Compiling //x:y", "Rustc", 0, 100),
                    slice("subprocess.run", "subprocess run", 0, 80),
                ],
            ),
            track(
                2,
                LaneClass::Execution,
                vec![slice("other", "generic", 0, 100)],
            ),
        ]);
        let mode = FlameMode {
            weight: FlameWeight::WallShare,
            ..FlameMode::default()
        };
        let g =
            ScopeAggregate::build_for_tracks(&p, mode, &TimeSelection::default(), |_| true).flame;
        let rustc = child(&g, &g.root(), "Rustc");
        let sub = child(&g, &rustc, "subprocess.run");
        assert_eq!((rustc.total_us, sub.total_us), (50, 40));
        assert_eq!(child(&g, &rustc, "Compiling //x:y").total_us, 10);
        assert_eq!(child(&g, &sub, "Compiling //x:y").total_us, 40);
    }

    #[test]
    fn the_bottom_up_graph_carries_wall_share_self_time() {
        let p = sharing_profile();
        let top_down = wall_share_of(&p).flame;
        let bottom_up = ScopeAggregate::build_for_tracks(
            &p,
            FlameMode {
                direction: FlameDirection::BottomUp,
                ..wall_share()
            },
            &TimeSelection::default(),
            |_| true,
        )
        .flame;
        // No frame's children overrun it, so every microsecond of the root is
        // some frame's self time and the two graphs total the same.
        assert_eq!(bottom_up.root().total_us, top_down.root().total_us);
        let a_self: i64 = (0..top_down.len())
            .filter(|&i| top_down.str(top_down.name(i)) == "a")
            .map(|i| top_down.self_us(i))
            .sum();
        assert_eq!(child(&bottom_up, &bottom_up.root(), "a").total_us, a_self);
    }

    #[test]
    fn stats_read_the_same_whatever_the_flame_is_weighed_by() {
        for p in [full_profile(), sharing_profile(), fetching_profile()] {
            let t = p.threads();
            let scan = TrackFlamer::new(&p);
            for labels in [FlameLabels::Kind, FlameLabels::Name] {
                for direction in [FlameDirection::TopDown, FlameDirection::BottomUp] {
                    for regions in [&[][..], &[(5, 60)], &[(0, 20), (40, 90)]] {
                        for critical_path in [false, true] {
                            let built = |weight| {
                                ScopeAggregate::build_for_tracks_reusing(
                                    &scan,
                                    FlameMode {
                                        direction,
                                        labels,
                                        weight,
                                    },
                                    &window(regions),
                                    |i| critical_path || t[i].lane_class != LaneClass::CriticalPath,
                                )
                            };
                            let by_duration = built(FlameWeight::Duration);
                            let by_share = built(FlameWeight::WallShare);
                            assert_eq!(by_share.stats, by_duration.stats);
                            assert_eq!(by_share.default_order, by_duration.default_order);
                            assert_eq!(by_share.grand_total, by_duration.grand_total);
                            // The flame itself does differ: lanes overlap in
                            // both profiles, so shares fall short of durations.
                            assert!(
                                by_share.flame.root().total_us < by_duration.flame.root().total_us
                            );
                        }
                    }
                }
            }
        }
    }

    /// A span holding a child that runs one microsecond past it.
    fn overshooting_lane() -> Vec<TestSlice> {
        vec![slice("p", "generic", 0, 100), slice("c", "generic", 60, 41)]
    }

    /// A span holding two children that overlap from 30 to 70 and together
    /// outlast it.
    fn overlapping_lane() -> Vec<TestSlice> {
        vec![
            slice("p", "generic", 0, 100),
            slice("s1", "generic", 10, 60),
            slice("s2", "generic", 30, 65),
        ]
    }

    /// `lane` beside a lane working for `other_us`, so every microsecond of
    /// both is halved.
    fn halved(lane: Vec<TestSlice>, other_us: i64) -> Arc<Profile> {
        profile(vec![
            track(1, LaneClass::Execution, lane),
            track(
                2,
                LaneClass::Execution,
                vec![slice("other", "generic", 0, other_us)],
            ),
        ])
    }

    #[test]
    fn a_child_overshooting_its_parent_weighs_its_whole_share() {
        let g = wall_share_of(&halved(overshooting_lane(), 101)).flame;
        let parent = child(&g, &g.root(), "p");
        assert_eq!(parent.total_us, 50);
        // Half of 41, the lane's clock rounding up at 101.
        assert_eq!(child(&g, &parent, "c").total_us, 21);
        assert_eq!(parent.self_us, 29);
        assert_eq!(frame_total(&g, "other"), 51);
    }

    #[test]
    fn overlapping_siblings_each_weigh_their_own_share() {
        let g = wall_share_of(&halved(overlapping_lane(), 100)).flame;
        let parent = child(&g, &g.root(), "p");
        assert_eq!(parent.total_us, 50);
        assert_eq!(child(&g, &parent, "s1").total_us, 30);
        // Half of 65, the lane's clock rounding up at 95.
        assert_eq!(child(&g, &parent, "s2").total_us, 33);
        assert_eq!(parent.self_us, 0);
        // The lane is open once however its spans overlap, so the root is the
        // time that elapsed.
        assert_eq!(g.root().total_us, 100);
    }

    #[test]
    fn a_lane_sharing_with_no_other_weighs_as_its_durations_do() {
        for lane in [overshooting_lane(), overlapping_lane()] {
            let p = profile(vec![track(1, LaneClass::Execution, lane)]);
            let by_duration = ScopeAggregate::build_for_tracks(
                &p,
                FlameMode {
                    weight: FlameWeight::Duration,
                    ..wall_share()
                },
                &TimeSelection::default(),
                |_| true,
            );
            assert_eq!(
                resolved(&wall_share_of(&p).flame),
                resolved(&by_duration.flame)
            );
        }
    }

    #[test]
    fn mnemonic_detail_frames_carry_the_wall_share_a_window_holds() {
        // The action shares every microsecond with the other lane.
        let p = profile(vec![
            track(
                1,
                LaneClass::Execution,
                vec![
                    action_named("Compiling //x:y", "Rustc", 0, 100),
                    slice("subprocess.run", "subprocess run", 0, 80),
                ],
            ),
            track(
                2,
                LaneClass::Execution,
                vec![slice("other", "generic", 0, 100)],
            ),
        ]);
        let scan = TrackFlamer::new(&p);
        let mode = FlameMode {
            weight: FlameWeight::WallShare,
            ..FlameMode::default()
        };
        // (Rustc, subprocess.run, Rustc's own detail, subprocess.run's detail)
        let within = |regions: &[(i64, i64)]| {
            let g =
                ScopeAggregate::build_for_tracks_reusing(&scan, mode, &window(regions), |_| true)
                    .flame;
            let rustc = child(&g, &g.root(), "Rustc");
            let sub = child(&g, &rustc, "subprocess.run");
            (
                rustc.total_us,
                sub.total_us,
                child(&g, &rustc, "Compiling //x:y").total_us,
                child(&g, &sub, "Compiling //x:y").total_us,
            )
        };
        assert_eq!(within(&[(20, 90)]), (35, 30, 5, 30));
        assert_eq!(within(&[(0, 10), (50, 90)]), (25, 20, 5, 20));
    }

    /// Asserts each `(label, spans)` of `expected` to be the spans the frames
    /// labelled so merge top-down, the count of the label's bottom-up root, and
    /// the count of its stats row, under either weight.
    fn assert_span_counts(p: &Arc<Profile>, expected: &[(&str, u32)]) {
        let scan = TrackFlamer::new(p);
        for weight in [FlameWeight::Duration, FlameWeight::WallShare] {
            let built = |direction| {
                let mode = FlameMode {
                    direction,
                    weight,
                    ..FlameMode::default()
                };
                ScopeAggregate::build_for_tracks_reusing(
                    &scan,
                    mode,
                    &TimeSelection::default(),
                    |_| true,
                )
            };
            let top_down = built(FlameDirection::TopDown);
            let bottom_up = built(FlameDirection::BottomUp);
            for &(label, spans) in expected {
                let g = &top_down.flame;
                let merged: u32 = span_frames(g)
                    .filter(|&i| g.str(g.name(i)) == label)
                    .map(|i| g.count(i))
                    .sum();
                assert_eq!(merged, spans, "{label} top-down, {weight:?}");
                let inverted = &bottom_up.flame;
                assert_eq!(
                    child(inverted, &inverted.root(), label).count,
                    spans,
                    "{label} bottom-up, {weight:?}"
                );
                for agg in [&top_down, &bottom_up] {
                    let row = agg
                        .stats
                        .iter()
                        .find(|row| agg.flame.str(row.key) == label)
                        .unwrap_or_else(|| panic!("no row {label}"));
                    assert_eq!(row.count, spans, "{label} stats, {weight:?}");
                }
            }
        }
    }

    #[test]
    fn a_kind_frame_counts_each_of_its_spans_once_bottom_up() {
        // The nameless package's time stays on the kind's frame beside the
        // detail frames of the named ones, and package `c` is all nested time,
        // so no detail frame stands for it.
        let creation = |package, start, dur| slice(package, "package creation", start, dur);
        let p = profile(vec![track(
            1,
            LaneClass::Analysis,
            vec![
                creation("", 0, 100),
                creation("a", 100, 50),
                creation("b", 150, 50),
                creation("c", 200, 60),
                slice("load", "generic", 200, 60),
                slice("@@r", "Fetching repository", 300, 100),
                creation("", 310, 30),
            ],
        )]);
        assert_span_counts(
            &p,
            &[
                ("package creation", 5),
                ("load", 1),
                ("Fetching repository", 1),
            ],
        );
    }

    #[test]
    fn an_action_frame_counts_each_of_its_spans_once_bottom_up() {
        // The nameless action's time stays on the mnemonic's frame beside the
        // detail frame of the named one.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                action_named("", "Rustc", 0, 40),
                action_named("Compiling //x:y", "Rustc", 50, 100),
                slice("subprocess.run", "subprocess run", 60, 80),
            ],
        )]);
        assert_span_counts(&p, &[("Rustc", 2), ("subprocess.run", 1)]);
    }

    #[test]
    fn a_kind_frame_weighs_its_instances_by_wall_share_and_counts_them_by_duration() {
        // Two repositories fetched side by side, each lane with half of every
        // microsecond, and a third fetched alone afterwards.
        let fetch = |repository, start| slice(repository, "Fetching repository", start, 100);
        let p = profile(vec![
            track(
                1,
                LaneClass::Analysis,
                vec![fetch("@@a", 0), fetch("@@c", 100)],
            ),
            track(2, LaneClass::Analysis, vec![fetch("@@b", 0)]),
        ]);
        let scan = TrackFlamer::new(&p);
        let built = |direction, regions: &[(i64, i64)]| {
            let mode = FlameMode {
                direction,
                weight: FlameWeight::WallShare,
                ..FlameMode::default()
            };
            ScopeAggregate::build_for_tracks_reusing(&scan, mode, &window(regions), |_| true)
        };
        let instances = |agg: &ScopeAggregate| {
            let g = &agg.flame;
            let fetching = child(g, &g.root(), "Fetching repository");
            let mut kids: Vec<(String, i64)> = g
                .children(&fetching)
                .iter()
                .map(|c| (g.str(c.name).to_owned(), c.total_us))
                .collect();
            kids.sort();
            (g.children(&g.root()).len(), fetching.total_us, kids)
        };
        let whole = [
            ("@@a".to_owned(), 50),
            ("@@b".to_owned(), 50),
            ("@@c".to_owned(), 100),
        ];
        for direction in [FlameDirection::TopDown, FlameDirection::BottomUp] {
            let agg = built(direction, &[]);
            assert_eq!(instances(&agg), (1, 200, whole.to_vec()), "{direction:?}");
            assert_eq!(
                resolved_rows(&agg.flame, &agg.stats),
                [("Fetching repository".to_owned(), 3, 300, 100)],
                "{direction:?}"
            );
            let clipped = built(direction, &[(60, 120)]);
            assert_eq!(
                instances(&clipped),
                (
                    1,
                    60,
                    vec![
                        ("@@a".to_owned(), 20),
                        ("@@b".to_owned(), 20),
                        ("@@c".to_owned(), 20)
                    ]
                ),
                "{direction:?}"
            );
            assert_eq!(
                resolved_rows(&clipped.flame, &clipped.stats),
                [("Fetching repository".to_owned(), 3, 100, 40)],
                "{direction:?}"
            );
        }
    }

    #[test]
    fn a_scan_builds_the_same_wall_share_graph_whichever_weight_it_built_first() {
        /// How many random profiles are built beside the fixed ones.
        const RANDOM_PROFILES: usize = 16;
        /// Every random span begins before this.
        const HORIZON_US: i64 = 80;
        const SEED: u64 = 0xDEAD_BEEF_CAFE_F00D;

        let mut rng = Rng::with_seed(SEED);
        let mut profiles = vec![
            full_profile(),
            sharing_profile(),
            halved(overshooting_lane(), 101),
            halved(overlapping_lane(), 100),
        ];
        profiles.extend((0..RANDOM_PROFILES).map(|_| random_profile(&mut rng, HORIZON_US)));
        for (case, p) in profiles.iter().enumerate() {
            let duration_first = TrackFlamer::new(p);
            let wall_first = TrackFlamer::new(p);
            let from = rng.i64(0..60);
            let to = from + 1 + rng.i64(0..40);
            let windows = [
                window(&[]),
                window(&[(from, to)]),
                window(&[(from, to), (to + 5, to + 30)]),
            ];
            for labels in [FlameLabels::Kind, FlameLabels::Name] {
                for win in &windows {
                    let built = |scan: &TrackFlamer, weight| {
                        let mode = FlameMode {
                            labels,
                            weight,
                            ..FlameMode::default()
                        };
                        ScopeAggregate::build_for_tracks_reusing(scan, mode, win, |_| true)
                    };
                    let by_duration = built(&duration_first, FlameWeight::Duration);
                    let filled = built(&duration_first, FlameWeight::WallShare);
                    let at_once = built(&wall_first, FlameWeight::WallShare);
                    assert_eq!(
                        resolved(&filled.flame),
                        resolved(&at_once.flame),
                        "case {case}"
                    );
                    assert_eq!(filled.stats, by_duration.stats, "case {case}");
                    assert_eq!(at_once.stats, by_duration.stats, "case {case}");
                }
            }
        }
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
        let rows_total: i64 = agg.stats.iter().map(|r| r.self_us).sum();
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
            let mode = FlameMode {
                direction,
                ..FlameMode::default()
            };
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
            Box::new(|i: usize| i.is_multiple_of(2)),
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
        window.insert(span_between(0, 100).expect("a region of non-zero width"));
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
        // All rows tie on self time, so the default order is pure key order:
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
    fn default_order_is_self_desc_then_key_text() {
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
                    (-r.self_us, agg.flame.str(r.key))
                })
                .collect();
            assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{keys:?}");
            assert_eq!(
                agg.grand_total,
                agg.stats.iter().map(|r| r.self_us).sum::<i64>()
            );
        }
    }
}
