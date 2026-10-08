//! Which of a joint graph's inputs and dependencies make up the graph a view
//! draws and walks.

use fixedbitset::FixedBitSet;

use crate::capability::{Axis, Capabilities, Support};
use crate::facets::{Facets, Tri};
use crate::graph::{Declared, Edge, Graph, Kind, Node, id_span};
use crate::intern::Id;
use crate::provenance::{Input, InputSet, Provenance};

/// A kind of dependency a view may leave out, as a Bazel query flag does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Deps {
    /// Those no attribute the dependent's BUILD file sets names.
    Implicit,
    /// Those crossing between the configurations tools are built in and the
    /// others. Only cquery tells them, since only it states configurations,
    /// so they are left out only while cquery is read, and a query leaving
    /// them out is Bazel's only as a cquery.
    Tool,
}

impl Deps {
    pub const ALL: [Deps; 2] = [Deps::Implicit, Deps::Tool];

    /// The Bazel query flag leaving these dependencies out.
    pub fn flag(self) -> &'static str {
        match self {
            Deps::Implicit => "--noimplicit_deps",
            Deps::Tool => "--notool_deps",
        }
    }
}

/// What a view takes of a joint graph: the inputs whose nodes and edges it
/// keeps, of those the graph was read from, and the kinds of [`Deps`] whose
/// edges it keeps. At least one input is always on, since [`InputSet`] is
/// never empty, and a kind of dependency is left out only while an input on
/// tells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Sources {
    loaded: InputSet,
    inputs: InputSet,
    /// Of the inputs loaded, those telling implicit dependencies, then those
    /// telling tool ones.
    telling: [Option<InputSet>; 2],
    /// Whether the reader chose to keep each kind of dependency, by its
    /// position in [`Deps::ALL`]; a choice takes effect only while an input on
    /// tells that kind.
    chosen: [bool; 2],
}

impl Sources {
    /// Every input of a graph `capabilities` counts on, and every dependency
    /// kept.
    pub fn reading(capabilities: &Capabilities) -> Self {
        let loaded = capabilities.inputs();
        let telling = |axis: Axis, inputs: &[Input]| {
            let told = (inputs.iter().copied())
                .filter(|&input| loaded.contains(input))
                .filter(|&input| {
                    let support = capabilities.reading(input.into()).support(axis);
                    !matches!(support, Support::Unavailable { .. })
                });
            InputSet::of(told)
        };
        Sources {
            loaded,
            inputs: loaded,
            telling: [
                telling(Axis::DependencyClass, &[Input::Query, Input::Cquery]),
                telling(Axis::ExecConfiguration, &[Input::Cquery]),
            ],
            chosen: [true; 2],
        }
    }

    /// Whether an input on tells `deps` apart from the rest.
    pub fn tells(self, deps: Deps) -> bool {
        self.telling[deps as usize].is_some_and(|telling| telling.intersects(self.inputs))
    }

    /// Whether the edges of `deps` are kept.
    pub fn keeps(self, deps: Deps) -> bool {
        self.chosen[deps as usize] || !self.tells(deps)
    }

    /// These sources keeping the edges of `deps` or leaving them out; absent
    /// where no input on tells them.
    pub fn with_deps(self, deps: Deps, keep: bool) -> Option<Self> {
        let mut chosen = self.chosen;
        chosen[deps as usize] = keep;
        self.tells(deps).then_some(Sources { chosen, ..self })
    }

    /// The inputs the graph was read from, which are all these can turn on.
    pub fn loaded(self) -> InputSet {
        self.loaded
    }

    /// The inputs on.
    pub fn inputs(self) -> InputSet {
        self.inputs
    }

