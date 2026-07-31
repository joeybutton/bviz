use bviz_graph_layout::{Id, LaidOut};
use std::collections::{HashMap, HashSet, VecDeque};

/// The lit/dimmed partition induced by hovering a node.
pub(crate) struct Dimming {
    /// Boxes kept lit: the hovered node's subtree and its direct neighbors.
    pub(crate) lit: HashSet<Id>,
    /// Connectors kept lit.
    pub(crate) lit_edges: HashSet<(Id, Id)>,
    /// Whether any dimming is in effect; false when the hover reveals no
    /// incident edge or dimming is off.
    pub(crate) applies: bool,
}

impl Dimming {
    /// Nothing dimmed, so every box and connector paints at full strength.
    fn off() -> Self {
        Self { lit: HashSet::new(), lit_edges: HashSet::new(), applies: false }
    }
}

/// The lit/dimmed partition for the hovered node. Dimming traces the hovered
/// node's whole subtree neighborhood: the boxes geometrically nested inside the
/// hovered box (it and its descendants, since the layout nests children within
/// their parent's rect) plus the far endpoint of every edge incident to that
/// subtree. The subtree's incident edges stay lit and everything else dims, but
/// only when there is at least one such edge to reveal — an edgeless subtree (or
/// a disabled toggle) would fade the graph for no signal, so those suppress
/// dimming entirely. Each step below walks the whole view, so the cases that
/// suppress it are answered before any of them run.
pub(crate) fn compute_dimming(g: &LaidOut, hovered: Option<Id>, dim_enabled: bool) -> Dimming {
    let Some(hovered) = hovered.filter(|_| dim_enabled) else {
        return Dimming::off();
    };
    let Some(h) = g.boxes.iter().find(|b| b.id == hovered) else {
        return Dimming::off();
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
    // The subtree plus the far endpoint of each full-connectivity edge incident
    // to it: the direct neighbors, the only nodes a hover lights. This walks the
    // full connectivity, not the drawn connectors, so a neighbor still lights up
    // when reduction dropped the edge to it. The far endpoints are split by
    // direction so the path tracing below follows dependency edges the right way.
    // An empty active set (no incident edge) leaves dimming off.
    let mut active: HashSet<Id> = subtree.clone();
    let mut out_targets: HashSet<Id> = HashSet::new();
    let mut in_targets: HashSet<Id> = HashSet::new();
    let mut has_edge = false;
    for &(from, to) in &g.full_edges {
        let (f_in, t_in) = (subtree.contains(&from), subtree.contains(&to));
        if f_in || t_in {
            has_edge = true;
        }
        if f_in && !t_in {
            active.insert(to);
            out_targets.insert(to);
        } else if t_in && !f_in {
            active.insert(from);
            in_targets.insert(from);
        }
    }
    if !has_edge {
        active.clear();
    }
    // Edges to light. Every drawn edge incident to the subtree is a surviving
    // direct-neighbor edge — the drawn graph is a subset of the full
    // connectivity — so light them all; a compound box thus lights every
    // member's neighbor edge at once, not one at a time. A neighbor whose direct
    // edge reduction dropped is reached by no such edge; for it alone, trace the
    // shortest drawn path so it is not left floating. A path's interior nodes are
    // conduits, not neighbors, so they stay out of `lit` (the undimmed boxes
    // remain just the direct neighbors).
    let mut lit_edges: HashSet<(Id, Id)> = HashSet::new();
    if has_edge {
        for c in &g.connectors {
            if subtree.contains(&c.from) || subtree.contains(&c.to) {
                lit_edges.insert((c.from, c.to));
            }
        }
        // Neighbors a drawn edge already reaches need no path.
        let reached: HashSet<Id> = lit_edges
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
            trace_paths(&subtree, &out_missing, &out_adj, true, &mut lit_edges);
            trace_paths(&subtree, &in_missing, &in_adj, false, &mut lit_edges);
        }
    }
    Dimming {
        lit: active,
        lit_edges,
        applies: has_edge,
    }
}

/// Lights the shortest drawn path from `subtree` to each target. A breadth-first
/// walk over `adj` records each node's predecessor; reconstructing from a target
/// back to the subtree yields the path's edges. With `forward`, `adj` is the
/// out-adjacency and a predecessor edge `p->cur` is itself a drawn edge; otherwise
/// `adj` is the in-adjacency and the drawn edge runs the other way, `cur->p`. The
/// walk seeds the subtree in id order so a tie between equal-length paths resolves
/// the same way every paint, rather than flickering with hash iteration order.
fn trace_paths(
    subtree: &HashSet<Id>,
    targets: &HashSet<Id>,
    adj: &HashMap<Id, Vec<Id>>,
    forward: bool,
    lit: &mut HashSet<(Id, Id)>,
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
            lit.insert(if forward { (prev, cur) } else { (cur, prev) });
            cur = prev;
        }
    }
}
