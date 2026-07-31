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
use bazel_query_proto::blaze_query::Rule;

use crate::graph::{EdgeKind, Graph, Kind};
use crate::intern::{Id, Names};
use crate::label::{self, label_and_kind, PackageHierarchy};
use crate::query;

#[cfg(test)]
fn build(results: &[CqueryResult]) -> (Graph, Names) {
    let packages = package_keys(results);
    let mut names = Names::default();
    let graph = build_with_packages(results, &packages, &mut names);
    (graph, names)
}

/// The (repository, package path) keys of every configured target this cquery
/// names, for the package hierarchy. A rule contributes its rule name's package,
/// any other target the package of its `label_and_kind` label.
pub(crate) fn package_keys(results: &[CqueryResult]) -> BTreeSet<(String, String)> {
    label::package_keys(
        results
            .iter()
            .flat_map(|r| r.results.iter())
            .filter_map(|ct| ct.target.as_ref())
            .filter_map(|target| {
                if let Ok(Discriminator::Rule) = Discriminator::try_from(target.r#type) {
                    Some(target.rule.as_ref()?.name.as_str())
                } else {
                    label_and_kind(target).map(|(label, _)| label)
                }
            }),
    )
}

pub(crate) fn generated_labels(results: &[CqueryResult]) -> HashSet<String> {
    query::generated_labels(
        results
            .iter()
            .flat_map(|r| r.results.iter())
            .filter_map(|ct| ct.target.as_ref()),
    )
}

/// Builds the cquery graph attaching its boxes to the `packages` hierarchy,
/// which a joint graph passes as the union of every input's packages so shared
/// package boxes carry one label and parent.
pub(crate) fn build_with_packages(
    results: &[CqueryResult],
    packages: &BTreeSet<(String, String)>,
    names: &mut Names,
) -> Graph {
    let cts: Vec<&ConfiguredTarget> = results.iter().flat_map(|r| r.results.iter()).collect();

    let mut b = Builder {
        graph: Graph::new(),
        names,
        config_checksum: config_checksums(results),
    };
    let index = build_nodes(&mut b, &cts, packages, config_mnemonics(results));
    wire_edges(&mut b, &cts, &index);
    b.graph
}

/// The state both build passes share.
struct Builder<'a> {
    graph: Graph,
    names: &'a mut Names,
    config_checksum: HashMap<u32, String>,
}

impl Builder<'_> {
    fn config_key(&self, config_id: u32) -> String {
        label::config_key(
            config_id,
            self.config_checksum.get(&config_id).map(String::as_str),
        )
    }
}

/// Each configuration's mnemonic, dropping the empty ones so a target in such a
/// configuration falls back to its id.
fn config_mnemonics(results: &[CqueryResult]) -> HashMap<u32, String> {
    results
        .iter()
        .flat_map(|r| r.configurations.iter())
        .filter(|c| !c.mnemonic.is_empty())
        .map(|c| (c.id, c.mnemonic.clone()))
        .collect()
}

/// Each configuration's checksum, the stable part of a configured target's key.
fn config_checksums(results: &[CqueryResult]) -> HashMap<u32, String> {
    results
        .iter()
        .flat_map(|r| r.configurations.iter())
        .map(|c| (c.id, c.checksum.clone()))
        .collect()
}

/// The node id maps pass 1 builds for pass 2 to resolve dependency edges against.
#[derive(Default)]
struct NodeIndex {
    /// Rule label -> its target container id.
    target_box: HashMap<String, Id>,
    /// Non-rule leaf label -> its node id.
    leaf_id: HashMap<String, Id>,
    /// The (label, configuration key) pairs that have a configured-target node.
    ct_ids: HashSet<(String, String)>,
}

/// Pass 1: the nodes each target contributes.
fn build_nodes(
    b: &mut Builder,
    cts: &[&ConfiguredTarget],
    packages: &BTreeSet<(String, String)>,
    config_mnemonic: HashMap<u32, String>,
) -> NodeIndex {
    let hierarchy = PackageHierarchy::build(packages, &mut b.graph, b.names);
    let mut nodes = Nodes {
        b,
        hierarchy,
        config_mnemonic,
        index: NodeIndex::default(),
    };

    for ct in cts {
        let Some(target) = ct.target.as_ref() else {
            continue;
        };
        if let Ok(Discriminator::Rule) = Discriminator::try_from(target.r#type) {
            let Some(rule) = target.rule.as_ref() else {
                continue;
            };
            nodes.add_rule_nodes(&rule.name, ct.configuration_id);
        } else if let Some((label, kind)) = label_and_kind(target) {
            nodes.add_leaf_node(label, kind);
        }
    }

    nodes.index
}

