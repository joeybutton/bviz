//! Merges any combination of query, cquery, and aquery inputs into one compound
//! graph. Node ids are query-independent, so shared entities collapse: a target
//! present in both query and aquery, a configured target present in both cquery
//! and aquery, and a source file that is also a target all become one node. Each
//! input's edges keep their own [`EdgeKind`], so the renderer can still tell the
//! query, cquery, and aquery dependencies apart in the merged graph.
//!
//! Package boxes fold by the whole package set, so every input builds its
//! hierarchy from the union of all inputs' packages: a `pkg:` box then carries
//! one label and parent no matter which input emitted it.

use std::collections::{BTreeSet, HashSet};

use bazel_query_proto::analysis::{ActionGraphContainer, CqueryResult};
use bazel_query_proto::blaze_query::Target;

use crate::graph::Graph;
use crate::{aquery, cquery, query};

pub fn build_joint(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
    external_sources: bool,
) -> Graph {
    let mut union: BTreeSet<(String, String)> = BTreeSet::new();
    if let Some(a) = aquery {
        union.extend(aquery::package_keys(a));
    }
    if let Some(c) = cquery {
        union.extend(cquery::package_keys(c));
    }
    if let Some(q) = query {
        union.extend(query::package_keys(q));
    }

    let mut graphs: Vec<Graph> = Vec::new();
    if let Some(a) = aquery {
        graphs.push(aquery::build_with_packages(a, external_sources, &union));
    }
    if let Some(c) = cquery {
        graphs.push(cquery::build_with_packages(c, &union));
    }
    if let Some(q) = query {
        let refs: Vec<&Target> = q.iter().collect();
        graphs.push(query::build_with_packages(&refs, &union));
    }

    merge(graphs)
}

/// Unions the graphs into one: nodes deduped by id (same-id nodes carry an
/// identical kind, label, and parent by construction, so the first wins) and
/// edges deduped by `(from, to, kind)`.
fn merge(graphs: Vec<Graph>) -> Graph {
    let mut merged = Graph::new();
    let mut seen_node: HashSet<String> = HashSet::new();
    let mut seen_edge: HashSet<(String, String, crate::graph::EdgeKind)> = HashSet::new();
    for graph in graphs {
        for node in graph.nodes {
            if seen_node.insert(node.id.clone()) {
                merged.nodes.push(node);
            }
        }
        for edge in graph.edges {
            if seen_edge.insert((edge.from.clone(), edge.to.clone(), edge.kind)) {
                merged.edges.push(edge);
            }
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Node};
    use bazel_query_proto::analysis::{
        Action, Artifact, DepSetOfFiles, PathFragment, Target as ActionTarget,
    };
    use bazel_query_proto::blaze_query::{target::Discriminator, Rule, SourceFile};

    fn rule(name: &str, inputs: &[&str]) -> Target {
        Target {
            r#type: Discriminator::Rule as i32,
            rule: Some(Rule {
                name: name.to_string(),
                rule_input: inputs.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn source(name: &str) -> Target {
        Target {
            r#type: Discriminator::SourceFile as i32,
            source_file: Some(SourceFile {
                name: name.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn find<'a>(graph: &'a Graph, id: &str) -> &'a Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap()
    }

    fn edges(graph: &Graph) -> HashSet<(&str, &str, EdgeKind)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str(), e.kind))
            .collect()
    }

    #[test]
    fn shared_target_collapses_and_keeps_each_querys_edge() {
        // query: //foo:app depends on //foo:lib (a Query edge into the shared
        // target). aquery: //foo:lib (target id 1) has a Compile action reading
        // one source, an Aquery edge into the same target's subtree.
        let query_targets = vec![source("//foo:lib"), rule("//foo:app", &["//foo:lib"])];

        let aquery_container = ActionGraphContainer {
            artifacts: vec![Artifact {
                id: 1,
                path_fragment_id: 1,
                is_tree_artifact: false,
            }],
            path_fragments: vec![PathFragment {
                id: 1,
                label: "foo/in.c".to_string(),
                parent_id: 0,
            }],
            targets: vec![ActionTarget {
                id: 1,
                label: "//foo:lib".to_string(),
                ..Default::default()
            }],
            dep_set_of_files: vec![DepSetOfFiles {
                id: 10,
                direct_artifact_ids: vec![1],
                transitive_dep_set_ids: vec![],
            }],
            actions: vec![Action {
                target_id: 1,
                mnemonic: "Compile".to_string(),
                configuration_id: 1,
                input_dep_set_ids: vec![10],
                ..Default::default()
            }],
            ..Default::default()
        };

        let graph = build_joint(
            Some(&[aquery_container]),
            None,
            Some(&query_targets),
            false,
        );

        // The target //foo:lib appears once though both inputs name it.
        assert_eq!(
            graph.nodes.iter().filter(|n| n.id == "target://foo:lib").count(),
            1
        );
        // The query dependency edge and an aquery input edge both survive.
        let edges = edges(&graph);
        assert!(edges
            .iter()
            .any(|(_, _, k)| *k == EdgeKind::Query));
        assert!(edges
            .iter()
            .any(|(_, _, k)| *k == EdgeKind::Aquery));
        // The package box appears once.
        assert_eq!(graph.nodes.iter().filter(|n| n.id == "pkg:foo").count(), 1);
    }

    #[test]
    fn union_folds_packages_consistently_across_inputs() {
        // query knows a/b and a/c (so a branches); aquery knows only a/b. Built
        // from the union, aquery's lone a/b must still nest under the //a box
        // rather than fold into a top-level //a/b.
        let query_targets = vec![rule("//a/b:x", &[]), rule("//a/c:y", &[])];
        let aquery_container = ActionGraphContainer {
            targets: vec![ActionTarget {
                id: 1,
                label: "//a/b:z".to_string(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 1,
                mnemonic: "Compile".to_string(),
                configuration_id: 1,
                ..Default::default()
            }],
            ..Default::default()
        };

        let graph = build_joint(
            Some(&[aquery_container]),
            None,
            Some(&query_targets),
            false,
        );

        assert_eq!(find(&graph, "pkg:a").label, "//a");
        let nested = find(&graph, "pkg:a/b");
        assert_eq!(nested.label, "b");
        assert_eq!(nested.parent.as_deref(), Some("pkg:a"));
    }
}
