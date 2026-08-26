use crate::{AnyPanel, Layout};
use gpui::Axis;

/// The arrangement a viewer asks for when it opens a document: one panel with a
/// pane to itself, several sharing a pane as tabs, or two arrangements either
/// side of a divider. A workspace translates it into the [`Layout`] of panes it
/// frames.
///
/// The leaf type is a parameter so the translation can be exercised on plain
/// values, away from the entities real panels need.
pub enum PanelGroup<T = AnyPanel> {
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
        first: Box<PanelGroup<T>>,
        second: Box<PanelGroup<T>>,
    },
}

impl<T> PanelGroup<T> {
    /// `first` on show with `rest` tabbed behind it in one pane. One panel is
    /// [`PanelGroup::One`], which a pane frames without a tab strip.
    pub fn tabs(first: T, rest: Vec<T>) -> Self {
        match rest.is_empty() {
            true => Self::One(first),
            false => Self::Tabs { first, rest },
        }
    }

    pub fn split(axis: Axis, ratio: f32, first: PanelGroup<T>, second: PanelGroup<T>) -> Self {
        Self::Split {
            axis,
            ratio,
            first: Box::new(first),
            second: Box::new(second),
        }
    }

    /// The arrangement of panes this group describes, `pane` making each pane
    /// from the panel it shows and the panels tabbed behind it.
    pub fn arrange<P>(self, pane: &mut impl FnMut(T, Vec<T>) -> P) -> Layout<P> {
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

    /// The groups one above the other in the order given, each spanning the
    /// width and laid out at the same share of the height they divide.
    pub fn stack(first: Self, rest: Vec<Self>) -> Self {
        Self::stacked(first, rest.into_iter())
    }

    /// Even shares out of splits that divide in two: `first` is given one share
    /// of the extent and the stack of the groups below it the remaining
    /// `rest.len()`, which that stack divides the same way.
    fn stacked(first: Self, mut rest: std::vec::IntoIter<Self>) -> Self {
        let below = rest.len();
        match rest.next() {
            None => first,
            Some(next) => Self::split(
                Axis::Vertical,
                1.0 / (below + 1) as f32,
                first,
                Self::stacked(next, rest),
            ),
        }
    }

    /// The panels, in the order they are laid out.
    pub fn panels_mut(&mut self) -> Vec<&mut T> {
        let mut panels = Vec::new();
        self.push_panels_mut(&mut panels);
        panels
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
    use crate::layout::shape;

    /// The arrangement `group` describes, each pane written as the panels it
    /// holds in tab order.
    fn arrange(group: PanelGroup<char>) -> Layout<String> {
        group.arrange(&mut |first, rest| std::iter::once(first).chain(rest).collect())
    }

    #[test]
    fn one_panel_is_one_pane_holding_it() {
        assert_eq!(shape(&arrange(PanelGroup::One('a'))), "a");
    }

    /// A pane frames a lone panel with no tab strip, so a group of one is the
    /// same arrangement however it was asked for.
    #[test]
    fn a_group_of_one_tab_is_that_panel_alone() {
        let group = PanelGroup::tabs('a', Vec::new());
        assert!(matches!(group, PanelGroup::One('a')));
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
            PanelGroup::One('a'),
            PanelGroup::tabs('b', vec!['c']),
        );
        assert_eq!(shape(&arrange(group)), "(a/bc)");
    }

    #[test]
    fn a_split_is_laid_out_at_the_share_the_group_asked_for() {
        let group = PanelGroup::split(
            Axis::Horizontal,
            0.3,
            PanelGroup::One('a'),
            PanelGroup::One('b'),
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
                PanelGroup::One('c'),
            ),
            PanelGroup::One('d'),
        );
        let layout = arrange(group);
        assert_eq!(shape(&layout), "((ab|c)/d)");
        assert_eq!(layout.first_pane(), "ab");
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

    /// The stack of `count` panels 'a', 'b', ..., each in a pane of its own.
    fn stack_of(count: usize) -> PanelGroup<char> {
        let panel = |index: usize| PanelGroup::One((b'a' + index as u8) as char);
        PanelGroup::stack(panel(0), (1..count).map(panel).collect())
    }

    /// A window holding one document is laid out by that document alone, so a
    /// stack of one is the arrangement it was given.
    #[test]
    fn a_stack_of_one_group_is_that_group() {
        let group = PanelGroup::split(
            Axis::Vertical,
            0.65,
            PanelGroup::One('a'),
            PanelGroup::tabs('b', vec!['c']),
        );
        let stacked = PanelGroup::stack(group, Vec::new());
        assert_eq!(shape(&arrange(stacked)), "(a/bc)");
    }

    #[test]
    fn a_stack_lays_its_groups_out_in_the_order_given() {
        assert_eq!(shape(&arrange(stack_of(2))), "(a/b)");
        assert_eq!(shape(&arrange(stack_of(3))), "(a/(b/c))");
        assert_eq!(shape(&arrange(stack_of(5))), "(a/(b/(c/(d/e))))");
    }

    #[test]
    fn every_group_of_a_stack_takes_the_same_share_of_the_height() {
        for count in 1..=8 {
            let layout = arrange(stack_of(count));
            let even = 1.0 / count as f32;
            for (share, pane) in regions(&layout, count) {
                assert!(
                    (share - even).abs() < SHARE_ROUNDING,
                    "a stack of {count} lays {pane} out at {share}, not {even}"
                );
            }
        }
    }

    /// Stacking documents does not disturb what each asked for: a region holds
    /// the arrangement its own group describes, splits, tabs and all.
    #[test]
    fn a_stacked_group_keeps_the_arrangement_it_asked_for() {
        let layout = arrange(PanelGroup::stack(
            PanelGroup::One('a'),
            vec![
                PanelGroup::split(
                    Axis::Vertical,
                    0.6,
                    PanelGroup::One('b'),
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
            PanelGroup::One('a'),
            vec![
                PanelGroup::split(
                    Axis::Horizontal,
                    0.5,
                    PanelGroup::One('b'),
                    PanelGroup::One('c'),
                ),
                PanelGroup::One('d'),
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
                PanelGroup::One('c'),
                PanelGroup::One('d'),
            ),
        );
        let borrowed: Vec<char> = group.panels_mut().into_iter().map(|panel| *panel).collect();
        assert_eq!(borrowed, ['a', 'b', 'c', 'd']);
    }
}
