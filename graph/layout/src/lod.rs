//! Folds a containment forest to a bounded level-of-detail view.
//!
//! Only the descendants of *expanded* containers are shown; every other subtree
//! folds into its shallowest collapsed ancestor, which then renders as a leaf.
//! A singleton container (exactly one child) is always shown expanded, so it
//! never acts as a fold boundary or an expand/collapse handle. Running this
//! before quotient and layout shrinks a graph of tens of thousands of nodes to
//! a view whose size the caller caps.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};

use crate::graph::{EdgeKind, Graph};

/// The ids of nodes that are containers (named as some node's `parent`).
pub fn containers(graph: &Graph) -> HashSet<String> {
    graph
        .nodes
        .iter()
        .filter_map(|n| n.parent.clone())
        .collect()
}

/// The ids of multi-child containers (two or more children). Only these get an
/// expand/collapse handle: leaves can't expand and singletons auto-expand.
pub fn expandable_containers(graph: &Graph) -> HashSet<String> {
    child_counts(graph)
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Number of nodes naming each container as their `parent`.
fn child_counts(graph: &Graph) -> HashMap<&str, usize> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for node in &graph.nodes {
        if let Some(parent) = node.parent.as_deref() {
            *counts.entry(parent).or_default() += 1;
        }
    }
    counts
}

/// Collapses `graph` to the view in which only the descendants of *effectively
/// expanded* containers are shown — a container is effectively expanded when it
/// is in `expanded` or is a singleton (exactly one child), so single-child
/// chains always reveal their descendants. Every node folds to its
/// "representative": the shallowest node on its root->node path that is neither
/// in `expanded` nor a singleton (or the node itself when its whole ancestor
/// chain is effectively expanded). Each distinct representative becomes one node
/// (keeping that node's label/kind/parent), so a collapsed container renders as
/// a leaf. Edges are remapped to their endpoints' representatives, self-loops
/// (both endpoints in the same collapsed subtree) dropped, and duplicates merged
/// keeping their `EdgeKind`.
pub fn collapse(graph: &Graph, expanded: &HashSet<String>) -> Graph {
    let indexed = IndexedGraph::new(graph);
    let rep = indexed.reps(&indexed.expanded_indices(expanded));

    let mut out = Graph::new();
    for (i, node) in graph.nodes.iter().enumerate() {
        if rep[i] == i as u32 {
            out.add_node(
                node.id.clone(),
                node.label.clone(),
                node.kind.clone(),
                node.parent.clone(),
            );
        }
    }

    let mut seen: HashSet<(u32, u32, EdgeKind)> = HashSet::new();
    for &(from, to, kind) in &indexed.edges {
        let (from, to) = (rep[from as usize], rep[to as usize]);
        if from != to && seen.insert((from, to, kind)) {
            out.add_edge(
                indexed.id_str[from as usize].to_string(),
                indexed.id_str[to as usize].to_string(),
                kind,
            );
        }
    }
    out
}

