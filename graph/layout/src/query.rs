//! Builds a compound graph from `bazel query --output=streamed_proto` output:
//! one leaf node per target, grouped under its package container, with edges
//! from each rule's inputs to the rule. An input naming a target absent from the
//! stream is left unwired rather than synthesizing a phantom node.

use std::collections::{BTreeSet, HashMap, HashSet};

use bazel_query_proto::blaze_query::{Attribute, Target, attribute::Tristate};

use crate::capability::Axis;
#[cfg(test)]
use crate::facets::Facets;
use crate::facets::{FacetsBuilder, Tri};
use crate::graph::{EdgeKind, Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::label::{self, PackageHierarchy, RepoMapping, label_and_kind};

#[cfg(test)]
fn build(targets: &[Target]) -> (Graph, Names, Facets) {
    build_in(targets, &RepoMapping::default())
}

#[cfg(test)]
fn build_in(targets: &[Target], repos: &RepoMapping) -> (Graph, Names, Facets) {
    let refs: Vec<&Target> = targets.iter().collect();
    let packages = package_keys(targets, repos);
    let mut names = Names::default();
    let mut facets = FacetsBuilder::default();
    let graph = build_with_packages(&refs, &packages, repos, &mut names, &mut facets);
    (graph, names, facets.seal().0)
}

/// Every label this query names.
pub(crate) fn labels(targets: &[Target]) -> impl Iterator<Item = &str> {
    targets
        .iter()
        .filter_map(label_and_kind)
        .map(|(label, _)| label)
}

/// The (repository, package path) keys of every target this query names, for the
/// package hierarchy.
pub(crate) fn package_keys(targets: &[Target], repos: &RepoMapping) -> BTreeSet<(String, String)> {
    label::package_keys(labels(targets), repos)
}

/// The labels of generated-file targets among `targets`, which a joint graph
/// uses to recognize aquery output artifacts that are also targets.
pub(crate) fn generated_labels<'a>(
    targets: impl IntoIterator<Item = &'a Target>,
    repos: &RepoMapping,
) -> HashSet<String> {
    targets
        .into_iter()
        .filter_map(label_and_kind)
        .filter(|(_, kind)| *kind == Kind::Artifact)
        .map(|(label, _)| repos.canonicalize_label(label).into_owned())
        .collect()
}

/// Builds the query graph attaching its boxes to the `packages` hierarchy,
/// which a joint graph passes as the union of every input's packages so shared
/// package boxes carry one label and parent.
pub(crate) fn build_with_packages(
    targets: &[&Target],
    packages: &BTreeSet<(String, String)>,
    repos: &RepoMapping,
    names: &mut Names,
    facets: &mut FacetsBuilder,
) -> Graph {
    let mut graph = Graph::new();
    // Node id per label, so a label seen more than once maps to one node.
    let mut node_by_label: HashMap<String, Id> = HashMap::new();

    let hierarchy = PackageHierarchy::build(packages, &mut graph, names, facets);

    // Pass 1: one leaf node per target under its package container.
    for target in targets {
        let Some((written, kind)) = label_and_kind(target) else {
            continue;
        };
        let label = repos.canonicalize_label(written);
        if node_by_label.contains_key(label.as_ref()) {
            continue;
        }
        let node_id = names.intern(&label::target_node_id(&label));
        let parent = hierarchy.package_box(&label);
        graph.add_node(Node::new(
            node_id,
            label::target_label(&label),
            kind,
            parent,
        ));
        describe_target(facets, target, node_id, parent, &label);
        node_by_label.insert(label.into_owned(), node_id);
    }

    // Pass 2: edges from each rule's inputs (dependency -> dependent).
    let mut seen_edge: HashSet<(Id, Id)> = HashSet::new();
    for target in targets {
        let Some(rule) = target.rule.as_ref() else {
            continue;
        };
        let Some(&to) = node_by_label.get(repos.canonicalize_label(&rule.name).as_ref()) else {
            continue;
        };
        for input_label in &rule.rule_input {
            let Some(&from) = node_by_label.get(repos.canonicalize_label(input_label).as_ref())
            else {
                continue;
            };
            if seen_edge.insert((from, to)) {
                graph.add_edge(from, to, EdgeKind::Query);
            }
        }
    }

    graph
}

