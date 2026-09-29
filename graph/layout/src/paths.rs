//! The part of a graph lying on dependency paths into one of its nodes, after
//! Pathfinder (Partl et al., "Pathfinder: Visual Analysis of Paths in Graphs",
//! Computer Graphics Forum 35(3), 2016).
//!
//! A path is walked over edges and over containment. An edge leaving a container
//! leaves each node inside it, and an edge landing on a target lands on each of
//! its configured targets, which is what joins the label-level edges a query
//! states to the configured edges a cquery states. A node named as an endpoint
//! stands for its whole subtree, and an edge running within that subtree is no
//! part of any path to or from it.
//!
//! A path may pass through a cycle, and every edge of a cycle one passes
//! through is kept, whether or not a path repeating no node could use it.

use std::collections::VecDeque;

use crate::graph::{Edge, Graph, Kind, id_span};
use crate::intern::Id;

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
        .filter(|e| reach.reached(e.from) && !within(&seeds, e))
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
            forward.reached(e.from)
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

/// Which way along its edges a walk runs.
#[derive(Clone, Copy)]
enum Toward {
    Dependents,
    Dependencies,
}

/// A table of lists by dense key, all held in one buffer.
struct CompressedSparseRows {
    /// Where each key's list starts in `items`, with one more entry closing the
    /// last.
    starts: Vec<u32>,
    items: Vec<u32>,
}

impl CompressedSparseRows {
    /// The table of `entries` over keys `0..keys`. `entries` yields the same
    /// pairs each time it is called: one pass counts them, the next places them.
    fn new<I: Iterator<Item = (usize, u32)>>(keys: usize, entries: impl Fn() -> I) -> Self {
        let mut starts: Vec<u32> = vec![0; keys + 1];
        for (key, _) in entries() {
            starts[key + 1] += 1;
        }
        for key in 0..keys {
            starts[key + 1] += starts[key];
        }
        let mut next: Vec<u32> = starts[..keys].to_vec();
        let mut items: Vec<u32> = vec![0; starts[keys] as usize];
        for (key, item) in entries() {
            items[next[key] as usize] = item;
            next[key] += 1;
        }
        CompressedSparseRows { starts, items }
    }

    fn of(&self, key: usize) -> &[u32] {
        &self.items[self.starts[key] as usize..self.starts[key + 1] as usize]
    }
}

/// A graph's adjacency and containment, by node id.
struct Index<'g> {
    graph: &'g Graph,
    parent: Vec<Option<Id>>,
    kind: Vec<Option<Kind>>,
    children: CompressedSparseRows,
    /// The edges leaving each node, by position in the graph's edge list.
    outgoing: CompressedSparseRows,
    /// The edges landing on each node, likewise.
    incoming: CompressedSparseRows,
}

impl<'g> Index<'g> {
    /// The index of `graph`, spanning `named` too where the graph mentions none
    /// of them, so that each stands as a node with nothing around it.
    fn new(graph: &'g Graph, named: &[Id]) -> Self {
        let ids = named
            .iter()
            .map(|id| id.0 as usize + 1)
            .fold(id_span(graph), usize::max);
        let mut parent: Vec<Option<Id>> = vec![None; ids];
        let mut kind: Vec<Option<Kind>> = vec![None; ids];
        for node in &graph.nodes {
            parent[node.id.0 as usize] = node.parent;
            kind[node.id.0 as usize] = Some(node.kind);
        }
        let children = CompressedSparseRows::new(ids, || {
            graph
                .nodes
                .iter()
                .filter_map(|n| Some((n.parent?.0 as usize, n.id.0)))
        });
        let outgoing = CompressedSparseRows::new(ids, || {
            graph
                .edges
                .iter()
                .enumerate()
                .map(|(i, e)| (e.from.0 as usize, i as u32))
        });
        let incoming = CompressedSparseRows::new(ids, || {
            graph
                .edges
                .iter()
                .enumerate()
                .map(|(i, e)| (e.to.0 as usize, i as u32))
        });
        Index {
            graph,
            parent,
            kind,
            children,
            outgoing,
            incoming,
        }
    }

    fn ids(&self) -> usize {
        self.parent.len()
    }

    /// Which nodes `root`'s subtree holds, by id.
    fn subtree(&self, root: Id) -> Vec<bool> {
        let mut members: Vec<bool> = vec![false; self.ids()];
        let mut open: Vec<u32> = vec![root.0];
        while let Some(id) = open.pop() {
            members[id as usize] = true;
            open.extend_from_slice(self.children.of(id as usize));
        }
        members
    }

    /// Every state reachable from `seeds` walking `toward`, by fewest edges.
    ///
    /// A node is either arrived at, where an edge or the target holding it
    /// brought a path, or only lifted through, where a path arrived at a node
    /// below it. Toward dependents, both leave by the node's own edges and lift
    /// to its parent, and only an arrival at a target descends to what it holds.
    /// Toward dependencies the walk retraces those moves backwards, so each
    /// state it reaches leads on to one of `seeds`. Containment moves cost
    /// nothing, so the walk is a 0-1 breadth-first search.
    fn walk(&self, seeds: &[usize], toward: Toward) -> Walk {
        let mut walk = Walk {
            fewest: vec![UNREACHED; 2 * self.ids()],
            step: vec![Step::Seed; 2 * self.ids()],
        };
        let mut open: VecDeque<usize> = VecDeque::with_capacity(self.ids());
        for &seed in seeds {
            walk.fewest[seed] = 0;
            open.push_back(seed);
        }
        let mut moves: Vec<Move> = Vec::new();
        while let Some(at) = open.pop_front() {
            moves.clear();
            self.moves(at, toward, &mut moves);
            for &Move { to, over } in &moves {
                let (edges, step) = match over {
                    Some(edge) => (walk.fewest[at] + 1, Step::Edge(at, edge)),
                    None => (walk.fewest[at], Step::Contain(at)),
                };
                if walk.improve(to, edges, step) {
                    match over {
                        Some(_) => open.push_back(to),
                        None => open.push_front(to),
                    }
                }
            }
        }
        walk
    }

