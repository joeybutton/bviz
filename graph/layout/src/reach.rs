//! Walks over a graph's edges and containment together. An edge leaving a
//! container leaves each node inside it, and an edge landing on a target lands
//! on each of its configured targets, which is what joins the label-level edges
//! a query states to the configured edges a cquery states.

use std::collections::VecDeque;

use crate::graph::{EdgeKind, Graph, Kind, id_span};
use crate::intern::Id;

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
    pub(crate) fn walk(&self, seeds: &[usize], toward: Toward) -> Walk {
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
