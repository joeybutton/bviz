//! Where each cell of the icicle drawing a dominator tree stands.

use bviz_graph_layout::dominators::Dominators;
use gpui::{Bounds, Pixels, point, px, size};
use std::ops::Range;

/// A tree whose nodes are weighed by how many points each stands over, itself
/// among them, and whose siblings come heaviest first. Its tops come in two
/// runs, each heaviest first: the roots, then the shared tops.
pub(crate) trait Tree {
    /// How many points the tree holds, which its tops' weights sum to.
    fn total(&self) -> u32;
    fn roots(&self) -> Range<u32>;
    fn shared_tops(&self) -> Range<u32>;
    /// What the weights of the shared tops sum to.
    fn shared_weight(&self) -> u32;
    fn children(&self, node: u32) -> Range<u32>;
    fn weight(&self, node: u32) -> u32;
    fn parent(&self, node: u32) -> Parent;
}

impl Tree for Dominators {
    fn total(&self) -> u32 {
        Dominators::total(self)
    }

    fn roots(&self) -> Range<u32> {
        Dominators::roots(self)
    }

    fn shared_tops(&self) -> Range<u32> {
        Dominators::shared_tops(self)
    }

    fn shared_weight(&self) -> u32 {
        Dominators::shared_weight(self)
    }

    fn children(&self, node: u32) -> Range<u32> {
        Dominators::children(self, node)
    }

    fn weight(&self, node: u32) -> u32 {
        Dominators::weight(self, node)
    }

    fn parent(&self, node: u32) -> Parent {
        match Dominators::parent(self, node) {
            Some(parent) => Parent::Node(parent),
            None if Dominators::shared_tops(self).contains(&node) => Parent::Shared,
            None => Parent::Build,
        }
    }
}

/// What a cell of the icicle stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Holds {
    /// The whole build, over every top of the tree.
    Build,
    /// Every shared top of the tree.
    Shared,
    Node(u32),
    /// A run of siblings too narrow to draw apart.
    Rest(Run),
}

impl Holds {
    /// The node this is, where it is one.
    pub(crate) fn node(self) -> Option<u32> {
        match self {
            Holds::Node(node) => Some(node),
            Holds::Build | Holds::Shared | Holds::Rest(_) => None,
        }
    }

    /// The points of `tree` the cell holding this stands over.
    pub(crate) fn weight(self, tree: &impl Tree) -> u32 {
        match self {
            Holds::Build => tree.total(),
            Holds::Shared => tree.shared_weight(),
            Holds::Node(node) => tree.weight(node),
            Holds::Rest(run) => run.weight,
        }
    }

    /// What the cell holding this stands under in `tree`; none for the whole
    /// build.
    pub(crate) fn parent(self, tree: &impl Tree) -> Option<Holds> {
        match self {
            Holds::Build => None,
            Holds::Shared => Some(Holds::Build),
            Holds::Node(node) => Some(tree.parent(node).into()),
            Holds::Rest(run) => Some(run.parent.into()),
        }
    }

    /// Whether the cell holding this stands directly under the one holding
    /// `over` in `tree`, or is gathered by it where that is a rest cell.
    pub(crate) fn stands_under(self, over: Holds, tree: &impl Tree) -> bool {
        match (over, self) {
            (Holds::Rest(run), Holds::Node(node)) => {
                tree.parent(node) == run.parent && run.siblings().contains(&node)
            }
            (Holds::Rest(run), Holds::Rest(within)) => {
                within.parent == run.parent
                    && run.siblings().start <= within.first
                    && within.siblings().end <= run.siblings().end
            }
            (Holds::Rest(_), Holds::Build | Holds::Shared) => false,
            (over, holds) => holds.parent(tree) == Some(over),
        }
    }
}

/// What a row of siblings stands under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Parent {
    /// Over the roots.
    Build,
    /// Over the shared tops.
    Shared,
    /// Over its children.
    Node(u32),
}

