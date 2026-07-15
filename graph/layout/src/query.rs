//! Builds a compound graph from `bazel query --output=streamed_proto` output:
//! one leaf node per target, grouped under its package container, with edges
//! from each rule's inputs to the rule. An input naming a target absent from the
//! stream is left unwired rather than synthesizing a phantom node.

use std::collections::{BTreeSet, HashMap, HashSet};

use bazel_query_proto::blaze_query::Target;

use crate::graph::{EdgeKind, Graph, Kind};
use crate::intern::{Id, Names};
use crate::label::{self, label_and_kind, PackageHierarchy};

pub fn build(targets: &[Target]) -> (Graph, Names) {
    let refs: Vec<&Target> = targets.iter().collect();
    let packages = package_keys(targets);
    let mut names = Names::default();
    let graph = build_with_packages(&refs, &packages, &mut names);
    (graph, names)
}

/// The (repository, package path) keys of every target this query names, for the
/// package hierarchy.
pub(crate) fn package_keys(targets: &[Target]) -> BTreeSet<(String, String)> {
    targets
        .iter()
        .filter_map(label_and_kind)
        .filter_map(|(label, _)| label::package_key(label))
        .collect()
}

/// The labels of generated-file targets among `targets`, which a joint graph
/// uses to recognize aquery output artifacts that are also targets.
pub(crate) fn generated_labels<'a>(
    targets: impl IntoIterator<Item = &'a Target>,
) -> HashSet<String> {
    targets
        .into_iter()
        .filter_map(label_and_kind)
        .filter(|(_, kind)| *kind == Kind::Artifact)
        .map(|(label, _)| label.to_string())
        .collect()
}

/// Builds the query graph attaching its boxes to the `packages` hierarchy,
/// which a joint graph passes as the union of every input's packages so shared
/// package boxes carry one label and parent.
pub(crate) fn build_with_packages(
    targets: &[&Target],
    packages: &BTreeSet<(String, String)>,
    names: &mut Names,
) -> Graph {
    let mut graph = Graph::new();
    // Node id per label, so a label seen more than once maps to one node.
    let mut node_by_label: HashMap<String, Id> = HashMap::new();

    let hierarchy = PackageHierarchy::build(packages, &mut graph, names);

    // Pass 1: one leaf node per target under its package container.
    for target in targets {
        let Some((label, kind)) = label_and_kind(target) else {
            continue;
        };
        if node_by_label.contains_key(label) {
            continue;
        }
        let node_id = names.intern(&format!("target:{label}"));
        let parent = hierarchy.package_box(label);
        graph.add_node(node_id, label::target_label(label), kind, parent);
        node_by_label.insert(label.to_string(), node_id);
    }

    // Pass 2: edges from each rule's inputs (dependency -> dependent).
    let mut seen_edge: HashSet<(Id, Id)> = HashSet::new();
    for target in targets {
        let Some(rule) = target.rule.as_ref() else {
            continue;
        };
        let Some(&to) = node_by_label.get(&rule.name) else {
            continue;
        };
        for input_label in &rule.rule_input {
            let Some(&from) = node_by_label.get(input_label) else {
                continue;
            };
            if seen_edge.insert((from, to)) {
                graph.add_edge(from, to, EdgeKind::Query);
            }
        }
    }

    graph
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{edges, find, parent_of};
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

    #[test]
    fn rule_nests_in_its_package_container() {
        let (graph, names) = build(&[rule("//foo:app", &[])]);

        assert_eq!(find(&graph, &names, "pkg:foo").kind, Kind::Package);
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");
        let node = find(&graph, &names, "target://foo:app");
        assert_eq!(node.kind, Kind::Target);
        assert_eq!(node.label, ":app");
        assert_eq!(parent_of(&graph, &names, "target://foo:app"), Some("pkg:foo"));
    }

    #[test]
    fn source_and_generated_files_get_their_kinds() {
        let (graph, names) = build(&[source("//foo:in.c"), generated("//foo:out.o")]);

        assert_eq!(find(&graph, &names, "target://foo:in.c").kind, Kind::Source);
        assert_eq!(find(&graph, &names, "target://foo:out.o").kind, Kind::Artifact);
    }

    #[test]
    fn rule_input_to_present_target_is_an_edge() {
        // Input -> dependent: the producer is `from`, the consuming rule is `to`.
        let (graph, names) = build(&[source("//foo:in.c"), rule("//foo:app", &["//foo:in.c"])]);

        assert!(edges(&graph, &names).contains(&("target://foo:in.c", "target://foo:app")));
    }

    #[test]
    fn rule_input_to_absent_label_is_dropped() {
        let (graph, names) = build(&[rule("//foo:app", &["//bar:missing"])]);

        assert!(!graph.nodes.iter().any(|n| names.resolve(n.id) == "target://bar:missing"));
        assert!(edges(&graph, &names).is_empty());
    }

    #[test]
    fn external_target_nests_in_repo_qualified_package() {
        let (graph, names) = build(&[rule("//foo:x", &[]), rule("@somerepo//foo:y", &[])]);

        assert_eq!(parent_of(&graph, &names, "target://foo:x"), Some("pkg:foo"));
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");

        let external = parent_of(&graph, &names, "target:@somerepo//foo:y").unwrap();
        assert_ne!(external, "pkg:foo");
        assert_eq!(find(&graph, &names, external).label, "@somerepo//foo");
    }
}