/// Mutable state threaded through node creation.
struct Nodes<'a, 'n> {
    b: &'a mut Builder<'n>,
    hierarchy: PackageHierarchy,
    config_mnemonic: HashMap<u32, String>,
    index: NodeIndex,
}

impl Nodes<'_, '_> {
    /// Creates the rule's target container the first time its label is seen, and
    /// a configured-target leaf the first time it is built in `config_id`.
    fn add_rule_nodes(&mut self, label: &str, config_id: u32) {
        if !self.index.target_box.contains_key(label) {
            let parent = self.hierarchy.package_box(label);
            let id = self.b.names.intern(&format!("target:{label}"));
            self.b
                .graph
                .add_node(id, label::target_label(label), Kind::Target, parent);
            self.index.target_box.insert(label.to_string(), id);
        }
        let key = self.b.config_key(config_id);
        if self.index.ct_ids.insert((label.to_string(), key.clone())) {
            let mnemonic = label::config_mnemonic(
                config_id,
                self.config_mnemonic.get(&config_id).map(String::as_str),
            );
            let id = self.b.names.intern(&label::ct_node_id(label, &key));
            self.b.graph.add_node(
                id,
                mnemonic,
                Kind::ConfiguredTarget,
                Some(self.index.target_box[label]),
            );
        }
    }

    /// Creates the leaf a non-rule target becomes, the first time its label is seen.
    fn add_leaf_node(&mut self, label: &str, kind: Kind) {
        if !self.index.leaf_id.contains_key(label) {
            let parent = self.hierarchy.package_box(label);
            let id = self.b.names.intern(&format!("target:{label}"));
            self.b
                .graph
                .add_node(id, label::target_label(label), kind, parent);
            self.index.leaf_id.insert(label.to_string(), id);
        }
    }
}

