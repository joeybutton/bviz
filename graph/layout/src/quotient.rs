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

use crate::graph::{Edge, EdgeKind, Graph, Kind, Node};
use crate::intern::{Id, Names};

/// Returns a graph in which interchangeable `source`/`artifact` leaves are
/// merged, together with the box each input node became: a collapsed group's
/// members map to the group id, everything else is absent (it kept its own id).
/// Projecting another edge set over the same nodes through this map lands it on
/// the same boxes, which a caller uses to draw a reduced edge subset.
///
/// A multi-member group whose id is in `expanded` is emitted as a `group`
/// container holding its members unchanged (reparented to the group) rather than
/// collapsed into one leaf, so the caller can reveal the files behind a group
/// while leaving every other group collapsed. Nodes and edges of the result are
/// sorted by id for an output that is independent of the input order; passing an
/// empty `expanded` set yields the fully collapsed result.
pub fn quotient(
    graph: &Graph,
    expanded: &HashSet<Id>,
    names: &mut Names,
) -> (Graph, BTreeMap<Id, Id>) {
    let producers = neighbor_lists(graph, |e| (e.to, e.from));
    let consumers = neighbor_lists(graph, |e| (e.from, e.to));

    // Nodes that hold children: only leaf data nodes merge, so a container of
    // kind `artifact` (a labeled generated output holding its configured
    // targets) is never collapsed into a group.
    let containers = crate::lod::containers(graph);
    let mergeable = |node: &Node| is_candidate(node.kind) && !containers.contains(&node.id);

    let groups = group_by_signature(graph, &mergeable, &producers, &consumers);
    let partition = partition_groups(&groups, expanded, names);

    let mut out = Graph::new();
    emit_nodes(&mut out, graph, &mergeable, &partition);
    emit_edges(&mut out, graph, &partition.remap);

    // Sort by resolved name, not interned id: the integer id depends on the
    // order names were interned, so name-keyed sorting is what makes the output
    // independent of the input order.
    out.nodes
        .sort_by(|a, b| names.resolve(a.id).cmp(names.resolve(b.id)));
    out.edges.sort_by(|a, b| {
        (names.resolve(a.from), names.resolve(a.to), a.kind).cmp(&(
            names.resolve(b.from),
            names.resolve(b.to),
            b.kind,
        ))
    });
    (out, partition.remap)
}

/// Groups mergeable candidate ids by signature; non-candidates are passed
/// through.
fn group_by_signature(
    graph: &Graph,
    mergeable: &impl Fn(&Node) -> bool,
    producers: &BTreeMap<Id, Vec<Id>>,
    consumers: &BTreeMap<Id, Vec<Id>>,
) -> BTreeMap<Signature, Vec<Id>> {
    let mut groups: BTreeMap<Signature, Vec<Id>> = BTreeMap::new();
    for node in &graph.nodes {
        if !mergeable(node) {
            continue;
        }
        let signature = Signature {
            kind: node.kind,
            parent: node.parent,
            producers: sorted_neighbors(producers, node.id),
            consumers: sorted_neighbors(consumers, node.id),
        };
        groups.entry(signature).or_default().push(node.id);
    }
    groups
}

/// How each signature group is realized in the output.
struct Partition {
    /// Member id -> group id for collapsed groups and singletons; expanded-group
    /// members and non-candidates are absent (they keep their own ids).
    remap: BTreeMap<Id, Id>,
    /// Group id -> member count, for labeling, recorded only for multi-member
    /// groups (both collapsed and expanded).
    group_sizes: BTreeMap<Id, usize>,
    /// Member id -> container group id, for members of expanded groups only.
    member_container: BTreeMap<Id, Id>,
}

