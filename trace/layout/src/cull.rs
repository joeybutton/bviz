use bviz_trace_profile::{Profile, Slice};

use crate::lanes::{LaneLayout, LaneMetrics};
use crate::paint::{Span, painted};
use crate::view::TimeView;

/// The visible region, in pixels. `scroll_y` is how far the lanes are scrolled up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    pub scroll_y: f32,
}

/// The time-to-pixel mapping, visible pixel region, lane sizing, and minimum
/// slice width — the parameters that place a profile's slices onto the screen.
#[derive(Debug, Clone, Copy)]
pub struct Cull<'a> {
    pub view: &'a TimeView,
    pub viewport: &'a Viewport,
    pub metrics: &'a LaneMetrics,
    pub min_px: f32,
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

/// Slices that fall within the viewport, dropping off-screen lanes and off-time
/// slices. Each is placed against the boxes already painted at its depth by
/// `paint::painted`, and its painted span is bounded to a margin around the
/// viewport.
pub fn visible_slices(profile: &Profile, layout: &LaneLayout, cull: Cull) -> Vec<VisibleSlice> {
    let Cull {
        view,
        viewport,
        metrics,
        min_px,
    } = cull;
    let view_end_us = view.t_of(viewport.width);
    let mut out = Vec::new();
    let mut covered: Vec<f64> = Vec::new();
    for lane in &layout.lanes {
        let lane_top = lane.top - viewport.scroll_y;
        if lane_top + lane.height < 0.0 || lane_top > viewport.height {
            continue;
        }
        covered.clear();
        let rows_top = lane.top + metrics.header_height - viewport.scroll_y;
        let track = &profile.threads()[lane.track];
        let slices = profile.slices(track);
        // Slices are sorted by start, so none past the right edge can be visible.
        let upper = slices.partition_point(|s| s.start_us as f64 <= view_end_us);
        // Nothing left of this bound paints, so the covered edge is still clear
        // at every slice up to it: against a clear edge a slice's painted right
        // edge is at most its own end plus min_px, which for an end left of
        // t_of(-min_px) is off the left of the viewport. Ends' prefix max is
        // monotone, so binary-searching it for that backed-off bound skips no
        // slice with visible extent (only edge cases within a float rounding
        // step of the boundary, whose painted width is sub-pixel).
        let view_start_us = view.t_of(-min_px);
        let end_maxes = profile.end_maxes(track);
        let lower = end_maxes[..upper].partition_point(|&e| (e as f64) < view_start_us);
        for (i, s) in slices[lower..upper].iter().enumerate() {
            emit_slice(
                s,
                lane.track,
                lower + i,
                cull,
                rows_top,
                &mut covered,
                &mut out,
            );
        }
    }
    out
}

/// Place one slice for painting, bounding its painted span to a viewport margin.
/// `covered` carries the painted right edge per depth row across the calls of one
/// lane; `slice` is the slice's index in the track.
pub(crate) fn emit_slice(
    s: &Slice,
    track: usize,
    slice: usize,
    cull: Cull,
    rows_top: f32,
    covered: &mut Vec<f64>,
    out: &mut Vec<VisibleSlice>,
) {
    let Cull {
        view,
        viewport,
        metrics,
        min_px,
    } = cull;
    let depth = s.depth as usize;
    if depth >= covered.len() {
        covered.resize(depth + 1, f64::NEG_INFINITY);
    }
    let x = view.x_of(s.start_us) as f64;
    let content = Span {
        x,
        w: view.x_of(s.end_us()) as f64 - x,
    };
    let Some(paint) = painted(content, covered[depth], min_px as f64) else {
        return;
    };
    let view_w = viewport.width as f64;
    if paint.end() < 0.0 || paint.x > view_w {
        return;
    }
    covered[depth] = paint.end();
    // Bound the painted span to a margin around the viewport. At extreme zoom a
    // span reaching far off-screen otherwise carries coordinates large enough
    // that the renderer's rounded-rect distance field loses precision and places
    // the clipped edge wrong, bleeding past the content mask; the visible portion
    // is unchanged since the margin exceeds the viewport.
    let paint_x = paint.x.max(-view_w);
    let paint_w = paint.end().min(2.0 * view_w) - paint_x;
    out.push(VisibleSlice {
        track,
        slice,
        x: paint_x as f32,
        y: rows_top + s.depth as f32 * metrics.row_height,
        w: paint_w as f32,
        h: metrics.row_height,
    });
}
