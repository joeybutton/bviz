use crate::{AnyPanel, Layout};
use gpui::Axis;

/// How panels are arranged in panes, which a workspace translates into the
/// [`Layout`] of panes it frames.
///
/// The leaf type is a parameter so the translation can be exercised on plain
/// values, away from the entities real panels need.
pub(crate) struct PanelGroup<T = AnyPanel> {
    arrangement: Arrangement<T>,
}

/// One panel with a pane to itself, several sharing a pane as tabs, or two
/// arrangements parted by a divider.
enum Arrangement<T> {
    One(T),
    /// Panels sharing one pane, `first` on show and `rest` tabbed behind it.
    Tabs {
        first: T,
        rest: Vec<T>,
    },
    /// `first` then `second` along `axis`, `first` at `ratio` of the extent.
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<Arrangement<T>>,
        second: Box<Arrangement<T>>,
    },
}

impl<T> PanelGroup<T> {
    fn of(arrangement: Arrangement<T>) -> Self {
        Self { arrangement }
    }

    /// One panel, which a pane frames without a tab strip.
    pub(crate) fn one(panel: T) -> Self {
        Self::of(Arrangement::One(panel))
    }

    /// `first` on show with `rest` tabbed behind it in one pane. A pane frames
    /// a lone panel without a tab strip, so a group of one panel is
    /// [`PanelGroup::one`] however it was asked for.
    pub(crate) fn tabs(first: T, rest: Vec<T>) -> Self {
        Self::of(Arrangement::tabbed(first, rest))
    }

    /// `first` and `second` either side of a divider along `axis`, the first at
    /// `ratio` of the extent they share.
    pub(crate) fn split(axis: Axis, ratio: f32, first: Self, second: Self) -> Self {
        Self::of(Arrangement::Split {
            axis,
            ratio,
            first: Box::new(first.arrangement),
            second: Box::new(second.arrangement),
        })
    }

    /// The arrangement of panes this group describes, `pane` making each pane
    /// from the panel it shows and the panels tabbed behind it.
    pub(crate) fn arrange<P>(self, pane: &mut impl FnMut(T, Vec<T>) -> P) -> Layout<P> {
        self.arrangement.arrange(pane)
    }
}

impl<T> PanelGroup<Option<T>> {
    /// The same arrangement over the panels that are there, the empty places
    /// taking no part: a pane is framed around the tabs left to it, and a split
    /// left one side is that side alone. `None` where no panel is there at all.
    pub(crate) fn compact(self) -> Option<PanelGroup<T>> {
        self.arrangement.compact().map(PanelGroup::of)
    }
}

impl<T> Arrangement<T> {
    fn tabbed(first: T, rest: Vec<T>) -> Self {
        match rest.is_empty() {
            true => Self::One(first),
            false => Self::Tabs { first, rest },
        }
    }

