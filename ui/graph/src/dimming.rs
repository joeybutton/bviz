use crate::build::trace_paths;
use bviz_graph_layout::{Id, LaidOut};
use std::collections::{HashMap, HashSet};

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

/// The lit/dimmed partition for the hovered node. Dimming traces the hovered
/// node's whole subtree neighborhood: the boxes geometrically nested inside the
/// hovered box (it and its descendants, since the layout nests children within
/// their parent's rect) plus the far endpoint of every edge incident to that
/// subtree. The subtree's incident edges stay lit and everything else dims, but
/// only when there is at least one such edge to reveal — an edgeless subtree (or
/// a disabled toggle) would fade the graph for no signal, so those suppress
/// dimming entirely.
pub(crate) fn compute_dimming(g: &LaidOut, hovered: Option<Id>, dim_enabled: bool) -> Dimming {
    let hovered_box = hovered.and_then(|h| g.boxes.iter().find(|b| b.id == h));
    let subtree: HashSet<Id> = hovered_box
        .filter(|_| dim_enabled)
        .map(|h| {
            g.boxes
                .iter()
                .filter(|b| {
                    b.x >= h.x
                        && b.y >= h.y
                        && b.x + b.width <= h.x + h.width
                        && b.y + b.height <= h.y + h.height
                })
                .map(|b| b.id)
                .collect()
        })
        .unwrap_or_default();
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
        let mut out_adj: HashMap<Id, Vec<Id>> = HashMap::new();
        let mut in_adj: HashMap<Id, Vec<Id>> = HashMap::new();
        for c in &g.connectors {
            out_adj.entry(c.from).or_default().push(c.to);
            in_adj.entry(c.to).or_default().push(c.from);
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
        trace_paths(&subtree, &out_missing, &out_adj, true, &mut lit_edges);
        trace_paths(&subtree, &in_missing, &in_adj, false, &mut lit_edges);
    }
    Dimming {
        lit: active,
        lit_edges,
        applies: has_edge,
    }
}
