//! Restricts a graph to a chosen set of nodes.
//!
//! Every node outside the set is dropped, along with its containment position
//! and its edges. To keep the containment forest well-formed for the layout
//! engine (which only ever descends from a `parent == None` root, so a node
//! whose parent was dropped would never be reached), each surviving node
//! reparents to the nearest ancestor that also survives, or becomes a root
//! itself. To keep the dependency structure legible, a pair of surviving nodes
//! whose only original connection ran through dropped nodes gets a synthetic
//! [`EdgeKind::Transitive`] edge in place of the path that no longer exists.
//!
//! A survivor keeps the id it carried in the whole graph, so the restriction's
//! id space is the whole graph's with holes in it. That is what lets a filter
//! apply to one view without disturbing the rest: the expansion set, the
//! selection, the layout's pinning anchors and the name table all key on the
//! id, and all of them go on meaning what they meant.

use std::collections::{HashMap, HashSet};

use crate::graph::{EdgeKind, Graph};
use crate::intern::Id;

/// `graph` restricted to `keep`: every kept node, reparented to its nearest
/// surviving ancestor; every edge the original graph had between two kept
/// nodes; and one [`EdgeKind::Transitive`] edge for each kept pair the original
/// graph connected only via dropped nodes and that keeps no direct edge of its
/// own. An id in `keep` that names no node in `graph` is ignored.
pub fn filter(graph: &Graph, keep: &HashSet<Id>) -> Graph {
    let parent_of: HashMap<Id, Option<Id>> = graph.nodes.iter().map(|n| (n.id, n.parent)).collect();

    let mut out = Graph::new();
    for node in &graph.nodes {
        if !keep.contains(&node.id) {
            continue;
        }
        let mut parent = node.parent;
        while let Some(p) = parent {
            if keep.contains(&p) {
                break;
            }
            parent = parent_of.get(&p).copied().flatten();
        }
        out.add_node(node.id, node.label.clone(), node.kind, parent);
    }

    let mut direct: HashSet<(Id, Id)> = HashSet::new();
    for edge in &graph.edges {
        if keep.contains(&edge.from) && keep.contains(&edge.to) {
            direct.insert((edge.from, edge.to));
            out.add_edge(edge.from, edge.to, edge.kind);
        }
    }

    let successors = successors(graph);
    let frontier = kept_frontiers(graph, keep, &successors);
    let mut transitive: HashSet<(Id, Id)> = HashSet::new();
    for edge in &graph.edges {
        if !keep.contains(&edge.from) || keep.contains(&edge.to) {
            continue;
        }
        for &to in &frontier[&edge.to] {
            if edge.from != to && !direct.contains(&(edge.from, to)) {
                transitive.insert((edge.from, to));
            }
        }
    }
    for (from, to) in transitive {
        out.add_edge(from, to, EdgeKind::Transitive);
    }

    out
}

fn successors(graph: &Graph) -> HashMap<Id, Vec<Id>> {
    let mut successors: HashMap<Id, Vec<Id>> = HashMap::new();
    for edge in &graph.edges {
        successors.entry(edge.from).or_default().push(edge.to);
    }
    successors
}

