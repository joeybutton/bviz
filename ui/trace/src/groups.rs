use crate::track_order::{TrackSort, family_runs, sort_tracks_by_name};
use crate::{GROUP_BAND_HEIGHT, METRICS, TOP_N_PER_GROUP};
use bviz_trace_layout::{Lane, LaneLayout};
use bviz_trace_profile::{LaneClass, Profile};

/// Lane classes in the order their groups stack in the flame chart: the critical
/// path and main thread first, then the build phases (loading, analysis,
/// execution), then gc and the leftovers.
pub(crate) const GROUP_ORDER: [LaneClass; 7] = [
    LaneClass::CriticalPath,
    LaneClass::Main,
    LaneClass::RepoFetch,
    LaneClass::Analysis,
    LaneClass::Execution,
    LaneClass::Gc,
    LaneClass::Other,
];

/// A collapsible group header in the flame chart, one per non-empty lane class,
/// placed at `top` above its group's lanes. `shown`/`total` count the group's
/// lanes drawn versus visible; a group with more than [`TOP_N_PER_GROUP`] visible
/// lanes is `capped`, showing only its busiest [`TOP_N_PER_GROUP`] until `show_all`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GroupBand {
    pub(crate) group: usize,
    pub(crate) class: LaneClass,
    pub(crate) top: f32,
    pub(crate) collapsed: bool,
    pub(crate) shown: usize,
    pub(crate) total: usize,
    pub(crate) capped: bool,
    pub(crate) show_all: bool,
}

/// A pool's label row, placed at `top` above the lanes it heads and under the
/// band of the group it belongs to. `shown`/`total` count the pool's lanes drawn
/// versus visible, so a pool the seat distribution left without a seat still
/// reports its size.
#[derive(Debug, Clone)]
pub(crate) struct FamilyBand {
    pub(crate) name: String,
    pub(crate) top: f32,
    pub(crate) shown: usize,
    pub(crate) total: usize,
}

/// The header rows overlaid on a grouped lane layout, and every row of the chart
/// they and its lanes stack into, in the order they stack.
#[derive(Debug, Clone, Default)]
pub(crate) struct GroupChrome {
    pub(crate) bands: Vec<GroupBand>,
    pub(crate) families: Vec<FamilyBand>,
    rows: Vec<ChartRow>,
}

/// A row of the flame chart's vertical stack as the chart currently draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChartRow {
    pub(crate) top: f32,
    pub(crate) kind: RowKind,
}

/// Which row a [`ChartRow`] is, and what it stands for in the document: a band's
/// lane-class group, as an index into [`GROUP_ORDER`] — what the collapse and
/// show-all toggles key on — a pool heading part of a group, as its place among
/// the pools that group's tracks fall into (fixed for a profile, so it holds
/// while the pool's members are hidden), or a lane's track (an index into
/// `Profile::threads`, so it survives a restack) under the group it hangs from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RowKind {
    Band { group: usize },
    Family { group: usize, family: usize },
    Lane { group: usize, track: usize },
}

impl RowKind {
    fn group(self) -> usize {
        match self {
            RowKind::Band { group }
            | RowKind::Family { group, .. }
            | RowKind::Lane { group, .. } => group,
        }
    }

    /// Whether both name the same row of the document, whatever place the chart
    /// currently stacks it in.
    fn same_row(self, other: Self) -> bool {
        match (self, other) {
            (RowKind::Band { group: a }, RowKind::Band { group: b }) => a == b,
            (
                RowKind::Family {
                    group: g0,
                    family: f0,
                },
                RowKind::Family {
                    group: g1,
                    family: f1,
                },
            ) => (g0, f0) == (g1, f1),
            (
                RowKind::Lane {
                    group: g0,
                    track: t0,
                    ..
                },
                RowKind::Lane {
                    group: g1,
                    track: t1,
                    ..
                },
            ) => (g0, t0) == (g1, t1),
            _ => false,
        }
    }
}

/// The flame chart's rows in `top` order.
pub(crate) fn chart_rows(chrome: &GroupChrome) -> impl Iterator<Item = ChartRow> + '_ {
    chrome.rows.iter().copied()
}

/// Where the flame chart's viewport top edge sits, as a position the rows carry
/// with them as the chart restacks them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RowAnchor {
    /// The top of the chart.
    Start,
    /// `below_top` pixels below `row`'s top edge, which past the end of the
    /// chart runs beyond the row itself, `index` being the row's place among
    /// the rows its group stacks, counting the band as 0. That place is the
    /// order the chart emits a group's rows in, which every track order shares.
    Row {
        row: RowKind,
        index: usize,
        below_top: f32,
    },
}

/// The place each row holds among the rows its group stacks, alongside the row.
fn indexed(rows: impl Iterator<Item = ChartRow>) -> impl Iterator<Item = (usize, ChartRow)> {
    let mut index = 0;
    rows.map(move |row| {
        index = match row.kind {
            RowKind::Band { .. } => 0,
            RowKind::Family { .. } | RowKind::Lane { .. } => index + 1,
        };
        (index, row)
    })
}

/// The anchor for `y` pixels down the chart. A `y` past the last row anchors to
/// that row, so the position returns once the chart grows back that far.
pub(crate) fn anchor_at(rows: impl Iterator<Item = ChartRow>, y: f32) -> RowAnchor {
    indexed(rows)
        .take_while(|(_, row)| row.top <= y)
        .last()
        .map_or(RowAnchor::Start, |(index, row)| RowAnchor::Row {
            row: row.kind,
            index,
            below_top: y - row.top,
        })
}

/// How many pixels down the chart `anchor` sits. A row the chart no longer draws
/// resolves to the last row drawn at or above the place it held in its group,
/// else the last row of the groups before it, and the start when nothing
/// precedes it.
pub(crate) fn anchor_offset(rows: impl Iterator<Item = ChartRow>, anchor: RowAnchor) -> f32 {
    let RowAnchor::Row {
        row,
        index,
        below_top,
    } = anchor
    else {
        return 0.0;
    };
    let group = row.group();
    let mut fallback = 0.0;
    for (drawn_index, drawn) in indexed(rows) {
        if drawn.kind.same_row(row) {
            return drawn.top + below_top;
        }
        // A track keeps its group but not its place in it, so the anchored row
        // can be anywhere among its group's.
        if drawn.kind.group() > group {
            break;
        }
        if (drawn.kind.group(), drawn_index) <= (group, index) {
            fallback = drawn.top;
        }
    }
    fallback
}

/// What a press on a group header band hit: its gutter half (toggles the group's
/// collapse) or, for a capped expanded group, its time half (toggles whether the
/// group shows every lane or only its busiest).
#[derive(Debug, Clone, Copy)]
pub(crate) enum GroupHit {
    Collapse(usize),
    ShowAll(usize),
}