impl From<Parent> for Holds {
    fn from(parent: Parent) -> Self {
        match parent {
            Parent::Build => Holds::Build,
            Parent::Shared => Holds::Shared,
            Parent::Node(node) => Holds::Node(node),
        }
    }
}

/// The siblings from `first` to the last under `parent`, which a row of the
/// icicle had too little room to draw apart. Only [`lay_out`] makes one, so
/// `count` and `weight` are always how many siblings the run holds and the
/// points they weigh together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    parent: Parent,
    first: u32,
    count: u32,
    weight: u32,
}

impl Run {
    pub(crate) fn parent(&self) -> Parent {
        self.parent
    }

    pub(crate) fn count(&self) -> u32 {
        self.count
    }

    pub(crate) fn siblings(&self) -> Range<u32> {
        self.first..self.first + self.count
    }

    pub(crate) fn weight(&self) -> u32 {
        self.weight
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cell {
    pub(crate) rect: Bounds<Pixels>,
    pub(crate) holds: Holds,
}

/// How far a cell's edge may stray from its parent's by the sums laying
/// siblings side by side and still lie within it.
const EDGE_SLACK: Pixels = px(0.01);

/// Whether `inner` lies across no more than `outer` does, as a cell [`lay_out`]
/// lays out does under each cell above it.
pub(crate) fn spans(outer: Bounds<Pixels>, inner: Bounds<Pixels>) -> bool {
    inner.left() >= outer.left() - EDGE_SLACK && inner.right() <= outer.right() + EDGE_SLACK
}

/// Whether `below` lies in the row under `above`, which [`lay_out`] lays out
/// a row's height apart.
pub(crate) fn row_under(above: Bounds<Pixels>, below: Bounds<Pixels>) -> bool {
    let row = above.size.height;
    (below.top() - above.bottom()).abs() < row / 2.
}

/// The narrowest a cell is drawn at all.
pub(crate) const VISIBLE_WIDTH: Pixels = px(1.);

/// How much of the row beneath a run on top its own rest cell takes, where
/// the run's siblings are too many to draw apart at once.
const RUN_REST_SHARE: f32 = 0.2;

/// The cells of the icicle drawing `tree` from the cell holding `top`, in
/// `bounds`, a row `row` tall each.
///
/// The first row is `top` at full width, and each row beneath holds the
/// children of each cell above it, laid from its left edge at widths in
/// proportion to their weights, so what a cell leaves of its own width beyond
/// its children's is its own point. Under the whole build those are the
/// roots, and the width they leave is one cell holding the shared tops, which
/// are laid out beneath it; [`split`] divides the row between the two. Only
/// the rows `bounds` holds whole are laid out. A child narrower than
/// `min_width` ends its row of siblings: it and every lighter sibling share
/// one rest cell spanning the width left, laid out where that is visible at
/// all, and nothing below it. The cells laid out are thus bounded by how many
/// fit, whatever the size of the tree. The rest cell of the roots is laid out
/// all the same, `min_width` wide, with those drawn apart giving way to it,
/// so that no root is out of reach.
///
/// A run on top holds its siblings in the same way, scaled to their summed
/// weight. Where even the heaviest of them is too narrow to draw at that
/// scale, the heaviest are drawn by their own summed weight across all but
/// [`RUN_REST_SHARE`] of the row, as many as stay `min_width` wide, and the
/// rest cell takes that share, so each run opened draws some of its siblings
/// apart.
pub(crate) fn lay_out(
    tree: &impl Tree,
    top: Holds,
    bounds: Bounds<Pixels>,
    row: Pixels,
    min_width: Pixels,
) -> Vec<Cell> {
    let rows = (bounds.size.height / row).floor() as usize;
    if rows == 0 || tree.total() == 0 {
        return Vec::new();
    }
    let per_row = (bounds.size.width / min_width).floor() as usize;
    let mut cells: Vec<Cell> = Vec::with_capacity(per_row + 1);
    cells.push(Cell {
        rect: Bounds::new(bounds.origin, size(bounds.size.width, row)),
        holds: top,
    });
    let mut above = 0..cells.len();
    for depth in 1..rows {
        let y = bounds.origin.y + row * depth as f32;
        let start = cells.len();
        cells.reserve(per_row + above.len() + 1);
        for parent in above {
            let Cell { rect, holds } = cells[parent];
            let width = rect.size.width;
            // `own` is what the cell weighs beyond the siblings laid under it.
            let (parent, laid, weight, own) = match holds {
                Holds::Build => {
                    let (laid, shared) = (tree.roots(), tree.shared_weight());
                    (Parent::Build, laid, tree.total(), shared)
                }
                Holds::Shared => {
                    let (laid, weight) = (tree.shared_tops(), tree.shared_weight());
                    (Parent::Shared, laid, weight, 0)
                }
                Holds::Node(node) => (
                    Parent::Node(node),
                    tree.children(node),
                    tree.weight(node),
                    1,
                ),
                Holds::Rest(run) if depth == 1 => (run.parent, run.siblings(), run.weight, 0),
                Holds::Rest(_) => continue,
            };
            // The siblings share `span` by weight, weighing `span_weight`
            // between them, and the cell keeps `beyond` for what it weighs
            // itself.
            let (span, span_weight, beyond) = match holds {
                Holds::Build => {
                    let (span, shared) = split(width, own, weight, min_width);
                    (span, weight - own, shared)
                }
                _ => (width, weight, width / weight as f32 * own as f32),
            };
            // The children drawn apart are those of `laid` wide enough; a
            // rest cell gathers the rest up to `end`.
            let end = laid.end;
            let share = |sibling: u32, span: Pixels, span_weight: u32| {
                span * (tree.weight(sibling) as f32 / span_weight as f32)
            };
            let (page, span, span_weight) = match holds {
                Holds::Rest(run) if share(run.first, span, span_weight) < min_width => {
                    let room = width * (1. - RUN_REST_SHARE);
                    heaviest_page(tree, laid.clone(), room, min_width)
                }
                _ => (laid.clone(), span, span_weight),
            };
            let draw_apart = |cells: &mut Vec<Cell>, page: Range<u32>, span, span_weight| {
                let (mut x, mut drawn) = (rect.origin.x, 0);
                let mut gathered = page.end;
                for child in page {
                    let width = share(child, span, span_weight);
                    if width < min_width {
                        gathered = child;
                        break;
                    }
                    cells.push(Cell {
                        rect: Bounds::new(point(x, y), size(width, row)),
                        holds: Holds::Node(child),
                    });
                    x += width;
                    drawn += tree.weight(child);
                }
                (x, drawn, gathered)
            };
            let first_apart = cells.len();
            let (mut x, mut drawn, mut gathered) = draw_apart(&mut cells, page, span, span_weight);
            let squeezed = gathered < end && rect.right() - beyond - x < min_width;
            if parent == Parent::Build && squeezed {
                cells.truncate(first_apart);
                let (page, room, summed) = heaviest_page(tree, laid, span - min_width, min_width);
                (x, drawn, gathered) = draw_apart(&mut cells, page, room, summed);
            }
            let rest = rect.right() - beyond - x;
            if gathered < end && rest >= VISIBLE_WIDTH {
                cells.push(Cell {
                    rect: Bounds::new(point(x, y), size(rest, row)),
                    holds: Holds::Rest(Run {
                        parent,
                        first: gathered,
                        count: end - gathered,
                        weight: weight - own - drawn,
                    }),
                });
            }
            if holds == Holds::Build && beyond >= VISIBLE_WIDTH {
                cells.push(Cell {
                    rect: Bounds::new(point(rect.right() - beyond, y), size(beyond, row)),
                    holds: Holds::Shared,
                });
            }
        }
        above = start..cells.len();
        if above.is_empty() {
            break;
        }
    }
    cells
}

/// The left edge and width [`lay_out`] gives the cell holding `child` by its
/// weight among its siblings under `parent`, or among those `parent` gathers
/// where it is a rest cell, whether or not it is wide enough to be drawn
/// apart.
pub(crate) fn share_of(
    tree: &impl Tree,
    parent: Cell,
    child: Holds,
    min_width: Pixels,
) -> (Pixels, Pixels) {
    let Cell { rect, holds } = parent;
    let (siblings, weight, span) = match holds {
        Holds::Build => {
            let shared = tree.shared_weight();
            let (roots, beyond) = split(rect.size.width, shared, tree.total(), min_width);
            if child == Holds::Shared {
                return (rect.right() - beyond, beyond);
            }
            (tree.roots(), tree.total() - shared, roots)
        }
        Holds::Shared => (tree.shared_tops(), tree.shared_weight(), rect.size.width),
        Holds::Node(node) => (tree.children(node), tree.weight(node), rect.size.width),
        Holds::Rest(run) => (run.siblings(), run.weight, rect.size.width),
    };
    let first = match child {
        Holds::Node(node) => node,
        Holds::Rest(run) => run.first,
        Holds::Build | Holds::Shared => siblings.start,
    };
    let before: u32 = (siblings.start..first)
        .map(|sibling| tree.weight(sibling))
        .sum();
    let per_point = span / weight as f32;
    (
        rect.left() + per_point * before as f32,
        per_point * child.weight(tree) as f32,
    )
}

/// How a row `width` wide under the whole build, which weighs `weight`, is
/// divided between its roots and the cell of the shared tops,
/// which weigh `shared`, as the width of each: by weight, but with neither
/// narrower than `min_width`, or than half a row too narrow for two such, so
/// that neither is left without a cell where the other outweighs it. Either
/// weighing nothing takes no width.
fn split(width: Pixels, shared: u32, weight: u32, min_width: Pixels) -> (Pixels, Pixels) {
    if shared == 0 {
        return (width, px(0.));
    }
    if shared == weight {
        return (px(0.), width);
    }
    let least = min_width.min(width / 2.);
    let by_weight = width / weight as f32 * shared as f32;
    if by_weight < least {
        (width - least, least)
    } else if width - by_weight < least {
        (least, width - least)
    } else {
        (width - by_weight, by_weight)
    }
}

/// The heaviest of `siblings` that each stay `min_width` wide when they
/// share `room` by their summed weight, with `room` and that weight. Only as
/// many siblings are weighed as are drawn, and one more.
fn heaviest_page(
    tree: &impl Tree,
    siblings: Range<u32>,
    room: Pixels,
    min_width: Pixels,
) -> (Range<u32>, Pixels, u32) {
    let first = siblings.start;
    let (mut end, mut summed) = (first, 0);
    for sibling in siblings {
        let weight = tree.weight(sibling);
        let widened = summed + weight;
        if room * (weight as f32 / widened as f32) < min_width {
            break;
        }
        (end, summed) = (sibling + 1, widened);
    }
    (first..end, room, summed)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// A tree laid out as [`Dominators`] lays one out: each node's children
    /// consecutive, heaviest first, and the tops ahead of every other node,
    /// the roots first.
    pub(in crate::exclusive) struct Laid {
        roots: u32,
        weights: Vec<u32>,
        /// By node, where its children start, and past the last node the node
        /// count.
        bounds: Vec<u32>,
    }

    /// A node by its weight and its children.
    #[derive(Clone)]
    pub(in crate::exclusive) struct Spec(
        pub(in crate::exclusive) u32,
        pub(in crate::exclusive) Vec<Spec>,
    );

    impl Laid {
        /// A tree whose tops are all roots.
        pub(in crate::exclusive) fn of(roots: &[Spec]) -> Self {
            Self::sharing(roots, &[])
        }

        pub(in crate::exclusive) fn sharing(roots: &[Spec], shared: &[Spec]) -> Self {
            let mut order: Vec<&Spec> = roots.iter().chain(shared).collect();
            let mut bounds = Vec::new();
            let mut at = 0;
            while let Some(&spec) = order.get(at) {
                bounds.push(order.len() as u32);
                order.extend(spec.1.iter());
                at += 1;
            }
            bounds.push(order.len() as u32);
            Self {
                roots: roots.len() as u32,
                weights: order.iter().map(|spec| spec.0).collect(),
                bounds,
            }
        }
    }

    impl Tree for Laid {
        fn total(&self) -> u32 {
            (0..self.bounds[0]).map(|top| self.weight(top)).sum()
        }

        fn roots(&self) -> Range<u32> {
            0..self.roots
        }

        fn shared_tops(&self) -> Range<u32> {
            self.roots..self.bounds[0]
        }

        fn shared_weight(&self) -> u32 {
            self.shared_tops().map(|top| self.weight(top)).sum()
        }

        fn children(&self, node: u32) -> Range<u32> {
            self.bounds[node as usize]..self.bounds[node as usize + 1]
        }

        fn weight(&self, node: u32) -> u32 {
            self.weights[node as usize]
        }

        fn parent(&self, node: u32) -> Parent {
            let holding =
                (0..self.weights.len() as u32).find(|&p| self.children(p).contains(&node));
            match holding {
                Some(parent) => Parent::Node(parent),
                None if self.shared_tops().contains(&node) => Parent::Shared,
                None => Parent::Build,
            }
        }
    }

    pub(in crate::exclusive) fn leaf(weight: u32) -> Spec {
        Spec(weight, Vec::new())
    }

    pub(in crate::exclusive) const ROW: Pixels = px(10.);

    pub(in crate::exclusive) fn area(width: f32, rows: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(0.), px(0.)), size(px(width), ROW * rows))
    }

