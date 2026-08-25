use crate::chart::{
    clip_polyline_x, collapse_runs, downsample_columns, format_counter, format_time, nice_step,
    sample_at, step_vertices, unit_for,
};
use crate::regions::{Regions, graph_lanes};
use crate::theme::{self, Palette};
use crate::{
    CROSSHAIR_DOT_RADIUS, CROSSHAIR_LABEL_GAP, CROSSHAIR_VALUE_SIZE, METRICS, RULER_TARGET_TICK_PX,
};
use bviz_trace_layout::TimeView;
use bviz_trace_profile::Profile;
use gpui::{
    App, BorderStyle, Bounds, ContentMask, Hsla, PathBuilder, Pixels, Point, TextAlign, Window,
    fill, point, px, quad, size,
};

/// How far a counter plot's drawn edge may be displaced, in device pixels, in
/// exchange for spending fewer vertices on it: half a device pixel, which the
/// raster cannot resolve.
const COUNTER_EDGE_SLACK_PX: f32 = 0.5;

/// Maps a counter lane's values to y-positions in its chart area, so the plotted
/// line and the crosshair readout share one transform. The floor (value 0) sits
/// at `chart_bottom`; `max_value` reaches `chart_top`.
struct CounterYScale {
    chart_top: f32,
    chart_bottom: f32,
    scale: f32,
}

impl CounterYScale {
    fn new(lane_top: f32, chart_h: f32, max_value: f64) -> Self {
        let chart_top = lane_top + METRICS.header_height;
        Self {
            chart_top,
            chart_bottom: chart_top + chart_h,
            scale: chart_h / max_value as f32,
        }
    }

    fn value_to_y(&self, value: f64) -> f32 {
        self.chart_bottom - value as f32 * self.scale
    }
}

/// Paints the counters strip: every counter lane fitted to the strip's
/// height. Each lane is a header band over a chart area that holds the lane's
/// series as a filled step area scaled to the chart height and the series' peak.
/// Charts are confined to the content column. Work per lane is bounded by the
/// viewport: the visible points are found by binary search, and dense runs are
/// collapsed to per-pixel-column extremes and then thinned to the edge slack
/// before tessellation, so a chart costs vertices in proportion to the detail it
/// can show rather than to the samples behind it.
pub(crate) fn paint_graph(
    profile: &Profile,
    tv: TimeView,
    regions: Regions,
    palette: &Palette,
    window: &mut Window,
) {
    if !regions.has_graph {
        return;
    }
    let counters = profile.counters();
    let lanes = graph_lanes(counters.len(), regions.counters_top, regions.counters_h());
    let edge_slack = COUNTER_EDGE_SLACK_PX / window.scale_factor();
    let content_mask = ContentMask {
        bounds: Bounds::new(
            point(px(regions.content_left), px(regions.counters_top)),
            size(px(regions.time_w), px(regions.counters_h())),
        ),
    };
    for gl in lanes {
        let counter = &counters[gl.lane];
        let band = Bounds::new(
            point(px(regions.content_left), px(gl.top)),
            size(px(regions.time_w), px(METRICS.header_height)),
        );
        window.paint_quad(fill(band, palette.header_band));
        if gl.chart_h <= 0.0 {
            continue;
        }

        let Some(series) = profile.series(counter).first() else {
            continue;
        };
        if series.max_value <= 0.0 {
            continue;
        }
        let points = profile.points(series);
        if points.len() < 2 {
            continue;
        }
        let y_scale = CounterYScale::new(gl.top, gl.chart_h, series.max_value);
        let chart_bottom = y_scale.chart_bottom;
        let x_of = |ts| regions.content_left + tv.x_of(ts);
        // Clip the series to a margin around the content column so off-screen
        // vertices at high zoom stay near the window and the renderer keeps the
        // filled area's precision; the margin exceeds the window, so the visible
        // plot is unchanged.
        let clip_lo = regions.content_left - regions.time_w;
        let clip_hi = regions.content_left + 2.0 * regions.time_w;
        // The points that can reach the clip strip: from the last point at or
        // before its left edge to the first at or after its right edge.
        let t_lo = tv.t_of(clip_lo - regions.content_left);
        let t_hi = tv.t_of(clip_hi - regions.content_left);
        let start = points
            .partition_point(|p| (p.ts_us as f64) < t_lo)
            .saturating_sub(1);
        let end = (points.partition_point(|p| (p.ts_us as f64) < t_hi) + 1).min(points.len());
        let visible = &points[start..end];
        if visible.len() < 2 {
            continue;
        }
        let boundary = downsample_columns(step_vertices(
            visible
                .iter()
                .map(|p| (x_of(p.ts_us), y_scale.value_to_y(p.value))),
        ));
        let boundary = collapse_runs(&boundary, edge_slack);
        let top = clip_polyline_x(&boundary, clip_lo, clip_hi);
        if top.len() < 2 {
            continue;
        }
        window.with_content_mask(Some(content_mask), |window| {
            let mut area = PathBuilder::fill();
            area.move_to(point(px(top[0].0), px(top[0].1)));
            for &(x, y) in &top[1..] {
                area.line_to(point(px(x), px(y)));
            }
            // The area sits on the chart floor, so the base is two corners.
            area.line_to(point(px(top[top.len() - 1].0), px(chart_bottom)));
            area.line_to(point(px(top[0].0), px(chart_bottom)));
            if let Ok(path) = area.build() {
                window.paint_path(path, theme::counter_color(profile.str(series.name)));
            }
        });
    }
}

