//! Losslessly merges leaf data nodes that are topologically interchangeable.
//!
//! Two leaf nodes of kind `source` or `artifact` are interchangeable when they
//! share the same kind, the same parent, the same set of producers (in-edges),
//! and the same set of consumers (out-edges). Such nodes have identical neighbor
//! sets, so collapsing them into one group node preserves all reachability and
//! edge routing. This folds the dominant Bazel pattern in which a target's whole
//! source list feeds a single compile action: each source's only consumer is
//! that one action, so they all merge into one node with one edge.
//!
//! Leaf data nodes never neighbor each other (consumption flows
//! source/artifact -> action and action -> artifact), so a candidate's
//! neighbors are never themselves candidates. The merge is therefore exact in a
//! single pass over the original graph: remapping candidate ids cannot perturb
//! the signatures it groups by.

use std::collections::{BTreeMap, HashSet};

use crate::graph::{Edge, EdgeKind, Graph, Node};

/// Returns a graph in which interchangeable `source`/`artifact` leaves are
/// merged. A multi-member group whose id is in `expanded` is emitted as a
/// `group` container holding its members unchanged (reparented to the group)
/// rather than collapsed into one leaf, so the caller can reveal the files
/// behind a group while leaving every other group collapsed. Nodes and edges of
/// the result are sorted by id for an output that is independent of the input
/// order; passing an empty `expanded` set yields the fully collapsed result.
pub fn quotient(graph: &Graph, expanded: &HashSet<String>) -> Graph {
    let producers = neighbor_lists(graph, |e| (&e.to, &e.from));
    let consumers = neighbor_lists(graph, |e| (&e.from, &e.to));

    // Nodes that hold children: only leaf data nodes merge, so a container of
    // kind `artifact` (a labeled generated output holding its configured
    // targets) is never collapsed into a group.
    let containers: HashSet<&str> = graph.nodes.iter().filter_map(|n| n.parent.as_deref()).collect();
    let mergeable = |node: &Node| is_candidate(&node.kind) && !containers.contains(node.id.as_str());

    // Group candidate ids by signature; non-candidates are passed through.
    let mut groups: BTreeMap<Signature, Vec<&str>> = BTreeMap::new();
    for node in &graph.nodes {
        if !mergeable(node) {
            continue;
        }
        let signature = Signature {
            kind: node.kind.clone(),
            parent: node.parent.clone(),
            producers: sorted_neighbors(&producers, &node.id),
            consumers: sorted_neighbors(&consumers, &node.id),
        };
        groups.entry(signature).or_default().push(&node.id);
    }

    // Collapsed multi-member groups remap their members to the group id; an
    // expanded multi-member group instead becomes a container holding its
    // members, which keep their own ids.
    let mut remap: BTreeMap<&str, String> = BTreeMap::new();
    // Group id -> member count, for labeling, recorded only for multi-member
    // groups (both collapsed and expanded).
    let mut group_sizes: BTreeMap<String, usize> = BTreeMap::new();
    // Member id -> container group id, for members of expanded groups only.
    let mut member_container: BTreeMap<&str, String> = BTreeMap::new();
    for members in groups.values() {
        let target = group_id(members);
        if members.len() < 2 {
            remap.insert(members[0], target);
            continue;
        }
        group_sizes.insert(target.clone(), members.len());
        if expanded.contains(&target) {
            for &member in members {
                member_container.insert(member, target.clone());
            }
            continue;
        }
        for &member in members {
            remap.insert(member, target.clone());
        }
    }

    let mut out = Graph::new();

    // Non-candidates unchanged; collapsed groups emit one leaf; expanded groups
    // emit a container followed by their members reparented to it.
    let mut emitted: BTreeMap<String, ()> = BTreeMap::new();
    for node in &graph.nodes {
        if !mergeable(node) {
            out.nodes.push(clone_node(node));
            continue;
        }
        // A member of an expanded group: emit the container once, then the
        // member with its id intact and parent set to the group.
        if let Some(gid) = member_container.get(node.id.as_str()).cloned() {
            if emitted.insert(gid.clone(), ()).is_none() {
                out.nodes.push(Node {
                    id: gid.clone(),
                    label: group_label(&node.kind, group_sizes[&gid]),
                    kind: "group".to_string(),
                    parent: node.parent.clone(),
                });
            }
            out.nodes.push(Node {
                id: node.id.clone(),
                label: node.label.clone(),
                kind: node.kind.clone(),
                parent: Some(gid),
            });
            continue;
        }
        let id = &remap[node.id.as_str()];
        if emitted.insert(id.clone(), ()).is_some() {
            continue;
        }
        let label = match group_sizes.get(id) {
            Some(&count) => group_label(&node.kind, count),
            None => node.label.clone(),
        };
        out.nodes.push(Node {
            id: id.clone(),
            label,
            kind: node.kind.clone(),
            parent: node.parent.clone(),
        });
    }

    // Remap edge endpoints (collapsed-group members only; expanded-group
    // members and singletons keep their ids), drop self-loops, and dedup.
    let mut seen_edge: BTreeMap<(String, String, EdgeKind), ()> = BTreeMap::new();
    for edge in &graph.edges {
        let from = remap
            .get(edge.from.as_str())
            .cloned()
            .unwrap_or_else(|| edge.from.clone());
        let to = remap
            .get(edge.to.as_str())
            .cloned()
            .unwrap_or_else(|| edge.to.clone());
        if from == to {
            continue;
        }
        if seen_edge
            .insert((from.clone(), to.clone(), edge.kind), ())
            .is_some()
        {
            continue;
        }
        out.edges.push(Edge {
            from,
            to,
            kind: edge.kind,
        });
    }

    out.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    out.edges
        .sort_by(|a, b| (&a.from, &a.to, a.kind).cmp(&(&b.from, &b.to, b.kind)));
    out
}

