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

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};

use bazel_query_proto::analysis::{Configuration, ConfiguredTarget, CqueryResult};
use bazel_query_proto::blaze_query::target::Discriminator;
use bazel_query_proto::blaze_query::{Rule, Target};

use crate::capability::Axis;
#[cfg(test)]
use crate::facets::Facets;
use crate::facets::{ConfigRef, FacetsBuilder};
use crate::graph::{EdgeKind, Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::label::{self, PackageHierarchy, RepoMapping, label_and_kind};
use crate::query;

#[cfg(test)]
fn build(results: &[CqueryResult]) -> (Graph, Names, Facets) {
    let repos = RepoMapping::default();
    let packages = package_keys(results, &repos);
    let mut names = Names::default();
    let mut facets = FacetsBuilder::default();
    let graph = build_with_packages(results, &packages, &repos, &mut names, &mut facets);
    (graph, names, facets.seal().0)
}

/// Every label this cquery names. A rule contributes its rule name, any other
/// target its `label_and_kind` label.
pub(crate) fn labels(results: &[CqueryResult]) -> impl Iterator<Item = &str> {
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
        })
}

/// The (repository, package path) keys of every configured target this cquery
/// names, for the package hierarchy.
pub(crate) fn package_keys(
    results: &[CqueryResult],
    repos: &RepoMapping,
) -> BTreeSet<(String, String)> {
    label::package_keys(labels(results), repos)
}

pub(crate) fn generated_labels(results: &[CqueryResult], repos: &RepoMapping) -> HashSet<String> {
    query::generated_labels(
        results
            .iter()
            .flat_map(|r| r.results.iter())
            .filter_map(|ct| ct.target.as_ref()),
        repos,
    )
}

/// Builds the cquery graph attaching its boxes to the `packages` hierarchy,
/// which a joint graph passes as the union of every input's packages so shared
/// package boxes carry one label and parent.
pub(crate) fn build_with_packages(
    results: &[CqueryResult],
    packages: &BTreeSet<(String, String)>,
    repos: &RepoMapping,
    names: &mut Names,
    facets: &mut FacetsBuilder,
) -> Graph {
    let cts: Vec<&ConfiguredTarget> = results.iter().flat_map(|r| r.results.iter()).collect();

    let mut b = Builder {
        graph: Graph::new(),
        names,
        repos,
        facets,
        configurations: configurations(results),
    };
    let index = build_nodes(&mut b, &cts, packages);
    wire_edges(&mut b, &cts, &index);
    b.graph
}

/// The state both build passes share.
struct Builder<'a> {
    graph: Graph,
    names: &'a mut Names,
    repos: &'a RepoMapping,
    facets: &'a mut FacetsBuilder,
    configurations: HashMap<u32, &'a Configuration>,
}

impl Builder<'_> {
    fn config_key(&self, config_id: u32) -> String {
        label::config_key(
            config_id,
            self.configurations
                .get(&config_id)
                .map(|c| c.checksum.as_str()),
        )
    }

    fn config_mnemonic(&self, config_id: u32) -> String {
        label::config_mnemonic(
            config_id,
            self.configurations
                .get(&config_id)
                .map(|c| c.mnemonic.as_str()),
        )
    }

    /// The handle for the configuration `config_id` names, absent for a target
    /// carrying no configuration, which names id `0`.
    fn intern_config(&mut self, config_id: u32) -> Option<ConfigRef> {
        let config = *self.configurations.get(&config_id)?;
        self.facets.facets().intern_configuration(config)
    }

    /// Records for the configured-target node `node` the configuration it is
    /// built in, and what its target container `container` states.
    fn describe_configured(&mut self, node: Id, container: Id, config_id: u32) {
        let config = self.intern_config(config_id);
        self.facets.observe(Axis::Configuration, config.is_some());
        if let Some(config) = config {
            self.facets.facets().set_config(node, Some(config));
        }
        self.facets.facets().inherit(node, container);
    }

    fn canonical<'l>(&self, label: &'l str) -> Cow<'l, str> {
        self.repos.canonicalize_label(label)
    }
}