    fn arrange<P>(self, pane: &mut impl FnMut(T, Vec<T>) -> P) -> Layout<P> {
        match self {
            Self::One(panel) => Layout::Pane(pane(panel, Vec::new())),
            Self::Tabs { first, rest } => Layout::Pane(pane(first, rest)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => {
                let first = first.arrange(pane);
                Layout::split_at(axis, ratio, first, second.arrange(pane))
            }
        }
    }
}

impl<T> Arrangement<Option<T>> {
    fn compact(self) -> Option<Arrangement<T>> {
        match self {
            Self::One(one) => Some(Arrangement::One(one?)),
            Self::Tabs { first, rest } => {
                let mut left = first.into_iter().chain(rest.into_iter().flatten());
                Some(Arrangement::tabbed(left.next()?, left.collect()))
            }
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => match (first.compact(), second.compact()) {
                (Some(first), Some(second)) => Some(Arrangement::Split {
                    axis,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (side, None) | (None, side) => side,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::shape;
    use crate::testing::{arrange, assert_extents};

    #[test]
    fn one_panel_is_one_pane_holding_it() {
        assert_eq!(shape(&arrange(PanelGroup::one('a'))), "a");
    }

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
        assert_extents(&arrange(group), &[("a", 0.3, 1.0), ("b", 0.7, 1.0)]);
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

    /// `panel` where `kept` holds it, and an empty place otherwise.
    fn kept_of(kept: &str, panel: char) -> Option<char> {
        kept.contains(panel).then_some(panel)
    }

    /// The group of 'a' to 'f' — 'a' over a pane of 'b' and 'c' tabbed, beside
    /// 'd' over 'e' and 'f' tabbed — with the panels `kept` leaves out taking
    /// no place in it.
    fn compacted(kept: &str) -> Option<PanelGroup<char>> {
        let one = |panel| PanelGroup::one(kept_of(kept, panel));
        let tabs =
            |first, second| PanelGroup::tabs(kept_of(kept, first), vec![kept_of(kept, second)]);
        PanelGroup::split(
            Axis::Horizontal,
            0.25,
            PanelGroup::split(Axis::Vertical, 0.5, one('a'), tabs('b', 'c')),
            PanelGroup::split(Axis::Vertical, 0.5, one('d'), tabs('e', 'f')),
        )
        .compact()
    }

    /// The shape of the group of `kept`, which has to hold panels at all.
    fn compacted_shape(kept: &str) -> String {
        shape(&arrange(compacted(kept).expect("a group of panels")))
    }

    #[test]
    fn a_group_of_every_panel_is_the_group_itself() {
        assert_eq!(compacted_shape("abcdef"), "((a/bc)|(d/ef))");
    }

    /// A pane goes on framing the tabs left to it, and shows the first of them.
    #[test]
    fn a_pane_keeps_the_tabs_left_to_it() {
        assert_eq!(compacted_shape("abdef"), "((a/b)|(d/ef))");
        assert_eq!(compacted_shape("acdef"), "((a/c)|(d/ef))");
    }

    /// The shape of the pane of 'a', 'b' and 'c' tabbed when only `kept` are
    /// left to it.
    fn tabs_kept(kept: &str) -> String {
        let pane = PanelGroup::tabs(
            kept_of(kept, 'a'),
            vec![kept_of(kept, 'b'), kept_of(kept, 'c')],
        )
        .compact()
        .expect("a pane of tabs");
        shape(&arrange(pane))
    }

    /// A pane holding more than two tabs keeps whichever are left to it in the
    /// order it held them, whether what closes is the tab it shows or one
    /// behind it, and shows the first of those left.
    #[test]
    fn a_pane_of_several_tabs_keeps_the_order_it_held_them_in() {
        assert_eq!(tabs_kept("abc"), "abc");
        assert_eq!(tabs_kept("bc"), "bc");
        assert_eq!(tabs_kept("ac"), "ac");
    }

    /// A side left on its own takes the whole extent of the split, the split
    /// that parted it from the other being gone with it.
    #[test]
    fn a_split_left_one_side_is_that_side() {
        assert_eq!(compacted_shape("abc"), "(a/bc)");
        assert_eq!(compacted_shape("def"), "(d/ef)");
        assert_eq!(compacted_shape("bcdef"), "(bc|(d/ef))");
        assert_eq!(compacted_shape("ad"), "(a|d)");
        assert_eq!(compacted_shape("e"), "e");
    }

    /// A split none of whose sides is left goes with them, and the group around
    /// it is laid out as though it had never been there.
    #[test]
    fn a_split_losing_every_side_goes_with_them() {
        assert_eq!(compacted_shape("a"), "a");
        assert_eq!(compacted_shape("bc"), "bc");
    }

    #[test]
    fn a_group_of_no_panels_at_all_is_nothing() {
        assert!(compacted("").is_none());
    }

    /// A split that is the side left to another keeps the proportions its own
    /// sides asked for.
    #[test]
    fn the_sides_left_to_a_split_hold_the_proportions_they_asked_for() {
        let kept = |panel| PanelGroup::one(kept_of("bc", panel));
        let group = PanelGroup::split(
            Axis::Vertical,
            0.25,
            kept('a'),
            PanelGroup::split(Axis::Vertical, 0.4, kept('b'), kept('c')),
        )
        .compact()
        .expect("a group of two panels");
        assert_extents(&arrange(group), &[("b", 1.0, 0.4), ("c", 1.0, 0.6)]);
    }
}
