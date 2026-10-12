use crate::chart::{
    CounterUnit, ValueScale, clip_polyline_x, collapse_runs, downsample_columns, format_counter,
    format_time, nice_step, sample_at, step_vertices, unit_for,
};
use crate::regions::{GraphLane, Regions};
use crate::theme::{self, Palette};
use crate::{
    CROSSHAIR_DOT_RADIUS, CROSSHAIR_LABEL_GAP, CROSSHAIR_VALUE_SIZE, GUTTER_LABEL_SIZE, METRICS,
    RULER_TARGET_TICK_PX, SLICE_TEXT_PAD,
};
use bviz_trace_layout::{TimeView, WorkingLanes};
use bviz_trace_profile::{CounterPoint, Profile};
use gpui::{
    App, BorderStyle, Bounds, ContentMask, Hsla, Path, Pixels, Point, TextAlign, Window, fill,
    point, px, quad, size,
};

const WORKING_LANES_NAME: &str = "working lanes";

/// What the chart of a track says while its series is on the way.
const PENDING_LABEL: &str = "Counting…";

/// How much of a series' fill is left over the range of values a step stands
/// for, between the least of them and the most.
const RANGE_FILL_OPACITY: f32 = 0.45;

/// A track of the counters strip.
enum StripTrack {
    /// The profile's [`WorkingLanes`]. It leads the strip, ahead of the action
    /// counts, the recorded measure of how much runs at once.
    WorkingLanes,
    /// The recorded counter at this place in [`Profile::counters`].
    Recorded(usize),
}

impl StripTrack {
    fn at(track: usize) -> StripTrack {
        match track.checked_sub(1) {
            None => StripTrack::WorkingLanes,
            Some(recorded) => StripTrack::Recorded(recorded),
        }
    }
}

/// How many tracks the counters strip of `profile` holds: the working lanes'
/// and each recorded counter's.
pub(crate) fn track_count(profile: &Profile) -> usize {
    1 + profile.counters().len()
}

pub(crate) fn track_name(profile: &Profile, track: usize) -> &str {
    match StripTrack::at(track) {
        StripTrack::WorkingLanes => WORKING_LANES_NAME,
        StripTrack::Recorded(at) => profile.str(profile.counters()[at].name),
    }
}

/// The series of a profile's counters strip, by their place in it.
#[derive(Clone, Copy)]
pub(crate) struct StripCounters<'a> {
    profile: &'a Profile,
    /// Absent until derived.
    working_lanes: Option<&'a WorkingLanes>,
}

/// What a counter track plots.
struct CounterSeriesView<'a> {
    /// Ascending by time, each value holding until the next point.
    points: &'a [CounterPoint],
    /// Where a point stands for a range of values of which it is the least,
    /// the most of each range, at the same instants.
    most: Option<&'a [CounterPoint]>,
    max_value: f64,
    color: Hsla,
    unit: CounterUnit,
}

impl<'a> StripCounters<'a> {
    pub(crate) fn new(profile: &'a Profile, working_lanes: Option<&'a WorkingLanes>) -> Self {
        StripCounters {
            profile,
            working_lanes,
        }
    }

    /// The series of the track at `track`, and `None` for a track with
    /// nothing to plot.
    fn series(self, track: usize) -> Option<CounterSeriesView<'a>> {
        let profile = self.profile;
        let view = match StripTrack::at(track) {
            StripTrack::WorkingLanes => {
                let lanes = self.working_lanes?;
                CounterSeriesView {
                    points: lanes.least(),
                    most: lanes.most(),
                    max_value: lanes.max(),
                    color: theme::counter_color(WORKING_LANES_NAME),
                    unit: CounterUnit::Lanes,
                }
            }
            StripTrack::Recorded(at) => {
                let counter = &profile.counters()[at];
                let series = profile.series(counter).first()?;
                CounterSeriesView {
                    points: profile.points(series),
                    most: None,
                    max_value: series.max_value,
                    color: theme::counter_color(profile.str(series.name)),
                    unit: unit_for(profile.str(counter.name)),
                }
            }
        };
        (view.max_value > 0.0).then_some(view)
    }
}

