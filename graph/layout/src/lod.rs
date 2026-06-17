//! Folds a containment forest to a bounded level-of-detail view.
//!
//! Only the descendants of *expanded* containers are shown; every other subtree
//! folds into its shallowest collapsed ancestor, which then renders as a leaf.
//! Running this before quotient and layout shrinks a graph of tens of thousands
//! of nodes to a view whose size the caller caps.

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

/// Collapses `graph` to the view in which only the descendants of `expanded`
/// containers are shown. Every node folds to its "representative": the
/// shallowest node on its root->node path that is not in `expanded` (or the node
/// itself when its whole ancestor chain is expanded). Each distinct
/// representative becomes one node (keeping that node's label/kind/parent), so a
/// collapsed container renders as a leaf. Edges are remapped to their endpoints'
/// representatives, self-loops (both endpoints in the same collapsed subtree)
/// dropped, and duplicates merged keeping their `EdgeKind`.
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

/// A startup expansion set: opens containers breadth-first from the roots,
/// taking each whose direct children still fit `max_nodes`, so the visible node
/// budget is filled rather than stopping at the first level that overflows.
/// Then it drops the deepest expansions until the collapsed view also fits
/// `max_edges`. The result is a bounded but non-trivial overview.
pub fn default_expansion(graph: &Graph, max_nodes: usize, max_edges: usize) -> HashSet<String> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    let mut roots: Vec<&str> = Vec::new();
    for node in &graph.nodes {
        match node.parent.as_deref() {
            Some(parent) => children.entry(parent).or_default().push(&node.id),
            None => roots.push(&node.id),
        }
    }

    // Greedy breadth-first fill: a root is always shown, and expanding a
    // container reveals its direct children, so accept one while its children
    // fit the node budget. `order` records the expansions shallowest-first.
    let mut expanded: HashSet<String> = HashSet::new();
    let mut order: Vec<&str> = Vec::new();
    let mut shown = roots.len();
    let mut queue: VecDeque<&str> =
        roots.iter().copied().filter(|r| children.contains_key(r)).collect();
    while let Some(c) = queue.pop_front() {
        let kids = &children[c];
        if shown + kids.len() > max_nodes {
            continue;
        }
        expanded.insert(c.to_string());
        order.push(c);
        shown += kids.len();
        for &k in kids {
            if children.contains_key(k) {
                queue.push_back(k);
            }
        }
    }

    // Expanding a container only de-aggregates edges, so the collapsed edge count
    // rises monotonically with the shallowest-first prefix of `order`. Keep the
    // largest prefix whose view fits `max_edges`.
    if collapse(graph, &expanded).edges.len() > max_edges {
        let fits = |k: usize| {
            let prefix: HashSet<String> = order[..k].iter().map(|s| s.to_string()).collect();
            collapse(graph, &prefix).edges.len() <= max_edges
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
        expanded = order[..lo].iter().map(|s| s.to_string()).collect();
    }
    expanded
}

/// Memoized representative lookup over ids borrowed from the graph.
struct Representatives<'a> {
    parent: HashMap<&'a str, &'a str>,
    expanded: &'a HashSet<String>,
    memo: HashMap<&'a str, &'a str>,
}

impl<'a> Representatives<'a> {
    fn new(graph: &'a Graph, expanded: &'a HashSet<String>) -> Self {
        Self {
            parent: parent_map(graph),
            expanded,
            memo: HashMap::new(),
        }
    }

    /// The shallowest node on `id`'s root->id path not in `expanded`, falling
    /// back to `id` when the whole chain is expanded.
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
            .find(|n| !self.expanded.contains(*n))
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

    /// root -> child -> leaf under each of two roots A and B.
    fn two_towers() -> Graph {
        let mut g = Graph::new();
        for root in ["A", "B"] {
            g.add_node(root, root, "package", None);
            let child = format!("{root}.child");
            let leaf = format!("{root}.leaf");
            g.add_node(child.clone(), &child, "target", Some(root.to_string()));
            g.add_node(leaf, "leaf", "action", Some(child));
        }
        g
    }