/// Collapsed multi-member groups remap their members to the group id; an
/// expanded multi-member group instead becomes a container holding its members,
/// which keep their own ids.
fn partition_groups(
    groups: &BTreeMap<Signature, Vec<Id>>,
    expanded: &HashSet<Id>,
    names: &mut Names,
) -> Partition {
    let mut remap: BTreeMap<Id, Id> = BTreeMap::new();
    let mut group_sizes: BTreeMap<Id, usize> = BTreeMap::new();
    let mut member_container: BTreeMap<Id, Id> = BTreeMap::new();
    for members in groups.values() {
        let target = group_id(members, names);
        if members.len() < 2 {
            remap.insert(members[0], target);
            continue;
        }
        group_sizes.insert(target, members.len());
        if expanded.contains(&target) {
            for &member in members {
                member_container.insert(member, target);
            }
            continue;
        }
        for &member in members {
            remap.insert(member, target);
        }
    }
    Partition {
        remap,
        group_sizes,
        member_container,
    }
}

/// Emits the quotient's nodes from the [`Partition`]: kept nodes, group leaves,
/// and group containers with their reparented members.
fn emit_nodes(
    out: &mut Graph,
    graph: &Graph,
    mergeable: &impl Fn(&Node) -> bool,
    part: &Partition,
) {
    let mut emitted: BTreeMap<Id, ()> = BTreeMap::new();
    for node in &graph.nodes {
        if !mergeable(node) {
            out.nodes.push(clone_node(node));
            continue;
        }
        // A member of an expanded group: emit the container once, then the
        // member with its id intact and parent set to the group.
        if let Some(&gid) = part.member_container.get(&node.id) {
            if emitted.insert(gid, ()).is_none() {
                out.nodes.push(Node {
                    id: gid,
                    label: group_label(node.kind, part.group_sizes[&gid]),
                    kind: Kind::MergeGroup,
                    group: true,
                    parent: node.parent,
                });
            }
            out.nodes.push(Node {
                id: node.id,
                label: node.label.clone(),
                kind: node.kind,
                group: false,
                parent: Some(gid),
            });
            continue;
        }
        let id = part.remap[&node.id];
        if emitted.insert(id, ()).is_some() {
            continue;
        }
        let label = match part.group_sizes.get(&id) {
            Some(&count) => group_label(node.kind, count),
            None => node.label.clone(),
        };
        out.nodes.push(Node {
            id,
            label,
            kind: node.kind,
            group: part.group_sizes.contains_key(&id),
            parent: node.parent,
        });
    }
}

/// Remap edge endpoints (collapsed-group members only; expanded-group members
/// and singletons keep their ids), drop self-loops, and dedup.
fn emit_edges(out: &mut Graph, graph: &Graph, remap: &BTreeMap<Id, Id>) {
    let mut seen_edge: BTreeMap<(Id, Id, EdgeKind), ()> = BTreeMap::new();
    for edge in &graph.edges {
        let from = remap.get(&edge.from).copied().unwrap_or(edge.from);
        let to = remap.get(&edge.to).copied().unwrap_or(edge.to);
        if from == to {
            continue;
        }
        if seen_edge.insert((from, to, edge.kind), ()).is_some() {
            continue;
        }
        out.edges.push(Edge {
            from,
            to,
            kind: edge.kind,
        });
    }
}

/// The neighborhood key two candidates must share to merge.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Signature {
    kind: Kind,
    parent: Option<Id>,
    producers: Vec<Id>,
    consumers: Vec<Id>,
}

fn is_candidate(kind: Kind) -> bool {
    matches!(kind, Kind::Source | Kind::Artifact)
}

/// Group node id: a stable id minted over the member whose name sorts first.
/// Each original id belongs to exactly one group, so this is unique. Keying on
/// the name rather than the interned integer keeps the id independent of the
/// order members were interned in. A singleton keeps its own id.
fn group_id(members: &[Id], names: &mut Names) -> Id {
    let smallest = *members.iter().min_by_key(|&&id| names.resolve(id)).unwrap();
    if members.len() >= 2 {
        let s = format!("group:{}", names.resolve(smallest));
        names.intern(&s)
    } else {
        smallest
    }
}

/// Group node label: a count of the merged leaves.
fn group_label(kind: Kind, count: usize) -> String {
    match kind {
        Kind::Source => format!("{count} sources"),
        _ => format!("{count} files"),
    }
}

