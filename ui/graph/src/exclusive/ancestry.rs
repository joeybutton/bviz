//! The rows standing over the cell the icicle is drawn from, each a full
//! row wide: the whole build, then where the focus stands deep, a row for
//! the ancestors left out, then its nearest ancestors.

use super::icicle::{Cell, Holds, Tree};
use gpui::{Bounds, Pixels, point, size};

/// How many of the focus's nearest ancestors below the whole build are drawn
/// a row each; the rest are gathered into one row.
pub(crate) const NEAREST: usize = 2;

/// What a row over the focus stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Row {
    /// An ancestor of the focus, the whole build among them.
    Ancestor(Holds),
    /// The `count` ancestors between the whole build and those drawn.
    Hidden { count: u32 },
}

/// The ancestors of the cell holding `top` in `tree`, the whole build first
/// and its parent last; none for the whole build.
pub(crate) fn ancestors(tree: &impl Tree, top: Holds) -> Vec<Holds> {
    let mut ancestors: Vec<Holds> =
        std::iter::successors(top.parent(tree), |holds| holds.parent(tree)).collect();
    ancestors.reverse();
    ancestors
}

/// The ancestors of a cell parted as the rows over it draw them.
struct Parted {
    /// The whole build, over every other cell.
    build: Holds,
    /// Those between the whole build and the nearest, highest first.
    hidden: Vec<Holds>,
    /// At most [`NEAREST`], highest first.
    nearest: Vec<Holds>,
}

/// The ancestors of the cell holding `top` in `tree`, parted; none for the
/// whole build.
fn parted(tree: &impl Tree, top: Holds) -> Option<Parted> {
    let ancestors = ancestors(tree, top);
    let (&build, below) = ancestors.split_first()?;
    let (hidden, nearest) = below.split_at(below.len().saturating_sub(NEAREST));
    Some(Parted {
        build,
        hidden: hidden.to_vec(),
        nearest: nearest.to_vec(),
    })
}

/// The rows standing over the cell holding `top` in `tree`, top first: none
/// over the whole build; else the whole build, then the ancestors left out
/// where there are more than [`NEAREST`] below it, then the nearest.
pub(crate) fn rows(tree: &impl Tree, top: Holds) -> Vec<Row> {
    let Some(Parted {
        build,
        hidden,
        nearest,
    }) = parted(tree, top)
    else {
        return Vec::new();
    };
    let gathered = (!hidden.is_empty()).then_some(Row::Hidden {
        count: hidden.len() as u32,
    });
    let mut rows = Vec::with_capacity(1 + usize::from(gathered.is_some()) + nearest.len());
    rows.push(Row::Ancestor(build));
    rows.extend(gathered);
    rows.extend(nearest.into_iter().map(Row::Ancestor));
    rows
}

/// The ancestors of the cell holding `top` that [`rows`] gathers into one
/// row, the highest first.
pub(crate) fn hidden(tree: &impl Tree, top: Holds) -> Vec<Holds> {
    parted(tree, top).map_or_else(Vec::new, |parted| parted.hidden)
}

/// Something drawn over the icicle that the pointer or the keys can pick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Spot {
    /// A cell: of the icicle, or of an ancestor over it.
    Cell(Cell),
    /// The row gathering `count` ancestors.
    Hidden { rect: Bounds<Pixels>, count: u32 },
}

impl Spot {
    pub(crate) fn rect(&self) -> Bounds<Pixels> {
        match self {
            Spot::Cell(cell) => cell.rect,
            Spot::Hidden { rect, .. } => *rect,
        }
    }

    /// What this stands for, wherever it is laid.
    pub(crate) fn stands(&self) -> Stands {
        match self {
            Spot::Cell(cell) => Stands::Cell(cell.holds),
            Spot::Hidden { .. } => Stands::Hidden,
        }
    }

    /// Whether this and `other` stand for the same, wherever each is laid.
    pub(crate) fn same(&self, other: &Spot) -> bool {
        self.stands() == other.stands()
    }
}

/// What a [`Spot`] stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stands {
    Cell(Holds),
    /// The row gathering the ancestors left out.
    Hidden,
}