/// Paints the hover crosshair: a vertical line at the cursor x spanning both
/// sections and the hovered absolute time near the ruler, then the per-lane
/// counter readouts.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_crosshair(
    profile: &Profile,
    tv: TimeView,
    regions: Regions,
    at: Point<Pixels>,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let cx_px = f32::from(at.x).min(regions.time_right);
    let t = tv.t_of(cx_px - regions.content_left);

    let crosshair_line = |top: f32, height: f32, window: &mut Window| {
        window.paint_quad(fill(
            Bounds::new(point(px(cx_px), px(top)), size(px(1.), px(height))),
            palette.crosshair,
        ));
    };
    crosshair_line(regions.flame_top, regions.flame_h(), window);
    // The counters strip shares the time axis, so the crosshair continues into it.
    if show_counters {
        crosshair_line(regions.counters_top, regions.counters_h(), window);
    }

    let font = bviz_ui_assets::text_metrics(window).font;

    // The hovered time, formatted like the ruler labels, just under the ruler.
    let step = nice_step(RULER_TARGET_TICK_PX / tv.px_per_us);
    let time_line = bviz_ui_assets::shape_line(
        window,
        format_time(t, step),
        font.clone(),
        px(CROSSHAIR_VALUE_SIZE),
        palette.label.into(),
    );
    let time_w = f32::from(time_line.width());
    let time_x = (cx_px + CROSSHAIR_LABEL_GAP).min(regions.time_right - time_w);
    let _ = time_line.paint(
        point(px(time_x), px(regions.flame_top + 1.)),
        px(CROSSHAIR_VALUE_SIZE),
        TextAlign::Left,
        None,
        window,
        cx,
    );

    if !show_counters {
        return;
    }
    paint_counter_readouts(profile, regions, cx_px, t, &font, palette, window, cx);
}

/// For each counter lane whose series covers time `t` (microseconds), paints a
/// dot where the crosshair at `cx_px` meets the lane's line plus a value label.
/// Scaling matches [`paint_graph`].
#[allow(clippy::too_many_arguments)]
fn paint_counter_readouts(
    profile: &Profile,
    regions: Regions,
    cx_px: f32,
    t: f64,
    font: &gpui::Font,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let counters = profile.counters();
    let lanes = graph_lanes(counters.len(), regions.counters_top, regions.counters_h());
    for gl in lanes {
        if gl.chart_h <= 0.0 {
            continue;
        }
        let counter = &counters[gl.lane];
        let Some(series) = profile.series(counter).first() else {
            continue;
        };
        if series.max_value <= 0.0 {
            continue;
        }
        let Some(value) = sample_at(profile.points(series), t) else {
            continue;
        };
        let y_scale = CounterYScale::new(gl.top, gl.chart_h, series.max_value);
        let chart_top = y_scale.chart_top;
        let chart_bottom = y_scale.chart_bottom;
        let dot_y = y_scale.value_to_y(value);
        let color = theme::counter_color(profile.str(series.name));
        window.paint_quad(quad(
            Bounds::new(
                point(
                    px(cx_px - CROSSHAIR_DOT_RADIUS),
                    px(dot_y - CROSSHAIR_DOT_RADIUS),
                ),
                size(
                    px(2.0 * CROSSHAIR_DOT_RADIUS),
                    px(2.0 * CROSSHAIR_DOT_RADIUS),
                ),
            ),
            px(CROSSHAIR_DOT_RADIUS),
            color,
            px(0.),
            Hsla::from(palette.border),
            BorderStyle::Solid,
        ));

        // The label's top sits at its dot, clamped into the chart.
        let max_label_y = (chart_bottom - CROSSHAIR_VALUE_SIZE).max(chart_top);
        let label_y = dot_y.clamp(chart_top, max_label_y);
        let unit = unit_for(profile.str(counter.name));
        let value_line = bviz_ui_assets::shape_line(
            window,
            format_counter(value, unit),
            font.clone(),
            px(CROSSHAIR_VALUE_SIZE),
            bviz_ui_theme::opaque(color),
        );
        let value_w = f32::from(value_line.width());
        let value_x = crate::label_x(cx_px, value_w, CROSSHAIR_LABEL_GAP, regions.time_right);
        let _ = value_line.paint(
            point(px(value_x), px(label_y)),
            px(CROSSHAIR_VALUE_SIZE),
            TextAlign::Left,
            None,
            window,
            cx,
        );
    }
}
