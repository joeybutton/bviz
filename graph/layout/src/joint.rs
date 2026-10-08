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

use crate::attributes::{Attributes, AttributesBuilder};
use crate::capability::Capabilities;
use crate::facets::{Facets, FacetsBuilder};
use crate::graph::{Edge, Graph, Node};
use crate::intern::{Id, Names};
use crate::label::{self, RepoMapping, canonical_repo};
use crate::provenance::{Input, InputSet, Provenance};
use crate::{aquery, cquery, query};

/// A joint graph and what a view of it reads alongside.
pub struct Joint {
    pub graph: Graph,
    /// The names the graph's ids resolve through.
    pub names: Names,
    /// The facets a filter selects on.
    pub facets: Facets,
    /// Which axes those facets let a filter select by.
    pub capabilities: Capabilities,
    /// The attributes each rule's targets and configured targets state.
    pub attributes: Attributes,
    pub provenance: Provenance,
}

/// Merges the inputs given into one joint graph.
///
/// # Panics
///
/// Where no input is given, since a joint graph is read from at least one.
pub fn build_joint(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
    external_sources: bool,
) -> Joint {
    let repos = repo_mapping(aquery, cquery, query);
    let union = package_union(aquery, cquery, query, &repos);
    let labeled_outputs = labeled_outputs(cquery, query, &repos);
    let mut names = Names::default();
    let mut facets = FacetsBuilder::default();
    let mut attributes = AttributesBuilder::default();
    let graphs = build_inputs(
        aquery,
        cquery,
        query,
        external_sources,
        &union,
        &labeled_outputs,
        &repos,
        &mut names,
        &mut facets,
        &mut attributes,
    );
    let loaded = InputSet::of(graphs.iter().map(|&(input, _)| input))
        .expect("a joint graph is read from at least one input");
    let (facets, capabilities) = facets.seal(loaded);
    let (mut graph, names, facets, attributes, provenance) = canonicalize(
        merge(graphs, names.len()),
        &names,
        facets,
        attributes.seal(),
    );
    label::show_written_repositories(&mut graph, &facets);
    Joint {
        graph,
        names,
        facets,
        capabilities,
        attributes,
        provenance,
    }
}

/// The apparent-to-canonical repository mapping inferred from every canonical
/// repository name the inputs state outright. aquery always writes them, and
/// query and cquery do under `--consistent_labels`.
fn repo_mapping(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
) -> RepoMapping {
    let observed = aquery
        .into_iter()
        .flat_map(aquery::labels)
        .chain(cquery.into_iter().flat_map(cquery::labels))
        .chain(query.into_iter().flat_map(query::labels))
        .filter_map(canonical_repo);
    RepoMapping::infer(observed)
}

fn package_union(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
    repos: &RepoMapping,
) -> BTreeSet<(String, String)> {
    let mut union: BTreeSet<(String, String)> = BTreeSet::new();
    if let Some(a) = aquery {
        union.extend(aquery::package_keys(a, repos));
    }
    if let Some(c) = cquery {
        union.extend(cquery::package_keys(c, repos));
    }
    if let Some(q) = query {
        union.extend(query::package_keys(q, repos));
    }
    union
}

/// Labels of generated files that query or cquery knows as targets, so aquery
/// promotes the matching output artifacts to merge with those target nodes.
fn labeled_outputs(
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
    repos: &RepoMapping,
) -> HashSet<String> {
    let mut labeled_outputs: HashSet<String> = HashSet::new();
    if let Some(c) = cquery {
        labeled_outputs.extend(cquery::generated_labels(c, repos));
    }
    if let Some(q) = query {
        labeled_outputs.extend(query::generated_labels(q, repos));
    }
    labeled_outputs
}

