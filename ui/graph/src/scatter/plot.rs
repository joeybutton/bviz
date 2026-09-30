//! Where the scatter plot's marks and ticks fall: counts on logarithmic axes,
//! and a mark's size by its fan-in.

use bviz_graph_layout::metrics::Metrics;
use bviz_graph_layout::select::Verdict;
use bviz_graph_layout::{Id, Kind};
use gpui::{Bounds, Pixels, Point, point, px};

/// A count axis scaled by `log10(1 + n)`, so a count of zero sits at the origin
/// and every decade past it spans the same length.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct LogScale {
    /// The count at the far end of the axis: the first step of the 1-2-5
    /// sequence at or past every count plotted on it.
    top: u32,
}

/// A mark on a [`LogScale`]. The majors are zero and the powers of ten; the
/// minors fall at twice and five times each power.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Tick {
    pub(crate) value: u32,
    pub(crate) major: bool,
}

/// The multiples of each power of ten a tick falls on.
const TICK_STEPS: [u32; 3] = [1, 2, 5];

impl LogScale {
    /// An axis reaching at least `max`, and one step even where `max` is zero.
    pub(crate) fn covering(max: u32) -> Self {
        let top = decades()
            .flat_map(|decade| TICK_STEPS.map(|step| decade.checked_mul(step)))
            .map_while(|value| value)
            .find(|&value| value >= max)
            .unwrap_or(u32::MAX);
        Self { top }
    }

    /// Where `n` falls along the axis, from 0 at the origin to 1 at its far end.
    pub(crate) fn fraction(self, n: u32) -> f32 {
        (log_count(n) / log_count(self.top)) as f32
    }

    /// The narrowest share of the axis a zoom shows.
    fn narrowest(self) -> f32 {
        (MIN_SPAN_DECADES / log_count(self.top)).min(1.0) as f32
    }

    /// The ticks from zero to the far end, ascending.
    pub(crate) fn ticks(self) -> Vec<Tick> {
        let mut ticks = vec![Tick {
            value: 0,
            major: true,
        }];
        for decade in decades() {
            for step in TICK_STEPS {
                match decade.checked_mul(step).filter(|&value| value <= self.top) {
                    Some(value) => ticks.push(Tick {
                        value,
                        major: step == 1,
                    }),
                    None => return ticks,
                }
            }
        }
        ticks
    }
}

/// The powers of ten a `u32` holds, ascending.
fn decades() -> impl Iterator<Item = u32> {
    std::iter::successors(Some(1u32), |&decade| decade.checked_mul(10))
}

fn log_count(n: u32) -> f64 {
    (f64::from(n) + 1.0).log10()
}

/// How a tick's count reads: a thousand and a million shortened to `k` and `M`.
pub(crate) fn tick_label(value: u32) -> String {
    const THOUSAND: u32 = 1_000;
    const MILLION: u32 = 1_000_000;
    match value {
        v if v >= MILLION && v % MILLION == 0 => format!("{}M", v / MILLION),
        v if v >= THOUSAND && v % THOUSAND == 0 => format!("{}k", v / THOUSAND),
        v => v.to_string(),
    }
}

/// The radius of the mark for a target of no fan-in.
pub(crate) const MIN_RADIUS: f32 = 2.5;
/// The radius of the mark for the target of the largest fan-in.
pub(crate) const MAX_RADIUS: f32 = 8.0;

/// One target as the plot draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Mark {
    pub(crate) target: Metrics,
    pub(crate) radius: f32,
    /// Whether the filter draws the target as no part of what it selects.
    pub(crate) faded: bool,
}

/// The marks a set of targets draws as, and the axes that hold them. The axes
/// and the mark sizes are scaled to every target measured, drawn or not, so a
/// filter changes which marks are drawn and never where one falls.
pub(crate) struct Plotted {
    /// The faded before the rest, so none hides a mark the filter selects, and
    /// largest first within each, so a small mark is drawn over any large one
    /// it overlaps.
    pub(crate) marks: Vec<Mark>,
    /// How many targets were measured, of which `marks` draws those the filter
    /// does not hide.
    pub(crate) measured: usize,
    /// Closure along the horizontal axis.
    pub(crate) x: LogScale,
    /// Blast radius along the vertical axis.
    pub(crate) y: LogScale,
    /// The kinds of target among the marks, in enum order.
    pub(crate) kinds: Vec<Kind>,
}