/// A startup expansion set: the largest prefix of the reachable multi-child
/// containers — ordered cheapest-first, by the number of nodes each reveals —
/// whose collapsed view fits both `max_nodes` and `max_edges`. Singletons
/// auto-expand in `collapse` and leaves can't expand, so only multi-child
/// containers are candidates; their direct children's nearest multi-child
/// descendants become reachable as each is expanded. Visiting the cheapest
/// container next keeps each prefix step small, so the search settles close to
/// the caps rather than overshooting a whole level and falling far short.
pub fn default_expansion(graph: &Graph, max_nodes: usize, max_edges: usize) -> HashSet<String> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut roots: Vec<&str> = Vec::new();
    for node in &graph.nodes {
        match node.parent.as_deref() {
            Some(parent) => children.entry(parent).or_default().push(&node.id),
            None => roots.push(&node.id),
        }
    }

    // Nearest multi-child container at or below `n`, walking through single-child
    // nodes (which auto-expand) until a node with two or more or zero children.
    fn descend<'a>(children: &HashMap<&'a str, Vec<&'a str>>, mut n: &'a str) -> Option<&'a str> {
        loop {
            match children.get(n).map(Vec::as_slice) {
                Some([only]) => n = only,
                Some(_) => return Some(n),
                None => return None,
            }
        }
    }

    // Nodes revealed by expanding a container: its direct children plus the
    // singleton descendants that auto-expand beneath each.
    fn visible_subtree<'a>(
        children: &HashMap<&'a str, Vec<&'a str>>,
        n: &'a str,
        memo: &mut HashMap<&'a str, usize>,
    ) -> usize {
        if let Some(&v) = memo.get(n) {
            return v;
        }
        let v = 1 + match children.get(n).map(Vec::as_slice) {
            Some([only]) => visible_subtree(children, only, memo),
            _ => 0,
        };
        memo.insert(n, v);
        v
    }

    // Best-first over reachable multi-child containers, cheapest reveal first
    // (ties by id for a stable order). A container is reached only after its
    // nearest multi-child ancestor is taken, so `order` is ancestor-closed and
    // its prefixes nest.
    let mut order: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut memo: HashMap<&str, usize> = HashMap::new();
    let mut heap: BinaryHeap<Reverse<(usize, &str)>> = BinaryHeap::new();
    for r in &roots {
        if let Some(d) = descend(&children, r) {
            let reveal = children[d].iter().map(|c| visible_subtree(&children, c, &mut memo)).sum();
            heap.push(Reverse((reveal, d)));
        }
    }
    while let Some(Reverse((_, c))) = heap.pop() {
        if !seen.insert(c) {
            continue;
        }
        order.push(c);
        for &k in &children[c] {
            if let Some(d) = descend(&children, k) {
                if !seen.contains(d) {
                    let reveal =
                        children[d].iter().map(|c| visible_subtree(&children, c, &mut memo)).sum();
                    heap.push(Reverse((reveal, d)));
                }
            }
        }
    }

    // Both counts rise monotonically with the prefix length (more expansion only
    // reveals nodes and de-aggregates edges), so binary-search the largest prefix
    // whose collapsed view fits both caps. The binary search probes many
    // candidate prefixes, so it counts each view over an integer-indexed copy of
    // the graph rather than materializing the collapsed graph every time.
    let indexed = IndexedGraph::new(graph);
    let order_idx: Vec<u32> = order.iter().map(|id| indexed.idx[id]).collect();
    let fits = |k: usize| {
        let (nodes, edges) = indexed.counts(&order_idx[..k]);
        nodes <= max_nodes && edges <= max_edges
    };
    let (mut lo, mut hi) = (0usize, order.len());
    while lo < hi {
        let mid = lo + (hi - lo + 1) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    order[..lo].iter().map(|s| s.to_string()).collect()
}

/// An integer-indexed copy of a graph's containment forest and edges, so a
/// collapsed view can be computed (by [`collapse`]) or merely sized (by
/// [`Self::counts`], for the startup search) without re-walking string ids.
/// Every node id is interned to an index; edge endpoints that are not nodes
/// intern to their own indices past `node_count`, so they fold to themselves
/// exactly as [`collapse`] leaves them.
struct IndexedGraph<'a> {
    idx: HashMap<&'a str, u32>,
    /// The string id of each index, for materializing a collapsed view's edges.
    id_str: Vec<&'a str>,
    /// Containment parent of each index, or `None` for a root.
    parent: Vec<Option<u32>>,
    /// Whether each index is a singleton container, which always auto-expands.
    singleton: Vec<bool>,
    edges: Vec<(u32, u32, EdgeKind)>,
    /// Indices `0..node_count` are the graph's own nodes; higher indices are
    /// non-node edge endpoints and never count as collapsed-view nodes.
    node_count: usize,
}

