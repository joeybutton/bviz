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
        Self { boxes: HashSet::new(), edges: HashSet::new() }
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
