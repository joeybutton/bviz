//! Walks over a graph's edges and containment together. An edge leaving a
//! container leaves each node inside it, and an edge landing on a target lands
//! on each of its configured targets, which is what joins the label-level edges
//! a query states to the configured edges a cquery states.

use std::collections::VecDeque;

use fixedbitset::FixedBitSet;

use crate::graph::{EdgeKind, Graph, Kind, id_span};
use crate::intern::Id;

/// How far down the containment of a build a node stands: a target or file
/// above the configured targets copying it, and those above the actions they
/// run and the files those write. Ordered shallowest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Level {
    Target,
    ConfiguredTarget,
    Action,
}

impl Level {
    /// The level of a node of `kind` held, through any merge groups, by a node
    /// of `parent`. `None` for a
    /// node standing at every level: a frame, and a source file, which no
    /// configuration copies, so that the walk of any level reaches it. A
    /// generated file stands with the targets where a package holds it, and
    /// with the actions where a configured target does.
    pub(crate) fn of(kind: Kind, parent: Option<Kind>) -> Option<Level> {
        match kind {
            Kind::Target | Kind::TargetGroup => Some(Level::Target),
            Kind::ConfiguredTarget => Some(Level::ConfiguredTarget),
            Kind::Action => Some(Level::Action),
            Kind::Artifact => match parent {
                Some(Kind::ConfiguredTarget) => Some(Level::Action),
                _ => Some(Level::Target),
            },
            Kind::Source | Kind::Package | Kind::MergeGroup => None,
        }
    }
}

/// What a walk may cover beyond what its moves reach.
#[derive(Clone, Copy)]
pub(crate) struct Bounds<'a> {
    /// The most edges a walk takes; containment moves take none.
    pub(crate) depth: Option<u32>,
    /// The nodes a walk keeps to, by id, but for those above the ceiling it
    /// passes through; a seed outside them is dropped.
    pub(crate) within: Option<&'a FixedBitSet>,
    /// The shallowest level the walk covers. A node standing above it is only
    /// passed through between two nodes at or below it: toward dependents a
    /// walk lifts into it to leave by an edge landing at or below the ceiling,
    /// as a cquery edge leaves a dependency's target for a configured target,
    /// and toward dependencies it enters over such an edge to descend to what
    /// it holds. Each move is held back alike in either direction, so a walk
    /// toward dependencies retraces exactly the walks toward dependents.
    pub(crate) ceiling: Level,
}

impl Bounds<'_> {
    pub(crate) const NONE: Bounds<'static> = Bounds {
        depth: None,
        within: None,
        ceiling: Level::Target,
    };
}

/// Which way along its edges a walk runs.
#[derive(Clone, Copy)]
pub(crate) enum Toward {
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
pub(crate) struct Index<'g> {
    graph: &'g Graph,
    parent: Vec<Option<Id>>,
    kind: Vec<Option<Kind>>,
    level: Vec<Option<Level>>,
    children: CompressedSparseRows,
    /// The edges leaving each node, by position in the graph's edge list, but
    /// for those standing in for a path through nodes a filter dropped, which
    /// a walk would cross twice.
    outgoing: CompressedSparseRows,
    /// The edges landing on each node, likewise.
    incoming: CompressedSparseRows,
}