impl<'a> IndexedGraph<'a> {
    fn new(graph: &'a Graph) -> Self {
        let child_counts = child_counts(graph);
        let mut idx: HashMap<&str, u32> = HashMap::with_capacity(graph.nodes.len());
        let mut id_str: Vec<&str> = Vec::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            idx.insert(node.id.as_str(), idx.len() as u32);
            id_str.push(node.id.as_str());
        }
        let node_count = graph.nodes.len();
        let mut parent: Vec<Option<u32>> = graph
            .nodes
            .iter()
            .map(|n| n.parent.as_deref().map(|p| idx[p]))
            .collect();
        let mut singleton: Vec<bool> = graph
            .nodes
            .iter()
            .map(|n| child_counts.get(n.id.as_str()) == Some(&1))
            .collect();
        let mut intern = |id: &'a str, idx: &mut HashMap<&'a str, u32>| -> u32 {
            *idx.entry(id).or_insert_with(|| {
                parent.push(None);
                singleton.push(false);
                id_str.push(id);
                (parent.len() - 1) as u32
            })
        };
        let edges = graph
            .edges
            .iter()
            .map(|e| {
                let from = intern(e.from.as_str(), &mut idx);
                let to = intern(e.to.as_str(), &mut idx);
                (from, to, e.kind)
            })
            .collect();
        Self { idx, id_str, parent, singleton, edges, node_count }
    }

    /// The indices of the `expanded` ids that name a node in this graph.
    fn expanded_indices(&self, expanded: &HashSet<String>) -> Vec<u32> {
        expanded.iter().filter_map(|id| self.idx.get(id.as_str()).copied()).collect()
    }

    /// The representative each index folds to under `expanded`: the shallowest
    /// node on its root->node chain that is neither expanded nor a singleton
    /// (both always reveal their children), or the node itself when its whole
    /// chain is. A node is its own representative exactly when [`collapse`]
    /// emits it.
    fn reps(&self, expanded: &[u32]) -> Vec<u32> {
        let mut eff = self.singleton.clone();
        for &i in expanded {
            eff[i as usize] = true;
        }
        (0..self.parent.len() as u32)
            .map(|i| {
                let mut best = if eff[i as usize] { None } else { Some(i) };
                let mut cur = i;
                while let Some(p) = self.parent[cur as usize] {
                    if !eff[p as usize] {
                        best = Some(p);
                    }
                    cur = p;
                }
                best.unwrap_or(i)
            })
            .collect()
    }

    /// The (node, edge) counts of the view collapsing every container outside
    /// `expanded` (given as indices), without materializing it: representatives
    /// that are graph nodes, and edges remapped to representatives, self-loops
    /// dropped, deduped by `(from, to, kind)`.
    fn counts(&self, expanded: &[u32]) -> (usize, usize) {
        let rep = self.reps(expanded);
        let nodes = (0..self.node_count).filter(|&i| rep[i] == i as u32).count();
        let mut seen: HashSet<(u32, u32, EdgeKind)> = HashSet::new();
        for &(from, to, kind) in &self.edges {
            let (from, to) = (rep[from as usize], rep[to as usize]);
            if from != to {
                seen.insert((from, to, kind));
            }
        }
        (nodes, seen.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Kind;

    fn expanded(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn set<'a>(ids: &[&'a str]) -> HashSet<&'a str> {
        ids.iter().copied().collect()
    }

    fn node_ids(graph: &Graph) -> HashSet<&str> {
        graph.nodes.iter().map(|n| n.id.as_str()).collect()
    }

    fn edges(graph: &Graph) -> HashSet<(&str, &str)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
    }

    fn find<'a>(graph: &'a Graph, id: &str) -> &'a super::super::graph::Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap()
    }

    /// Two roots A and B, each a multi-child container holding `x`/`y`
    /// grandchild leaves, so each is a fold boundary.
    fn two_towers() -> Graph {
        let mut g = Graph::new();
        for root in ["A", "B"] {
            g.add_node(root, root, Kind::Package, None);
            for kid in ["x", "y"] {
                let child = format!("{root}.{kid}");
                g.add_node(child, "leaf", Kind::Action, Some(root.to_string()));
            }
        }
        g
    }

    /// A single-child chain `A -> b -> c -> leaf`: every node but the leaf is a
    /// singleton container.
    fn singleton_chain() -> Graph {
        let mut g = Graph::new();
        g.add_node("A", "A", Kind::Package, None);
        g.add_node("b", "b", Kind::Target, Some("A".to_string()));
        g.add_node("c", "c", Kind::Target, Some("b".to_string()));
        g.add_node("leaf", "leaf", Kind::Action, Some("c".to_string()));
        g
    }

    #[test]
    fn containers_are_the_parent_referenced_ids() {
        let g = two_towers();
        assert_eq!(containers(&g), expanded(&["A", "B"]));
    }

    #[test]
    fn expandable_containers_are_only_multi_child() {
        let mut g = singleton_chain();
        // Make `b` a two-child container; its other child is a leaf.
        g.add_node("b.extra", "leaf", Kind::Action, Some("b".to_string()));

        // The singleton-only ancestors (A) and leaves never qualify; `b` does.
        assert_eq!(expandable_containers(&g), expanded(&["b"]));
    }

    #[test]
    fn empty_expanded_folds_each_tower_to_its_root() {
        let mut g = two_towers();
        // Cross-tower edge between leaves of the two folded roots.
        g.add_edge("A.x", "B.y", EdgeKind::Aquery);

        let out = collapse(&g, &HashSet::new());

        assert_eq!(node_ids(&out), set(&["A", "B"]));
        assert_eq!(edges(&out), [("A", "B")].into_iter().collect());
        // The folded root keeps its own kind and renders as a leaf (below).
        assert_eq!(find(&out, "A").kind, Kind::Package);
    }

    #[test]
    fn singleton_chain_auto_expands_whole_chain() {
        let g = singleton_chain();
        // No container in the chain is expandable, and yet, with nothing
        // expanded, every singleton auto-expands down to the leaf.
        assert!(expandable_containers(&g).is_empty());

        let out = collapse(&g, &HashSet::new());

        assert_eq!(node_ids(&out), set(&["A", "b", "c", "leaf"]));
    }

    #[test]
    fn multi_child_container_is_a_collapse_boundary() {
        let mut g = Graph::new();
        g.add_node("R", "R", Kind::Package, None);
        g.add_node("x", "leaf", Kind::Action, Some("R".to_string()));
        g.add_node("y", "leaf", Kind::Action, Some("R".to_string()));

        // R is multi-child, so it is the only expand handle and a fold boundary.
        assert_eq!(expandable_containers(&g), expanded(&["R"]));
        let out = collapse(&g, &HashSet::new());
        assert_eq!(node_ids(&out), set(&["R"]));
    }

    #[test]
    fn multi_child_root_over_singleton_chains() {
        let mut g = Graph::new();
        g.add_node("R", "R", Kind::Package, None);
        // Each of R's two children is the head of a singleton chain to a leaf.
        for c in ["p", "q"] {
            g.add_node(c, c, Kind::Target, Some("R".to_string()));
            let mid = format!("{c}.mid");
            let leaf = format!("{c}.leaf");
            g.add_node(mid.clone(), "mid", Kind::Target, Some(c.to_string()));
            g.add_node(leaf, "leaf", Kind::Action, Some(mid));
        }

        // R is the sole expand handle; its singleton descendants are transparent.
        assert_eq!(expandable_containers(&g), expanded(&["R"]));

        // Folded: R alone (its children are singletons but R is the boundary).
        assert_eq!(node_ids(&collapse(&g, &HashSet::new())), set(&["R"]));

        // Expanded: R, both children, and each child's auto-expanded chain.
        let out = collapse(&g, &expanded(&["R"]));
        assert_eq!(
            node_ids(&out),
            set(&["R", "p", "p.mid", "p.leaf", "q", "q.mid", "q.leaf"]),
        );
    }

    #[test]
    fn cross_child_edge_under_one_expanded_root() {
        let mut g = Graph::new();
        g.add_node("R", "R", Kind::Package, None);
        for c in ["c1", "c2"] {
            g.add_node(c, c, Kind::Target, Some("R".to_string()));
            // Two leaves keep each child a multi-child fold boundary.
            for leaf in ["a", "b"] {
                let id = format!("{c}.{leaf}");
                g.add_node(id, "leaf", Kind::Action, Some(c.to_string()));
            }
        }
        // Internal to c1 (self-loop once folded), and cross-child c1 -> c2.
        g.add_edge("c1.a", "c1.b", EdgeKind::Aquery);
        g.add_edge("c1.a", "c2.a", EdgeKind::Aquery);

        let out = collapse(&g, &expanded(&["R"]));

        // The expanded root and its two children are shown; grandchildren fold
        // into their child.
        assert_eq!(node_ids(&out), set(&["R", "c1", "c2"]));
        // c1.a -> c1.b is a self-loop (both fold to c1) and dropped; the
        // cross-child c1.a -> c2.a remaps to c1 -> c2.
        assert_eq!(edges(&out), [("c1", "c2")].into_iter().collect());
    }

    #[test]
    fn duplicate_edges_dedup_distinct_kinds_kept() {
        let mut g = two_towers();
        // Two A-subtree -> B-subtree edges of the same kind fold to one A -> B.
        g.add_edge("A.x", "B.x", EdgeKind::Aquery);
        g.add_edge("A.y", "B.y", EdgeKind::Aquery);
        // A different kind between the same folded pair survives separately.
        g.add_edge("A.x", "B.y", EdgeKind::Query);

        let out = collapse(&g, &HashSet::new());

        let kinds: HashSet<EdgeKind> = out
            .edges
            .iter()
            .filter(|e| e.from == "A" && e.to == "B")
            .map(|e| e.kind)
            .collect();
        assert_eq!(kinds, [EdgeKind::Aquery, EdgeKind::Query].into_iter().collect());
        assert_eq!(out.edges.len(), 2);
    }

    #[test]
    fn default_expansion_honors_caps() {
        let mut g = two_towers();
        g.add_edge("A.x", "B.y", EdgeKind::Aquery);

        // Caps that only the root view (2 nodes, 1 edge) fits: no root expands.
        let tiny = default_expansion(&g, 2, 1);
        assert!(tiny.is_empty());
        let v = collapse(&g, &tiny);
        assert!(v.nodes.len() <= 2 && v.edges.len() <= 1);

        // Generous caps expand every multi-child container: the full graph shows.
        let all = default_expansion(&g, 100, 100);
        let v = collapse(&g, &all);
        assert_eq!(v.nodes.len(), g.nodes.len());
        assert!(v.nodes.len() <= 100 && v.edges.len() <= 100);
        assert_eq!(all, expandable_containers(&g));
    }

    #[test]
    fn default_expansion_view_fits_both_caps() {
        let mut g = two_towers();
        // A single folded edge between the towers, so the minimal (root) view of
        // 2 nodes and 1 edge always fits these caps.
        g.add_edge("A.x", "B.y", EdgeKind::Aquery);

        for (max_nodes, max_edges) in [(2, 1), (4, 1), (4, 2), (6, 2), (100, 100)] {
            let view = collapse(&g, &default_expansion(&g, max_nodes, max_edges));
            assert!(view.nodes.len() <= max_nodes);
            assert!(view.edges.len() <= max_edges);
        }
    }

    #[test]
    fn default_expansion_sees_through_singleton_chains() {
        // A singleton-chain root whose chain ends in a multi-child container.
        let mut g = Graph::new();
        g.add_node("A", "A", Kind::Package, None);
        g.add_node("b", "b", Kind::Target, Some("A".to_string()));
        for leaf in ["x", "y"] {
            g.add_node(leaf, "leaf", Kind::Action, Some("b".to_string()));
        }

        // `b` is the reachable multi-child container; A is a transparent
        // singleton, so generous caps expand `b` alone.
        let all = default_expansion(&g, 100, 100);
        assert_eq!(all, expanded(&["b"]));
        assert_eq!(node_ids(&collapse(&g, &all)), set(&["A", "b", "x", "y"]));
    }

    #[test]
    fn collapsed_container_is_emitted_as_a_leaf() {
        let g = two_towers();
        let out = collapse(&g, &HashSet::new());
        // A is a container in the source graph but, folded, nothing names it as
        // parent in the collapsed view.
        assert!(out.nodes.iter().all(|n| n.parent.as_deref() != Some("A")));
        assert!(node_ids(&out).contains("A"));
    }
}
