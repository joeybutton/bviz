use crate::{
    BAR_WIDTH, FLAME_MIN_HEIGHT, METRICS, MIN_CHART_HEIGHT, MIN_GUTTER, RULER_HEIGHT,
    STATS_HEADER_HEIGHT, STATS_ROW_HEIGHT, TAB_BAR_HEIGHT,
};
use bviz_trace_layout::LaneLayout;
use gpui::{Bounds, Pixels};

/// The window content split into the flame chart region on top, the tab bar,
/// and the bottom panel, plus the gutter/time-axis columns shared across them.
/// Derived once per frame from the viewport bounds, the counter count, the panel
/// height, the gutter width, and the flame lanes' total height (to decide whether
/// a vertical bar column must be reserved). Both paint and the mouse handlers
/// compute the same regions so hit-testing matches what was drawn.
#[derive(Clone, Copy)]
pub(crate) struct Regions {
    /// Left edge of the time content, past the gutter.
    pub(crate) content_left: f32,
    /// Right edge of the content column (where the vertical bar/gutter end).
    pub(crate) content_right: f32,
    /// Right edge of the time axis, left of the reserved vertical-bar column.
    /// Everything mapped to time ends here so nothing underlaps the bar.
    pub(crate) time_right: f32,
    /// Width of the time axis: the content column less the reserved vertical-bar
    /// column.
    pub(crate) time_w: f32,
    /// Top of the flame lanes (ruler bottom).
    pub(crate) flame_top: f32,
    /// Bottom of the scrollable flame lanes; the time horizontal bar sits below.
    pub(crate) flame_bottom: f32,
    /// Top of the tab bar (below the flame region and its horizontal bar).
    pub(crate) tab_top: f32,
    /// Bottom of the tab bar, which is the bottom panel's top.
    pub(crate) panel_top: f32,
    /// Bottom of the bottom panel (window bottom).
    pub(crate) panel_bottom: f32,
    /// Whether the profile has counters (so the Counters tab has content).
    pub(crate) has_graph: bool,
}

impl Regions {
    /// Splits `bounds` given the counter count, the panel's stored height, the
    /// gutter width, and the flame lanes' total height (to decide whether a
    /// vertical bar shows and so a column must be reserved for it).
    pub(crate) fn new(
        bounds: Bounds<Pixels>,
        n_counters: usize,
        panel_height: f32,
        gutter_width: f32,
        flame_total: f32,
    ) -> Self {
        let ox = f32::from(bounds.origin.x);
        let oy = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let vh = f32::from(bounds.size.height);
        let panel_bottom = oy + vh;
        let panel_h = clamp_panel_height(panel_height, n_counters, vh);
        let panel_top = (panel_bottom - panel_h).max(oy);
        let tab_top = (panel_top - TAB_BAR_HEIGHT).max(oy);
        // The flame region: ruler at the top, scrollable lanes, then a strip for
        // the time horizontal bar always reserved above the tab bar.
        let flame_top = oy + RULER_HEIGHT;
        let flame_bottom = (tab_top - BAR_WIDTH).max(flame_top);
        // Clamp the stored gutter width to the current window: a widened gutter
        // shrunk under by a later window resize must still leave the time axis
        // its share rather than collapsing it.
        let gutter_width = clamp_gutter_width(gutter_width, w);
        let content_left = ox + gutter_width;
        let content_w = (w - gutter_width).max(0.0);
        let content_right = ox + w;
        let flame_h = flame_bottom - flame_top;
        // Reserve a column for the vertical bar when the flame lanes overflow it,
        // so slices and charts end left of the bar rather than under it.
        let vbar_reserve = if flame_total > flame_h.max(0.0) {
            BAR_WIDTH
        } else {
            0.0
        };
        Regions {
            content_left,
            content_right,
            time_right: content_right - vbar_reserve,
            time_w: (content_w - vbar_reserve).max(0.0),
            flame_top,
            flame_bottom,
            tab_top,
            panel_top,
            panel_bottom,
            has_graph: n_counters > 0,
        }
    }

    /// Height the flame lanes scroll within.
    pub(crate) fn flame_h(&self) -> f32 {
        (self.flame_bottom - self.flame_top).max(0.0)
    }

    /// Height of the bottom panel below the tab bar.
    pub(crate) fn panel_h(&self) -> f32 {
        (self.panel_bottom - self.panel_top).max(0.0)
    }
}

/// The bottom panel's height bounded to leave the flame region at least
/// `FLAME_MIN_HEIGHT` and to keep the panel tall enough for its content — every
/// counter lane at least a header band plus `MIN_CHART_HEIGHT` of chart, or at
/// least a stats header plus one row — so no tab collapses to nothing.
pub(crate) fn clamp_panel_height(panel_height: f32, n_counters: usize, vh: f32) -> f32 {
    let counter_floor = n_counters as f32 * (METRICS.header_height + MIN_CHART_HEIGHT);
    let min = counter_floor.max(STATS_HEADER_HEIGHT + STATS_ROW_HEIGHT);
    let max = (vh - RULER_HEIGHT - BAR_WIDTH - TAB_BAR_HEIGHT - FLAME_MIN_HEIGHT).max(min);
    panel_height.clamp(min, max)
}

/// The gutter width bounded to stay legible yet never claim more than half the
/// window, so the time axis keeps the larger share.
pub(crate) fn clamp_gutter_width(gutter_width: f32, window_w: f32) -> f32 {
    let max = (window_w * 0.5).max(MIN_GUTTER);
    gutter_width.clamp(MIN_GUTTER, max)
}

/// Total height the flame section's thread lanes occupy, ignoring the layout's
/// counter lanes (the graph section places those itself).
pub(crate) fn flame_total_height(layout: &LaneLayout) -> f32 {
    layout.lanes.last().map(|l| l.top + l.height).unwrap_or(0.0)
}

/// The pixel ceiling for the lane scroll: the thread-lane height past the flame
/// region's on-screen height.
pub(crate) fn max_lane_scroll(layout: &LaneLayout, regions: &Regions) -> f32 {
    (flame_total_height(layout) - regions.flame_h()).max(0.0)
}

/// One fitted counter lane in the panel: its top (window y) and the height of
/// its chart area below the header band.
#[derive(Clone, Copy)]
pub(crate) struct GraphLane {
    pub(crate) lane: usize,
    pub(crate) top: f32,
    pub(crate) chart_h: f32,
}

/// Distributes the graph section's height evenly across its `n` counter lanes:
/// each lane is a header band plus a chart area that shrinks to zero as the
/// section collapses toward the label-only minimum.
pub(crate) fn graph_lanes(n: usize, graph_top: f32, graph_h: f32) -> Vec<GraphLane> {
    if n == 0 {
        return Vec::new();
    }
    let chart_h = ((graph_h - n as f32 * METRICS.header_height) / n as f32).max(0.0);
    let lane_h = METRICS.header_height + chart_h;
    (0..n)
        .map(|lane| GraphLane {
            lane,
            top: graph_top + lane as f32 * lane_h,
            chart_h,
        })
        .collect()
}
