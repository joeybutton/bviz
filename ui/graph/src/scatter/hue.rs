//! The colour each point of the scatter plot is drawn in, and the legend naming
//! those colours.

use super::axis::Clocks;
use super::kind_names;
use crate::Palette;
use crate::timing::Ran;
use bviz_graph_layout::facets::Sym;
use bviz_graph_layout::metrics::{Measurement, Metrics, Unit};
use bviz_graph_layout::{Facets, Kind};
use bviz_invocation::Runner;
use bviz_ui_theme::DataColors;
use gpui::{Rgba, SharedString};
use std::collections::HashMap;

/// One of the theme's chromatic data slots, by position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Slot(u8);

/// The theme's chromatic slots, by their position on its wheel, in an order
/// each prefix of which holds its slots as far apart on the wheel as any
/// choice of as many can.
const SPREAD: [u8; DataColors::CHROMATIC] = [0, 4, 2, 5, 6, 3, 1];

impl Slot {
    /// Every slot, the most distinct hues first.
    fn spread() -> impl Iterator<Item = Slot> {
        SPREAD.into_iter().map(Slot)
    }
}

/// What a mark is coloured by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hue {
    /// The accent the graph draws a node of the kind in.
    Kind(Kind),
    Slot(Slot),
    /// Where the actions of the point ran.
    Ran(Ran),
    /// A point no action time lands on, where marks are coloured by where
    /// their actions ran.
    Untimed,
    /// The catch-all for whatever no slot is spent on.
    Other,
}

impl Hue {
    pub(crate) fn color(self, palette: &Palette) -> Rgba {
        match self {
            Hue::Kind(kind) => palette.kind_accent(kind),
            Hue::Slot(Slot(n)) => palette.data.chromatic()[usize::from(n)].into(),
            Hue::Ran(ran) => palette.ran_mark(Some(ran)),
            Hue::Untimed => palette.ran_mark(None),
            Hue::Other => palette.data.neutral.into(),
        }
    }

    /// Whether marks of this hue are drawn beneath the rest: those of points
    /// no action time lands on, which can far outnumber the timed points and
    /// would otherwise bury them.
    pub(crate) fn recedes(self) -> bool {
        self == Hue::Untimed
    }
}

/// How a legend names the actions no slot is spent on.
const OTHER: &str = "other";

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
    /// Each point by the kind of node it is.
    Kinds,
    /// Each action by its mnemonic, the most frequent first taking a slot each
    /// while slots last, and the rest [`Hue::Other`].
    Mnemonics(Vec<(Sym, Slot)>),
    /// Each point by where its actions ran, and one no action time lands on
    /// [`Hue::Untimed`].
    Runners,
}

impl Colouring {
    /// The colouring `by` of points of `unit`, ranking mnemonics by how many
    /// of the actions `reference` measures run each, so that a hue means the
    /// same whichever inputs a plot of the unit reads. Equally frequent
    /// mnemonics rank by name, so neither the ranking nor which of those tied
    /// for the last slot takes it turns on the order actions are read.
    pub(crate) fn of(by: ColourBy, unit: Unit, reference: &Measurement, facets: &Facets) -> Self {
        if by == ColourBy::Runner {
            return Colouring::Runners;
        }
        if unit != Unit::Action {
            return Colouring::Kinds;
        }
        let mut counts: HashMap<Sym, u32> = HashMap::new();
        let mnemonics = reference
            .points
            .iter()
            .filter_map(|p| facets.mnemonic(p.id));
        for mnemonic in mnemonics {
            *counts.entry(mnemonic).or_default() += 1;
        }
        let mut ranked: Vec<(Sym, u32)> = counts.into_iter().collect();
        let name = |sym: Sym| facets.strings().resolve(sym);
        ranked.sort_unstable_by(|(a, m), (b, n)| n.cmp(m).then_with(|| name(*a).cmp(name(*b))));
        let slotted = ranked.into_iter().zip(Slot::spread());
        Colouring::Mnemonics(slotted.map(|((sym, _), slot)| (sym, slot)).collect())
    }

    /// The hue of `point`, whose actions ran as `clocks` says.
    pub(crate) fn hue(&self, point: &Metrics, facets: &Facets, clocks: &Clocks) -> Hue {
        match self {
            Colouring::Kinds => Hue::Kind(point.kind),
            Colouring::Mnemonics(slotted) => {
                slot_of(slotted, point, facets).map_or(Hue::Other, Hue::Slot)
            }
            Colouring::Runners => clocks.ran(point.id).map_or(Hue::Untimed, Hue::Ran),
        }
    }

