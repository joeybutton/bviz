use crate::{AnyPanel, Layout};
use gpui::Axis;
use std::num::NonZeroU16;

/// The share of a window a document asks for beside the documents it is stacked
/// with: a group of twice another's weight is laid out at twice its height.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Weight(NonZeroU16);

impl Weight {
    /// What a view of text asks for. A log is read in lines, so past the lines
    /// worth having in view at once, more height shows more of nothing.
    pub const LOG: Self = Self::new(1);

    /// What a view drawing over the whole extent it is given asks for. A canvas
    /// or a timeline resolves more of its document at every height, and so is
    /// worth stacking at twice a view of text.
    pub const CHART: Self = Self::new(2);

    /// `shares` of the height, at least one: a document laid out at no share of
    /// the window would be laid out at no height at all.
    pub const fn new(shares: u16) -> Self {
        Self(match NonZeroU16::new(shares) {
            Some(shares) => shares,
            None => NonZeroU16::MIN,
        })
    }

    /// Widened to add up with the weights of a stack without overflowing.
    fn shares(self) -> u32 {
        self.0.get().into()
    }
}

impl Default for Weight {
    /// One share, so groups that ask for no weight of their own divide the
    /// height they share evenly.
    fn default() -> Self {
        Self::new(1)
    }
}

/// What a viewer asks for when it opens a document: how the panels showing it
/// are arranged, and what the document is worth where a window holds several. A
/// workspace translates the arrangement into the [`Layout`] of panes it frames.
///
/// The leaf type is a parameter so the translation can be exercised on plain
/// values, away from the entities real panels need.
pub struct PanelGroup<T = AnyPanel> {
    arrangement: Arrangement<T>,
    weight: Weight,
}

/// One panel with a pane to itself, several sharing a pane as tabs, or two
/// arrangements either side of a divider.
enum Arrangement<T> {
    One(T),
    /// Panels sharing one pane, `first` on show and `rest` tabbed behind it.
    Tabs {
        first: T,
        rest: Vec<T>,
    },
    /// Two arrangements laid out along `axis`, the first at `ratio` of the
    /// extent they share.
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Arrangement<T>>,
        second: Box<Arrangement<T>>,
    },
}

impl<T> PanelGroup<T> {
    fn of(arrangement: Arrangement<T>) -> Self {
        Self {
            arrangement,
            weight: Weight::default(),
        }
    }

    /// One panel, which a pane frames without a tab strip.
    pub fn one(panel: T) -> Self {
        Self::of(Arrangement::One(panel))
    }

    /// `first` on show with `rest` tabbed behind it in one pane. One panel is
    /// [`PanelGroup::one`].
    pub fn tabs(first: T, rest: Vec<T>) -> Self {
        match rest.is_empty() {
            true => Self::one(first),
            false => Self::of(Arrangement::Tabs { first, rest }),
        }
    }

    /// `first` and `second` either side of a divider along `axis`, the first at
    /// `ratio` of the extent they share whatever either side weighs.
    pub fn split(axis: Axis, ratio: f32, first: Self, second: Self) -> Self {
        Self::of(Arrangement::Split {
            axis,
            ratio,
            first: Box::new(first.arrangement),
            second: Box::new(second.arrangement),
        })
    }

    /// The same group, asking for `weight` of the window.
    pub fn with_weight(mut self, weight: Weight) -> Self {
        self.weight = weight;
        self
    }

    pub fn weight(&self) -> Weight {
        self.weight
    }

    /// The arrangement of panes this group describes, `pane` making each pane
    /// from the panel it shows and the panels tabbed behind it.
    pub fn arrange<P>(self, pane: &mut impl FnMut(T, Vec<T>) -> P) -> Layout<P> {
        self.arrangement.arrange(pane)
    }

    /// The groups one above the other in the order given, each spanning the
    /// width and taking the share of the height its weight asks for.
    pub fn stack(first: Self, rest: Vec<Self>) -> Self {
        Self::stacked(first, rest.into_iter())
    }