/// For each node id, the sorted list of distinct neighbor ids selected by
/// `pick`, which maps an edge to its (anchor, neighbor) pair.
fn neighbor_lists(graph: &Graph, pick: impl Fn(&Edge) -> (Id, Id)) -> BTreeMap<Id, Vec<Id>> {
    let mut map: BTreeMap<Id, Vec<Id>> = BTreeMap::new();
    for edge in &graph.edges {
        let (anchor, neighbor) = pick(edge);
        map.entry(anchor).or_default().push(neighbor);
    }
    map
}

fn sorted_neighbors(lists: &BTreeMap<Id, Vec<Id>>, id: Id) -> Vec<Id> {
    let mut ids: Vec<Id> = lists.get(&id).cloned().unwrap_or_default();
    ids.sort_unstable();
    ids.dedup();
    ids
}

fn clone_node(node: &Node) -> Node {
    Node {
        id: node.id,
        label: node.label.clone(),
        kind: node.kind,
        group: node.group,
        parent: node.parent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::find;

    fn leaf(graph: &mut Graph, names: &mut Names, id: &str, kind: Kind, parent: Option<&str>) {
        node(graph, names, id, id, kind, parent);
    }

    fn node(
        graph: &mut Graph,
        names: &mut Names,
        id: &str,
        label: &str,
        kind: Kind,
        parent: Option<&str>,
    ) {
        let parent = parent.map(|p| names.intern(p));
        let id = names.intern(id);
        graph.add_node(Node::new(id, label, kind, parent));
    }

    fn edge(graph: &mut Graph, names: &mut Names, from: &str, to: &str, kind: EdgeKind) {
        let from = names.intern(from);
        let to = names.intern(to);
        graph.add_edge(from, to, kind);
    }

    fn node_ids<'a>(graph: &Graph, names: &'a Names) -> Vec<&'a str> {
        graph.nodes.iter().map(|n| names.resolve(n.id)).collect()
    }

    fn edges<'a>(graph: &Graph, names: &'a Names) -> Vec<(&'a str, &'a str)> {
        graph
            .edges
            .iter()
            .map(|e| (names.resolve(e.from), names.resolve(e.to)))
            .collect()
    }

    /// Collapse every group (the empty-`expanded` default).
    fn collapsed(graph: &Graph, names: &mut Names) -> Graph {
        quotient(graph, &HashSet::new(), names).0
    }

    /// The interned ids of `ids`, interned into `names` so that a group id minted
    /// by `quotient` (e.g. `group:source1`) resolves to this same id.
    fn expanded(names: &mut Names, ids: &[&str]) -> HashSet<Id> {
        ids.iter().map(|s| names.intern(s)).collect()
    }

    #[test]
    fn sources_with_one_shared_consumer_merge() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(
            &mut graph,
            &mut names,
            "action0",
            "Compile",
            Kind::Action,
            None,
        );
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, &mut names, id, Kind::Source, None);
            edge(&mut graph, &mut names, id, "action0", EdgeKind::Aquery);
        }

        let out = collapsed(&graph, &mut names);

        // action0 plus the single group node.
        assert_eq!(out.nodes.len(), 2);
        let group = find(&out, &names, "group:source1");
        assert_eq!(group.label, "3 sources");
        assert_eq!(group.kind, Kind::Source);
        assert_eq!(edges(&out, &names), vec![("group:source1", "action0")]);
    }

    #[test]
    fn sources_with_different_consumers_do_not_merge() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(&mut graph, &mut names, "action0", "A", Kind::Action, None);
        node(&mut graph, &mut names, "action1", "B", Kind::Action, None);
        leaf(&mut graph, &mut names, "source1", Kind::Source, None);
        leaf(&mut graph, &mut names, "source2", Kind::Source, None);
        edge(
            &mut graph,
            &mut names,
            "source1",
            "action0",
            EdgeKind::Aquery,
        );
        edge(
            &mut graph,
            &mut names,
            "source2",
            "action1",
            EdgeKind::Aquery,
        );

        let out = collapsed(&graph, &mut names);

        assert!(node_ids(&out, &names).contains(&"source1"));
        assert!(node_ids(&out, &names).contains(&"source2"));
        assert!(
            !node_ids(&out, &names)
                .iter()
                .any(|id| id.starts_with("group:"))
        );
    }

    #[test]
    fn artifacts_sharing_producer_and_consumers_merge() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(
            &mut graph,
            &mut names,
            "action0",
            "Produce",
            Kind::Action,
            None,
        );
        node(
            &mut graph,
            &mut names,
            "action1",
            "Link",
            Kind::Action,
            None,
        );
        leaf(
            &mut graph,
            &mut names,
            "artifact1",
            Kind::Artifact,
            Some("ct"),
        );
        leaf(
            &mut graph,
            &mut names,
            "artifact2",
            Kind::Artifact,
            Some("ct"),
        );
        node(
            &mut graph,
            &mut names,
            "ct",
            "cfg",
            Kind::ConfiguredTarget,
            None,
        );
        for id in ["artifact1", "artifact2"] {
            edge(&mut graph, &mut names, "action0", id, EdgeKind::Aquery);
            edge(&mut graph, &mut names, id, "action1", EdgeKind::Aquery);
        }

        let out = collapsed(&graph, &mut names);

        let group = find(&out, &names, "group:artifact1");
        assert_eq!(group.label, "2 files");
        assert_eq!(group.parent.map(|p| names.resolve(p)), Some("ct"));
        // Producer edge dedups to one action -> group.
        assert_eq!(
            edges(&out, &names),
            vec![
                ("action0", "group:artifact1"),
                ("group:artifact1", "action1"),
            ]
        );
    }

    #[test]
    fn artifacts_with_different_producers_do_not_merge() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(&mut graph, &mut names, "action0", "P0", Kind::Action, None);
        node(&mut graph, &mut names, "action1", "P1", Kind::Action, None);
        node(
            &mut graph,
            &mut names,
            "action2",
            "Consume",
            Kind::Action,
            None,
        );
        leaf(&mut graph, &mut names, "artifact1", Kind::Artifact, None);
        leaf(&mut graph, &mut names, "artifact2", Kind::Artifact, None);
        edge(
            &mut graph,
            &mut names,
            "action0",
            "artifact1",
            EdgeKind::Aquery,
        );
        edge(
            &mut graph,
            &mut names,
            "action1",
            "artifact2",
            EdgeKind::Aquery,
        );
        edge(
            &mut graph,
            &mut names,
            "artifact1",
            "action2",
            EdgeKind::Aquery,
        );
        edge(
            &mut graph,
            &mut names,
            "artifact2",
            "action2",
            EdgeKind::Aquery,
        );

        let out = collapsed(&graph, &mut names);

        assert!(node_ids(&out, &names).contains(&"artifact1"));
        assert!(node_ids(&out, &names).contains(&"artifact2"));
        assert!(
            !node_ids(&out, &names)
                .iter()
                .any(|id| id.starts_with("group:"))
        );
    }

    #[test]
    fn artifact_container_is_not_merged() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(
            &mut graph,
            &mut names,
            "action0",
            "Produce",
            Kind::Action,
            None,
        );
        // Two artifact leaves with identical neighbors would normally merge,
        // but artifact2 is a container (a promoted labeled output holds a
        // configured-target child), which blocks the merge.
        leaf(&mut graph, &mut names, "artifact1", Kind::Artifact, None);
        leaf(&mut graph, &mut names, "artifact2", Kind::Artifact, None);
        edge(
            &mut graph,
            &mut names,
            "artifact1",
            "action0",
            EdgeKind::Aquery,
        );
        edge(
            &mut graph,
            &mut names,
            "artifact2",
            "action0",
            EdgeKind::Aquery,
        );
        node(
            &mut graph,
            &mut names,
            "ct1",
            "cfg",
            Kind::ConfiguredTarget,
            Some("artifact2"),
        );

        let out = collapsed(&graph, &mut names);

        assert!(
            !node_ids(&out, &names)
                .iter()
                .any(|id| id.starts_with("group:"))
        );
        for id in ["artifact1", "artifact2", "ct1"] {
            assert!(node_ids(&out, &names).contains(&id), "{id} missing");
        }
    }

    #[test]
    fn source_and_artifact_never_merge() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(
            &mut graph,
            &mut names,
            "action0",
            "Consume",
            Kind::Action,
            None,
        );
        leaf(&mut graph, &mut names, "source1", Kind::Source, None);
        leaf(&mut graph, &mut names, "artifact1", Kind::Artifact, None);
        edge(
            &mut graph,
            &mut names,
            "source1",
            "action0",
            EdgeKind::Aquery,
        );
        edge(
            &mut graph,
            &mut names,
            "artifact1",
            "action0",
            EdgeKind::Aquery,
        );

        let out = collapsed(&graph, &mut names);

        assert!(node_ids(&out, &names).contains(&"source1"));
        assert!(node_ids(&out, &names).contains(&"artifact1"));
        assert!(
            !node_ids(&out, &names)
                .iter()
                .any(|id| id.starts_with("group:"))
        );
    }

    #[test]
    fn output_is_independent_of_insertion_order() {
        let mut a = Graph::new();
        let mut names_a = Names::default();
        node(
            &mut a,
            &mut names_a,
            "action0",
            "Compile",
            Kind::Action,
            None,
        );
        for id in ["source1", "source2", "source3"] {
            leaf(&mut a, &mut names_a, id, Kind::Source, None);
            edge(&mut a, &mut names_a, id, "action0", EdgeKind::Aquery);
        }

        let mut b = Graph::new();
        let mut names_b = Names::default();
        for id in ["source3", "source1", "source2"] {
            leaf(&mut b, &mut names_b, id, Kind::Source, None);
        }
        node(
            &mut b,
            &mut names_b,
            "action0",
            "Compile",
            Kind::Action,
            None,
        );
        edge(&mut b, &mut names_b, "source2", "action0", EdgeKind::Aquery);
        edge(&mut b, &mut names_b, "source3", "action0", EdgeKind::Aquery);
        edge(&mut b, &mut names_b, "source1", "action0", EdgeKind::Aquery);

        let qa = quotient(&a, &HashSet::new(), &mut names_a).0;
        let qb = quotient(&b, &HashSet::new(), &mut names_b).0;

        assert_eq!(node_ids(&qa, &names_a), node_ids(&qb, &names_b));
        assert_eq!(edges(&qa, &names_a), edges(&qb, &names_b));
    }

    #[test]
    fn containers_and_actions_pass_through() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(
            &mut graph,
            &mut names,
            "target1",
            "//:t",
            Kind::Target,
            None,
        );
        node(
            &mut graph,
            &mut names,
            "ct",
            "cfg",
            Kind::ConfiguredTarget,
            Some("target1"),
        );
        node(
            &mut graph,
            &mut names,
            "action0",
            "Compile",
            Kind::Action,
            Some("ct"),
        );
        leaf(&mut graph, &mut names, "source1", Kind::Source, None);
        edge(
            &mut graph,
            &mut names,
            "source1",
            "action0",
            EdgeKind::Aquery,
        );

        let out = collapsed(&graph, &mut names);

        for id in ["target1", "ct", "action0", "source1"] {
            assert!(node_ids(&out, &names).contains(&id), "{id} missing");
        }
        assert_eq!(edges(&out, &names), vec![("source1", "action0")]);
    }

    #[test]
    fn expanded_group_becomes_container_holding_its_members() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(
            &mut graph,
            &mut names,
            "action0",
            "Compile",
            Kind::Action,
            None,
        );
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, &mut names, id, Kind::Source, None);
            edge(&mut graph, &mut names, id, "action0", EdgeKind::Aquery);
        }

        let set = expanded(&mut names, &["group:source1"]);
        let out = quotient(&graph, &set, &mut names).0;

        // The group is a container; the collapsed leaf is gone.
        let group = find(&out, &names, "group:source1");
        assert_eq!(group.kind, Kind::MergeGroup);
        assert_eq!(group.label, "3 sources");
        assert_eq!(group.parent, None);
        // Each member survives with its own id, reparented to the group.
        for id in ["source1", "source2", "source3"] {
            let member = find(&out, &names, id);
            assert_eq!(member.kind, Kind::Source);
            assert_eq!(
                member.parent.map(|p| names.resolve(p)),
                Some("group:source1")
            );
        }
        // Member edges keep their original endpoints (no group: remap).
        assert_eq!(
            edges(&out, &names),
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
        let mut names = Names::default();
        node(
            &mut graph,
            &mut names,
            "action0",
            "Compile",
            Kind::Action,
            None,
        );
        for id in ["source1", "source2", "source3"] {
            leaf(&mut graph, &mut names, id, Kind::Source, None);
            edge(&mut graph, &mut names, id, "action0", EdgeKind::Aquery);
        }

        // Expanding an id that is not a multi-member group collapses as usual.
        let set = expanded(&mut names, &["group:other"]);
        let out = quotient(&graph, &set, &mut names).0;

        assert_eq!(out.nodes.len(), 2);
        assert_eq!(find(&out, &names, "group:source1").kind, Kind::Source);
        assert_eq!(edges(&out, &names), vec![("group:source1", "action0")]);
    }

    #[test]
    fn one_group_expands_while_another_collapses() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        node(&mut graph, &mut names, "action0", "A", Kind::Action, None);
        node(&mut graph, &mut names, "action1", "B", Kind::Action, None);
        // Group on action0 (will be expanded).
        for id in ["source1", "source2"] {
            leaf(&mut graph, &mut names, id, Kind::Source, None);
            edge(&mut graph, &mut names, id, "action0", EdgeKind::Aquery);
        }
        // Group on action1 (will stay collapsed).
        for id in ["source3", "source4"] {
            leaf(&mut graph, &mut names, id, Kind::Source, None);
            edge(&mut graph, &mut names, id, "action1", EdgeKind::Aquery);
        }

        let set = expanded(&mut names, &["group:source1"]);
        let out = quotient(&graph, &set, &mut names).0;

        // First group is a container with its members.
        assert_eq!(find(&out, &names, "group:source1").kind, Kind::MergeGroup);
        assert_eq!(
            find(&out, &names, "source1")
                .parent
                .map(|p| names.resolve(p)),
            Some("group:source1")
        );
        assert_eq!(
            find(&out, &names, "source2")
                .parent
                .map(|p| names.resolve(p)),
            Some("group:source1")
        );
        // Second group collapses to a single source leaf.
        assert_eq!(find(&out, &names, "group:source3").kind, Kind::Source);
        assert_eq!(find(&out, &names, "group:source3").label, "2 sources");
        assert!(!node_ids(&out, &names).contains(&"source3"));
        assert_eq!(
            edges(&out, &names),
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
        let mut names_a = Names::default();
        node(
            &mut a,
            &mut names_a,
            "action0",
            "Compile",
            Kind::Action,
            None,
        );
        for id in ["source1", "source2", "source3"] {
            leaf(&mut a, &mut names_a, id, Kind::Source, None);
            edge(&mut a, &mut names_a, id, "action0", EdgeKind::Aquery);
        }

        let mut b = Graph::new();
        let mut names_b = Names::default();
        for id in ["source3", "source1", "source2"] {
            leaf(&mut b, &mut names_b, id, Kind::Source, None);
        }
        node(
            &mut b,
            &mut names_b,
            "action0",
            "Compile",
            Kind::Action,
            None,
        );
        edge(&mut b, &mut names_b, "source2", "action0", EdgeKind::Aquery);
        edge(&mut b, &mut names_b, "source3", "action0", EdgeKind::Aquery);
        edge(&mut b, &mut names_b, "source1", "action0", EdgeKind::Aquery);

        let set_a = expanded(&mut names_a, &["group:source1"]);
        let set_b = expanded(&mut names_b, &["group:source1"]);
        let qa = quotient(&a, &set_a, &mut names_a).0;
        let qb = quotient(&b, &set_b, &mut names_b).0;

        assert_eq!(node_ids(&qa, &names_a), node_ids(&qb, &names_b));
        assert_eq!(edges(&qa, &names_a), edges(&qb, &names_b));
    }
}
