use crate::{GROUP_BAND_HEIGHT, METRICS, TOP_N_PER_GROUP};
use bviz_trace_layout::{Lane, LaneLayout};
use bviz_trace_profile::{LaneClass, Profile};

/// The full layout restricted to the tracks `visible` admits, the kept lanes
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
    let activity = |t: usize| profile.slices(&threads[t]).len();
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
        // Busiest first; tie-break by tid so the order is stable frame to frame.
        tracks.sort_by(|&a, &b| {
            activity(b)
                .cmp(&activity(a))
                .then(threads[a].tid.cmp(&threads[b].tid))
        });
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
        total_height: top,
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
