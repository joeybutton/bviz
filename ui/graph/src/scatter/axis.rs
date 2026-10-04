//! The measures the scatter plot places its points by.

use super::plot::{HOUR_MS, Ladder, MINUTE_MS, SECOND_MS, tick_label};
use crate::duration::lasted;
use crate::timing::{NodeTimes, Ran, Untimed};
use bviz_graph_layout::Id;
use bviz_graph_layout::metrics::Metrics;
use rustc_hash::FxHashMap;
use std::time::Duration;

/// What an axis of the plot places a point by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    /// How much of the build the point pulls in.
    Closure,
    /// How much of the build depends on the point.
    BlastRadius,
    /// How long the actions the point stands for took, summed; placed in
    /// milliseconds.
    ActionTime,
}

impl Axis {
    pub(crate) const ALL: [Axis; 3] = [Axis::Closure, Axis::BlastRadius, Axis::ActionTime];

    /// Where along the axis `point`, whose actions took `time`, falls, in the
    /// values its scale is laid out in; absent where the point has no such
    /// measure.
    pub(crate) fn position(self, point: &Metrics, time: Option<Duration>) -> Option<u32> {
        match self {
            Axis::Closure => Some(point.closure.value()),
            Axis::BlastRadius => Some(point.blast_radius.value()),
            Axis::ActionTime => {
                let millis = (time?.as_secs_f64() * f64::from(SECOND_MS)).round();
                Some(millis.min(f64::from(u32::MAX)) as u32)
            }
        }
    }

    /// Where a picker offers the measure, in the order of [`ALL`](Self::ALL),
    /// which is the order they are declared in.
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    /// Whether the axis places a point by the action time the graph places on
    /// it, which a point can lack, and which changes with the session's
    /// timings.
    pub(crate) fn reads_time(self) -> bool {
        self == Axis::ActionTime
    }

    /// How a line of text names the measure.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Axis::Closure => "closure",
            Axis::BlastRadius => "blast radius",
            Axis::ActionTime => "action time",
        }
    }

    /// What the plot reads along the axis.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Axis::Closure => "closure size (transitive dependencies)",
            Axis::BlastRadius => "blast radius (transitive dependents)",
            Axis::ActionTime => "action time (wall time of its actions, summed)",
        }
    }

    /// How the measure of `point`, whose actions took `time`, reads, marked
    /// where it is an estimate.
    pub(crate) fn reading(self, point: &Metrics, time: Option<Duration>) -> String {
        match self {
            Axis::Closure => point.closure.to_string(),
            Axis::BlastRadius => point.blast_radius.to_string(),
            Axis::ActionTime => time.map_or_else(|| "none".to_owned(), lasted),
        }
    }

    /// The values the axis's ticks fall on.
    pub(crate) fn ladder(self) -> Ladder {
        match self {
            Axis::Closure | Axis::BlastRadius => Ladder::Counts,
            Axis::ActionTime => Ladder::Millis,
        }
    }

    /// How a tick at `value` along the axis reads.
    pub(crate) fn tick_label(self, value: u32) -> String {
        match self {
            Axis::Closure | Axis::BlastRadius => tick_label(value),
            Axis::ActionTime => time_tick_label(value),
        }
    }
}

/// How a tick `millis` along the time axis reads: in the largest unit it
/// reaches, which a tick of [`Ladder::Millis`] is a whole number of.
fn time_tick_label(millis: u32) -> String {
    match millis {
        0 => tick_label(millis),
        _ if millis < SECOND_MS => format!("{millis}ms"),
        _ if millis < MINUTE_MS => format!("{}s", millis / SECOND_MS),
        _ if millis < HOUR_MS => format!("{}m", millis / MINUTE_MS),
        _ => format!("{}h", millis / HOUR_MS),
    }
}

/// The measures a plot places its points by: across, and up, never the same
/// measure on both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Axes {
    x: Axis,
    y: Axis,
}

impl Axes {
    /// Closure across, blast radius up.
    pub(crate) const OPENING: Axes = Axes {
        x: Axis::Closure,
        y: Axis::BlastRadius,
    };

    pub(crate) fn x(self) -> Axis {
        self.x
    }

    pub(crate) fn y(self) -> Axis {
        self.y
    }

    /// These axes with `axis` across, swapped with the measure up where that
    /// is `axis`.
    pub(crate) fn across(self, axis: Axis) -> Axes {
        match axis == self.y {
            true => Axes { x: axis, y: self.x },
            false => Axes { x: axis, ..self },
        }
    }