impl<'g> Index<'g> {
    /// The index of `graph`, spanning `named` too where the graph mentions none
    /// of them, so that each stands as a node with nothing around it.
    pub(crate) fn new(graph: &'g Graph, named: &[Id]) -> Self {
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
        let mut level: Vec<Option<Level>> = vec![None; ids];
        for node in &graph.nodes {
            let holders = std::iter::successors(node.parent, |&at| parent[at.0 as usize]);
            let mut kinds = holders.map(|holder| kind[holder.0 as usize]);
            let holder = kinds.find(|&kind| kind != Some(Kind::MergeGroup)).flatten();
            level[node.id.0 as usize] = Level::of(node.kind, holder);
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
                .filter(|(_, e)| e.kind != EdgeKind::Transitive)
                .map(|(i, e)| (e.from.0 as usize, i as u32))
        });
        let incoming = CompressedSparseRows::new(ids, || {
            graph
                .edges
                .iter()
                .enumerate()
                .filter(|(_, e)| e.kind != EdgeKind::Transitive)
                .map(|(i, e)| (e.to.0 as usize, i as u32))
        });
        Index {
            graph,
            parent,
            kind,
            level,
            children,
            outgoing,
            incoming,
        }
    }

    fn ids(&self) -> usize {
        self.parent.len()
    }

    /// Which nodes `root`'s subtree holds, by id.
    pub(crate) fn subtree(&self, root: Id) -> Vec<bool> {
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
    pub(crate) fn walk(&self, seeds: &[usize], toward: Toward, bounds: Bounds<'_>) -> Walk {
        let mut walk = Walk {
            fewest: vec![UNREACHED; 2 * self.ids()],
            step: vec![Step::Seed; 2 * self.ids()],
        };
        let above = |id: usize| self.level[id].is_some_and(|level| level < bounds.ceiling);
        let kept = |state: usize| {
            let id = State::id(state);
            above(id) || bounds.within.is_none_or(|within| within.contains(id))
        };
        let mut open: VecDeque<usize> = VecDeque::with_capacity(self.ids());
        for &seed in seeds.iter().filter(|&&seed| kept(seed)) {
            walk.fewest[seed] = 0;
            open.push_back(seed);
        }
        let mut moves: Vec<Move> = Vec::new();
        while let Some(at) = open.pop_front() {
            moves.clear();
            self.moves(at, toward, &mut moves);
            for &Move { to, over, pivot } in &moves {
                if above(pivot) || !kept(to) {
                    continue;
                }
                let (edges, step) = match over {
                    Some(_) if bounds.depth.is_some_and(|depth| walk.fewest[at] >= depth) => {
                        continue;
                    }
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
                    moves.push(Move::over(State::arrived(to), edge, to));
                }
                if let Some(parent) = self.parent[id] {
                    moves.push(Move::within(State::lifted(parent.0 as usize), id));
                }
                if arrival && is_target(id) {
                    moves.extend(held().map(|child| Move::within(State::arrived(child), id)));
                }
            }
            Toward::Dependencies => {
                if arrival {
                    for &edge in self.incoming.of(id) {
                        let from = self.graph.edges[edge as usize].from.0 as usize;
                        moves.push(Move::over(State::arrived(from), edge, id));
                        moves.push(Move::over(State::lifted(from), edge, id));
                    }
                } else {
                    for child in held() {
                        moves.push(Move::within(State::arrived(child), child));
                        moves.push(Move::within(State::lifted(child), child));
                    }
                }
                match self.parent[id] {
                    Some(parent) if arrival && is_target(parent.0 as usize) => {
                        let parent = parent.0 as usize;
                        moves.push(Move::within(State::arrived(parent), parent));
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
    /// The node whose standing above a walk's ceiling holds the move back:
    /// the one an edge lands on, a lift leaves, or a descent leaves, read the
    /// same way whichever direction the walk runs.
    pivot: usize,
}

impl Move {
    fn over(to: usize, edge: u32, pivot: usize) -> Self {
        Move {
            to,
            over: Some(edge),
            pivot,
        }
    }

    fn within(to: usize, pivot: usize) -> Self {
        Move {
            to,
            over: None,
            pivot,
        }
    }
}

/// The fewest edges of a state no walk has reached.
const UNREACHED: u32 = u32::MAX;

/// A walk's position: a node, arrived at or only lifted through. Each node has
/// two, the arrival at `2 * id` and the lift beside it.
pub(crate) struct State;

impl State {
    pub(crate) fn arrived(id: usize) -> usize {
        2 * id
    }

    pub(crate) fn lifted(id: usize) -> usize {
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
pub(crate) struct Walk {
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

    pub(crate) fn reached(&self, id: Id) -> bool {
        let id = id.0 as usize;
        self.fewest[State::arrived(id)] != UNREACHED || self.fewest[State::lifted(id)] != UNREACHED
    }

    pub(crate) fn arrived(&self, id: Id) -> bool {
        self.fewest[State::arrived(id.0 as usize)] != UNREACHED
    }

    /// The state of `id` reached in fewest edges, `None` where neither is.
    pub(crate) fn nearest(&self, id: Id) -> Option<usize> {
        let id = id.0 as usize;
        [State::arrived(id), State::lifted(id)]
            .into_iter()
            .filter(|&state| self.fewest[state] != UNREACHED)
            .min_by_key(|&state| self.fewest[state])
    }

    /// The nodes the walk passed on its way from `start`'s subtree to `end`, a
    /// state of `finish`: `start`, the ends of each edge it took, and `finish`,
    /// each named once where consecutive.
    pub(crate) fn chain(&self, graph: &Graph, end: usize, start: Id, finish: Id) -> Vec<Id> {
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
    use crate::graph::Node;

    /// Package `0` holds targets `1` to `4`, holding configured targets `11`
    /// to `14`; `12` holds action `22` and `13` action `23`. A label edge
    /// joins `1` to `2`, configured edges `11` to `12` and `12` to `13`, and an
    /// action edge `22` to `23`. Target `4` feeds `13` by a configured edge
    /// leaving the target, as cquery states one without `--transitions`.
    fn levels() -> Graph {
        let mut graph = Graph::new();
        let nodes = [
            (0, Kind::Package, None),
            (1, Kind::Target, Some(0)),
            (2, Kind::Target, Some(0)),
            (3, Kind::Target, Some(0)),
            (4, Kind::Target, Some(0)),
            (11, Kind::ConfiguredTarget, Some(1)),
            (12, Kind::ConfiguredTarget, Some(2)),
            (13, Kind::ConfiguredTarget, Some(3)),
            (14, Kind::ConfiguredTarget, Some(4)),
            (22, Kind::Action, Some(12)),
            (23, Kind::Action, Some(13)),
        ];
        for (id, kind, parent) in nodes {
            graph.add_node(Node::new(Id(id), "n", kind, parent.map(Id)));
        }
        for (from, to, kind) in [
            (1, 2, EdgeKind::Query),
            (11, 12, EdgeKind::Cquery),
            (12, 13, EdgeKind::Cquery),
            (4, 13, EdgeKind::Cquery),
            (22, 23, EdgeKind::Aquery),
        ] {
            graph.add_edge(Id(from), Id(to), kind);
        }
        graph
    }

    /// A chain `0 -> 1 -> 2 -> 3` of targets.
    fn chain() -> Graph {
        let mut graph = Graph::new();
        for id in 0..4 {
            graph.add_node(Node::new(Id(id), "n", Kind::Target, None));
        }
        for id in 0..3 {
            graph.add_edge(Id(id), Id(id + 1), EdgeKind::Query);
        }
        graph
    }

    /// The nodes `walk` reached in either state at or below `ceiling`, frames
    /// aside, by id.
    fn covered(graph: &Graph, walk: &Walk, ceiling: Level) -> Vec<u32> {
        let index = Index::new(graph, &[]);
        let mut ids: Vec<u32> = (graph.nodes.iter())
            .filter(|node| walk.reached(node.id) && !node.kind.is_frame())
            .filter(|node| index.level[node.id.0 as usize].is_none_or(|level| level >= ceiling))
            .map(|node| node.id.0)
            .collect();
        ids.sort_unstable();
        ids
    }

    fn reached(graph: &Graph, walk: &Walk) -> Vec<u32> {
        let mut ids: Vec<u32> = (graph.nodes.iter())
            .filter(|node| walk.reached(node.id))
            .map(|node| node.id.0)
            .collect();
        ids.sort_unstable();
        ids
    }

    fn both(id: u32) -> [usize; 2] {
        [State::arrived(id as usize), State::lifted(id as usize)]
    }

    fn ceiling(ceiling: Level) -> Bounds<'static> {
        Bounds {
            ceiling,
            ..Bounds::NONE
        }
    }

    #[test]
    fn a_walk_takes_no_more_edges_than_its_depth() {
        let graph = chain();
        let index = Index::new(&graph, &[]);
        let bounded = |depth| Bounds {
            depth,
            ..Bounds::NONE
        };
        let seeds = [State::arrived(0)];
        let walk = |depth| index.walk(&seeds, Toward::Dependents, bounded(depth));
        assert_eq!(reached(&graph, &walk(Some(0))), [0]);
        assert_eq!(reached(&graph, &walk(Some(2))), [0, 1, 2]);
        assert_eq!(reached(&graph, &walk(None)), [0, 1, 2, 3]);
    }

    #[test]
    fn a_containment_move_counts_against_no_depth() {
        let graph = levels();
        let index = Index::new(&graph, &[]);
        let bounds = Bounds {
            depth: Some(1),
            ..Bounds::NONE
        };
        let walk = index.walk(&[State::arrived(1)], Toward::Dependents, bounds);
        assert_eq!(reached(&graph, &walk), [0, 1, 2, 11, 12]);
    }

    #[test]
    fn a_walk_keeps_within_its_nodes() {
        let graph = chain();
        let index = Index::new(&graph, &[]);
        let mut within = FixedBitSet::with_capacity(4);
        within.extend([0, 1, 3]);
        let bounds = Bounds {
            within: Some(&within),
            ..Bounds::NONE
        };
        let walk = index.walk(&[State::arrived(0)], Toward::Dependents, bounds);
        assert_eq!(reached(&graph, &walk), [0, 1]);
        let walk = index.walk(&[State::arrived(2)], Toward::Dependents, bounds);
        assert!(reached(&graph, &walk).is_empty(), "a seed outside");
    }

    #[test]
    fn a_walk_from_configured_targets_covers_no_target_or_label_edge() {
        let graph = levels();
        let index = Index::new(&graph, &[]);
        let configured = Level::ConfiguredTarget;
        let walk = index.walk(&both(13), Toward::Dependencies, ceiling(configured));
        assert_eq!(covered(&graph, &walk, configured), [11, 12, 13, 14, 22, 23]);
        let walk = index.walk(&both(13), Toward::Dependencies, ceiling(Level::Target));
        assert_eq!(
            covered(&graph, &walk, Level::Target),
            [1, 2, 3, 4, 11, 12, 13, 14, 22, 23]
        );

        let walk = index.walk(
            &[State::arrived(11)],
            Toward::Dependents,
            ceiling(configured),
        );
        assert_eq!(covered(&graph, &walk, configured), [11, 12, 13]);
        let walk = index.walk(
            &[State::arrived(14)],
            Toward::Dependents,
            ceiling(configured),
        );
        assert_eq!(
            covered(&graph, &walk, configured),
            [13, 14],
            "through its target"
        );
    }

    #[test]
    fn a_walk_from_actions_covers_no_configured_target() {
        let graph = levels();
        let index = Index::new(&graph, &[]);
        let action = Level::Action;
        let walk = index.walk(&[State::arrived(22)], Toward::Dependents, ceiling(action));
        assert_eq!(covered(&graph, &walk, action), [22, 23]);
        let walk = index.walk(&both(23), Toward::Dependencies, ceiling(action));
        assert_eq!(covered(&graph, &walk, action), [22, 23]);
    }

    #[test]
    fn a_file_stands_at_the_level_of_what_holds_it() {
        let file = |parent| Level::of(Kind::Artifact, parent);
        assert_eq!(file(Some(Kind::Package)), Some(Level::Target));
        assert_eq!(file(Some(Kind::ConfiguredTarget)), Some(Level::Action));
        assert_eq!(Level::of(Kind::Source, Some(Kind::Package)), None);
        let mut graph = Graph::new();
        let nodes = [
            (1, Kind::ConfiguredTarget, None),
            (2, Kind::MergeGroup, Some(1)),
            (3, Kind::Artifact, Some(2)),
        ];
        for (id, kind, parent) in nodes {
            graph.add_node(Node::new(Id(id), "n", kind, parent.map(Id)));
        }
        let index = Index::new(&graph, &[]);
        assert_eq!(
            index.level[3],
            Some(Level::Action),
            "through its merge group"
        );
        assert_eq!(Level::of(Kind::Package, None), None);
    }
}