/// For every dropped node, the kept nodes reachable from it by a path through
/// dropped nodes only. Two dropped nodes on a cycle can each reach whatever the
/// other reaches, so this is a fixed point, not a single downward pass: each
/// dropped node starts with its direct kept successors, then repeatedly absorbs
/// its dropped successors' frontiers until a pass adds nothing. A pass that
/// resolved cycles by depth-first order instead — finalizing each node the
/// first time it is visited — would under-count a node revisited through a
/// back edge, since more of the cycle's reach may still be pending when that
/// first visit finalizes it.
fn kept_frontiers(
    graph: &Graph,
    keep: &HashSet<Id>,
    successors: &HashMap<Id, Vec<Id>>,
) -> HashMap<Id, HashSet<Id>> {
    let dropped: Vec<Id> = graph
        .nodes
        .iter()
        .map(|n| n.id)
        .filter(|id| !keep.contains(id))
        .collect();
    let mut frontier: HashMap<Id, HashSet<Id>> = dropped
        .iter()
        .map(|&d| {
            let kept_succ = successors
                .get(&d)
                .into_iter()
                .flatten()
                .copied()
                .filter(|s| keep.contains(s))
                .collect();
            (d, kept_succ)
        })
        .collect();

    let mut changed = true;
    while changed {
        changed = false;
        for &d in &dropped {
            let added: Vec<Id> = successors
                .get(&d)
                .into_iter()
                .flatten()
                .filter(|s| !keep.contains(s))
                .flat_map(|s| frontier[s].iter().copied())
                .filter(|to| !frontier[&d].contains(to))
                .collect();
            if !added.is_empty() {
                frontier.get_mut(&d).unwrap().extend(added);
                changed = true;
            }
        }
    }
    frontier
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::graph::Kind;
    use crate::intern::Names;

    /// A graph built from `(from, to, EdgeKind::Query)` edges plus, for any node
    /// not otherwise named, a lone node with no parent.
    fn build(edges: &[(&str, &str)]) -> (Graph, Names) {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mut seen = HashSet::new();
        for &(a, b) in edges {
            for id in [a, b] {
                if seen.insert(id) {
                    g.add_node(n.intern(id), id, Kind::Action, None);
                }
            }
        }
        for &(a, b) in edges {
            g.add_edge(n.intern(a), n.intern(b), EdgeKind::Query);
        }
        (g, n)
    }

    fn ids(n: &Names, labels: &[&str]) -> HashSet<Id> {
        labels.iter().map(|l| n.get(l).unwrap()).collect()
    }

    fn edges(g: &Graph, n: &Names) -> HashSet<(String, String, EdgeKind)> {
        g.edges
            .iter()
            .map(|e| {
                (
                    n.resolve(e.from).to_string(),
                    n.resolve(e.to).to_string(),
                    e.kind,
                )
            })
            .collect()
    }

    #[test]
    fn a_chain_collapses_to_one_transitive_edge() {
        let (g, n) = build(&[("a", "b"), ("b", "c")]);
        let out = filter(&g, &ids(&n, &["a", "c"]));
        assert_eq!(out.nodes.len(), 2);
        assert_eq!(
            edges(&out, &n),
            [("a".to_string(), "c".to_string(), EdgeKind::Transitive)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn a_direct_edge_needs_no_transitive_duplicate() {
        let (g, n) = build(&[("a", "b"), ("a", "c")]);
        let out = filter(&g, &ids(&n, &["a", "c"]));
        assert_eq!(
            edges(&out, &n),
            [("a".to_string(), "c".to_string(), EdgeKind::Query)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn fan_out_through_a_dropped_hub_reaches_every_kept_successor() {
        let (g, n) = build(&[("a", "d"), ("d", "b"), ("d", "c")]);
        let out = filter(&g, &ids(&n, &["a", "b", "c"]));
        assert_eq!(
            edges(&out, &n),
            [
                ("a".to_string(), "b".to_string(), EdgeKind::Transitive),
                ("a".to_string(), "c".to_string(), EdgeKind::Transitive),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn fan_in_through_a_dropped_hub_reaches_every_kept_predecessor() {
        let (g, n) = build(&[("a", "d"), ("b", "d"), ("d", "c")]);
        let out = filter(&g, &ids(&n, &["a", "b", "c"]));
        assert_eq!(
            edges(&out, &n),
            [
                ("a".to_string(), "c".to_string(), EdgeKind::Transitive),
                ("b".to_string(), "c".to_string(), EdgeKind::Transitive),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn a_cycle_of_dropped_nodes_still_terminates() {
        // a -> d1 -> d2 -> d1 (cycle) and d2 -> b: b must still be reached.
        let mut g = Graph::new();
        let mut n = Names::default();
        for id in ["a", "d1", "d2", "b"] {
            g.add_node(n.intern(id), id, Kind::Action, None);
        }
        for (from, to) in [("a", "d1"), ("d1", "d2"), ("d2", "d1"), ("d2", "b")] {
            g.add_edge(n.intern(from), n.intern(to), EdgeKind::Query);
        }
        let out = filter(&g, &ids(&n, &["a", "b"]));
        assert_eq!(
            edges(&out, &n),
            [("a".to_string(), "b".to_string(), EdgeKind::Transitive)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn a_cycle_shares_reach_between_both_of_its_dropped_nodes() {
        // d1 <-> d2 is a cycle; d1 also reaches b and d2 also reaches c. Both a
        // (which only enters at d1) and x (which only enters at d2) must see
        // both b and c, since either dropped node can reach the other.
        let mut g = Graph::new();
        let mut n = Names::default();
        for id in ["a", "x", "d1", "d2", "b", "c"] {
            g.add_node(n.intern(id), id, Kind::Action, None);
        }
        for (from, to) in [
            ("a", "d1"),
            ("x", "d2"),
            ("d1", "d2"),
            ("d2", "d1"),
            ("d1", "b"),
            ("d2", "c"),
        ] {
            g.add_edge(n.intern(from), n.intern(to), EdgeKind::Query);
        }
        let out = filter(&g, &ids(&n, &["a", "x", "b", "c"]));
        assert_eq!(
            edges(&out, &n),
            [
                ("a".to_string(), "b".to_string(), EdgeKind::Transitive),
                ("a".to_string(), "c".to_string(), EdgeKind::Transitive),
                ("x".to_string(), "b".to_string(), EdgeKind::Transitive),
                ("x".to_string(), "c".to_string(), EdgeKind::Transitive),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn dropping_an_intermediate_container_reparents_to_the_nearest_kept_ancestor() {
        let mut g = Graph::new();
        let mut n = Names::default();
        let root = n.intern("root");
        g.add_node(root, "root", Kind::Package, None);
        let mid = n.intern("mid");
        g.add_node(mid, "mid", Kind::Package, Some(root));
        let leaf = n.intern("leaf");
        g.add_node(leaf, "leaf", Kind::Target, Some(mid));

        let out = filter(&g, &ids(&n, &["root", "leaf"]));
        let leaf_out = out.nodes.iter().find(|node| node.id == leaf).unwrap();
        assert_eq!(leaf_out.parent, Some(root));
    }

    #[test]
    fn dropping_every_ancestor_leaves_a_root() {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mid = n.intern("mid");
        g.add_node(mid, "mid", Kind::Package, None);
        let leaf = n.intern("leaf");
        g.add_node(leaf, "leaf", Kind::Target, Some(mid));

        let out = filter(&g, &ids(&n, &["leaf"]));
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].parent, None);
    }

    /// Two packages, each holding two targets, each holding two actions, with a
    /// dependency from each action to the next, so a restriction of it has both
    /// containment and edges to carry over. Each node's label is the name it is
    /// interned under, so resolving an id says which node it names.
    fn forest() -> (Graph, Names) {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mut actions: Vec<Id> = Vec::new();
        for p in 0..2 {
            let package = format!("//p{p}");
            g.add_node(n.intern(&package), &package, Kind::Package, None);
            for t in 0..2 {
                let target = format!("{package}:t{t}");
                let parent = n.get(&package);
                g.add_node(n.intern(&target), &target, Kind::Target, parent);
                for a in 0..2 {
                    let action = format!("{target}#a{a}");
                    let parent = n.get(&target);
                    g.add_node(n.intern(&action), &action, Kind::Action, parent);
                    actions.push(n.get(&action).unwrap());
                }
            }
        }
        for pair in actions.windows(2) {
            g.add_edge(pair[0], pair[1], EdgeKind::Aquery);
        }
        (g, n)
    }

    /// Every other id of [`forest`], which drops some containers whose children
    /// survive and carries the largest surviving id well past the survivors'
    /// count.
    fn every_other(graph: &Graph) -> HashSet<Id> {
        graph
            .nodes
            .iter()
            .map(|node| node.id)
            .filter(|id| id.0 % 2 == 0)
            .collect()
    }

    #[test]
    fn a_survivor_keeps_the_id_it_carried_in_the_whole_graph() {
        let (g, _) = forest();
        let keep = every_other(&g);

        let out = filter(&g, &keep);

        assert!(out.nodes.len() < g.nodes.len(), "nothing was dropped");
        assert_eq!(
            out.nodes
                .iter()
                .map(|node| node.id)
                .collect::<HashSet<Id>>(),
            keep,
        );
        assert!(
            out.nodes
                .iter()
                .any(|node| node.id.0 as usize >= out.nodes.len()),
            "every surviving id fits within the survivors' count",
        );
    }

    /// The name table is built over the whole graph and never rebuilt, so a
    /// restriction's ids have to go on resolving through it.
    #[test]
    fn a_survivor_resolves_through_the_whole_graph_name_table() {
        let (g, n) = forest();
        let out = filter(&g, &every_other(&g));
        for node in &out.nodes {
            assert_eq!(n.resolve(node.id), node.label);
        }
    }

    /// The passes downstream of a restriction index by id, so they have to take
    /// an id space with holes in it.
    #[test]
    fn a_restricted_graph_feeds_the_fold_the_reduction_and_the_map() {
        let (g, _) = forest();
        let keep = every_other(&g);
        let out = filter(&g, &keep);

        let folded = crate::lod::collapse(&out, &crate::lod::expandable_containers(&out));
        assert!(folded.nodes.iter().all(|node| keep.contains(&node.id)));

        let reduced = crate::reduce::transitive_reduction(&out);
        assert!(reduced.edges.len() <= out.edges.len());

        let map = crate::map::map_layout(Arc::new(out.clone()), Arc::new(reduced));
        let finest = map.level(map.level_count() - 1);
        assert_eq!(
            finest.boxes.iter().map(|b| b.id).collect::<HashSet<Id>>(),
            keep,
        );
    }

    #[test]
    fn an_id_absent_from_the_graph_is_ignored() {
        let (g, n) = build(&[("a", "b")]);
        let mut keep = ids(&n, &["a", "b"]);
        keep.insert(Id(9999));
        let out = filter(&g, &keep);
        assert_eq!(out.nodes.len(), 2);
    }
}
