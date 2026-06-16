//! Builds a compound graph from `bazel query --output=streamed_proto` output:
//! one leaf node per target, grouped under its package container, with edges
//! from each rule's inputs to the rule. An input naming a target absent from the
//! stream is left unwired rather than synthesizing a phantom node.

use std::collections::{HashMap, HashSet};

use bazel_query_proto::blaze_query::{target::Discriminator, Target};

use crate::graph::Graph;
use crate::label::{package_box, package_of};

pub fn build(targets: &[Target]) -> Graph {
    build_targets(&targets.iter().collect::<Vec<_>>())
}

/// The label and node kind a target carries, by its populated variant, or `None`
/// when the variant message is absent.
fn label_and_kind(target: &Target) -> Option<(&str, &'static str)> {
    match Discriminator::try_from(target.r#type).ok()? {
        Discriminator::Rule => Some((&target.rule.as_ref()?.name, "target")),
        Discriminator::SourceFile => Some((&target.source_file.as_ref()?.name, "source")),
        Discriminator::GeneratedFile => Some((&target.generated_file.as_ref()?.name, "artifact")),
        Discriminator::PackageGroup => Some((&target.package_group.as_ref()?.name, "group")),
        Discriminator::EnvironmentGroup => {
            Some((&target.environment_group.as_ref()?.name, "group"))
        }
    }
}

pub(crate) fn build_targets(targets: &[&Target]) -> Graph {
    let mut graph = Graph::new();
    let mut package_boxes: HashSet<String> = HashSet::new();
    // Node id per label, deduping a label that appears more than once (as the
    // same target under several configurations does in cquery output).
    let mut node_by_label: HashMap<String, String> = HashMap::new();

    // Pass 1: one leaf node per target under its package container.
    for target in targets {
        let Some((label, kind)) = label_and_kind(target) else {
            continue;
        };
        if node_by_label.contains_key(label) {
            continue;
        }
        let node_id = format!("q:{label}");
        let parent = package_of(label).map(|pkg| package_box(pkg, &mut package_boxes, &mut graph));
        graph.add_node(node_id.clone(), label, kind, parent);
        node_by_label.insert(label.to_string(), node_id);
    }

    // Pass 2: edges from each rule's inputs (dependency -> dependent).
    let mut seen_edge: HashSet<(String, String)> = HashSet::new();
    for target in targets {
        let Some(rule) = target.rule.as_ref() else {
            continue;
        };
        let Some(to) = node_by_label.get(&rule.name) else {
            continue;
        };
        for input_label in &rule.rule_input {
            let Some(from) = node_by_label.get(input_label) else {
                continue;
            };
            if seen_edge.insert((from.clone(), to.clone())) {
                graph.add_edge(from.clone(), to.clone());
            }
        }
    }

    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use bazel_query_proto::blaze_query::{GeneratedFile, Rule, SourceFile};

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

    fn generated(name: &str) -> Target {
        Target {
            r#type: Discriminator::GeneratedFile as i32,
            generated_file: Some(GeneratedFile {
                name: name.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn find<'a>(graph: &'a Graph, id: &str) -> &'a Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap()
    }

    fn parent_of<'a>(graph: &'a Graph, id: &str) -> Option<&'a str> {
        find(graph, id).parent.as_deref()
    }

    fn edges(graph: &Graph) -> HashSet<(&str, &str)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
    }

    #[test]
    fn rule_nests_in_its_package_container() {
        let graph = build(&[rule("//foo:app", &[])]);

        assert_eq!(find(&graph, "pkg:foo").kind, "package");
        assert_eq!(find(&graph, "pkg:foo").label, "//foo");
        let node = find(&graph, "q://foo:app");
        assert_eq!(node.kind, "target");
        assert_eq!(node.label, "//foo:app");
        assert_eq!(parent_of(&graph, "q://foo:app"), Some("pkg:foo"));
    }

    #[test]
    fn source_and_generated_files_get_their_kinds() {
        let graph = build(&[source("//foo:in.c"), generated("//foo:out.o")]);

        assert_eq!(find(&graph, "q://foo:in.c").kind, "source");
        assert_eq!(find(&graph, "q://foo:out.o").kind, "artifact");
    }

    #[test]
    fn rule_input_to_present_target_is_an_edge() {
        // Input -> dependent: the producer is `from`, the consuming rule is `to`.
        let graph = build(&[source("//foo:in.c"), rule("//foo:app", &["//foo:in.c"])]);

        assert!(edges(&graph).contains(&("q://foo:in.c", "q://foo:app")));
    }

    #[test]
    fn rule_input_to_absent_label_is_dropped() {
        let graph = build(&[rule("//foo:app", &["//bar:missing"])]);

        assert!(!graph.nodes.iter().any(|n| n.id == "q://bar:missing"));
        assert!(edges(&graph).is_empty());
    }

    #[test]
    fn external_target_nests_in_repo_qualified_package() {
        let graph = build(&[rule("//foo:x", &[]), rule("@somerepo//foo:y", &[])]);

        assert_eq!(parent_of(&graph, "q://foo:x"), Some("pkg:foo"));
        assert_eq!(find(&graph, "pkg:foo").label, "//foo");

        let external = parent_of(&graph, "q:@somerepo//foo:y").unwrap();
        assert_ne!(external, "pkg:foo");
        assert_eq!(find(&graph, external).label, "@somerepo//foo");
    }
}
