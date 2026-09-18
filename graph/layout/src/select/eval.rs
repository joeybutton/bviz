//! Evaluating a [`Filter`] over every node of a graph at once.
//!
//! Each rule is resolved against the graph's facet tables before the nodes are
//! scanned, so that a term over strings becomes a set of handles or a column
//! read by id and testing one node costs a few integer lookups.

use crate::facets::{Facets, SparseFacet, Sym, Tri};
use crate::graph::{Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::label;
use crate::select::{Aspects, Filter, Predicate, Rule, Values, Visibility, stating};

impl Filter {
    /// The visibility of every node of `graph`, whose facets are `facets` and
    /// whose labels resolve through `names`.
    pub fn evaluate(&self, graph: &Graph, names: &Names, facets: &Facets) -> Verdicts {
        let ids = graph
            .nodes
            .iter()
            .map(|node| node.id.0 as usize + 1)
            .max()
            .unwrap_or(0);
        let rules: Vec<Resolved> = self
            .rules
            .iter()
            .map(|rule| Resolved::of(rule, names, facets, ids))
            .collect();

        let unmatched = self.base.visibility();
        let mut by_id = vec![unmatched; ids];
        for node in &graph.nodes {
            if let Some(rule) = rules.iter().rev().find(|rule| rule.matches(node, facets)) {
                by_id[node.id.0 as usize] = rule.visibility;
            }
        }
        for (&id, &pinned) in &self.pins {
            if let Some(verdict) = by_id.get_mut(id.0 as usize) {
                *verdict = pinned;
            }
        }
        Verdicts { by_id, unmatched }
    }
}

/// What a filter made of every node of one graph, by node id.
#[derive(Clone, Debug)]
pub struct Verdicts {
    by_id: Vec<Visibility>,
    unmatched: Visibility,
}

impl Verdicts {
    /// The verdict for the node `id` names. An id the evaluated graph does not
    /// name reads as the verdict of a node no rule reached.
    pub fn get(&self, id: Id) -> Visibility {
        self.by_id
            .get(id.0 as usize)
            .copied()
            .unwrap_or(self.unmatched)
    }
}

/// One rule with its predicate resolved against the graph being evaluated.
///
/// The terms reading one facet are resolved together, since the values a term
/// names and the categories another admits constrain that one facet and the
/// handles passing both are settled here rather than per node.
struct Resolved<'a> {
    kinds: Option<&'a Values<Kind>>,
    testonly: Option<Tri>,
    repos: Option<Syms>,
    rule_classes: Option<Syms>,
    configs: Option<Syms>,
    mnemonics: Option<Syms>,
    tags: Option<Column>,
    aspects: Option<Column>,
    /// Read by the id of the target a node belongs to, which several nodes
    /// share, rather than by the node's own id.
    labels: Option<Column>,
    visibility: Visibility,
}

impl<'a> Resolved<'a> {
    fn of(rule: &'a Rule, names: &Names, facets: &Facets, ids: usize) -> Self {
        let predicate = &rule.predicate;
        Resolved {
            kinds: predicate.kinds.as_ref(),
            testonly: predicate.testonly.map(stating),
            repos: repos(predicate, facets),
            rule_classes: rule_classes(predicate, facets),
            configs: configs(predicate, facets),
            mnemonics: predicate
                .mnemonics
                .as_ref()
                .map(|mnemonics| interned(mnemonics, facets)),
            tags: predicate.tags.as_ref().map(|tags| {
                let admitted = interned(tags, facets);
                column(facets.tags(), ids, false, |values| {
                    values.iter().any(|&tag| admitted.holds(tag))
                })
            }),
            aspects: predicate
                .aspects
                .as_ref()
                .map(|aspects| aspect_column(aspects, facets, ids)),
            labels: label_column(predicate, names),
            visibility: rule.visibility,
        }
    }