#[allow(clippy::too_many_arguments)]
fn build_inputs(
    aquery: Option<&[ActionGraphContainer]>,
    cquery: Option<&[CqueryResult]>,
    query: Option<&[Target]>,
    external_sources: bool,
    union: &BTreeSet<(String, String)>,
    labeled_outputs: &HashSet<String>,
    repos: &RepoMapping,
    names: &mut Names,
    facets: &mut FacetsBuilder,
    attributes: &mut AttributesBuilder,
) -> Vec<(Input, Graph)> {
    let mut graphs: Vec<(Input, Graph)> = Vec::with_capacity(Input::ALL.len());
    if let Some(a) = aquery {
        let graph = aquery::build_with_packages(
            a,
            external_sources,
            union,
            labeled_outputs,
            repos,
            names,
            facets,
        );
        graphs.push((Input::Aquery, graph));
    }
    if let Some(c) = cquery {
        let graph = cquery::build_with_packages(c, union, repos, names, facets, attributes);
        graphs.push((Input::Cquery, graph));
    }
    if let Some(q) = query {
        let refs: Vec<&Target> = q.iter().collect();
        let graph = query::build_with_packages(&refs, union, repos, names, facets, attributes);
        graphs.push((Input::Query, graph));
    }
    graphs
}

/// Re-numbers a graph's ids into lexicographic order of their names, so a graph
/// built from differently ordered inputs gets identical ids. Bazel's aquery proto
/// output is not order-stable, and the level-of-detail pass breaks reveal-count
/// ties between candidate containers by id, so without a name-ordered id the
/// startup view would vary run to run. Each node also then sits at the position of
/// its own id (`nodes[i].id == Id(i)`), the dense indexing that pass relies on.
fn canonicalize(
    merged: Merged,
    names: &Names,
    mut facets: Facets,
    mut attributes: Attributes,
) -> (Graph, Names, Facets, Attributes, Provenance) {
    let mut order: Vec<u32> = (0..names.len() as u32).collect();
    order.sort_by(|&a, &b| names.resolve(Id(a)).cmp(names.resolve(Id(b))));
    let mut remap = vec![Id(0); names.len()];
    let mut canon = Names::default();
    for &old in &order {
        remap[old as usize] = canon.intern(names.resolve(Id(old)));
    }
    let map = |id: Id| remap[id.0 as usize];

    let mut by_id: Vec<Option<(&Node, InputSet)>> = vec![None; names.len()];
    for (node, inputs) in &merged.nodes {
        by_id[node.id.0 as usize] = Some((node, *inputs));
    }
    let mut out = Graph::new();
    let mut built_by: Vec<InputSet> = Vec::with_capacity(order.len());
    for &old in &order {
        let (node, inputs) = by_id[old as usize].expect("every id names a node");
        out.add_node(Node::new(
            map(node.id),
            node.label.clone(),
            node.kind,
            node.parent.map(map),
        ));
        built_by.push(inputs);
    }
    for edge in &merged.edges {
        out.add_edge(map(edge.from), map(edge.to), edge.kind);
    }
    facets.renumber(&remap);
    attributes.renumber(&remap);
    (out, canon, facets, attributes, Provenance::new(built_by))
}

/// The union of the inputs' graphs.
struct Merged {
    /// Each node, with the inputs that built it.
    nodes: Vec<(Node, InputSet)>,
    edges: Vec<Edge>,
}