    /// Each cell as what it holds, its left edge and width.
    fn drawn(cells: &[Cell]) -> Vec<(Holds, f32, f32)> {
        let edges = |rect: Bounds<Pixels>| (f32::from(rect.left()), f32::from(rect.size.width));
        cells
            .iter()
            .map(|cell| {
                let (left, width) = edges(cell.rect);
                (cell.holds, left, width)
            })
            .collect()
    }

    #[test]
    fn the_whole_build_spans_the_top_row_and_its_tops_share_the_next_by_weight() {
        // Tops 0 and 1 weigh 6 and 2; 0 holds 2 and 3, of 3 and 2.
        let tree = Laid::of(&[Spec(6, vec![leaf(3), leaf(2)]), leaf(2)]);
        let cells = lay_out(&tree, Holds::Build, area(80., 3.), ROW, px(1.));
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 80.),
                (Holds::Node(0), 0., 60.),
                (Holds::Node(1), 60., 20.),
                (Holds::Node(2), 0., 30.),
                (Holds::Node(3), 30., 20.),
            ]
        );
        assert_eq!(cells[3].rect.top(), ROW * 2.);
    }

    #[test]
    fn a_focus_spans_the_top_row_and_leaves_its_own_point_to_the_right() {
        let tree = Laid::of(&[Spec(4, vec![leaf(3)]), leaf(1)]);
        let cells = lay_out(&tree, Holds::Node(0), area(40., 2.), ROW, px(1.));
        assert_eq!(
            drawn(&cells),
            [(Holds::Node(0), 0., 40.), (Holds::Node(2), 0., 30.)]
        );
    }

    #[test]
    fn rows_past_the_bottom_are_not_laid_out() {
        let chain = Spec(3, vec![Spec(2, vec![leaf(1)])]);
        let tree = Laid::of(&[chain]);
        let cells = lay_out(&tree, Holds::Build, area(30., 2.5), ROW, px(1.));
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn siblings_too_narrow_to_draw_share_one_rest_cell() {
        // Under a top of 10: children of 5, 2, 1 and 1, the top itself the last.
        let top = Spec(10, vec![leaf(5), Spec(2, vec![leaf(1)]), leaf(1), leaf(1)]);
        let tree = Laid::of(&[top]);
        let cells = lay_out(&tree, Holds::Build, area(100., 3.), ROW, px(25.));
        let rest = Holds::Rest(Run {
            parent: Parent::Node(0),
            first: 2,
            count: 3,
            weight: 4,
        });
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 100.),
                (Holds::Node(0), 0., 100.),
                (Holds::Node(1), 0., 50.),
                (rest, 50., 40.),
            ]
        );
    }

    #[test]
    fn a_run_on_top_spreads_its_siblings_by_their_summed_weight() {
        // Under a top of 10: children of 5, 2, 1 and 1, the one of 2 holding
        // a leaf.
        let top = Spec(10, vec![leaf(5), Spec(2, vec![leaf(1)]), leaf(1), leaf(1)]);
        let tree = Laid::of(&[top]);
        let cells = lay_out(&tree, Holds::Build, area(100., 3.), ROW, px(25.));
        let Some(&Cell { holds: run, .. }) = cells.last() else {
            panic!("no cells laid out");
        };
        let cells = lay_out(&tree, run, area(100., 3.), ROW, px(25.));
        assert_eq!(
            drawn(&cells),
            [
                (run, 0., 100.),
                (Holds::Node(2), 0., 50.),
                (Holds::Node(3), 50., 25.),
                (Holds::Node(4), 75., 25.),
                (Holds::Node(5), 0., 25.),
            ]
        );
    }

    #[test]
    fn a_run_on_top_gathers_its_own_rest_and_lays_out_nothing_under_it() {
        let top = Spec(10, vec![leaf(5), Spec(2, vec![leaf(1)]), leaf(1), leaf(1)]);
        let tree = Laid::of(&[top]);
        let run = Holds::Rest(Run {
            parent: Parent::Node(0),
            first: 2,
            count: 3,
            weight: 4,
        });
        let cells = lay_out(&tree, run, area(100., 3.), ROW, px(30.));
        let rest = |parent, first, count, weight| {
            Holds::Rest(Run {
                parent: Parent::Node(parent),
                first,
                count,
                weight,
            })
        };
        assert_eq!(
            drawn(&cells),
            [
                (run, 0., 100.),
                (Holds::Node(2), 0., 50.),
                (rest(0, 3, 2, 2), 50., 50.),
                (rest(2, 5, 1, 1), 0., 25.),
            ]
        );
    }

    /// A run whose heaviest sibling is too narrow at the run's own scale
    /// still draws some of its siblings apart.
    #[test]
    fn a_run_too_many_to_draw_apart_spreads_its_heaviest_and_gathers_the_rest() {
        let tree = Laid::of(&vec![leaf(1); 10]);
        let cells = lay_out(&tree, Holds::Build, area(100., 2.), ROW, px(20.));
        let Some(&Cell { holds: run, .. }) = cells.last() else {
            panic!("no cells laid out");
        };
        let rest = Holds::Rest(Run {
            parent: Parent::Build,
            first: 0,
            count: 10,
            weight: 10,
        });
        assert_eq!(run, rest);
        let cells = lay_out(&tree, run, area(100., 2.), ROW, px(20.));
        let rest = Holds::Rest(Run {
            parent: Parent::Build,
            first: 4,
            count: 6,
            weight: 6,
        });
        assert_eq!(
            drawn(&cells),
            [
                (run, 0., 100.),
                (Holds::Node(0), 0., 20.),
                (Holds::Node(1), 20., 20.),
                (Holds::Node(2), 40., 20.),
                (Holds::Node(3), 60., 20.),
                (rest, 80., 20.),
            ]
        );
    }

    /// The roots take the row under the whole build, and one cell after them
    /// the shared tops, which lie beneath it.
    #[test]
    fn the_shared_tops_are_laid_out_under_one_cell_after_the_roots() {
        // Roots 0 and 1 weigh 3 and 1; shared 2 and 3 weigh 4 and 2.
        let roots = [Spec(3, vec![leaf(2)]), leaf(1)];
        let tree = Laid::sharing(&roots, &[Spec(4, vec![leaf(3)]), leaf(2)]);
        let cells = lay_out(&tree, Holds::Build, area(100., 4.), ROW, px(1.));
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 100.),
                (Holds::Node(0), 0., 30.),
                (Holds::Node(1), 30., 10.),
                (Holds::Shared, 40., 60.),
                (Holds::Node(4), 0., 20.),
                (Holds::Node(2), 40., 40.),
                (Holds::Node(3), 80., 20.),
                (Holds::Node(5), 40., 30.),
            ]
        );
    }

    #[test]
    fn the_shared_tops_on_top_spread_by_their_summed_weight() {
        let tree = Laid::sharing(&[leaf(4)], &[Spec(4, vec![leaf(3)]), leaf(2)]);
        let cells = lay_out(&tree, Holds::Shared, area(60., 3.), ROW, px(1.));
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Shared, 0., 60.),
                (Holds::Node(1), 0., 40.),
                (Holds::Node(2), 40., 20.),
                (Holds::Node(3), 0., 30.),
            ]
        );
    }

    #[test]
    fn a_tree_sharing_no_top_has_no_cell_for_them() {
        let tree = Laid::of(&[leaf(3), leaf(1)]);
        let cells = lay_out(&tree, Holds::Build, area(40., 2.), ROW, px(1.));
        let holds: Vec<Holds> = cells.iter().map(|cell| cell.holds).collect();
        assert_eq!(holds, [Holds::Build, Holds::Node(0), Holds::Node(1)]);
    }

    /// The roots too narrow to draw apart share a rest cell short of the cell
    /// of the shared tops, and so do the shared tops beneath it.
    #[test]
    fn the_roots_and_the_shared_tops_each_gather_their_own_rest() {
        let roots = [leaf(4), leaf(1), leaf(1)];
        let tree = Laid::sharing(&roots, &[leaf(2), leaf(1), leaf(1)]);
        let cells = lay_out(&tree, Holds::Build, area(100., 3.), ROW, px(15.));
        let rest = |parent, first, count, weight| {
            Holds::Rest(Run {
                parent,
                first,
                count,
                weight,
            })
        };
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 100.),
                (Holds::Node(0), 0., 40.),
                (rest(Parent::Build, 1, 2, 2), 40., 20.),
                (Holds::Shared, 60., 40.),
                (Holds::Node(3), 60., 20.),
                (rest(Parent::Shared, 4, 2, 2), 80., 20.),
            ]
        );
    }

    /// However little the roots weigh beside the shared tops, they keep a
    /// cell under the whole build.
    #[test]
    fn points_the_shared_tops_outweigh_keep_a_cell() {
        let shared = [Spec(998, vec![leaf(997)])];
        let tree = Laid::sharing(&[leaf(1), leaf(1)], &shared);
        let cells = lay_out(&tree, Holds::Build, area(100., 3.), ROW, px(4.));
        let run = Holds::Rest(Run {
            parent: Parent::Build,
            first: 0,
            count: 2,
            weight: 2,
        });
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 100.),
                (run, 0., 4.),
                (Holds::Shared, 4., 96.),
                (Holds::Node(2), 4., 96.),
            ]
        );
        let tree = Laid::sharing(&[leaf(1)], &shared);
        let cells = lay_out(&tree, Holds::Build, area(100., 2.), ROW, px(4.));
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 100.),
                (Holds::Node(0), 0., 4.),
                (Holds::Shared, 4., 96.),
            ]
        );
    }

    /// However little the shared tops weigh beside the roots, their cell is
    /// laid out, and they beneath it.
    #[test]
    fn shared_tops_outweighed_keep_their_cell() {
        let tree = Laid::sharing(&[leaf(600), leaf(399)], &[leaf(1)]);
        let cells = lay_out(&tree, Holds::Build, area(100., 3.), ROW, px(4.));
        let holds: Vec<Holds> = cells.iter().map(|cell| cell.holds).collect();
        assert_eq!(
            holds,
            [
                Holds::Build,
                Holds::Node(0),
                Holds::Node(1),
                Holds::Shared,
                Holds::Node(2),
            ]
        );
        assert_eq!(
            drawn(&cells)[3..],
            [(Holds::Shared, 96., 4.), (Holds::Node(2), 96., 4.)]
        );
    }

    /// A row too narrow for two cells of the least width is halved between
    /// the roots and the shared tops.
    #[test]
    fn a_row_too_narrow_for_two_cells_is_halved() {
        let tree = Laid::sharing(&[leaf(1)], &[leaf(999)]);
        let cells = lay_out(&tree, Holds::Build, area(6., 2.), ROW, px(4.));
        let run = Holds::Rest(Run {
            parent: Parent::Build,
            first: 0,
            count: 1,
            weight: 1,
        });
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 6.),
                (run, 0., 3.),
                (Holds::Shared, 3., 3.),
            ]
        );
    }

    #[test]
    fn a_rest_too_narrow_to_see_is_left_out() {
        let top = Spec(1002, vec![leaf(1000), leaf(1)]);
        let tree = Laid::of(&[top]);
        let cells = lay_out(&tree, Holds::Build, area(100., 3.), ROW, px(5.));
        let holds: Vec<Holds> = cells.iter().map(|cell| cell.holds).collect();
        assert_eq!(holds, [Holds::Build, Holds::Node(0), Holds::Node(1)]);
    }

    /// Under the whole build, and in a run of its siblings on top, a point
    /// far lighter than those drawn apart keeps a rest cell wide enough to
    /// reach.
    #[test]
    fn a_rest_under_the_whole_build_is_never_left_out() {
        let tree = Laid::of(&[leaf(999), leaf(1)]);
        let rest = Holds::Rest(Run {
            parent: Parent::Build,
            first: 1,
            count: 1,
            weight: 1,
        });
        let cells = lay_out(&tree, Holds::Build, area(100., 2.), ROW, px(4.));
        assert_eq!(
            drawn(&cells),
            [
                (Holds::Build, 0., 100.),
                (Holds::Node(0), 0., 96.),
                (rest, 96., 4.),
            ]
        );
        let run = Holds::Rest(Run {
            parent: Parent::Build,
            first: 0,
            count: 2,
            weight: 1000,
        });
        let cells = lay_out(&tree, run, area(100., 2.), ROW, px(4.));
        assert_eq!(
            drawn(&cells),
            [(run, 0., 100.), (Holds::Node(0), 0., 96.), (rest, 96., 4.)]
        );
    }

    /// However many points stand beneath a node, no more cells are laid
    /// out than the rows hold at the narrowest width drawn.
    #[test]
    fn a_wide_tree_lays_out_only_what_is_visible() {
        const LEAVES: u32 = 100_000;
        let tree = Laid::of(&[Spec(LEAVES + 1, vec![leaf(1); LEAVES as usize])]);
        let cells = lay_out(&tree, Holds::Build, area(200., 4.), ROW, px(4.));
        assert_eq!(cells.len(), 3);
        assert_eq!(
            cells[2].holds,
            Holds::Rest(Run {
                parent: Parent::Node(0),
                first: 1,
                count: LEAVES,
                weight: LEAVES,
            })
        );
    }

    #[test]
    fn an_empty_tree_or_a_canvas_too_short_for_a_row_lays_out_nothing() {
        assert!(lay_out(&Laid::of(&[]), Holds::Build, area(100., 3.), ROW, px(1.)).is_empty());
        let tree = Laid::of(&[leaf(1)]);
        assert!(lay_out(&tree, Holds::Build, area(100., 0.5), ROW, px(1.)).is_empty());
    }
}
