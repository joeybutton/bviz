//! The colour each point of the scatter plot is drawn in, and the legend naming
//! those colours.

use super::axis::Clocks;
use crate::hue::{Colouring as Identities, Hue};
use crate::timing::Ran;
use bviz_graph_layout::Facets;
use bviz_graph_layout::metrics::{Measurement, Metrics, Unit};
use bviz_invocation::Runner;
use gpui::SharedString;

/// How a legend names the points no action time lands on.
const UNTIMED: &str = "no action time";

/// What a plot's marks are coloured by, as the reader chooses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColourBy {
    /// What each point is: an action's mnemonic, any other point's kind.
    Identity,
    /// Where the actions each point stands for ran.
    Runner,
}

impl ColourBy {
    /// In the order the picker offers them, which is the order they are
    /// declared in.
    pub(crate) const ALL: [ColourBy; 2] = [ColourBy::Identity, ColourBy::Runner];

    /// Where the picker offers it.
    pub(crate) fn index(self) -> usize {
        self as usize
    }

    /// How the picker names the colouring for points of `unit`.
    pub(crate) fn label(self, unit: Unit) -> &'static str {
        match (self, unit) {
            (ColourBy::Identity, Unit::Action) => "mnemonic",
            (ColourBy::Identity, _) => "kind",
            (ColourBy::Runner, _) => "runner",
        }
    }
}

/// How the points of a plot are coloured.
pub(crate) enum Colouring {
    /// Each point by what it is.
    Identities(Identities),
    /// Each point by where its actions ran, and one no action time lands on
    /// [`Hue::Untimed`].
    Runners,
}

impl Colouring {
    /// The colouring `by` of points of `unit`, ranking mnemonics over the
    /// actions `reference` measures.
    pub(crate) fn of(by: ColourBy, unit: Unit, reference: &Measurement, facets: &Facets) -> Self {
        match by {
            ColourBy::Identity => {
                let ranked = reference.points.iter().map(|point| point.id);
                Colouring::Identities(Identities::of(unit, ranked, facets))
            }
            ColourBy::Runner => Colouring::Runners,
        }
    }

    /// The hue of `point`, whose actions ran as `clocks` says.
    pub(crate) fn hue(&self, point: &Metrics, facets: &Facets, clocks: &Clocks) -> Hue {
        match self {
            Colouring::Identities(identities) => identities.hue(point.id, point.kind, facets),
            Colouring::Runners => clocks.ran(point.id).map_or(Hue::Untimed, Hue::Ran),
        }
    }

    /// Each hue `points`, whose actions ran as `clocks` says, are drawn in,
    /// once, and what the legend names it: what points are in the order their
    /// colouring names them, and runners in the order of `Runner::ALL` with a
    /// mix of them and then the points no action time lands on last.
    pub(crate) fn legend<'a>(
        &self,
        points: impl Iterator<Item = &'a Metrics>,
        facets: &Facets,
        clocks: &Clocks,
    ) -> Vec<(SharedString, Hue)> {
        match self {
            Colouring::Identities(identities) => {
                identities.legend(points.map(|point| (point.id, point.kind)), facets)
            }
            Colouring::Runners => {
                let mut ran = [false; Runner::ALL.len()];
                let (mut mixed, mut untimed) = (false, false);
                for point in points {
                    match clocks.ran(point.id) {
                        Some(Ran::On(runner)) => ran[runner.index()] = true,
                        Some(Ran::Mixed) => mixed = true,
                        None => untimed = true,
                    }
                }
                let on = Runner::ALL
                    .into_iter()
                    .filter(|runner| ran[runner.index()])
                    .map(Ran::On);
                let ran = on.chain(mixed.then_some(Ran::Mixed));
                let named = ran.map(|ran| (ran.label().into(), Hue::Ran(ran)));
                let rest = untimed.then(|| (UNTIMED.into(), Hue::Untimed));
                named.chain(rest).collect()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::metrics::{Count, Scope, Sizing};
    use bviz_graph_layout::{Id, InputSet, Kind};
    use std::time::Duration;

    fn action(id: u32) -> Metrics {
        Metrics {
            id: Id(id),
            kind: Kind::Action,
            closure: Count::Exact(0),
            blast_radius: Count::Exact(0),
            fan_in: 0,
            fan_out: 0,
            actions: 1,
        }
    }

    fn names(legend: &[(SharedString, Hue)]) -> Vec<&str> {
        legend.iter().map(|(name, _)| name.as_ref()).collect()
    }

    #[test]
    fn runners_read_in_their_order_then_a_mix_then_the_untimed() {
        let facets = Facets::default();
        let measured = Measurement {
            scope: Scope::new(Unit::Action, InputSet::ALL).unwrap(),
            points: (0..5).map(action).collect(),
            sizing: Sizing::FanIn,
        };
        let second = Duration::from_secs(1);
        let clocks = Clocks::of_runs(&[
            (Id(0), second, Ran::On(Runner::Sandboxed)),
            (Id(1), second, Ran::Mixed),
            (Id(2), second, Ran::On(Runner::RemoteExecution)),
            (Id(3), second, Ran::On(Runner::Sandboxed)),
        ]);
        let colouring = Colouring::of(ColourBy::Runner, Unit::Action, &measured, &facets);
        assert_eq!(
            colouring.hue(&measured.points[2], &facets, &clocks),
            Hue::Ran(Ran::On(Runner::RemoteExecution))
        );
        assert_eq!(
            colouring.hue(&measured.points[4], &facets, &clocks),
            Hue::Untimed
        );
        let legend = colouring.legend(measured.points.iter(), &facets, &clocks);
        assert_eq!(
            names(&legend),
            ["remote execution", "sandboxed", "mixed runners", UNTIMED]
        );
    }
}
