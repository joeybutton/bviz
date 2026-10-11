use crate::{Layout, Pane};
use gpui::Entity;

/// How a window lays its panes out, and which of them — if any — is drawn in
/// place of the whole of it. The pane drawn in place of the arrangement is one
/// the layout lays out, and drawing it leaves the layout untouched, so putting
/// the panes back leaves every one of them at the share it held.
///
/// The leaf type is a parameter, as [`Layout`]'s is, so the rule can be
/// exercised on plain values away from the window real panes need.
pub(crate) struct Arrangement<P = Entity<Pane>> {
    layout: Layout<P>,
    maximized: Option<P>,
}

/// What asking an arrangement for the whole of a window's body did to it.
#[must_use]
#[derive(PartialEq, Debug)]
pub(crate) enum Maximize<P> {
    /// The pane asked for is drawn in place of the whole arrangement, which
    /// draws the panes laid out besides it no longer.
    Maximized,
    /// Every pane is back where the arrangement lays it out, these being the
    /// ones that brings back on screen: each is owed the repaint the frames
    /// drawn without it consumed.
    Restored(Vec<P>),
    /// Nothing. A window laying out a single pane draws that pane over the
    /// whole of its body already, and an arrangement asked for a pane it does
    /// not lay out has nothing to draw in place of it.
    Unchanged,
}

impl<P: Clone + PartialEq> Arrangement<P> {
    pub(crate) fn new(layout: Layout<P>) -> Self {
        Self {
            layout,
            maximized: None,
        }
    }

    pub(crate) fn layout(&self) -> &Layout<P> {
        &self.layout
    }

    pub(crate) fn maximized(&self) -> Option<&P> {
        self.maximized.as_ref()
    }

    /// Whether the arrangement lays `pane` out somewhere no frame draws it,
    /// another pane being drawn in place of the whole arrangement.
    pub(crate) fn hides(&self, pane: &P) -> bool {
        self.maximized.as_ref().is_some_and(|drawn| drawn != pane) && self.layout.contains(pane)
    }

    /// Whether asking for `pane` would draw it in place of the whole
    /// arrangement: the arrangement has to lay that pane out, beside at least
    /// one other pane for it to be drawn in place of. An arrangement already
    /// drawing a pane in place of the rest answers an ask by putting them back,
    /// so it draws none in place of them afresh.
    pub(crate) fn maximizes(&self, pane: &P) -> bool {
        self.maximized.is_none()
            && self.layout.contains(pane)
            && self.layout.panes().nth(1).is_some()
    }

    /// Draws `pane` in place of the whole arrangement, or puts every pane back
    /// where one is drawn in place of them already. Asking is the only way to
    /// either, so a pane is drawn in place of the rest only from an arrangement
    /// that draws all of them, and none can be hidden behind a pane that is
    /// itself hidden.
    pub(crate) fn toggle_maximize(&mut self, pane: &P) -> Maximize<P> {
        if self.maximized.is_some() {
            return Maximize::Restored(self.restore());
        }
        if !self.maximizes(pane) {
            return Maximize::Unchanged;
        }
        self.maximized = Some(pane.clone());
        Maximize::Maximized
    }

    /// Puts every pane back where the arrangement lays it out, reporting the
    /// panes this brings back on screen.
    pub(crate) fn restore(&mut self) -> Vec<P> {
        match self.maximized.take() {
            Some(maximized) => self.others(&maximized),
            None => Vec::new(),
        }
    }

    /// Puts every pane back where the arrangement hides `pane`, so that a
    /// frame draws it, reporting the panes this brings back on screen. A pane
    /// drawn already, the maximized one among them, leaves the arrangement as
    /// it stands.
    pub(crate) fn uncover(&mut self, pane: &P) -> Vec<P> {
        if self.hides(pane) {
            self.restore()
        } else {
            Vec::new()
        }
    }

    /// The arrangement, to change. A pane drawn in place of the whole of it is
    /// put back first, and this is the only way to an arrangement that can be
    /// changed, so no change reaches one part of which is hidden.
    pub(crate) fn rearrange(&mut self) -> &mut Layout<P> {
        self.maximized = None;
        &mut self.layout
    }

