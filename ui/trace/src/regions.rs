use crate::{
    COUNTERS_DIVIDER_HEIGHT, FLAME_MIN_HEIGHT, METRICS, MIN_CHART_HEIGHT, MIN_GUTTER, RULER_HEIGHT,
};
use bviz_trace_layout::LaneLayout;
use bviz_ui_components::SCROLLBAR_THICKNESS;
use gpui::{Bounds, Pixels, Point, point, px, size};

/// The timeline split into the flame chart region on top, the divider, and the
/// counters strip, plus the gutter/time-axis columns shared across them. Derived
/// once per frame from the viewport bounds, the counter count, the strip height,
/// the gutter width, and the flame lanes' total height (to decide whether a
/// vertical bar column must be reserved). Both paint and the mouse handlers
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
    /// Top of the ruler band (the viewport's top edge).
    ruler_top: f32,
    /// Top of the flame lanes (ruler bottom).
    pub(crate) flame_top: f32,
    /// Bottom of the scrollable flame lanes; the time horizontal bar sits below.
    pub(crate) flame_bottom: f32,
    /// Top of the divider (below the flame region and its horizontal bar).
    divider_top: f32,
    /// Top of the counters strip, which is the divider's bottom.
    pub(crate) counters_top: f32,
    /// Bottom of the counters strip, which is the viewport's bottom edge and so
    /// the bottom of the gutter column running the whole height.
    pub(crate) counters_bottom: f32,
    /// Whether the profile has counters, and so whether the strip and the
    /// divider that resizes it are drawn at all.
    pub(crate) has_graph: bool,
}

impl Regions {
    /// Splits `bounds` given the counter count, the counters strip's stored
    /// height, the gutter width, and the flame lanes' total height (to decide
    /// whether a vertical bar shows and so a column must be reserved for it).
    pub(crate) fn new(
        bounds: Bounds<Pixels>,
        n_counters: usize,
        counters_height: f32,
        gutter_width: f32,
        flame_total: f32,
    ) -> Self {
        let ox = f32::from(bounds.origin.x);
        let oy = f32::from(bounds.origin.y);
        let w = f32::from(bounds.size.width);
        let vh = f32::from(bounds.size.height);
        let has_graph = n_counters > 0;
        let counters_bottom = oy + vh;
        let counters_h = clamp_counters_height(counters_height, n_counters, vh);
        let counters_top = (counters_bottom - counters_h).max(oy);
        let divider_top = match has_graph {
            true => (counters_top - COUNTERS_DIVIDER_HEIGHT).max(oy),
            false => counters_top,
        };
        // The flame region: ruler at the top, scrollable lanes, then a strip for
        // the time horizontal bar always reserved above the divider.
        let flame_top = oy + RULER_HEIGHT;
        let flame_bottom = (divider_top - SCROLLBAR_THICKNESS).max(flame_top);
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
            SCROLLBAR_THICKNESS
        } else {
            0.0
        };
        Regions {
            content_left,
            content_right,
            time_right: content_right - vbar_reserve,
            time_w: (content_w - vbar_reserve).max(0.0),
            ruler_top: oy,
            flame_top,
            flame_bottom,
            divider_top,
            counters_top,
            counters_bottom,
            has_graph,
        }
    }

    /// The band between the flame lanes and the counters strip that a press
    /// takes hold of to resize the strip.
    pub(crate) fn divider_rect(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.content_left), px(self.divider_top)),
            size(
                px((self.content_right - self.content_left).max(0.0)),
                px(COUNTERS_DIVIDER_HEIGHT),
            ),
        )
    }

    /// The ruler band over the time axis: the strip the selection's regions are
    /// tinted and gripped in, and that a press takes hold of them from.
    pub(crate) fn ruler_rect(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.content_left), px(self.ruler_top)),
            size(px(self.time_w), px(self.flame_top - self.ruler_top)),
        )
    }

    pub(crate) fn in_ruler(&self, pos: Point<Pixels>) -> bool {
        self.ruler_rect().contains(&pos)
    }

    /// The flame lanes' time content, which their paint is masked to and their
    /// hover is tested against, so a tooltip is offered exactly where a slice
    /// can be drawn.
    pub(crate) fn flame_rect(&self) -> Bounds<Pixels> {
        Bounds::new(
            point(px(self.content_left), px(self.flame_top)),
            size(px(self.time_w), px(self.flame_h())),
        )
    }

    pub(crate) fn in_flame(&self, pos: Point<Pixels>) -> bool {
        self.flame_rect().contains(&pos)
    }

    /// Height the flame lanes scroll within.
    pub(crate) fn flame_h(&self) -> f32 {
        (self.flame_bottom - self.flame_top).max(0.0)
    }

    /// Height of the counters strip below the divider.
    pub(crate) fn counters_h(&self) -> f32 {
        (self.counters_bottom - self.counters_top).max(0.0)
    }
}

/// The counters strip's height bounded to leave the flame region at least
/// `FLAME_MIN_HEIGHT` and to keep every counter lane a header band plus
/// `MIN_CHART_HEIGHT` of chart, so no lane collapses to nothing. A profile with
/// no counters has no strip, whatever height was dragged for another one.
pub(crate) fn clamp_counters_height(counters_height: f32, n_counters: usize, vh: f32) -> f32 {
    if n_counters == 0 {
        return 0.0;
    }
    let min = n_counters as f32 * (METRICS.header_height + MIN_CHART_HEIGHT);
    let max =
        (vh - RULER_HEIGHT - SCROLLBAR_THICKNESS - COUNTERS_DIVIDER_HEIGHT - FLAME_MIN_HEIGHT)
            .max(min);
    counters_height.clamp(min, max)
}

/// The gutter width bounded to stay legible yet never claim more than half the
/// window, so the time axis keeps the larger share.
pub(crate) fn clamp_gutter_width(gutter_width: f32, window_w: f32) -> f32 {
    let max = (window_w * 0.5).max(MIN_GUTTER);
    gutter_width.clamp(MIN_GUTTER, max)
}

/// Total height the flame section's rows occupy — thread lanes and the group
/// bands they hang under, which share one coordinate space — ignoring the
/// layout's counter lanes (the counters strip places those itself).
pub(crate) fn flame_total_height(layout: &LaneLayout) -> f32 {
    layout.total_height
}

/// The pixel ceiling for the lane scroll: the thread-lane height past the flame
/// region's on-screen height.
pub(crate) fn max_lane_scroll(layout: &LaneLayout, regions: &Regions) -> f32 {
    (flame_total_height(layout) - regions.flame_h()).max(0.0)
}

/// One fitted counter lane in the strip: its top (window y) and the height of
/// its chart area below the header band.
#[derive(Clone, Copy)]
pub(crate) struct GraphLane {
    pub(crate) lane: usize,
    pub(crate) top: f32,
    pub(crate) chart_h: f32,
}

/// Distributes the counters strip's height evenly across its `n` counter lanes:
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
