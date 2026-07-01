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

/// Slices that fall within the viewport, dropping off-screen lanes and
/// off-time slices. A slice too narrow to reach `min_px` is widened to it so it
/// stays a visible marker of where to zoom in; to keep such markers from
/// stacking, a widened slice starting in a pixel column an earlier slice at the
/// same depth already covers is dropped. Slices wide enough to render on their
/// own are always kept.
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
    // First pixel column free of an emitted slice, per depth row; markers whose
    // start column falls before it coalesce into the slice already there.
    let mut next_free_col: Vec<i32> = Vec::new();
    for lane in &layout.lanes {
        let lane_top = lane.top - viewport.scroll_y;
        if lane_top + lane.height < 0.0 || lane_top > viewport.height {
            continue;
        }
        next_free_col.clear();
        let rows_top = lane.top + metrics.header_height - viewport.scroll_y;
        let slices = profile.slices(&profile.threads()[lane.track]);
        // Slices are sorted by start, so none past the right edge can be visible.
        let upper = slices.partition_point(|s| s.start_us as f64 <= view_end_us);
        for (slice, s) in slices[..upper].iter().enumerate() {
            let x = view.x_of(s.start_us);
            let natural = view.x_of(s.start_us + s.dur_us) - x;
            let w = natural.max(min_px);
            if x + w < 0.0 || x > viewport.width {
                continue;
            }
            let depth = s.depth as usize;
            if depth >= next_free_col.len() {
                next_free_col.resize(depth + 1, i32::MIN);
            }
            let start_col = x.floor() as i32;
            if natural < min_px && start_col < next_free_col[depth] {
                continue;
            }
            next_free_col[depth] = ((x + w).floor() as i32).max(start_col + 1);
            // Bound the painted span to a margin around the viewport. At extreme
            // zoom a span reaching far off-screen otherwise carries coordinates
            // large enough that the renderer's rounded-rect distance field loses
            // precision and places the clipped edge wrong, bleeding past the
            // content mask; the visible portion is unchanged since the margin
            // exceeds the viewport.
            let paint_x = x.max(-viewport.width);
            let paint_w = (x + w).min(2.0 * viewport.width) - paint_x;
            out.push(VisibleSlice {
                track: lane.track,
                slice,
                x: paint_x,
                y: rows_top + s.depth as f32 * metrics.row_height,
                w: paint_w,
                h: metrics.row_height,
            });
        }
    }
    out
}
