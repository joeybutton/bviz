use crate::graph::{Graph, Node};
use crate::intern::Names;
use std::collections::HashSet;

pub(crate) fn find<'a>(graph: &'a Graph, names: &Names, id: &str) -> &'a Node {
    graph
        .nodes
        .iter()
        .find(|n| names.resolve(n.id) == id)
        .unwrap()
}

pub(crate) fn parent_of<'a>(graph: &Graph, names: &'a Names, id: &str) -> Option<&'a str> {
    find(graph, names, id).parent.map(|p| names.resolve(p))
}

pub(crate) fn edges<'a>(graph: &Graph, names: &'a Names) -> HashSet<(&'a str, &'a str)> {
    graph
        .edges
        .iter()
        .map(|e| (names.resolve(e.from), names.resolve(e.to)))
        .collect()
}
