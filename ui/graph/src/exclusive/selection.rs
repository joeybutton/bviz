//! The steps the keys take a selection through the cells of the icicle.

use super::icicle::{Cell, Tree, row_under, spans};

/// Where a key takes the selection from a cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// To the sibling on its left.
    Previous,
    /// To the sibling on its right.
    Next,
    /// To the cell it stands under.
    Parent,
    /// To the heaviest of the cells under it.
    Child,
}

/// The cell `step` takes the selection to from `from`, among `cells` laid
/// out by [`super::icicle::lay_out`] from `tree`; none where there is no
/// cell that way.
pub(crate) fn stepped(cells: &[Cell], from: &Cell, step: Step, tree: &impl Tree) -> Option<Cell> {
    let under = |above: &Cell| {
        let above = above.rect;
        cells
            .iter()
            .filter(move |below| row_under(above, below.rect) && spans(above, below.rect))
    };
    let parent = || {
        let mut over = cells.iter();
        over.find(|above| row_under(above.rect, from.rect) && spans(above.rect, from.rect))
    };
    match step {
        Step::Parent => parent().copied(),
        Step::Child => {
            let mut heaviest: Option<&Cell> = None;
            for child in under(from) {
                let weight = child.holds.weight(tree);
                if heaviest.is_none_or(|heaviest| heaviest.holds.weight(tree) < weight) {
                    heaviest = Some(child);
                }
            }
            heaviest.copied()
        }
        Step::Previous | Step::Next => {
            let mut siblings: Vec<&Cell> = under(parent()?).collect();
            siblings.sort_by(|a, b| f32::from(a.rect.left()).total_cmp(&f32::from(b.rect.left())));
            let at = siblings
                .iter()
                .position(|sibling| sibling.holds == from.holds)?;
            let to = match step {
                Step::Previous => at.checked_sub(1)?,
                _ => at + 1,
            };
            siblings.get(to).map(|&&sibling| sibling)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exclusive::icicle::tests::{Laid, ROW, Spec, area, leaf};
    use crate::exclusive::icicle::{Holds, Parent, lay_out};
    use gpui::px;

    /// Roots of 5, 3 and 1, the first holding children of 2 and 2, beside
    /// shared tops of 4 and 2.
    fn tree() -> Laid {
        let roots = [Spec(5, vec![leaf(2), leaf(2)]), leaf(3), leaf(1)];
        Laid::sharing(&roots, &[leaf(4), leaf(2)])
    }

    fn cells(tree: &Laid) -> Vec<Cell> {
        lay_out(tree, Holds::Build, area(160., 3.), ROW, px(1.))
    }

    fn step(tree: &Laid, from: Holds, step: Step) -> Option<Holds> {
        let cells = cells(tree);
        let from = cells.iter().find(|cell| cell.holds == from).unwrap();
        stepped(&cells, from, step, tree).map(|cell| cell.holds)
    }

    #[test]
    fn left_and_right_step_through_the_siblings_and_stop_at_either_end() {
        let tree = tree();
        assert_eq!(
            step(&tree, Holds::Node(0), Step::Next),
            Some(Holds::Node(1))
        );
        assert_eq!(
            step(&tree, Holds::Node(1), Step::Previous),
            Some(Holds::Node(0))
        );
        assert_eq!(step(&tree, Holds::Node(0), Step::Previous), None);
        assert_eq!(
            step(&tree, Holds::Node(5), Step::Next),
            Some(Holds::Node(6))
        );
        assert_eq!(step(&tree, Holds::Node(6), Step::Next), None);
    }

    /// The cell of the shared tops is a sibling of the roots, and the shared
    /// tops are not, standing under it.
    #[test]
    fn the_cell_of_the_shared_tops_stands_among_the_roots() {
        let tree = tree();
        assert_eq!(step(&tree, Holds::Node(2), Step::Next), Some(Holds::Shared));
        assert_eq!(
            step(&tree, Holds::Shared, Step::Child),
            Some(Holds::Node(3))
        );
        assert_eq!(
            step(&tree, Holds::Node(4), Step::Parent),
            Some(Holds::Shared)
        );
        assert_eq!(step(&tree, Holds::Shared, Step::Parent), Some(Holds::Build));
    }

    #[test]
    fn up_reaches_the_cell_on_top_and_stops_there() {
        let tree = tree();
        assert_eq!(
            step(&tree, Holds::Node(5), Step::Parent),
            Some(Holds::Node(0))
        );
        assert_eq!(
            step(&tree, Holds::Node(0), Step::Parent),
            Some(Holds::Build)
        );
        assert_eq!(step(&tree, Holds::Build, Step::Parent), None);
        assert_eq!(step(&tree, Holds::Build, Step::Previous), None);
    }

    /// Down takes the heaviest cell under, which under the whole build is
    /// the cell of the shared tops where they outweigh every root.
    #[test]
    fn down_takes_the_heaviest_cell_under() {
        let tree = tree();
        assert_eq!(step(&tree, Holds::Build, Step::Child), Some(Holds::Shared));
        assert_eq!(
            step(&tree, Holds::Node(0), Step::Child),
            Some(Holds::Node(5))
        );
        assert_eq!(step(&tree, Holds::Node(1), Step::Child), None);
    }

    /// A rest cell is a sibling like any other cell.
    #[test]
    fn a_rest_cell_is_stepped_to_as_a_sibling() {
        let tree = Laid::of(&[leaf(90), leaf(6), leaf(4)]);
        let cells = lay_out(&tree, Holds::Build, area(100., 2.), ROW, px(8.));
        let first = cells
            .iter()
            .find(|cell| cell.holds == Holds::Node(0))
            .unwrap();
        let rest = stepped(&cells, first, Step::Next, &tree).map(|cell| cell.holds);
        let Some(Holds::Rest(run)) = rest else {
            panic!("{rest:?}");
        };
        assert_eq!(run.parent(), Parent::Build);
    }
}
