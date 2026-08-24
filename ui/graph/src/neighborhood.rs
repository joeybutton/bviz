use bviz_graph_layout::{Id, LaidOut};
use std::collections::{HashMap, HashSet, VecDeque};

/// What a hover marks out of the graph, empty where nothing is marked.
pub(crate) struct Neighborhood {
    /// The hovered node's subtree and its direct neighbors.
    pub(crate) boxes: HashSet<Id>,
    /// The drawn connectors that reach those neighbors from the subtree.
    pub(crate) edges: HashSet<(Id, Id)>,
}

impl Neighborhood {
    fn none() -> Self {
        Self {
            boxes: HashSet::new(),
            edges: HashSet::new(),
        }
    }
}

/// What hovering `hovered` marks. Its subtree is derived geometrically, the
/// layout nesting a box's children within its own rect, so the boxes falling
/// inside it are exactly its descendants.
pub(crate) fn compute_neighborhood(g: &LaidOut, hovered: Option<Id>) -> Neighborhood {
    let Some(hovered) = hovered else {
        return Neighborhood::none();
    };
    let Some(h) = g.boxes.iter().find(|b| b.id == hovered) else {
        return Neighborhood::none();
    };
    let subtree: HashSet<Id> = g
        .boxes
        .iter()
        .filter(|b| {
            b.x >= h.x
                && b.y >= h.y
                && b.x + b.width <= h.x + h.width
                && b.y + b.height <= h.y + h.height
        })
        .map(|b| b.id)
        .collect();
    // The far endpoints are split by direction so the path tracing below follows
    // the dependency edges the right way.
    let mut boxes: HashSet<Id> = subtree.clone();
    let mut out_targets: HashSet<Id> = HashSet::new();
    let mut in_targets: HashSet<Id> = HashSet::new();
    for &(from, to) in &g.full_edges {
        let (f_in, t_in) = (subtree.contains(&from), subtree.contains(&to));
        if f_in && !t_in {
            boxes.insert(to);
            out_targets.insert(to);
        } else if t_in && !f_in {
            boxes.insert(from);
            in_targets.insert(from);
        }
    }
    // Every drawn edge incident to the subtree is a surviving direct-neighbor
    // edge — the drawn graph is a subset of the full connectivity — so mark them
    // all; a compound box thus marks every member's neighbor edge in one go. A
    // neighbor whose direct edge reduction dropped is reached by no such edge;
    // for it alone, trace the shortest drawn path so it is not left floating.
    // That path contributes edges only: its interior nodes are conduits, so
    // `boxes` holds the direct neighbors alone.
    let mut edges: HashSet<(Id, Id)> = HashSet::new();
    for c in &g.connectors {
        if subtree.contains(&c.from) || subtree.contains(&c.to) {
            edges.insert((c.from, c.to));
        }
    }
    // Neighbors a drawn edge already reaches need no path.
    let reached: HashSet<Id> = edges
        .iter()
        .flat_map(|&(f, t)| [f, t])
        .filter(|n| !subtree.contains(n))
        .collect();
    let out_missing: HashSet<Id> = out_targets.difference(&reached).copied().collect();
    let in_missing: HashSet<Id> = in_targets.difference(&reached).copied().collect();
    // Indexing the connectors costs an entry per drawn edge, so build the
    // adjacency only where a path has to be traced through it.
    if !out_missing.is_empty() || !in_missing.is_empty() {
        let mut out_adj: HashMap<Id, Vec<Id>> = HashMap::new();
        let mut in_adj: HashMap<Id, Vec<Id>> = HashMap::new();
        for c in &g.connectors {
            out_adj.entry(c.from).or_default().push(c.to);
            in_adj.entry(c.to).or_default().push(c.from);
        }
        trace_paths(&subtree, &out_missing, &out_adj, true, &mut edges);
        trace_paths(&subtree, &in_missing, &in_adj, false, &mut edges);
    }
    Neighborhood { boxes, edges }
}