impl CounterSeriesView<'_> {
    /// What the crosshair reads at time `t` (microseconds): the value in
    /// effect there, or the least and the most of the values a step stands
    /// for, and `None` outside the series.
    fn readout(&self, t: f64) -> Option<Readout> {
        let least = sample_at(self.points, t)?;
        let most = self.most.and_then(|most| sample_at(most, t));
        Some(match most {
            Some(most) if most != least => Readout::Range(least, most),
            _ => Readout::Value(least),
        })
    }
}

/// A counter track's reading under the crosshair.
pub(crate) struct LaneReadout {
    y_scale: CounterYScale,
    readout: Readout,
    color: Hsla,
    unit: CounterUnit,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Readout {
    Value(f64),
    Range(f64, f64),
}

impl Readout {
    fn label(self, unit: CounterUnit) -> String {
        match self {
            Readout::Value(value) => format_counter(value, unit),
            Readout::Range(least, most) => format!(
                "{}–{}",
                format_counter(least, unit),
                format_counter(most, unit)
            ),
        }
    }

    /// The values a dot marks on the plot, lowest first.
    fn values(self) -> impl Iterator<Item = f64> {
        let (least, most) = match self {
            Readout::Value(value) => (value, None),
            Readout::Range(least, most) => (least, Some(most)),
        };
        std::iter::once(least).chain(most)
    }
}

/// How far a counter plot's drawn edge may be displaced, in device pixels, in
/// exchange for spending fewer vertices on it: half a device pixel, which the
/// raster cannot resolve.
const COUNTER_EDGE_SLACK_PX: f32 = 0.5;

/// Space between a crosshair value label and each side edge of its tag.
const READOUT_TAG_PAD: f32 = 2.0;

/// Maps a counter lane's values to y-positions in its chart area, so the plotted
/// line and the crosshair readout share one transform. The floor (value 0) sits
/// at `chart_bottom`; `max_value` reaches `chart_top`.
struct CounterYScale {
    chart_top: f32,
    chart_bottom: f32,
    max_value: f64,
    scale: ValueScale,
}

impl CounterYScale {
    fn new(lane_top: f32, chart_h: f32, series: &CounterSeriesView) -> Self {
        let chart_top = lane_top + METRICS.header_height;
        Self {
            chart_top,
            chart_bottom: chart_top + chart_h,
            max_value: series.max_value,
            scale: series.unit.value_scale(),
        }
    }