/// `rows` laid out a row `row` tall each from the top of `bounds` across its
/// width, and the bounds left under them.
pub(crate) fn lay_out(
    rows: &[Row],
    bounds: Bounds<Pixels>,
    row: Pixels,
) -> (Vec<Spot>, Bounds<Pixels>) {
    let rect = |at: usize| {
        let top = bounds.origin.y + row * at as f32;
        Bounds::new(point(bounds.origin.x, top), size(bounds.size.width, row))
    };
    let spots = rows
        .iter()
        .enumerate()
        .map(|(at, &standing)| match standing {
            Row::Ancestor(holds) => Spot::Cell(Cell {
                rect: rect(at),
                holds,
            }),
            Row::Hidden { count } => Spot::Hidden {
                rect: rect(at),
                count,
            },
        });
    let spots: Vec<Spot> = spots.collect();
    (spots, below(bounds, row * rows.len() as f32))
}

/// What of `bounds` lies more than `depth` below its top.
pub(crate) fn below(bounds: Bounds<Pixels>, depth: Pixels) -> Bounds<Pixels> {
    let taken = depth.min(bounds.size.height);
    Bounds::new(
        point(bounds.origin.x, bounds.origin.y + taken),
        size(bounds.size.width, bounds.size.height - taken),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exclusive::icicle::Parent;
    use crate::exclusive::icicle::tests::{Laid, ROW, Spec, area, leaf};
    use gpui::px;

    /// A chain five deep under one root, beside a second root, and a shared
    /// top holding a child.
    fn tree() -> Laid {
        let chain = Spec(
            5,
            vec![Spec(4, vec![Spec(3, vec![Spec(2, vec![leaf(1)])])])],
        );
        Laid::sharing(&[chain, leaf(1)], &[Spec(2, vec![leaf(1)])])
    }

    /// The node at `depth` along the chain under the first root, the root at
    /// depth 0.
    fn along(tree: &Laid, depth: usize) -> Holds {
        let mut node = 0;
        for _ in 0..depth {
            node = tree.children(node).start;
        }
        Holds::Node(node)
    }

    #[test]
    fn the_whole_build_has_nothing_over_it() {
        assert_eq!(rows(&tree(), Holds::Build), []);
    }

    #[test]
    fn a_shallow_focus_has_every_ancestor_drawn() {
        let tree = tree();
        assert_eq!(rows(&tree, along(&tree, 0)), [Row::Ancestor(Holds::Build)]);
        let shared_child = Holds::Node(tree.children(2).start);
        assert_eq!(
            rows(&tree, shared_child),
            [
                Row::Ancestor(Holds::Build),
                Row::Ancestor(Holds::Shared),
                Row::Ancestor(Holds::Node(2)),
            ]
        );
    }

    /// Past the [`NEAREST`] nearest ancestors, the rest between them and the
    /// whole build are gathered into one row counting them.
    #[test]
    fn a_deep_focus_gathers_its_higher_ancestors_into_one_row() {
        let tree = tree();
        let focus = along(&tree, 4);
        assert_eq!(
            rows(&tree, focus),
            [
                Row::Ancestor(Holds::Build),
                Row::Hidden { count: 2 },
                Row::Ancestor(along(&tree, 2)),
                Row::Ancestor(along(&tree, 3)),
            ]
        );
        assert_eq!(hidden(&tree, focus), [along(&tree, 0), along(&tree, 1)]);
        assert_eq!(hidden(&tree, along(&tree, 2)), []);
    }

    /// A run of siblings stands under the cell over them, which is named a
    /// row of its own.
    #[test]
    fn a_run_stands_under_its_parent() {
        let tree = tree();
        let cells = super::super::icicle::lay_out(&tree, Holds::Build, area(10., 2.), ROW, px(4.));
        let run = cells
            .iter()
            .find_map(|cell| match cell.holds {
                Holds::Rest(run) if run.parent() == Parent::Build => Some(cell.holds),
                _ => None,
            })
            .unwrap();
        assert_eq!(rows(&tree, run), [Row::Ancestor(Holds::Build)]);
    }

    #[test]
    fn the_rows_lie_a_row_apart_across_the_width_and_leave_the_rest_under_them() {
        let tree = tree();
        let rows = rows(&tree, along(&tree, 4));
        let (spots, under) = lay_out(&rows, area(100., 10.), ROW);
        let tops: Vec<Pixels> = spots.iter().map(|spot| spot.rect().top()).collect();
        assert_eq!(tops, [px(0.), ROW, ROW * 2., ROW * 3.]);
        assert!(spots.iter().all(|spot| spot.rect().size.width == px(100.)));
        assert_eq!(
            under,
            Bounds::new(point(px(0.), ROW * 4.), size(px(100.), ROW * 6.))
        );
    }
}
