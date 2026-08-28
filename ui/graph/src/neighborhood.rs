use crate::relation::{Direction, Relation};
use bviz_graph_layout::{Id, LaidOut};
use std::collections::{HashMap, HashSet, VecDeque};

/// What a marking picks out of the graph, empty where nothing is marked.
pub(crate) struct Neighborhood {
    /// The marked node's subtree, together with the nodes the relation reaches
    /// from it.
    pub(crate) boxes: HashSet<Id>,
    /// The drawn connectors tying those together.
    pub(crate) edges: HashSet<(Id, Id)>,
}

impl Neighborhood {
    pub(crate) fn none() -> Self {
        Self {
            boxes: HashSet::new(),
            edges: HashSet::new(),
        }
    }
}

/// What marking `of` under `relation` picks out. Its subtree is derived
/// geometrically, the layout nesting a box's children within its own rect, so the
/// boxes falling inside it are exactly its descendants.
pub(crate) fn compute_neighborhood(
    g: &LaidOut,
    of: Option<Id>,
    relation: Relation,
) -> Neighborhood {
    let Some(of) = of else {
        return Neighborhood::none();
    };
    let Some(h) = g.boxes.iter().find(|b| b.id == of) else {
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
    // A route drawn within the subtree runs between two marked boxes however far
    // the relation reaches past it.
    let edges: HashSet<(Id, Id)> = g
        .connectors
        .iter()
        .filter(|c| subtree.contains(&c.from) && subtree.contains(&c.to))
        .map(|c| (c.from, c.to))
        .collect();
    let mut marked = Neighborhood {
        boxes: subtree.clone(),
        edges,
    };
    match relation {
        Relation::None => {}
        Relation::Direct(direction) => mark_direct(g, &subtree, direction, &mut marked),
        Relation::Transitive(direction) => mark_transitive(g, &subtree, direction, &mut marked),
    }
    marked
}

/// Marks the nodes one edge from `subtree`, and the drawn connectors reaching
/// them.
///
/// Every drawn edge incident to the subtree is a surviving direct-neighbor edge
/// — the drawn graph is a subset of the full connectivity — so mark them all; a
/// compound box thus marks every member's neighbor edge in one go. A neighbor
/// whose direct edge reduction dropped is reached by no such edge; for it alone,
/// trace the shortest drawn path so it is not left floating. That path
/// contributes edges only: its interior nodes are conduits, so the marked boxes
/// hold the direct neighbors alone.
fn mark_direct(
    g: &LaidOut,
    subtree: &HashSet<Id>,
    direction: Direction,
    marked: &mut Neighborhood,
) {
    // The far endpoints are split by direction so the path tracing below follows
    // the dependency edges the right way.
    let mut out_targets: HashSet<Id> = HashSet::new();
    let mut in_targets: HashSet<Id> = HashSet::new();
    for &(from, to) in &g.full_edges {
        let (f_in, t_in) = (subtree.contains(&from), subtree.contains(&to));
        if f_in && !t_in && direction.outward() {
            out_targets.insert(to);
        } else if t_in && !f_in && direction.inward() {
            in_targets.insert(from);
        }
    }
    marked.boxes.extend(out_targets.iter().copied());
    marked.boxes.extend(in_targets.iter().copied());
    for c in &g.connectors {
        if (subtree.contains(&c.from) && out_targets.contains(&c.to))
            || (subtree.contains(&c.to) && in_targets.contains(&c.from))
        {
            marked.edges.insert((c.from, c.to));
        }
    }
    // Neighbors a drawn edge already reaches need no path.
    let reached: HashSet<Id> = marked
        .edges
        .iter()
        .flat_map(|&(f, t)| [f, t])
        .filter(|n| !subtree.contains(n))
        .collect();
    let out_missing: HashSet<Id> = out_targets.difference(&reached).copied().collect();
    let in_missing: HashSet<Id> = in_targets.difference(&reached).copied().collect();
    if !out_missing.is_empty() {
        let adj = adjacency(drawn_edges(g), true);
        trace_paths(subtree, &out_missing, &adj, true, &mut marked.edges);
    }
    if !in_missing.is_empty() {
        let adj = adjacency(drawn_edges(g), false);
        trace_paths(subtree, &in_missing, &adj, false, &mut marked.edges);
    }
}

/// Marks every node a path of edges reaches from `subtree`, and the drawn
/// connectors running between marked nodes along the direction walked.
///
/// No path is traced here, unlike in a direct marking: a node a drawn path
/// reaches is reachable over the full connectivity too — the drawn graph being a
/// subset of it — and so is itself marked, which leaves every edge of that path
/// running between two marked nodes and thus marked with them.
fn mark_transitive(
    g: &LaidOut,
    subtree: &HashSet<Id>,
    direction: Direction,
    marked: &mut Neighborhood,
) {
    let outward = match direction.outward() {
        true => reachable(&g.full_edges, subtree, true),
        false => HashSet::new(),
    };
    let inward = match direction.inward() {
        true => reachable(&g.full_edges, subtree, false),
        false => HashSet::new(),
    };
    marked.boxes.extend(outward.iter().copied());
    marked.boxes.extend(inward.iter().copied());
    for c in &g.connectors {
        let along =
            outward.contains(&c.to) && (subtree.contains(&c.from) || outward.contains(&c.from));
        let against =
            inward.contains(&c.from) && (subtree.contains(&c.to) || inward.contains(&c.to));
        if along || against {
            marked.edges.insert((c.from, c.to));
        }
    }
}

/// Every node `edges` carries a walk from `subtree` to, following each edge from
/// its source when `forward` and from its target otherwise. The subtree's own
/// nodes are left out, so what comes back is what lies beyond it.
fn reachable(edges: &[(Id, Id)], subtree: &HashSet<Id>, forward: bool) -> HashSet<Id> {
    let adj = adjacency(edges.iter().copied(), forward);
    let mut seen = subtree.clone();
    let mut queue: VecDeque<Id> = subtree.iter().copied().collect();
    let mut beyond = HashSet::new();
    while let Some(u) = queue.pop_front() {
        for &w in adj.get(&u).into_iter().flatten() {
            if seen.insert(w) {
                beyond.insert(w);
                queue.push_back(w);
            }
        }
    }
    beyond
}

/// The node pairs the connectors of `g` are routed for.
fn drawn_edges(g: &LaidOut) -> impl ExactSizeIterator<Item = (Id, Id)> + '_ {
    g.connectors.iter().map(|c| (c.from, c.to))
}

