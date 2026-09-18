//! Which nodes of a graph a filter shows, stated as rules over their facets.
//!
//! A node's verdict is the visibility of the last [`Rule`] matching it, the
//! filter's [`Base`] where no rule does, and its pin where it carries one. The
//! set of shown nodes is therefore a result the rules reach rather than state of
//! its own, and survives a graph being rebuilt, which the ids in such a set
//! would not.

mod eval;
pub mod pattern;

use std::borrow::Borrow;
use std::collections::HashMap;

use crate::facets::{Facets, RuleCategory, Tri};
use crate::graph::{Kind, Node};
use crate::intern::{Id, Names};
use crate::label;
use crate::repo::RepoCategory;
pub use eval::Verdicts;
use pattern::{PackagePrefix, Pattern};

/// What a filter says is to become of one node.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Visibility {
    Shown,
    Hidden,
    /// Drawn, but marked as no part of what the filter selects.
    Faded,
}

/// The verdict a filter reaches for a node no rule of it matches.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Base {
    #[default]
    ShowAll,
    HideAll,
}

impl Base {
    pub fn visibility(self) -> Visibility {
        match self {
            Base::ShowAll => Visibility::Shown,
            Base::HideAll => Visibility::Hidden,
        }
    }
}

/// The visibility a [`Predicate`] gives the nodes it matches.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Rule {
    pub predicate: Predicate,
    pub visibility: Visibility,
}

/// A filter over one graph's nodes.
///
/// The rules are read in order and the last one matching a node decides it, as
/// a gitignore file's patterns do, so a rule narrows or widens the ones before
/// it without either being rewritten. `pins` decide the nodes they name whatever
/// the rules say, which is what lets a node shown or hidden on its own outlast a
/// change to a rule that has nothing to do with it.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Filter {
    pub base: Base,
    pub rules: Vec<Rule>,
    pub pins: HashMap<Id, Visibility>,
}

impl Filter {
    /// The visibility of `node`, whose facets are `facets` and whose labels
    /// resolve through `names`.
    pub fn visibility(&self, node: &Node, facets: &Facets, names: &Names) -> Visibility {
        if let Some(&pinned) = self.pins.get(&node.id) {
            return pinned;
        }
        self.rules
            .iter()
            .rev()
            .find(|rule| rule.predicate.matches(node, facets, names))
            .map_or(self.base.visibility(), |rule| rule.visibility)
    }
}

/// The values one term admits, of which a node's facet must carry one.
///
/// Sorted and deduplicated, and never empty: a term admitting nothing matches no
/// node at all, which is what leaving the term unset already says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Values<T>(Vec<T>);

impl<T: Ord> Values<T> {
    /// `None` where `values` is empty.
    pub fn new(values: impl IntoIterator<Item = T>) -> Option<Self> {
        let mut values: Vec<T> = values.into_iter().collect();
        values.sort_unstable();
        values.dedup();
        (!values.is_empty()).then_some(Values(values))
    }

    pub fn contains<Q>(&self, value: &Q) -> bool
    where
        T: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0
            .binary_search_by(|held| held.borrow().cmp(value))
            .is_ok()
    }
}

impl<T> Values<T> {
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }
}

/// What a predicate asks of the aspects that took part in producing a node.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Aspects {
    /// At least one aspect, whichever it is.
    Any,
    /// No aspect. The inputs say nothing of an aspect either for a node no
    /// aspect ran on or for one described without the aspects being asked for,
    /// so this covers both; which of the two a graph holds is what
    /// [`Axis::Aspect`](crate::capability::Axis::Aspect)'s support states.
    None,
    Named(Values<String>),
}

