//! Where the scatter plot's marks and ticks fall: counts on logarithmic axes,
//! and a mark's size by its fan-in.

use bviz_graph_layout::Kind;
use bviz_graph_layout::metrics::TargetMetrics;
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
    pub(crate) target: TargetMetrics,
    pub(crate) radius: f32,
}

/// The marks a set of targets draws as, and the axes that hold them.
pub(crate) struct Plotted {
    /// Largest first, so a small mark is drawn over any large one it overlaps.
    pub(crate) marks: Vec<Mark>,
    /// Closure along the horizontal axis.
    pub(crate) x: LogScale,
    /// Blast radius along the vertical axis.
    pub(crate) y: LogScale,
    /// The kinds of target among the marks, in enum order.
    pub(crate) kinds: Vec<Kind>,
}

impl Plotted {
    pub(crate) fn of(targets: &[TargetMetrics]) -> Self {
        let max = |measure: fn(&TargetMetrics) -> u32| targets.iter().map(measure).max();
        let largest_fan_in = max(|t| t.fan_in).unwrap_or(0);
        let mut marks: Vec<Mark> = targets
            .iter()
            .map(|&target| Mark {
                target,
                radius: radius(target.fan_in, largest_fan_in),
            })
            .collect();
        marks.sort_by_key(|mark| std::cmp::Reverse(mark.target.fan_in));
        let mut kinds: Vec<Kind> = targets.iter().map(|t| t.kind).collect();
        kinds.sort_unstable();
        kinds.dedup();
        Self {
            kinds,
            marks,
            x: LogScale::covering(max(|t| t.closure).unwrap_or(0)),
            y: LogScale::covering(max(|t| t.blast_radius).unwrap_or(0)),
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

/// The region counts are placed in, inset from the axes by the largest mark's
/// radius so no mark crosses them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DataArea {
    bounds: Bounds<Pixels>,
    x: LogScale,
    y: LogScale,
}

impl DataArea {
    /// The data region within the frame `axes` draws its axes along.
    pub(crate) fn within(axes: Bounds<Pixels>, x: LogScale, y: LogScale) -> Self {
        let inset = px(MAX_RADIUS);
        let bounds = Bounds::from_corners(
            point(axes.left() + inset, axes.top() + inset),
            point(axes.right() - inset, axes.bottom() - inset),
        );
        Self { bounds, x, y }
    }

    /// The horizontal position of a closure of `n`.
    pub(crate) fn x_of(&self, n: u32) -> Pixels {
        self.bounds.left() + self.bounds.size.width * self.x.fraction(n)
    }

    /// The vertical position of a blast radius of `n`, rising up the plot.
    pub(crate) fn y_of(&self, n: u32) -> Pixels {
        self.bounds.bottom() - self.bounds.size.height * self.y.fraction(n)
    }

    pub(crate) fn place(&self, target: &TargetMetrics) -> Point<Pixels> {
        point(self.x_of(target.closure), self.y_of(target.blast_radius))
    }

    /// The mark of `marks` a pointer at `at` picks: the one whose centre is
    /// nearest, among those whose rim lies within [`PICK_SLOP`] of it, and the
    /// one drawn over the others among equally near ones.
    pub(crate) fn pick(&self, marks: &[Mark], at: Point<Pixels>) -> Option<Picked> {
        let mut nearest: Option<(usize, f32)> = None;
        for (index, mark) in marks.iter().enumerate() {
            let center = self.place(&mark.target);
            let distance = f32::from(center.x - at.x).hypot(f32::from(center.y - at.y));
            let reaches = distance <= mark.radius + PICK_SLOP;
            if reaches && nearest.is_none_or(|(_, best)| distance <= best) {
                nearest = Some((index, distance));
            }
        }
        let (index, _) = nearest?;
        let at_same_place = |mark: &Mark| {
            let (one, other) = (&mark.target, &marks[index].target);
            (one.closure, one.blast_radius) == (other.closure, other.blast_radius)
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
    use bviz_graph_layout::Id;
    use gpui::size;

    fn target(closure: u32, blast_radius: u32, fan_in: u32) -> TargetMetrics {
        TargetMetrics {
            id: Id(0),
            kind: Kind::Target,
            closure,
            blast_radius,
            fan_in,
            fan_out: 0,
        }
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
        let plotted = Plotted::of(&[target(1, 1, 0), target(1, 1, 100), target(1, 1, 3)]);
        let radii: Vec<f32> = plotted.marks.iter().map(|m| m.radius).collect();
        assert_eq!(radii.first(), Some(&MAX_RADIUS));
        assert_eq!(radii.last(), Some(&MIN_RADIUS));
        assert!(radii.windows(2).all(|pair| pair[0] >= pair[1]));
    }

    #[test]
    fn no_fan_in_anywhere_draws_every_mark_at_the_smallest_size() {
        let plotted = Plotted::of(&[target(0, 0, 0), target(4, 2, 0)]);
        assert!(plotted.marks.iter().all(|m| m.radius == MIN_RADIUS));
    }

    #[test]
    fn every_mark_stays_clear_of_the_axes() {
        let targets = [target(0, 0, 9), target(4_000, 17, 9), target(3, 90_000, 9)];
        let plotted = Plotted::of(&targets);
        let axes = Bounds::new(point(px(40.), px(10.)), size(px(300.), px(200.)));
        let area = DataArea::within(axes, plotted.x, plotted.y);
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
        let plotted = Plotted::of(&targets);
        let axes = Bounds::new(point(px(0.), px(0.)), size(px(400.), px(300.)));
        let area = DataArea::within(axes, plotted.x, plotted.y);
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
            .rposition(|m| (m.target.closure, m.target.blast_radius) == (100, 100));
        assert_eq!(shared.map(|p| p.mark), top);
        assert_eq!(shared.map(|p| p.coincident), Some(1));

        let far = origin + point(px(MAX_RADIUS + PICK_SLOP + 1.), px(0.));
        assert_eq!(area.pick(&plotted.marks, far), None);
    }

    #[test]
    fn more_blast_radius_places_a_mark_higher() {
        let plotted = Plotted::of(&[target(0, 0, 0), target(0, 50, 0)]);
        let axes = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(100.)));
        let area = DataArea::within(axes, plotted.x, plotted.y);
        assert!(area.y_of(50) < area.y_of(0));
        assert_eq!(area.x_of(0), axes.left() + px(MAX_RADIUS));
    }
}