    /// These axes with `axis` up, by the rule [`across`](Self::across) keeps.
    pub(crate) fn up(self, axis: Axis) -> Axes {
        self.transposed().across(axis).transposed()
    }

    fn transposed(self) -> Axes {
        Axes {
            x: self.y,
            y: self.x,
        }
    }

    /// These axes with the measure on neither in place of one that
    /// [reads time](Axis::reads_time).
    pub(crate) fn untimed(self) -> Axes {
        let spare = match (self.x, self.y) {
            (Axis::Closure, Axis::BlastRadius) | (Axis::BlastRadius, Axis::Closure) => {
                Axis::ActionTime
            }
            (Axis::Closure, _) | (_, Axis::Closure) => Axis::BlastRadius,
            _ => Axis::Closure,
        };
        match (self.x.reads_time(), self.y.reads_time()) {
            (true, _) => self.across(spare),
            (_, true) => self.up(spare),
            _ => self,
        }
    }

    /// Where `point`, whose actions took `time`, falls along each axis's
    /// scale; absent where it has no measure on one of them.
    pub(crate) fn place(self, point: &Metrics, time: Option<Duration>) -> Option<(u32, u32)> {
        Some((self.x.position(point, time)?, self.y.position(point, time)?))
    }

    /// Whether either axis [reads time](Axis::reads_time).
    pub(crate) fn reads_time(self) -> bool {
        self.x.reads_time() || self.y.reads_time()
    }

    /// How `point`, whose actions took `time`, reads on both axes, each
    /// measure named.
    pub(crate) fn readings(self, point: &Metrics, time: Option<Duration>) -> String {
        let reads = |axis: Axis| format!("{} {}", axis.name(), axis.reading(point, time));
        format!("{} · {}", reads(self.x), reads(self.y))
    }

    /// How `point`, whose actions took `time`, reads on every measure it has
    /// that neither axis places it by, each named; absent where it has none.
    pub(crate) fn unplaced(self, point: &Metrics, time: Option<Duration>) -> Option<String> {
        let rest: Vec<String> = Axis::ALL
            .into_iter()
            .filter(|&axis| axis != self.x && axis != self.y)
            .filter(|axis| axis.position(point, time).is_some())
            .map(|axis| format!("{} {}", axis.name(), axis.reading(point, time)))
            .collect();
        (!rest.is_empty()).then(|| rest.join(" · "))
    }

    /// The names of both measures, as a header over a column of
    /// [`values`](Self::values) reads.
    pub(crate) fn names(self) -> String {
        format!("{} · {}", self.x.name(), self.y.name())
    }

    /// How `point`, whose actions took `time`, reads on both axes, under a
    /// header of their [`names`](Self::names).
    pub(crate) fn values(self, point: &Metrics, time: Option<Duration>) -> String {
        format!(
            "{} · {}",
            self.x.reading(point, time),
            self.y.reading(point, time)
        )
    }
}

/// How long the actions each point of a measure stands for took and where
/// they ran, of the points any action time lands on, held apart from the
/// graph so that a plot can be drawn off the UI thread.
#[derive(Debug, Default)]
pub(crate) struct Clocks(FxHashMap<Id, Clock>);

/// How long the actions of one point took, and where they ran.
#[derive(Clone, Copy, Debug)]
struct Clock {
    wall: Duration,
    ran: Ran,
}

impl Clocks {
    /// The action time `times` places on each of `points`, and where those
    /// actions ran.
    pub(crate) fn of(times: Result<&NodeTimes, Untimed>, points: &[Metrics]) -> Self {
        let Ok(times) = times else {
            return Clocks::default();
        };
        let mut clocks = FxHashMap::with_capacity_and_hasher(points.len(), Default::default());
        clocks.extend(points.iter().filter_map(|point| {
            let timed = times.get(point.id)?;
            let clock = Clock {
                wall: timed.held.total().wall,
                ran: Ran::of(&timed.held),
            };
            Some((point.id, clock))
        }));
        Clocks(clocks)
    }

    pub(crate) fn time(&self, id: Id) -> Option<Duration> {
        Some(self.0.get(&id)?.wall)
    }

    pub(crate) fn ran(&self, id: Id) -> Option<Ran> {
        Some(self.0.get(&id)?.ran)
    }

