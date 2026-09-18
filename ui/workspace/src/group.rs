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

    /// The weight widened, so that a stack's weights add up to what it divides
    /// between its groups without overflowing what one weight holds.
    fn shares(self) -> f32 {
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

/// One panel with a pane to itself, several sharing a pane as tabs, or several
/// arrangements parted by dividers.
enum Arrangement<T> {
    One(T),
    /// Panels sharing one pane, `first` on show and `rest` tabbed behind it.
    Tabs {
        first: T,
        rest: Vec<T>,
    },
    /// `first` and then each of `rest` laid out one after another along `axis`.
    Split {
        axis: Axis,
        first: Box<Flexed<T>>,
        rest: Vec<Flexed<T>>,
    },
}

/// One side of a split.
struct Flexed<T> {
    arrangement: Arrangement<T>,
    flex: f32,
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
            first: Box::new(Flexed {
                arrangement: first.arrangement,
                flex: ratio,
            }),
            rest: vec![Flexed {
                arrangement: second.arrangement,
                flex: 1.0 - ratio,
            }],
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
    /// width and taking the share of the height its weight asks for. The stack
    /// is one split and a group laid out along its axis joins it, so every
    /// divider between two panes stacked this way is a sibling of the rest and
    /// is grabbed over the whole height.
    pub fn stack(first: Self, rest: Vec<Self>) -> Self {
        if rest.is_empty() {
            return first;
        }
        let flexed = |group: Self| Flexed {
            flex: group.weight.shares(),
            arrangement: group.arrangement,
        };
        Self::of(Arrangement::Split {
            axis: Axis::Vertical,
            first: Box::new(flexed(first)),
            rest: rest.into_iter().map(flexed).collect(),
        })
    }

    /// The same arrangement and weight over what `panel` makes of each panel,
    /// visiting them in the order they are laid out.
    pub(crate) fn map<U>(self, panel: &mut impl FnMut(T) -> U) -> PanelGroup<U> {
        PanelGroup {
            arrangement: self.arrangement.map(panel),
            weight: self.weight,
        }
    }
}

impl<T> Arrangement<T> {
    fn arrange<P>(self, pane: &mut impl FnMut(T, Vec<T>) -> P) -> Layout<P> {
        match self {
            Self::One(panel) => Layout::Pane(pane(panel, Vec::new())),
            Self::Tabs { first, rest } => Layout::Pane(pane(first, rest)),
            Self::Split { axis, first, rest } => {
                let first = *first;
                let leading = (first.arrangement.arrange(pane), first.flex);
                let mut sides = Vec::with_capacity(rest.len());
                for side in rest {
                    sides.push((side.arrangement.arrange(pane), side.flex));
                }
                Layout::divided(axis, leading, sides)
            }
        }
    }

    fn map<U>(self, panel: &mut impl FnMut(T) -> U) -> Arrangement<U> {
        match self {
            Self::One(one) => Arrangement::One(panel(one)),
            Self::Tabs { first, rest } => Arrangement::Tabs {
                first: panel(first),
                rest: rest.into_iter().map(panel).collect(),
            },
            Self::Split { axis, first, rest } => {
                let first = *first;
                Arrangement::Split {
                    axis,
                    first: Box::new(Flexed {
                        arrangement: first.arrangement.map(panel),
                        flex: first.flex,
                    }),
                    rest: rest
                        .into_iter()
                        .map(|side| Flexed {
                            arrangement: side.arrangement.map(panel),
                            flex: side.flex,
                        })
                        .collect(),
                }
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
        let shares: Vec<f32> = split.sides().into_iter().map(|side| side.share).collect();
        assert_eq!(shares, [0.3, 0.7]);
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

    /// A share is divided out of a stack's weights, so an even share is even to
    /// within one division's rounding — far under a pixel of any window.
    const SHARE_ROUNDING: f32 = 1e-6;

    /// The `count` regions a stack is laid out in, each as the share of the
    /// height it takes and the arrangement it holds.
    fn regions(layout: &Layout<String>, count: usize) -> Vec<(f32, String)> {
        if count == 1 {
            return vec![(1.0, shape(layout))];
        }
        let Layout::Split(split) = layout else {
            panic!("a stack of {count} regions is the one split parting them");
        };
        assert_eq!(split.axis(), Axis::Vertical);
        let regions: Vec<(f32, String)> = split
            .sides()
            .into_iter()
            .map(|side| (side.share, shape(side.layout)))
            .collect();
        assert_eq!(regions.len(), count, "a region per side of the stack");
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
    /// the height, top to bottom, which between them are the whole of it: shares
    /// that add up to less leave the window room no pane is laid out in.
    fn assert_shares(weights: &[Weight], expected: &[f32]) {
        assert_eq!(expected.len(), weights.len(), "a share per group stacked");
        let layout = arrange(stack_weighing(weights));
        let regions = regions(&layout, weights.len());
        let whole: f32 = regions.iter().map(|(share, _)| share).sum();
        for ((share, pane), want) in regions.into_iter().zip(expected) {
            assert!(
                (share - want).abs() < SHARE_ROUNDING,
                "{weights:?} lays {pane} out at {share}, not {want}"
            );
        }
        assert!(
            (whole - 1.0).abs() < SHARE_ROUNDING,
            "{weights:?} lays its groups out over {whole} of the height, not all of it"
        );
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

    /// Every group of a stack is a side of the one split, so however many
    /// documents a window holds, each region's divider is a sibling of the rest
    /// and parts the whole height.
    #[test]
    fn a_stack_lays_its_groups_out_side_by_side_in_the_order_given() {
        assert_eq!(shape(&arrange(stack_of(2))), "(a/b)");
        assert_eq!(shape(&arrange(stack_of(3))), "(a/b/c)");
        assert_eq!(shape(&arrange(stack_of(5))), "(a/b/c/d/e)");
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

    /// Stacking documents does not disturb what each asked for: the panes of a
    /// document split along the stack's own axis are laid out as sides of the
    /// stack, in the order and at the shares of the document's own region its
    /// group divided that region into.
    #[test]
    fn a_stacked_group_keeps_the_order_and_the_proportions_it_asked_for() {
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
        let region = 1.0 / 3.0;
        let expected = [
            (region, "a"),
            (region * 0.6, "b"),
            (region * 0.4, "cd"),
            (region, "ef"),
        ];
        for ((share, held), (want, want_held)) in
            regions(&layout, expected.len()).into_iter().zip(expected)
        {
            assert_eq!(held, want_held);
            assert!(
                (share - want).abs() < SHARE_ROUNDING,
                "{held} is laid out at {share} of the height, not {want}"
            );
        }
    }

    /// A document that is a split across the stack's axis keeps a region of its
    /// own, which its own divider parts.
    #[test]
    fn a_stacked_group_split_across_the_stack_keeps_a_region_of_its_own() {
        let layout = arrange(PanelGroup::stack(
            PanelGroup::one('a'),
            vec![PanelGroup::split(
                Axis::Horizontal,
                0.25,
                PanelGroup::one('b'),
                PanelGroup::tabs('c', vec!['d']),
            )],
        ));
        assert_eq!(shape(&layout), "(a/(b|cd))");
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
        assert_eq!(shape(&layout), "(a/(b|c)/d)");
        assert_eq!(layout.remove(&"b".to_string()), Removal::Removed);
        assert_eq!(shape(&layout), "(a/c/d)");
        assert_eq!(layout.remove(&"c".to_string()), Removal::Removed);
        assert_eq!(shape(&layout), "(a/d)");
    }

    /// Mapping a group keeps where each panel stands and what the group weighs,
    /// and meets the panels in the order they are laid out.
    #[test]
    fn a_mapped_group_keeps_its_arrangement_and_weight() {
        let group = PanelGroup::split(
            Axis::Vertical,
            0.5,
            PanelGroup::one('a'),
            PanelGroup::tabs('b', vec!['c']),
        )
        .with_weight(Weight::CHART);
        let mut met = Vec::new();
        let mapped = group.map(&mut |panel| {
            met.push(panel);
            panel.to_ascii_uppercase()
        });
        assert_eq!(met, ['a', 'b', 'c']);
        assert_eq!(mapped.weight(), Weight::CHART);
        assert_eq!(shape(&arrange(mapped)), "(A/BC)");
    }
}
