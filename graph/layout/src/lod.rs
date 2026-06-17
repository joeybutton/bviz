//! Folds a containment forest to a bounded level-of-detail view.
//!
//! Only the descendants of *expanded* containers are shown; every other subtree
//! folds into its shallowest collapsed ancestor, which then renders as a leaf.
//! A singleton container (exactly one child) is always shown expanded, so it
//! never acts as a fold boundary or an expand/collapse handle. Running this
//! before quotient and layout shrinks a graph of tens of thousands of nodes to
//! a view whose size the caller caps.

use std::collections::{HashMap, HashSet, VecDeque};

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
    let mut reps = Representatives::new(graph, expanded);

    let mut out = Graph::new();
    for node in &graph.nodes {
        if reps.of(&node.id) == node.id.as_str() {
            out.add_node(
                node.id.clone(),
                node.label.clone(),
                node.kind.clone(),
                node.parent.clone(),
            );
        }
    }

    let mut seen: HashSet<(String, String, EdgeKind)> = HashSet::new();
    for edge in &graph.edges {
        let from = reps.of(&edge.from).to_string();
        let to = reps.of(&edge.to).to_string();
        if from == to {
            continue;
        }
        if seen.insert((from.clone(), to.clone(), edge.kind)) {
            out.add_edge(from, to, edge.kind);
        }
    }
    out
}

/// A startup expansion set: the largest breadth-first prefix of the reachable
/// multi-child containers whose collapsed view fits both `max_nodes` and
/// `max_edges`. Singletons auto-expand in `collapse` and leaves can't expand, so
/// only multi-child containers are candidates; their direct children's nearest
/// multi-child descendants become reachable as each is expanded. The result is a
/// bounded but non-trivial overview.
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

    // BFS over reachable multi-child containers, shallowest-first. Expanding one
    // reveals its direct children, each contributing its nearest multi-child
    // descendant. Every container's nearest multi-child ancestor precedes it, so
    // the order is ancestor-closed and its prefixes nest.
    let mut order: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut queue: VecDeque<&str> = roots.iter().filter_map(|r| descend(&children, r)).collect();
    while let Some(c) = queue.pop_front() {
        if !seen.insert(c) {
            continue;
        }
        order.push(c);
        for &k in &children[c] {
            if let Some(d) = descend(&children, k) {
                queue.push_back(d);
            }
        }
    }

    // Both counts rise monotonically with the prefix length (more expansion only
    // reveals nodes and de-aggregates edges), so binary-search the largest prefix
    // whose collapsed view fits both caps.
    let fits = |k: usize| {
        let prefix: HashSet<String> = order[..k].iter().map(|s| s.to_string()).collect();
        let view = collapse(graph, &prefix);
        view.nodes.len() <= max_nodes && view.edges.len() <= max_edges
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

/// Memoized representative lookup over ids borrowed from the graph.
struct Representatives<'a> {
    parent: HashMap<&'a str, &'a str>,
    child_counts: HashMap<&'a str, usize>,
    expanded: &'a HashSet<String>,
    memo: HashMap<&'a str, &'a str>,
}

impl<'a> Representatives<'a> {
    fn new(graph: &'a Graph, expanded: &'a HashSet<String>) -> Self {
        Self {
            parent: parent_map(graph),
            child_counts: child_counts(graph),
            expanded,
            memo: HashMap::new(),
        }
    }

    /// A container is effectively expanded when listed in `expanded` or a
    /// singleton; its descendants are then shown rather than folded into it.
    fn is_expanded(&self, id: &str) -> bool {
        self.expanded.contains(id) || self.child_counts.get(id) == Some(&1)
    }

    /// The shallowest node on `id`'s root->id path that is not effectively
    /// expanded, falling back to `id` when the whole chain is.
    fn of(&mut self, id: &'a str) -> &'a str {
        if let Some(&rep) = self.memo.get(id) {
            return rep;
        }
        // Ancestor chain root->id.
        let mut chain = vec![id];
        let mut cur = id;
        while let Some(&p) = self.parent.get(cur) {
            chain.push(p);
            cur = p;
        }
        let rep = chain
            .iter()
            .rev()
            .copied()
            .find(|n| !self.is_expanded(n))
            .unwrap_or(id);
        self.memo.insert(id, rep);
        rep
    }
}

fn parent_map(graph: &Graph) -> HashMap<&str, &str> {
    graph
        .nodes
        .iter()
        .filter_map(|n| n.parent.as_deref().map(|p| (n.id.as_str(), p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
            g.add_node(root, root, "package", None);
            for kid in ["x", "y"] {
                let child = format!("{root}.{kid}");
                g.add_node(child, "leaf", "action", Some(root.to_string()));
            }
        }
        g
    }

    /// A single-child chain `A -> b -> c -> leaf`: every node but the leaf is a
    /// singleton container.
    fn singleton_chain() -> Graph {
        let mut g = Graph::new();
        g.add_node("A", "A", "package", None);
        g.add_node("b", "b", "target", Some("A".to_string()));
        g.add_node("c", "c", "target", Some("b".to_string()));
        g.add_node("leaf", "leaf", "action", Some("c".to_string()));
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
        g.add_node("b.extra", "leaf", "action", Some("b".to_string()));

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
        assert_eq!(find(&out, "A").kind, "package");
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
        g.add_node("R", "R", "package", None);
        g.add_node("x", "leaf", "action", Some("R".to_string()));
        g.add_node("y", "leaf", "action", Some("R".to_string()));

        // R is multi-child, so it is the only expand handle and a fold boundary.
        assert_eq!(expandable_containers(&g), expanded(&["R"]));
        let out = collapse(&g, &HashSet::new());
        assert_eq!(node_ids(&out), set(&["R"]));
    }

    #[test]
    fn multi_child_root_over_singleton_chains() {
        let mut g = Graph::new();
        g.add_node("R", "R", "package", None);
        // Each of R's two children is the head of a singleton chain to a leaf.
        for c in ["p", "q"] {
            g.add_node(c, c, "target", Some("R".to_string()));
            let mid = format!("{c}.mid");
            let leaf = format!("{c}.leaf");
            g.add_node(mid.clone(), "mid", "target", Some(c.to_string()));
            g.add_node(leaf, "leaf", "action", Some(mid));
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
        g.add_node("R", "R", "package", None);
        for c in ["c1", "c2"] {
            g.add_node(c, c, "target", Some("R".to_string()));
            // Two leaves keep each child a multi-child fold boundary.
            for leaf in ["a", "b"] {
                let id = format!("{c}.{leaf}");
                g.add_node(id, "leaf", "action", Some(c.to_string()));
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
        g.add_node("A", "A", "package", None);
        g.add_node("b", "b", "target", Some("A".to_string()));
        for leaf in ["x", "y"] {
            g.add_node(leaf, "leaf", "action", Some("b".to_string()));
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
