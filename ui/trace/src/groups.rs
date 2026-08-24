use crate::{GROUP_BAND_HEIGHT, METRICS, TOP_N_PER_GROUP};
use bviz_trace_layout::{Lane, LaneLayout};
use bviz_trace_profile::{LaneClass, Profile, ThreadTrack};

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
/// lanes is `capped`, showing only the busiest until `show_all`.
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

/// The group headers overlaid on a grouped lane layout.
#[derive(Debug, Clone, Default)]
pub(crate) struct GroupChrome {
    pub(crate) bands: Vec<GroupBand>,
}

/// A row of the flame chart's vertical stack as the chart currently draws it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ChartRow {
    pub(crate) top: f32,
    pub(crate) kind: RowKind,
}

/// Which row a [`ChartRow`] is, and what it stands for in the document: a band's
/// lane-class group, as an index into [`GROUP_ORDER`] — what the collapse and
/// show-all toggles key on — or a lane's track (an index into
/// `Profile::threads`, so it survives a restack) under the group it hangs from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RowKind {
    Band {
        group: usize,
    },
    Lane {
        group: usize,
        track: usize,
        /// The lane's place in its group's activity order, which is fixed for a
        /// profile: `track` names the row itself, `rank` the place another lane
        /// takes over when the chart stops drawing that track.
        rank: usize,
    },
}

impl RowKind {
    /// Chart order over row identities: groups follow [`GROUP_ORDER`], a group's
    /// band leads its lanes, and its lanes follow their ranks.
    fn order(self) -> (usize, u8, usize) {
        match self {
            RowKind::Band { group } => (group, 0, 0),
            RowKind::Lane { group, rank, .. } => (group, 1, rank),
        }
    }

    fn group(self) -> usize {
        match self {
            RowKind::Band { group } | RowKind::Lane { group, .. } => group,
        }
    }

    /// Whether both name the same row of the document, whatever rank the chart
    /// currently gives it.
    fn same_row(self, other: Self) -> bool {
        match (self, other) {
            (RowKind::Band { group: a }, RowKind::Band { group: b }) => a == b,
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

/// The flame chart's rows in `top` order — each group's header band followed by
/// the lanes it shows — over a grouped layout and the chrome built with it.
pub(crate) fn chart_rows<'a>(
    layout: &'a LaneLayout,
    chrome: &'a GroupChrome,
) -> impl Iterator<Item = ChartRow> + 'a {
    let mut bands = chrome.bands.iter();
    let mut lanes = layout.lanes.iter();
    // The group whose lanes are being walked, the rank the next of them takes,
    // and how many of them are left.
    let mut under: Option<(usize, usize, usize)> = None;
    std::iter::from_fn(move || {
        if let Some((group, rank, left)) = under {
            under = (left > 1).then_some((group, rank + 1, left - 1));
            let lane = lanes.next()?;
            return Some(ChartRow {
                top: lane.top,
                kind: RowKind::Lane {
                    group,
                    track: lane.track,
                    rank,
                },
            });
        }
        let band = bands.next()?;
        under = (band.shown > 0).then_some((band.group, 0, band.shown));
        Some(ChartRow {
            top: band.top,
            kind: RowKind::Band { group: band.group },
        })
    })
}

/// Where the flame chart's viewport top edge sits, as a position the rows carry
/// with them when a collapse, a visibility change, or an activity restack moves
/// them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RowAnchor {
    /// The top of the chart.
    Start,
    /// `below_top` pixels below `row`'s top edge, which past the end of the
    /// chart runs beyond the row itself.
    Row { row: RowKind, below_top: f32 },
}

/// The anchor for `y` pixels down the chart. A `y` past the last row anchors to
/// that row, so the position returns once the chart grows back that far.
pub(crate) fn anchor_at(rows: impl Iterator<Item = ChartRow>, y: f32) -> RowAnchor {
    rows.take_while(|row| row.top <= y)
        .last()
        .map_or(RowAnchor::Start, |row| RowAnchor::Row {
            row: row.kind,
            below_top: y - row.top,
        })
}

/// How many pixels down the chart `anchor` sits. A row the chart no longer draws
/// — its track hidden, its group collapsed or emptied — resolves to the last row
/// drawn above the place it held: the lane that took over its rank in its group,
/// else the group's band, else the last row of the groups before it, and the
/// start when nothing precedes it.
pub(crate) fn anchor_offset(rows: impl Iterator<Item = ChartRow>, anchor: RowAnchor) -> f32 {
    let RowAnchor::Row { row, below_top } = anchor else {
        return 0.0;
    };
    let mut fallback = 0.0;
    for drawn in rows {
        if drawn.kind.same_row(row) {
            return drawn.top + below_top;
        }
        // A track keeps its group but not its rank, so the anchored row can be
        // anywhere among its group's.
        if drawn.kind.group() > row.group() {
            break;
        }
        if drawn.kind.order() <= row.order() {
            fallback = drawn.top;
        }
    }
    fallback
}

