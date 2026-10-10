//! The colour each point of a measure is drawn in, and the legend naming those
//! colours.

use crate::Palette;
use crate::point::kind_names;
use crate::timing::Ran;
use bviz_graph_layout::facets::Sym;
use bviz_graph_layout::metrics::Unit;
use bviz_graph_layout::{Facets, Id, Kind};
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

/// What a point is coloured by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Hue {
    /// The accent the graph draws a node of the kind in.
    Kind(Kind),
    Slot(Slot),
    /// Where the actions of the point ran.
    Ran(Ran),
    /// A point no action time lands on, where points are coloured by where
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

    /// Whether points of this hue are drawn beneath the rest: those no action
    /// time lands on, which can far outnumber the timed points and would
    /// otherwise bury them.
    pub(crate) fn recedes(self) -> bool {
        self == Hue::Untimed
    }
}

/// How a legend names the actions no slot is spent on.
const OTHER: &str = "other";

/// How the points of a measure are coloured.
pub(crate) enum Colouring {
    /// Each point by the kind of node it is.
    Kinds,
    /// Each action by its mnemonic, the most frequent first taking a slot each
    /// while slots last, and the rest [`Hue::Other`].
    Mnemonics(Vec<(Sym, Slot)>),
}

impl Colouring {
    /// The colouring of points of `unit`, ranking mnemonics by how many of the
    /// actions `reference` names run each; ranked over every action the graph
    /// holds, a hue means the same whichever inputs a view of the unit reads.
    /// Equally frequent mnemonics rank by name, so neither the ranking nor
    /// which of those tied for the last slot takes it turns on the order
    /// actions are read.
    pub(crate) fn of(unit: Unit, reference: impl Iterator<Item = Id>, facets: &Facets) -> Self {
        if unit != Unit::Action {
            return Colouring::Kinds;
        }
        let mut counts: HashMap<Sym, u32> = HashMap::new();
        for mnemonic in reference.filter_map(|id| facets.mnemonic(id)) {
            *counts.entry(mnemonic).or_default() += 1;
        }
        let mut ranked: Vec<(Sym, u32)> = counts.into_iter().collect();
        let name = |sym: Sym| facets.strings().resolve(sym);
        ranked.sort_unstable_by(|(a, m), (b, n)| n.cmp(m).then_with(|| name(*a).cmp(name(*b))));
        let slotted = ranked.into_iter().zip(Slot::spread());
        Colouring::Mnemonics(slotted.map(|((sym, _), slot)| (sym, slot)).collect())
    }

    /// The hue of the point `id`, of `kind`.
    pub(crate) fn hue(&self, id: Id, kind: Kind, facets: &Facets) -> Hue {
        match self {
            Colouring::Kinds => Hue::Kind(kind),
            Colouring::Mnemonics(slotted) => {
                slot_of(slotted, id, facets).map_or(Hue::Other, Hue::Slot)
            }
        }
    }

    /// Each hue `points`, each by its node and kind, are drawn in, once, and
    /// what the legend names it: kinds in their enum order, of which those a
    /// reader names alike read as one, and mnemonics by rank, with the rest
    /// last.
    pub(crate) fn legend(
        &self,
        points: impl Iterator<Item = (Id, Kind)>,
        facets: &Facets,
    ) -> Vec<(SharedString, Hue)> {
        match self {
            Colouring::Kinds => {
                let mut kinds: Vec<Kind> = points.map(|(_, kind)| kind).collect();
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
                for (id, _) in points {
                    match slot_of(slotted, id, facets) {
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
        }
    }
}

/// The slot of `slotted` the mnemonic of the action `id` takes, absent where no
/// slot is spent on it.
fn slot_of(slotted: &[(Sym, Slot)], id: Id, facets: &Facets) -> Option<Slot> {
    let mnemonic = facets.mnemonic(id)?;
    let &(_, slot) = slotted.iter().find(|&&(sym, _)| sym == mnemonic)?;
    Some(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One action per mnemonic of `mnemonics`, by id in that order.
    fn actions(mnemonics: &[&str]) -> (Vec<Id>, Facets) {
        let mut facets = Facets::default();
        for (id, name) in mnemonics.iter().enumerate() {
            let sym = facets.intern_mnemonic(name);
            facets.set_mnemonic(Id(id as u32), Some(sym));
        }
        let ids = (0..mnemonics.len() as u32).map(Id).collect();
        (ids, facets)
    }

    fn of_actions(ids: &[Id]) -> impl Iterator<Item = (Id, Kind)> {
        ids.iter().map(|&id| (id, Kind::Action))
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
        let (ids, facets) = actions(&mnemonics);
        let colouring = Colouring::of(Unit::Action, ids.iter().copied(), &facets);
        assert_eq!(colouring.hue(ids[0], Kind::Action, &facets), Hue::Other);
        assert_eq!(
            colouring.hue(ids[1], Kind::Action, &facets),
            Hue::Slot(Slot(SPREAD[0]))
        );

        let legend = colouring.legend(of_actions(&ids), &facets);
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
        let (ids, facets) = actions(&["Zip", "Cc", "Zip", "Cc", "Ar"]);
        let colouring = Colouring::of(Unit::Action, ids.iter().copied(), &facets);
        let legend = colouring.legend(of_actions(&ids), &facets);
        assert_eq!(names(&legend), ["Cc", "Zip", "Ar"]);
    }

    #[test]
    fn kinds_named_alike_read_as_one_legend_entry() {
        let (_, facets) = actions(&[]);
        let targets =
            [Kind::Target, Kind::ConfiguredTarget, Kind::Target].map(|kind| (Id(0), kind));
        let legend = Colouring::Kinds.legend(targets.into_iter(), &facets);
        assert_eq!(names(&legend), ["rule targets", "configured targets"]);
    }
}