/// Pass 2: the edges into each rule's configured target.
fn wire_edges(b: &mut Builder, cts: &[&ConfiguredTarget], index: &NodeIndex) {
    let mut edges = Edges {
        b,
        index,
        seen: HashSet::new(),
    };
    for ct in cts {
        let Some(target) = ct.target.as_ref() else {
            continue;
        };
        if !matches!(Discriminator::try_from(target.r#type), Ok(Discriminator::Rule)) {
            continue;
        }
        let Some(rule) = target.rule.as_ref() else {
            continue;
        };
        edges.wire_rule(rule, ct.configuration_id);
    }
}

/// Resolves dependencies against the nodes pass 1 created and wires them to
/// their consumer.
struct Edges<'a, 'n> {
    b: &'a mut Builder<'n>,
    index: &'a NodeIndex,
    /// The (from, to) pairs already wired, so a dependency named twice gets one edge.
    seen: HashSet<(Id, Id)>,
}

impl Edges<'_, '_> {
    /// An edge from each dependency `rule` consumes to its configured target in
    /// `config_id`.
    fn wire_rule(&mut self, rule: &Rule, config_id: u32) {
        let key = self.b.config_key(config_id);
        let consumer = self.b.names.intern(&label::ct_node_id(&rule.name, &key));
        if rule.configured_rule_input.is_empty() {
            for dep_label in &rule.rule_input {
                if let Some(dep) = self.label_dep(dep_label) {
                    self.add_edge(dep, consumer);
                }
            }
        } else {
            for input in &rule.configured_rule_input {
                let Some(dep_label) = input.label.as_deref() else {
                    continue;
                };
                if let Some(dep) = self.configured_dep(dep_label, input.configuration_id) {
                    self.add_edge(dep, consumer);
                }
            }
        }
    }

    /// The node a configuration-resolved dependency lands on: the dependency's
    /// configured target when that configuration is present, its source/generated
    /// leaf when the dependency carries no configuration, and the dependency's
    /// target container as a fallback. `None` when the dependency is outside the
    /// queried set.
    fn configured_dep(&mut self, dep_label: &str, dep_config: Option<u32>) -> Option<Id> {
        match dep_config {
            Some(config_id) if config_id != 0 => {
                let key = self.b.config_key(config_id);
                if self.index.ct_ids.contains(&(dep_label.to_string(), key.clone())) {
                    Some(self.b.names.intern(&label::ct_node_id(dep_label, &key)))
                } else {
                    self.index.target_box.get(dep_label).copied()
                }
            }
            _ => self
                .index
                .leaf_id
                .get(dep_label)
                .or_else(|| self.index.target_box.get(dep_label))
                .copied(),
        }
    }

    /// The node a label-only dependency lands on: the dependency's target
    /// container, or its leaf when it is not a rule. `None` when outside the
    /// queried set.
    fn label_dep(&self, dep_label: &str) -> Option<Id> {
        self.index
            .target_box
            .get(dep_label)
            .or_else(|| self.index.leaf_id.get(dep_label))
            .copied()
    }

    fn add_edge(&mut self, from: Id, to: Id) {
        if from == to {
            return;
        }
        if self.seen.insert((from, to)) {
            self.b.graph.add_edge(from, to, EdgeKind::Cquery);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{edges, find};
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
            checksum: format!("ck{id}"),
            ..Default::default()
        }
    }

    fn result(results: Vec<ConfiguredTarget>, configurations: Vec<Configuration>) -> CqueryResult {
        CqueryResult {
            results,
            configurations,
        }
    }

    #[test]
    fn rule_in_two_configs_nests_a_ct_leaf_per_configuration() {
        let (graph, names) = build(&[result(
            vec![rule_ct("//foo:app", 1), rule_ct("//foo:app", 2)],
            vec![config(1, "k8-fastbuild"), config(2, "local_gnu-opt-exec")],
        )]);

        let container = find(&graph, &names, "target://foo:app");
        assert_eq!(container.kind, Kind::Target);
        assert_eq!(container.label, ":app");
        assert_eq!(container.parent.map(|p| names.resolve(p)), Some("pkg:foo"));

        let ct1 = find(&graph, &names, "ct://foo:app#ck1");
        assert_eq!(ct1.kind, Kind::ConfiguredTarget);
        assert_eq!(ct1.label, "k8-fastbuild");
        assert_eq!(ct1.parent.map(|p| names.resolve(p)), Some("target://foo:app"));
        assert_eq!(find(&graph, &names, "ct://foo:app#ck2").label, "local_gnu-opt-exec");
    }

    #[test]
    fn configuration_without_a_mnemonic_falls_back_to_its_id() {
        let (graph, names) = build(&[result(vec![rule_ct("//foo:app", 7)], vec![])]);

        assert_eq!(find(&graph, &names, "ct://foo:app#cfg7").label, "config 7");
    }

    #[test]
    fn configured_rule_input_edges_match_dependency_configurations() {
        // app#1 depends on lib in config 2 (an exec dep) and config 1.
        let (graph, names) = build(&[result(
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
        let edges = edges(&graph, &names);
        assert!(edges.contains(&("ct://foo:lib#ck2", "ct://foo:app#ck1")));
        assert!(edges.contains(&("ct://foo:lib#ck1", "ct://foo:app#ck1")));
    }

    #[test]
    fn rule_input_falls_back_to_the_dependency_target_container() {
        // No configured_rule_input, so the label-only rule_input lands on lib's
        // target container rather than a specific configured target.
        let (graph, names) = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &["//foo:lib"], &[]),
                rule_ct("//foo:lib", 1),
            ],
            vec![config(1, "target")],
        )]);

        assert!(edges(&graph, &names).contains(&("target://foo:lib", "ct://foo:app#ck1")));
    }

    #[test]
    fn configured_dependency_in_an_absent_configuration_lands_on_the_container() {
        // app depends on lib in config 2, but only lib's config-1 configured
        // target is present, so the edge lands on lib's target container.
        let (graph, names) = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &[], &[("//foo:lib", Some(2))]),
                rule_ct("//foo:lib", 1),
            ],
            vec![config(1, "target")],
        )]);

        assert!(!graph.nodes.iter().any(|n| names.resolve(n.id) == "ct://foo:lib#cfg2"));
        assert!(edges(&graph, &names).contains(&("target://foo:lib", "ct://foo:app#ck1")));
    }

    #[test]
    fn dependency_without_a_configuration_lands_on_its_source_leaf() {
        let (graph, names) = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &[], &[("//foo:in.c", None)]),
                source_ct("//foo:in.c"),
            ],
            vec![config(1, "target")],
        )]);

        let leaf = find(&graph, &names, "target://foo:in.c");
        assert_eq!(leaf.kind, Kind::Source);
        assert!(edges(&graph, &names).contains(&("target://foo:in.c", "ct://foo:app#ck1")));
    }

    #[test]
    fn label_only_rule_input_naming_a_source_lands_on_its_leaf() {
        // rule_input (no configured_rule_input) naming a source file resolves to
        // that source leaf, not a target container.
        let (graph, names) = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &["//foo:in.c"], &[]),
                source_ct("//foo:in.c"),
            ],
            vec![config(1, "target")],
        )]);

        assert!(edges(&graph, &names).contains(&("target://foo:in.c", "ct://foo:app#ck1")));
    }

    #[test]
    fn dependency_outside_the_queried_set_is_dropped() {
        let (graph, names) = build(&[result(
            vec![with_inputs(
                rule_ct("//foo:app", 1),
                &[],
                &[("@external//:dep", Some(1))],
            )],
            vec![config(1, "target")],
        )]);

        assert!(!graph.nodes.iter().any(|n| n.label == "@external//:dep"));
        assert!(edges(&graph, &names).is_empty());
    }
}
