//! Which of a joint graph's inputs make up the graph a view draws and walks.

use fixedbitset::FixedBitSet;

use crate::graph::{Edge, Graph, Node, id_span};
use crate::intern::Id;
use crate::provenance::{Input, InputSet, Provenance};

/// What a view takes of a joint graph: the inputs whose nodes and edges it
/// keeps, of those the graph was read from. At least one input is always on,
/// since [`InputSet`] is never empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sources {
    loaded: InputSet,
    inputs: InputSet,
}

impl Sources {
    /// Every input of `loaded` on.
    pub fn reading(loaded: InputSet) -> Self {
        Sources {
            loaded,
            inputs: loaded,
        }
    }

    /// The inputs the graph was read from, which are all these can turn on.
    pub fn loaded(self) -> InputSet {
        self.loaded
    }

    /// The inputs on.
    pub fn inputs(self) -> InputSet {
        self.inputs
    }

    /// Whether every input the graph was read from is on.
    pub fn whole(self) -> bool {
        self.inputs == self.loaded
    }

    /// These sources with `input` turned on or off; absent where that turns
    /// on an input the graph was not read from, or off the last input on.
    pub fn with_input(self, input: Input, on: bool) -> Option<Self> {
        let inputs = match on {
            true => Some(self.inputs.with(input)).filter(|_| self.loaded.contains(input)),
            false => self.inputs.without(input),
        };
        inputs.map(|inputs| Sources { inputs, ..self })
    }

    /// `graph` as these sources leave it, `provenance` saying which inputs
    /// built each of its nodes. A node stays where any input that built it is
    /// on, and a frame where it still frames a node that stays; an edge stays
    /// where the input it was read from is on and both its ends stay. A node
    /// whose parent goes is held by its nearest ancestor that stays.
    ///
    /// A [`Transitive`](crate::graph::EdgeKind::Transitive) edge stands for a
    /// path through nodes this may drop, so none stays.
    pub fn mask(self, graph: &Graph, provenance: &Provenance) -> Graph {
        let span = id_span(graph);
        let mut parent: Vec<Option<Id>> = vec![None; span];
        let mut is_frame: Vec<bool> = vec![false; span];
        let mut kept: Vec<bool> = vec![false; span];
        for node in &graph.nodes {
            let id = node.id.0 as usize;
            parent[id] = node.parent;
            is_frame[id] = node.kind.is_frame();
            kept[id] = !is_frame[id]
                && (provenance.of(node.id)).is_some_and(|inputs| inputs.intersects(self.inputs));
        }
        for node in &graph.nodes {
            let id = node.id.0 as usize;
            if !kept[id] || is_frame[id] {
                continue;
            }
            // Each frame is kept once and each walk stops at the first frame
            // already kept, so the whole forest costs one step per node.
            let mut at = node.parent;
            while let Some(ancestor) = at {
                let id = ancestor.0 as usize;
                if is_frame[id] {
                    if kept[id] {
                        break;
                    }
                    kept[id] = true;
                }
                at = parent[id];
            }
        }

        let mut masked = Graph::new();
        masked.nodes.reserve(graph.nodes.len());
        for node in graph.nodes.iter().filter(|node| kept[node.id.0 as usize]) {
            let held_by = std::iter::successors(node.parent, |&at| parent[at.0 as usize])
                .find(|&at| kept[at.0 as usize]);
            masked.add_node(Node {
                parent: held_by,
                ..node.clone()
            });
        }
        masked.edges.reserve(graph.edges.len());
        masked.edges.extend(
            (graph.edges.iter())
                .filter(|edge| self.keeps_edge(edge))
                .filter(|edge| kept[edge.from.0 as usize] && kept[edge.to.0 as usize])
                .cloned(),
        );
        masked
    }

    /// The edges of `edges` these sources keep between the nodes of `masked`,
    /// a graph [`Self::mask`] left, as it keeps the graph's own.
    pub fn mask_edges(self, edges: &[Edge], masked: &Graph) -> Vec<Edge> {
        let mut held = FixedBitSet::with_capacity(id_span(masked));
        held.extend(masked.nodes.iter().map(|node| node.id.0 as usize));
        (edges.iter())
            .filter(|edge| self.keeps_edge(edge))
            .filter(|edge| held.contains(edge.from.0 as usize) && held.contains(edge.to.0 as usize))
            .cloned()
            .collect()
    }

    /// Whether these sources keep `edge`, whatever they keep of its ends.
    fn keeps_edge(self, edge: &Edge) -> bool {
        edge.kind.input().is_some_and(|i| self.inputs.contains(i))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Kind};