/// The neighborhood key two candidates must share to merge.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Signature {
    kind: String,
    parent: Option<String>,
    producers: Vec<String>,
    consumers: Vec<String>,
}

fn is_candidate(kind: &str) -> bool {
    kind == "source" || kind == "artifact"
}

/// Group node id: a stable prefix over the lexicographically smallest member id.
/// Each original id belongs to exactly one group, so this is unique. A singleton
/// keeps its own id.
fn group_id(members: &[&str]) -> String {
    let smallest = members.iter().min().copied().unwrap_or_default();
    if members.len() >= 2 {
        format!("group:{smallest}")
    } else {
        smallest.to_string()
    }
}

/// Group node label: a count of the merged leaves.
fn group_label(kind: &str, count: usize) -> String {
    match kind {
        "source" => format!("{count} sources"),
        _ => format!("{count} files"),
    }
}

/// For each node id, the sorted list of distinct neighbor ids selected by
/// `pick`, which maps an edge to its (anchor, neighbor) pair.
fn neighbor_lists<'a>(
    graph: &'a Graph,
    pick: impl Fn(&'a Edge) -> (&'a String, &'a String),
) -> BTreeMap<&'a str, Vec<&'a str>> {
    let mut map: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &graph.edges {
        let (anchor, neighbor) = pick(edge);
        map.entry(anchor.as_str())
            .or_default()
            .push(neighbor.as_str());
    }
    map
}