    /// Whether these sources leave the graph whole: every input it was read
    /// from on, and every dependency kept.
    pub fn whole(self) -> bool {
        self.inputs == self.loaded && Deps::ALL.into_iter().all(|deps| self.keeps(deps))
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

    /// Whether these sources keep `edge`, by the input it was read from and
    /// the kind of dependency it is, whatever they keep of its ends.
    fn keeps_edge(self, edge: &Edge) -> bool {
        let read = edge.kind.input().is_some_and(|i| self.inputs.contains(i));
        let implicit = edge.class.declaration() == Declared::Implicit;
        let tool = edge.class.is_tool(self.inputs);
        read && (self.keeps(Deps::Implicit) || !implicit) && (self.keeps(Deps::Tool) || !tool)
    }

    /// The configured targets of `graph` built in a tool configuration, as
    /// `facets` states them, where these sources leave tool dependencies out
    /// and a walk keeps to one side of the boundary between tool
    /// configurations and the rest.
    pub fn tools(self, graph: &Graph, facets: &Facets) -> Option<FixedBitSet> {
        if self.keeps(Deps::Tool) {
            return None;
        }
        let mut tools = FixedBitSet::with_capacity(id_span(graph));
        let configured = (graph.nodes.iter()).filter(|node| node.kind == Kind::ConfiguredTarget);
        tools.extend(
            configured
                .map(|node| node.id)
                .filter(|&id| {
                    let entry = facets
                        .config(id)
                        .and_then(|config| facets.config_entry(config));
                    entry.is_some_and(|entry| entry.is_tool == Tri::Yes)
                })
                .map(|id| id.0 as usize),
        );
        Some(tools)
    }

    /// `graph` as these sources leave it, `provenance` saying which inputs
    /// built each of its nodes. A node stays where any input that built it is
    /// on, and a frame where it still frames a node that stays; an edge stays
    /// where the input it was read from is on, the kind of dependency it is
    /// kept, and both its ends stay. A node whose parent goes is held by its
    /// nearest ancestor that stays.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::CapabilitiesBuilder;
    use crate::graph::EdgeKind;

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

    /// Every input of `loaded` on, each describing everything it may.
    fn reading(loaded: InputSet) -> Sources {
        Sources::reading(&CapabilitiesBuilder::default().seal(loaded))
    }

    fn sources(inputs: &[Input]) -> Sources {
        reading(InputSet::of(inputs.iter().copied()).unwrap())
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
        let masked = reading(InputSet::ALL).mask(&graph, &provenance);

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

    /// A query edge of each declaration and a cquery tool edge, between
    /// nodes query and cquery both built, beside a query edge cquery states
    /// only tool edges for.
    fn classified() -> (Graph, Provenance) {
        use crate::graph::DepClass;
        let mut graph = Graph::new();
        for id in 0..4 {
            graph.add_node(Node::new(Id(id), "n", Kind::Target, None));
        }
        let explicit = DepClass::new(Declared::Explicit);
        let implicit = DepClass::new(Declared::Implicit);
        let unknown = DepClass::new(Declared::Unknown);
        graph.add_classified_edge(Id(0), Id(1), EdgeKind::Query, explicit);
        graph.add_classified_edge(Id(1), Id(2), EdgeKind::Query, implicit);
        graph.add_classified_edge(Id(2), Id(3), EdgeKind::Query, unknown);
        graph.add_classified_edge(Id(0), Id(3), EdgeKind::Query, explicit.tool_in_cquery());
        graph.add_classified_edge(Id(3), Id(0), EdgeKind::Cquery, implicit.tool());
        let both = InputSet::of([Input::Query, Input::Cquery]).unwrap();
        (graph, Provenance::new(vec![both; 4]))
    }

    fn ends(graph: &Graph) -> Vec<(u32, u32)> {
        graph.edges.iter().map(|e| (e.from.0, e.to.0)).collect()
    }

    #[test]
    fn leaving_implicit_dependencies_out_drops_their_edges_alone() {
        let (graph, provenance) = classified();
        let every = sources(&[Input::Query, Input::Cquery]);
        let explicit = every.with_deps(Deps::Implicit, false).unwrap();

        let masked = explicit.mask(&graph, &provenance);
        assert_eq!(ends(&masked), [(0, 1), (2, 3), (0, 3)]);
        assert_eq!(ids(&masked), ids(&graph), "every node stays");
        assert!(!explicit.whole() && !explicit.keeps(Deps::Implicit));
    }

    #[test]
    fn leaving_tool_dependencies_out_drops_cquerys_and_the_target_edges_it_derives() {
        let (graph, provenance) = classified();
        let every = sources(&[Input::Query, Input::Cquery]);
        let targets = every.with_deps(Deps::Tool, false).unwrap();

        assert_eq!(
            ends(&targets.mask(&graph, &provenance)),
            [(0, 1), (1, 2), (2, 3)]
        );
    }

    /// Tool dependencies are kept while cquery is off, and left out again
    /// when it comes back.
    #[test]
    fn tool_dependencies_are_left_out_only_while_cquery_is_on() {
        let every = sources(&[Input::Query, Input::Cquery]);
        let query = every.with_input(Input::Cquery, false).unwrap();
        assert_eq!(query.with_deps(Deps::Tool, false), None);

        let targets = every.with_deps(Deps::Tool, false).unwrap();
        let without_cquery = targets.with_input(Input::Cquery, false).unwrap();
        assert!(without_cquery.keeps(Deps::Tool));
        let again = without_cquery.with_input(Input::Cquery, true).unwrap();
        assert_eq!(again, targets);
    }

    /// Implicit dependencies are kept while no input on states the
    /// attributes that tell them, whatever the reader chose.
    #[test]
    fn implicit_dependencies_are_left_out_only_while_an_input_on_tells_them() {
        let mut capabilities = CapabilitiesBuilder::default();
        capabilities.observe(Input::Query, Axis::DependencyClass, false);
        capabilities.observe(Input::Cquery, Axis::DependencyClass, true);
        let loaded = InputSet::of([Input::Query, Input::Cquery]).unwrap();
        let every = Sources::reading(&capabilities.seal(loaded));
        let explicit = every.with_deps(Deps::Implicit, false).unwrap();
        assert!(!explicit.keeps(Deps::Implicit));

        let query = explicit.with_input(Input::Cquery, false).unwrap();
        assert!(!query.tells(Deps::Implicit) && query.keeps(Deps::Implicit));
        assert_eq!(query.with_deps(Deps::Implicit, true), None);
        let again = query.with_input(Input::Cquery, true).unwrap();
        assert!(!again.keeps(Deps::Implicit));
    }
}