    fn value_to_y(&self, value: f64) -> f32 {
        let chart_h = self.chart_bottom - self.chart_top;
        self.chart_bottom - self.scale.height_of(value, self.max_value) * chart_h
    }
}

/// Paints the counters strip, scrolled up by `scroll` pixels: every counter lane
/// meeting the strip. Each lane is a header band over a chart area that holds the
/// lane's series as a filled step area scaled to the chart height and the series'
/// peak, and a lighter fill over the range of values wherever a step stands for
/// one. Charts are confined to the content column and the strip's own band, so a
/// lane crossing either edge is cut off there. Work per lane is bounded by the
/// viewport: the visible points are found by binary search, and dense runs are
/// collapsed to per-pixel-column extremes and then thinned to the edge slack
/// before they are filled, so a chart costs vertices in proportion to the detail
/// it can show rather than to the samples behind it.
pub(crate) fn paint_graph(
    counters: StripCounters,
    tv: TimeView,
    regions: Regions,
    scroll: f32,
    palette: &Palette,
    window: &mut Window,
) {
    if !regions.shows_counters() {
        return;
    }
    let edge_slack = COUNTER_EDGE_SLACK_PX / window.scale_factor();
    let content_mask = ContentMask {
        bounds: regions.counters_rect(),
    };
    window.with_content_mask(Some(content_mask), |window| {
        for gl in regions.counter_lanes(scroll) {
            let band = Bounds::new(
                point(px(regions.content_left), px(gl.top)),
                size(px(regions.time_w), px(METRICS.header_height)),
            );
            window.paint_quad(fill(band, palette.header_band));

            let Some(series) = counters.series(gl.lane) else {
                continue;
            };
            let y_scale = CounterYScale::new(gl.top, gl.chart_h, &series);
            let boundary = |points| plot_boundary(points, tv, regions, &y_scale, edge_slack);
            let mut paint = |points, color| {
                if let Some(top) = boundary(points) {
                    paint_area(&top, y_scale.chart_bottom, color, window);
                }
            };
            match series.most {
                None => paint(series.points, series.color),
                Some(most) => {
                    let range = series.color.opacity(RANGE_FILL_OPACITY);
                    paint(most, range);
                    let over = Hsla {
                        a: alpha_over(range.a, series.color.a),
                        ..series.color
                    };
                    paint(series.points, over);
                }
            }
        }
    });
}

/// The alpha of a fill that, laid over one of its own colour at alpha `under`,
/// covers as a single fill at alpha `whole` does.
fn alpha_over(under: f32, whole: f32) -> f32 {
    1.0 - (1.0 - whole) / (1.0 - under)
}

/// The boundary a step series plots as, clipped to the content column: `None`
/// where fewer than two of its vertices are in view.
fn plot_boundary(
    points: &[CounterPoint],
    tv: TimeView,
    regions: Regions,
    y_scale: &CounterYScale,
    edge_slack: f32,
) -> Option<Vec<(f32, f32)>> {
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
        return None;
    }
    let boundary = downsample_columns(step_vertices(
        visible
            .iter()
            .map(|p| (x_of(p.ts_us), y_scale.value_to_y(p.value))),
    ));
    let boundary = collapse_runs(&boundary, edge_slack);
    let top = clip_polyline_x(&boundary, clip_lo, clip_hi);
    (top.len() >= 2).then_some(top)
}

/// The texture coordinates of a path triangle that has no curved edge.
const STRAIGHT_EDGED: (Point<f32>, Point<f32>, Point<f32>) = {
    let solid = Point { x: 0., y: 1. };
    (solid, solid, solid)
};

/// Fills the area between `top`, whose x never decreases, and the chart floor
/// at `floor_y`, as a trapezoid under each of its segments: a cost in
/// proportion to its vertices however jagged it is.
fn paint_area(top: &[(f32, f32)], floor_y: f32, color: Hsla, window: &mut Window) {
    let at = |(x, y): (f32, f32)| point(px(x), px(y));
    let mut area = Path::new(at(top[0]));
    let mut filled = false;
    for segment in top.windows(2) {
        let (from, to) = (segment[0], segment[1]);
        if to.0 <= from.0 {
            continue;
        }
        let (from_floor, to_floor) = (at((from.0, floor_y)), at((to.0, floor_y)));
        area.push_triangle((at(from), at(to), to_floor), STRAIGHT_EDGED);
        area.push_triangle((at(from), to_floor, from_floor), STRAIGHT_EDGED);
        filled = true;
    }
    if filled {
        window.paint_path(area, color);
    }
}

/// Paints [`PENDING_LABEL`] at the foot of the working lanes' chart, level
/// with its name in the gutter, with the strip scrolled up by `scroll` pixels.
pub(crate) fn paint_working_lanes_pending(
    regions: Regions,
    scroll: f32,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let content_mask = ContentMask {
        bounds: regions.counters_rect(),
    };
    window.with_content_mask(Some(content_mask), |window| {
        let working_lanes = regions
            .counter_lanes(scroll)
            .filter(|gl| matches!(StripTrack::at(gl.lane), StripTrack::WorkingLanes));
        for gl in working_lanes {
            paint_pending(&gl, regions, palette, window, cx);
        }
    });
}

/// Paints [`PENDING_LABEL`] at the foot of a lane's chart, level with the
/// lane's name in the gutter.
fn paint_pending(
    gl: &GraphLane,
    regions: Regions,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let font = bviz_ui_assets::text_metrics(window).font;
    let size = px(GUTTER_LABEL_SIZE);
    let line = bviz_ui_assets::shape_line(window, PENDING_LABEL, font, size, palette.label.into());
    let lane_bottom = gl.top + METRICS.header_height + gl.chart_h;
    let origin = point(
        px(regions.content_left + SLICE_TEXT_PAD),
        px(lane_bottom - GUTTER_LABEL_SIZE - SLICE_TEXT_PAD),
    );
    let _ = line.paint(origin, size, TextAlign::Left, None, window, cx);
}

