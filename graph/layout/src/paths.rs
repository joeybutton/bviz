//! The part of a graph lying on dependency paths into one of its nodes, after
//! Pathfinder (Partl et al., "Pathfinder: Visual Analysis of Paths in Graphs",
//! Computer Graphics Forum 35(3), 2016).
//!
//! A path is walked as [`crate::reach`] walks. A node named as an endpoint
//! stands for its whole subtree, and an edge running within that subtree is no
//! part of any path to or from it.
//!
//! A path may pass through a cycle, and every edge of a cycle one passes
//! through is kept, whether or not a path repeating no node could use it.

use crate::graph::{Edge, EdgeKind, Graph, id_span};
use crate::intern::Id;
use crate::reach::{Index, State, Toward};

/// The nodes and edges lying on some dependency path.
#[derive(Clone)]
pub struct Paths {
    /// Sorted by id.
    pub nodes: Vec<Id>,
    /// In the order the graph lists them.
    pub edges: Vec<Edge>,
}

impl Paths {
    /// `graph` cut down to these paths: their nodes with every containment
    /// ancestor, so each still stands in the package and target holding it, and
    /// their edges alone. Every node kept keeps its id and its parent.
    pub fn subgraph(&self, graph: &Graph) -> Graph {
        let ids = self
            .nodes
            .iter()
            .map(|id| id.0 as usize + 1)
            .fold(id_span(graph), usize::max);
        let mut parent: Vec<Option<Id>> = vec![None; ids];
        for node in &graph.nodes {
            parent[node.id.0 as usize] = node.parent;
        }
        let mut kept: Vec<bool> = vec![false; ids];
        for &id in &self.nodes {
            let mut at = Some(id);
            while let Some(id) = at.filter(|id| !kept[id.0 as usize]) {
                kept[id.0 as usize] = true;
                at = parent[id.0 as usize];
            }
        }
        Graph {
            nodes: graph
                .nodes
                .iter()
                .filter(|node| kept[node.id.0 as usize])
                .cloned()
                .collect(),
            edges: self.edges.clone(),
        }
    }
}

/// The dependency paths joining a dependent to one of its dependencies.
#[derive(Clone)]
pub struct Between {
    pub paths: Paths,
    /// One path of fewest edges, from the dependency to the dependent. Each
    /// node is joined to the next by an edge of `paths` or stands in one
    /// containment line with it.
    pub shortest: Vec<Id>,
}

/// Everything that depends on `dependency`, directly or not, with the edges it
/// depends on it through.
pub fn dependents(graph: &Graph, dependency: Id) -> Paths {
    let index = Index::new(graph, &[dependency]);
    let seeds = index.subtree(dependency);
    let reach = index.walk(&arrivals(&seeds), Toward::Dependents);
    let edges: Vec<Edge> = graph
        .edges
        .iter()
        .filter(|e| walked(e) && reach.reached(e.from) && !within(&seeds, e))
        .cloned()
        .collect();
    Paths {
        nodes: spanned(&[dependency], &edges),
        edges,
    }
}

/// The dependency paths from `dependent` down to `dependency`, `None` where
/// `dependent` does not depend on it.
pub fn between(graph: &Graph, dependency: Id, dependent: Id) -> Option<Between> {
    let index = Index::new(graph, &[dependency, dependent]);
    let from = index.subtree(dependency);
    let forward = index.walk(&arrivals(&from), Toward::Dependents);
    let end = forward.nearest(dependent)?;
    let to = index.subtree(dependent);
    let at_dependent = dependent.0 as usize;
    let backward = index.walk(
        &[State::arrived(at_dependent), State::lifted(at_dependent)],
        Toward::Dependencies,
    );
    let edges: Vec<Edge> = graph
        .edges
        .iter()
        .filter(|e| {
            walked(e)
                && forward.reached(e.from)
                && backward.arrived(e.to)
                && !within(&from, e)
                && !within(&to, e)
        })
        .cloned()
        .collect();
    let shortest = forward.chain(graph, end, dependency, dependent);
    Some(Between {
        paths: Paths {
            nodes: spanned(&[dependency, dependent], &edges),
            edges,
        },
        shortest,
    })
}

/// The arrival at each node `members` marks.
fn arrivals(members: &[bool]) -> Vec<usize> {
    let count = members.iter().filter(|&&member| member).count();
    let mut states: Vec<usize> = Vec::with_capacity(count);
    states.extend(
        (0..members.len())
            .filter(|&id| members[id])
            .map(State::arrived),
    );
    states
}

