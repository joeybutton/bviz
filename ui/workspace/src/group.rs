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

    /// The panels, in the order they are laid out.
    pub fn panels(self) -> Vec<T> {
        let mut panels = Vec::new();
        self.push_panels(&mut panels);
        panels
    }

    fn push_panels(self, out: &mut Vec<T>) {
        match self {
            Self::One(panel) => out.push(panel),
            Self::Tabs { first, rest } => {
                out.push(first);
                out.extend(rest);
            }
            Self::Split { first, second, .. } => {
                first.push_panels(out);
                second.push_panels(out);
            }
        }
    }

    /// The panels borrowed, in the order they are laid out.
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
        assert_eq!(group.panels(), ['a', 'b', 'c', 'd']);
    }
}