/// Where the crosshair at `at` stands: its x, held to the time axis, and the
/// time there in microseconds.
fn crosshair_at(at: Point<Pixels>, tv: TimeView, regions: Regions) -> (f32, f64) {
    let x = f32::from(at.x).min(regions.time_right);
    (x, tv.t_of(x - regions.content_left))
}

/// The readings of the counter tracks meeting the strip, scrolled up by
/// `scroll` pixels, whose series cover the time under the crosshair at `at`.
pub(crate) fn counter_readouts(
    counters: StripCounters,
    tv: TimeView,
    regions: Regions,
    at: Point<Pixels>,
    scroll: f32,
) -> Vec<LaneReadout> {
    let (_, t) = crosshair_at(at, tv, regions);
    regions
        .counter_lanes(scroll)
        .filter_map(|gl| {
            let series = counters.series(gl.lane)?;
            Some(LaneReadout {
                readout: series.readout(t)?,
                y_scale: CounterYScale::new(gl.top, gl.chart_h, &series),
                color: series.color,
                unit: series.unit,
            })
        })
        .collect()
}

/// Paints the hover crosshair: a vertical line at the cursor x spanning both
/// sections and the hovered absolute time near the ruler, then `readouts`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_crosshair(
    readouts: &[LaneReadout],
    tv: TimeView,
    regions: Regions,
    at: Point<Pixels>,
    show_counters: bool,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let (cx_px, t) = crosshair_at(at, tv, regions);

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
    paint_counter_readouts(readouts, regions, cx_px, &font, palette, window, cx);
}