/// Every configuration the results state, by the id its configured targets name
/// it by. The results are read whole before any node is built, so a result
/// stating its configurations after the configured targets that refer to them
/// resolves as readily as one stating them first.
fn configurations(results: &[CqueryResult]) -> HashMap<u32, &Configuration> {
    results
        .iter()
        .flat_map(|r| r.configurations.iter())
        .map(|c| (c.id, c))
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
) -> NodeIndex {
    let hierarchy = PackageHierarchy::build(packages, &mut b.graph, b.names, b.facets);
    let mut nodes = Nodes {
        b,
        hierarchy,
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
            nodes.add_rule_nodes(target, &rule.name, ct.configuration_id);
        } else if let Some((label, kind)) = label_and_kind(target) {
            nodes.add_leaf_node(target, label, kind);
        }
    }

    nodes.index
}

/// Mutable state threaded through node creation.
struct Nodes<'a, 'n> {
    b: &'a mut Builder<'n>,
    hierarchy: PackageHierarchy,
    index: NodeIndex,
}

impl Nodes<'_, '_> {
    /// Creates the rule's target container the first time its label is seen, and
    /// a configured-target leaf the first time it is built in `config_id`.
    /// `written` is the label as the input printed it.
    fn add_rule_nodes(&mut self, target: &Target, written: &str, config_id: u32) {
        let label = self.b.canonical(written);
        if !self.index.target_box.contains_key(label.as_ref()) {
            let parent = self.hierarchy.package_box(&label);
            let id = self.b.names.intern(&label::target_node_id(&label));
            self.b.graph.add_node(Node::new(
                id,
                label::target_label(&label),
                Kind::Target,
                parent,
            ));
            query::describe_target(self.b.facets, target, id, parent, &label, written);
            self.index.target_box.insert(label.to_string(), id);
        }
        let container = self.index.target_box[label.as_ref()];
        let key = self.b.config_key(config_id);
        if self.index.ct_ids.insert((label.to_string(), key.clone())) {
            let mnemonic = self.b.config_mnemonic(config_id);
            let id = self.b.names.intern(&label::ct_node_id(&label, &key));
            self.b.graph.add_node(Node::new(
                id,
                mnemonic,
                Kind::ConfiguredTarget,
                Some(container),
            ));
            self.b.describe_configured(id, container, config_id);
        }
    }

