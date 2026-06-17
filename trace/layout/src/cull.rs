use bviz_trace_profile::Profile;

use crate::lanes::{LaneLayout, LaneMetrics};
use crate::view::TimeView;

/// The visible region, in pixels. `scroll_y` is how far the lanes are scrolled up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub scroll_y: f32,
}

/// A slice positioned for painting. `track`/`slice` index back into the profile
/// for label and tooltip lookup.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VisibleSlice {
    pub track: usize,
    pub slice: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// Slices that fall within the viewport, dropping off-screen lanes, off-time
/// slices, and ones rendering narrower than `min_px`.
pub fn visible_slices(
    profile: &Profile,
    layout: &LaneLayout,
    view: &TimeView,
    viewport: &Viewport,
    metrics: &LaneMetrics,
    min_px: f32,
) -> Vec<VisibleSlice> {
    let view_end_us = view.t_of(viewport.width);
    let mut out = Vec::new();
    for lane in &layout.lanes {
        let lane_top = lane.top - viewport.scroll_y;
        if lane_top + lane.height < 0.0 || lane_top > viewport.height {
            continue;
        }
        let rows_top = lane.top + metrics.header_height - viewport.scroll_y;
        let slices = &profile.threads[lane.track].slices;
        // Slices are sorted by start, so none past the right edge can be visible.
        let upper = slices.partition_point(|s| s.start_us as f64 <= view_end_us);
        for (slice, s) in slices[..upper].iter().enumerate() {
            let x = view.x_of(s.start_us);
            let w = view.x_of(s.start_us + s.dur_us) - x;
            if w < min_px || x + w < 0.0 || x > viewport.width {
                continue;
            }
            out.push(VisibleSlice {
                track: lane.track,
                slice,
                x,
                y: rows_top + s.depth as f32 * metrics.row_height,
                w,
                h: metrics.row_height,
            });
        }
    }
    out
}
