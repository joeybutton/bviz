//! Which actions of a graph take an input from which.

use crate::graph::{EdgeKind, Graph, Kind, id_span};
use crate::intern::Id;

/// The actions of a graph and which take an input from which.
#[derive(Debug, PartialEq, Eq)]
pub struct ActionDependencies {
    /// In the order the graph holds them.
    pub actions: Vec<Id>,
    /// Each `(dependent, dependency)` pair of positions in `actions` where
    /// the dependent reads an output the dependency writes, once for every
    /// such output.
    pub dependencies: Vec<(usize, usize)>,
}

/// The [`ActionDependencies`] of `graph`, through its aquery edges.
pub fn action_dependencies(graph: &Graph) -> ActionDependencies {
    let span = id_span(graph);
    let mut position: Vec<Option<usize>> = vec![None; span];
    let mut actions = Vec::new();
    for node in graph.nodes.iter().filter(|node| node.kind == Kind::Action) {
        position[node.id.0 as usize] = Some(actions.len());
        actions.push(node.id);
    }
    let action_at = |id: Id| position[id.0 as usize];
    let aquery = || {
        graph
            .edges
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Aquery)
    };
    let mut producer: Vec<Option<usize>> = vec![None; span];
    for edge in aquery() {
        if let Some(writer) = action_at(edge.from) {
            producer[edge.to.0 as usize] = Some(writer);
        }
    }
    let reads = aquery().filter_map(|edge| Some((action_at(edge.to)?, edge.from)));
    let mut dependencies = Vec::with_capacity(reads.clone().count());
    for (reader, input) in reads {
        if let Some(writer) = producer[input.0 as usize] {
            dependencies.push((reader, writer));
        }
    }
    ActionDependencies {
        actions,
        dependencies,
    }
}