/// A conjunction of terms over one node's facets, each of which constrains the
/// nodes matching only where it is set.
///
/// A term reading a facet the node does not carry never holds: a source file
/// states no rule class, and a term over rule classes must take that for no rule
/// class rather than for one of its own.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct Predicate {
    pub kinds: Option<Values<Kind>>,
    pub repo_categories: Option<Values<RepoCategory>>,
    /// Canonical repository names.
    pub repos: Option<Values<String>>,
    pub rule_classes: Option<Values<String>>,
    pub rule_categories: Option<Values<RuleCategory>>,
    /// Configuration checksums.
    pub configs: Option<Values<String>>,
    /// Whether the node is built in a configuration for a tool the build runs.
    pub is_tool: Option<bool>,
    /// Action mnemonics.
    pub mnemonics: Option<Values<String>>,
    pub aspects: Option<Aspects>,
    /// Tags, of which the node's target must carry one.
    pub tags: Option<Values<String>>,
    pub testonly: Option<bool>,
    /// A pattern over the canonical label of the target the node belongs to.
    pub label: Option<Pattern>,
    pub package: Option<PackagePrefix>,
}

impl Predicate {
    /// Whether every term this predicate sets holds of `node`.
    pub fn matches(&self, node: &Node, facets: &Facets, names: &Names) -> bool {
        let id = node.id;
        let strings = facets.strings();
        if let Some(kinds) = &self.kinds
            && !kinds.contains(&node.kind)
        {
            return false;
        }
        if let Some(categories) = &self.repo_categories {
            let category = facets
                .repo(id)
                .and_then(|repo| facets.repo_entry(repo))
                .map(|entry| entry.origin.category());
            if !category.is_some_and(|category| categories.contains(&category)) {
                return false;
            }
        }
        if let Some(repos) = &self.repos {
            let name = facets.repo(id).map(|repo| strings.resolve(repo.sym()));
            if !name.is_some_and(|name| repos.contains(name)) {
                return false;
            }
        }
        if let Some(rule_classes) = &self.rule_classes {
            let name = facets
                .rule_class(id)
                .map(|rule_class| strings.resolve(rule_class.sym()));
            if !name.is_some_and(|name| rule_classes.contains(name)) {
                return false;
            }
        }
        if let Some(categories) = &self.rule_categories {
            let category = facets
                .rule_class(id)
                .and_then(|rule_class| facets.rule_category(rule_class));
            if !category.is_some_and(|category| categories.contains(&category)) {
                return false;
            }
        }
        if let Some(configs) = &self.configs {
            let checksum = facets
                .config(id)
                .map(|config| strings.resolve(config.sym()));
            if !checksum.is_some_and(|checksum| configs.contains(checksum)) {
                return false;
            }
        }
        if let Some(is_tool) = self.is_tool {
            let stated = facets
                .config(id)
                .and_then(|config| facets.config_entry(config))
                .map(|entry| entry.is_tool);
            if stated != Some(stating(is_tool)) {
                return false;
            }
        }
        if let Some(mnemonics) = &self.mnemonics {
            let name = facets
                .mnemonic(id)
                .map(|mnemonic| strings.resolve(mnemonic));
            if !name.is_some_and(|name| mnemonics.contains(name)) {
                return false;
            }
        }
        if let Some(aspects) = &self.aspects {
            let carried = facets.aspects().values(id);
            let holds = match aspects {
                Aspects::Any => !carried.is_empty(),
                Aspects::None => carried.is_empty(),
                Aspects::Named(named) => carried
                    .iter()
                    .any(|&aspect| named.contains(strings.resolve(aspect))),
            };
            if !holds {
                return false;
            }
        }
        if let Some(tags) = &self.tags
            && !facets
                .tags()
                .values(id)
                .iter()
                .any(|&tag| tags.contains(strings.resolve(tag)))
        {
            return false;
        }
        if let Some(testonly) = self.testonly
            && facets.testonly(id) != stating(testonly)
        {
            return false;
        }
        if self.label.is_some() || self.package.is_some() {
            let Some(label) = canonical_label(facets, names, id) else {
                return false;
            };
            if let Some(pattern) = &self.label
                && !pattern.matches(label)
            {
                return false;
            }
            if let Some(package) = &self.package
                && !package.matches(label)
            {
                return false;
            }
        }
        true
    }
}