    fn matches(&self, node: &Node, facets: &Facets) -> bool {
        let id = node.id;
        self.kinds.is_none_or(|kinds| kinds.contains(&node.kind))
            && self
                .testonly
                .is_none_or(|testonly| facets.testonly(id) == testonly)
            && self
                .repos
                .as_ref()
                .is_none_or(|repos| facets.repo(id).is_some_and(|repo| repos.holds(repo.sym())))
            && self.rule_classes.as_ref().is_none_or(|rule_classes| {
                facets
                    .rule_class(id)
                    .is_some_and(|rule_class| rule_classes.holds(rule_class.sym()))
            })
            && self.configs.as_ref().is_none_or(|configs| {
                facets
                    .config(id)
                    .is_some_and(|config| configs.holds(config.sym()))
            })
            && self.mnemonics.as_ref().is_none_or(|mnemonics| {
                facets
                    .mnemonic(id)
                    .is_some_and(|mnemonic| mnemonics.holds(mnemonic))
            })
            && self.tags.as_ref().is_none_or(|tags| tags.holds(id))
            && self
                .aspects
                .as_ref()
                .is_none_or(|aspects| aspects.holds(id))
            && self
                .labels
                .as_ref()
                .is_none_or(|labels| facets.label(id).is_some_and(|label| labels.holds(label)))
    }
}

/// The facet strings a term admits, by handle. Empty where every value the term
/// names is one this graph never interned, which admits no node at all.
struct Syms(Vec<Sym>);

impl Syms {
    fn of(syms: impl Iterator<Item = Sym>) -> Self {
        let mut syms: Vec<Sym> = syms.collect();
        syms.sort_unstable();
        syms.dedup();
        Syms(syms)
    }

    fn holds(&self, sym: Sym) -> bool {
        self.0.binary_search(&sym).is_ok()
    }
}

/// A term's answer for each id, settled before the nodes are scanned.
struct Column {
    held: Vec<bool>,
    /// The answer for an id past the ones the column was built over, which is
    /// the answer for a node carrying nothing of the facet the term reads.
    beyond: bool,
}

impl Column {
    fn holds(&self, id: Id) -> bool {
        self.held.get(id.0 as usize).copied().unwrap_or(self.beyond)
    }
}

/// The handles of the values `values` names, dropping every name this graph
/// never interned.
fn interned(values: &Values<String>, facets: &Facets) -> Syms {
    Syms::of(
        values
            .as_slice()
            .iter()
            .filter_map(|value| facets.strings().get(value)),
    )
}

fn repos(predicate: &Predicate, facets: &Facets) -> Option<Syms> {
    let (names, categories) = (predicate.repos.as_ref(), predicate.repo_categories.as_ref());
    if names.is_none() && categories.is_none() {
        return None;
    }
    Some(Syms::of(
        facets
            .repos()
            .filter(|(repo, entry)| {
                names.is_none_or(|names| names.contains(facets.strings().resolve(repo.sym())))
                    && categories
                        .is_none_or(|categories| categories.contains(&entry.origin.category()))
            })
            .map(|(repo, _)| repo.sym()),
    ))
}

fn rule_classes(predicate: &Predicate, facets: &Facets) -> Option<Syms> {
    let (names, categories) = (
        predicate.rule_classes.as_ref(),
        predicate.rule_categories.as_ref(),
    );
    if names.is_none() && categories.is_none() {
        return None;
    }
    Some(Syms::of(
        facets
            .rule_classes()
            .filter(|(rule_class, category)| {
                names.is_none_or(|names| names.contains(facets.strings().resolve(rule_class.sym())))
                    && categories.is_none_or(|categories| categories.contains(category))
            })
            .map(|(rule_class, _)| rule_class.sym()),
    ))
}

fn configs(predicate: &Predicate, facets: &Facets) -> Option<Syms> {
    let (checksums, is_tool) = (predicate.configs.as_ref(), predicate.is_tool);
    if checksums.is_none() && is_tool.is_none() {
        return None;
    }
    Some(Syms::of(
        facets
            .configs()
            .filter(|(config, entry)| {
                checksums.is_none_or(|checksums| {
                    checksums.contains(facets.strings().resolve(config.sym()))
                }) && is_tool.is_none_or(|is_tool| entry.is_tool == stating(is_tool))
            })
            .map(|(config, _)| config.sym()),
    ))
}

fn aspect_column(aspects: &Aspects, facets: &Facets, ids: usize) -> Column {
    match aspects {
        Aspects::Any => column(facets.aspects(), ids, false, |values| !values.is_empty()),
        Aspects::None => column(facets.aspects(), ids, true, |values| values.is_empty()),
        Aspects::Named(named) => {
            let admitted = interned(named, facets);
            column(facets.aspects(), ids, false, |values| {
                values.iter().any(|&value| admitted.holds(value))
            })
        }
    }
}

