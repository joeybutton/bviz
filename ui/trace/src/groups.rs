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
pub(crate) fn sort_tracks_by_activity(profile: &Profile, threads: &[ThreadTrack], tracks: &mut [usize]) {
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
            .filter(|&t| {
                threads[t].lane_class == class && visible.get(t).copied().unwrap_or(true)
            })
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
    use bviz_trace_layout::lay_out_lanes;

    /// What a row of the flame chart's stack draws: a band's group, or a lane's
    /// track.
    #[derive(Debug, Clone, Copy)]
    enum Item {
        Band(usize),
        Lane(usize),
    }

    #[derive(Debug)]
    struct Row {
        item: Item,
        top: f32,
        bottom: f32,
    }

    /// The stack in draw order — each band followed by the lanes it shows —
    /// taking every edge from the layout and chrome as built.
    fn rows(layout: &LaneLayout, chrome: &GroupChrome) -> Vec<Row> {
        let mut rows = Vec::with_capacity(chrome.bands.len() + layout.lanes.len());
        let mut lanes = layout.lanes.iter();
        for band in &chrome.bands {
            rows.push(Row {
                item: Item::Band(band.group),
                top: band.top,
                bottom: band.top + GROUP_BAND_HEIGHT,
            });
            rows.extend(lanes.by_ref().take(band.shown).map(|lane| Row {
                item: Item::Lane(lane.track),
                top: lane.top,
                bottom: lane.top + lane.height,
            }));
        }
        rows
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

        let stack = rows(&layout, &chrome);
        let bands: Vec<usize> = stack
            .iter()
            .filter_map(|r| match r.item {
                Item::Band(group) => Some(group),
                Item::Lane(_) => None,
            })
            .collect();
        let lanes: Vec<usize> = stack
            .iter()
            .filter_map(|r| match r.item {
                Item::Lane(track) => Some(track),
                Item::Band(_) => None,
            })
            .collect();
        assert_eq!(bands, chrome.bands.iter().map(|b| b.group).collect::<Vec<_>>());
        assert_eq!(lanes, layout.lanes.iter().map(|l| l.track).collect::<Vec<_>>());
        assert!(stack.windows(2).all(|w| w[0].top <= w[1].top), "{stack:?}");
    }

    #[test]
    fn flame_total_height_ends_at_the_last_row() {
        let (profile, full, visible) = fixture();
        let build = |visible: &[bool], collapsed: &[bool; GROUP_ORDER.len()]| {
            build_grouped_layout(&profile, &full, visible, collapsed, &[false; GROUP_ORDER.len()])
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
            (
                "every group collapsed",
                &visible[..],
                &all_collapsed,
                |c| !c.bands.is_empty() && c.bands.iter().all(|b| b.collapsed),
            ),
            (
                "no non-empty group",
                &nothing_visible[..],
                &expanded,
                |c| c.bands.is_empty(),
            ),
        ] {
            let (layout, chrome) = build(visible, collapsed);
            assert!(shape(&chrome), "{label}");
            let last_bottom = rows(&layout, &chrome).last().map_or(0.0, |row| row.bottom);
            assert_eq!(flame_total_height(&layout), last_bottom, "{label}");
        }
    }
}