    /// Each hue `points`, whose actions ran as `clocks` says, are drawn in,
    /// once, and what the legend names it: kinds in their enum order, of which
    /// those a reader names alike read as one, mnemonics by rank with the rest
    /// last, and runners in the order of `Runner::ALL` with a mix of them and
    /// then the points no action time lands on last.
    pub(crate) fn legend<'a>(
        &self,
        points: impl Iterator<Item = &'a Metrics>,
        facets: &Facets,
        clocks: &Clocks,
    ) -> Vec<(SharedString, Hue)> {
        match self {
            Colouring::Kinds => {
                let mut kinds: Vec<Kind> = points.map(|point| point.kind).collect();
                kinds.sort_unstable();
                kinds.dedup();
                let mut legend: Vec<(SharedString, Hue)> = Vec::with_capacity(kinds.len());
                for kind in kinds {
                    let name = kind_names(kind).many;
                    if !legend.iter().any(|(named, _)| named == name) {
                        legend.push((name.into(), Hue::Kind(kind)));
                    }
                }
                legend
            }
            Colouring::Mnemonics(slotted) => {
                let mut drawn = [false; DataColors::CHROMATIC];
                let mut other = false;
                for point in points {
                    match slot_of(slotted, point, facets) {
                        Some(Slot(n)) => drawn[usize::from(n)] = true,
                        None => other = true,
                    }
                }
                let named = slotted
                    .iter()
                    .filter(|&&(_, Slot(n))| drawn[usize::from(n)])
                    .map(|&(sym, slot)| {
                        let name = facets.strings().resolve(sym).to_owned();
                        (name.into(), Hue::Slot(slot))
                    });
                let rest = other.then(|| (OTHER.into(), Hue::Other));
                named.chain(rest).collect()
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

/// The slot of `slotted` the mnemonic of `point` takes, absent where no slot is
/// spent on it.
fn slot_of(slotted: &[(Sym, Slot)], point: &Metrics, facets: &Facets) -> Option<Slot> {
    let mnemonic = facets.mnemonic(point.id)?;
    let &(_, slot) = slotted.iter().find(|&&(sym, _)| sym == mnemonic)?;
    Some(slot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::metrics::{Count, Scope, Sizing};
    use bviz_graph_layout::{Id, InputSet};
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

    /// One action per mnemonic of `mnemonics`, by id in that order.
    fn actions(mnemonics: &[&str]) -> (Measurement, Facets) {
        let mut facets = Facets::default();
        for (id, name) in mnemonics.iter().enumerate() {
            let sym = facets.intern_mnemonic(name);
            facets.set_mnemonic(Id(id as u32), Some(sym));
        }
        let points = (0..mnemonics.len() as u32).map(action).collect();
        let scope = Scope::new(Unit::Action, InputSet::ALL).unwrap();
        let measured = Measurement {
            scope,
            points,
            sizing: Sizing::FanIn,
        };
        (measured, facets)
    }

    fn names(legend: &[(SharedString, Hue)]) -> Vec<&str> {
        legend.iter().map(|(name, _)| name.as_ref()).collect()
    }

    #[test]
    fn the_most_frequent_mnemonics_take_the_slots_and_the_rest_share_one_hue() {
        let mut mnemonics = vec!["Rare"];
        let common: Vec<String> = (0..DataColors::CHROMATIC)
            .map(|n| format!("M{n}"))
            .collect();
        for name in &common {
            mnemonics.extend([name.as_str(), name.as_str()]);
        }
        let (measured, facets) = actions(&mnemonics);
        let colouring = Colouring::of(ColourBy::Identity, Unit::Action, &measured, &facets);
        assert_eq!(
            colouring.hue(&measured.points[0], &facets, &Clocks::default()),
            Hue::Other
        );
        assert_eq!(
            colouring.hue(&measured.points[1], &facets, &Clocks::default()),
            Hue::Slot(Slot(SPREAD[0]))
        );

        let legend = colouring.legend(measured.points.iter(), &facets, &Clocks::default());
        let mut expected: Vec<&str> = common.iter().map(String::as_str).collect();
        expected.push(OTHER);
        assert_eq!(names(&legend), expected);
    }

    /// How far apart the slots `slots` lie on the wheel at their nearest.
    fn nearest(slots: &[u8]) -> usize {
        let wheel = DataColors::CHROMATIC;
        let mut nearest = wheel;
        for (i, &a) in slots.iter().enumerate() {
            for &b in &slots[i + 1..] {
                let apart = usize::from(a.abs_diff(b));
                nearest = nearest.min(apart.min(wheel - apart));
            }
        }
        nearest
    }

    /// However few mnemonics take slots, none take neighbouring hues where
    /// another choice of as many slots would keep them further apart.
    #[test]
    fn every_prefix_of_the_slots_is_spread_as_far_as_it_can_be() {
        let wheel = DataColors::CHROMATIC as u8;
        let mut sorted = SPREAD;
        sorted.sort_unstable();
        assert_eq!(sorted.to_vec(), (0..wheel).collect::<Vec<_>>());
        for taken in 2..=SPREAD.len() {
            let best = (0u32..1 << wheel)
                .filter(|choice| choice.count_ones() as usize == taken)
                .map(|choice| {
                    let slots: Vec<u8> = (0..wheel).filter(|&n| choice & 1 << n != 0).collect();
                    nearest(&slots)
                })
                .max();
            assert_eq!(Some(nearest(&SPREAD[..taken])), best, "{taken} slots");
        }
    }

    #[test]
    fn equally_frequent_mnemonics_rank_by_name() {
        let (measured, facets) = actions(&["Zip", "Cc", "Zip", "Cc", "Ar"]);
        let colouring = Colouring::of(ColourBy::Identity, Unit::Action, &measured, &facets);
        let legend = colouring.legend(measured.points.iter(), &facets, &Clocks::default());
        assert_eq!(names(&legend), ["Cc", "Zip", "Ar"]);
    }

    #[test]
    fn kinds_named_alike_read_as_one_legend_entry() {
        let (_, facets) = actions(&[]);
        let targets = [Kind::Target, Kind::ConfiguredTarget, Kind::Target]
            .map(|kind| Metrics { kind, ..action(0) });
        let legend = Colouring::Kinds.legend(targets.iter(), &facets, &Clocks::default());
        assert_eq!(names(&legend), ["rule targets", "configured targets"]);
    }

    #[test]
    fn runners_read_in_their_order_then_a_mix_then_the_untimed() {
        let (measured, facets) = actions(&["A", "B", "C", "D", "E"]);
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