    /// Creates the leaf a non-rule target becomes, the first time its label is
    /// seen. `written` is the label as the input printed it.
    fn add_leaf_node(&mut self, target: &Target, written: &str, kind: Kind) {
        let label = self.b.canonical(written);
        if !self.index.leaf_id.contains_key(label.as_ref()) {
            let parent = self.hierarchy.package_box(&label);
            let id = self.b.names.intern(&label::target_node_id(&label));
            self.b
                .graph
                .add_node(Node::new(id, label::target_label(&label), kind, parent));
            query::describe_target(self.b.facets, target, id, parent, &label, written);
            self.index.leaf_id.insert(label.into_owned(), id);
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
        if !matches!(
            Discriminator::try_from(target.r#type),
            Ok(Discriminator::Rule)
        ) {
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
        if !rule.rule_input.is_empty() || !rule.configured_rule_input.is_empty() {
            let resolved = !rule.configured_rule_input.is_empty();
            self.b.facets.observe(Axis::ExecConfiguration, resolved);
        }
        let key = self.b.config_key(config_id);
        let name = self.b.canonical(&rule.name);
        let consumer = self.b.names.intern(&label::ct_node_id(&name, &key));
        if rule.configured_rule_input.is_empty() {
            for dep_label in &rule.rule_input {
                let dep_label = self.b.canonical(dep_label);
                if let Some(dep) = self.label_dep(&dep_label) {
                    self.add_edge(dep, consumer);
                }
            }
        } else {
            for input in &rule.configured_rule_input {
                let Some(dep_label) = input.label.as_deref() else {
                    continue;
                };
                let dep_label = self.b.canonical(dep_label);
                if let Some(dep) = self.configured_dep(&dep_label, input.configuration_id) {
                    self.add_edge(dep, consumer);
                }
            }
        }
    }

    /// The node a configuration-resolved dependency lands on: the dependency's
    /// configured target when that configuration is present, its source/generated
    /// leaf when the dependency carries no configuration, and the label-only
    /// resolution as a fallback. `None` when the dependency is outside the
    /// queried set.
    fn configured_dep(&mut self, dep_label: &str, dep_config: Option<u32>) -> Option<Id> {
        match dep_config {
            Some(config_id) if config_id != 0 => {
                let key = self.b.config_key(config_id);
                if self
                    .index
                    .ct_ids
                    .contains(&(dep_label.to_string(), key.clone()))
                {
                    Some(self.b.names.intern(&label::ct_node_id(dep_label, &key)))
                } else {
                    self.label_dep(dep_label)
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
    use crate::facets::{RuleCategory, Tri};
    use crate::test_support::{edges, find};
    use bazel_query_proto::blaze_query::{
        Attribute, ConfiguredRuleInput, GeneratedFile, Rule, SourceFile,
        attribute::Discriminator as AttributeType,
    };

    fn rule_ct(name: &str, config_id: u32) -> ConfiguredTarget {
        declared_ct(name, config_id, "cc_library", Vec::new())
    }

    /// A configured target for a rule of class `rule_class` whose attributes the
    /// query was asked to print.
    fn declared_ct(
        name: &str,
        config_id: u32,
        rule_class: &str,
        attributes: Vec<Attribute>,
    ) -> ConfiguredTarget {
        ConfiguredTarget {
            target: Some(Target {
                r#type: Discriminator::Rule as i32,
                rule: Some(Rule {
                    name: name.to_string(),
                    rule_class: rule_class.to_string(),
                    attribute: attributes,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            configuration_id: config_id,
            ..Default::default()
        }
    }

    fn tags(values: &[&str]) -> Attribute {
        Attribute {
            name: "tags".to_string(),
            r#type: AttributeType::StringList as i32,
            string_list_value: values.iter().map(|v| v.to_string()).collect(),
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

    fn generated_ct(name: &str) -> ConfiguredTarget {
        ConfiguredTarget {
            target: Some(Target {
                r#type: Discriminator::GeneratedFile as i32,
                generated_file: Some(GeneratedFile {
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

    /// A configuration Bazel marks as building tools, as it does an exec
    /// configuration.
    fn tool_config(id: u32, mnemonic: &str) -> Configuration {
        Configuration {
            is_tool: true,
            ..config(id, mnemonic)
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
        let (graph, names, _) = build(&[result(
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
        assert_eq!(
            ct1.parent.map(|p| names.resolve(p)),
            Some("target://foo:app")
        );
        assert_eq!(
            find(&graph, &names, "ct://foo:app#ck2").label,
            "local_gnu-opt-exec"
        );
    }

    #[test]
    fn a_configured_targets_config_facet_names_the_configuration_it_is_built_in() {
        let (_, names, facets) = build(&[result(
            vec![rule_ct("//foo:app", 1)],
            vec![config(1, "k8-fastbuild")],
        )]);

        let node = names.get("ct://foo:app#ck1").unwrap();
        let config = facets.config(node).unwrap();
        assert_eq!(facets.strings().resolve(config.sym()), "ck1");
        let entry = facets.config_entry(config).unwrap();
        assert_eq!(
            entry.mnemonic.map(|name| facets.strings().resolve(name)),
            Some("k8-fastbuild")
        );
        assert_eq!(entry.is_tool, Tri::No);
    }

    #[test]
    fn an_exec_configured_target_reads_the_configurations_tool_flag() {
        let (_, names, facets) = build(&[result(
            vec![rule_ct("//foo:app", 2)],
            vec![tool_config(2, "k8-opt-exec")],
        )]);

        let node = names.get("ct://foo:app#ck2").unwrap();
        let config = facets.config(node).unwrap();
        assert_eq!(facets.config_entry(config).unwrap().is_tool, Tri::Yes);
    }

    #[test]
    fn a_configured_target_naming_no_configuration_carries_no_config_facet() {
        let (_, names, facets) = build(&[result(vec![rule_ct("//foo:app", 0)], vec![])]);

        let node = names.get("ct://foo:app#cfg0").unwrap();
        assert_eq!(facets.config(node), None);
    }

    #[test]
    fn a_configured_target_carries_what_its_target_declares() {
        let (_, names, facets) = build(&[result(
            vec![declared_ct(
                "//foo:app",
                1,
                "cc_test",
                vec![tags(&["manual"])],
            )],
            vec![config(1, "k8-fastbuild")],
        )]);

        let container = names.get("target://foo:app").unwrap();
        let node = names.get("ct://foo:app#ck1").unwrap();
        let rule_class = facets.rule_class(node).unwrap();
        assert_eq!(facets.strings().resolve(rule_class.sym()), "cc_test");
        assert_eq!(facets.rule_category(rule_class), Some(RuleCategory::Test));
        assert_eq!(facets.label(node), Some(container));
        assert_eq!(facets.package(node), names.get("pkg:foo"));
        assert_eq!(facets.repo(node), facets.repo(container));
        // Tags stay on the target that declares them, which the configured
        // target reaches through its label.
        let tags: Vec<&str> = facets
            .tags()
            .values(container)
            .iter()
            .map(|&tag| facets.strings().resolve(tag))
            .collect();
        assert_eq!(tags, ["manual"]);
    }

    #[test]
    fn a_configuration_stated_after_its_configured_targets_still_resolves() {
        // cquery streams a `CqueryResult` per configured target and the
        // configurations in results of their own at the end of the stream.
        let (_, names, facets) = build(&[
            result(vec![rule_ct("//foo:app", 1)], vec![]),
            result(vec![], vec![config(1, "k8-fastbuild")]),
        ]);

        let node = names.get("ct://foo:app#ck1").unwrap();
        let config = facets.config(node).unwrap();
        assert_eq!(facets.strings().resolve(config.sym()), "ck1");
    }

    #[test]
    fn configuration_without_a_mnemonic_falls_back_to_its_id() {
        let (graph, names, _) = build(&[result(vec![rule_ct("//foo:app", 7)], vec![])]);

        assert_eq!(find(&graph, &names, "ct://foo:app#cfg7").label, "config 7");
    }

    #[test]
    fn configured_rule_input_edges_match_dependency_configurations() {
        // app#1 depends on lib in config 2 (an exec dep) and config 1.
        let (graph, names, _) = build(&[result(
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
        let (graph, names, _) = build(&[result(
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
        let (graph, names, _) = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &[], &[("//foo:lib", Some(2))]),
                rule_ct("//foo:lib", 1),
            ],
            vec![config(1, "target")],
        )]);

        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| names.resolve(n.id) == "ct://foo:lib#cfg2")
        );
        assert!(edges(&graph, &names).contains(&("target://foo:lib", "ct://foo:app#ck1")));
    }

    #[test]
    fn dependency_without_a_configuration_lands_on_its_source_leaf() {
        let (graph, names, _) = build(&[result(
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
    fn configured_dependency_on_a_generated_file_lands_on_its_leaf() {
        // A file dependency carries the consumer's configuration id but has no
        // configured target of its own, so the edge reaches its leaf.
        let (graph, names, _) = build(&[result(
            vec![
                with_inputs(rule_ct("//foo:app", 1), &[], &[("//foo:gen.out", Some(1))]),
                generated_ct("//foo:gen.out"),
            ],
            vec![config(1, "target")],
        )]);

        assert_eq!(
            find(&graph, &names, "target://foo:gen.out").kind,
            Kind::Artifact
        );
        assert!(edges(&graph, &names).contains(&("target://foo:gen.out", "ct://foo:app#ck1")));
    }

    #[test]
    fn label_only_rule_input_naming_a_source_lands_on_its_leaf() {
        // rule_input (no configured_rule_input) naming a source file resolves to
        // that source leaf, not a target container.
        let (graph, names, _) = build(&[result(
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
        let (graph, names, _) = build(&[result(
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