/// Unions the graphs into one, whose ids are all below `ids`. Nodes are deduped
/// by id: a package, target, configured target, or source that several inputs
/// name appears in each input's graph with an identical kind, label, and parent,
/// so the first wins. Edges are concatenated without deduping: every builder
/// already drops its own duplicate edges, and each tags its edges with its own
/// [`EdgeKind`], so no edge from one input can equal one from another.
fn merge(graphs: Vec<(Input, Graph)>, ids: usize) -> Merged {
    let nodes = graphs.iter().map(|(_, graph)| graph.nodes.len()).sum();
    let edges = graphs.iter().map(|(_, graph)| graph.edges.len()).sum();
    let mut merged = Merged {
        nodes: Vec::with_capacity(nodes),
        edges: Vec::with_capacity(edges),
    };
    // By id, the node's index among the merged nodes.
    let mut position: Vec<Option<u32>> = vec![None; ids];
    for (input, graph) in graphs {
        for node in graph.nodes {
            let at = &mut position[node.id.0 as usize];
            match *at {
                Some(at) => {
                    let (_, inputs) = &mut merged.nodes[at as usize];
                    *inputs = inputs.with(input);
                }
                None => {
                    *at = Some(merged.nodes.len() as u32);
                    merged.nodes.push((node, input.into()));
                }
            }
        }
        merged.edges.extend(graph.edges);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attributes::Stated;
    use crate::capability::{Axis, Support};
    use crate::graph::{EdgeKind, Kind};
    use crate::label::Resolver;
    use crate::test_support::find;
    use bazel_query_proto::analysis::{
        Action, Artifact, Configuration, ConfiguredTarget, DepSetOfFiles, PathFragment,
        Target as ActionTarget,
    };
    use bazel_query_proto::blaze_query::{
        Attribute, GeneratedFile, Rule, SourceFile, attribute::Discriminator as AttributeType,
        target::Discriminator,
    };

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

        let Joint {
            graph,
            names,
            provenance,
            ..
        } = build_joint(Some(&[aquery_container]), None, Some(&query_targets), false);

        // The target //foo:lib appears once though both inputs name it, and
        // carries both as its provenance.
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| names.resolve(n.id) == "target://foo:lib")
                .count(),
            1
        );
        let built_by = |name: &str| provenance.of(names.get(name).unwrap());
        assert_eq!(
            built_by("target://foo:lib"),
            InputSet::of([Input::Query, Input::Aquery])
        );
        assert_eq!(built_by("target://foo:app"), Some(Input::Query.into()));
        assert_eq!(built_by("action0"), Some(Input::Aquery.into()));
        // The query dependency edge and an aquery input edge both survive.
        let edges = edges(&graph, &names);
        assert!(edges.iter().any(|(_, _, k)| *k == EdgeKind::Query));
        assert!(edges.iter().any(|(_, _, k)| *k == EdgeKind::Aquery));
        // The package box appears once.
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| names.resolve(n.id) == "pkg:foo")
                .count(),
            1
        );
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

        let Joint { graph, names, .. } =
            build_joint(Some(&[aquery_container]), None, Some(&query_targets), false);

        // The promoted target node is shared by query and aquery: it appears once.
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| names.resolve(n.id) == "target://pkg:gen.out")
                .count(),
            1
        );
        // aquery's promotion contributes the per-config box and concrete artifact.
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| names.resolve(n.id) == "ct://pkg:gen.out#ck1")
        );
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| names.resolve(n.id) == "artifact://pkg:gen.out#ck1")
        );
    }

    #[test]
    fn external_repository_output_merges_across_query_and_aquery() {
        // query knows @@somerepo+//pkg:gen.out as a generated-file target;
        // aquery produces it under that repository's output tree.
        let query_targets = vec![Target {
            r#type: Discriminator::GeneratedFile as i32,
            generated_file: Some(GeneratedFile {
                name: "@@somerepo+//pkg:gen.out".into(),
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
                    label: "external/somerepo+/pkg/gen.out".into(),
                    parent_id: 102,
                },
            ],
            targets: vec![ActionTarget {
                id: 9,
                label: "@@somerepo+//pkg:gen".into(),
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

        let Joint { graph, names, .. } =
            build_joint(Some(&[aquery_container]), None, Some(&query_targets), false);

        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| names.resolve(n.id) == "target:@@somerepo+//pkg:gen.out")
                .count(),
            1
        );
        let concrete = find(&graph, &names, "artifact:@@somerepo+//pkg:gen.out#ck1");
        assert_eq!(
            concrete.parent.map(|p| names.resolve(p)),
            Some("ct:@@somerepo+//pkg:gen.out#ck1")
        );
    }

    /// An aquery container whose one target is `label` under target id `id`,
    /// with a single action so the target reaches the graph. Target ids are
    /// shared across containers, so containers combined in one query need
    /// distinct ones.
    fn aquery_target(id: u32, label: &str) -> ActionGraphContainer {
        ActionGraphContainer {
            targets: vec![ActionTarget {
                id,
                label: label.to_string(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: id,
                mnemonic: "Compile".to_string(),
                configuration_id: 1,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn node_count(graph: &Graph, names: &Names, id: &str) -> usize {
        graph
            .nodes
            .iter()
            .filter(|n| names.resolve(n.id) == id)
            .count()
    }

    #[test]
    fn the_main_repositorys_two_label_forms_are_one_node() {
        // `--consistent_labels` writes the main repository as `@@//foo:app`,
        // where aquery writes the same target as `//foo:app`.
        let query_targets = vec![rule("@@//foo:app", &[])];

        let Joint { graph, names, .. } = build_joint(
            Some(&[aquery_target(1, "//foo:app")]),
            None,
            Some(&query_targets),
            false,
        );

        assert_eq!(node_count(&graph, &names, "target://foo:app"), 1);
        assert_eq!(node_count(&graph, &names, "target:@@//foo:app"), 0);
        assert_eq!(node_count(&graph, &names, "pkg:foo"), 1);
    }

    #[test]
    fn an_apparent_repository_name_joins_the_canonical_node_it_stands_for() {
        // query prints the apparent `@somerepo//` and aquery the canonical
        // `@@somerepo//` of the repository bearing that very name, so both name
        // one target.
        let query_targets = vec![rule("@somerepo//foo:lib", &[])];

        let Joint { graph, names, .. } = build_joint(
            Some(&[aquery_target(1, "@@somerepo//foo:lib")]),
            None,
            Some(&query_targets),
            false,
        );

        assert_eq!(node_count(&graph, &names, "target:@@somerepo//foo:lib"), 1);
        assert_eq!(node_count(&graph, &names, "target:@somerepo//foo:lib"), 0);
        assert_eq!(node_count(&graph, &names, "pkg:@@somerepo//foo"), 1);
    }

    #[test]
    fn a_module_name_an_apparent_label_wrote_keeps_its_own_node() {
        // `@somerepo//` is whatever the module that printed it maps that name
        // to, which need not be the module named `somerepo` the aquery names.
        // Showing the target twice beats attaching it to the wrong repository.
        let query_targets = vec![rule("@somerepo//foo:lib", &[])];

        let Joint { graph, names, .. } = build_joint(
            Some(&[aquery_target(1, "@@somerepo+//foo:lib")]),
            None,
            Some(&query_targets),
            false,
        );

        assert_eq!(node_count(&graph, &names, "target:@somerepo//foo:lib"), 1);
        assert_eq!(node_count(&graph, &names, "target:@@somerepo+//foo:lib"), 1);
    }

    /// An aquery container whose one target is `label` under target id 9,
    /// built under the configuration `checksum` names by one action writing
    /// `bazel-out/bin/{output}`.
    fn aquery_generating(label: &str, checksum: &str, output: &str) -> ActionGraphContainer {
        ActionGraphContainer {
            artifacts: vec![Artifact {
                id: 1,
                path_fragment_id: 102,
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
                    label: "bin".into(),
                    parent_id: 100,
                },
                PathFragment {
                    id: 102,
                    label: output.into(),
                    parent_id: 101,
                },
            ],
            targets: vec![ActionTarget {
                id: 9,
                label: label.into(),
                ..Default::default()
            }],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8".into(),
                checksum: checksum.into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 9,
                mnemonic: "Genrule".into(),
                configuration_id: 1,
                output_ids: vec![1],
                primary_output_id: 1,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn a_main_repository_label_resolves_in_either_form_bazel_prints() {
        let Joint { names, .. } = build_joint(
            Some(&[aquery_generating("//pkg:gen", "ck1", "pkg/gen.out")]),
            None,
            Some(&[rule("@@//pkg:gen", &[])]),
            false,
        );
        let resolver = Resolver::new(&names);

        let target = names.get("target://pkg:gen");
        assert!(target.is_some());
        assert_eq!(resolver.target("//pkg:gen"), target);
        assert_eq!(resolver.target("@@//pkg:gen"), target);
        assert_eq!(resolver.target("//pkg:other"), None);

        let configured = names.get("ct://pkg:gen#ck1");
        assert!(configured.is_some());
        assert_eq!(resolver.configured_target("//pkg:gen", "ck1"), configured);
        assert_eq!(resolver.configured_target("@@//pkg:gen", "ck1"), configured);
        assert_eq!(resolver.configured_target("//pkg:gen", "ck2"), None);
    }

    #[test]
    fn an_external_repository_label_resolves_by_its_canonical_form() {
        let Joint { names, .. } = build_joint(
            Some(&[aquery_generating(
                "@@somerepo+//pkg:gen",
                "ck1",
                "external/somerepo+/pkg/gen.out",
            )]),
            None,
            None,
            false,
        );
        let resolver = Resolver::new(&names);

        let target = names.get("target:@@somerepo+//pkg:gen");
        assert!(target.is_some());
        assert_eq!(resolver.target("@@somerepo+//pkg:gen"), target);
        // The module's name is some module's own word for a repository, which
        // need not be this one.
        assert_eq!(resolver.target("@somerepo//pkg:gen"), None);
        assert_eq!(
            resolver.configured_target("@@somerepo+//pkg:gen", "ck1"),
            names.get("ct:@@somerepo+//pkg:gen#ck1")
        );
    }

    #[test]
    fn an_action_resolves_by_the_exec_path_of_its_primary_output() {
        let Joint { graph, names, .. } = build_joint(
            Some(&[aquery_generating("//pkg:gen", "ck1", "pkg/gen.out")]),
            None,
            None,
            false,
        );
        let resolver = Resolver::new(&names);

        let action = resolver
            .action("bazel-out/bin/pkg/gen.out")
            .expect("the action writing the output");
        assert_eq!(graph.nodes[action.0 as usize].kind, Kind::Action);
        assert_eq!(resolver.action("bazel-out/bin/pkg/other.out"), None);
    }

    #[test]
    fn the_capabilities_count_the_inputs_the_graph_was_read_from() {
        let joint = build_joint(
            Some(&[aquery_generating("//pkg:gen", "ck1", "pkg/gen.out")]),
            None,
            Some(&[rule("//pkg:gen", &[])]),
            false,
        );
        assert_eq!(
            joint.capabilities.inputs(),
            InputSet::of([Input::Query, Input::Aquery]).unwrap()
        );
    }

    #[test]
    fn an_apparent_label_resolves_to_whichever_spelling_the_graph_took() {
        // The first repository bears the apparent name as its canonical one,
        // which the aquery states, so its node is canonical; nothing names the
        // second canonically, so its node keeps the name as written.
        let Joint { names, .. } = build_joint(
            Some(&[aquery_target(1, "@@somerepo//foo:lib")]),
            None,
            Some(&[
                rule("@somerepo//foo:lib", &[]),
                rule("@other//bar:lib", &[]),
            ]),
            false,
        );
        let resolver = Resolver::new(&names);

        let canonical = names.get("target:@@somerepo//foo:lib");
        assert!(canonical.is_some());
        assert_eq!(resolver.target("@somerepo//foo:lib"), canonical);
        assert_eq!(resolver.target("@@somerepo//foo:lib"), canonical);

        let apparent = names.get("target:@other//bar:lib");
        assert!(apparent.is_some());
        assert_eq!(resolver.target("@other//bar:lib"), apparent);
        assert_eq!(resolver.target("@@other//bar:lib"), None);
    }

    #[test]
    fn a_configuration_both_an_aquery_and_a_cquery_name_is_one_entry() {
        // The two layers number their configuration ids independently — here 4
        // and 1 for the one configuration — so only the checksum joins them.
        let cquery_results = vec![CqueryResult {
            results: vec![ConfiguredTarget {
                target: Some(Target {
                    r#type: Discriminator::Rule as i32,
                    rule: Some(Rule {
                        name: "//foo:app".into(),
                        rule_class: "cc_binary".into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                configuration_id: 4,
                ..Default::default()
            }],
            configurations: vec![Configuration {
                id: 4,
                mnemonic: "k8-fastbuild".into(),
                checksum: "shared".into(),
                ..Default::default()
            }],
        }];
        let aquery_container = ActionGraphContainer {
            targets: vec![ActionTarget {
                id: 1,
                label: "//bar:lib".into(),
                ..Default::default()
            }],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8-fastbuild".into(),
                checksum: "shared".into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 1,
                mnemonic: "CppCompile".into(),
                configuration_id: 1,
                ..Default::default()
            }],
            ..Default::default()
        };

        let Joint { names, facets, .. } = build_joint(
            Some(&[aquery_container]),
            Some(&cquery_results),
            None,
            false,
        );

        let from_cquery = facets.config(names.get("ct://foo:app#shared").unwrap());
        let from_aquery = facets.config(names.get("ct://bar:lib#shared").unwrap());
        assert_eq!(from_cquery, from_aquery);
        let config = from_cquery.unwrap();
        assert_eq!(facets.strings().resolve(config.sym()), "shared");
        assert_eq!(
            facets
                .config_entry(config)
                .unwrap()
                .mnemonic
                .map(|name| facets.strings().resolve(name)),
            Some("k8-fastbuild")
        );
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

        let Joint { graph, names, .. } =
            build_joint(Some(&[aquery_container]), None, Some(&query_targets), false);

        assert_eq!(find(&graph, &names, "pkg:a").label, "//a");
        let nested = find(&graph, &names, "pkg:a/b");
        assert_eq!(nested.label, "b");
        assert_eq!(nested.parent.map(|p| names.resolve(p)), Some("pkg:a"));
    }

    /// A rule target with the attributes `attributes` printed for it.
    fn attributed(name: &str, attributes: Vec<Attribute>) -> Target {
        Target {
            r#type: Discriminator::Rule as i32,
            rule: Some(Rule {
                name: name.to_string(),
                rule_class: "cc_library".to_string(),
                attribute: attributes,
                ..Default::default()
            }),
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

    /// A cquery result for the rule `name` in the configuration `config_id`,
    /// alongside the configurations the result declares.
    fn cquery_rule(name: &str, config_id: u32, configurations: Vec<Configuration>) -> CqueryResult {
        CqueryResult {
            results: vec![ConfiguredTarget {
                target: Some(rule(name, &[])),
                configuration_id: config_id,
                ..Default::default()
            }],
            configurations,
        }
    }

    /// A target holds what the query dump states of the rule, unconfigured,
    /// and a configured target what the cquery dump states, configured.
    #[test]
    fn a_target_and_its_configured_target_each_hold_their_own_inputs_attributes() {
        let query_targets = vec![attributed("//foo:app", vec![tags(&["unconfigured"])])];
        let configured = attributed("//foo:app", vec![tags(&["configured"])]);
        let cquery_results = vec![CqueryResult {
            results: vec![ConfiguredTarget {
                target: Some(configured),
                configuration_id: 1,
                ..Default::default()
            }],
            configurations: vec![Configuration {
                id: 1,
                checksum: "c0ffee".into(),
                ..Default::default()
            }],
        }];

        let Joint {
            names, attributes, ..
        } = build_joint(None, Some(&cquery_results), Some(&query_targets), false);

        let tags_of = |node: &str| {
            let name = attributes.name("tags").unwrap();
            let Stated::Row(row) = attributes.stated(names.get(node).unwrap(), Some(name)) else {
                panic!("{node} states no tags");
            };
            let mut printed = String::new();
            attributes.any_printed(&attributes.row(row).value, |text| {
                printed = text.to_string();
                true
            });
            printed
        };
        assert_eq!(tags_of("target://foo:app"), "[unconfigured]");
        assert_eq!(tags_of("ct://foo:app#c0ffee"), "[configured]");
    }

    #[test]
    fn a_configured_target_naming_no_configuration_leaves_the_axis_short() {
        let cquery_results = vec![cquery_rule("//foo:app", 0, Vec::new())];

        let Joint { capabilities, .. } = build_joint(None, Some(&cquery_results), None, false);

        assert_eq!(
            capabilities.support(Axis::Configuration),
            Support::Partial {
                covered: 0,
                relevant: 1,
                flag: None
            }
        );
    }

    /// aquery names no configuration per dependency, so the axis has nothing to
    /// be short of rather than a cquery flag to ask for.
    #[test]
    fn an_aquery_alone_asks_for_no_dependency_configuration_flag() {
        let aquery_container = ActionGraphContainer {
            targets: vec![ActionTarget {
                id: 1,
                label: "//foo:app".into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 1,
                mnemonic: "CppCompile".into(),
                configuration_id: 1,
                ..Default::default()
            }],
            ..Default::default()
        };

        let Joint { capabilities, .. } = build_joint(Some(&[aquery_container]), None, None, false);

        assert_eq!(
            capabilities.support(Axis::ExecConfiguration),
            Support::Available
        );
    }

    #[test]
    fn an_aquery_whose_actions_ran_under_no_aspect_still_filters_on_aspects() {
        // Every action lists the aspects that produced it, so a build that ran
        // none supports the axis with nothing in it: no flag is missing.
        let aquery_container = ActionGraphContainer {
            targets: vec![ActionTarget {
                id: 1,
                label: "//foo:app".into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 1,
                mnemonic: "CppCompile".into(),
                configuration_id: 1,
                ..Default::default()
            }],
            ..Default::default()
        };

        let Joint {
            names,
            facets,
            capabilities,
            ..
        } = build_joint(Some(&[aquery_container]), None, None, false);

        assert_eq!(capabilities.support(Axis::Aspect), Support::Available);
        let action = names.get("action0").unwrap();
        assert!(facets.aspects().values(action).is_empty());
    }

    #[test]
    fn a_query_printed_without_attributes_cannot_filter_on_tags_or_testonly() {
        let query_targets = vec![attributed("//foo:app", Vec::new())];

        let Joint { capabilities, .. } = build_joint(None, None, Some(&query_targets), false);

        let missing = Support::Unavailable {
            flag: "--proto:output_rule_attrs",
        };
        assert_eq!(capabilities.support(Axis::Tags), missing);
        assert_eq!(capabilities.support(Axis::Testonly), missing);
        // The rule class reaches the proto whatever the attributes do.
        assert_eq!(capabilities.support(Axis::RuleClass), Support::Available);
    }

    /// `--proto:output_rule_attrs` names the attributes to print, so a rule can
    /// carry one axis's attribute while carrying nothing for the other.
    #[test]
    fn an_attribute_list_holding_tags_alone_leaves_only_testonly_unfilterable() {
        let query_targets = vec![attributed("//foo:app", vec![tags(&["manual"])])];

        let Joint { capabilities, .. } = build_joint(None, None, Some(&query_targets), false);

        assert_eq!(capabilities.support(Axis::Tags), Support::Available);
        assert_eq!(
            capabilities.support(Axis::Testonly),
            Support::Unavailable {
                flag: "--proto:output_rule_attrs"
            }
        );
    }

    #[test]
    fn an_axis_only_some_targets_state_is_reported_with_its_reach() {
        let query_targets = vec![
            attributed("//foo:app", vec![tags(&["manual"])]),
            attributed("//foo:lib", Vec::new()),
        ];

        let Joint { capabilities, .. } = build_joint(None, None, Some(&query_targets), false);

        assert_eq!(
            capabilities.support(Axis::Tags),
            Support::Partial {
                covered: 1,
                relevant: 2,
                flag: Some("--proto:output_rule_attrs"),
            }
        );
    }
}