/// Indexes `edges` by their source when `forward` and by their target otherwise,
/// so a walk following them that way reads each node's successors in one lookup.
fn adjacency(
    edges: impl ExactSizeIterator<Item = (Id, Id)>,
    forward: bool,
) -> HashMap<Id, Vec<Id>> {
    let mut adj: HashMap<Id, Vec<Id>> = HashMap::with_capacity(edges.len());
    for (from, to) in edges {
        let (at, reaches) = match forward {
            true => (from, to),
            false => (to, from),
        };
        adj.entry(at).or_default().push(reaches);
    }
    adj
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

    /// What hovering `id` marks.
    fn hover(g: &LaidOut, id: u32) -> Neighborhood {
        compute_neighborhood(g, Some(Id(id)), Relation::Direct(Direction::Both))
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
        let n = hover(&g, 0);
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
        assert_eq!(hover(&g, 0).boxes, ids(&[0, 1]));
    }

    #[test]
    fn a_reduced_out_neighbor_is_marked_along_the_drawn_path() {
        let n = hover(&reduced_chain(2), 0);
        assert_eq!(n.boxes, ids(&[0, 1, 2]));
        assert_eq!(n.edges, pairs(&[(0, 1), (1, 2)]));
    }

    #[test]
    fn a_reduced_in_neighbor_is_marked_along_the_drawn_path() {
        let n = hover(&reduced_chain(2), 2);
        assert_eq!(n.boxes, ids(&[0, 1, 2]));
        assert_eq!(n.edges, pairs(&[(0, 1), (1, 2)]));
    }

    #[test]
    fn a_traced_path_marks_its_edges_but_not_its_interior_nodes() {
        let n = hover(&reduced_chain(3), 0);
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
        let first = hover(&g, 0);
        for _ in 0..REPEATS {
            let again = hover(&g, 0);
            assert_eq!(again.boxes, first.boxes);
            assert_eq!(again.edges, first.edges);
        }
    }

    #[test]
    fn a_hover_on_an_id_no_box_carries_marks_nothing() {
        let no_such_box = 9;
        let n = hover(&reduced_chain(2), no_such_box);
        assert!(n.boxes.is_empty());
        assert!(n.edges.is_empty());
    }

    #[test]
    fn no_hover_marks_nothing() {
        let n = compute_neighborhood(&reduced_chain(2), None, Relation::Direct(Direction::Both));
        assert!(n.boxes.is_empty());
        assert!(n.edges.is_empty());
    }

    /// A row of leaves the middle one has an input and an output in, so a
    /// directed marking from it takes one and leaves the other.
    fn one_in_one_out() -> LaidOut {
        drawing(
            (0..=2).map(leaf).collect(),
            &[(0, 1), (1, 2)],
            &[(0, 1), (1, 2)],
        )
    }

    #[test]
    fn marking_no_relation_marks_the_selected_node_alone() {
        let n = compute_neighborhood(&one_in_one_out(), Some(Id(1)), Relation::None);
        assert_eq!(n.boxes, ids(&[1]));
        assert!(n.edges.is_empty());
    }

    #[test]
    fn a_direct_marking_takes_only_the_direction_it_runs_in() {
        let g = one_in_one_out();
        let inputs = compute_neighborhood(&g, Some(Id(1)), Relation::Direct(Direction::In));
        assert_eq!(inputs.boxes, ids(&[0, 1]));
        assert_eq!(inputs.edges, pairs(&[(0, 1)]));
        let outputs = compute_neighborhood(&g, Some(Id(1)), Relation::Direct(Direction::Out));
        assert_eq!(outputs.boxes, ids(&[1, 2]));
        assert_eq!(outputs.edges, pairs(&[(1, 2)]));
    }

    #[test]
    fn a_direct_marking_stops_one_edge_out() {
        // A chain long enough that the node two edges along is left unmarked.
        let g = drawing(
            (0..=3).map(leaf).collect(),
            &[(0, 1), (1, 2), (2, 3)],
            &[(0, 1), (1, 2), (2, 3)],
        );
        let n = compute_neighborhood(&g, Some(Id(0)), Relation::Direct(Direction::Out));
        assert_eq!(n.boxes, ids(&[0, 1]));
        assert_eq!(n.edges, pairs(&[(0, 1)]));
    }

    #[test]
    fn a_transitive_marking_runs_the_whole_chain_the_direction_it_takes() {
        let g = drawing(
            (0..=3).map(leaf).collect(),
            &[(0, 1), (1, 2), (2, 3)],
            &[(0, 1), (1, 2), (2, 3)],
        );
        let outputs = compute_neighborhood(&g, Some(Id(1)), Relation::Transitive(Direction::Out));
        assert_eq!(outputs.boxes, ids(&[1, 2, 3]));
        assert_eq!(outputs.edges, pairs(&[(1, 2), (2, 3)]));
        let inputs = compute_neighborhood(&g, Some(Id(2)), Relation::Transitive(Direction::In));
        assert_eq!(inputs.boxes, ids(&[0, 1, 2]));
        assert_eq!(inputs.edges, pairs(&[(0, 1), (1, 2)]));
        let both = compute_neighborhood(&g, Some(Id(1)), Relation::Transitive(Direction::Both));
        assert_eq!(both.boxes, ids(&[0, 1, 2, 3]));
        assert_eq!(both.edges, pairs(&[(0, 1), (1, 2), (2, 3)]));
    }

    /// A node whose only outputs branch off a sibling stays clear of a marking
    /// running the other branch.
    #[test]
    fn a_transitive_marking_leaves_a_branch_it_does_not_reach() {
        let g = drawing(
            (0..=3).map(leaf).collect(),
            &[(0, 1), (0, 2), (2, 3)],
            &[(0, 1), (0, 2), (2, 3)],
        );
        let n = compute_neighborhood(&g, Some(Id(2)), Relation::Transitive(Direction::Out));
        assert_eq!(n.boxes, ids(&[2, 3]));
        assert_eq!(n.edges, pairs(&[(2, 3)]));
    }

    /// A transitive marking spans itself with drawn connectors, so a node whose
    /// direct edge reduction dropped is still tied to what marked it.
    #[test]
    fn a_transitive_marking_needs_no_path_traced_to_a_reduced_neighbor() {
        let n = compute_neighborhood(
            &reduced_chain(3),
            Some(Id(0)),
            Relation::Transitive(Direction::Out),
        );
        assert_eq!(n.boxes, ids(&[0, 1, 2, 3]));
        assert_eq!(n.edges, pairs(&[(0, 1), (1, 2), (2, 3)]));
    }

    /// A container marks what its members are related to, not what they are to
    /// one another: an edge inside it is drawn between marked boxes either way.
    #[test]
    fn a_container_marks_the_relations_of_every_member() {
        let g = drawing(
            vec![
                lbox(0, rect(0.0, 0.0, 400.0, 200.0), true),
                lbox(1, rect(20.0, 40.0, 100.0, 40.0), false),
                lbox(2, rect(200.0, 40.0, 100.0, 40.0), false),
                leaf(3),
                leaf(4),
            ],
            &[(1, 2), (3, 1), (2, 4)],
            &[(1, 2), (3, 1), (2, 4)],
        );
        let n = compute_neighborhood(&g, Some(Id(0)), Relation::Direct(Direction::Out));
        assert_eq!(n.boxes, ids(&[0, 1, 2, 4]));
        assert_eq!(n.edges, pairs(&[(1, 2), (2, 4)]));
    }
}
