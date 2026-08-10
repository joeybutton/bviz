mod cull;
mod flame;
mod lanes;
mod paint;
mod scopes;
mod selection;
mod stats;
#[cfg(test)]
mod test_profile;
mod view;

pub use bviz_trace_profile::{LaneClass, SpanRole, Workload};
pub use cull::{Cull, Viewport, VisibleSlice, visible_slices};
pub use flame::{
    FlameBox, FlameDirection, FlameGraph, FlameLabels, FlameMetrics, FlameMode, FlameNode,
    FlamePlacement, TrackFlamer, flame_height, lay_out_flame,
};
pub use lanes::{CounterPos, Lane, LaneLayout, LaneMetrics, lay_out_lanes};
pub use scopes::ScopeAggregate;
pub use selection::{TimeRegion, TimeSelection, span_between};
pub use stats::{StatRow, stats_from_flame};
pub use view::{FlameView, TimeView};

/// Sizes rayon's global thread pool to the physical worker count. Call once at
/// startup, before any parallel layout pass; a no-op if the pool already exists.
pub fn init_thread_pool() {
    rayon::ThreadPoolBuilder::new()
        .num_threads(bviz_trace_profile::worker_count())
        .build_global()
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_profile::{counter, profile, profile_with_counters, slice, track};
    use bviz_trace_profile::{LaneClass, TimeSpan};

    /// A track whose slices' nesting yields `depth_rows` slice rows.
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

    /// Asserts one depth row's painted spans — `(x, w)` pairs in paint order —
    /// leave no hole over `[start, end)`: the first reaches back to `start`, each
    /// reaches the next, and the last reaches `end`.
    pub(crate) fn assert_gapless(row: &[(f32, f32)], start: f32, end: f32) {
        assert!(!row.is_empty(), "nothing painted over [{start}, {end})");
        for pair in row.windows(2) {
            let (x, w) = pair[0];
            assert!(x + w >= pair[1].0, "hole between {:?} and {:?}", pair[0], pair[1]);
        }
        let last = row[row.len() - 1];
        assert!(row[0].0 <= start, "band opens at {} past {start}", row[0].0);
        assert!(last.0 + last.1 >= end, "band closes at {} short of {end}", last.0 + last.1);
    }

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
        let vis = visible_slices(
            &p,
            &layout,
            Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: 0.5 },
        );
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
        let vis = visible_slices(
            &p,
            &layout,
            Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: 1.0 },
        );
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

    /// The painted `(x, w)` of every slice `visible_slices` keeps, in paint
    /// order, over a viewport wide enough to hold the whole profile.
    fn painted_row(p: &bviz_trace_profile::Profile, view: TimeView) -> Vec<(f32, f32)> {
        let layout = lay_out_lanes(p, &METRICS);
        let viewport = Viewport {
            width: 300.0,
            height: 100.0,
            scroll_y: 0.0,
        };
        visible_slices(
            p,
            &layout,
            Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: 1.0 },
        )
        .iter()
        .map(|v| (v.x, v.w))
        .collect()
    }

    #[test]
    fn contiguous_subpixel_slices_paint_a_gapless_band() {
        // Eleven touching 3us slices at 0.25px/us: each is 0.75px, so none
        // renders on its own, and the run starts mid-column at 0.5px.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            (0..11)
                .map(|i: i64| slice(&format!("s{i}"), "general information", 2 + 3 * i, 3))
                .collect(),
        )]);
        let view = TimeView {
            start_us: 0.0,
            px_per_us: 0.25,
        };
        assert_gapless(&painted_row(&p, view), view.x_of(2), view.x_of(35));
    }

    #[test]
    fn a_subpixel_run_joins_the_wide_slices_around_it_without_a_gap() {
        // At 0.125px/us a 1px slice, then three 0.875px ones — each running past
        // the edge the one before it paints to — then a 3px slice. The run leaves
        // off at 3.625px, whose fractional part exceeds a half, so the wide
        // slice's left edge rounds up to the next device column: a painted span
        // stopping short of it leaves a whole empty column between the two.
        let p = profile(vec![track(
            1,
            LaneClass::Execution,
            vec![
                slice("w0", "general information", 0, 8),
                slice("n0", "general information", 8, 7),
                slice("n1", "general information", 15, 7),
                slice("n2", "general information", 22, 7),
                slice("w1", "general information", 29, 24),
            ],
        )]);
        let view = TimeView {
            start_us: 0.0,
            px_per_us: 0.125,
        };
        assert_gapless(&painted_row(&p, view), view.x_of(0), view.x_of(53));
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
        let vis = visible_slices(
            &p,
            &layout,
            Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: 1.0 },
        );
        assert_eq!(vis.len(), 1);
        let s = vis[0];
        assert_eq!(s.x, -viewport.width);
        assert_eq!(s.x + s.w, 2.0 * viewport.width);
        // The whole visible width still lies within the painted rect.
        assert!(s.x <= 0.0 && s.x + s.w >= viewport.width);
    }

    /// The full left-to-right scan `visible_slices` is checked against: every
    /// slice up to the right cull bound is tested, with no end-max lower bound.
    fn visible_slices_full_scan(
        profile: &bviz_trace_profile::Profile,
        layout: &LaneLayout,
        cull: Cull,
    ) -> Vec<VisibleSlice> {
        let Cull {
            view,
            viewport,
            metrics,
            min_px: _,
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
            let slices = profile.slices(&profile.threads()[lane.track]);
            let upper = slices.partition_point(|s| s.start_us as f64 <= view_end_us);
            for (slice, s) in slices[..upper].iter().enumerate() {
                crate::cull::emit_slice(s, lane.track, slice, cull, rows_top, &mut covered, &mut out);
            }
        }
        out
    }

    #[test]
    fn indexed_cull_matches_full_scan() {
        const MIN_PX: f32 = 4.0;
        let p = profile(vec![
            track(
                1,
                LaneClass::Execution,
                vec![
                    // A span covering the whole timeline: crosses every view.
                    slice("span", "general information", 0, 1_000_000),
                    // Short slices far left of the mid/tail views.
                    slice("left1", "general information", 1_000, 10),
                    slice("left2", "general information", 5_000, 10),
                    // A tiny slice too far left to be saved by widening, and
                    // one just left of the mid view's start that appears only
                    // via min-px widening.
                    slice("tiny_far", "general information", 499_990, 2),
                    slice("tiny_kept", "general information", 499_998, 1),
                    // Slices inside the mid view, one wide and one a marker.
                    slice("in1", "general information", 500_010, 100),
                    slice("in2", "general information", 500_150, 2),
                    slice("tail", "general information", 999_900, 50),
                ],
            ),
            // A lane where the mid view's left bound lands strictly inside the
            // slice list — early slices culled by the end-max bound, later ones
            // emitted — so an emitted index not offset by that bound would name
            // the wrong slice.
            track(
                2,
                LaneClass::Analysis,
                vec![
                    slice("b1", "general information", 1_000, 50),
                    slice("b2", "general information", 2_000, 5),
                    slice("b4", "general information", 499_997, 1),
                    slice("b3", "general information", 500_100, 80),
                ],
            ),
        ]);
        let layout = lay_out_lanes(&p, &METRICS);
        let viewport = Viewport {
            width: 300.0,
            height: 200.0,
            scroll_y: 0.0,
        };
        let views = [
            ("fitted", TimeView::fit(p.span(), viewport.width, 24.0)),
            ("mid", TimeView { start_us: 500_000.0, px_per_us: 1.0 }),
            ("mid coarse", TimeView { start_us: 490_000.0, px_per_us: 0.01 }),
            ("mid deep", TimeView { start_us: 500_000.0, px_per_us: 50.0 }),
            ("tail", TimeView { start_us: 999_800.0, px_per_us: 1.0 }),
            ("before content", TimeView { start_us: -50_000.0, px_per_us: 1.0 }),
        ];
        for (label, view) in views {
            let cull = Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: MIN_PX };
            let indexed = visible_slices(&p, &layout, cull);
            let full = visible_slices_full_scan(&p, &layout, cull);
            assert_eq!(indexed, full, "view {label}");
        }
        // The mid view exercises every class: the crossing span, the widened
        // tiny slice, and the interior slices survive; everything ending left
        // of the widening margin is culled.
        let (_, mid) = views[1];
        let names: Vec<&str> = visible_slices(
            &p,
            &layout,
            Cull { view: &mid, viewport: &viewport, metrics: &METRICS, min_px: MIN_PX },
        )
        .iter()
            .map(|v| p.str(p.slices(&p.threads()[v.track])[v.slice].name))
            .collect();
        assert_eq!(names, ["span", "tiny_kept", "in1", "in2", "b4", "b3"]);
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
        assert!(
            visible_slices(
                &p,
                &layout,
                Cull { view: &view, viewport: &viewport, metrics: &METRICS, min_px: 0.5 },
            )
            .is_empty()
        );
    }
}
