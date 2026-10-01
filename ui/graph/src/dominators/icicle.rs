//! Where each cell of the icicle drawing a dominator tree stands.

use bviz_graph_layout::dominators::Dominators;
use gpui::{Bounds, Pixels, point, px, size};
use std::ops::Range;

/// A tree whose nodes are weighed by how many points each stands over, itself
/// among them, and whose siblings come heaviest first.
pub(crate) trait Tree {
    /// How many points the tree holds, which its tops' weights sum to.
    fn total(&self) -> u32;
    fn tops(&self) -> Range<u32>;
    fn children(&self, node: u32) -> Range<u32>;
    fn weight(&self, node: u32) -> u32;
}

impl Tree for Dominators {
    fn total(&self) -> u32 {
        Dominators::total(self)
    }

    fn tops(&self) -> Range<u32> {
        Dominators::tops(self)
    }

    fn children(&self, node: u32) -> Range<u32> {
        Dominators::children(self, node)
    }

    fn weight(&self, node: u32) -> u32 {
        Dominators::weight(self, node)
    }
}

/// What a cell of the icicle stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Holds {
    /// The whole build, over every top of the tree.
    Build,
    Node(u32),
    /// A run of siblings too narrow to draw apart.
    Rest(Run),
}

/// The siblings from `first` to the last of `parent`'s children, or of the
/// tops where there is no `parent`, which a row of the icicle had too little
/// room to draw apart. Only [`lay_out`] makes one, so `count` and `weight`
/// are always how many siblings the run holds and the points they weigh
/// together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    parent: Option<u32>,
    first: u32,
    count: u32,
    weight: u32,
}

impl Run {
    pub(crate) fn parent(&self) -> Option<u32> {
        self.parent
    }

    pub(crate) fn count(&self) -> u32 {
        self.count
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

/// The narrowest a run of siblings too narrow to draw apart is drawn at all.
const VISIBLE_WIDTH: Pixels = px(1.);

/// How much of the row beneath a run on top its own rest cell takes, where
/// the run's siblings are too many to draw apart at once.
const RUN_REST_SHARE: f32 = 0.2;

/// The cells of the icicle drawing `tree` from the cell holding `top`, in
/// `bounds`, a row `row` tall each.
///
/// The first row is `top` at full width, and each row beneath holds the
/// children of each cell above it, laid from its left edge at widths in
/// proportion to their weights, so what a cell leaves of its own width beyond
/// its children's is its own point. Only the rows `bounds` holds whole are
/// laid out. A child narrower than `min_width` ends its row of siblings: it
/// and every lighter sibling share one rest cell spanning the width left,
/// laid out where that is visible at all, and nothing below it. The cells
/// laid out are thus bounded by how many fit, whatever the size of the tree.
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
        cells.reserve(per_row + above.len());
        for parent in above {
            let Cell { rect, holds } = cells[parent];
            let (parent, laid, weight, own) = match holds {
                Holds::Build => (None, tree.tops(), tree.total(), 0),
                Holds::Node(node) => (Some(node), tree.children(node), tree.weight(node), 1),
                Holds::Rest(run) if depth == 1 => {
                    let siblings = run.first..run.first + run.count;
                    (run.parent, siblings, run.weight, 0)
                }
                Holds::Rest(_) => continue,
            };
            // The children drawn apart are those of `laid` wide enough at
            // `scale`; a rest cell gathers the rest up to `end`.
            let end = laid.end;
            let scale = rect.size.width / weight as f32;
            let (laid, scale) = match holds {
                Holds::Rest(run) if scale * (tree.weight(run.first) as f32) < min_width => {
                    let room = rect.size.width * (1. - RUN_REST_SHARE);
                    heaviest_page(tree, run, room, min_width)
                }
                _ => (laid, scale),
            };
            let (mut x, mut drawn) = (rect.origin.x, 0);
            let mut gathered = laid.end;
            for child in laid {
                let width = scale * tree.weight(child) as f32;
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
            let rest = rect.right() - scale * own as f32 - x;
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
        }
        above = start..cells.len();
        if above.is_empty() {
            break;
        }
    }
    cells
}

/// The heaviest siblings of `run` that each stay `min_width` wide when they
/// share `room` by their summed weight, and the scale that spreads them so.
/// Only as many siblings are weighed as are drawn, and one more.
fn heaviest_page(
    tree: &impl Tree,
    run: Run,
    room: Pixels,
    min_width: Pixels,
) -> (Range<u32>, Pixels) {
    let (mut end, mut summed) = (run.first, 0);
    for sibling in run.first..run.first + run.count {
        let weight = tree.weight(sibling);
        let widened = summed + weight;
        if room * (weight as f32 / widened as f32) < min_width {
            break;
        }
        (end, summed) = (sibling + 1, widened);
    }
    (run.first..end, room / summed.max(1) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree laid out as [`Dominators`] lays one out: each node's children
    /// consecutive, heaviest first, and the tops ahead of every other node.
    struct Laid {
        weights: Vec<u32>,
        /// By node, where its children start, and past the last node the node
        /// count.
        bounds: Vec<u32>,
    }

    /// A node by its weight and its children.
    #[derive(Clone)]
    struct Spec(u32, Vec<Spec>);

    impl Laid {
        fn of(tops: &[Spec]) -> Self {
            let mut order: Vec<&Spec> = tops.iter().collect();
            let mut bounds = Vec::new();
            let mut at = 0;
            while let Some(&spec) = order.get(at) {
                bounds.push(order.len() as u32);
                order.extend(spec.1.iter());
                at += 1;
            }
            bounds.push(order.len() as u32);
            Self {
                weights: order.iter().map(|spec| spec.0).collect(),
                bounds,
            }
        }
    }

    impl Tree for Laid {
        fn total(&self) -> u32 {
            self.tops().map(|top| self.weights[top as usize]).sum()
        }

        fn tops(&self) -> Range<u32> {
            0..self.bounds[0]
        }

        fn children(&self, node: u32) -> Range<u32> {
            self.bounds[node as usize]..self.bounds[node as usize + 1]
        }

        fn weight(&self, node: u32) -> u32 {
            self.weights[node as usize]
        }
    }

    fn leaf(weight: u32) -> Spec {
        Spec(weight, Vec::new())
    }

    const ROW: Pixels = px(10.);

    fn area(width: f32, rows: f32) -> Bounds<Pixels> {
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
            parent: Some(0),
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
            parent: Some(0),
            first: 2,
            count: 3,
            weight: 4,
        });
        let cells = lay_out(&tree, run, area(100., 3.), ROW, px(30.));
        let rest = |parent, first, count, weight| {
            Holds::Rest(Run {
                parent: Some(parent),
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
            parent: None,
            first: 0,
            count: 10,
            weight: 10,
        });
        assert_eq!(run, rest);
        let cells = lay_out(&tree, run, area(100., 2.), ROW, px(20.));
        let rest = Holds::Rest(Run {
            parent: None,
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

    #[test]
    fn a_rest_too_narrow_to_see_is_left_out() {
        let top = Spec(1001, vec![leaf(1000)]);
        let tree = Laid::of(&[top, leaf(1)]);
        let cells = lay_out(&tree, Holds::Build, area(100., 2.), ROW, px(5.));
        let holds: Vec<Holds> = cells.iter().map(|cell| cell.holds).collect();
        assert_eq!(holds, [Holds::Build, Holds::Node(0)]);
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
                parent: Some(0),
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