/// A column over `ids` answering `carried` for every node `facet` records values
/// for and `bare` for every other node, which carries none.
fn column(facet: &SparseFacet, ids: usize, bare: bool, carried: impl Fn(&[Sym]) -> bool) -> Column {
    let mut held = vec![bare; ids];
    for (id, values) in facet.rows() {
        if let Some(slot) = held.get_mut(id.0 as usize) {
            *slot = carried(values);
        }
    }
    Column { held, beyond: bare }
}

/// A column over every name the graph interned, answering whether the label
/// terms hold of the target that name is the node id of. A name that is no
/// target's node id names no label, and so answers no label term.
fn label_column(predicate: &Predicate, names: &Names) -> Option<Column> {
    let (pattern, package) = (predicate.label.as_ref(), predicate.package.as_ref());
    if pattern.is_none() && package.is_none() {
        return None;
    }
    let mut held = vec![false; names.len()];
    for (id, held) in held.iter_mut().enumerate() {
        let Some(label) = label::target_node_label(names.resolve(Id(id as u32))) else {
            continue;
        };
        *held = pattern.is_none_or(|pattern| pattern.matches(label))
            && package.is_none_or(|package| package.matches(label));
    }
    Some(Column {
        held,
        beyond: false,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::facets::{FacetsBuilder, RuleCategory};
    use crate::repo::RepoCategory;
    use crate::select::Base;
    use crate::select::pattern::{PackagePrefix, Pattern};

    /// A graph holding, in the main repository, a `cc_test` target with the
    /// configured target, action, and output artifact it owns, all of which
    /// inherit the target's facets; beside them a `cc_library` target of an
    /// external repository built for a tool, a source file no input described
    /// beyond its label, and a group box such as the quotient pass mints, whose
    /// id lies past every facet the inputs stated.
    struct Fixture {
        graph: Graph,
        names: Names,
        facets: Facets,
    }

    const APP: &str = "target://foo:app";
    const CONFIGURED: &str = "ct://foo:app#ck1";
    const ACTION: &str = "action0";
    const OUTPUT: &str = "artifact://foo:app.o#ck1";
    const LIB: &str = "target:@@rules_foo+//bar:lib";
    const SOURCE: &str = "target://foo:main.c";
    const GROUP: &str = "group:target://foo:main.c";

    impl Fixture {
        fn new() -> Self {
            let mut graph = Graph::new();
            let mut names = Names::default();
            let mut builder = FacetsBuilder::default();

            let app = names.intern(APP);
            graph.add_node(app, ":app", Kind::Target, None);
            builder.describe_label(app, None, "//foo:app", "//foo:app");
            builder.set_tags(app, ["manual"]);
            builder.set_aspects(app, ["//tools:coverage.bzl%instrument"]);
            let facets = builder.facets();
            let rule_class = facets.intern_rule_class("cc_test");
            facets.set_rule_class(app, Some(rule_class));
            let config = facets.intern_config("ck1", Some("k8-fastbuild"), Tri::No);
            facets.set_config(app, Some(config));
            facets.set_testonly(app, Tri::Yes);

            for (name, kind, label) in [
                (CONFIGURED, Kind::ConfiguredTarget, ":app"),
                (ACTION, Kind::Action, "CppCompile"),
                (OUTPUT, Kind::Artifact, ":app.o"),
            ] {
                let id = names.intern(name);
                graph.add_node(id, label, kind, Some(app));
                builder.facets().inherit(id, app);
            }
            let action = names.get(ACTION).expect("the action just interned");
            let facets = builder.facets();
            let mnemonic = facets.intern_mnemonic("CppCompile");
            facets.set_mnemonic(action, Some(mnemonic));

            let lib = names.intern(LIB);
            graph.add_node(lib, ":lib", Kind::Target, None);
            builder.describe_label(lib, None, "@@rules_foo+//bar:lib", "@rules_foo//bar:lib");
            let facets = builder.facets();
            let rule_class = facets.intern_rule_class("cc_library");
            facets.set_rule_class(lib, Some(rule_class));
            let exec = facets.intern_config("ck2", Some("k8-opt-exec"), Tri::Yes);
            facets.set_config(lib, Some(exec));

            let source = names.intern(SOURCE);
            graph.add_node(source, ":main.c", Kind::Source, None);
            builder.describe_label(source, None, "//foo:main.c", "//foo:main.c");

            let (facets, _) = builder.seal();
            let group = names.intern(GROUP);
            graph.add_node(group, "2 sources", Kind::Source, None);

            Fixture {
                graph,
                names,
                facets,
            }
        }

        fn evaluate(&self, filter: &Filter) -> Verdicts {
            filter.evaluate(&self.graph, &self.names, &self.facets)
        }

        fn id(&self, name: &str) -> Id {
            self.names.get(name).expect("an interned node")
        }

        /// The nodes `filter` gives `visibility`, by name.
        fn nodes(&self, filter: &Filter, visibility: Visibility) -> BTreeSet<&str> {
            let verdicts = self.evaluate(filter);
            self.graph
                .nodes
                .iter()
                .filter(|node| verdicts.get(node.id) == visibility)
                .map(|node| self.names.resolve(node.id))
                .collect()
        }
    }

    fn named<'a>(names: impl IntoIterator<Item = &'a str>) -> BTreeSet<&'a str> {
        names.into_iter().collect()
    }

    /// A filter hiding every node but the ones `predicate` matches.
    fn showing(predicate: Predicate) -> Filter {
        Filter {
            base: Base::HideAll,
            rules: vec![Rule {
                predicate,
                visibility: Visibility::Shown,
            }],
            ..Filter::default()
        }
    }

    fn values(values: &[&str]) -> Option<Values<String>> {
        Values::new(values.iter().map(|&value| value.to_string()))
    }

    #[test]
    fn a_repository_term_keeps_the_nodes_of_the_repositories_it_names() {
        let fixture = Fixture::new();
        let main = showing(Predicate {
            repo_categories: Values::new([RepoCategory::Main]),
            ..Predicate::default()
        });

        assert_eq!(
            fixture.nodes(&main, Visibility::Shown),
            named([APP, CONFIGURED, ACTION, OUTPUT, SOURCE])
        );
        assert_eq!(
            fixture.nodes(&main, Visibility::Hidden),
            named([LIB, GROUP])
        );
    }

    #[test]
    fn a_repository_the_graph_never_saw_keeps_nothing() {
        let fixture = Fixture::new();
        let elsewhere = showing(Predicate {
            repos: values(&["rules_bar+"]),
            ..Predicate::default()
        });

        assert!(fixture.nodes(&elsewhere, Visibility::Shown).is_empty());
    }

    /// A configured target, an action, and an output artifact inherit the rule
    /// class of the target they belong to, so a term over rule classes reaches
    /// the whole of what a target became and not the target box alone.
    #[test]
    fn a_rule_class_term_reaches_everything_a_target_became() {
        let fixture = Fixture::new();
        let tests = showing(Predicate {
            rule_categories: Values::new([RuleCategory::Test]),
            ..Predicate::default()
        });

        assert_eq!(
            fixture.nodes(&tests, Visibility::Shown),
            named([APP, CONFIGURED, ACTION, OUTPUT])
        );
    }

    #[test]
    fn a_label_pattern_reaches_the_nodes_of_the_targets_it_names() {
        let fixture = Fixture::new();
        let foo = showing(Predicate {
            label: Some(Pattern::parse("//foo/...")),
            ..Predicate::default()
        });

        assert_eq!(
            fixture.nodes(&foo, Visibility::Shown),
            named([APP, CONFIGURED, ACTION, OUTPUT, SOURCE])
        );
    }

    /// The quotient pass mints group ids above every id the inputs described, so
    /// a group answers a term over any facet as a node carrying none does.
    #[test]
    fn an_id_past_the_facets_carries_nothing_and_answers_every_term() {
        let fixture = Fixture::new();
        let group = fixture.id(GROUP);
        let over_facets = showing(Predicate {
            rule_classes: values(&["cc_test"]),
            tags: values(&["manual"]),
            aspects: Some(Aspects::Any),
            label: Some(Pattern::parse("//foo/...")),
            ..Predicate::default()
        });
        assert_eq!(
            fixture.evaluate(&over_facets).get(group),
            Visibility::Hidden
        );

        let unaspected = showing(Predicate {
            aspects: Some(Aspects::None),
            ..Predicate::default()
        });
        assert_eq!(fixture.evaluate(&unaspected).get(group), Visibility::Shown);

        let everything = showing(Predicate::default());
        assert_eq!(fixture.evaluate(&everything).get(group), Visibility::Shown);
    }

    #[test]
    fn an_id_the_graph_does_not_name_reads_as_a_node_no_rule_reached() {
        let fixture = Fixture::new();
        let verdicts = fixture.evaluate(&showing(Predicate::default()));

        assert_eq!(verdicts.get(Id(9_999)), Visibility::Hidden);
    }

    #[test]
    fn a_pin_decides_the_node_it_names_whatever_the_rules_say() {
        let fixture = Fixture::new();
        let mut filter = showing(Predicate {
            kinds: Values::new([Kind::Target]),
            ..Predicate::default()
        });
        filter.pins.insert(fixture.id(APP), Visibility::Faded);
        filter.pins.insert(fixture.id(SOURCE), Visibility::Shown);

        let verdicts = fixture.evaluate(&filter);
        assert_eq!(verdicts.get(fixture.id(APP)), Visibility::Faded);
        assert_eq!(verdicts.get(fixture.id(SOURCE)), Visibility::Shown);
        assert_eq!(verdicts.get(fixture.id(LIB)), Visibility::Shown);
        assert_eq!(verdicts.get(fixture.id(ACTION)), Visibility::Hidden);
    }

    /// A filter whose rules overlap, so that the order they are read in decides
    /// several of the nodes and a pin decides one more.
    fn layered(fixture: &Fixture) -> Filter {
        let rule = |predicate, visibility| Rule {
            predicate,
            visibility,
        };
        Filter {
            base: Base::HideAll,
            rules: vec![
                rule(Predicate::default(), Visibility::Shown),
                rule(
                    Predicate {
                        kinds: Values::new([Kind::Action, Kind::Artifact]),
                        ..Predicate::default()
                    },
                    Visibility::Hidden,
                ),
                rule(
                    Predicate {
                        rule_categories: Values::new([RuleCategory::Test]),
                        testonly: Some(true),
                        ..Predicate::default()
                    },
                    Visibility::Faded,
                ),
                rule(
                    Predicate {
                        repos: values(&["rules_foo+"]),
                        is_tool: Some(true),
                        ..Predicate::default()
                    },
                    Visibility::Hidden,
                ),
                rule(
                    Predicate {
                        mnemonics: values(&["CppCompile"]),
                        ..Predicate::default()
                    },
                    Visibility::Shown,
                ),
                rule(
                    Predicate {
                        package: PackagePrefix::parse("//foo"),
                        aspects: Some(Aspects::Named(
                            values(&["//tools:coverage.bzl%instrument"]).expect("one aspect"),
                        )),
                        ..Predicate::default()
                    },
                    Visibility::Faded,
                ),
                rule(
                    Predicate {
                        label: Some(Pattern::parse("main.c")),
                        ..Predicate::default()
                    },
                    Visibility::Shown,
                ),
            ],
            pins: [(fixture.id(OUTPUT), Visibility::Faded)]
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn overlapping_rules_leave_each_node_with_the_last_verdict_reached() {
        let fixture = Fixture::new();
        let filter = layered(&fixture);

        assert_eq!(
            fixture.nodes(&filter, Visibility::Shown),
            named([ACTION, SOURCE, GROUP])
        );
        assert_eq!(
            fixture.nodes(&filter, Visibility::Faded),
            named([APP, CONFIGURED, OUTPUT])
        );
        assert_eq!(fixture.nodes(&filter, Visibility::Hidden), named([LIB]));
    }

    /// The scan resolves the rules into handle sets and columns before reading a
    /// node, where reading a node rule by rule resolves nothing; the two must
    /// reach the same verdict for every node.
    #[test]
    fn the_scan_agrees_with_reading_the_rules_node_by_node() {
        let fixture = Fixture::new();
        let filter = layered(&fixture);
        let verdicts = fixture.evaluate(&filter);

        for node in &fixture.graph.nodes {
            assert_eq!(
                verdicts.get(node.id),
                filter.visibility(node, &fixture.facets, &fixture.names),
                "{}",
                fixture.names.resolve(node.id)
            );
        }
    }
}