impl Plotted {
    /// The marks of `targets` as the filter whose verdict on a node `verdict`
    /// gives draws them.
    pub(crate) fn of(targets: &[Metrics], verdict: impl Fn(Id) -> Verdict) -> Self {
        let max = |measure: fn(&Metrics) -> u32| targets.iter().map(measure).max();
        let largest_fan_in = max(|t| t.fan_in).unwrap_or(0);
        let mut marks: Vec<Mark> = Vec::with_capacity(targets.len());
        marks.extend(targets.iter().filter_map(|&target| {
            let faded = match verdict(target.id) {
                Verdict::Hidden => return None,
                Verdict::Faded => true,
                // The graph draws a target kept only to frame what it holds as
                // it draws one the filter selects.
                Verdict::Shown | Verdict::Context => false,
            };
            Some(Mark {
                target,
                radius: radius(target.fan_in, largest_fan_in),
                faded,
            })
        }));
        marks.sort_by_key(|mark| (!mark.faded, std::cmp::Reverse(mark.target.fan_in)));
        let mut kinds: Vec<Kind> = targets.iter().map(|t| t.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        Self {
            kinds,
            marks,
            measured: targets.len(),
            x: LogScale::covering(max(|t| t.closure.value()).unwrap_or(0)),
            y: LogScale::covering(max(|t| t.blast_radius.value()).unwrap_or(0)),
        }
    }
}

/// The radius of the mark for a target of `fan_in` direct dependents, where the
/// largest has `largest`. Its area grows with the logarithm of the fan-in, so the
/// few heavily depended-on targets stand out without burying the rest.
fn radius(fan_in: u32, largest: u32) -> f32 {
    let share = match largest {
        0 => 0.0,
        _ => (log_count(fan_in) / log_count(largest)).sqrt() as f32,
    };
    MIN_RADIUS + (MAX_RADIUS - MIN_RADIUS) * share
}

/// The narrowest stretch of an axis a zoom shows, in decades of `1 + n`:
/// twice the widest gap between neighbouring ticks, `log10(5 / 2)`, which the
/// step from twice to five times a power of ten approaches, so at least two
/// ticks are always in view.
const MIN_SPAN_DECADES: f64 = 2.0 * (1.0 - 2.0 * std::f64::consts::LOG10_2);

/// The part of an axis the plot shows, from `lo` for `len`, both as fractions
/// of the whole axis from its origin: always within the axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Span {
    lo: f32,
    len: f32,
}

impl Span {
    pub(crate) const WHOLE: Span = Span { lo: 0.0, len: 1.0 };

    fn new(lo: f32, len: f32) -> Self {
        let len = len.min(1.0);
        Span {
            lo: lo.clamp(0.0, 1.0 - len),
            len,
        }
    }

    /// Where the axis fraction `f` falls across the span, from 0 at its start
    /// to 1 at its end.
    fn across(self, f: f32) -> f32 {
        (f - self.lo) / self.len
    }

    /// The span `factor` times narrower, a factor below one widening it, but
    /// no narrower than `narrowest`, about the point `at` across it, which
    /// stays where it was.
    fn zoomed(self, at: f32, factor: f32, narrowest: f32) -> Self {
        let len = (self.len / factor).clamp(narrowest.min(self.len), 1.0);
        Span::new(self.lo + at * (self.len - len), len)
    }

    /// The span slid `by` of its own length toward the axis's far end.
    fn panned(self, by: f32) -> Self {
        Span::new(self.lo + by * self.len, self.len)
    }
}

/// The part of each axis the plot shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Zoom {
    pub(crate) x: Span,
    pub(crate) y: Span,
}

impl Zoom {
    pub(crate) const WHOLE: Zoom = Zoom {
        x: Span::WHOLE,
        y: Span::WHOLE,
    };
}

/// The region counts are placed in, inset from the axes by the largest mark's
/// radius so that, at the whole of each axis, no mark crosses them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DataArea {
    bounds: Bounds<Pixels>,
    /// The frame the axes are drawn along, outside which nothing is drawn.
    frame: Bounds<Pixels>,
    x: LogScale,
    y: LogScale,
    zoom: Zoom,
}

impl DataArea {
    /// The data region within the frame `axes` draws its axes along, showing
    /// the part of each axis `zoom` names.
    pub(crate) fn within(axes: Bounds<Pixels>, x: LogScale, y: LogScale, zoom: Zoom) -> Self {
        let inset = px(MAX_RADIUS);
        let bounds = Bounds::from_corners(
            point(axes.left() + inset, axes.top() + inset),
            point(axes.right() - inset, axes.bottom() - inset),
        );
        Self {
            bounds,
            frame: axes,
            x,
            y,
            zoom,
        }
    }