    /// The clocks of the points `runs` names, by id, time and runner.
    #[cfg(test)]
    pub(crate) fn of_runs(runs: &[(Id, Duration, Ran)]) -> Self {
        let clocks = runs
            .iter()
            .map(|&(id, wall, ran)| (id, Clock { wall, ran }));
        Clocks(clocks.collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::Kind;
    use bviz_graph_layout::metrics::Count;

    fn point(closure: u32, blast_radius: u32) -> Metrics {
        Metrics {
            id: Id(0),
            kind: Kind::Action,
            closure: Count::Exact(closure),
            blast_radius: Count::Estimated(blast_radius),
            fan_in: 0,
            fan_out: 0,
            actions: 1,
        }
    }

    #[test]
    fn action_time_places_a_point_by_its_milliseconds_and_leaves_an_untimed_one_out() {
        let at = |time| Axis::ActionTime.position(&point(3, 4), time);
        assert_eq!(at(Some(Duration::from_micros(2_499_600))), Some(2_500));
        assert_eq!(at(Some(Duration::from_micros(400))), Some(0));
        assert_eq!(at(None), None);
        assert_eq!(Axis::Closure.position(&point(3, 4), None), Some(3));
    }

    #[test]
    fn a_time_tick_reads_in_the_largest_unit_it_reaches() {
        let ticks = [
            0,
            5,
            500,
            SECOND_MS,
            30 * SECOND_MS,
            MINUTE_MS,
            20 * MINUTE_MS,
            HOUR_MS,
            50 * HOUR_MS,
        ];
        let labels = ["0", "5ms", "500ms", "1s", "30s", "1m", "20m", "1h", "50h"];
        assert_eq!(
            ticks.map(|value| Axis::ActionTime.tick_label(value)),
            labels
        );
    }

    #[test]
    fn a_tooltip_reads_the_plotted_measures_and_then_those_left_off_the_axes() {
        let timed = Some(Duration::from_millis(3_200));
        let by_time = Axes::OPENING.across(Axis::ActionTime);
        assert_eq!(
            by_time.readings(&point(3, 4), timed),
            "action time 3.2s · blast radius ≈4"
        );
        assert_eq!(
            by_time.unplaced(&point(3, 4), timed).as_deref(),
            Some("closure 3")
        );
        let by_closure = by_time.across(Axis::Closure);
        assert_eq!(
            by_closure.unplaced(&point(3, 4), timed).as_deref(),
            Some("action time 3.2s")
        );
        assert_eq!(by_closure.unplaced(&point(3, 4), None), None);
    }

    #[test]
    fn choosing_the_measure_the_other_axis_shows_swaps_the_axes() {
        let opening = Axes::OPENING;
        let swapped = Axes {
            x: Axis::BlastRadius,
            y: Axis::Closure,
        };
        assert_eq!(opening.across(Axis::BlastRadius), swapped);
        assert_eq!(opening.up(Axis::Closure), swapped);
        assert_eq!(opening.across(Axis::Closure), opening);
        assert_eq!(
            opening.up(Axis::ActionTime),
            Axes {
                x: Axis::Closure,
                y: Axis::ActionTime,
            }
        );
        let closure_by_time = opening.up(Axis::Closure).across(Axis::ActionTime);
        assert_eq!(
            closure_by_time,
            Axes {
                x: Axis::ActionTime,
                y: Axis::Closure,
            }
        );
        for x in Axis::ALL {
            for y in Axis::ALL {
                let axes = opening.across(x).up(y);
                assert_ne!(axes.x, axes.y);
                assert_eq!(axes.y, y);
            }
        }
    }

    #[test]
    fn an_axis_reading_time_falls_back_to_the_measure_on_neither_axis() {
        let fallen = |x, y| Axes { x, y }.untimed();
        let (closure, blast, time) = (Axis::Closure, Axis::BlastRadius, Axis::ActionTime);
        assert_eq!(
            fallen(time, blast),
            Axes {
                x: closure,
                y: blast
            }
        );
        assert_eq!(
            fallen(time, closure),
            Axes {
                x: blast,
                y: closure
            }
        );
        assert_eq!(
            fallen(closure, time),
            Axes {
                x: closure,
                y: blast
            }
        );
        assert_eq!(
            fallen(blast, time),
            Axes {
                x: blast,
                y: closure
            }
        );
        assert_eq!(
            fallen(blast, closure),
            Axes {
                x: blast,
                y: closure
            }
        );
    }
}