/// The pools a lane class's tracks fall into, in the order the group stacks
/// them, each with the tracks it holds. Every track of the class counts, hidden
/// or not, so a pool keeps its place in the group while its members come and go.
fn class_pools<'a>(profile: &'a Profile, class: LaneClass) -> Vec<(&'a str, Vec<usize>)> {
    let threads = profile.threads();
    let mut tracks: Vec<usize> = (0..threads.len())
        .filter(|&t| threads[t].lane_class == class)
        .collect();
    sort_tracks_by_name(profile, threads, &mut tracks);
    let runs = family_runs(profile, threads, &tracks);
    let mut pools = Vec::with_capacity(runs.len());
    let mut start = 0;
    for (family, members) in runs {
        pools.push((family, tracks[start..start + members].to_vec()));
        start += members;
    }
    pools
}

/// A run of a group's drawn lanes stacked together.
struct PoolRun<'a> {
    /// The pool heading the run — its place among the group's pools, its name,
    /// and how many of its tracks are visible — where anything heads it.
    header: Option<(usize, &'a str, usize)>,
    lanes: &'a [usize],
}

/// The runs `tracks` — a group's drawn lanes, in `sort`'s order — falls into.
/// The name order gathers each pool into a run of its own, headed once the group
/// holds more than one pool; the weight order interleaves the pools, so nothing
/// heads its lanes and they form a single run.
fn pool_runs<'a>(
    sort: TrackSort,
    pools: &'a [(&'a str, Vec<usize>)],
    tracks: &'a [usize],
    shows: impl Fn(usize) -> bool,
) -> Vec<PoolRun<'a>> {
    match sort {
        TrackSort::Weight => vec![PoolRun {
            header: None,
            lanes: tracks,
        }],
        TrackSort::Name => {
            let occupied: Vec<(usize, &str, usize)> = pools
                .iter()
                .enumerate()
                .map(|(family, (name, members))| {
                    (family, *name, members.iter().filter(|&&t| shows(t)).count())
                })
                .filter(|&(_, _, visible_members)| visible_members > 0)
                .collect();
            let headed = occupied.len() > 1;
            let mut runs = Vec::with_capacity(occupied.len());
            let mut seated = tracks;
            for (family, name, visible_members) in occupied {
                let in_pool = pools[family].1.as_slice();
                let seats = seated.iter().take_while(|t| in_pool.contains(t)).count();
                let (lanes, rest) = seated.split_at(seats);
                seated = rest;
                runs.push(PoolRun {
                    header: headed.then_some((family, name, visible_members)),
                    lanes,
                });
            }
            runs
        }
    }
}

/// The viewer-held state a grouped lane layout is stacked under.
pub(crate) struct GroupLayoutState<'a> {
    /// Which tracks the chart draws, indexed by `Profile::threads`; a track past
    /// the end is drawn.
    pub(crate) visible: &'a [bool],
    pub(crate) collapsed: &'a [bool; GROUP_ORDER.len()],
    /// Which groups draw every lane rather than a sample of [`TOP_N_PER_GROUP`].
    pub(crate) show_all: &'a [bool; GROUP_ORDER.len()],
    pub(crate) sort: TrackSort,
}

/// The lane layout the flame chart draws: visible tracks sorted into lane-class
/// groups, each group behind a header band and ordered by [`GroupLayoutState`]'s
/// sort, so a collapsed group folds its lanes away and a capped one keeps its
/// sample in that same order. A pool the order gathers into a run of its own is
/// headed by a [`FamilyBand`], which a group of one pool has no use for.
///
/// Returns the restacked [`LaneLayout`] (its `lanes` tops leave room for the
/// bands) and the [`GroupChrome`] painted over it.
pub(crate) fn build_grouped_layout(
    profile: &Profile,
    full: &LaneLayout,
    state: &GroupLayoutState,
) -> (LaneLayout, GroupChrome) {
    let threads = profile.threads();
    let mut lanes = Vec::with_capacity(threads.len());
    let mut chrome = GroupChrome {
        bands: Vec::with_capacity(GROUP_ORDER.len()),
        families: Vec::new(),
        rows: Vec::with_capacity(threads.len() + GROUP_ORDER.len()),
    };
    let mut top = 0.0;
    for (group, &class) in GROUP_ORDER.iter().enumerate() {
        let shows = |t: usize| state.visible.get(t).copied().unwrap_or(true);
        let pools = class_pools(profile, class);
        let mut tracks: Vec<usize> = pools
            .iter()
            .flat_map(|(_, members)| members.iter().copied().filter(|&t| shows(t)))
            .collect();
        if tracks.is_empty() {
            continue;
        }
        let total = tracks.len();
        let band_top = top;
        top += GROUP_BAND_HEIGHT + METRICS.lane_gap;
        chrome.rows.push(ChartRow {
            top: band_top,
            kind: RowKind::Band { group },
        });
        let is_collapsed = state.collapsed[group];
        let capped = total > TOP_N_PER_GROUP;
        let mut shown = 0;
        if !is_collapsed {
            state.sort.order(profile, threads, &mut tracks);
            if !state.show_all[group] {
                state
                    .sort
                    .sample(profile, threads, &mut tracks, TOP_N_PER_GROUP);
            }
            shown = tracks.len();
            for run in pool_runs(state.sort, &pools, &tracks, shows) {
                if let Some((family, name, visible_members)) = run.header {
                    let kind = RowKind::Family { group, family };
                    chrome.rows.push(ChartRow { top, kind });
                    chrome.families.push(FamilyBand {
                        name: name.to_owned(),
                        top,
                        shown: run.lanes.len(),
                        total: visible_members,
                    });
                    top += GROUP_BAND_HEIGHT + METRICS.lane_gap;
                }
                for &t in run.lanes {
                    let lane = full.lanes[t];
                    chrome.rows.push(ChartRow {
                        top,
                        kind: RowKind::Lane { group, track: t },
                    });
                    lanes.push(Lane { top, ..lane });
                    top += lane.height + METRICS.lane_gap;
                }
            }
        }
        chrome.bands.push(GroupBand {
            group,
            class,
            top: band_top,
            collapsed: is_collapsed,
            shown,
            total,
            capped,
            show_all: state.show_all[group],
        });
    }
    let layout = LaneLayout {
        lanes,
        counters: full.counters.clone(),
        // Each row leaves a trailing gap, so the accumulator sits one gap below
        // the last row's bottom edge, where the stack ends.
        total_height: (top - METRICS.lane_gap).max(0.0),
    };
    (layout, chrome)
}

/// The initial collapse state: the loading group starts collapsed, the rest open.
pub(crate) fn default_collapsed_groups() -> [bool; GROUP_ORDER.len()] {
    let mut collapsed = [false; GROUP_ORDER.len()];
    if let Some(i) = GROUP_ORDER.iter().position(|&g| g == LaneClass::RepoFetch) {
        collapsed[i] = true;
    }
    collapsed
}

