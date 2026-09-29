//! The configuration transitions a graph's dependency edges cross.
//!
//! Only an edge joining two configured targets states both configurations, so
//! only such an edge — a cquery run with `--transitions=lite` or `full` writes
//! them — can be seen to cross one.

use rustc_hash::FxHashMap;

use crate::facets::{Facets, Tri};
use crate::graph::{Graph, Kind, id_span};
use crate::intern::Id;
use crate::layout::Connector;

/// A change of configuration from a dependent to its dependency.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Transition {
    /// The dependency is built in a configuration for a tool the build runs.
    Exec,
    Other,
}

impl Transition {
    /// Every transition, in the order a legend lists them.
    pub const ALL: [Transition; 2] = [Transition::Exec, Transition::Other];

    pub fn label(self) -> &'static str {
        match self {
            Transition::Exec => "exec transition",
            Transition::Other => "other transition",
        }
    }
}

/// The transitions one graph's edges cross, by the node pair each edge joins.
#[derive(Clone, Default)]
pub struct Transitions(FxHashMap<(Id, Id), Transition>);

impl Transitions {
    /// The transitions `graph`'s edges cross, read off the configurations
    /// `facets` records.
    pub fn of(graph: &Graph, facets: &Facets) -> Self {
        let mut configured: Vec<bool> = vec![false; id_span(graph)];
        for node in &graph.nodes {
            configured[node.id.0 as usize] = node.kind == Kind::ConfiguredTarget;
        }
        let crossed = graph.edges.iter().filter_map(|edge| {
            if !(configured[edge.from.0 as usize] && configured[edge.to.0 as usize]) {
                return None;
            }
            let (dependency, dependent) = (facets.config(edge.from)?, facets.config(edge.to)?);
            if dependency == dependent {
                return None;
            }
            let tool = facets
                .config_entry(dependency)
                .is_some_and(|entry| entry.is_tool == Tri::Yes);
            let transition = match tool {
                true => Transition::Exec,
                false => Transition::Other,
            };
            Some(((edge.from, edge.to), transition))
        });
        Transitions(crossed.collect())
    }

    /// The transition the edge from `from` to `to` crosses, `None` where it
    /// crosses none or the graph states no such edge.
    pub fn get(&self, from: Id, to: Id) -> Option<Transition> {
        self.0.get(&(from, to)).copied()
    }