    /// A package holding a target query and aquery both built, which holds a
    /// configured target cquery and aquery built, which holds an action aquery
    /// alone built; a second target query alone built depending on the first;
    /// and a package framing only an action aquery built.
    fn joint() -> (Graph, Provenance) {
        let query = InputSet::from(Input::Query);
        let aquery = InputSet::from(Input::Aquery);
        let nodes = [
            (0, Kind::Package, None, InputSet::ALL),
            (1, Kind::Target, Some(0), query.with(Input::Aquery)),
            (
                2,
                Kind::ConfiguredTarget,
                Some(1),
                aquery.with(Input::Cquery),
            ),
            (3, Kind::Action, Some(2), aquery),
            (4, Kind::Target, Some(0), query),
            (5, Kind::Package, None, InputSet::ALL),
            (6, Kind::Action, Some(5), aquery),
        ];
        let mut graph = Graph::new();
        let mut built = Vec::with_capacity(nodes.len());
        for (id, kind, parent, inputs) in nodes {
            graph.add_node(Node::new(Id(id), "n", kind, parent.map(Id)));
            built.push(inputs);
        }
        graph.add_edge(Id(1), Id(4), EdgeKind::Query);
        graph.add_edge(Id(3), Id(6), EdgeKind::Aquery);
        graph.add_edge(Id(2), Id(2), EdgeKind::Cquery);
        graph.add_edge(Id(1), Id(4), EdgeKind::Transitive);
        (graph, Provenance::new(built))
    }

    fn sources(inputs: &[Input]) -> Sources {
        Sources::reading(InputSet::of(inputs.iter().copied()).unwrap())
    }

    fn ids(graph: &Graph) -> Vec<u32> {
        graph.nodes.iter().map(|node| node.id.0).collect()
    }

    fn kinds(graph: &Graph) -> Vec<EdgeKind> {
        graph.edges.iter().map(|edge| edge.kind).collect()
    }

    #[test]
    fn every_input_on_keeps_every_node_and_every_read_edge() {
        let (graph, provenance) = joint();
        let masked = Sources::reading(InputSet::ALL).mask(&graph, &provenance);

        assert_eq!(ids(&masked), ids(&graph));
        assert_eq!(
            kinds(&masked),
            [EdgeKind::Query, EdgeKind::Aquery, EdgeKind::Cquery]
        );
    }

    #[test]
    fn a_node_stays_while_any_input_that_built_it_is_on() {
        let (graph, provenance) = joint();
        let masked = sources(&[Input::Query]).mask(&graph, &provenance);

        assert_eq!(ids(&masked), [0, 1, 4]);
        assert_eq!(kinds(&masked), [EdgeKind::Query]);
    }

    #[test]
    fn a_frame_left_framing_nothing_goes() {
        let (graph, provenance) = joint();
        let masked = sources(&[Input::Cquery]).mask(&graph, &provenance);

        assert_eq!(ids(&masked), [0, 2]);
        assert_eq!(kinds(&masked), [EdgeKind::Cquery]);
    }

    #[test]
    fn a_node_whose_parent_goes_is_held_by_the_nearest_ancestor_that_stays() {
        let (graph, provenance) = joint();
        let masked = sources(&[Input::Cquery]).mask(&graph, &provenance);

        let configured = masked.nodes.iter().find(|node| node.id == Id(2)).unwrap();
        assert_eq!(configured.parent, Some(Id(0)));
    }

    #[test]
    fn an_edge_goes_with_its_input_though_both_ends_stay() {
        let (graph, provenance) = joint();
        let masked = sources(&[Input::Cquery, Input::Aquery]).mask(&graph, &provenance);

        assert!(ids(&masked).contains(&1) && ids(&masked).contains(&2));
        assert_eq!(kinds(&masked), [EdgeKind::Aquery, EdgeKind::Cquery]);
    }

    #[test]
    fn the_last_input_on_cannot_be_turned_off() {
        let loaded = sources(&[Input::Query, Input::Aquery]);
        let one = loaded.with_input(Input::Query, false).unwrap();
        assert_eq!(one.inputs(), Input::Aquery.into());
        assert_eq!(one.with_input(Input::Aquery, false), None);

        assert_eq!(one.with_input(Input::Query, true), Some(loaded));
        assert!(loaded.whole() && !one.whole());
    }

    #[test]
    fn an_input_the_graph_was_not_read_from_cannot_be_turned_on() {
        let query = sources(&[Input::Query]);
        assert_eq!(query.with_input(Input::Cquery, true), None);
        assert_eq!(query.with_input(Input::Query, true), Some(query));
    }
}
