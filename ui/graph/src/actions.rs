//! A graph's actions as the invocation's other outputs read them.

use bviz_graph_layout::{ActionDependencies, Graph, Names, Resolver, action_dependencies};
use bviz_invocation::ActionGraph;

/// The actions of `graph`, which `names` names, and which take an input from
/// which; `None` for a graph holding no action, as one read from no aquery.
pub(crate) fn action_graph(graph: &Graph, names: &Names) -> Option<ActionGraph> {
    let ActionDependencies {
        actions,
        dependencies,
    } = action_dependencies(graph);
    if actions.is_empty() {
        return None;
    }
    let resolver = Resolver::new(names);
    let primary_outputs = actions
        .iter()
        .map(|&action| resolver.primary_output(action).map(str::to_owned))
        .collect();
    Some(ActionGraph::new(primary_outputs, dependencies))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bviz_graph_layout::graph::Node;
    use bviz_graph_layout::{EdgeKind, Kind};

    #[test]
    fn an_action_takes_an_input_from_the_action_writing_it() {
        let mut names = Names::default();
        let mut graph = Graph::new();
        let mut node = |name: &str, kind| {
            let id = names.intern(name);
            graph.add_node(Node::new(id, name, kind, None));
            id
        };
        let compile = node("action:bazel-out/a.o", Kind::Action);
        let object = node("artifact1", Kind::Artifact);
        let source = node("target://a:a.c", Kind::Source);
        let link = node("action:bazel-out/a", Kind::Action);
        let unnamed = node("action7", Kind::Action);
        for (from, to) in [
            (source, compile),
            (compile, object),
            (object, link),
            (object, unnamed),
        ] {
            graph.add_edge(from, to, EdgeKind::Aquery);
        }
        graph.add_edge(compile, link, EdgeKind::Query);

        let actions = action_graph(&graph, &names).expect("a graph of actions");
        let outputs: Vec<Option<&str>> = actions
            .actions()
            .map(|action| actions.primary_output(action))
            .collect();
        assert_eq!(outputs, [Some("bazel-out/a.o"), Some("bazel-out/a"), None]);
        let [compile, link, unnamed] = actions.actions().collect::<Vec<_>>()[..] else {
            panic!("three actions");
        };
        assert_eq!(actions.dependencies(compile), []);
        assert_eq!(actions.dependencies(link), [compile], "not by a query edge");
        assert_eq!(actions.dependencies(unnamed), [compile]);
    }

    #[test]
    fn a_graph_of_no_action_has_no_action_graph() {
        let mut names = Names::default();
        let mut graph = Graph::new();
        let id = names.intern("target://a:a");
        graph.add_node(Node::new(id, "//a:a", Kind::Target, None));
        assert_eq!(action_graph(&graph, &names), None);
    }
}