/// Records what `target` states about the node it became: the `canonical` label
/// placing it in `package`, and, where the target is a rule, how it was
/// declared.
///
/// A rule's attributes reach the proto only under `--proto:output_rule_attrs`,
/// so a rule stating none is as ordinary as one stating all of them.
pub(crate) fn describe_target(
    facets: &mut FacetsBuilder,
    target: &Target,
    node: Id,
    package: Option<Id>,
    canonical: &str,
) {
    facets.describe_label(node, package, canonical);
    let Some(rule) = target.rule.as_ref() else {
        return;
    };
    let declared = !rule.rule_class.is_empty();
    facets.observe(Axis::RuleClass, declared);
    facets.observe(Axis::RuleCategory, declared);
    if declared {
        let rule_class = facets.facets().intern_rule_class(&rule.rule_class);
        facets.facets().set_rule_class(node, Some(rule_class));
    }
    let mut tagged = false;
    let mut testonly = false;
    for attribute in &rule.attribute {
        match attribute.name.as_str() {
            "tags" => {
                tagged = true;
                facets.set_tags(node, attribute.string_list_value.iter().map(String::as_str));
            }
            "testonly" => {
                testonly = true;
                facets.facets().set_testonly(node, yes_or_no(attribute));
            }
            _ => {}
        }
    }
    facets.observe(Axis::Tags, tagged);
    facets.observe(Axis::Testonly, testonly);
}