/// Whether a walk takes `edge`, which it does unless the edge stands in for a
/// path through nodes a filter dropped.
fn walked(edge: &Edge) -> bool {
    edge.kind != EdgeKind::Transitive
}

/// Whether both ends of `edge` lie in the subtree `members` marks.
fn within(members: &[bool], edge: &Edge) -> bool {
    members[edge.from.0 as usize] && members[edge.to.0 as usize]
}

/// `endpoints` and the ends of `edges`, sorted and each stated once.
fn spanned(endpoints: &[Id], edges: &[Edge]) -> Vec<Id> {
    let mut nodes: Vec<Id> = Vec::with_capacity(endpoints.len() + 2 * edges.len());
    nodes.extend_from_slice(endpoints);
    nodes.extend(edges.iter().flat_map(|e| [e.from, e.to]));
    nodes.sort_unstable();
    nodes.dedup();
    nodes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Kind, Node};

    fn graph(nodes: &[(u32, Kind, Option<u32>)], edges: &[(u32, u32)]) -> Graph {
        let mut graph = Graph::new();
        for &(id, kind, parent) in nodes {
            graph.add_node(Node::new(Id(id), "n", kind, parent.map(Id)));
        }
        for &(from, to) in edges {
            graph.add_edge(Id(from), Id(to), EdgeKind::Query);
        }
        graph
    }

    /// Flat targets, one per id in `0..count`.
    fn targets(count: u32, edges: &[(u32, u32)]) -> Graph {
        let nodes: Vec<(u32, Kind, Option<u32>)> =
            (0..count).map(|id| (id, Kind::Target, None)).collect();
        graph(&nodes, edges)
    }

    fn nodes(paths: &Paths) -> Vec<u32> {
        paths.nodes.iter().map(|id| id.0).collect()
    }

    fn edges(paths: &Paths) -> Vec<(u32, u32)> {
        let mut pairs: Vec<(u32, u32)> = paths.edges.iter().map(|e| (e.from.0, e.to.0)).collect();
        pairs.sort_unstable();
        pairs
    }

    fn chain(between: &Between) -> Vec<u32> {
        between.shortest.iter().map(|id| id.0).collect()
    }

    /// `0` feeds `1` and `2`, which both feed `3`; `4` is a second dependency
    /// of `1`, and `5` a dependent of `3`.
    fn diamond() -> Graph {
        targets(6, &[(0, 1), (0, 2), (1, 3), (2, 3), (4, 1), (3, 5)])
    }

    #[test]
    fn both_sides_of_a_diamond_lie_between_its_ends() {
        let found = between(&diamond(), Id(0), Id(3)).expect("a path");
        assert_eq!(nodes(&found.paths), [0, 1, 2, 3]);
        assert_eq!(edges(&found.paths), [(0, 1), (0, 2), (1, 3), (2, 3)]);
        assert_eq!(found.shortest.len(), 3);
        assert_eq!(found.shortest.first(), Some(&Id(0)));
        assert_eq!(found.shortest.last(), Some(&Id(3)));
    }

    #[test]
    fn a_branch_off_the_paths_is_left_out() {
        let found = between(&diamond(), Id(0), Id(3)).expect("a path");
        assert!(!found.paths.nodes.contains(&Id(4)), "another dependency");
        assert!(!found.paths.nodes.contains(&Id(5)), "a dependent beyond");
    }

    #[test]
    fn a_dependent_upstream_of_the_dependency_is_no_path() {
        assert!(between(&diamond(), Id(3), Id(0)).is_none());
        assert!(between(&diamond(), Id(4), Id(2)).is_none());
    }

    #[test]
    fn a_node_depends_on_itself_by_no_edge() {
        let found = between(&diamond(), Id(1), Id(1)).expect("a node reaches itself");
        assert_eq!(nodes(&found.paths), [1]);
        assert!(found.paths.edges.is_empty());
        assert_eq!(chain(&found), [1]);
    }

    #[test]
    fn a_cycle_on_the_way_is_kept_whole() {
        let cyclic = targets(4, &[(0, 1), (1, 2), (2, 1), (2, 3)]);
        let found = between(&cyclic, Id(0), Id(3)).expect("a path");
        assert_eq!(edges(&found.paths), [(0, 1), (1, 2), (2, 1), (2, 3)]);
        assert_eq!(chain(&found), [0, 1, 2, 3]);
    }

    #[test]
    fn with_no_dependent_named_every_dependent_is_kept() {
        let found = dependents(&diamond(), Id(1));
        assert_eq!(nodes(&found), [1, 3, 5]);
        assert_eq!(edges(&found), [(1, 3), (3, 5)]);
    }

    #[test]
    fn a_dependency_nothing_depends_on_stands_alone() {
        let found = dependents(&diamond(), Id(5));
        assert_eq!(nodes(&found), [5]);
        assert!(found.edges.is_empty());
    }

    /// Package `0` holds targets `1`, `2` and `3`, holding configured targets
    /// `11`, `12` and `13`. The two lower targets are joined only by a label
    /// edge `1 -> 2`, the upper two only by a configured edge `12 -> 13`.
    fn levels() -> Graph {
        graph(
            &[
                (0, Kind::Package, None),
                (1, Kind::Target, Some(0)),
                (2, Kind::Target, Some(0)),
                (3, Kind::Target, Some(0)),
                (11, Kind::ConfiguredTarget, Some(1)),
                (12, Kind::ConfiguredTarget, Some(2)),
                (13, Kind::ConfiguredTarget, Some(3)),
            ],
            &[(1, 2), (12, 13)],
        )
    }

    #[test]
    fn a_path_crosses_between_label_and_configured_edges() {
        let found = between(&levels(), Id(11), Id(13)).expect("a path");
        assert_eq!(edges(&found.paths), [(1, 2), (12, 13)]);
        assert_eq!(nodes(&found.paths), [1, 2, 11, 12, 13]);
        assert_eq!(chain(&found), [11, 1, 2, 12, 13]);
    }

    #[test]
    fn a_target_named_stands_for_its_configured_targets() {
        let found = between(&levels(), Id(2), Id(13)).expect("a path");
        assert_eq!(edges(&found.paths), [(12, 13)]);
        assert_eq!(chain(&found), [2, 12, 13]);
        let found = between(&levels(), Id(1), Id(3)).expect("a path");
        assert_eq!(edges(&found.paths), [(1, 2), (12, 13)]);
    }

    /// Every consecutive pair of `found`'s shortest chain is joined by one of
    /// its edges or stands in one containment line in `graph`.
    fn assert_chained(graph: &Graph, found: &Between) {
        let parent = |id: Id| graph.nodes.iter().find(|n| n.id == id)?.parent;
        let contains = |outer: Id, inner: Id| {
            std::iter::successors(Some(inner), |&id| parent(id)).any(|id| id == outer)
        };
        for pair in found.shortest.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            let joined = found.paths.edges.iter().any(|e| e.from == a && e.to == b);
            assert!(
                joined || contains(a, b) || contains(b, a),
                "{a:?} -> {b:?} in {:?}",
                chain(found)
            );
        }
    }

    /// Target `0` holds configured target `1`, which holds action `2`; action
    /// `3` feeds that action, and the configured target feeds `4`.
    #[test]
    fn a_path_into_a_member_leaves_by_its_container() {
        let graph = graph(
            &[
                (0, Kind::Target, None),
                (1, Kind::ConfiguredTarget, Some(0)),
                (2, Kind::Action, Some(1)),
                (3, Kind::Action, None),
                (4, Kind::Target, None),
            ],
            &[(3, 2), (1, 4)],
        );
        let found = between(&graph, Id(3), Id(4)).expect("a path");
        assert_eq!(edges(&found.paths), [(1, 4), (3, 2)]);
        assert_eq!(chain(&found), [3, 2, 1, 4]);
        assert_chained(&graph, &found);
    }

    /// An edge into a configured target reaches none of what it holds, so it
    /// is no part of a path leaving from inside it.
    #[test]
    fn an_edge_into_a_container_does_not_lead_out_of_its_members() {
        let graph = graph(
            &[
                (0, Kind::Target, None),
                (1, Kind::ConfiguredTarget, None),
                (2, Kind::Action, Some(1)),
                (3, Kind::Target, None),
            ],
            &[(0, 1), (2, 3), (0, 3)],
        );
        let found = between(&graph, Id(0), Id(3)).expect("a path");
        assert_eq!(edges(&found.paths), [(0, 3)]);
        assert_chained(&graph, &found);
    }

    #[test]
    fn every_shortest_chain_is_joined_link_by_link() {
        let cases = [
            (levels(), Id(11), Id(13)),
            (levels(), Id(1), Id(3)),
            (levels(), Id(2), Id(13)),
            (diamond(), Id(0), Id(5)),
        ];
        for (graph, dependency, dependent) in cases {
            let found = between(&graph, dependency, dependent).expect("a path");
            assert_chained(&graph, &found);
        }
    }

    #[test]
    fn a_dependent_inside_the_dependency_is_joined_by_containment_alone() {
        let found = between(&levels(), Id(1), Id(11)).expect("held within");
        assert!(found.paths.edges.is_empty());
        assert_eq!(chain(&found), [1, 11]);
        let found = between(&levels(), Id(11), Id(1)).expect("held within");
        assert!(found.paths.edges.is_empty());
        assert_eq!(chain(&found), [11, 1]);
    }

    #[test]
    fn a_package_joins_nothing_it_holds() {
        assert!(between(&levels(), Id(12), Id(11)).is_none());
        assert!(between(&levels(), Id(13), Id(1)).is_none());
    }

    /// An edge landing on a target reaches its configured targets, and no
    /// further down: what those hold is reached by edges of its own.
    #[test]
    fn a_label_edge_does_not_reach_below_the_configured_targets() {
        let mut graph = levels();
        graph.add_node(Node::new(Id(20), "compile", Kind::Action, Some(Id(12))));
        graph.add_node(Node::new(Id(21), "t", Kind::Target, None));
        graph.add_edge(Id(20), Id(21), EdgeKind::Aquery);
        let found = dependents(&graph, Id(1));
        assert!(!found.nodes.contains(&Id(21)), "{:?}", nodes(&found));
        assert_eq!(edges(&found), [(1, 2), (12, 13)]);
    }

    #[test]
    fn an_edge_within_a_named_subtree_is_no_path() {
        let mut graph = levels();
        graph.add_node(Node::new(Id(20), "compile", Kind::Action, Some(Id(11))));
        graph.add_node(Node::new(Id(21), "link", Kind::Action, Some(Id(11))));
        graph.add_edge(Id(20), Id(21), EdgeKind::Aquery);
        assert_eq!(edges(&dependents(&graph, Id(1))), [(1, 2), (12, 13)]);
        let found = between(&graph, Id(1), Id(3)).expect("a path");
        assert_eq!(edges(&found.paths), [(1, 2), (12, 13)]);
    }

    #[test]
    fn a_subgraph_keeps_the_frames_around_the_paths_and_their_edges_alone() {
        let mut graph = levels();
        graph.add_edge(Id(11), Id(12), EdgeKind::Cquery);
        let found = between(&graph, Id(12), Id(13)).expect("a path");
        let drawn = found.paths.subgraph(&graph);
        let mut kept: Vec<(u32, Option<u32>)> = drawn
            .nodes
            .iter()
            .map(|n| (n.id.0, n.parent.map(|p| p.0)))
            .collect();
        kept.sort_unstable();
        assert_eq!(
            kept,
            [
                (0, None),
                (2, Some(0)),
                (3, Some(0)),
                (12, Some(2)),
                (13, Some(3))
            ]
        );
        let pairs: Vec<(u32, u32)> = drawn.edges.iter().map(|e| (e.from.0, e.to.0)).collect();
        assert_eq!(pairs, [(12, 13)]);
    }

    #[test]
    fn an_edge_standing_in_for_a_path_lies_on_no_path() {
        let mut graph = diamond();
        graph.add_edge(Id(0), Id(5), EdgeKind::Transitive);
        assert_eq!(
            edges(&dependents(&graph, Id(0))),
            [(0, 1), (0, 2), (1, 3), (2, 3), (3, 5)]
        );
        let found = between(&graph, Id(0), Id(5)).expect("a path");
        assert_eq!(found.shortest.len(), 4);
        assert!(
            !found
                .paths
                .edges
                .iter()
                .any(|e| e.kind == EdgeKind::Transitive)
        );
    }

    #[test]
    fn an_endpoint_the_graph_does_not_mention_stands_alone() {
        let found = dependents(&diamond(), Id(40));
        assert_eq!(nodes(&found), [40]);
        assert!(between(&diamond(), Id(0), Id(40)).is_none());
    }
}