    /// The panes the arrangement lays out besides `pane`.
    fn others(&self, pane: &P) -> Vec<P> {
        let mut others = Vec::with_capacity(self.layout.panes().count());
        others.extend(self.layout.panes().filter(|held| *held != pane).cloned());
        others
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::shape;
    use crate::{DividerId, Removal, Side};
    use gpui::Axis;

    /// Panes 'a', 'b' and 'c' side by side, each asking for as much of the
    /// window as the others.
    fn row() -> Arrangement<char> {
        Arrangement::new(Layout::divided(
            Axis::Horizontal,
            (Layout::Pane('a'), 1.0),
            vec![(Layout::Pane('b'), 1.0), (Layout::Pane('c'), 1.0)],
        ))
    }

    /// Pane 'a' beside 'b' above 'c', so that the panes a maximized one hides
    /// are not all laid out by the split that lays it out.
    fn nested() -> Arrangement<char> {
        Arrangement::new(Layout::split_at(
            Axis::Horizontal,
            0.5,
            Layout::Pane('a'),
            Layout::split_at(Axis::Vertical, 0.5, Layout::Pane('b'), Layout::Pane('c')),
        ))
    }

    /// The shares the outermost split lays its sides out at.
    fn shares(arrangement: &Arrangement<char>) -> Vec<f32> {
        match arrangement.layout() {
            Layout::Pane(_) => Vec::new(),
            Layout::Split(split) => split.sides().into_iter().map(|side| side.share).collect(),
        }
    }

    /// Draws `pane` in place of the whole arrangement, which is what asking of
    /// an arrangement that draws every pane it lays out does.
    fn maximize(arrangement: &mut Arrangement<char>, pane: char) {
        assert_eq!(arrangement.toggle_maximize(&pane), Maximize::Maximized);
    }

    /// The divider at the first boundary of the arrangement's outermost split.
    fn first_divider(arrangement: &Arrangement<char>) -> DividerId {
        match arrangement.layout() {
            Layout::Pane(_) => panic!("the arrangement is a split"),
            Layout::Split(split) => split.sides()[1]
                .divider
                .expect("a side laid out after another is laid out against a divider"),
        }
    }

    #[test]
    fn a_maximized_pane_is_drawn_in_place_of_the_panes_it_hides() {
        let mut arrangement = row();
        assert_eq!(arrangement.toggle_maximize(&'b'), Maximize::Maximized);
        assert_eq!(arrangement.maximized(), Some(&'b'));
        assert!(arrangement.hides(&'a'));
        assert!(arrangement.hides(&'c'));
        assert!(!arrangement.hides(&'b'));
    }

    /// A pane is drawn in place of every pane the arrangement lays out, not of
    /// the ones laid out beside it by the split it is in.
    #[test]
    fn a_maximized_pane_hides_the_panes_around_it_at_every_depth() {
        let mut arrangement = nested();
        assert_eq!(arrangement.toggle_maximize(&'c'), Maximize::Maximized);
        assert_eq!(shape(arrangement.layout()), "(a|(b/c))");
        assert!(arrangement.hides(&'a'));
        assert!(arrangement.hides(&'b'));
        assert!(!arrangement.hides(&'c'));
    }

    #[test]
    fn an_arrangement_with_nothing_maximized_hides_no_pane() {
        let arrangement = row();
        assert_eq!(arrangement.maximized(), None);
        for pane in ['a', 'b', 'c'] {
            assert!(!arrangement.hides(&pane));
        }
    }

    /// A pane the arrangement does not lay out stands nowhere in the window,
    /// so there is nothing of it for another pane to be drawn in place of.
    #[test]
    fn an_arrangement_hides_no_pane_it_does_not_lay_out() {
        let mut arrangement = row();
        assert!(!arrangement.hides(&'z'));
        maximize(&mut arrangement, 'b');
        assert!(!arrangement.hides(&'z'));
    }

    /// A window laying out a single pane draws that pane over the whole of its
    /// body already, so the gesture asking for it has nothing to do.
    #[test]
    fn a_lone_pane_has_nothing_to_maximize() {
        let mut arrangement = Arrangement::new(Layout::Pane('a'));
        assert_eq!(arrangement.toggle_maximize(&'a'), Maximize::Unchanged);
        assert_eq!(arrangement.maximized(), None);
        assert!(!arrangement.hides(&'a'));
    }

    #[test]
    fn maximizing_a_pane_the_arrangement_does_not_lay_out_hides_nothing() {
        let mut arrangement = row();
        assert_eq!(arrangement.toggle_maximize(&'z'), Maximize::Unchanged);
        assert_eq!(arrangement.maximized(), None);
    }

    /// Asking while a pane is drawn in place of the rest asks for them back,
    /// whichever pane is named — so one pane is never drawn in place of another
    /// that is itself holding the rest back.
    #[test]
    fn asking_while_a_pane_is_maximized_puts_the_panes_back() {
        for asked in ['a', 'b', 'z'] {
            let mut arrangement = row();
            maximize(&mut arrangement, 'b');
            assert_eq!(
                arrangement.toggle_maximize(&asked),
                Maximize::Restored(vec!['a', 'c'])
            );
            assert_eq!(arrangement.maximized(), None);
        }
    }

    /// The panes a restore reports are the ones it brings back on screen, which
    /// is none where every pane is on screen already.
    #[test]
    fn restoring_reports_the_panes_it_brings_back() {
        let mut arrangement = row();
        assert!(arrangement.restore().is_empty());
        maximize(&mut arrangement, 'a');
        assert_eq!(arrangement.restore(), ['b', 'c']);
        assert_eq!(arrangement.maximized(), None);
        assert!(arrangement.restore().is_empty());
    }

    /// Uncovering a pane the arrangement hides puts every pane back, and
    /// uncovering one it draws — the maximized pane, or any pane where none
    /// is maximized — changes nothing.
    #[test]
    fn uncovering_puts_the_panes_back_only_for_a_hidden_pane() {
        let mut arrangement = row();
        assert!(arrangement.uncover(&'a').is_empty());
        maximize(&mut arrangement, 'b');
        assert!(arrangement.uncover(&'b').is_empty());
        assert_eq!(arrangement.maximized(), Some(&'b'));
        assert!(arrangement.uncover(&'z').is_empty());
        assert_eq!(arrangement.maximized(), Some(&'b'));
        assert_eq!(arrangement.uncover(&'a'), ['a', 'c']);
        assert_eq!(arrangement.maximized(), None);
    }

    /// Every pane the arrangement lays out besides the one drawn in place of
    /// them is reported, however deeply the arrangement nests it, so a caller
    /// marking them leaves none of them showing what a frame that left it out
    /// consumed.
    #[test]
    fn restoring_reports_every_pane_at_every_depth() {
        let mut arrangement = nested();
        maximize(&mut arrangement, 'b');
        assert_eq!(arrangement.restore(), ['a', 'c']);
    }

    /// The accessor that hands the arrangement over to be changed puts a
    /// maximized pane back, and the change lands on the arrangement it hands
    /// over rather than on a copy of it.
    #[test]
    fn a_change_lands_on_the_arrangement_a_maximized_pane_is_put_back_into() {
        /// A change to an arrangement, and the shape it leaves it in.
        type Change = (fn(&mut Layout<char>), &'static str);
        let changes: [Change; 4] = [
            (
                |layout| {
                    layout.remove(&'c');
                },
                "(a|b)",
            ),
            (
                |layout| {
                    layout.remove(&'b');
                },
                "(a|c)",
            ),
            (|layout| layout.insert_at_edge(Side::Left, 'z'), "(z|a|b|c)"),
            (
                |layout| {
                    layout.insert_beside(&'b', Side::Bottom, 'z');
                },
                "(a|(b/z)|c)",
            ),
        ];
        for (change, shaped) in changes {
            let mut arrangement = row();
            maximize(&mut arrangement, 'b');
            change(arrangement.rearrange());
            assert_eq!(arrangement.maximized(), None);
            assert_eq!(shape(arrangement.layout()), shaped);
            assert!(!arrangement.hides(&'a'));
        }
    }

    /// A divider dragged while a pane is drawn in place of the rest re-divides
    /// the arrangement they come back into.
    #[test]
    fn a_divider_moved_while_a_pane_is_maximized_re_divides_the_arrangement() {
        let mut arrangement = Arrangement::new(Layout::split_at(
            Axis::Horizontal,
            0.5,
            Layout::Pane('a'),
            Layout::Pane('b'),
        ));
        maximize(&mut arrangement, 'a');
        let divider = first_divider(&arrangement);
        assert!(arrangement.rearrange().move_divider(divider, 0.25));
        assert_eq!(arrangement.maximized(), None);
        assert_eq!(shares(&arrangement), [0.25, 0.75]);
    }

    /// Closing the very pane drawn in place of the rest takes it out and leaves
    /// the window drawing every pane it still lays out.
    #[test]
    fn closing_the_maximized_pane_leaves_the_panes_left_on_screen() {
        let mut arrangement = row();
        maximize(&mut arrangement, 'b');
        assert_eq!(arrangement.rearrange().remove(&'b'), Removal::Removed);
        assert_eq!(arrangement.maximized(), None);
        assert_eq!(shape(arrangement.layout()), "(a|c)");
        for pane in ['a', 'c'] {
            assert!(!arrangement.hides(&pane));
        }
    }

    /// Maximizing leaves the arrangement itself alone, so restoring puts every
    /// pane back at the share it held rather than at an even one.
    #[test]
    fn restoring_leaves_every_pane_at_the_share_it_held() {
        let mut arrangement = Arrangement::new(Layout::divided(
            Axis::Horizontal,
            (Layout::Pane('a'), 1.0),
            vec![(Layout::Pane('b'), 3.0)],
        ));
        let held = shares(&arrangement);
        assert_eq!(held, [0.25, 0.75]);
        maximize(&mut arrangement, 'a');
        assert_eq!(shape(arrangement.layout()), "(a|b)");
        arrangement.restore();
        assert_eq!(shares(&arrangement), held);
    }
}
