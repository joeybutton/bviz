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
//! source/artifact -> {depset, action} and action -> artifact), so a candidate's
//! neighbors are never themselves candidates. The merge is therefore exact in a
//! single pass over the original graph: remapping candidate ids cannot perturb
//! the signatures it groups by.

use std::collections::{BTreeMap, HashSet};

use crate::graph::{Edge, Graph, Node};

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

    // Group candidate ids by signature; non-candidates are passed through.
    let mut groups: BTreeMap<Signature, Vec<&str>> = BTreeMap::new();
    for node in &graph.nodes {
        if !is_candidate(&node.kind) {
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
        if !is_candidate(&node.kind) {
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
    let mut seen_edge: BTreeMap<(String, String), ()> = BTreeMap::new();
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
        if seen_edge.insert((from.clone(), to.clone()), ()).is_some() {
            continue;
        }
        out.edges.push(Edge { from, to });
    }

    out.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    out.edges
        .sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    out
}

/// Resolves depset junctions into the display graph: an expanded depset becomes a
/// container holding its members; a collapsed one stays a leaf, members hidden.
///
/// Every `member -> depset` edge is removed, because containment carries membership
/// once a depset is open and there is nothing to draw once it is closed. For an
/// EXPANDED depset (id in `expanded`) each direct member is re-parented to the
/// depset, so the depset gains children and renders as a container; the member
/// keeps its other edges (a generated artifact keeps its `producer -> artifact`
/// edge, which then routes into the container). For a COLLAPSED depset the member
/// edges are simply dropped, leaving the `"{n} files"` leaf on its
/// `depset -> consumer` edges. A member that is a direct member of two expanded
/// depsets is re-parented into the lexicographically smallest of them.
///
/// Pruning runs to a fixpoint over one rule: keep a node iff it has children OR an
/// incident edge OR its parent is an expanded depset. The last clause retains a
/// re-parented member whose only edge was to the depset (e.g. a source-group shown
/// inside the open container); without it such a node would look orphaned. A
/// collapsed depset's orphaned input keeps its package/target parent, not an
/// expanded-depset parent, so it is pruned, and any container thereby emptied is
/// pruned too. Nodes and edges are sorted by id for an output independent of input
/// order.
pub fn collapse_depsets(graph: &Graph, expanded: &HashSet<String>) -> Graph {
    let depsets: HashSet<&str> = graph
        .nodes
        .iter()
        .filter(|n| n.kind == "depset")
        .map(|n| n.id.as_str())
        .collect();

    // For each member, the expanded depsets it directly feeds (sorted), so the
    // tie-break for double membership is the lexicographically smallest.
    let mut member_targets: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for edge in &graph.edges {
        if expanded.contains(&edge.to) && depsets.contains(edge.to.as_str()) {
            member_targets
                .entry(edge.from.as_str())
                .or_default()
                .push(edge.to.as_str());
        }
    }
    let reparent: BTreeMap<&str, &str> = member_targets
        .into_iter()
        .map(|(member, mut targets)| {
            targets.sort_unstable();
            (member, targets[0])
        })
        .collect();

    // Re-parent members of expanded depsets; every other node keeps its parent.
    let mut nodes: Vec<Node> = graph
        .nodes
        .iter()
        .map(|n| {
            let mut node = clone_node(n);
            if let Some(&target) = reparent.get(n.id.as_str()) {
                node.parent = Some(target.to_string());
            }
            node
        })
        .collect();

    // Drop every member -> depset edge; containment now carries membership for
    // expanded depsets, and collapsed ones hide their members.
    let mut edges: Vec<Edge> = graph
        .edges
        .iter()
        .filter(|e| !depsets.contains(e.to.as_str()))
        .map(|e| Edge {
            from: e.from.clone(),
            to: e.to.clone(),
        })
        .collect();

    // Prune to a fixpoint: keep a node iff it has children, an incident edge, or
    // an expanded-depset parent. Dropping a node can empty its parent, so iterate.
    loop {
        let mut has_child: HashSet<&str> = HashSet::new();
        for node in &nodes {
            if let Some(parent) = node.parent.as_deref() {
                has_child.insert(parent);
            }
        }
        let mut incident: HashSet<&str> = HashSet::new();
        for edge in &edges {
            incident.insert(edge.from.as_str());
            incident.insert(edge.to.as_str());
        }
        let keep: HashSet<String> = nodes
            .iter()
            .filter(|n| {
                has_child.contains(n.id.as_str())
                    || incident.contains(n.id.as_str())
                    || n.parent
                        .as_deref()
                        .is_some_and(|p| expanded.contains(p) && depsets.contains(p))
            })
            .map(|n| n.id.clone())
            .collect();
        let before = nodes.len();
        nodes.retain(|n| keep.contains(&n.id));
        if nodes.len() == before {
            break;
        }
        edges.retain(|e| keep.contains(&e.from) && keep.contains(&e.to));
    }

    let mut out = Graph::new();
    out.nodes = nodes;
    out.edges = edges;
    out.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    out.edges
        .sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    out
}

/// The display graph: merge interchangeable leaves, then collapse depset
/// junctions. Quotient runs first so sources feeding one depset merge into a
/// single group that is then pruned as a unit when the depset is collapsed.
pub fn display(graph: &Graph, expanded: &HashSet<String>) -> Graph {
    collapse_depsets(&quotient(graph, expanded), expanded)
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

/// Group node label, matching the count-of-files phrasing used for depsets.
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
            graph.add_edge(id, "action0");
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
        graph.add_edge("source1", "action0");
        graph.add_edge("source2", "action1");

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
            graph.add_edge("action0", id);
            graph.add_edge(id, "action1");
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
        graph.add_edge("action0", "artifact1");
        graph.add_edge("action1", "artifact2");
        graph.add_edge("artifact1", "action2");
        graph.add_edge("artifact2", "action2");

        let out = collapsed(&graph);

        assert!(node_ids(&out).contains(&"artifact1"));
        assert!(node_ids(&out).contains(&"artifact2"));
        assert!(!node_ids(&out).iter().any(|id| id.starts_with("group:")));
    }

    #[test]
    fn source_and_artifact_never_merge() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Consume", "action", None);
        leaf(&mut graph, "source1", "source", None);
        leaf(&mut graph, "artifact1", "artifact", None);
        graph.add_edge("source1", "action0");
        graph.add_edge("artifact1", "action0");

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
            a.add_edge(id, "action0");
        }

        let mut b = Graph::new();
        for id in ["source3", "source1", "source2"] {
            leaf(&mut b, id, "source", None);
        }
        b.add_node("action0", "Compile", "action", None);
        b.add_edge("source2", "action0");
        b.add_edge("source3", "action0");
        b.add_edge("source1", "action0");

        let qa = collapsed(&a);
        let qb = collapsed(&b);

        assert_eq!(node_ids(&qa), node_ids(&qb));
        assert_eq!(edges(&qa), edges(&qb));
    }

    #[test]
    fn containers_actions_and_depsets_pass_through() {
        let mut graph = Graph::new();
        graph.add_node("target1", "//:t", "target", None);
        graph.add_node(
            "ct",
            "cfg",
            "configured-target",
            Some("target1".to_string()),
        );
        graph.add_node("action0", "Compile", "action", Some("ct".to_string()));
        graph.add_node("depset10", "2 files", "depset", None);
        graph.add_edge("depset10", "action0");

        let out = collapsed(&graph);

        for id in ["target1", "ct", "action0", "depset10"] {
            assert!(node_ids(&out).contains(&id), "{id} missing");
        }
        assert_eq!(edges(&out), vec![("depset10", "action0")]);
    }

    #[test]
    fn expanded_group_becomes_container_holding_its_members() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "action0");
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
            graph.add_edge(id, "action0");
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
            graph.add_edge(id, "action0");
        }
        // Group on action1 (will stay collapsed).
        for id in ["source3", "source4"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "action1");
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
            a.add_edge(id, "action0");
        }

        let mut b = Graph::new();
        for id in ["source3", "source1", "source2"] {
            leaf(&mut b, id, "source", None);
        }
        b.add_node("action0", "Compile", "action", None);
        b.add_edge("source2", "action0");
        b.add_edge("source3", "action0");
        b.add_edge("source1", "action0");

        let set = expanded(&["group:source1"]);
        let qa = quotient(&a, &set);
        let qb = quotient(&b, &set);

        assert_eq!(node_ids(&qa), node_ids(&qb));
        assert_eq!(edges(&qa), edges(&qb));
    }

    #[test]
    fn collapsed_depset_drops_member_edges_and_prunes_orphan_input() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("depset0", "3 files", "depset", None);
        leaf(&mut graph, "group:source1", "source", None);
        graph.add_edge("group:source1", "depset0");
        graph.add_edge("depset0", "action0");

        let out = collapse_depsets(&graph, &HashSet::new());

        // The member edge is gone, the orphaned input is pruned, the depset
        // survives on its consumer edge.
        assert!(!node_ids(&out).contains(&"group:source1"));
        assert!(node_ids(&out).contains(&"depset0"));
        assert_eq!(edges(&out), vec![("depset0", "action0")]);
    }

    #[test]
    fn expanded_depset_reparents_members_and_becomes_a_container() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("depset0", "3 files", "depset", None);
        leaf(&mut graph, "group:source1", "source", None);
        graph.add_edge("group:source1", "depset0");
        graph.add_edge("depset0", "action0");

        let out = collapse_depsets(&graph, &expanded(&["depset0"]));

        // The member is re-parented into the depset, which now has a child; the
        // member -> depset edge is replaced by containment.
        assert_eq!(
            find(&out, "group:source1").parent.as_deref(),
            Some("depset0")
        );
        assert!(out
            .nodes
            .iter()
            .any(|n| n.parent.as_deref() == Some("depset0")));
        assert_eq!(edges(&out), vec![("depset0", "action0")]);
    }

    #[test]
    fn expanded_depset_nests_generated_artifact_keeping_its_producer_edge() {
        let mut graph = Graph::new();
        graph.add_node("producer", "Gen", "action", None);
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("depset0", "1 files", "depset", None);
        leaf(&mut graph, "artifact1", "artifact", Some("producer"));
        graph.add_edge("producer", "artifact1");
        graph.add_edge("artifact1", "depset0");
        graph.add_edge("depset0", "action0");

        let out = collapse_depsets(&graph, &expanded(&["depset0"]));

        // The artifact is nested inside the depset yet keeps its producer edge,
        // which now routes into the container.
        assert_eq!(find(&out, "artifact1").parent.as_deref(), Some("depset0"));
        assert!(!edges(&out).contains(&("artifact1", "depset0")));
        assert!(edges(&out).contains(&("producer", "artifact1")));
        assert!(edges(&out).contains(&("depset0", "action0")));
    }

    #[test]
    fn expanded_depset_keeps_edgeless_source_member_via_its_parent() {
        let mut graph = Graph::new();
        graph.add_node("depset0", "1 files", "depset", None);
        graph.add_node("action0", "Compile", "action", None);
        // A source-group whose only edge is into the depset.
        leaf(&mut graph, "group:source1", "source", None);
        graph.add_edge("group:source1", "depset0");
        graph.add_edge("depset0", "action0");

        let out = collapse_depsets(&graph, &expanded(&["depset0"]));

        // The re-parented member has no remaining edge but survives because its
        // parent is the expanded depset.
        assert_eq!(
            find(&out, "group:source1").parent.as_deref(),
            Some("depset0")
        );
        assert_eq!(edges(&out), vec![("depset0", "action0")]);
    }

    #[test]
    fn expanded_nested_depset_is_reparented_into_its_outer_depset() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("outer", "2 files", "depset", None);
        graph.add_node("inner", "1 files", "depset", None);
        graph.add_edge("inner", "outer");
        graph.add_edge("outer", "action0");

        let out = collapse_depsets(&graph, &expanded(&["outer"]));

        // The inner depset becomes a child of the open outer depset, itself still
        // a collapsible leaf.
        assert_eq!(find(&out, "inner").parent.as_deref(), Some("outer"));
        assert_eq!(find(&out, "inner").kind, "depset");
        assert_eq!(edges(&out), vec![("outer", "action0")]);
    }

    #[test]
    fn member_of_two_expanded_depsets_goes_to_smallest_id() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("depset_b", "1 files", "depset", None);
        graph.add_node("depset_a", "1 files", "depset", None);
        leaf(&mut graph, "group:source1", "source", None);
        graph.add_edge("group:source1", "depset_b");
        graph.add_edge("group:source1", "depset_a");
        graph.add_edge("depset_a", "action0");
        graph.add_edge("depset_b", "action0");

        let out = collapse_depsets(&graph, &expanded(&["depset_a", "depset_b"]));

        // Re-parented into the lexicographically smallest depset; both member
        // edges are dropped.
        assert_eq!(
            find(&out, "group:source1").parent.as_deref(),
            Some("depset_a")
        );
        assert!(!edges(&out).contains(&("group:source1", "depset_a")));
        assert!(!edges(&out).contains(&("group:source1", "depset_b")));
    }

    #[test]
    fn collapsed_depset_keeps_generated_artifact_via_producer_edge() {
        let mut graph = Graph::new();
        graph.add_node("producer", "Gen", "action", None);
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("depset0", "1 files", "depset", None);
        leaf(&mut graph, "artifact1", "artifact", Some("producer"));
        graph.add_edge("producer", "artifact1");
        graph.add_edge("artifact1", "depset0");
        graph.add_edge("depset0", "action0");

        let out = collapse_depsets(&graph, &HashSet::new());

        // The member edge into the depset is dropped, but the artifact keeps its
        // producer edge and survives.
        assert!(node_ids(&out).contains(&"artifact1"));
        assert!(!edges(&out).contains(&("artifact1", "depset0")));
        assert!(edges(&out).contains(&("producer", "artifact1")));
        assert!(edges(&out).contains(&("depset0", "action0")));
    }

    #[test]
    fn pruning_removes_a_container_emptied_of_its_only_child() {
        let mut graph = Graph::new();
        graph.add_node("pkg", "//pkg", "package", None);
        graph.add_node("depset0", "1 files", "depset", None);
        graph.add_node("action0", "Compile", "action", None);
        // The package's only child is a source-group that feeds only the depset.
        leaf(&mut graph, "group:source1", "source", Some("pkg"));
        graph.add_edge("group:source1", "depset0");
        graph.add_edge("depset0", "action0");

        let out = collapse_depsets(&graph, &HashSet::new());

        // The source-group is pruned, leaving the package childless and edgeless,
        // so it is pruned in turn.
        assert!(!node_ids(&out).contains(&"group:source1"));
        assert!(!node_ids(&out).contains(&"pkg"));
        assert!(node_ids(&out).contains(&"depset0"));
    }

    #[test]
    fn pruning_removes_a_container_emptied_by_reparenting_into_an_expanded_depset() {
        let mut graph = Graph::new();
        graph.add_node("pkg", "//pkg", "package", None);
        graph.add_node("depset0", "1 files", "depset", None);
        graph.add_node("action0", "Compile", "action", None);
        // The package's only child is re-parented out into the expanded depset.
        leaf(&mut graph, "group:source1", "source", Some("pkg"));
        graph.add_edge("group:source1", "depset0");
        graph.add_edge("depset0", "action0");

        let out = collapse_depsets(&graph, &expanded(&["depset0"]));

        // The member moves into the depset; the package is now childless and
        // edgeless and is pruned.
        assert_eq!(
            find(&out, "group:source1").parent.as_deref(),
            Some("depset0")
        );
        assert!(!node_ids(&out).contains(&"pkg"));
    }

    #[test]
    fn collapse_output_is_independent_of_insertion_order() {
        let mut a = Graph::new();
        a.add_node("action0", "Compile", "action", None);
        a.add_node("depset0", "2 files", "depset", None);
        leaf(&mut a, "source1", "source", None);
        leaf(&mut a, "source2", "source", None);
        a.add_edge("source1", "depset0");
        a.add_edge("source2", "depset0");
        a.add_edge("depset0", "action0");

        let mut b = Graph::new();
        b.add_edge("depset0", "action0");
        b.add_edge("source2", "depset0");
        b.add_edge("source1", "depset0");
        leaf(&mut b, "source2", "source", None);
        b.add_node("depset0", "2 files", "depset", None);
        leaf(&mut b, "source1", "source", None);
        b.add_node("action0", "Compile", "action", None);

        let ca = collapse_depsets(&a, &HashSet::new());
        let cb = collapse_depsets(&b, &HashSet::new());

        assert_eq!(node_ids(&ca), node_ids(&cb));
        assert_eq!(edges(&ca), edges(&cb));
    }

    #[test]
    fn display_merges_then_prunes_sources_feeding_a_collapsed_depset() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("depset0", "3 files", "depset", None);
        // Three interchangeable sources feed only the depset.
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "depset0");
        }
        graph.add_edge("depset0", "action0");

        let out = display(&graph, &HashSet::new());

        // Quotient merges the three sources into one group, which is then pruned
        // as a unit; the depset remains on its consumer edge.
        assert!(!node_ids(&out).iter().any(|id| id.starts_with("group:")));
        for id in ["source1", "source2", "source3"] {
            assert!(!node_ids(&out).contains(&id));
        }
        assert!(node_ids(&out).contains(&"depset0"));
        assert_eq!(edges(&out), vec![("depset0", "action0")]);
    }

    #[test]
    fn display_merges_then_reparents_sources_into_an_expanded_depset() {
        let mut graph = Graph::new();
        graph.add_node("action0", "Compile", "action", None);
        graph.add_node("depset0", "3 files", "depset", None);
        // Three interchangeable sources feed only the depset.
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, id, "source", None);
            graph.add_edge(id, "depset0");
        }
        graph.add_edge("depset0", "action0");

        let out = display(&graph, &expanded(&["depset0"]));

        // Quotient merges the three sources into one group, which is re-parented
        // into the expanded depset as a unit; the depset is now a container.
        assert_eq!(
            find(&out, "group:source1").parent.as_deref(),
            Some("depset0")
        );
        for id in ["source1", "source2", "source3"] {
            assert!(!node_ids(&out).contains(&id));
        }
        assert_eq!(edges(&out), vec![("depset0", "action0")]);
    }
}