    /// Every move a walk `toward` makes from the state `at`, into `moves`.
    fn moves(&self, at: usize, toward: Toward, moves: &mut Vec<Move>) {
        let id = State::id(at);
        let arrival = State::is_arrival(at);
        let held = || self.children.of(id).iter().map(|&child| child as usize);
        let is_target = |id: usize| self.kind[id] == Some(Kind::Target);
        match toward {
            Toward::Dependents => {
                for &edge in self.outgoing.of(id) {
                    let to = self.graph.edges[edge as usize].to.0 as usize;
                    moves.push(Move::over(State::arrived(to), edge));
                }
                if let Some(parent) = self.parent[id] {
                    moves.push(Move::within(State::lifted(parent.0 as usize)));
                }
                if arrival && is_target(id) {
                    moves.extend(held().map(|child| Move::within(State::arrived(child))));
                }
            }
            Toward::Dependencies => {
                if arrival {
                    for &edge in self.incoming.of(id) {
                        let from = self.graph.edges[edge as usize].from.0 as usize;
                        moves.push(Move::over(State::arrived(from), edge));
                        moves.push(Move::over(State::lifted(from), edge));
                    }
                } else {
                    for child in held() {
                        moves.push(Move::within(State::arrived(child)));
                        moves.push(Move::within(State::lifted(child)));
                    }
                }
                match self.parent[id] {
                    Some(parent) if arrival && is_target(parent.0 as usize) => {
                        moves.push(Move::within(State::arrived(parent.0 as usize)));
                    }
                    _ => {}
                }
            }
        }
    }
}

/// One move of a walk: to a state, over an edge or within a containment line.
#[derive(Clone, Copy)]
struct Move {
    to: usize,
    /// The edge taken, by position in the graph's edge list.
    over: Option<u32>,
}

impl Move {
    fn over(to: usize, edge: u32) -> Self {
        Move {
            to,
            over: Some(edge),
        }
    }

    fn within(to: usize) -> Self {
        Move { to, over: None }
    }
}

/// The fewest edges of a state no walk has reached.
const UNREACHED: u32 = u32::MAX;

/// A walk's position: a node, arrived at or only lifted through. Each node has
/// two, the arrival at `2 * id` and the lift beside it.
struct State;

impl State {
    fn arrived(id: usize) -> usize {
        2 * id
    }

    fn lifted(id: usize) -> usize {
        2 * id + 1
    }

    fn id(state: usize) -> usize {
        state / 2
    }

    fn is_arrival(state: usize) -> bool {
        state.is_multiple_of(2)
    }
}

/// How a walk last reached a state.
#[derive(Clone, Copy)]
enum Step {
    Seed,
    /// A move to a parent or a child from the state given.
    Contain(usize),
    /// A move over the edge at this position of the graph's edge list from the
    /// state given.
    Edge(usize, u32),
}

/// What one walk reached, by state.
struct Walk {
    fewest: Vec<u32>,
    /// Meaningful only for a state `fewest` holds reached.
    step: Vec<Step>,
}

impl Walk {
    /// Records reaching `state` in `edges` by `step` where that beats what the
    /// walk had, reporting whether it did.
    fn improve(&mut self, state: usize, edges: u32, step: Step) -> bool {
        let better = edges < self.fewest[state];
        if better {
            self.fewest[state] = edges;
            self.step[state] = step;
        }
        better
    }

    fn reached(&self, id: Id) -> bool {
        let id = id.0 as usize;
        self.fewest[State::arrived(id)] != UNREACHED || self.fewest[State::lifted(id)] != UNREACHED
    }

    fn arrived(&self, id: Id) -> bool {
        self.fewest[State::arrived(id.0 as usize)] != UNREACHED
    }

    /// The state of `id` reached in fewest edges, `None` where neither is.
    fn nearest(&self, id: Id) -> Option<usize> {
        let id = id.0 as usize;
        [State::arrived(id), State::lifted(id)]
            .into_iter()
            .filter(|&state| self.fewest[state] != UNREACHED)
            .min_by_key(|&state| self.fewest[state])
    }

    /// The nodes the walk passed on its way from `start`'s subtree to `end`, a
    /// state of `finish`: `start`, the ends of each edge it took, and `finish`,
    /// each named once where consecutive.
    fn chain(&self, graph: &Graph, end: usize, start: Id, finish: Id) -> Vec<Id> {
        let mut taken: Vec<u32> = Vec::with_capacity(self.fewest[end] as usize);
        let mut at = end;
        loop {
            match self.step[at] {
                Step::Seed => break,
                Step::Contain(from) => at = from,
                Step::Edge(from, edge) => {
                    taken.push(edge);
                    at = from;
                }
            }
        }
        let mut chain: Vec<Id> = Vec::with_capacity(2 * taken.len() + 2);
        chain.push(start);
        let ends = taken.iter().rev().flat_map(|&edge| {
            let edge = &graph.edges[edge as usize];
            [edge.from, edge.to]
        });
        for id in ends.chain([finish]) {
            if chain.last() != Some(&id) {
                chain.push(id);
            }
        }
        chain
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Node};

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
    fn an_endpoint_the_graph_does_not_mention_stands_alone() {
        let found = dependents(&diamond(), Id(40));
        assert_eq!(nodes(&found), [40]);
        assert!(between(&diamond(), Id(0), Id(40)).is_none());
    }
}
