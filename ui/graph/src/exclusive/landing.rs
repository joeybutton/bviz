//! Where a node of the graph stands in a dominator tree the icicle draws,
//! which a jump to it lands on.

use crate::chips::Unmarked;
use crate::hidden::HiddenBy;
use crate::jump::Stands;
use crate::stand_in::{self, StandIns, Writers};
use bviz_graph_layout::dominators::{Above, Dominators, ShownTree};
use bviz_graph_layout::graph::Graph;
use bviz_graph_layout::select::Verdicts;
use bviz_graph_layout::{Facets, Id};

/// What a jump to a node lands on in a tree drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Landing {
    /// The cell of the tree's node `node`, drawn under `above`.
    Cell { node: u32, above: Above },
    /// The tree's point `point` stands for the node, and the filter hides it
    /// by what this names.
    Hidden(Id, HiddenBy),
    /// The tree draws nothing for the node, for the reason given.
    Absent(&'static str),
}

impl Landing {
    /// Why a tree landing a jump here rings no cell.
    pub(super) fn unmarked(self) -> Option<Unmarked> {
        match self {
            Landing::Cell { .. } => None,
            Landing::Hidden(..) => Some(Unmarked::Hidden),
            Landing::Absent(_) => Some(Unmarked::Absent),
        }
    }
}

/// Why a tree draws nothing for a point of its unit: no root chosen reaches
/// it.
const OUTSIDE_ROOTS: &str = "outside the roots drawn: no root reaches it";

/// Why a tree draws nothing for a node standing for no point of its unit.
const NOT_A_POINT: &str = "not a point of this tree";

/// Why a tree draws nothing for a node holding points of its unit.
const HOLDS_NONE_DRAWN: &str = "holds no point the roots drawn reach";

/// The graph a tree was drawn over, which a node is found in it through.
pub(super) struct Over<'a> {
    pub(super) graph: &'a Graph,
    pub(super) facets: &'a Facets,
    pub(super) writers: &'a Writers,
}

/// What a jump to `id` lands on in `tree` drawn as `shown` through
/// `verdicts`, none for a tree showing every point.
pub(super) fn landing(
    tree: &Dominators,
    shown: &ShownTree,
    verdicts: Option<&Verdicts>,
    over: &Over,
    id: Id,
) -> Landing {
    match stand_ins(tree, over, id) {
        StandIns::Points(points) => match points.iter().find_map(|&point| tree.index_of(point)) {
            Some(node) => shown_as(tree, shown, verdicts, node),
            None if points.is_empty() => Landing::Absent(NOT_A_POINT),
            None => Landing::Absent(OUTSIDE_ROOTS),
        },
        StandIns::Holding => {
            let held = |&node: &u32| stand_in::holds(over.graph, id, tree.id(node));
            match (0..tree.total()).find(held) {
                Some(node) => shown_as(tree, shown, verdicts, node),
                None => Landing::Absent(HOLDS_NONE_DRAWN),
            }
        }
    }
}

/// The points of `tree`'s unit that may stand for `id`.
fn stand_ins(tree: &Dominators, over: &Over, id: Id) -> StandIns {
    let unit = tree.scope().unit();
    let is_point = |id| tree.index_of(id).is_some();
    stand_in::stand_ins(unit, over.graph, over.facets, over.writers, is_point, id)
}

/// What a jump onto the tree's node `node` lands on.
fn shown_as(
    tree: &Dominators,
    shown: &ShownTree,
    verdicts: Option<&Verdicts>,
    node: u32,
) -> Landing {
    if let Some(placing) = shown.placing(node) {
        return Landing::Cell {
            node,
            above: placing.above,
        };
    }
    let point = tree.id(node);
    match verdicts.and_then(|verdicts| verdicts.hiding(point)) {
        Some(hiding) => Landing::Hidden(point, HiddenBy::of(hiding)),
        None => Landing::Absent(OUTSIDE_ROOTS),
    }
}

/// Where `id` stands in `tree` drawn as `shown`, as a palette row badges it,
/// read without a pass over the tree: a node holding points stands among
/// them.
pub(super) fn stands(
    tree: &Dominators,
    shown: &ShownTree,
    verdicts: Option<&Verdicts>,
    over: &Over,
    id: Id,
) -> Stands {
    let StandIns::Points(points) = stand_ins(tree, over, id) else {
        return Stands::Here;
    };
    let node = points.iter().find_map(|&point| tree.index_of(point));
    match node.map(|node| shown_as(tree, shown, verdicts, node)) {
        Some(Landing::Cell { .. }) => Stands::Here,
        Some(Landing::Hidden(_, by)) => Stands::Hidden(by),
        Some(Landing::Absent(_)) | None => Stands::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_selection_landing_on_no_cell_is_unmarked_as_it_lands() {
        let cell = Landing::Cell {
            node: 0,
            above: Above::Roots,
        };
        assert_eq!(cell.unmarked(), None);
        let hidden = Landing::Hidden(Id(1), HiddenBy::Pin);
        assert_eq!(hidden.unmarked(), Some(Unmarked::Hidden));
        let absent = Landing::Absent(OUTSIDE_ROOTS);
        assert_eq!(absent.unmarked(), Some(Unmarked::Absent));
    }
}