    /// Weighted shares out of splits that divide in two: `first` is given its
    /// own weight out of what the whole stack weighs, and the stack of the
    /// groups below it divides the rest the same way.
    fn stacked(first: Self, mut rest: std::vec::IntoIter<Self>) -> Self {
        let below: u32 = rest
            .as_slice()
            .iter()
            .map(|group| group.weight.shares())
            .sum();
        match rest.next() {
            None => first,
            Some(next) => {
                let share = first.weight.shares() as f32 / (first.weight.shares() + below) as f32;
                Self::split(Axis::Vertical, share, first, Self::stacked(next, rest))
            }
        }
    }

    /// The panels, in the order they are laid out.
    pub fn panels_mut(&mut self) -> Vec<&mut T> {
        let mut panels = Vec::new();
        self.arrangement.push_panels_mut(&mut panels);
        panels
    }
}

impl<T> Arrangement<T> {
    fn arrange<P>(self, pane: &mut impl FnMut(T, Vec<T>) -> P) -> Layout<P> {
        match self {
            Self::One(panel) => Layout::Pane(pane(panel, Vec::new())),
            Self::Tabs { first, rest } => Layout::Pane(pane(first, rest)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => Layout::split_at(axis, ratio, first.arrange(pane), second.arrange(pane)),
        }
    }

    fn push_panels_mut<'a>(&'a mut self, out: &mut Vec<&'a mut T>) {
        match self {
            Self::One(panel) => out.push(panel),
            Self::Tabs { first, rest } => {
                out.push(first);
                out.extend(rest);
            }
            Self::Split { first, second, .. } => {
                first.push_panels_mut(out);
                second.push_panels_mut(out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Removal;
    use crate::layout::{MIN_PANE_SHARE, shape};

    /// The arrangement `group` describes, each pane written as the panels it
    /// holds in tab order.
    fn arrange(group: PanelGroup<char>) -> Layout<String> {
        group.arrange(&mut |first, rest| std::iter::once(first).chain(rest).collect())
    }

    #[test]
    fn one_panel_is_one_pane_holding_it() {
        assert_eq!(shape(&arrange(PanelGroup::one('a'))), "a");
    }

    /// A pane frames a lone panel with no tab strip, so a group of one is the
    /// same arrangement however it was asked for.
    #[test]
    fn a_group_of_one_tab_is_that_panel_alone() {
        let group = PanelGroup::tabs('a', Vec::new());
        assert!(matches!(group.arrangement, Arrangement::One('a')));
        assert_eq!(shape(&arrange(group)), "a");
    }

    #[test]
    fn tabbed_panels_share_one_pane_in_the_order_given() {
        assert_eq!(
            shape(&arrange(PanelGroup::tabs('a', vec!['b', 'c']))),
            "abc"
        );
    }

    #[test]
    fn a_split_lays_its_two_sides_out_along_its_axis() {
        let group = PanelGroup::split(
            Axis::Vertical,
            0.65,
            PanelGroup::one('a'),
            PanelGroup::tabs('b', vec!['c']),
        );
        assert_eq!(shape(&arrange(group)), "(a/bc)");
    }

    #[test]
    fn a_split_is_laid_out_at_the_share_the_group_asked_for() {
        let group = PanelGroup::split(
            Axis::Horizontal,
            0.3,
            PanelGroup::one('a'),
            PanelGroup::one('b'),
        );
        let Layout::Split(split) = arrange(group) else {
            panic!("a split group arranges into a split");
        };
        assert_eq!(split.ratio(), 0.3);
        assert_eq!(split.axis(), Axis::Horizontal);
    }

    /// The pane a window opens showing is the one holding the group's leading
    /// panel, however deeply the arrangement nests it.
    #[test]
    fn the_first_pane_holds_the_leading_panel() {
        let group = PanelGroup::split(
            Axis::Vertical,
            0.5,
            PanelGroup::split(
                Axis::Horizontal,
                0.5,
                PanelGroup::tabs('a', vec!['b']),
                PanelGroup::one('c'),
            ),
            PanelGroup::one('d'),
        );
        let layout = arrange(group);
        assert_eq!(shape(&layout), "((ab|c)/d)");
        assert_eq!(layout.first_pane(), "ab");
    }

    #[test]
    fn a_group_weighs_one_share_until_it_asks_for_another() {
        assert_eq!(PanelGroup::one('a').weight(), Weight::default());
        assert_eq!(
            PanelGroup::one('a').with_weight(Weight::CHART).weight(),
            Weight::CHART
        );
    }

    /// A document asking for no share of the window is asking for the least
    /// there is to ask for, not for none.
    #[test]
    fn a_weight_of_no_shares_is_a_weight_of_one() {
        assert_eq!(Weight::new(0), Weight::new(1));
    }

    /// A share is divided out through the ratios of nested splits, so an even
    /// share is even to within the rounding of a few divisions — far under a
    /// pixel of any window.
    const SHARE_ROUNDING: f32 = 1e-6;

    /// The regions a stack of `count` groups is laid out in, each as the share
    /// of the height it takes and the arrangement it holds.
    fn regions(layout: &Layout<String>, count: usize) -> Vec<(f32, String)> {
        let mut regions = Vec::with_capacity(count);
        let mut remaining = layout;
        let mut left = 1.0;
        for _ in 1..count {
            let Layout::Split(split) = remaining else {
                panic!(
                    "a stack of {count} groups divides the height {} times",
                    count - 1
                );
            };
            assert_eq!(split.axis(), Axis::Vertical);
            regions.push((left * split.ratio(), shape(split.first())));
            left *= 1.0 - split.ratio();
            remaining = split.second();
        }
        regions.push((left, shape(remaining)));
        regions
    }

    /// The stack of a panel per weight, 'a', 'b', ..., each in a pane of its own
    /// asking for the weight given for it.
    fn stack_weighing(weights: &[Weight]) -> PanelGroup<char> {
        let panel = |(index, weight): (usize, &Weight)| {
            PanelGroup::one((b'a' + index as u8) as char).with_weight(*weight)
        };
        let mut groups = weights.iter().enumerate().map(panel);
        let first = groups.next().expect("a stack holds a group");
        PanelGroup::stack(first, groups.collect())
    }

    /// The stack of `count` panels 'a', 'b', ..., none asking for a weight of
    /// its own.
    fn stack_of(count: usize) -> PanelGroup<char> {
        stack_weighing(&vec![Weight::default(); count])
    }

    /// Asserts a stack of `weights` lays its groups out at `expected` shares of
    /// the height, top to bottom.
    fn assert_shares(weights: &[Weight], expected: &[f32]) {
        let layout = arrange(stack_weighing(weights));
        for ((share, pane), want) in regions(&layout, weights.len()).into_iter().zip(expected) {
            assert!(
                (share - want).abs() < SHARE_ROUNDING,
                "{weights:?} lays {pane} out at {share}, not {want}"
            );
        }
    }

    /// A window holding one document is laid out by that document alone, so a
    /// stack of one is the arrangement it was given, whatever it would have
    /// asked for beside others.
    #[test]
    fn a_stack_of_one_group_is_that_group() {
        for weight in [
            Weight::default(),
            Weight::LOG,
            Weight::CHART,
            Weight::new(9),
        ] {
            let group = PanelGroup::split(
                Axis::Vertical,
                0.65,
                PanelGroup::one('a'),
                PanelGroup::tabs('b', vec!['c']),
            )
            .with_weight(weight);
            let stacked = PanelGroup::stack(group, Vec::new());
            assert_eq!(shape(&arrange(stacked)), "(a/bc)");
        }
    }

    #[test]
    fn a_stack_lays_its_groups_out_in_the_order_given() {
        assert_eq!(shape(&arrange(stack_of(2))), "(a/b)");
        assert_eq!(shape(&arrange(stack_of(3))), "(a/(b/c))");
        assert_eq!(shape(&arrange(stack_of(5))), "(a/(b/(c/(d/e))))");
    }

    #[test]
    fn every_group_of_an_unweighted_stack_takes_the_same_share_of_the_height() {
        for count in 1..=8 {
            let even = 1.0 / count as f32;
            assert_shares(&vec![Weight::default(); count], &vec![even; count]);
        }
    }

    #[test]
    fn a_stack_divides_the_height_in_proportion_to_what_its_groups_weigh() {
        assert_shares(
            &[Weight::CHART, Weight::CHART, Weight::LOG],
            &[0.4, 0.4, 0.2],
        );
        assert_shares(&[Weight::new(1), Weight::new(3)], &[0.25, 0.75]);
        assert_shares(
            &[
                Weight::new(1),
                Weight::new(2),
                Weight::new(3),
                Weight::new(4),
            ],
            &[0.1, 0.2, 0.3, 0.4],
        );
        assert_shares(
            &[
                Weight::new(5),
                Weight::new(1),
                Weight::new(1),
                Weight::new(1),
            ],
            &[0.625, 0.125, 0.125, 0.125],
        );
    }

    /// Weighing a document heavily enough would squeeze the ones stacked with it
    /// out of reach, which no split allows: a group asking for less than the
    /// least a pane is laid out at is given that least share instead.
    #[test]
    fn a_group_outweighed_far_enough_still_takes_the_least_share_a_pane_takes() {
        let heavy = Weight::new(30);
        assert_shares(
            &[Weight::LOG, heavy],
            &[MIN_PANE_SHARE, 1.0 - MIN_PANE_SHARE],
        );
        assert_shares(
            &[heavy, Weight::LOG],
            &[1.0 - MIN_PANE_SHARE, MIN_PANE_SHARE],
        );
    }

    /// Stacking documents does not disturb what each asked for: a region holds
    /// the arrangement its own group describes, splits, tabs and all.
    #[test]
    fn a_stacked_group_keeps_the_arrangement_it_asked_for() {
        let layout = arrange(PanelGroup::stack(
            PanelGroup::one('a'),
            vec![
                PanelGroup::split(
                    Axis::Vertical,
                    0.6,
                    PanelGroup::one('b'),
                    PanelGroup::tabs('c', vec!['d']),
                ),
                PanelGroup::tabs('e', vec!['f']),
            ],
        ));
        let held: Vec<String> = regions(&layout, 3)
            .into_iter()
            .map(|(_, arrangement)| arrangement)
            .collect();
        assert_eq!(held, ["a", "(b/cd)", "ef"]);
    }

    /// A pane leaves the arrangement as its last panel closes, and the region a
    /// document held closes with its last pane while the documents stacked
    /// around it stand.
    #[test]
    fn closing_every_pane_of_a_stacked_group_leaves_the_others_stacked() {
        let mut layout = arrange(PanelGroup::stack(
            PanelGroup::one('a'),
            vec![
                PanelGroup::split(
                    Axis::Horizontal,
                    0.5,
                    PanelGroup::one('b'),
                    PanelGroup::one('c'),
                ),
                PanelGroup::one('d'),
            ],
        ));
        assert_eq!(shape(&layout), "(a/((b|c)/d))");
        assert_eq!(layout.remove(&"b".to_string()), Removal::Removed);
        assert_eq!(shape(&layout), "(a/(c/d))");
        assert_eq!(layout.remove(&"c".to_string()), Removal::Removed);
        assert_eq!(shape(&layout), "(a/d)");
    }

    #[test]
    fn the_panels_read_in_the_order_they_are_laid_out() {
        let mut group = PanelGroup::split(
            Axis::Vertical,
            0.5,
            PanelGroup::tabs('a', vec!['b']),
            PanelGroup::split(
                Axis::Horizontal,
                0.5,
                PanelGroup::one('c'),
                PanelGroup::one('d'),
            ),
        );
        let borrowed: Vec<char> = group.panels_mut().into_iter().map(|panel| *panel).collect();
        assert_eq!(borrowed, ['a', 'b', 'c', 'd']);
    }
}