fn sorted_neighbors(lists: &BTreeMap<&str, Vec<&str>>, id: &str) -> Vec<String> {
    let mut ids: Vec<String> = lists
        .get(id)
        .map(|v| v.iter().map(|s| s.to_string()).collect())
        .unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn clone_node(node: &Node) -> Node {
    Node {
        id: node.id.clone(),
        label: node.label.clone(),
        kind: node.kind.clone(),
        parent: node.parent.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn leaf(graph: &mut Graph, id: &str, kind: &str, parent: Option<&str>) {
        graph.add_node(id, id, kind, parent.map(str::to_string));
    }

    fn node_ids(graph: &Graph) -> Vec<&str> {
        graph.nodes.iter().map(|n| n.id.as_str()).collect()
    }

    fn edges(graph: &Graph) -> Vec<(&str, &str)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
    }

    fn find<'a>(graph: &'a Graph, id: &str) -> &'a Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap()
    }

    /// Collapse every group (the empty-`expanded` default).
    fn collapsed(graph: &Graph) -> Graph {
        quotient(graph, &HashSet::new())
    }

    fn expanded(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn sources_with_one_shared_consumer_merge() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "action0", EdgeKind::Aquery);
        }

        let out = collapsed(&graph);

        // action0 plus the single group node.
        assert_eq!(out.nodes.len(), 2);
        let group = find(&out, "group:source1");
        assert_eq!(group.label, "3 sources");
        assert_eq!(group.kind, "source");
        assert_eq!(edges(&out), vec![("group:source1", "action0")]);
    }

    #[test]
    fn sources_with_different_consumers_do_not_merge() {
        let mut graph = Graph::new();
        graph.add_node("action0", "A", "action", None);
        graph.add_node("action1", "B", "action", None);
        leaf(&mut graph, "source1", "source", None);
        leaf(&mut graph, "source2", "source", None);
        graph.add_edge("source1", "action0", EdgeKind::Aquery);
        graph.add_edge("source2", "action1", EdgeKind::Aquery);

        let out = collapsed(&graph);

        assert!(node_ids(&out).contains(&"source1"));
        assert!(node_ids(&out).contains(&"source2"));
        assert!(!node_ids(&out).iter().any(|id| id.starts_with("group:")));
    }

    #[test]
    fn artifacts_sharing_producer_and_consumers_merge() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Produce", "action", None);
        graph.add_node("action1", "Link", "action", None);
        leaf(&mut graph, "artifact1", "artifact", Some("ct"));
        leaf(&mut graph, "artifact2", "artifact", Some("ct"));
        graph.add_node("ct", "cfg", "configured-target", None);
        for id in ["artifact1", "artifact2"] {
            graph.add_edge("action0", id, EdgeKind::Aquery);
            graph.add_edge(id, "action1", EdgeKind::Aquery);
        }

        let out = collapsed(&graph);

        let group = find(&out, "group:artifact1");
        assert_eq!(group.label, "2 files");
        assert_eq!(group.parent.as_deref(), Some("ct"));
        // Producer edge dedups to one action -> group.
        assert_eq!(
            edges(&out),
            vec![
                ("action0", "group:artifact1"),
                ("group:artifact1", "action1"),
            ]
        );
    }

    #[test]
    fn artifacts_with_different_producers_do_not_merge() {
        let mut graph = Graph::new();
        graph.add_node("action0", "P0", "action", None);
        graph.add_node("action1", "P1", "action", None);
        graph.add_node("action2", "Consume", "action", None);
        leaf(&mut graph, "artifact1", "artifact", None);
        leaf(&mut graph, "artifact2", "artifact", None);
        graph.add_edge("action0", "artifact1", EdgeKind::Aquery);
        graph.add_edge("action1", "artifact2", EdgeKind::Aquery);
        graph.add_edge("artifact1", "action2", EdgeKind::Aquery);
        graph.add_edge("artifact2", "action2", EdgeKind::Aquery);

        let out = collapsed(&graph);

        assert!(node_ids(&out).contains(&"artifact1"));
        assert!(node_ids(&out).contains(&"artifact2"));
        assert!(!node_ids(&out).iter().any(|id| id.starts_with("group:")));
    }

    #[test]
    fn artifact_container_is_not_merged() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Produce", "action", None);
        // Two artifact leaves with identical neighbors would normally merge,
        // but artifact2 is a container (a promoted labeled output holds a
        // configured-target child), which blocks the merge.
        leaf(&mut graph, "artifact1", "artifact", None);
        leaf(&mut graph, "artifact2", "artifact", None);
        graph.add_edge("artifact1", "action0", EdgeKind::Aquery);
        graph.add_edge("artifact2", "action0", EdgeKind::Aquery);
        graph.add_node("ct1", "cfg", "configured-target", Some("artifact2".to_string()));

        let out = collapsed(&graph);

        assert!(!node_ids(&out).iter().any(|id| id.starts_with("group:")));
        for id in ["artifact1", "artifact2", "ct1"] {
            assert!(node_ids(&out).contains(&id), "{id} missing");
        }
    }

    #[test]
    fn source_and_artifact_never_merge() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Consume", "action", None);
        leaf(&mut graph, "source1", "source", None);
        leaf(&mut graph, "artifact1", "artifact", None);
        graph.add_edge("source1", "action0", EdgeKind::Aquery);
        graph.add_edge("artifact1", "action0", EdgeKind::Aquery);

        let out = collapsed(&graph);

        assert!(node_ids(&out).contains(&"source1"));
        assert!(node_ids(&out).contains(&"artifact1"));
        assert!(!node_ids(&out).iter().any(|id| id.starts_with("group:")));
    }

    #[test]
    fn output_is_independent_of_insertion_order() {
        let mut a = Graph::new();
        a.add_node("action0", "Compile", "action", None);
        for id in ["source1", "source2", "source3"] {
            leaf(&mut a, id, "source", None);
            a.add_edge(id, "action0", EdgeKind::Aquery);
        }

        let mut b = Graph::new();
        for id in ["source3", "source1", "source2"] {
            leaf(&mut b, id, "source", None);
        }
        b.add_node("action0", "Compile", "action", None);
        b.add_edge("source2", "action0", EdgeKind::Aquery);
        b.add_edge("source3", "action0", EdgeKind::Aquery);
        b.add_edge("source1", "action0", EdgeKind::Aquery);

        let qa = collapsed(&a);
        let qb = collapsed(&b);

        assert_eq!(node_ids(&qa), node_ids(&qb));
        assert_eq!(edges(&qa), edges(&qb));
    }

    #[test]
    fn containers_and_actions_pass_through() {
        let mut graph = Graph::new();
        graph.add_node("target1", "//:t", "target", None);
        graph.add_node(
            "ct",
            "cfg",
            "configured-target",
            Some("target1".to_string()),
        );
        graph.add_node("action0", "Compile", "action", Some("ct".to_string()));
        leaf(&mut graph, "source1", "source", None);
        graph.add_edge("source1", "action0", EdgeKind::Aquery);

        let out = collapsed(&graph);

        for id in ["target1", "ct", "action0", "source1"] {
            assert!(node_ids(&out).contains(&id), "{id} missing");
        }
        assert_eq!(edges(&out), vec![("source1", "action0")]);
    }

    #[test]
    fn expanded_group_becomes_container_holding_its_members() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "action0", EdgeKind::Aquery);
        }

        let out = quotient(&graph, &expanded(&["group:source1"]));

        // The group is a container; the collapsed leaf is gone.
        let group = find(&out, "group:source1");
        assert_eq!(group.kind, "group");
        assert_eq!(group.label, "3 sources");
        assert_eq!(group.parent, None);
        // Each member survives with its own id, reparented to the group.
        for id in ["source1", "source2", "source3"] {
            let member = find(&out, id);
            assert_eq!(member.kind, "source");
            assert_eq!(member.parent.as_deref(), Some("group:source1"));
        }
        // Member edges keep their original endpoints (no group: remap).
        assert_eq!(
            edges(&out),
            vec![
                ("source1", "action0"),
                ("source2", "action0"),
                ("source3", "action0"),
            ]
        );
    }

    #[test]
    fn expanded_set_naming_a_non_group_leaves_collapse_unchanged() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "action0", EdgeKind::Aquery);
        }

        // Expanding an id that is not a multi-member group collapses as usual.
        let out = quotient(&graph, &expanded(&["group:other"]));

        assert_eq!(out.nodes.len(), 2);
        assert_eq!(find(&out, "group:source1").kind, "source");
        assert_eq!(edges(&out), vec![("group:source1", "action0")]);
    }

    #[test]
    fn one_group_expands_while_another_collapses() {
        let mut graph = Graph::new();
        graph.add_node("action0", "A", "action", None);
        graph.add_node("action1", "B", "action", None);
        // Group on action0 (will be expanded).
        for id in ["source1", "source2"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "action0", EdgeKind::Aquery);
        }
        // Group on action1 (will stay collapsed).
        for id in ["source3", "source4"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "action1", EdgeKind::Aquery);
        }

        let out = quotient(&graph, &expanded(&["group:source1"]));

        // First group is a container with its members.
        assert_eq!(find(&out, "group:source1").kind, "group");
        assert_eq!(
            find(&out, "source1").parent.as_deref(),
            Some("group:source1")
        );
        assert_eq!(
            find(&out, "source2").parent.as_deref(),
            Some("group:source1")
        );
        // Second group collapses to a single source leaf.
        assert_eq!(find(&out, "group:source3").kind, "source");
        assert_eq!(find(&out, "group:source3").label, "2 sources");
        assert!(!node_ids(&out).contains(&"source3"));
        assert_eq!(
            edges(&out),
            vec![
                ("group:source3", "action1"),
                ("source1", "action0"),
                ("source2", "action0"),
            ]
        );
    }

    #[test]
    fn expanded_output_is_independent_of_insertion_order() {
        let mut a = Graph::new();
        a.add_node("action0", "Compile", "action", None);
        for id in ["source1", "source2", "source3"] {
            leaf(&mut a, id, "source", None);
            a.add_edge(id, "action0", EdgeKind::Aquery);
        }

        let mut b = Graph::new();
        for id in ["source3", "source1", "source2"] {
            leaf(&mut b, id, "source", None);
        }
        b.add_node("action0", "Compile", "action", None);
        b.add_edge("source2", "action0", EdgeKind::Aquery);
        b.add_edge("source3", "action0", EdgeKind::Aquery);
        b.add_edge("source1", "action0", EdgeKind::Aquery);

        let set = expanded(&["group:source1"]);
        let qa = quotient(&a, &set);
        let qb = quotient(&b, &set);

        assert_eq!(node_ids(&qa), node_ids(&qb));
        assert_eq!(edges(&qa), edges(&qb));
    }
}