/// What a press on a group header band hit: its gutter half (toggles the group's
/// collapse) or, for a capped expanded group, its time half (toggles whether the
/// group shows every lane or only the busiest).
#[derive(Debug, Clone, Copy)]
pub(crate) enum GroupHit {
    Collapse(usize),
    ShowAll(usize),
}

/// Orders `tracks` (indices into `threads`) busiest first — most slices first,
/// ties broken by tid so the order is stable frame to frame.
pub(crate) fn sort_tracks_by_activity(
    profile: &Profile,
    threads: &[ThreadTrack],
    tracks: &mut [usize],
) {
    tracks.sort_by(|&a, &b| {
        profile
            .slices(&threads[b])
            .len()
            .cmp(&profile.slices(&threads[a]).len())
            .then(threads[a].tid.cmp(&threads[b].tid))
    });
}

/// The lane layout the flame chart draws: visible tracks ordered into lane-class
/// groups, each group behind a header band and sorted by descending activity, so
/// the busiest lanes lead and a collapsed or capped group folds its lanes away.
/// Returns the restacked [`LaneLayout`] (its `lanes` tops leave room for the
/// bands) and the [`GroupChrome`] painted over it. A track absent from `visible`
/// defaults to shown.
pub(crate) fn build_grouped_layout(
    profile: &Profile,
    full: &LaneLayout,
    visible: &[bool],
    collapsed: &[bool; GROUP_ORDER.len()],
    show_all: &[bool; GROUP_ORDER.len()],
) -> (LaneLayout, GroupChrome) {
    let threads = profile.threads();
    let mut lanes = Vec::new();
    let mut chrome = GroupChrome::default();
    let mut top = 0.0;
    for (group, &class) in GROUP_ORDER.iter().enumerate() {
        let mut tracks: Vec<usize> = (0..threads.len())
            .filter(|&t| threads[t].lane_class == class && visible.get(t).copied().unwrap_or(true))
            .collect();
        if tracks.is_empty() {
            continue;
        }
        let total = tracks.len();
        sort_tracks_by_activity(profile, threads, &mut tracks);
        let band_top = top;
        top += GROUP_BAND_HEIGHT + METRICS.lane_gap;
        let is_collapsed = collapsed[group];
        let capped = total > TOP_N_PER_GROUP;
        let mut shown = 0;
        if !is_collapsed {
            let cap = if show_all[group] {
                tracks.len()
            } else {
                TOP_N_PER_GROUP.min(tracks.len())
            };
            for &t in &tracks[..cap] {
                let lane = full.lanes[t];
                lanes.push(Lane { top, ..lane });
                top += lane.height + METRICS.lane_gap;
            }
            shown = cap;
        }
        chrome.bands.push(GroupBand {
            group,
            class,
            top: band_top,
            collapsed: is_collapsed,
            shown,
            total,
            capped,
            show_all: show_all[group],
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
    use crate::{LINE_SCROLL_PX, drawn_scroll, scroll_request};
    use bviz_trace_layout::lay_out_lanes;

    /// Stacks one band per group, each followed by its `(track, height)` lanes,
    /// in the geometry the flame chart draws. A group given no lanes stands for
    /// a collapsed one; a group with no tracks at all, which
    /// [`build_grouped_layout`] drops from the stack, cannot be expressed here.
    fn stack(groups: &[&[(usize, f32)]]) -> (LaneLayout, GroupChrome) {
        let mut lanes = Vec::new();
        let mut bands = Vec::new();
        let mut top = 0.0;
        for (group, group_lanes) in groups.iter().enumerate() {
            let band_top = top;
            top += GROUP_BAND_HEIGHT + METRICS.lane_gap;
            for &(track, height) in *group_lanes {
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
        (layout, GroupChrome { bands })
    }

    /// Parses a profile of threads named `(thread name, slice count)` through the
    /// real Chrome-trace parser, which derives each thread's lane class from its
    /// name. A thread needs a slice to reach the model, so a count of zero yields
    /// no track.
    fn profile_of(threads: &[(String, usize)]) -> Profile {
        let mut events = Vec::new();
        for (i, (name, slices)) in threads.iter().enumerate() {
            // tid 0 is Bazel's synthetic critical-path thread, whatever its name.
            let tid = i + 1;
            events.push(format!(
                r#"{{"name":"thread_name","ph":"M","tid":{tid},"args":{{"name":"{name}"}}}}"#
            ));
            for ts in 0..*slices {
                events.push(format!(
                    r#"{{"cat":"general information","name":"s","ph":"X","ts":{ts},"dur":1,"tid":{tid}}}"#
                ));
            }
        }
        let json = format!(r#"{{"traceEvents":[{}]}}"#, events.join(","));
        Profile::from_bytes(json.as_bytes()).expect("parse test profile")
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
        let (layout, chrome) = stack(&[&[(7, 20.0), (3, 30.0)], &[(5, 40.0)]]);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
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
                    kind: RowKind::Lane {
                        group: 0,
                        track: 7,
                        rank: 0
                    },
                },
                ChartRow {
                    top: GROUP_BAND_HEIGHT + 20.0 + 2.0 * gap,
                    kind: RowKind::Lane {
                        group: 0,
                        track: 3,
                        rank: 1
                    },
                },
                ChartRow {
                    top: second_band_top,
                    kind: RowKind::Band { group: 1 },
                },
                ChartRow {
                    top: second_band_top + GROUP_BAND_HEIGHT + gap,
                    kind: RowKind::Lane {
                        group: 1,
                        track: 5,
                        rank: 0
                    },
                },
            ]
        );
    }

    #[test]
    fn a_collapsed_group_contributes_a_band_and_no_lanes() {
        let (layout, chrome) = stack(&[&[(1, 20.0)], &[], &[(2, 40.0)]]);
        let kinds: Vec<RowKind> = chart_rows(&layout, &chrome).map(|r| r.kind).collect();
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

        let (empty, no_bands) = stack(&[]);
        assert_eq!(chart_rows(&empty, &no_bands).count(), 0);
    }

    /// The rows of the chart `visible` and `collapsed` build out of [`fixture`].
    fn built_rows(
        profile: &Profile,
        full: &LaneLayout,
        visible: &[bool],
        collapsed: &[bool; GROUP_ORDER.len()],
    ) -> (LaneLayout, GroupChrome) {
        build_grouped_layout(
            profile,
            full,
            visible,
            collapsed,
            &[false; GROUP_ORDER.len()],
        )
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

    /// The rows of `group`'s lanes, in rank order.
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
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        assert_offsets_round_trip(&rows, layout.total_height);
    }

    #[test]
    fn every_offset_round_trips_over_fractional_row_geometry() {
        // Lane heights scaled for a display's pixel ratio stack the rows onto
        // fractional edges, where `top + (y - top)` need not land back on `y`.
        let deep: Vec<(usize, f32)> = (0..40).map(|i| (i, 10.5 + 0.37 * i as f32)).collect();
        let (layout, chrome) = stack(&[&[(100, 11.3), (101, 27.9)], &[], &deep]);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        assert!(
            rows.iter().filter(|r| r.top.fract() != 0.0).count() > rows.len() / 2,
            "rows off the pixel grid"
        );
        assert_offsets_round_trip(&rows, layout.total_height);
    }

    #[test]
    fn hiding_the_anchored_lane_lands_on_the_lane_that_takes_its_rank() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let execution = group_of(LaneClass::Execution);
        let lanes = lanes_of(&rows, execution);
        assert!(lanes.len() > 2, "a lane with lanes above and below it");
        let anchored = lanes[1];
        let anchor = anchor_at(rows.iter().copied(), anchored.top);

        let mut hidden = visible.clone();
        hidden[track_at(anchored)] = false;
        let (layout, chrome) = built_rows(&profile, &full, &hidden, &expanded);
        let redrawn: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let took_the_rank = lanes_of(&redrawn, execution)[1];
        assert_eq!(track_at(took_the_rank), track_at(lanes[2]));
        assert_eq!(took_the_rank.top, anchored.top, "it rises into the slot");
        assert_eq!(anchor_offset(redrawn.iter().copied(), anchor), anchored.top);
    }

    #[test]
    fn hiding_lanes_above_the_anchor_follows_the_anchored_lane_up() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let execution = group_of(LaneClass::Execution);
        let lanes = lanes_of(&rows, execution);
        assert!(lanes.len() > 3, "a lane with two lanes above it to hide");
        let anchored = lanes[3];
        let anchor = anchor_at(rows.iter().copied(), anchored.top + 3.0);

        let mut hidden = visible.clone();
        hidden[track_at(lanes[0])] = false;
        hidden[track_at(lanes[1])] = false;
        let (layout, chrome) = built_rows(&profile, &full, &hidden, &expanded);
        let redrawn: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
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
        // once they come back and rank it further down.
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
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let loading = group_of(LaneClass::RepoFetch);
        let lanes = lanes_of(&rows, loading);
        assert_eq!(lanes.len(), 2, "a group whose last rank goes away with it");
        let anchor = anchor_at(rows.iter().copied(), lanes[1].top);

        let mut hidden = visible.clone();
        hidden[track_at(lanes[1])] = false;
        let (layout, chrome) = built_rows(&profile, &full, &hidden, &expanded);
        let redrawn: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
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
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let execution = group_of(LaneClass::Execution);
        let anchored = lanes_of(&rows, execution)[1];
        let anchor = anchor_at(rows.iter().copied(), anchored.top);

        let mut collapsed = expanded;
        collapsed[execution] = true;
        let (layout, chrome) = built_rows(&profile, &full, &visible, &collapsed);
        let folded: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
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
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
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
        let folded: Vec<ChartRow> = chart_rows(&folded_layout, &folded_chrome).collect();
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
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let (i, RowKind::Lane { group, .. }) = a_lane_under_a_lane(&rows) else {
            panic!("a lane row");
        };
        let anchor = anchor_at(rows.iter().copied(), rows[i].top + 3.0);

        let mut collapsed = expanded;
        collapsed[group] = true;
        let (layout, chrome) = built_rows(&profile, &full, &visible, &collapsed);
        let band_top = chrome
            .bands
            .iter()
            .find(|b| b.group == group)
            .expect("a collapsed group keeps its band")
            .top;
        assert_eq!(
            anchor_offset(chart_rows(&layout, &chrome), anchor),
            band_top
        );
    }

    #[test]
    fn an_emptied_group_resolves_to_the_last_row_above_it() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
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
        assert_eq!(
            anchor_offset(chart_rows(&layout, &chrome), anchor),
            main_lane.top
        );
    }

    #[test]
    fn the_chart_top_anchors_to_its_first_row_and_an_empty_chart_to_the_start() {
        let (profile, full, visible) = fixture();
        let expanded = [false; GROUP_ORDER.len()];
        let (layout, chrome) = built_rows(&profile, &full, &visible, &expanded);
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let anchor = anchor_at(rows.iter().copied(), 0.0);
        assert_eq!(
            anchor,
            RowAnchor::Row {
                row: rows[0].kind,
                below_top: 0.0
            }
        );
        assert_eq!(anchor_offset(rows.iter().copied(), anchor), 0.0);

        // The anchored row gone with every other, and a chart with no rows at all.
        let nothing_visible = vec![false; profile.threads().len()];
        let (layout, chrome) = built_rows(&profile, &full, &nothing_visible, &expanded);
        assert_eq!(chart_rows(&layout, &chrome).count(), 0);
        assert_eq!(anchor_offset(chart_rows(&layout, &chrome), anchor), 0.0);
        assert_eq!(
            anchor_at(chart_rows(&layout, &chrome), 100.0),
            RowAnchor::Start
        );
        assert_eq!(
            anchor_offset(chart_rows(&layout, &chrome), RowAnchor::Start),
            0.0
        );
    }

    #[test]
    fn an_anchor_past_the_end_stays_past_the_end() {
        let (profile, full, visible) = fixture();
        let (layout, chrome) = built_rows(&profile, &full, &visible, &default_collapsed_groups());
        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let past = layout.total_height + 500.0;
        let anchor = anchor_at(rows.iter().copied(), past);
        let last_lane = *layout.lanes.last().expect("a last lane");
        let last_row = RowKind::Lane {
            group: group_of(LaneClass::Execution),
            track: last_lane.track,
            rank: TOP_N_PER_GROUP - 1,
        };
        assert_eq!(
            rows.last().map(|r| r.kind),
            Some(last_row),
            "the capped execution group's last lane ends the chart"
        );
        assert_eq!(
            anchor,
            RowAnchor::Row {
                row: last_row,
                below_top: past - last_lane.top
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

    #[test]
    fn a_built_layout_stacks_every_band_and_lane_once() {
        let (profile, full, visible) = fixture();
        let (layout, chrome) = build_grouped_layout(
            &profile,
            &full,
            &visible,
            &default_collapsed_groups(),
            &[false; GROUP_ORDER.len()],
        );
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

        let rows: Vec<ChartRow> = chart_rows(&layout, &chrome).collect();
        let bands: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Band { group } => Some(group),
                RowKind::Lane { .. } => None,
            })
            .collect();
        let lanes: Vec<usize> = rows
            .iter()
            .filter_map(|r| match r.kind {
                RowKind::Lane { track, .. } => Some(track),
                RowKind::Band { .. } => None,
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
            build_grouped_layout(
                &profile,
                &full,
                visible,
                collapsed,
                &[false; GROUP_ORDER.len()],
            )
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