    /// States on each of `connectors` the transition the edges drawn along it
    /// cross, the exec transition where several are crossed. `drawn` names the
    /// pair of boxes an edge of the graph these were found in is drawn between,
    /// `None` for an edge the drawing folds away.
    pub fn mark(&self, connectors: &mut [Connector], drawn: impl Fn(Id, Id) -> Option<(Id, Id)>) {
        let mut crossed: FxHashMap<(Id, Id), Transition> =
            FxHashMap::with_capacity_and_hasher(self.0.len(), Default::default());
        for (&(from, to), &transition) in &self.0 {
            if let Some(pair) = drawn(from, to) {
                let held = crossed.entry(pair).or_insert(transition);
                if transition == Transition::Exec {
                    *held = transition;
                }
            }
        }
        for connector in connectors {
            connector.transition = crossed.get(&(connector.from, connector.to)).copied();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Node};

    /// Configured targets `0` and `1` in the target configuration, `2` in the
    /// exec one and `3` in a third; target `4` and configured target `5` state
    /// no configuration.
    fn fixture() -> (Graph, Facets) {
        let mut graph = Graph::new();
        let mut facets = Facets::default();
        let target = facets.intern_config("ck1", Some("k8-fastbuild"), Tri::No);
        let exec = facets.intern_config("ck2", Some("k8-opt-exec"), Tri::Yes);
        let other = facets.intern_config("ck3", Some("k8-fastbuild-ST"), Tri::No);
        let configs = [Some(target), Some(target), Some(exec), Some(other)];
        for (id, config) in configs.into_iter().enumerate() {
            let id = Id(id as u32);
            graph.add_node(Node::new(id, "ct", Kind::ConfiguredTarget, None));
            facets.set_config(id, config);
        }
        graph.add_node(Node::new(Id(4), "t", Kind::Target, None));
        facets.set_config(Id(4), Some(exec));
        graph.add_node(Node::new(Id(5), "ct", Kind::ConfiguredTarget, None));
        for (from, to) in [(0, 1), (2, 0), (3, 0), (4, 0), (5, 0), (0, 2)] {
            graph.add_edge(Id(from), Id(to), EdgeKind::Cquery);
        }
        (graph, facets)
    }

    #[test]
    fn an_edge_within_one_configuration_crosses_nothing() {
        let (graph, facets) = fixture();
        assert_eq!(Transitions::of(&graph, &facets).get(Id(0), Id(1)), None);
    }

    #[test]
    fn a_dependency_built_as_a_tool_is_an_exec_transition() {
        let (graph, facets) = fixture();
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(2), Id(0)), Some(Transition::Exec));
        assert_eq!(transitions.get(Id(3), Id(0)), Some(Transition::Other));
    }

    /// Which configuration the dependency is built in decides the kind, so a
    /// tool depending on a target-configured node is no exec transition.
    #[test]
    fn the_dependency_decides_the_kind() {
        let (graph, facets) = fixture();
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(0), Id(2)), Some(Transition::Other));
    }

    fn connector(from: u32, to: u32) -> Connector {
        Connector {
            points: Vec::new(),
            kinds: vec![EdgeKind::Cquery],
            from: Id(from),
            to: Id(to),
            transition: None,
        }
    }

    /// Nodes `2` and `3` folded into box `20`, `0` drawn as `10` and `1` as
    /// `11`, and the rest folded away.
    #[test]
    fn a_connector_carries_the_transitions_folded_into_it() {
        let (graph, facets) = fixture();
        let drawn = |from: Id, to: Id| {
            let box_of = |id: Id| match id.0 {
                0 => Some(Id(10)),
                1 => Some(Id(11)),
                2 | 3 => Some(Id(20)),
                _ => None,
            };
            Some((box_of(from)?, box_of(to)?))
        };
        let mut connectors = [connector(20, 10), connector(10, 20), connector(10, 11)];
        Transitions::of(&graph, &facets).mark(&mut connectors, drawn);
        let marked: Vec<Option<Transition>> = connectors.iter().map(|c| c.transition).collect();
        assert_eq!(
            marked,
            [Some(Transition::Exec), Some(Transition::Other), None],
            "an exec transition and another folded together, one other, none"
        );
    }

    /// An action or an artifact takes the configuration of the configured
    /// target holding it, and an edge between two of them joins steps inside
    /// the build of their targets rather than a dependency one target takes on
    /// another, so it crosses no transition however the two are configured.
    #[test]
    fn an_edge_between_actions_and_artifacts_crosses_nothing() {
        let (mut graph, mut facets) = fixture();
        let exec = facets.config(Id(2)).expect("the exec configuration");
        let target = facets.config(Id(0)).expect("the target configuration");
        graph.add_node(Node::new(Id(6), "tool.jar", Kind::Artifact, Some(Id(2))));
        graph.add_node(Node::new(Id(7), "Javac", Kind::Action, Some(Id(0))));
        facets.set_config(Id(6), Some(exec));
        facets.set_config(Id(7), Some(target));
        graph.add_edge(Id(6), Id(7), EdgeKind::Aquery);
        graph.add_edge(Id(6), Id(0), EdgeKind::Aquery);
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(6), Id(7)), None, "artifact to action");
        assert_eq!(transitions.get(Id(6), Id(0)), None, "artifact to target");
    }

    #[test]
    fn an_end_stating_no_configuration_crosses_nothing() {
        let (graph, facets) = fixture();
        let transitions = Transitions::of(&graph, &facets);
        assert_eq!(transitions.get(Id(4), Id(0)), None, "a target");
        assert_eq!(transitions.get(Id(5), Id(0)), None, "no configuration");
    }
}