    pub(crate) fn frame(&self) -> Bounds<Pixels> {
        self.frame
    }

    /// The horizontal position of a closure of `n`.
    pub(crate) fn x_of(&self, n: u32) -> Pixels {
        let across = self.zoom.x.across(self.x.fraction(n));
        self.bounds.left() + self.bounds.size.width * across
    }

    /// The vertical position of a blast radius of `n`, rising up the plot.
    pub(crate) fn y_of(&self, n: u32) -> Pixels {
        let across = self.zoom.y.across(self.y.fraction(n));
        self.bounds.bottom() - self.bounds.size.height * across
    }

    /// Where a closure of `n` falls, where that is within the frame.
    pub(crate) fn visible_x(&self, n: u32) -> Option<Pixels> {
        let x = self.x_of(n);
        (self.frame.left()..=self.frame.right())
            .contains(&x)
            .then_some(x)
    }

    /// Where a blast radius of `n` falls, where that is within the frame.
    pub(crate) fn visible_y(&self, n: u32) -> Option<Pixels> {
        let y = self.y_of(n);
        (self.frame.top()..=self.frame.bottom())
            .contains(&y)
            .then_some(y)
    }

    /// The zoom `factor` times closer, a factor below one farther out, about
    /// the point under `at`, which stays under it.
    pub(crate) fn zoomed(&self, at: Point<Pixels>, factor: f32) -> Zoom {
        let across_x = (at.x - self.bounds.left()) / self.bounds.size.width;
        let across_y = (self.bounds.bottom() - at.y) / self.bounds.size.height;
        Zoom {
            x: self.zoom.x.zoomed(across_x, factor, self.x.narrowest()),
            y: self.zoom.y.zoomed(across_y, factor, self.y.narrowest()),
        }
    }

    /// The zoom carried along with a drag by `by`, so the counts under the
    /// pointer follow it.
    pub(crate) fn panned(&self, by: Point<Pixels>) -> Zoom {
        Zoom {
            x: self.zoom.x.panned(-(by.x / self.bounds.size.width)),
            y: self.zoom.y.panned(by.y / self.bounds.size.height),
        }
    }

    pub(crate) fn place(&self, target: &Metrics) -> Point<Pixels> {
        point(
            self.x_of(target.closure.value()),
            self.y_of(target.blast_radius.value()),
        )
    }

    /// The mark of `marks` a pointer at `at` picks: the one whose centre is
    /// nearest, among those whose rim lies within [`PICK_SLOP`] of it and whose
    /// centre lies within the frame, and the one drawn over the others among
    /// equally near ones.
    pub(crate) fn pick(&self, marks: &[Mark], at: Point<Pixels>) -> Option<Picked> {
        let mut nearest: Option<(usize, f32)> = None;
        for (index, mark) in marks.iter().enumerate() {
            let center = self.place(&mark.target);
            let distance = f32::from(center.x - at.x).hypot(f32::from(center.y - at.y));
            let reaches = distance <= mark.radius + PICK_SLOP && self.frame.contains(&center);
            if reaches && nearest.is_none_or(|(_, best)| distance <= best) {
                nearest = Some((index, distance));
            }
        }
        let (index, _) = nearest?;
        let at_same_place = |mark: &Mark| {
            let (one, other) = (&mark.target, &marks[index].target);
            let place = |m: &Metrics| (m.closure.value(), m.blast_radius.value());
            place(one) == place(other)
        };
        Some(Picked {
            mark: index,
            coincident: marks.iter().filter(|mark| at_same_place(mark)).count() - 1,
        })
    }
}

/// How far past a mark's rim a pointer still picks it, so that the smallest
/// marks are not a pixel-hunt.
pub(crate) const PICK_SLOP: f32 = 4.0;