/// The [`Tri`] a node must carry to answer `yes`, an unstated facet answering
/// neither.
pub(crate) fn stating(yes: bool) -> Tri {
    if yes { Tri::Yes } else { Tri::No }
}

/// The canonical label of the target `id` belongs to, absent where the node
/// belongs to no target.
pub(crate) fn canonical_label<'a>(facets: &Facets, names: &'a Names, id: Id) -> Option<&'a str> {
    label::target_node_label(names.resolve(facets.label(id)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::FacetsBuilder;
    use crate::graph::Graph;

    /// A graph of one `cc_test` target in the main repository with an action
    /// below it, one `cc_library` target in an external repository, and one
    /// source file that no input described beyond its label.
    struct Fixture {
        graph: Graph,
        names: Names,
        facets: Facets,
    }

    impl Fixture {
        fn new() -> Self {
            let mut graph = Graph::new();
            let mut names = Names::default();
            let mut builder = FacetsBuilder::default();

            let app = names.intern("target://foo:app");
            graph.add_node(app, ":app", Kind::Target, None);
            builder.describe_label(app, None, "//foo:app", "//foo:app");
            builder.set_tags(app, ["manual", "flaky"]);
            builder.set_aspects(app, ["//tools:coverage.bzl%instrument"]);
            let facets = builder.facets();
            let rule_class = facets.intern_rule_class("cc_test");
            facets.set_rule_class(app, Some(rule_class));
            let config = facets.intern_config("ck1", Some("k8-fastbuild"), Tri::No);
            facets.set_config(app, Some(config));
            facets.set_testonly(app, Tri::Yes);

            let action = names.intern("action0");
            graph.add_node(action, "CppCompile", Kind::Action, Some(app));
            let facets = builder.facets();
            let mnemonic = facets.intern_mnemonic("CppCompile");
            facets.set_mnemonic(action, Some(mnemonic));
            facets.inherit(action, app);

            let lib = names.intern("target:@@rules_foo+//bar:lib");
            graph.add_node(lib, ":lib", Kind::Target, None);
            builder.describe_label(lib, None, "@@rules_foo+//bar:lib", "@rules_foo//bar:lib");
            let facets = builder.facets();
            let rule_class = facets.intern_rule_class("cc_library");
            facets.set_rule_class(lib, Some(rule_class));
            let exec = facets.intern_config("ck2", Some("k8-opt-exec"), Tri::Yes);
            facets.set_config(lib, Some(exec));
            facets.set_testonly(lib, Tri::No);

            let source = names.intern("target://foo:main.c");
            graph.add_node(source, ":main.c", Kind::Source, None);
            builder.describe_label(source, None, "//foo:main.c", "//foo:main.c");

            let (facets, _) = builder.seal();
            Fixture {
                graph,
                names,
                facets,
            }
        }

        fn node(&self, name: &str) -> &Node {
            let id = self.names.get(name).expect("an interned node");
            self.graph
                .nodes
                .iter()
                .find(|node| node.id == id)
                .expect("a node of the graph")
        }

        fn matching(&self, predicate: &Predicate) -> Vec<&str> {
            self.graph
                .nodes
                .iter()
                .filter(|node| predicate.matches(node, &self.facets, &self.names))
                .map(|node| self.names.resolve(node.id))
                .collect()
        }
    }

    fn values(values: &[&str]) -> Option<Values<String>> {
        Values::new(values.iter().map(|&value| value.to_string()))
    }

    #[test]
    fn a_predicate_with_no_term_matches_every_node() {
        let fixture = Fixture::new();
        assert_eq!(fixture.matching(&Predicate::default()).len(), 4);
    }

    #[test]
    fn a_term_over_values_none_of_which_is_named_constrains_nothing() {
        assert_eq!(Values::<String>::new([]), None);
        assert_eq!(Values::<Kind>::new([]), None);
    }

    #[test]
    fn each_term_selects_the_nodes_carrying_one_of_its_values() {
        let fixture = Fixture::new();
        let app = "target://foo:app";
        let action = "action0";
        let lib = "target:@@rules_foo+//bar:lib";
        let source = "target://foo:main.c";
        let cases: Vec<(Predicate, Vec<&str>)> = vec![
            (
                Predicate {
                    kinds: Values::new([Kind::Target]),
                    ..Predicate::default()
                },
                vec![app, lib],
            ),
            (
                Predicate {
                    repo_categories: Values::new([RepoCategory::Module]),
                    ..Predicate::default()
                },
                vec![lib],
            ),
            (
                Predicate {
                    repos: values(&["rules_foo+"]),
                    ..Predicate::default()
                },
                vec![lib],
            ),
            (
                Predicate {
                    rule_classes: values(&["cc_test"]),
                    ..Predicate::default()
                },
                vec![app, action],
            ),
            (
                Predicate {
                    rule_categories: Values::new([RuleCategory::Test]),
                    ..Predicate::default()
                },
                vec![app, action],
            ),
            (
                Predicate {
                    configs: values(&["ck2"]),
                    ..Predicate::default()
                },
                vec![lib],
            ),
            (
                Predicate {
                    is_tool: Some(true),
                    ..Predicate::default()
                },
                vec![lib],
            ),
            (
                Predicate {
                    is_tool: Some(false),
                    ..Predicate::default()
                },
                vec![app, action],
            ),
            (
                Predicate {
                    mnemonics: values(&["CppCompile"]),
                    ..Predicate::default()
                },
                vec![action],
            ),
            (
                Predicate {
                    aspects: Some(Aspects::Any),
                    ..Predicate::default()
                },
                vec![app],
            ),
            (
                Predicate {
                    aspects: Some(Aspects::Named(
                        values(&["//tools:coverage.bzl%instrument"]).unwrap(),
                    )),
                    ..Predicate::default()
                },
                vec![app],
            ),
            (
                Predicate {
                    tags: values(&["flaky", "local"]),
                    ..Predicate::default()
                },
                vec![app],
            ),
            (
                Predicate {
                    testonly: Some(true),
                    ..Predicate::default()
                },
                vec![app, action],
            ),
            (
                Predicate {
                    label: Some(Pattern::parse("//foo:all")),
                    ..Predicate::default()
                },
                vec![app, action, source],
            ),
            (
                Predicate {
                    package: PackagePrefix::parse("@rules_foo//bar"),
                    ..Predicate::default()
                },
                vec![lib],
            ),
        ];

        for (predicate, expected) in cases {
            let mut matched = fixture.matching(&predicate);
            matched.sort_unstable();
            let mut expected = expected;
            expected.sort_unstable();
            assert_eq!(matched, expected, "{predicate:?}");
        }
    }

    /// Every term of a predicate must hold, so two terms no node answers
    /// together select nothing even though each selects on its own.
    #[test]
    fn a_predicate_holds_only_where_all_of_its_terms_do() {
        let fixture = Fixture::new();
        let both = Predicate {
            kinds: Values::new([Kind::Action]),
            rule_classes: values(&["cc_test"]),
            ..Predicate::default()
        };
        assert_eq!(fixture.matching(&both), ["action0"]);

        let neither = Predicate {
            kinds: Values::new([Kind::Source]),
            rule_classes: values(&["cc_test"]),
            ..Predicate::default()
        };
        assert!(fixture.matching(&neither).is_empty());
    }

    /// The difference between a rule-class term selecting the rules it names and
    /// one quietly taking every file node with it.
    #[test]
    fn a_node_carrying_no_such_facet_matches_no_term_over_it() {
        let fixture = Fixture::new();
        let source = fixture.node("target://foo:main.c");
        let terms = [
            Predicate {
                rule_classes: values(&["cc_test"]),
                ..Predicate::default()
            },
            Predicate {
                rule_categories: Values::new([RuleCategory::Other]),
                ..Predicate::default()
            },
            Predicate {
                configs: values(&["ck1"]),
                ..Predicate::default()
            },
            Predicate {
                is_tool: Some(false),
                ..Predicate::default()
            },
            Predicate {
                mnemonics: values(&["CppCompile"]),
                ..Predicate::default()
            },
            Predicate {
                tags: values(&["manual"]),
                ..Predicate::default()
            },
            Predicate {
                testonly: Some(false),
                ..Predicate::default()
            },
        ];

        for predicate in terms {
            assert!(
                !predicate.matches(source, &fixture.facets, &fixture.names),
                "{predicate:?}"
            );
        }
    }

    /// A node belonging to no target answers no term over labels, whichever
    /// label a node beside it carries.
    #[test]
    fn a_node_with_no_label_matches_no_pattern() {
        let mut fixture = Fixture::new();
        let group = fixture.names.intern("group:target://foo:app");
        fixture
            .graph
            .add_node(group, "2 sources", Kind::Source, None);

        let predicate = Predicate {
            label: Some(Pattern::parse("//foo/...")),
            ..Predicate::default()
        };
        assert!(!predicate.matches(
            fixture.node("group:target://foo:app"),
            &fixture.facets,
            &fixture.names
        ));
    }

    fn rule(predicate: Predicate, visibility: Visibility) -> Rule {
        Rule {
            predicate,
            visibility,
        }
    }

    fn hide_targets() -> Rule {
        rule(
            Predicate {
                kinds: Values::new([Kind::Target]),
                ..Predicate::default()
            },
            Visibility::Hidden,
        )
    }

    fn fade_tests() -> Rule {
        rule(
            Predicate {
                rule_categories: Values::new([RuleCategory::Test]),
                ..Predicate::default()
            },
            Visibility::Faded,
        )
    }

    #[test]
    fn a_node_no_rule_matches_takes_the_base_verdict() {
        let fixture = Fixture::new();
        let source = fixture.node("target://foo:main.c");
        let mut filter = Filter {
            rules: vec![hide_targets()],
            ..Filter::default()
        };
        assert_eq!(
            filter.visibility(source, &fixture.facets, &fixture.names),
            Visibility::Shown
        );

        filter.base = Base::HideAll;
        assert_eq!(
            filter.visibility(source, &fixture.facets, &fixture.names),
            Visibility::Hidden
        );
    }

    #[test]
    fn the_last_rule_matching_a_node_decides_it() {
        let fixture = Fixture::new();
        let app = fixture.node("target://foo:app");
        let first = Filter {
            rules: vec![hide_targets(), fade_tests()],
            ..Filter::default()
        };
        assert_eq!(
            first.visibility(app, &fixture.facets, &fixture.names),
            Visibility::Faded
        );

        let reversed = Filter {
            rules: vec![fade_tests(), hide_targets()],
            ..Filter::default()
        };
        assert_eq!(
            reversed.visibility(app, &fixture.facets, &fixture.names),
            Visibility::Hidden
        );
    }

    #[test]
    fn a_pin_decides_the_node_it_names_whatever_the_rules_say() {
        let fixture = Fixture::new();
        let app = fixture.node("target://foo:app");
        let lib = fixture.node("target:@@rules_foo+//bar:lib");
        let filter = Filter {
            base: Base::HideAll,
            rules: vec![hide_targets()],
            pins: [(app.id, Visibility::Shown)].into_iter().collect(),
        };

        assert_eq!(
            filter.visibility(app, &fixture.facets, &fixture.names),
            Visibility::Shown
        );
        assert_eq!(
            filter.visibility(lib, &fixture.facets, &fixture.names),
            Visibility::Hidden
        );
    }
}