/// The value of an attribute that says yes or no, which the proto states as a
/// boolean, or as a tristate for an attribute declared tristate, whose `AUTO`
/// says neither. An attribute whose value is a `select` states none of the
/// three, so it too says neither.
fn yes_or_no(attribute: &Attribute) -> Tri {
    if let Some(value) = attribute.boolean_value {
        return if value { Tri::Yes } else { Tri::No };
    }
    match attribute
        .tristate_value
        .and_then(|v| Tristate::try_from(v).ok())
    {
        Some(Tristate::Yes) => Tri::Yes,
        Some(Tristate::No) => Tri::No,
        Some(Tristate::Auto) | None => Tri::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::{Origin, RuleCategory};
    use crate::repo::RepoName;
    use crate::select::{Filter, Visibility};
    use crate::test_support::{edges, find, parent_of};
    use bazel_query_proto::blaze_query::{
        GeneratedFile, Rule, SourceFile, attribute::Discriminator as AttributeType,
        target::Discriminator,
    };

    fn rule(name: &str, inputs: &[&str]) -> Target {
        Target {
            r#type: Discriminator::Rule as i32,
            rule: Some(Rule {
                name: name.to_string(),
                rule_class: "cc_library".to_string(),
                rule_input: inputs.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn declared(name: &str, rule_class: &str, attributes: Vec<Attribute>) -> Target {
        Target {
            r#type: Discriminator::Rule as i32,
            rule: Some(Rule {
                name: name.to_string(),
                rule_class: rule_class.to_string(),
                attribute: attributes,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn string_list(name: &str, values: &[&str]) -> Attribute {
        Attribute {
            name: name.to_string(),
            r#type: AttributeType::StringList as i32,
            string_list_value: values.iter().map(|v| v.to_string()).collect(),
            ..Default::default()
        }
    }

    fn boolean(name: &str, value: bool) -> Attribute {
        Attribute {
            name: name.to_string(),
            r#type: AttributeType::Boolean as i32,
            boolean_value: Some(value),
            ..Default::default()
        }
    }

    fn tristate(name: &str, value: Tristate) -> Attribute {
        Attribute {
            name: name.to_string(),
            r#type: AttributeType::Tristate as i32,
            tristate_value: Some(value as i32),
            ..Default::default()
        }
    }

    /// The tags `facets` records for `node`, in a stable order.
    fn tags_of(facets: &Facets, node: Id) -> Vec<&str> {
        let mut tags: Vec<&str> = facets
            .tags()
            .values(node)
            .iter()
            .map(|&tag| facets.strings().resolve(tag))
            .collect();
        tags.sort_unstable();
        tags
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
        let (graph, names, _) = build(&[rule("//foo:app", &[])]);

        assert_eq!(find(&graph, &names, "pkg:foo").kind, Kind::Package);
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");
        let node = find(&graph, &names, "target://foo:app");
        assert_eq!(node.kind, Kind::Target);
        assert_eq!(node.label, ":app");
        assert_eq!(
            parent_of(&graph, &names, "target://foo:app"),
            Some("pkg:foo")
        );
    }

    #[test]
    fn source_and_generated_files_get_their_kinds() {
        let (graph, names, _) = build(&[source("//foo:in.c"), generated("//foo:out.o")]);

        assert_eq!(find(&graph, &names, "target://foo:in.c").kind, Kind::Source);
        assert_eq!(
            find(&graph, &names, "target://foo:out.o").kind,
            Kind::Artifact
        );
    }

    #[test]
    fn rule_input_to_present_target_is_an_edge() {
        // Input -> dependent: the producer is `from`, the consuming rule is `to`.
        let (graph, names, _) = build(&[source("//foo:in.c"), rule("//foo:app", &["//foo:in.c"])]);

        assert!(edges(&graph, &names).contains(&("target://foo:in.c", "target://foo:app")));
    }

    #[test]
    fn rule_input_to_absent_label_is_dropped() {
        let (graph, names, _) = build(&[rule("//foo:app", &["//bar:missing"])]);

        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| names.resolve(n.id) == "target://bar:missing")
        );
        assert!(edges(&graph, &names).is_empty());
    }

    #[test]
    fn a_rules_facets_state_how_it_was_declared_and_where_it_sits() {
        let (_, names, facets) = build(&[declared(
            "//foo:app",
            "cc_test",
            vec![
                string_list("tags", &["manual", "flaky"]),
                boolean("testonly", true),
            ],
        )]);

        let node = names.get("target://foo:app").unwrap();
        let rule_class = facets.rule_class(node).unwrap();
        assert_eq!(facets.strings().resolve(rule_class.sym()), "cc_test");
        assert_eq!(facets.rule_category(rule_class), Some(RuleCategory::Test));
        assert_eq!(tags_of(&facets, node), ["flaky", "manual"]);
        assert_eq!(facets.testonly(node), Tri::Yes);
        assert_eq!(facets.package(node), names.get("pkg:foo"));
        assert_eq!(facets.label(node), Some(node));
        let repo = facets.repo(node).unwrap();
        assert_eq!(facets.repo_name(repo), RepoName::Canonical(""));
        assert_eq!(facets.repo_entry(repo).origin(), Origin::Main);
    }

    #[test]
    fn a_testonly_attribute_written_as_a_tristate_reads_the_same() {
        let cases = [
            (Tristate::Yes, Tri::Yes),
            (Tristate::No, Tri::No),
            (Tristate::Auto, Tri::Unknown),
        ];

        for (written, expected) in cases {
            let (_, names, facets) = build(&[declared(
                "//foo:app",
                "cc_library",
                vec![tristate("testonly", written)],
            )]);

            let node = names.get("target://foo:app").unwrap();
            assert_eq!(facets.testonly(node), expected, "{written:?}");
        }
    }

    #[test]
    fn a_file_target_carries_no_rule_class_but_is_still_placed() {
        let (_, names, facets) = build(&[source("//foo:in.c")]);

        let node = names.get("target://foo:in.c").unwrap();
        assert_eq!(facets.rule_class(node), None);
        assert_eq!(facets.testonly(node), Tri::Unknown);
        assert!(tags_of(&facets, node).is_empty());
        assert_eq!(facets.package(node), names.get("pkg:foo"));
        assert_eq!(facets.label(node), Some(node));
        assert!(facets.repo(node).is_some());
    }

    #[test]
    fn a_rule_stating_no_class_offers_the_filter_no_rule_class() {
        let (_, names, facets) = build(&[declared("//foo:app", "", vec![])]);

        let node = names.get("target://foo:app").unwrap();
        assert_eq!(facets.rule_class(node), None);
        assert_eq!(facets.rule_classes().count(), 0);
    }

    #[test]
    fn a_rule_printed_without_its_attributes_states_no_tags_or_testonly() {
        let (_, names, facets) = build(&[declared("//foo:app", "cc_library", vec![])]);

        let node = names.get("target://foo:app").unwrap();
        assert!(facets.rule_class(node).is_some());
        assert!(tags_of(&facets, node).is_empty());
        assert_eq!(facets.testonly(node), Tri::Unknown);
    }

    #[test]
    fn an_apparent_label_records_the_canonical_repository_it_reaches() {
        let repos = RepoMapping::infer(["somerepo"].into_iter());

        let (_, names, facets) = build_in(&[rule("@somerepo//foo:lib", &[])], &repos);

        let node = names.get("target:@@somerepo//foo:lib").unwrap();
        let repo = facets.repo(node).unwrap();
        assert_eq!(facets.repo_name(repo), RepoName::Canonical("somerepo"));
    }

    /// A module name is the word that module's own mapping gives its
    /// repository, and a module depending on another under that word gives it
    /// the same one, so a label writing it identifies a repository of its own
    /// beside the module bearing the name.
    #[test]
    fn a_label_writing_a_module_name_lands_beside_the_module_bearing_it() {
        let repos = RepoMapping::infer(["foo+", "bar+"].into_iter());

        let (_, names, facets) = build_in(
            &[rule("@bar//pkg:t", &[]), rule("@@bar+//pkg:u", &[])],
            &repos,
        );

        let written = facets.repo(names.get("target:@bar//pkg:t").unwrap());
        let module = facets.repo(names.get("target:@@bar+//pkg:u").unwrap());
        assert_ne!(written, module);
        assert_eq!(facets.repos().count(), 2);
    }

    /// A repository no input names canonically is still a repository a reader
    /// filters on, so it is identified by the apparent name, which is all the
    /// inputs said about it. Nothing in that name says where it comes from.
    #[test]
    fn an_unresolved_apparent_repository_name_identifies_a_repository_of_its_own() {
        let repos = RepoMapping::infer(["rules_foo+"].into_iter());

        let (_, names, facets) = build_in(&[rule("@somerepo//foo:lib", &[])], &repos);

        let node = names.get("target:@somerepo//foo:lib").unwrap();
        let repo = facets.repo(node).expect("the repository the label names");
        assert_eq!(facets.repo_name(repo), RepoName::Apparent("somerepo"));
        assert_eq!(facets.repo_entry(repo).origin(), Origin::WorkspaceOrUnknown);
        assert_eq!(facets.repos().count(), 1);
    }

    /// An apparent name stands for whichever repository the module that wrote
    /// it maps it to, which need not be the one Bazel gave that canonical name,
    /// so the two are two repositories a filter takes away one at a time.
    #[test]
    fn an_apparent_name_and_a_canonical_one_reading_alike_are_two_repositories() {
        let repos = RepoMapping::infer(["rules_foo+"].into_iter());

        let (_, names, facets) = build_in(
            &[
                rule("@somerepo//foo:lib", &[]),
                rule("@@somerepo//bar:lib", &[]),
            ],
            &repos,
        );

        let apparent = facets.repo(names.get("target:@somerepo//foo:lib").unwrap());
        let canonical = facets.repo(names.get("target:@@somerepo//bar:lib").unwrap());
        assert_ne!(apparent, canonical);
        assert_eq!(facets.repos().count(), 2);
    }

    /// The repository mapping is inferred from every canonical name the inputs
    /// state before a node id is minted, so a label an input wrote apparently
    /// is already canonical by the time its repository is recorded and lands
    /// under the one identity rather than beside it.
    #[test]
    fn an_apparent_name_a_canonical_one_resolves_lands_under_that_repository() {
        let repos = RepoMapping::infer(["somerepo"].into_iter());

        let (_, names, facets) = build_in(
            &[
                rule("@somerepo//foo:lib", &[]),
                rule("@@somerepo//bar:lib", &[]),
            ],
            &repos,
        );

        let one = facets.repo(names.get("target:@@somerepo//foo:lib").unwrap());
        let other = facets.repo(names.get("target:@@somerepo//bar:lib").unwrap());
        assert_eq!(one, other);
        assert_eq!(facets.repos().count(), 1);
    }

    /// A box belongs to its repository as much as the targets inside it do, so a
    /// rule over repositories takes the whole of one away rather than leaving an
    /// empty box where its packages were.
    #[test]
    fn a_repository_rule_hides_that_repositorys_package_boxes() {
        use crate::select::{Predicate, Values};

        let (graph, names, facets) = build(&[
            rule("//foo:app", &[]),
            rule("@@somerepo+//bar/deep:lib", &[]),
        ]);
        let filter = Filter {
            rules: vec![crate::select::Rule {
                predicate: Predicate {
                    repos: Values::new([RepoName::Canonical("somerepo+".to_string())]),
                    ..Predicate::default()
                },
                visibility: Visibility::Hidden,
            }],
            ..Filter::default()
        };

        let verdicts = filter.evaluate(&graph, &names, &facets);
        let drawn: BTreeSet<&str> = graph
            .nodes
            .iter()
            .filter(|node| verdicts.get(node.id).drawn())
            .map(|node| names.resolve(node.id))
            .collect();
        assert_eq!(drawn, ["pkg:foo", "target://foo:app"].into_iter().collect());
    }

    /// The nodes `filter` draws a box for, by name.
    fn drawn<'a>(
        filter: &Filter,
        graph: &Graph,
        names: &'a Names,
        facets: &Facets,
    ) -> BTreeSet<&'a str> {
        let verdicts = filter.evaluate(graph, names, facets);
        graph
            .nodes
            .iter()
            .filter(|node| verdicts.get(node.id).drawn())
            .map(|node| names.resolve(node.id))
            .collect()
    }

    /// A filter whose one rule acts on everything `text` names.
    fn searching(text: &str, visibility: Visibility) -> Filter {
        use crate::select::{Base, Predicate, Rule, pattern::Pattern};
        Filter {
            base: match visibility {
                Visibility::Shown => Base::HideAll,
                _ => Base::ShowAll,
            },
            rules: vec![Rule {
                predicate: Predicate {
                    label: Some(Pattern::parse(text)),
                    ..Predicate::default()
                },
                visibility,
            }],
            ..Filter::default()
        }
    }

    /// A box stands for a package, so a pattern covering every package of a
    /// repository takes its boxes along with the targets in them — and hiding a
    /// container unwraps it, so nothing of those packages is left drawn.
    #[test]
    fn a_pattern_over_a_whole_repository_hides_its_package_boxes() {
        let (graph, names, facets) = build(&[
            rule("//:root", &[]),
            rule("//foo/bar:app", &[]),
            rule("@@somerepo+//baz:lib", &[]),
        ]);

        assert_eq!(
            drawn(
                &searching("//...", Visibility::Hidden),
                &graph,
                &names,
                &facets
            ),
            ["pkg:@@somerepo+//baz", "target:@@somerepo+//baz:lib"]
                .into_iter()
                .collect()
        );
    }

    /// A pattern and a package path are split at a component boundary, so a
    /// directory pattern reaches the boxes below it and not the box whose name
    /// merely starts with the same letters.
    #[test]
    fn a_directory_pattern_covers_the_boxes_below_it_alone() {
        let (graph, names, facets) =
            build(&[rule("//foo/bar:app", &[]), rule("//foobar:lib", &[])]);

        assert_eq!(
            drawn(
                &searching("//foo/...", Visibility::Hidden),
                &graph,
                &names,
                &facets
            ),
            ["pkg:foobar", "target://foobar:lib"].into_iter().collect()
        );
    }

    /// ':all' names the targets of one package, so the box standing for that
    /// package goes on framing the packages below it — which the pattern names
    /// nothing of, and which would stand at the root of the drawing were their
    /// frame unwrapped.
    #[test]
    fn an_all_pattern_leaves_the_box_above_a_package_it_does_not_name() {
        let (graph, names, facets) = build(&[rule("//foo:app", &[]), rule("//foo/bar:lib", &[])]);

        assert_eq!(
            drawn(
                &searching("//foo:all", Visibility::Hidden),
                &graph,
                &names,
                &facets
            ),
            ["pkg:foo", "pkg:foo/bar", "target://foo/bar:lib"]
                .into_iter()
                .collect()
        );
    }

    /// A directory holding packages without being one itself is drawn as a box
    /// all the same, so that the packages under it are framed together. Bazel
    /// rejects ':all' over such a directory, and the box is left framing the
    /// packages below it whichever way the pattern is read.
    #[test]
    fn an_all_pattern_leaves_the_box_of_a_directory_that_is_no_package() {
        let (graph, names, facets) = build(&[rule("//a/b:x", &[]), rule("//a/c:y", &[])]);

        assert_eq!(
            drawn(
                &searching("//a:all", Visibility::Hidden),
                &graph,
                &names,
                &facets
            ),
            [
                "pkg:a",
                "pkg:a/b",
                "pkg:a/c",
                "target://a/b:x",
                "target://a/c:y"
            ]
            .into_iter()
            .collect()
        );
    }

    /// A trailing '...' reaches a directory and everything under it, so it
    /// covers a box together with every box below that one and unwraps the
    /// whole nest.
    #[test]
    fn a_directory_pattern_covers_a_box_and_the_boxes_below_it() {
        let (graph, names, facets) = build(&[rule("//foo:app", &[]), rule("//foo/bar:lib", &[])]);

        assert!(
            drawn(
                &searching("//foo/...", Visibility::Hidden),
                &graph,
                &names,
                &facets
            )
            .is_empty()
        );
    }

    /// An explicit target pattern names one target of a package and no package,
    /// so the box holding that target goes on framing what is left of it.
    #[test]
    fn a_pattern_naming_one_target_leaves_its_package_box_drawn() {
        let (graph, names, facets) = build(&[rule("//foo:app", &[]), rule("//foo:lib", &[])]);

        assert_eq!(
            drawn(
                &searching("//foo:app", Visibility::Hidden),
                &graph,
                &names,
                &facets
            ),
            ["pkg:foo", "target://foo:lib"].into_iter().collect()
        );
    }

    /// Drawing the matches alone keeps the boxes of the packages the pattern
    /// covers, which is what leaves each match stating the package it sits in,
    /// and leaves out every package it covers nothing of.
    #[test]
    fn showing_only_a_patterns_matches_keeps_the_boxes_around_them() {
        let (graph, names, facets) = build(&[rule("//foo/bar:app", &[]), rule("//baz:lib", &[])]);

        assert_eq!(
            drawn(
                &searching("//foo/...", Visibility::Shown),
                &graph,
                &names,
                &facets
            ),
            ["pkg:foo/bar", "target://foo/bar:app"]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn external_target_nests_in_repo_qualified_package() {
        let (graph, names, _) = build(&[rule("//foo:x", &[]), rule("@somerepo//foo:y", &[])]);

        assert_eq!(parent_of(&graph, &names, "target://foo:x"), Some("pkg:foo"));
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");

        let external = parent_of(&graph, &names, "target:@somerepo//foo:y").unwrap();
        assert_ne!(external, "pkg:foo");
        assert_eq!(find(&graph, &names, external).label, "@somerepo//foo");
    }
}
