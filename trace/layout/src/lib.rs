mod cull;
mod flame;
mod lanes;
mod view;

pub use cull::{Viewport, VisibleSlice, visible_slices};
pub use flame::{
    FlameBox, FlameMetrics, FlameNode, build_flame_graph, flame_height, lay_out_flame,
};
pub use lanes::{CounterPos, Lane, LaneLayout, LaneMetrics, lay_out_lanes};
pub use view::TimeView;

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_trace_profile::{
        CounterLane, Metadata, Profile, Slice, SliceArgs, ThreadTrack, TimeSpan,
    };

    fn slice(start_us: i64, dur_us: i64, depth: u32) -> Slice {
        Slice {
            name: String::new(),
            category: String::new(),
            start_us,
            dur_us,
            depth,
            args: SliceArgs::default(),
        }
    }

    fn track(tid: i64, sort_index: i64, max_depth: u32, slices: Vec<Slice>) -> ThreadTrack {
        ThreadTrack {
            tid,
            name: format!("t{tid}"),
            sort_index,
            slices,
            max_depth,
        }
    }

    fn profile(threads: Vec<ThreadTrack>) -> Profile {
        Profile {
            metadata: Metadata::default(),
            threads,
            counters: Vec::new(),
            phase_markers: Vec::new(),
            span: TimeSpan {
                start_us: 0,
                end_us: 1000,
            },
        }
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
        let p = profile(vec![
            track(1, 0, 1, vec![slice(0, 10, 0)]),
            track(2, 1, 2, vec![slice(0, 10, 0)]),
        ]);
        let layout = lay_out_lanes(&p, &METRICS);
        assert_eq!(layout.lanes[0].top, 0.0);
        assert_eq!(layout.lanes[0].height, 15.0);
        assert_eq!(layout.lanes[1].top, 19.0);
        assert_eq!(layout.lanes[1].height, 20.0);
        assert_eq!(layout.total_height, 39.0);
    }

    #[test]
    fn counter_lanes_stack_below_threads() {
        let mut p = profile(vec![track(1, 0, 1, vec![slice(0, 10, 0)])]);
        p.counters = vec![CounterLane {
            name: "cpu".into(),
            series: Vec::new(),
        }];
        let layout = lay_out_lanes(&p, &METRICS);
        // Thread lane: top 0, height 10 + 5 = 15; then a 4px gap.
        assert_eq!(layout.lanes[0].height, 15.0);
        assert_eq!(layout.counters.len(), 1);
        assert_eq!(layout.counters[0].top, 19.0);
        assert_eq!(layout.counters[0].height, 30.0);
        assert_eq!(layout.total_height, 49.0);
    }

    #[test]
    fn culls_off_time_and_subpixel_slices() {
        let p = profile(vec![track(
            1,
            0,
            1,
            vec![
                slice(0, 100, 0),
                slice(200, 50, 0),
                slice(250, 0, 0),
                slice(10_000, 50, 0),
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
        let xs: Vec<f32> = vis.iter().map(|v| v.x).collect();
        assert_eq!(xs, vec![0.0, 200.0]);
        assert_eq!(vis[0].w, 100.0);
        assert_eq!(vis[0].y, METRICS.header_height);
    }

    #[test]
    fn culls_scrolled_off_lane() {
        let p = profile(vec![track(1, 0, 1, vec![slice(0, 100, 0)])]);
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
