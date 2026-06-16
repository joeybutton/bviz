//! Builds a compound graph from `bazel cquery --output=streamed_proto` output as
//! a package -> target -> configured-target hierarchy: each rule label is a
//! target container holding one configured-target leaf per configuration it is
//! built in, labeled by that configuration's mnemonic. Source and generated
//! files stay as leaves under their package.
//!
//! Each edge runs from a dependency to the configured target that consumes it,
//! matching the direction the aquery and query builders use. It reaches the
//! dependency's exact configured target when `configured_rule_input` resolves
//! that dependency's configuration, which cquery emits only under
//! `--transitions=lite` (or `full`); without it the edge falls back to the
//! label-only `rule_input` and lands on the dependency's target container, since
//! the configuration of each dependency is then unknown.

use std::collections::{BTreeSet, HashMap, HashSet};

use bazel_query_proto::analysis::{ConfiguredTarget, CqueryResult};
use bazel_query_proto::blaze_query::target::Discriminator;

use crate::graph::{EdgeKind, Graph};
use crate::label::{self, PackageHierarchy};
use crate::query::label_and_kind;

pub fn build(results: &[CqueryResult]) -> Graph {
    let config_mnemonic: HashMap<u32, String> = results
        .iter()
        .flat_map(|r| r.configurations.iter())
        .filter(|c| !c.mnemonic.is_empty())
        .map(|c| (c.id, c.mnemonic.clone()))
        .collect();
    let cts: Vec<&ConfiguredTarget> = results.iter().flat_map(|r| r.results.iter()).collect();

    let mut graph = Graph::new();
    // Rule label -> its target container id.
    let mut target_box: HashMap<String, String> = HashMap::new();
    // Non-rule leaf label -> its node id.
    let mut leaf_id: HashMap<String, String> = HashMap::new();
    // The (label, configuration id) pairs that have a configured-target node.
    let mut ct_ids: HashSet<(String, u32)> = HashSet::new();

    let packages: BTreeSet<(String, String)> = cts
        .iter()
        .filter_map(|ct| ct.target.as_ref())
        .filter_map(|target| {
            if let Ok(Discriminator::Rule) = Discriminator::try_from(target.r#type) {
                Some(target.rule.as_ref()?.name.as_str())
            } else {
                label_and_kind(target).map(|(label, _)| label)
            }
        })
        .filter_map(label::package_key)
        .collect();
    let hierarchy = PackageHierarchy::build(&packages, &mut graph);

    // Pass 1: a rule becomes a target container plus a configured-target leaf
    // per configuration; any other target becomes one leaf per label.
    for ct in &cts {
        let Some(target) = ct.target.as_ref() else {
            continue;
        };
        if let Ok(Discriminator::Rule) = Discriminator::try_from(target.r#type) {
            let Some(rule) = target.rule.as_ref() else {
                continue;
            };
            let label = rule.name.as_str();
            if !target_box.contains_key(label) {
                let parent = hierarchy.package_box(label).map(str::to_string);
                let id = format!("q:target:{label}");
                graph.add_node(id.clone(), label::target_label(label), "target", parent);
                target_box.insert(label.to_string(), id);
            }
            let config_id = ct.configuration_id;
            if ct_ids.insert((label.to_string(), config_id)) {
                let mnemonic = config_mnemonic
                    .get(&config_id)
                    .cloned()
                    .unwrap_or_else(|| format!("config {config_id}"));
                graph.add_node(
                    ct_node_id(label, config_id),
                    mnemonic,
                    "configured-target",
                    Some(target_box[label].clone()),
                );
            }
        } else if let Some((label, kind)) = label_and_kind(target) {
            if !leaf_id.contains_key(label) {
                let parent = hierarchy.package_box(label).map(str::to_string);
                let id = format!("q:{label}");
                graph.add_node(id.clone(), label::target_label(label), kind, parent);
                leaf_id.insert(label.to_string(), id);
            }
        }
    }

    // Pass 2: an edge from each dependency to the configured target that
    // consumes it.
    let mut seen_edge: HashSet<(String, String)> = HashSet::new();
    for ct in &cts {
        let Some(target) = ct.target.as_ref() else {
            continue;
        };
        if !matches!(Discriminator::try_from(target.r#type), Ok(Discriminator::Rule)) {
            continue;
        }
        let Some(rule) = target.rule.as_ref() else {
            continue;
        };
        let consumer = ct_node_id(&rule.name, ct.configuration_id);
        if rule.configured_rule_input.is_empty() {
            for dep_label in &rule.rule_input {
                if let Some(dep) = resolve_label_dep(dep_label, &target_box, &leaf_id) {
                    add_edge(&mut graph, &mut seen_edge, &dep, &consumer);
                }
            }
        } else {
            for input in &rule.configured_rule_input {
                let Some(dep_label) = input.label.as_deref() else {
                    continue;
                };
                if let Some(dep) = resolve_configured_dep(
                    dep_label,
                    input.configuration_id,
                    &ct_ids,
                    &target_box,
                    &leaf_id,
                ) {
                    add_edge(&mut graph, &mut seen_edge, &dep, &consumer);
                }
            }
        }
    }

    graph
}

fn ct_node_id(label: &str, config_id: u32) -> String {
    format!("q:ct:{label}#{config_id}")
}

/// The node a configuration-resolved dependency lands on: the dependency's
/// configured target when that configuration is present, its source/generated
/// leaf when the dependency carries no configuration, and the dependency's
/// target container as a fallback. `None` when the dependency is outside the
/// queried set.
fn resolve_configured_dep(
    dep_label: &str,
    dep_config: Option<u32>,
    ct_ids: &HashSet<(String, u32)>,
    target_box: &HashMap<String, String>,
    leaf_id: &HashMap<String, String>,
) -> Option<String> {
    match dep_config {
        Some(config_id) if config_id != 0 => {
            if ct_ids.contains(&(dep_label.to_string(), config_id)) {
                Some(ct_node_id(dep_label, config_id))
            } else {
                target_box.get(dep_label).cloned()
            }
        }
        _ => leaf_id
            .get(dep_label)
            .or_else(|| target_box.get(dep_label))
            .cloned(),
    }
}

/// The node a label-only dependency lands on: the dependency's target container,
/// or its leaf when it is not a rule. `None` when outside the queried set.
fn resolve_label_dep(
    dep_label: &str,
    target_box: &HashMap<String, String>,
    leaf_id: &HashMap<String, String>,
) -> Option<String> {
    target_box
        .get(dep_label)
        .or_else(|| leaf_id.get(dep_label))
        .cloned()
}

fn add_edge(graph: &mut Graph, seen: &mut HashSet<(String, String)>, from: &str, to: &str) {
    if from == to {
        return;
    }
    if seen.insert((from.to_string(), to.to_string())) {
        graph.add_edge(from.to_string(), to.to_string(), EdgeKind::Cquery);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use bazel_query_proto::analysis::Configuration;
    use bazel_query_proto::blaze_query::{ConfiguredRuleInput, Rule, SourceFile, Target};

    fn rule_ct(name: &str, config_id: u32) -> ConfiguredTarget {
        ConfiguredTarget {
            target: Some(Target {
                r#type: Discriminator::Rule as i32,
                rule: Some(Rule {
                    name: name.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            configuration_id: config_id,
            ..Default::default()
        }
    }

    /// Adds `rule_input`/`configured_rule_input` to the rule of a configured
    /// target produced by `rule_ct`.
    fn with_inputs(
        mut ct: ConfiguredTarget,
        rule_input: &[&str],
        configured: &[(&str, Option<u32>)],
    ) -> ConfiguredTarget {
        let rule = ct.target.as_mut().unwrap().rule.as_mut().unwrap();
        rule.rule_input = rule_input.iter().map(|s| s.to_string()).collect();
        rule.configured_rule_input = configured
            .iter()
            .map(|(label, config_id)| ConfiguredRuleInput {
                label: Some(label.to_string()),
                configuration_id: *config_id,
                ..Default::default()
            })
            .collect();
        ct
    }

    fn source_ct(name: &str) -> ConfiguredTarget {
        ConfiguredTarget {
            target: Some(Target {
                r#type: Discriminator::SourceFile as i32,
                source_file: Some(SourceFile {
                    name: name.to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn config(id: u32, mnemonic: &str) -> Configuration {
        Configuration {
            id,
            mnemonic: mnemonic.to_string(),
            ..Default::default()
        }
    }

    fn result(results: Vec<ConfiguredTarget>, configurations: Vec<Configuration>) -> CqueryResult {
        CqueryResult {
            results,
            configurations,
        }
    }

    fn find<'a>(graph: &'a Graph, id: &str) -> &'a Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap()
    }

    fn edges(graph: &Graph) -> HashSet<(&str, &str)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
    }

    #[test]
    fn rule_in_two_configs_nests_a_ct_leaf_per_configuration() {
        let graph = build(&[result(
            vec![rule_ct("//foo:app", 1), rule_ct("//foo:app", 2)],
            vec![config(1, "k8-fastbuild"), config(2, "local_gnu-opt-exec")],
        )]);

        let container = find(&graph, "q:target://foo:app");
        assert_eq!(container.kind, "target");
        assert_eq!(container.label, ":app");
        assert_eq!(container.parent.as_deref(), Some("pkg:foo"));

        let ct1 = find(&graph, "q:ct://foo:app#1");
        assert_eq!(ct1.kind, "configured-target");
        assert_eq!(ct1.label, "k8-fastbuild");
        assert_eq!(ct1.parent.as_deref(), Some("q:target://foo:app"));
        assert_eq!(find(&graph, "q:ct://foo:app#2").label, "local_gnu-opt-exec");
    }

    #[test]
    fn configuration_without_a_mnemonic_falls_back_to_its_id() {
        let graph = build(&[result(vec![rule_ct("//foo:app", 7)], vec![])]);

        assert_eq!(find(&graph, "q:ct://foo:app#7").label, "config 7");
    }

    #[test]
    fn configured_rule_input_edges_match_dependency_configurations() {
        // app#1 depends on lib in config 2 (an exec dep) and config 1.
        let graph = build(&[result(
            vec![
                with_inputs(
                    rule_ct("//foo:app", 1),
                    &[],
                    &[("//foo:lib", Some(2)), ("//foo:lib", Some(1))],
                ),
                rule_ct("//foo:lib", 1),
                rule_ct("//foo:lib", 2),
            ],
            vec![config(1, "target"), config(2, "exec")],
        )]);

        // Edges run dependency -> dependent, as in the query and aquery builders.
        let edges = edges(&graph);
        assert!(edges.contains(&("q:ct://foo:lib#2", "q:ct://foo:app#1")));
        assert!(edges.contains(&("q:ct://foo:lib#1", "q:ct://foo:app#1")));
    }

    #[test]
    fn rule_input_falls_back_to_the_dependency_target_container() {
        // No configured_rule_input, so the label-only rule_input lands on lib's
        // target container rather than a specific configured target.
        let graph = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &["//foo:lib"], &[]),
                rule_ct("//foo:lib", 1),
            ],
            vec![config(1, "target")],
        )]);

        assert!(edges(&graph).contains(&("q:target://foo:lib", "q:ct://foo:app#1")));
    }

    #[test]
    fn configured_dependency_in_an_absent_configuration_lands_on_the_container() {
        // app depends on lib in config 2, but only lib's config-1 configured
        // target is present, so the edge lands on lib's target container.
        let graph = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &[], &[("//foo:lib", Some(2))]),
                rule_ct("//foo:lib", 1),
            ],
            vec![config(1, "target")],
        )]);

        assert!(!graph.nodes.iter().any(|n| n.id == "q:ct://foo:lib#2"));
        assert!(edges(&graph).contains(&("q:target://foo:lib", "q:ct://foo:app#1")));
    }

    #[test]
    fn dependency_without_a_configuration_lands_on_its_source_leaf() {
        let graph = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &[], &[("//foo:in.c", None)]),
                source_ct("//foo:in.c"),
            ],
            vec![config(1, "target")],
        )]);

        let leaf = find(&graph, "q://foo:in.c");
        assert_eq!(leaf.kind, "source");
        assert!(edges(&graph).contains(&("q://foo:in.c", "q:ct://foo:app#1")));
    }

    #[test]
    fn label_only_rule_input_naming_a_source_lands_on_its_leaf() {
        // rule_input (no configured_rule_input) naming a source file resolves to
        // that source leaf, not a target container.
        let graph = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &["//foo:in.c"], &[]),
                source_ct("//foo:in.c"),
            ],
            vec![config(1, "target")],
        )]);

        assert!(edges(&graph).contains(&("q://foo:in.c", "q:ct://foo:app#1")));
    }

    #[test]
    fn dependency_outside_the_queried_set_is_dropped() {
        let graph = build(&[result(
            vec![with_inputs(
                rule_ct("//foo:app", 1),
                &[],
                &[("@external//:dep", Some(1))],
            )],
            vec![config(1, "target")],
        )]);

        assert!(!graph.nodes.iter().any(|n| n.label == "@external//:dep"));
        assert!(edges(&graph).is_empty());
    }
}