/// The mark a pointer picks, by its index among the plotted marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Picked {
    pub(crate) mark: usize,
    /// How many other marks stand for targets at exactly its place, which it
    /// hides.
    pub(crate) coincident: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::metrics::Count;
    use gpui::size;

    fn target(closure: u32, blast_radius: u32, fan_in: u32) -> Metrics {
        Metrics {
            id: Id(closure),
            kind: Kind::Target,
            closure: Count::Exact(closure),
            blast_radius: Count::Exact(blast_radius),
            fan_in,
            fan_out: 0,
            actions: 0,
        }
    }

    /// The marks of `targets` through a filter drawing every one.
    fn shown(targets: &[Metrics]) -> Plotted {
        Plotted::of(targets, |_| Verdict::Shown)
    }

    #[test]
    fn an_axis_ends_on_the_first_step_past_its_largest_count() {
        let top = |max| LogScale::covering(max).top;
        assert_eq!(
            [top(0), top(1), top(3), top(20), top(21), top(999)],
            [1, 1, 5, 20, 50, 1_000]
        );
        assert_eq!(top(u32::MAX), u32::MAX);
    }

    #[test]
    fn zero_sits_at_the_origin_and_the_top_at_the_end() {
        let scale = LogScale::covering(700);
        assert_eq!(scale.fraction(0), 0.0);
        assert_eq!(scale.fraction(1_000), 1.0);
        let decade = |n: u32| scale.fraction(n * 10 - 1) - scale.fraction(n - 1);
        assert!(
            (decade(10) - decade(100)).abs() < 1e-6,
            "decades differ in length"
        );
    }

    #[test]
    fn ticks_mark_each_decade_with_minors_between() {
        let ticks: Vec<(u32, bool)> = LogScale::covering(150)
            .ticks()
            .into_iter()
            .map(|t| (t.value, t.major))
            .collect();
        assert_eq!(
            ticks,
            [
                (0, true),
                (1, true),
                (2, false),
                (5, false),
                (10, true),
                (20, false),
                (50, false),
                (100, true),
                (200, false),
            ]
        );
    }

    #[test]
    fn tick_labels_shorten_round_thousands_and_millions() {
        let labels: Vec<String> = [0, 5, 100, 1_000, 20_000, 2_000_000, 1_500]
            .map(tick_label)
            .into();
        assert_eq!(labels, ["0", "5", "100", "1k", "20k", "2M", "1500"]);
    }

    #[test]
    fn marks_grow_with_fan_in_within_their_bounds_and_draw_largest_first() {
        let plotted = shown(&[target(1, 1, 0), target(1, 1, 100), target(1, 1, 3)]);
        let radii: Vec<f32> = plotted.marks.iter().map(|m| m.radius).collect();
        assert_eq!(radii.first(), Some(&MAX_RADIUS));
        assert_eq!(radii.last(), Some(&MIN_RADIUS));
        assert!(radii.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn no_fan_in_anywhere_draws_every_mark_at_the_smallest_size() {
        let plotted = shown(&[target(0, 0, 0), target(4, 2, 0)]);
        assert!(plotted.marks.iter().all(|m| m.radius == MIN_RADIUS));
    }

    #[test]
    fn every_mark_stays_clear_of_the_axes() {
        let targets = [target(0, 0, 9), target(4_000, 17, 9), target(3, 90_000, 9)];
        let plotted = shown(&targets);
        let axes = Bounds::new(point(px(40.), px(10.)), size(px(300.), px(200.)));
        let area = DataArea::within(axes, plotted.x, plotted.y, Zoom::WHOLE);
        for mark in &plotted.marks {
            let at = area.place(&mark.target);
            let r = px(mark.radius);
            assert!(
                at.x - r >= axes.left() && at.x + r <= axes.right(),
                "{at:?}"
            );
            assert!(
                at.y - r >= axes.top() && at.y + r <= axes.bottom(),
                "{at:?}"
            );
        }
    }

    #[test]
    fn a_pointer_picks_the_nearest_mark_within_reach_and_counts_those_it_hides() {
        let targets = [target(0, 0, 0), target(100, 100, 0), target(100, 100, 5)];
        let plotted = shown(&targets);
        let axes = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(300.)));
        let area = DataArea::within(axes, plotted.x, plotted.y, Zoom::WHOLE);
        let origin = area.place(&targets[0]);
        let picked = area.pick(&plotted.marks, origin + point(px(1.), px(-1.)));
        let origin_mark = plotted.marks.iter().position(|m| m.target == targets[0]);
        assert_eq!(picked.map(|p| p.mark), origin_mark);
        assert_eq!(picked.map(|p| p.coincident), Some(0));

        let shared = area.pick(&plotted.marks, area.place(&targets[1]));
        // The mark drawn last is the one on top.
        let top = plotted
            .marks
            .iter()
            .rposition(|m| (m.target.closure.value(), m.target.blast_radius.value()) == (100, 100));
        assert_eq!(shared.map(|p| p.mark), top);
        assert_eq!(shared.map(|p| p.coincident), Some(1));

        let far = origin + point(px(MAX_RADIUS + PICK_SLOP + 1.), px(0.));
        assert_eq!(area.pick(&plotted.marks, far), None);
    }

    #[test]
    fn a_filter_draws_what_it_keeps_on_axes_scaled_to_every_target() {
        let targets = [target(3, 1, 9), target(400, 2, 0), target(7, 90, 2)];
        let hidden = targets[1].id;
        let faded = targets[2].id;
        let plotted = Plotted::of(&targets, |id| match id {
            id if id == hidden => Verdict::Hidden,
            id if id == faded => Verdict::Faded,
            _ => Verdict::Shown,
        });
        let drawn: Vec<(Id, bool)> = plotted
            .marks
            .iter()
            .map(|m| (m.target.id, m.faded))
            .collect();
        assert_eq!(drawn, [(faded, true), (targets[0].id, false)]);
        assert_eq!(plotted.measured, targets.len());
        let whole = shown(&targets);
        assert_eq!((plotted.x, plotted.y), (whole.x, whole.y));
        let radius_of = |plotted: &Plotted, id: Id| {
            plotted
                .marks
                .iter()
                .find(|m| m.target.id == id)
                .map(|m| m.radius)
        };
        assert_eq!(radius_of(&plotted, faded), radius_of(&whole, faded));
    }

    #[test]
    fn a_zoom_holds_the_point_it_is_taken_about_and_stays_within_the_axis() {
        const NARROWEST: f32 = 0.05;
        let span = Span::WHOLE.zoomed(0.25, 4.0, NARROWEST);
        assert!((span.len - 0.25).abs() < 1e-6);
        assert!((span.across(0.25) - 0.25).abs() < 1e-6);
        let wider = span.zoomed(0.5, 0.1, NARROWEST);
        assert_eq!(wider, Span::WHOLE);
        assert_eq!(Span::WHOLE.zoomed(0.5, 1e6, NARROWEST).len, NARROWEST);
        assert_eq!(span.panned(-10.0).lo, 0.0);
        assert!((span.panned(10.0).lo - 0.75).abs() < 1e-6);
    }

    #[test]
    fn the_deepest_zoom_keeps_two_ticks_in_view_anywhere_on_the_axis() {
        for top in [1, 7, 150, 90_000] {
            let scale = LogScale::covering(top);
            let values: Vec<f32> = scale
                .ticks()
                .iter()
                .map(|t| scale.fraction(t.value))
                .collect();
            let narrowest = scale.narrowest();
            let steps = 100;
            for step in 0..=steps {
                let lo = (1.0 - narrowest) * step as f32 / steps as f32;
                let in_view = values
                    .iter()
                    .filter(|&&f| f >= lo - 1e-6 && f <= lo + narrowest + 1e-6)
                    .count();
                assert!(in_view >= 2, "top {top}, from {lo}: {in_view} ticks");
            }
        }
    }

    #[test]
    fn a_zoomed_plot_keeps_the_counts_under_the_pointer_and_picks_none_it_clips() {
        let targets = [target(0, 0, 0), target(1_000, 1_000, 0), target(30, 30, 0)];
        let plotted = shown(&targets);
        let axes = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(300.)));
        let whole = DataArea::within(axes, plotted.x, plotted.y, Zoom::WHOLE);
        let at = whole.place(&targets[2]);
        let zoom = whole.zoomed(at, 3.0);
        let zoomed = DataArea::within(axes, plotted.x, plotted.y, zoom);
        let moved = zoomed.place(&targets[2]) - at;
        assert!(f32::from(moved.x).abs() < 0.01 && f32::from(moved.y).abs() < 0.01);
        let corner = zoomed.place(&targets[1]);
        assert!(!axes.contains(&corner));
        assert_eq!(zoomed.pick(&plotted.marks, corner), None);
    }

    #[test]
    fn more_blast_radius_places_a_mark_higher() {
        let plotted = shown(&[target(0, 0, 0), target(0, 50, 0)]);
        let axes = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.)));
        let area = DataArea::within(axes, plotted.x, plotted.y, Zoom::WHOLE);
        assert!(area.y_of(50) < area.y_of(0));
        assert_eq!(area.x_of(0), axes.left() + px(MAX_RADIUS));
    }
}