/// Marks the shortest drawn path from `subtree` to each target. A breadth-first
/// walk over `adj` records each node's predecessor; reconstructing from a target
/// back to the subtree yields the path's edges. With `forward`, `adj` is the
/// out-adjacency and a predecessor edge `p->cur` is itself a drawn edge; otherwise
/// `adj` is the in-adjacency and the drawn edge runs the other way, `cur->p`. The
/// walk seeds the subtree in id order, so a tie between equal-length paths
/// resolves the same way every paint.
fn trace_paths(
    subtree: &HashSet<Id>,
    targets: &HashSet<Id>,
    adj: &HashMap<Id, Vec<Id>>,
    forward: bool,
    marked: &mut HashSet<(Id, Id)>,
) {
    let mut came: HashMap<Id, Id> = HashMap::new();
    let mut seen: HashSet<Id> = subtree.clone();
    let mut seeds: Vec<Id> = subtree.iter().copied().collect();
    seeds.sort_unstable_by_key(|id| id.0);
    let mut queue: VecDeque<Id> = seeds.into();
    while let Some(u) = queue.pop_front() {
        for &w in adj.get(&u).into_iter().flatten() {
            if seen.insert(w) {
                came.insert(w, u);
                queue.push_back(w);
            }
        }
    }
    for &target in targets {
        let mut cur = target;
        while !subtree.contains(&cur) {
            let Some(&prev) = came.get(&cur) else { break };
            marked.insert(if forward { (prev, cur) } else { (cur, prev) });
            cur = prev;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Rect;
    use bviz_graph_layout::{Box, Connector, EdgeKind, Kind};

    const LEAF_WIDTH: f64 = 100.0;
    const LEAF_HEIGHT: f64 = 40.0;
    /// The spacing of a row of leaves, wider than a leaf so none of them falls
    /// inside another.
    const LEAF_PITCH: f64 = 200.0;

    /// How many times a marking is recomputed to catch one that varies with hash
    /// iteration order: each call hashes into freshly seeded tables, so a tie
    /// left to them comes out both ways well within this many draws.
    const REPEATS: usize = 32;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    fn lbox(id: u32, r: Rect, compound: bool) -> Box {
        Box {
            id: Id(id),
            label: String::new(),
            kind: Kind::Source,
            x: r.x,
            y: r.y,
            width: r.w,
            height: r.h,
            compound,
            group: false,
        }
    }

    fn leaf(id: u32) -> Box {
        lbox(
            id,
            rect(f64::from(id) * LEAF_PITCH, 0.0, LEAF_WIDTH, LEAF_HEIGHT),
            false,
        )
    }

    /// A drawing of `boxes` over the connectivity `full`, of which `drawn` is the
    /// part a connector is routed for.
    fn drawing(boxes: Vec<Box>, drawn: &[(u32, u32)], full: &[(u32, u32)]) -> LaidOut {
        LaidOut {
            boxes,
            connectors: drawn
                .iter()
                .map(|&(from, to)| Connector {
                    points: Vec::new(),
                    kinds: vec![EdgeKind::Query],
                    from: Id(from),
                    to: Id(to),
                })
                .collect(),
            full_edges: full.iter().map(|&(from, to)| (Id(from), Id(to))).collect(),
            ..Default::default()
        }
    }

    /// Leaves `0..=last` in a row, consecutive ones drawn, the run's two ends
    /// connected by an edge reduction left undrawn.
    fn reduced_chain(last: u32) -> LaidOut {
        let drawn: Vec<(u32, u32)> = (1..=last).map(|i| (i - 1, i)).collect();
        let mut full = drawn.clone();
        full.push((0, last));
        drawing((0..=last).map(leaf).collect(), &drawn, &full)
    }

    fn ids(of: &[u32]) -> HashSet<Id> {
        of.iter().map(|&id| Id(id)).collect()
    }

    fn pairs(of: &[(u32, u32)]) -> HashSet<(Id, Id)> {
        of.iter().map(|&(from, to)| (Id(from), Id(to))).collect()
    }

    /// Nothing suppresses a hover for want of an edge on the hovered node.
    #[test]
    fn an_edgeless_hover_marks_its_subtree_alone() {
        let g = drawing(
            vec![
                lbox(0, rect(0.0, 0.0, 150.0, 100.0), true),
                lbox(1, rect(10.0, 20.0, 100.0, 40.0), false),
                leaf(2),
                leaf(3),
            ],
            &[(2, 3)],
            &[(2, 3)],
        );
        let n = compute_neighborhood(&g, Some(Id(0)));
        assert_eq!(n.boxes, ids(&[0, 1]));
        assert!(n.edges.is_empty());
    }

    #[test]
    fn a_container_marks_the_boxes_nested_in_it_not_one_overlapping_it() {
        let g = drawing(
            vec![
                lbox(0, rect(0.0, 0.0, 200.0, 200.0), true),
                lbox(1, rect(20.0, 40.0, 100.0, 40.0), false),
                lbox(2, rect(150.0, 150.0, 200.0, 200.0), false),
            ],
            &[],
            &[],
        );
        let n = compute_neighborhood(&g, Some(Id(0)));
        assert_eq!(n.boxes, ids(&[0, 1]));
    }

    #[test]
    fn a_reduced_out_neighbor_is_marked_along_the_drawn_path() {
        let n = compute_neighborhood(&reduced_chain(2), Some(Id(0)));
        assert_eq!(n.boxes, ids(&[0, 1, 2]));
        assert_eq!(n.edges, pairs(&[(0, 1), (1, 2)]));
    }

    #[test]
    fn a_reduced_in_neighbor_is_marked_along_the_drawn_path() {
        let n = compute_neighborhood(&reduced_chain(2), Some(Id(2)));
        assert_eq!(n.boxes, ids(&[0, 1, 2]));
        assert_eq!(n.edges, pairs(&[(0, 1), (1, 2)]));
    }

    #[test]
    fn a_traced_path_marks_its_edges_but_not_its_interior_nodes() {
        let n = compute_neighborhood(&reduced_chain(3), Some(Id(0)));
        assert_eq!(n.boxes, ids(&[0, 1, 3]));
        assert_eq!(n.edges, pairs(&[(0, 1), (1, 2), (2, 3)]));
    }

    /// Two drawn paths of equal length reach node 5, one from each member of the
    /// hovered container, so which of them is marked turns on the order the walk
    /// takes the members in.
    #[test]
    fn a_tie_between_equal_length_paths_resolves_alike_every_call() {
        let g = drawing(
            vec![
                lbox(0, rect(0.0, 0.0, 400.0, 200.0), true),
                lbox(1, rect(20.0, 40.0, 100.0, 40.0), false),
                lbox(2, rect(200.0, 40.0, 100.0, 40.0), false),
                leaf(3),
                leaf(4),
                leaf(5),
            ],
            &[(1, 3), (2, 4), (3, 5), (4, 5)],
            &[(1, 3), (2, 4), (3, 5), (4, 5), (1, 5)],
        );
        let first = compute_neighborhood(&g, Some(Id(0)));
        for _ in 0..REPEATS {
            let again = compute_neighborhood(&g, Some(Id(0)));
            assert_eq!(again.boxes, first.boxes);
            assert_eq!(again.edges, first.edges);
        }
    }

    #[test]
    fn a_hover_on_an_id_no_box_carries_marks_nothing() {
        let no_such_box = Id(9);
        let n = compute_neighborhood(&reduced_chain(2), Some(no_such_box));
        assert!(n.boxes.is_empty());
        assert!(n.edges.is_empty());
    }

    #[test]
    fn no_hover_marks_nothing() {
        let n = compute_neighborhood(&reduced_chain(2), None);
        assert!(n.boxes.is_empty());
        assert!(n.edges.is_empty());
    }
}
