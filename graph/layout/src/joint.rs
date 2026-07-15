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
use crate::intern::{Id, Names};
use crate::{aquery, cquery, query};

pub fn build_joint(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
    external_sources: bool,
) -> (Graph, Names) {
    let union = package_union(aquery, cquery, query);
    let labeled_outputs = labeled_outputs(cquery, query);
    let mut names = Names::default();
    let graphs = build_inputs(
        aquery,
        cquery,
        query,
        external_sources,
        &union,
        &labeled_outputs,
        &mut names,
    );
    canonicalize(merge(graphs), &names)
}

fn package_union(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
) -> BTreeSet<(String, String)> {
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
    union
}

/// Labels of generated files that query or cquery knows as targets, so aquery
/// promotes the matching output artifacts to merge with those target nodes.
fn labeled_outputs(cquery: Option<&[CqueryResult]>, query: Option<&[Target]>) -> HashSet<String> {
    let mut labeled_outputs: HashSet<String> = HashSet::new();
    if let Some(c) = cquery {
        labeled_outputs.extend(cquery::generated_labels(c));
    }
    if let Some(q) = query {
        labeled_outputs.extend(query::generated_labels(q));
    }
    labeled_outputs
}

fn build_inputs(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
    external_sources: bool,
    union: &BTreeSet<(String, String)>,
    labeled_outputs: &HashSet<String>,
    names: &mut Names,
) -> Vec<Graph> {
    let mut graphs: Vec<Graph> = Vec::new();
    if let Some(a) = aquery {
        graphs.push(aquery::build_with_packages(
            a,
            external_sources,
            union,
            labeled_outputs,
            names,
        ));
    }
    if let Some(c) = cquery {
        graphs.push(cquery::build_with_packages(c, union, names));
    }
    if let Some(q) = query {
        let refs: Vec<&Target> = q.iter().collect();
        graphs.push(query::build_with_packages(&refs, union, names));
    }
    graphs
}

/// Re-numbers a graph's ids into lexicographic order of their names, so a graph
/// built from differently ordered inputs gets identical ids. Bazel's aquery proto
/// output is not order-stable, and the level-of-detail pass breaks reveal-count
/// ties between candidate containers by id, so without a name-ordered id the
/// startup view would vary run to run. Each node also then sits at the position of
/// its own id (`nodes[i].id == Id(i)`), the dense indexing that pass relies on.
fn canonicalize(graph: Graph, names: &Names) -> (Graph, Names) {
    let mut order: Vec<u32> = (0..names.len() as u32).collect();
    order.sort_by(|&a, &b| names.resolve(Id(a)).cmp(names.resolve(Id(b))));
    let mut remap = vec![Id(0); names.len()];
    let mut canon = Names::default();
    for &old in &order {
        remap[old as usize] = canon.intern(names.resolve(Id(old)));
    }
    let map = |id: Id| remap[id.0 as usize];

    let mut by_id: Vec<Option<&crate::graph::Node>> = vec![None; names.len()];
    for node in &graph.nodes {
        by_id[node.id.0 as usize] = Some(node);
    }
    let mut out = Graph::new();
    for &old in &order {
        let node = by_id[old as usize].expect("every id names a node");
        out.add_node(map(node.id), node.label.clone(), node.kind, node.parent.map(map));
    }
    for edge in &graph.edges {
        out.add_edge(map(edge.from), map(edge.to), edge.kind);
    }
    (out, canon)
}