/// For each of `readouts`, paints a dot where the crosshair at `cx_px` meets
/// the lane's line, or one at each end of the range a step stands for, plus a
/// value label on a [`ReadoutTag`](theme::ReadoutTag). Scaling matches
/// [`paint_graph`].
fn paint_counter_readouts(
    readouts: &[LaneReadout],
    regions: Regions,
    cx_px: f32,
    font: &gpui::Font,
    palette: &Palette,
    window: &mut Window,
    cx: &mut App,
) {
    let content_mask = ContentMask {
        bounds: regions.counters_rect(),
    };
    window.with_content_mask(Some(content_mask), |window| {
        for lane in readouts {
            let y_scale = &lane.y_scale;
            let chart_top = y_scale.chart_top;
            let chart_bottom = y_scale.chart_bottom;
            let color = lane.color;
            // Left at the highest of the dots.
            let mut dot_y = chart_bottom;
            for value in lane.readout.values() {
                dot_y = y_scale.value_to_y(value);
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
            }

            // The label's top sits at its dot, clamped into the chart.
            let max_label_y = (chart_bottom - CROSSHAIR_VALUE_SIZE).max(chart_top);
            let label_y = dot_y.clamp(chart_top, max_label_y);
            let tag = palette.readout_tag(color);
            let value_line = bviz_ui_assets::shape_line(
                window,
                lane.readout.label(lane.unit),
                font.clone(),
                px(CROSSHAIR_VALUE_SIZE),
                tag.label,
            );
            let tag_w = f32::from(value_line.width()) + 2.0 * READOUT_TAG_PAD;
            let tag_x = crate::label_x(cx_px, tag_w, CROSSHAIR_LABEL_GAP, regions.time_right);
            window.paint_quad(fill(
                Bounds::new(
                    point(px(tag_x), px(label_y)),
                    size(px(tag_w), px(CROSSHAIR_VALUE_SIZE)),
                ),
                tag.fill,
            ));
            let _ = value_line.paint(
                point(px(tag_x + READOUT_TAG_PAD), px(label_y)),
                px(CROSSHAIR_VALUE_SIZE),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_trace_layout::TrackFlamer;
    use std::sync::Arc;

    /// Two execution lanes working over `[0, 100)` and `[50, 150)`, and one
    /// recorded counter.
    fn profile() -> Arc<Profile> {
        let json = r#"{"traceEvents":[
            {"name":"thread_name","ph":"M","tid":1,"args":{"name":"skyframe-evaluator-execution-1"}},
            {"name":"thread_name","ph":"M","tid":2,"args":{"name":"skyframe-evaluator-execution-2"}},
            {"cat":"general information","name":"a","ph":"X","ts":0,"dur":100,"tid":1},
            {"cat":"general information","name":"b","ph":"X","ts":50,"dur":100,"tid":2},
            {"name":"action count","ph":"C","ts":0,"tid":0,"args":{"action":3}},
            {"name":"action count","ph":"C","ts":100,"tid":0,"args":{"action":7}}
        ]}"#;
        Arc::new(
            Profile::from_bytes(json.as_bytes())
                .expect("parse test profile")
                .into_inner(),
        )
    }

    #[test]
    fn the_working_lanes_stand_ahead_of_the_recorded_counters() {
        let profile = profile();
        let lanes = TrackFlamer::new(&profile).working_lanes();
        let strip = StripCounters::new(&profile, Some(&lanes));
        assert_eq!(track_count(&profile), 2);
        assert_eq!(track_name(&profile, 0), WORKING_LANES_NAME);
        assert!(track_name(&profile, 1).starts_with("action count"));
        let recorded = strip.series(1).expect("the recorded counter's series");
        assert_eq!(recorded.max_value, 7.0);
        assert_eq!(sample_at(recorded.points, 50.0), Some(3.0));
    }

    #[test]
    fn the_working_lanes_read_as_a_whole_count_under_the_crosshair() {
        let profile = profile();
        let derived = TrackFlamer::new(&profile).working_lanes();
        let strip = StripCounters::new(&profile, Some(&derived));
        let lanes = strip.series(0).expect("the derived series");
        assert_eq!(lanes.max_value, 2.0);
        let label = |t| lanes.readout(t).map(|readout| readout.label(lanes.unit));
        assert_eq!(label(10.0).as_deref(), Some("1"));
        assert_eq!(label(75.0).as_deref(), Some("2"));
        assert_eq!(label(150.0).as_deref(), Some("0"));
        assert_eq!(label(151.0), None);
    }

    #[test]
    fn a_fill_laid_over_a_range_fill_covers_as_the_series_fill_alone_does() {
        let series = theme::counter_color(WORKING_LANES_NAME).a;
        let range = series * RANGE_FILL_OPACITY;
        let over = alpha_over(range, series);
        assert!(0.0 < over && over < series);
        let covered = 1.0 - (1.0 - range) * (1.0 - over);
        assert!((covered - series).abs() < 1e-6);
    }

    #[test]
    fn a_step_standing_for_several_counts_reads_as_their_range() {
        let point = |ts_us, value| CounterPoint { ts_us, value };
        let series = CounterSeriesView {
            points: &[point(0, 2.0), point(10, 5.0), point(20, 0.0)],
            most: Some(&[point(0, 7.0), point(10, 5.0), point(20, 0.0)]),
            max_value: 7.0,
            color: theme::counter_color(WORKING_LANES_NAME),
            unit: CounterUnit::Lanes,
        };
        let range = series.readout(4.0).expect("a step in the series");
        assert_eq!(range, Readout::Range(2.0, 7.0));
        assert_eq!(range.label(series.unit), "2–7");
        assert_eq!(range.values().collect::<Vec<_>>(), [2.0, 7.0]);
        // A step of one count reads as that count, whatever its neighbours.
        let whole = series.readout(12.0).expect("a step in the series");
        assert_eq!(whole.label(series.unit), "5");
        assert_eq!(whole.values().collect::<Vec<_>>(), [5.0]);
    }

    #[test]
    fn the_working_lanes_plot_nothing_until_derived() {
        let profile = profile();
        let strip = StripCounters::new(&profile, None);
        assert!(strip.series(0).is_none());
        assert!(strip.series(1).is_some());
    }
}
