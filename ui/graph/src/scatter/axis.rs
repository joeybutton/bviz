//! The measures the scatter plot places its points by.

use super::plot::tick_label;
use bviz_graph_layout::metrics::Metrics;

/// What an axis of the plot places a point by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Axis {
    /// How much of the build the point pulls in.
    Closure,
    /// How much of the build depends on the point.
    BlastRadius,
}

impl Axis {
    /// Where along the axis `point` falls, in the values its scale is laid out
    /// in.
    pub(crate) fn position(self, point: &Metrics) -> u32 {
        match self {
            Axis::Closure => point.closure.value(),
            Axis::BlastRadius => point.blast_radius.value(),
        }
    }

    /// How a line of text names the measure.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Axis::Closure => "closure",
            Axis::BlastRadius => "blast radius",
        }
    }

    /// What the plot reads along the axis.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Axis::Closure => "closure size (transitive dependencies)",
            Axis::BlastRadius => "blast radius (transitive dependents)",
        }
    }

    /// How the measure of `point` reads, marked where it is an estimate.
    pub(crate) fn reading(self, point: &Metrics) -> String {
        match self {
            Axis::Closure => point.closure.to_string(),
            Axis::BlastRadius => point.blast_radius.to_string(),
        }
    }

    /// How a tick at `value` along the axis reads.
    pub(crate) fn tick_label(self, value: u32) -> String {
        tick_label(value)
    }
}

/// The measures a plot places its points by: across, and up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Axes {
    pub(crate) x: Axis,
    pub(crate) y: Axis,
}

impl Axes {
    /// How `point` reads on both axes, each measure named.
    pub(crate) fn readings(self, point: &Metrics) -> String {
        let reads = |axis: Axis| format!("{} {}", axis.name(), axis.reading(point));
        format!("{} · {}", reads(self.x), reads(self.y))
    }

    /// The names of both measures, as a header over a column of
    /// [`values`](Self::values) reads.
    pub(crate) fn names(self) -> String {
        format!("{} · {}", self.x.name(), self.y.name())
    }

    /// How `point` reads on both axes, under a header of their
    /// [`names`](Self::names).
    pub(crate) fn values(self, point: &Metrics) -> String {
        format!("{} · {}", self.x.reading(point), self.y.reading(point))
    }
}