/// Unions the graphs into one. Nodes are deduped by id: a package, target,
/// configured target, or source that several inputs name appears in each input's
/// graph with an identical kind, label, and parent, so the first wins. Edges are
/// concatenated without deduping: every builder already drops its own duplicate
/// edges, and each tags its edges with its own [`EdgeKind`], so no edge from one
/// input can equal one from another.
fn merge(graphs: Vec<Graph>) -> Graph {
    let mut merged = Graph::new();
    let mut seen_node: HashSet<Id> = HashSet::new();
    for graph in graphs {
        for node in graph.nodes {
            if seen_node.insert(node.id) {
                merged.nodes.push(node);
            }
        }
        merged.edges.extend(graph.edges);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::EdgeKind;
    use crate::test_support::find;
    use bazel_query_proto::analysis::{
        Action, Artifact, DepSetOfFiles, PathFragment, Target as ActionTarget,
    };
    use bazel_query_proto::analysis::Configuration;
    use bazel_query_proto::blaze_query::{target::Discriminator, GeneratedFile, Rule, SourceFile};

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

    fn edges<'a>(graph: &Graph, names: &'a Names) -> HashSet<(&'a str, &'a str, EdgeKind)> {
        graph
            .edges
            .iter()
            .map(|e| (names.resolve(e.from), names.resolve(e.to), e.kind))
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

        let (graph, names) = build_joint(
            Some(&[aquery_container]),
            None,
            Some(&query_targets),
            false,
        );

        // The target //foo:lib appears once though both inputs name it.
        assert_eq!(
            graph.nodes.iter().filter(|n| names.resolve(n.id) == "target://foo:lib").count(),
            1
        );
        // The query dependency edge and an aquery input edge both survive.
        let edges = edges(&graph, &names);
        assert!(edges
            .iter()
            .any(|(_, _, k)| *k == EdgeKind::Query));
        assert!(edges
            .iter()
            .any(|(_, _, k)| *k == EdgeKind::Aquery));
        // The package box appears once.
        assert_eq!(graph.nodes.iter().filter(|n| names.resolve(n.id) == "pkg:foo").count(), 1);
    }

    #[test]
    fn labeled_generated_output_merges_across_query_and_aquery() {
        // query knows //pkg:gen.out as a generated-file target; aquery produces
        // it as the genrule //pkg:gen's output. The output is promoted to the
        // shared target node and merges with the query target.
        let query_targets = vec![Target {
            r#type: Discriminator::GeneratedFile as i32,
            generated_file: Some(GeneratedFile {
                name: "//pkg:gen.out".into(),
                ..Default::default()
            }),
            ..Default::default()
        }];

        let aquery_container = ActionGraphContainer {
            artifacts: vec![Artifact {
                id: 1,
                path_fragment_id: 103,
                is_tree_artifact: false,
            }],
            path_fragments: vec![
                PathFragment {
                    id: 100,
                    label: "bazel-out".into(),
                    parent_id: 0,
                },
                PathFragment {
                    id: 101,
                    label: "k8-fastbuild".into(),
                    parent_id: 100,
                },
                PathFragment {
                    id: 102,
                    label: "bin".into(),
                    parent_id: 101,
                },
                PathFragment {
                    id: 103,
                    label: "pkg/gen.out".into(),
                    parent_id: 102,
                },
            ],
            targets: vec![ActionTarget {
                id: 9,
                label: "//pkg:gen".into(),
                ..Default::default()
            }],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8".into(),
                checksum: "ck1".into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 9,
                mnemonic: "Genrule".into(),
                configuration_id: 1,
                output_ids: vec![1],
                ..Default::default()
            }],
            ..Default::default()
        };

        let (graph, names) = build_joint(Some(&[aquery_container]), None, Some(&query_targets), false);

        // The promoted target node is shared by query and aquery: it appears once.
        assert_eq!(
            graph.nodes.iter().filter(|n| names.resolve(n.id) == "target://pkg:gen.out").count(),
            1
        );
        // aquery's promotion contributes the per-config box and concrete artifact.
        assert!(graph.nodes.iter().any(|n| names.resolve(n.id) == "ct://pkg:gen.out#ck1"));
        assert!(graph.nodes.iter().any(|n| names.resolve(n.id) == "artifact://pkg:gen.out#ck1"));
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

        let (graph, names) = build_joint(
            Some(&[aquery_container]),
            None,
            Some(&query_targets),
            false,
        );

        assert_eq!(find(&graph, &names, "pkg:a").label, "//a");
        let nested = find(&graph, &names, "pkg:a/b");
        assert_eq!(nested.label, "b");
        assert_eq!(nested.parent.map(|p| names.resolve(p)), Some("pkg:a"));
    }
}