/// The readable name of a lane-class group's header band.
pub(crate) fn group_label(class: LaneClass) -> &'static str {
    match class {
        LaneClass::CriticalPath => "Critical path",
        LaneClass::Main => "Main",
        LaneClass::RepoFetch => "Loading",
        LaneClass::Analysis => "Analysis",
        LaneClass::Execution => "Execution",
        LaneClass::Gc => "Garbage collection",
        LaneClass::Other => "Other",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::flame_total_height;
    use crate::test_profile::profile_of;
    use crate::{LINE_SCROLL_PX, drawn_scroll, scroll_request};
    use bviz_trace_layout::lay_out_lanes;
    use bviz_trace_profile::track_family;

    /// Stacks one band per group, each followed by its `(track, height)` lanes,
    /// in the geometry the flame chart draws. Every group holds a single pool, so
    /// none carries a pool header. A group given no lanes stands for a collapsed
    /// one; a group with no tracks at all, which [`build_grouped_layout`] drops
    /// from the stack, cannot be expressed here.
    fn stack(groups: &[&[(usize, f32)]]) -> (LaneLayout, GroupChrome) {
        let mut lanes = Vec::new();
        let mut bands = Vec::new();
        let mut rows = Vec::new();
        let mut top = 0.0;
        for (group, group_lanes) in groups.iter().enumerate() {
            let band_top = top;
            top += GROUP_BAND_HEIGHT + METRICS.lane_gap;
            rows.push(ChartRow {
                top: band_top,
                kind: RowKind::Band { group },
            });
            for &(track, height) in group_lanes.iter() {
                rows.push(ChartRow {
                    top,
                    kind: RowKind::Lane { group, track },
                });
                lanes.push(Lane {
                    track,
                    top,
                    height,
                    depth_rows: 1,
                });
                top += height + METRICS.lane_gap;
            }
            bands.push(GroupBand {
                group,
                class: GROUP_ORDER[group],
                top: band_top,
                collapsed: group_lanes.is_empty(),
                shown: group_lanes.len(),
                total: group_lanes.len().max(1),
                capped: false,
                show_all: false,
            });
        }
        let layout = LaneLayout {
            lanes,
            counters: Vec::new(),
            total_height: (top - METRICS.lane_gap).max(0.0),
        };
        (
            layout,
            GroupChrome {
                bands,
                families: Vec::new(),
                rows,
            },
        )
    }

    /// The index in `profile.threads()` of the single track of `class`.
    fn track_of(profile: &Profile, class: LaneClass) -> usize {
        let mut of_class = profile
            .threads()
            .iter()
            .enumerate()
            .filter(|(_, t)| t.lane_class == class)
            .map(|(i, _)| i);
        let track = of_class.next().expect("a track of the class");
        assert_eq!(of_class.next(), None, "more than one track of the class");
        track
    }

    #[test]
    fn chart_rows_walk_each_band_then_its_lanes() {
        let (_, chrome) = stack(&[&[(7, 20.0), (3, 30.0)], &[(5, 40.0)]]);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let gap = METRICS.lane_gap;
        let second_band_top = GROUP_BAND_HEIGHT + 50.0 + 3.0 * gap;
        assert_eq!(
            rows,
            vec![
                ChartRow {
                    top: 0.0,
                    kind: RowKind::Band { group: 0 },
                },
                ChartRow {
                    top: GROUP_BAND_HEIGHT + gap,
                    kind: RowKind::Lane { group: 0, track: 7 },
                },
                ChartRow {
                    top: GROUP_BAND_HEIGHT + 20.0 + 2.0 * gap,
                    kind: RowKind::Lane { group: 0, track: 3 },
                },
                ChartRow {
                    top: second_band_top,
                    kind: RowKind::Band { group: 1 },
                },
                ChartRow {
                    top: second_band_top + GROUP_BAND_HEIGHT + gap,
                    kind: RowKind::Lane { group: 1, track: 5 },
                },
            ]
        );
    }

    #[test]
    fn a_collapsed_group_contributes_a_band_and_no_lanes() {
        let (_, chrome) = stack(&[&[(1, 20.0)], &[], &[(2, 40.0)]]);
        let kinds: Vec<RowKind> = chart_rows(&chrome).map(|r| r.kind).collect();
        assert!(matches!(
            kinds[1],
            RowKind::Lane {
                group: 0,
                track: 1,
                ..
            }
        ));
        assert!(matches!(kinds[2], RowKind::Band { group: 1 }));
        assert!(matches!(kinds[3], RowKind::Band { group: 2 }));
        assert_eq!(kinds.len(), 5);

        let (_, no_bands) = stack(&[]);
        assert_eq!(chart_rows(&no_bands).count(), 0);
    }

    /// The chart `visible` and `collapsed` build out of a fixture, every capped
    /// group sampled, with its tracks standing in `sort`.
    fn built_in(
        profile: &Profile,
        full: &LaneLayout,
        visible: &[bool],
        collapsed: &[bool; GROUP_ORDER.len()],
        sort: TrackSort,
    ) -> (LaneLayout, GroupChrome) {
        build_grouped_layout(
            profile,
            full,
            &GroupLayoutState {
                visible,
                collapsed,
                show_all: &[false; GROUP_ORDER.len()],
                sort,
            },
        )
    }

    /// The rows of the chart `visible` and `collapsed` build out of [`fixture`],
    /// under the name order.
    fn built_rows(
        profile: &Profile,
        full: &LaneLayout,
        visible: &[bool],
        collapsed: &[bool; GROUP_ORDER.len()],
    ) -> (LaneLayout, GroupChrome) {
        built_in(profile, full, visible, collapsed, TrackSort::Name)
    }

    /// The first lane row that has another lane of its group above it, so a
    /// resolution that falls back to the group's band is a move upward.
    fn a_lane_under_a_lane(rows: &[ChartRow]) -> (usize, RowKind) {
        let i = (1..rows.len())
            .find(|&i| {
                matches!(
                    (rows[i - 1].kind, rows[i].kind),
                    (RowKind::Lane { .. }, RowKind::Lane { .. })
                )
            })
            .expect("a lane below another lane of its group");
        (i, rows[i].kind)
    }

    /// The index in [`GROUP_ORDER`] of the group of `class`.
    fn group_of(class: LaneClass) -> usize {
        GROUP_ORDER
            .iter()
            .position(|&c| c == class)
            .expect("a group of the class")
    }

    /// The rows of `group`'s lanes, in the order the chart stacks them.
    fn lanes_of(rows: &[ChartRow], group: usize) -> Vec<ChartRow> {
        rows.iter()
            .copied()
            .filter(|r| matches!(r.kind, RowKind::Lane { group: g, .. } if g == group))
            .collect()
    }

    /// The track a lane row draws.
    fn track_at(row: ChartRow) -> usize {
        let RowKind::Lane { track, .. } = row.kind else {
            panic!("a lane row");
        };
        track
    }

    /// Asserts that every offset from the top of the chart down to
    /// `total_height` — each row's top edge, points inside a row, and points in
    /// the gaps between them — comes back unchanged through its anchor.
    fn assert_offsets_round_trip(rows: &[ChartRow], total_height: f32) {
        assert!(rows.len() > 1, "a chart of several rows");
        let mut probes = vec![0.0f32, total_height];
        for pair in rows.windows(2) {
            let (top, next) = (pair[0].top, pair[1].top);
            probes.extend([top, top + 0.25, (top + next) / 2.0, next - 0.25]);
        }
        let last = rows.last().expect("a last row").top;
        probes.extend([last, (last + total_height) / 2.0]);

        for y in probes {
            let anchor = anchor_at(rows.iter().copied(), y);
            assert_eq!(anchor_offset(rows.iter().copied(), anchor), y, "y = {y}");
        }
    }

    #[test]
    fn every_offset_down_a_chart_round_trips_through_its_anchor() {
        let (profile, full, visible) = fixture();
        let (layout, chrome) = built_rows(&profile, &full, &visible, &default_collapsed_groups());
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        assert_offsets_round_trip(&rows, layout.total_height);
    }

    #[test]
    fn every_offset_round_trips_over_fractional_row_geometry() {
        // Lane heights scaled for a display's pixel ratio stack the rows onto
        // fractional edges, where `top + (y - top)` need not land back on `y`.
        let deep: Vec<(usize, f32)> = (0..40).map(|i| (i, 10.5 + 0.37 * i as f32)).collect();
        let (layout, chrome) = stack(&[&[(100, 11.3), (101, 27.9)], &[], &deep]);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        assert!(
            rows.iter().filter(|r| r.top.fract() != 0.0).count() > rows.len() / 2,
            "rows off the pixel grid"
        );
        assert_offsets_round_trip(&rows, layout.total_height);
    }

    #[test]
    fn hiding_the_anchored_lane_lands_on_the_lane_that_takes_its_place() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (_, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let execution = group_of(LaneClass::Execution);
        let lanes = lanes_of(&rows, execution);
        assert!(lanes.len() > 2, "a lane with lanes above and below it");
        let anchored = lanes[1];
        let anchor = anchor_at(rows.iter().copied(), anchored.top);

        let mut hidden = visible.clone();
        hidden[track_at(anchored)] = false;
        let (_, chrome) = built_rows(&profile, &full, &hidden, &expanded);
        let redrawn: Vec<ChartRow> = chart_rows(&chrome).collect();
        let took_the_place = lanes_of(&redrawn, execution)[1];
        assert_ne!(
            track_at(took_the_place),
            track_at(anchored),
            "the lane is gone"
        );
        assert_eq!(
            took_the_place.top, anchored.top,
            "a lane rises into the slot"
        );
        assert_eq!(anchor_offset(redrawn.iter().copied(), anchor), anchored.top);
    }

    #[test]
    fn hiding_lanes_above_the_anchor_follows_the_anchored_lane_up() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (_, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let execution = group_of(LaneClass::Execution);
        let lanes = lanes_of(&rows, execution);
        assert!(lanes.len() > 3, "a lane with two lanes above it to hide");
        let anchored = lanes[3];
        let anchor = anchor_at(rows.iter().copied(), anchored.top + 3.0);

        let mut hidden = visible.clone();
        hidden[track_at(lanes[0])] = false;
        hidden[track_at(lanes[1])] = false;
        let (_, chrome) = built_rows(&profile, &full, &hidden, &expanded);
        let redrawn: Vec<ChartRow> = chart_rows(&chrome).collect();
        let still_drawn = redrawn
            .iter()
            .find(|r| r.kind.same_row(anchored.kind))
            .expect("the anchored lane");
        assert!(still_drawn.top < anchored.top, "two fewer lanes above it");
        assert_eq!(
            anchor_offset(redrawn.iter().copied(), anchor),
            still_drawn.top + 3.0
        );

        // An anchor taken while the lanes above are hidden finds its own lane
        // once they come back and stack it further down.
        let from_redrawn = anchor_at(redrawn.iter().copied(), still_drawn.top + 3.0);
        assert_eq!(
            anchor_offset(rows.iter().copied(), from_redrawn),
            anchored.top + 3.0
        );
    }

    #[test]
    fn hiding_the_last_lane_of_a_group_falls_back_to_the_lane_before_it() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (_, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let loading = group_of(LaneClass::RepoFetch);
        let lanes = lanes_of(&rows, loading);
        assert_eq!(lanes.len(), 2, "a group whose last place goes away with it");
        let anchor = anchor_at(rows.iter().copied(), lanes[1].top);

        let mut hidden = visible.clone();
        hidden[track_at(lanes[1])] = false;
        let (_, chrome) = built_rows(&profile, &full, &hidden, &expanded);
        let redrawn: Vec<ChartRow> = chart_rows(&chrome).collect();
        let left = lanes_of(&redrawn, loading);
        assert_eq!(left.len(), 1);
        assert_eq!(anchor_offset(redrawn.iter().copied(), anchor), left[0].top);
    }

    /// The anchor a gesture asking for `request` pixels down leaves standing,
    /// as the timeline re-anchors its lane scroll.
    fn scrolled(rows: &[ChartRow], anchor: RowAnchor, ceiling: f32, request: f32) -> RowAnchor {
        let held = anchor_offset(rows.iter().copied(), anchor);
        match scroll_request(held, ceiling, request) {
            Some(target) => anchor_at(rows.iter().copied(), target),
            None => anchor,
        }
    }

    #[test]
    fn a_gesture_with_no_vertical_delta_keeps_the_standing_anchor() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (_, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let execution = group_of(LaneClass::Execution);
        let anchored = lanes_of(&rows, execution)[1];
        let anchor = anchor_at(rows.iter().copied(), anchored.top);

        let mut collapsed = expanded;
        collapsed[execution] = true;
        let (_, chrome) = built_rows(&profile, &full, &visible, &collapsed);
        let folded: Vec<ChartRow> = chart_rows(&chrome).collect();
        let resolved = anchor_offset(folded.iter().copied(), anchor);
        assert_ne!(resolved, anchored.top, "the anchored lane is undrawn");

        // A ceiling the resolved place sits well inside, so only the delta decides.
        let ceiling = resolved + LINE_SCROLL_PX;
        let kept = scrolled(&folded, anchor, ceiling, resolved);
        assert_eq!(kept, anchor);
        assert_eq!(anchor_offset(rows.iter().copied(), kept), anchored.top);
        assert_ne!(
            scrolled(&folded, anchor, ceiling, resolved - 5.0),
            anchor,
            "a moved edge re-anchors"
        );
    }

    #[test]
    fn a_gesture_past_a_shrunken_ceiling_keeps_the_standing_anchor() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let execution = group_of(LaneClass::Execution);
        let anchored = lanes_of(&rows, execution)[1];
        let anchor = anchor_at(rows.iter().copied(), anchored.top);

        // A viewport the folded chart barely fills, so the anchored lane's place
        // is out of reach while its group is folded and back in reach after.
        let viewport_h = 3.0 * GROUP_BAND_HEIGHT;
        let ceiling = |l: &LaneLayout| (flame_total_height(l) - viewport_h).max(0.0);
        assert!(
            ceiling(&layout) > anchored.top,
            "the lane is reachable unfolded"
        );

        let mut collapsed = expanded;
        collapsed[execution] = true;
        let (folded_layout, folded_chrome) = built_rows(&profile, &full, &visible, &collapsed);
        let folded: Vec<ChartRow> = chart_rows(&folded_chrome).collect();
        let folded_ceiling = ceiling(&folded_layout);
        let held = anchor_offset(folded.iter().copied(), anchor);
        assert!(
            held > folded_ceiling,
            "the folded chart draws it at the ceiling"
        );

        // A wheel notch down while the folded chart already shows its last row.
        let request = drawn_scroll(held, folded_ceiling) + LINE_SCROLL_PX;
        let kept = scrolled(&folded, anchor, folded_ceiling, request);
        assert_eq!(kept, anchor);
        assert_eq!(anchor_offset(rows.iter().copied(), kept), anchored.top);
    }

    #[test]
    fn a_collapsed_group_resolves_its_lanes_to_its_band() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (_, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let (i, RowKind::Lane { group, .. }) = a_lane_under_a_lane(&rows) else {
            panic!("a lane row");
        };
        let anchor = anchor_at(rows.iter().copied(), rows[i].top + 3.0);

        let mut collapsed = expanded;
        collapsed[group] = true;
        let (_, chrome) = built_rows(&profile, &full, &visible, &collapsed);
        let band_top = chrome
            .bands
            .iter()
            .find(|b| b.group == group)
            .expect("a collapsed group keeps its band")
            .top;
        assert_eq!(anchor_offset(chart_rows(&chrome), anchor), band_top);
    }

    #[test]
    fn an_emptied_group_resolves_to_the_last_row_above_it() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (_, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let loading = group_of(LaneClass::RepoFetch);
        let i = rows
            .iter()
            .position(|r| r.kind == RowKind::Band { group: loading })
            .expect("the loading band");
        let anchor = anchor_at(rows.iter().copied(), rows[i].top);

        let mut hidden = visible.clone();
        for (track, thread) in profile.threads().iter().enumerate() {
            if thread.lane_class == LaneClass::RepoFetch {
                hidden[track] = false;
            }
        }
        let (layout, chrome) = built_rows(&profile, &full, &hidden, &expanded);
        let last_above = chrome
            .bands
            .iter()
            .take_while(|b| b.group < loading)
            .last()
            .expect("a group above the emptied one");
        assert_eq!(
            (last_above.class, last_above.shown),
            (LaneClass::Main, 1),
            "the main group's one lane ends the rows above"
        );
        let main = track_of(&profile, LaneClass::Main);
        let main_lane = layout
            .lanes
            .iter()
            .find(|l| l.track == main)
            .expect("the main lane");
        assert_eq!(anchor_offset(chart_rows(&chrome), anchor), main_lane.top);
    }

    #[test]
    fn the_chart_top_anchors_to_its_first_row_and_an_empty_chart_to_the_start() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (_, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let anchor = anchor_at(rows.iter().copied(), 0.0);
        assert_eq!(
            anchor,
            RowAnchor::Row {
                row: rows[0].kind,
                index: 0,
                below_top: 0.0
            }
        );
        assert_eq!(anchor_offset(rows.iter().copied(), anchor), 0.0);

        // The anchored row gone with every other, and a chart with no rows at all.
        let nothing_visible = vec![false; profile.threads().len()];
        let (_, chrome) = built_rows(&profile, &full, &nothing_visible, &expanded);
        assert_eq!(chart_rows(&chrome).count(), 0);
        assert_eq!(anchor_offset(chart_rows(&chrome), anchor), 0.0);
        assert_eq!(anchor_at(chart_rows(&chrome), 100.0), RowAnchor::Start);
        assert_eq!(anchor_offset(chart_rows(&chrome), RowAnchor::Start), 0.0);
    }

    #[test]
    fn an_anchor_past_the_end_stays_past_the_end() {
        let (profile, full, visible) = fixture();
        let (layout, chrome) = built_rows(&profile, &full, &visible, &default_collapsed_groups());
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let past = layout.total_height + 500.0;
        let anchor = anchor_at(rows.iter().copied(), past);
        let last_lane = *layout.lanes.last().expect("a last lane");
        let last_row = RowKind::Lane {
            group: group_of(LaneClass::Execution),
            track: last_lane.track,
        };
        // The execution group stands its band over the lanes the cap leaves it.
        let last_index = TOP_N_PER_GROUP;
        assert_eq!(
            rows.last().map(|r| r.kind),
            Some(last_row),
            "the capped execution group's last lane ends the chart"
        );
        assert_eq!(
            anchor,
            RowAnchor::Row {
                row: last_row,
                index: last_index,
                below_top: past - last_lane.top,
            }
        );
        assert_eq!(anchor_offset(rows.iter().copied(), anchor), past);
    }

    /// A profile covering every shape a group takes once the hidden tracks are
    /// dropped: an execution group still over [`TOP_N_PER_GROUP`], a two-track
    /// loading group, a lone main track, and a gc group with nothing left.
    /// Returns it with the unrestricted layout and the visibility mask.
    fn fixture() -> (Profile, LaneLayout, Vec<bool>) {
        let mut threads: Vec<(String, usize)> = (0..TOP_N_PER_GROUP + 2)
            .map(|i| (format!("skyframe-evaluator-execution-{i}"), i + 1))
            .collect();
        threads.push(("starlark-repository-a".to_owned(), 1));
        threads.push(("starlark-repository-b".to_owned(), 2));
        threads.push(("Main Thread".to_owned(), 1));
        threads.push(("Garbage Collector".to_owned(), 1));
        let profile = profile_of(&threads);
        let full = lay_out_lanes(&profile, &METRICS);
        let mut visible = vec![true; profile.threads().len()];
        visible[track_of(&profile, LaneClass::Gc)] = false;
        let an_execution_track = profile
            .threads()
            .iter()
            .position(|t| t.lane_class == LaneClass::Execution)
            .expect("an execution track");
        visible[an_execution_track] = false;
        (profile, full, visible)
    }

    /// The busy pool of [`pooled_fixture`]'s execution group, as its family name
    /// and member count.
    const BUSY_POOL: (&str, usize) = ("skyframe-evaluator-execution", 16);

    /// The near-idle pool of [`pooled_fixture`]'s execution group, by far the
    /// larger of the two and by far the lighter.
    const IDLE_POOL: (&str, usize) = ("sandbox", 100);

    /// A profile whose execution group falls into two pools of very different
    /// weight, over a lone main thread. Returns it with the unrestricted layout
    /// and the visibility mask, which hides nothing.
    fn pooled_fixture() -> (Profile, LaneLayout, Vec<bool>) {
        const BUSY_SLICES: usize = 50;
        let mut threads: Vec<(String, usize)> = (0..BUSY_POOL.1)
            .map(|i| (format!("{}-{i}", BUSY_POOL.0), BUSY_SLICES))
            .collect();
        threads.extend((0..IDLE_POOL.1).map(|i| (format!("{}-{i}", IDLE_POOL.0), 1)));
        threads.push(("Main Thread".to_owned(), 1));
        let profile = profile_of(&threads);
        let full = lay_out_lanes(&profile, &METRICS);
        let visible = vec![true; profile.threads().len()];
        (profile, full, visible)
    }

    /// The chart's pool headers, as name and shown-over-total lane count.
    fn pool_headers(chrome: &GroupChrome) -> Vec<(&str, usize, usize)> {
        chrome
            .families
            .iter()
            .map(|family| (family.name.as_str(), family.shown, family.total))
            .collect()
    }

    /// The rows `group` stacks, in chart order.
    fn rows_of(chrome: &GroupChrome, group: usize) -> Vec<ChartRow> {
        chart_rows(chrome)
            .filter(|row| row.kind.group() == group)
            .collect()
    }

    /// The row kinds `group` stacks, in chart order.
    fn kinds_of(chrome: &GroupChrome, group: usize) -> Vec<RowKind> {
        rows_of(chrome, group).iter().map(|row| row.kind).collect()
    }

    /// The pool the track a lane row draws belongs to.
    fn pool_at(profile: &Profile, row: ChartRow) -> &str {
        let thread = &profile.threads()[track_at(row)];
        track_family(thread.tid, profile.str(thread.name))
    }

    #[test]
    fn a_sampled_group_seats_every_pool_and_heads_it_with_its_counts() {
        let (profile, full, visible) = pooled_fixture();
        let (_, chrome) = built_rows(&profile, &full, &visible, &[false; GROUP_ORDER.len()]);
        // The pool order is by name, and the busy pool outbids the idle one for
        // every seat past the one each pool is guaranteed.
        assert_eq!(
            pool_headers(&chrome),
            [
                (IDLE_POOL.0, 1, IDLE_POOL.1),
                (BUSY_POOL.0, TOP_N_PER_GROUP - 1, BUSY_POOL.1),
            ]
        );
        let execution = chrome
            .bands
            .iter()
            .find(|b| b.class == LaneClass::Execution)
            .expect("an execution band");
        assert_eq!(
            (execution.shown, execution.total),
            (TOP_N_PER_GROUP, BUSY_POOL.1 + IDLE_POOL.1)
        );
    }

    #[test]
    fn a_pool_header_leads_the_lanes_it_heads() {
        let (profile, full, visible) = pooled_fixture();
        let (_, chrome) = built_rows(&profile, &full, &visible, &[false; GROUP_ORDER.len()]);
        let rows = rows_of(&chrome, group_of(LaneClass::Execution));
        assert!(matches!(rows[0].kind, RowKind::Band { .. }));
        assert!(matches!(rows[1].kind, RowKind::Family { family: 0, .. }));
        assert_eq!(pool_at(&profile, rows[2]), IDLE_POOL.0);
        assert!(matches!(rows[3].kind, RowKind::Family { family: 1, .. }));
        let busy: Vec<&str> = rows[4..]
            .iter()
            .map(|row| pool_at(&profile, *row))
            .collect();
        assert_eq!(busy, vec![BUSY_POOL.0; TOP_N_PER_GROUP - 1]);
    }

    #[test]
    fn a_group_of_one_pool_carries_no_pool_header() {
        let (profile, full, visible) = fixture();
        let (_, chrome) = built_rows(&profile, &full, &visible, &[false; GROUP_ORDER.len()]);
        assert!(chrome.bands.len() > 1, "several groups, each of one pool");
        assert!(chrome.families.is_empty());
    }

    #[test]
    fn showing_every_lane_draws_every_track_under_its_pool_header() {
        let (profile, full, visible) = pooled_fixture();
        let mut every_lane = [false; GROUP_ORDER.len()];
        every_lane[group_of(LaneClass::Execution)] = true;
        let (layout, chrome) = build_grouped_layout(
            &profile,
            &full,
            &GroupLayoutState {
                visible: &visible,
                collapsed: &[false; GROUP_ORDER.len()],
                show_all: &every_lane,
                sort: TrackSort::Name,
            },
        );
        assert_eq!(
            pool_headers(&chrome),
            [
                (IDLE_POOL.0, IDLE_POOL.1, IDLE_POOL.1),
                (BUSY_POOL.0, BUSY_POOL.1, BUSY_POOL.1),
            ]
        );
        assert_eq!(
            layout.lanes.len(),
            BUSY_POOL.1 + IDLE_POOL.1 + 1,
            "the main thread's lane too"
        );
    }

    #[test]
    fn a_collapsed_group_folds_its_pool_headers_away_with_its_lanes() {
        let (profile, full, visible) = pooled_fixture();
        let mut collapsed = [false; GROUP_ORDER.len()];
        let execution = group_of(LaneClass::Execution);
        collapsed[execution] = true;
        let (layout, chrome) = built_rows(&profile, &full, &visible, &collapsed);
        assert!(chrome.families.is_empty());
        assert_eq!(kinds_of(&chrome, execution).len(), 1, "its band alone");
        assert_eq!(layout.lanes.len(), 1, "the main thread's lane alone");
    }

    #[test]
    fn a_pool_header_row_falls_outside_every_group_band() {
        let (profile, full, visible) = pooled_fixture();
        let (_, chrome) = built_rows(&profile, &full, &visible, &[false; GROUP_ORDER.len()]);
        assert!(!chrome.families.is_empty(), "a group of several pools");
        // A press is read against a band's rect, so an overlap would fold a group
        // from a row that toggles nothing.
        for family in &chrome.families {
            for band in &chrome.bands {
                let overlaps = family.top < band.top + GROUP_BAND_HEIGHT
                    && band.top < family.top + GROUP_BAND_HEIGHT;
                assert!(!overlaps, "{family:?} over {band:?}");
            }
        }
    }

    #[test]
    fn every_offset_down_a_pooled_chart_round_trips_through_its_anchor() {
        let (profile, full, visible) = pooled_fixture();
        let (layout, chrome) = built_rows(&profile, &full, &visible, &[false; GROUP_ORDER.len()]);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        assert!(
            rows.iter()
                .any(|r| matches!(r.kind, RowKind::Family { .. })),
            "a pool header row to anchor on"
        );
        assert_offsets_round_trip(&rows, layout.total_height);
    }

    #[test]
    fn a_pool_header_holds_its_anchor_as_the_group_stops_showing_every_lane() {
        let (profile, full, visible) = pooled_fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let mut every_lane = expanded;
        every_lane[group_of(LaneClass::Execution)] = true;
        let (_, shown_all) = build_grouped_layout(
            &profile,
            &full,
            &GroupLayoutState {
                visible: &visible,
                collapsed: &expanded,
                show_all: &every_lane,
                sort: TrackSort::Name,
            },
        );
        let rows: Vec<ChartRow> = chart_rows(&shown_all).collect();
        let header = *rows
            .iter()
            .rfind(|row| matches!(row.kind, RowKind::Family { .. }))
            .expect("the busy pool's header");
        let anchor = anchor_at(rows.iter().copied(), header.top + 3.0);

        let (_, sampled) = built_rows(&profile, &full, &visible, &expanded);
        let redrawn: Vec<ChartRow> = chart_rows(&sampled).collect();
        let held = redrawn
            .iter()
            .find(|row| row.kind.same_row(header.kind))
            .expect("the header the sampled group keeps");
        assert!(held.top < header.top, "fewer lanes stand above it");
        assert_eq!(
            anchor_offset(redrawn.iter().copied(), anchor),
            held.top + 3.0
        );
    }

    #[test]
    fn a_capped_group_draws_its_busiest_tracks_in_name_order() {
        let (profile, full, visible) = fixture();
        let (_, chrome) = built_rows(&profile, &full, &visible, &[false; GROUP_ORDER.len()]);
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let names: Vec<&str> = lanes_of(&rows, group_of(LaneClass::Execution))
            .iter()
            .map(|row| profile.str(profile.threads()[track_at(*row)].name))
            .collect();
        // The fixture's execution tracks carry slice counts rising with their
        // worker index, so the cap drops the lowest-numbered visible ones.
        let busiest: Vec<String> = (2..=TOP_N_PER_GROUP + 1)
            .map(|i| format!("skyframe-evaluator-execution-{i}"))
            .collect();
        assert_eq!(names, busiest);
    }

    /// The chart of a fixture with every lane drawn, its groups standing their
    /// tracks in `sort`.
    fn built_showing_all(
        profile: &Profile,
        full: &LaneLayout,
        visible: &[bool],
        sort: TrackSort,
    ) -> (LaneLayout, GroupChrome) {
        build_grouped_layout(
            profile,
            full,
            &GroupLayoutState {
                visible,
                collapsed: &[false; GROUP_ORDER.len()],
                show_all: &[true; GROUP_ORDER.len()],
                sort,
            },
        )
    }

    /// The tracks `group` draws, in the order the chart stacks them.
    fn drawn_tracks(rows: &[ChartRow], group: usize) -> Vec<usize> {
        lanes_of(rows, group)
            .iter()
            .map(|row| track_at(*row))
            .collect()
    }

    #[test]
    fn the_weight_order_stacks_a_group_busiest_first_and_heads_no_pool() {
        let (profile, full, visible) = pooled_fixture();
        let (_, chrome) = built_showing_all(&profile, &full, &visible, TrackSort::Weight);
        let threads = profile.threads();
        let mut busiest: Vec<usize> = (0..threads.len())
            .filter(|&t| threads[t].lane_class == LaneClass::Execution)
            .collect();
        TrackSort::Weight.order(&profile, threads, &mut busiest);

        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        assert_eq!(drawn_tracks(&rows, group_of(LaneClass::Execution)), busiest);
        assert!(
            chrome.families.is_empty(),
            "a group of two pools, headed by neither"
        );
    }

    #[test]
    fn the_weight_order_seats_a_capped_group_by_weight_alone() {
        let (profile, full, visible) = pooled_fixture();
        let (_, chrome) = built_in(
            &profile,
            &full,
            &visible,
            &[false; GROUP_ORDER.len()],
            TrackSort::Weight,
        );
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let drawn = drawn_tracks(&rows, group_of(LaneClass::Execution));
        assert_eq!(drawn.len(), TOP_N_PER_GROUP);
        let names: Vec<&str> = drawn
            .iter()
            .map(|&t| profile.str(profile.threads()[t].name))
            .collect();
        assert!(
            names.iter().all(|name| name.starts_with(BUSY_POOL.0)),
            "every seat goes to the pool holding the weight: {names:?}"
        );
        assert!(chrome.families.is_empty());
    }

    #[test]
    fn changing_the_order_restacks_a_group_without_moving_a_track_out_of_it() {
        let (profile, full, visible) = pooled_fixture();
        let (_, by_name) = built_showing_all(&profile, &full, &visible, TrackSort::Name);
        let (_, by_weight) = built_showing_all(&profile, &full, &visible, TrackSort::Weight);
        let named: Vec<ChartRow> = chart_rows(&by_name).collect();
        let weighed: Vec<ChartRow> = chart_rows(&by_weight).collect();
        for group in 0..GROUP_ORDER.len() {
            let sorted = |rows: &[ChartRow]| {
                let mut tracks = drawn_tracks(rows, group);
                tracks.sort_unstable();
                tracks
            };
            assert_eq!(sorted(&named), sorted(&weighed), "group {group}");
        }
        let execution = group_of(LaneClass::Execution);
        assert_ne!(
            drawn_tracks(&named, execution),
            drawn_tracks(&weighed, execution),
            "the same tracks, stacked in a different order"
        );
    }

    #[test]
    fn every_offset_down_a_weight_ordered_chart_round_trips_through_its_anchor() {
        let (profile, full, visible) = pooled_fixture();
        let (layout, chrome) = built_in(
            &profile,
            &full,
            &visible,
            &[false; GROUP_ORDER.len()],
            TrackSort::Weight,
        );
        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        assert_offsets_round_trip(&rows, layout.total_height);
    }

    /// The near-idle pool of [`many_pool_fixture`]'s execution group, which the
    /// weight order's sample drops every lane of.
    const DROPPED_POOL: &str = "sandbox";

    /// The pools of [`many_pool_fixture`]'s execution group, each as its family
    /// name, member count, and slices per member. The last holds nearly all the
    /// group's weight and more members than a capped group seats, so the weight
    /// order's sample seats it alone.
    const POOLS: [(&str, usize, usize); 4] = [
        ("combined-cache", 3, 5),
        ("merkle-tree", 3, 4),
        (DROPPED_POOL, 6, 1),
        ("skyframe-evaluator-execution", TOP_N_PER_GROUP + 2, 50),
    ];

    /// A profile whose execution group falls into [`POOLS`], over a lone main
    /// thread. Returns it with the unrestricted layout and the visibility mask,
    /// which hides nothing.
    fn many_pool_fixture() -> (Profile, LaneLayout, Vec<bool>) {
        let mut threads: Vec<(String, usize)> = POOLS
            .iter()
            .flat_map(|&(pool, members, slices)| {
                (0..members).map(move |i| (format!("{pool}-{i}"), slices))
            })
            .collect();
        threads.push(("Main Thread".to_owned(), 1));
        let profile = profile_of(&threads);
        let full = lay_out_lanes(&profile, &METRICS);
        let visible = vec![true; profile.threads().len()];
        (profile, full, visible)
    }

    /// The chart of [`many_pool_fixture`] under each track order, with the rows
    /// its execution group stacks in each.
    fn ordered_both_ways(
        profile: &Profile,
        full: &LaneLayout,
        visible: &[bool],
    ) -> (GroupChrome, Vec<ChartRow>, GroupChrome, Vec<ChartRow>) {
        let expanded = [false; GROUP_ORDER.len()];
        let (_, by_name) = built_in(profile, full, visible, &expanded, TrackSort::Name);
        let (_, by_weight) = built_in(profile, full, visible, &expanded, TrackSort::Weight);
        let execution = group_of(LaneClass::Execution);
        let named = rows_of(&by_name, execution);
        let weighed = rows_of(&by_weight, execution);
        (by_name, named, by_weight, weighed)
    }

    /// Asserts that `anchor` resolves against `chrome` to the row `group` stacks
    /// at `place`, which stands inside the group rather than at its end.
    fn assert_holds_its_place(chrome: &GroupChrome, group: usize, place: usize, anchor: RowAnchor) {
        let stacked = rows_of(chrome, group);
        assert!(
            place < stacked.len(),
            "the group stacks {} rows, short of place {place}",
            stacked.len()
        );
        let resolved = anchor_offset(chart_rows(chrome), anchor);
        assert_eq!(resolved, stacked[place].top, "the row holding the place");
        let band = stacked[0].top;
        let last = stacked.last().expect("a row").top;
        assert!(
            resolved >= band,
            "{resolved} above the group's band at {band}"
        );
        assert!(resolved < last, "{resolved} at the group's last row {last}");
    }

    #[test]
    fn a_lane_the_weight_order_drops_keeps_its_place_in_its_group() {
        let (profile, full, visible) = many_pool_fixture();
        let (by_name, named, by_weight, _) = ordered_both_ways(&profile, &full, &visible);
        let place = named
            .iter()
            .position(|row| {
                matches!(row.kind, RowKind::Lane { .. }) && pool_at(&profile, *row) == DROPPED_POOL
            })
            .expect("a drawn lane of the near-idle pool");
        let anchor = anchor_at(chart_rows(&by_name), named[place].top);

        assert!(
            !chart_rows(&by_weight).any(|row| row.kind.same_row(named[place].kind)),
            "the weight order's sample drops the anchored lane"
        );
        assert_holds_its_place(&by_weight, group_of(LaneClass::Execution), place, anchor);
    }

    #[test]
    fn a_lane_the_name_order_drops_keeps_its_place_in_its_group() {
        let (profile, full, visible) = many_pool_fixture();
        let (by_name, named, by_weight, weighed) = ordered_both_ways(&profile, &full, &visible);
        let place = weighed
            .iter()
            .position(|row| !named.iter().any(|drawn| drawn.kind.same_row(row.kind)))
            .expect("a drawn lane the name order's sample drops");
        let anchor = anchor_at(chart_rows(&by_weight), weighed[place].top);

        assert_holds_its_place(&by_name, group_of(LaneClass::Execution), place, anchor);
    }

    #[test]
    fn a_lane_both_orders_draw_holds_its_anchor_across_the_change() {
        let (profile, full, visible) = many_pool_fixture();
        let (by_name, named, by_weight, weighed) = ordered_both_ways(&profile, &full, &visible);
        let anchored = *named
            .iter()
            .rfind(|row| weighed.iter().any(|drawn| drawn.kind.same_row(row.kind)))
            .expect("a lane both orders draw");
        let restacked = *weighed
            .iter()
            .find(|row| row.kind.same_row(anchored.kind))
            .expect("the lane the weight order keeps");
        assert_ne!(restacked.top, anchored.top, "the orders stack it elsewhere");

        let anchor = anchor_at(chart_rows(&by_name), anchored.top + 3.0);
        assert_eq!(
            anchor_offset(chart_rows(&by_weight), anchor),
            restacked.top + 3.0
        );
    }

    #[test]
    fn every_offset_down_a_many_pool_chart_round_trips_under_either_order() {
        let (profile, full, visible) = many_pool_fixture();
        for sort in [TrackSort::Name, TrackSort::Weight] {
            let (layout, chrome) =
                built_in(&profile, &full, &visible, &[false; GROUP_ORDER.len()], sort);
            let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
            assert_offsets_round_trip(&rows, layout.total_height);
        }
    }

    #[test]
    fn a_built_layout_stacks_every_band_and_lane_once() {
        let (profile, full, visible) = fixture();
        let (layout, chrome) = built_rows(&profile, &full, &visible, &default_collapsed_groups());
        let band = |class| chrome.bands.iter().find(|b| b.class == class);
        let execution = band(LaneClass::Execution).expect("an execution band");
        assert!(execution.capped);
        assert_eq!(
            (execution.shown, execution.total),
            (TOP_N_PER_GROUP, TOP_N_PER_GROUP + 1)
        );
        let loading = band(LaneClass::RepoFetch).expect("a loading band");
        assert!(loading.collapsed);
        assert_eq!((loading.shown, loading.total), (0, 2));
        assert_eq!(band(LaneClass::Main).map(|b| b.shown), Some(1));
        assert!(band(LaneClass::Gc).is_none(), "every gc track is hidden");

        let rows: Vec<ChartRow> = chart_rows(&chrome).collect();
        let bands: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Band { group } => Some(group),
                RowKind::Family { .. } | RowKind::Lane { .. } => None,
            })
            .collect();
        let lanes: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Lane { track, .. } => Some(track),
                RowKind::Band { .. } | RowKind::Family { .. } => None,
            })
            .collect();
        assert_eq!(
            bands,
            chrome.bands.iter().map(|b| b.group).collect::<Vec<_>>()
        );
        assert_eq!(
            lanes,
            layout.lanes.iter().map(|l| l.track).collect::<Vec<_>>()
        );
        assert!(rows.windows(2).all(|w| w[0].top <= w[1].top), "{rows:?}");
    }

    #[test]
    fn flame_total_height_ends_at_the_last_row() {
        let (profile, full, visible) = fixture();
        let build = |visible: &[bool], collapsed: &[bool; GROUP_ORDER.len()]| {
            built_rows(&profile, &full, visible, collapsed)
        };
        let expanded = [false; GROUP_ORDER.len()];
        let middle_collapsed = default_collapsed_groups();
        let mut last_collapsed = expanded;
        last_collapsed[GROUP_ORDER
            .iter()
            .position(|&c| c == LaneClass::Execution)
            .expect("an execution group")] = true;
        let all_collapsed = [true; GROUP_ORDER.len()];
        let nothing_visible = vec![false; profile.threads().len()];

        type Shape = fn(&GroupChrome) -> bool;
        for (label, visible, collapsed, shape) in [
            (
                "every group expanded",
                &visible[..],
                &expanded,
                (|c| !c.bands.is_empty() && c.bands.iter().all(|b| !b.collapsed)) as Shape,
            ),
            (
                "a collapsed group in the middle",
                &visible[..],
                &middle_collapsed,
                |c| {
                    c.bands
                        .iter()
                        .position(|b| b.collapsed)
                        .is_some_and(|i| i > 0 && i + 1 < c.bands.len())
                },
            ),
            (
                "a collapsed group last",
                &visible[..],
                &last_collapsed,
                |c| c.bands.len() > 1 && c.bands.last().is_some_and(|b| b.collapsed),
            ),
            ("every group collapsed", &visible[..], &all_collapsed, |c| {
                !c.bands.is_empty() && c.bands.iter().all(|b| b.collapsed)
            }),
            ("no non-empty group", &nothing_visible[..], &expanded, |c| {
                c.bands.is_empty()
            }),
        ] {
            let (layout, chrome) = build(visible, collapsed);
            assert!(shape(&chrome), "{label}");
            let last_band = chrome.bands.last().map(|b| b.top + GROUP_BAND_HEIGHT);
            let last_lane = layout.lanes.last().map(|l| l.top + l.height);
            let last_bottom = last_band.into_iter().chain(last_lane).fold(0.0, f32::max);
            assert_eq!(flame_total_height(&layout), last_bottom, "{label}");
        }
    }
}