    #[test]
    fn containers_are_the_parent_referenced_ids() {
        let g = two_towers();
        assert_eq!(containers(&g), expanded(&["A", "A.child", "B", "B.child"]));
    }

    #[test]
    fn empty_expanded_folds_each_tower_to_its_root() {
        let mut g = two_towers();
        // Cross-tower edge between the deepest leaves.
        g.add_edge("A.leaf", "B.leaf", EdgeKind::Aquery);

        let out = collapse(&g, &HashSet::new());

        assert_eq!(node_ids(&out), set(&["A", "B"]));
        assert_eq!(edges(&out), [("A", "B")].into_iter().collect());
        // The folded root keeps its own kind and renders as a leaf (below).
        assert_eq!(find(&out, "A").kind, "package");
    }

    #[test]
    fn expanding_a_root_reveals_its_children_only() {
        let mut g = two_towers();
        // Internal to A's still-collapsed child: a self-loop once folded.
        g.add_edge("A.leaf", "A.child", EdgeKind::Aquery);
        // Crosses from A's subtree into B's still-folded tower.
        g.add_edge("A.leaf", "B.leaf", EdgeKind::Aquery);

        let out = collapse(&g, &expanded(&["A"]));

        // The expanded root and its direct child are shown (the child's parent
        // chain is fully expanded, so the child is its own representative); the
        // grandchild folds into the child. B stays folded to its root.
        assert_eq!(node_ids(&out), set(&["A", "A.child", "B"]));
        // The internal A.leaf -> A.child edge is a self-loop (both fold to
        // A.child) and dropped; the cross-tower edge remaps to A.child -> B.
        assert_eq!(edges(&out), [("A.child", "B")].into_iter().collect());
    }

    #[test]
    fn cross_child_edge_under_one_expanded_root() {
        let mut g = Graph::new();
        g.add_node("R", "R", "package", None);
        for c in ["c1", "c2"] {
            g.add_node(c, c, "target", Some("R".to_string()));
            let leaf = format!("{c}.leaf");
            g.add_node(leaf, "leaf", "action", Some(c.to_string()));
        }
        // Internal to c1 (self-loop), and cross-child c1 -> c2.
        g.add_edge("c1.leaf", "c1", EdgeKind::Aquery);
        g.add_edge("c1.leaf", "c2.leaf", EdgeKind::Aquery);

        let out = collapse(&g, &expanded(&["R"]));

        // The expanded root and its two children are shown; grandchildren fold
        // into their child.
        assert_eq!(node_ids(&out), set(&["R", "c1", "c2"]));
        // c1.leaf -> c1 is a self-loop (both fold to c1) and dropped; the
        // cross-child c1.leaf -> c2.leaf remaps to c1 -> c2.
        assert_eq!(edges(&out), [("c1", "c2")].into_iter().collect());
    }

    #[test]
    fn duplicate_edges_dedup_distinct_kinds_kept() {
        let mut g = two_towers();
        // Two A-subtree -> B-subtree edges of the same kind fold to one A -> B.
        g.add_edge("A.leaf", "B.leaf", EdgeKind::Aquery);
        g.add_edge("A.child", "B.child", EdgeKind::Aquery);
        // A different kind between the same folded pair survives separately.
        g.add_edge("A.leaf", "B.child", EdgeKind::Query);

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
        g.add_edge("A.leaf", "B.leaf", EdgeKind::Aquery);

        // Caps that only the root view (2 nodes, 1 edge) fits: no level expands.
        let tiny = default_expansion(&g, 2, 1);
        assert!(tiny.is_empty());
        let v = collapse(&g, &tiny);
        assert!(v.nodes.len() <= 2 && v.edges.len() <= 1);

        // Generous caps expand every level: the full graph is shown.
        let all = default_expansion(&g, 100, 100);
        let v = collapse(&g, &all);
        assert_eq!(v.nodes.len(), g.nodes.len());
        assert!(v.nodes.len() <= 100 && v.edges.len() <= 100);
        assert_eq!(all, containers(&g));
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
