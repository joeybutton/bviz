mod cull;
mod flame;
mod lanes;
mod stats;
#[cfg(test)]
mod test_profile;
mod view;

pub use bviz_trace_profile::{LaneClass, WorkClass};
pub use cull::{Viewport, VisibleSlice, visible_slices};
pub use flame::{
    FlameBox, FlameGraph, FlameMetrics, FlameNode, FlameView, GroupMode, Scope, build_flame_graph,
    flame_box_at, flame_height, lay_out_flame,
};
pub use lanes::{CounterPos, Lane, LaneLayout, LaneMetrics, lay_out_lanes};
pub use stats::{StatRow, aggregate_stats, stats_from_flame};
pub use view::TimeView;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_profile::{counter, profile, profile_with_counters, slice, track};
    use bviz_trace_profile::{LaneClass, TimeSpan};

    /// A track whose slices' nesting yields `depth_rows` slice rows (max_depth).
    /// One slice gives one row; an outer span containing `depth_rows - 1` nested
    /// spans gives that many rows.
    fn track_rows(tid: i64, depth_rows: u32) -> crate::test_profile::TestTrack {
        let mut slices = vec![slice("s0", "general information", 0, 10)];
        for d in 1..depth_rows {
            // Each deeper span is strictly contained in the one above it.
            let dur = 10 - d as i64;
            slices.push(slice(&format!("s{d}"), "general information", 0, dur.max(1)));
        }
        track(tid, LaneClass::Execution, slices)
    }

    const METRICS: LaneMetrics = LaneMetrics {
        header_height: 10.0,
        row_height: 5.0,
        lane_gap: 4.0,
        counter_height: 20.0,
    };

    #[test]
    fn fit_maps_span_to_width() {
        let span = TimeSpan {
            start_us: 0,
            end_us: 1000,
        };
        // No margin: the span fills the width exactly.
        let view = TimeView::fit(span, 100.0, 0.0);
        assert_eq!(view.px_per_us, 0.1);
        assert_eq!(view.x_of(500), 50.0);
        assert_eq!(view.t_of(50.0), 500.0);
        // A 10px margin insets the span: it spans pixels [10, 90].
        let inset = TimeView::fit(span, 100.0, 10.0);
        assert_eq!(inset.x_of(0), 10.0);
        assert_eq!(inset.x_of(1000), 90.0);
    }

    #[test]
    fn zoom_keeps_time_under_cursor_fixed() {
        let mut view = TimeView::fit(
            TimeSpan {
                start_us: 0,
                end_us: 1000,
            },
            100.0,
            0.0,
        );
        let anchor_t = view.t_of(50.0);
        view.zoom_at(50.0, 2.0);
        assert_eq!(view.px_per_us, 0.2);
        assert_eq!(view.t_of(50.0), anchor_t);
    }

    #[test]
    fn pan_shifts_start_by_pixels() {
        let mut view = TimeView {
            start_us: 0.0,
            px_per_us: 2.0,
        };
        view.pan_px(10.0);
        assert_eq!(view.start_us, -5.0);
    }

    #[test]
    fn clamp_bounds_pan_to_span() {
        let span = TimeSpan {
            start_us: 0,
            end_us: 1000,
        };
        // Zoomed in (100px shows 500us): the left edge stays in [0, 500].
        let mut zoomed = TimeView {
            start_us: 900.0,
            px_per_us: 0.2,
        };
        zoomed.clamp_to(span, 100.0, 0.0);
        assert_eq!(zoomed.start_us, 500.0);
        zoomed.start_us = -300.0;
        zoomed.clamp_to(span, 100.0, 0.0);
        assert_eq!(zoomed.start_us, 0.0);
        // Zoomed out so the whole span fits: it is pinned to the left.
        let mut wide = TimeView {
            start_us: 400.0,
            px_per_us: 0.05,
        };
        wide.clamp_to(span, 100.0, 0.0);
        assert_eq!(wide.start_us, 0.0);
    }

    #[test]
    fn lanes_stack_with_gaps() {
        // tid 1 (one slice row) stacks above tid 2 (two nested rows); both default
        // to the same sort index, so they order by tid.
        let p = profile(vec![track_rows(1, 1), track_rows(2, 2)]);
        let layout = lay_out_lanes(&p, &METRICS);
        assert_eq!(layout.lanes[0].top, 0.0);
        assert_eq!(layout.lanes[0].height, 15.0);
        assert_eq!(layout.lanes[1].top, 19.0);
        assert_eq!(layout.lanes[1].height, 20.0);
        assert_eq!(layout.total_height, 39.0);
    }

    #[test]
    fn counter_lanes_stack_below_threads() {
        let p = profile_with_counters(vec![track_rows(1, 1)], &[counter("cpu", vec![(0, 1.0)])]);
        let layout = lay_out_lanes(&p, &METRICS);
        // Thread lane: top 0, height 10 + 5 = 15; then a 4px gap.
        assert_eq!(layout.lanes[0].height, 15.0);
        assert_eq!(layout.counters.len(), 1);
        assert_eq!(layout.counters[0].top, 19.0);
        assert_eq!(layout.counters[0].height, 30.0);
        assert_eq!(layout.total_height, 49.0);
    }

    #[test]
    fn culls_off_time_and_widens_subpixel_slices() {
        // Four disjoint top-level slices: one on-screen, one on-screen, one
        // zero-width, one far off-time. All sit at depth 0 (no nesting).
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                slice("a", "general information", 0, 100),
                slice("b", "general information", 200, 50),
                slice("c", "general information", 250, 0),
                slice("d", "general information", 10_000, 50),
            ],
        )]);
        let layout = lay_out_lanes(&p, &METRICS);
        let view = TimeView {
            start_us: 0.0,
            px_per_us: 1.0,
        };
        let viewport = Viewport {
            width: 300.0,
            height: 100.0,
            scroll_y: 0.0,
        };
        let vis = visible_slices(&p, &layout, &view, &viewport, &METRICS, 0.5);
        let placed: Vec<(f32, f32)> = vis.iter().map(|v| (v.x, v.w)).collect();
        // "d" is off-time and dropped; the zero-width "c" survives as a
        // min-width marker in its own pixel column past "b".
        assert_eq!(placed, vec![(0.0, 100.0), (200.0, 50.0), (250.0, 0.5)]);
        assert_eq!(vis[0].y, METRICS.header_height);
    }

    #[test]
    fn subpixel_slices_coalesce_into_one_marker_per_column() {
        // At 0.1px/us, ten microseconds share a pixel column. An outer span
        // holds three zero-width children: two land in the same column, one a
        // column over. Coalescing keeps one marker per column, and the wide
        // outer span covering those columns at depth 0 does not suppress the
        // depth-1 markers.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                slice("outer", "general information", 100, 200),
                slice("a", "general information", 110, 0),
                slice("b", "general information", 112, 0),
                slice("c", "general information", 130, 0),
            ],
        )]);
        let layout = lay_out_lanes(&p, &METRICS);
        let view = TimeView {
            start_us: 0.0,
            px_per_us: 0.1,
        };
        let viewport = Viewport {
            width: 300.0,
            height: 100.0,
            scroll_y: 0.0,
        };
        let vis = visible_slices(&p, &layout, &view, &viewport, &METRICS, 1.0);
        let placed: Vec<(f32, f32, f32)> = vis.iter().map(|v| (v.x, v.w, v.y)).collect();
        let row0 = METRICS.header_height;
        let row1 = METRICS.header_height + METRICS.row_height;
        assert_eq!(
            placed,
            vec![
                (10.0, 20.0, row0), // depth-0 "outer" spans columns 10..30
                (11.0, 1.0, row1),  // depth-1: "a"; "b" coalesces into its column
                (13.0, 1.0, row1),  // depth-1: "c" opens a fresh column
            ]
        );
    }

    #[test]
    fn far_off_screen_span_is_clamped_to_a_viewport_margin() {
        // A span far wider than the view: at high zoom its raw pixel extent is
        // enormous. It is kept (it covers the whole viewport) but its painted
        // rect is bounded to one viewport width past each edge, leaving the
        // on-screen portion [0, width] fully covered.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![slice("wide", "general information", 0, 1_000_000)],
        )]);
        let layout = lay_out_lanes(&p, &METRICS);
        // Center the view deep inside the span so it spills far off both edges.
        let view = TimeView {
            start_us: 500_000.0,
            px_per_us: 10.0,
        };
        let viewport = Viewport {
            width: 300.0,
            height: 100.0,
            scroll_y: 0.0,
        };
        let vis = visible_slices(&p, &layout, &view, &viewport, &METRICS, 1.0);
        assert_eq!(vis.len(), 1);
        let s = vis[0];
        assert_eq!(s.x, -viewport.width);
        assert_eq!(s.x + s.w, 2.0 * viewport.width);
        // The whole visible width still lies within the painted rect.
        assert!(s.x <= 0.0 && s.x + s.w >= viewport.width);
    }

    #[test]
    fn culls_scrolled_off_lane() {
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![slice("a", "general information", 0, 100)],
        )]);
        let layout = lay_out_lanes(&p, &METRICS);
        let view = TimeView {
            start_us: 0.0,
            px_per_us: 1.0,
        };
        let viewport = Viewport {
            width: 300.0,
            height: 100.0,
            scroll_y: 1000.0,
        };
        assert!(visible_slices(&p, &layout, &view, &viewport, &METRICS, 0.5).is_empty());
    }
}
