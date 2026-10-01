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
    /// The last `count` of a run of siblings, each too narrow to draw apart,
    /// and the points they weigh together.
    Rest {
        count: u32,
        weight: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Cell {
    pub(crate) rect: Bounds<Pixels>,
    pub(crate) holds: Holds,
}

/// The narrowest a run of siblings too narrow to draw apart is drawn at all.
const VISIBLE_WIDTH: Pixels = px(1.);

/// The cells of the icicle drawing `tree` in `bounds`, a row `row` tall each.
///
/// The first row is the whole build at full width, and each row beneath
/// holds the children of each cell above it, laid from its left edge at widths
/// in proportion to their weights, so what a cell leaves of its own width
/// beyond its children's is its own point. Only the rows `bounds` holds whole
/// are laid out. A child narrower than `min_width` ends its row of siblings: it
/// and every lighter sibling share one rest cell spanning their summed width,
/// laid out where that is visible at all, and nothing below it. The cells laid
/// out are thus bounded by how many fit, whatever the size of the tree.
pub(crate) fn lay_out(
    tree: &impl Tree,
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
        holds: Holds::Build,
    });
    let mut above = 0..cells.len();
    for depth in 1..rows {
        let y = bounds.origin.y + row * depth as f32;
        let start = cells.len();
        cells.reserve(per_row + above.len());
        for parent in above {
            let Cell { rect, holds } = cells[parent];
            let (children, weight, own) = match holds {
                Holds::Build => (tree.tops(), tree.total(), 0),
                Holds::Node(node) => (tree.children(node), tree.weight(node), 1),
                Holds::Rest { .. } => continue,
            };
            let scale = rect.size.width / weight as f32;
            let (mut x, mut laid) = (rect.origin.x, 0);
            for child in children.clone() {
                let width = scale * tree.weight(child) as f32;
                if width < min_width {
                    let weight = weight - own - laid;
                    let width = scale * weight as f32;
                    if width >= VISIBLE_WIDTH {
                        let count = children.end - child;
                        cells.push(Cell {
                            rect: Bounds::new(point(x, y), size(width, row)),
                            holds: Holds::Rest { count, weight },
                        });
                    }
                    break;
                }
                cells.push(Cell {
                    rect: Bounds::new(point(x, y), size(width, row)),
                    holds: Holds::Node(child),
                });
                x += width;
                laid += tree.weight(child);
            }
        }
        above = start..cells.len();
        if above.is_empty() {
            break;
        }
    }
    cells
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
        let cells = lay_out(&tree, area(80., 3.), ROW, px(1.));
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
    fn rows_past_the_bottom_are_not_laid_out() {
        let chain = Spec(3, vec![Spec(2, vec![leaf(1)])]);
        let tree = Laid::of(&[chain]);
        let cells = lay_out(&tree, area(30., 2.5), ROW, px(1.));
        assert_eq!(cells.len(), 2);
    }

    #[test]
    fn siblings_too_narrow_to_draw_share_one_rest_cell() {
        // Under a top of 10: children of 5, 2, 1 and 1, the top itself the last.
        let top = Spec(10, vec![leaf(5), Spec(2, vec![leaf(1)]), leaf(1), leaf(1)]);
        let tree = Laid::of(&[top]);
        let cells = lay_out(&tree, area(100., 3.), ROW, px(25.));
        let rest = Holds::Rest {
            count: 3,
            weight: 4,
        };
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
    fn a_rest_too_narrow_to_see_is_left_out() {
        let top = Spec(1001, vec![leaf(1000)]);
        let tree = Laid::of(&[top, leaf(1)]);
        let cells = lay_out(&tree, area(100., 2.), ROW, px(5.));
        let holds: Vec<Holds> = cells.iter().map(|cell| cell.holds).collect();
        assert_eq!(holds, [Holds::Build, Holds::Node(0)]);
    }

    /// However many points stand beneath a node, no more cells are laid
    /// out than the rows hold at the narrowest width drawn.
    #[test]
    fn a_wide_tree_lays_out_only_what_is_visible() {
        const LEAVES: u32 = 100_000;
        let tree = Laid::of(&[Spec(LEAVES + 1, vec![leaf(1); LEAVES as usize])]);
        let cells = lay_out(&tree, area(200., 4.), ROW, px(4.));
        assert_eq!(cells.len(), 3);
        assert_eq!(
            cells[2].holds,
            Holds::Rest {
                count: LEAVES,
                weight: LEAVES,
            }
        );
    }

    #[test]
    fn an_empty_tree_or_a_canvas_too_short_for_a_row_lays_out_nothing() {
        assert!(lay_out(&Laid::of(&[]), area(100., 3.), ROW, px(1.)).is_empty());
        let tree = Laid::of(&[leaf(1)]);
        assert!(lay_out(&tree, area(100., 0.5), ROW, px(1.)).is_empty());
    }
}
