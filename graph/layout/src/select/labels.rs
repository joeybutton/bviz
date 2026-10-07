//! The canonical labels one graph's nodes belong to, gathered so that a
//! [`Pattern`] is matched over each label once however many nodes carry it.

use crate::facets::{Facets, WrittenRepos};
use crate::graph::Graph;
use crate::intern::{Id, Names};
use crate::label::{package_node_parts, target_node_label};
use crate::select::pattern::Pattern;
use std::borrow::Cow;
use std::ops::Range;

/// The canonical label of the target the node `id` belongs to, which for a
/// target is its own; `None` for a node belonging to none.
pub fn carried_label<'a>(names: &'a Names, facets: &Facets, id: Id) -> Option<&'a str> {
    target_node_label(names.resolve(facets.label(id)?))
}

/// The directory the package box `id` stands for, written as a label writes
/// a package ahead of its `:`; `None` for a node that is no package box.
pub fn package_label(names: &Names, id: Id) -> Option<String> {
    let (repo, path) = package_node_parts(names.resolve(id))?;
    Some(format!("{repo}//{path}"))
}

/// Every target the nodes of one graph belong to: the id of the node naming the
/// target, and the canonical label it carries, ordered by that label.
pub struct LabelIndex {
    entries: Vec<(Id, String)>,
    /// The names the graph's repositories were written by, which a pattern's
    /// repository part is read against.
    repos: WrittenRepos,
}

impl LabelIndex {
    /// The targets `graph`'s nodes belong to, whose names resolve through
    /// `names`. A node belonging to no target — a group box, a package frame —
    /// names none and is left out.
    pub fn of(graph: &Graph, names: &Names, facets: &Facets) -> Self {
        let mut targets: Vec<Id> = Vec::with_capacity(graph.nodes.len());
        targets.extend(graph.nodes.iter().filter_map(|node| facets.label(node.id)));
        targets.sort_unstable();
        targets.dedup();
        let mut entries: Vec<(Id, String)> = Vec::with_capacity(targets.len());
        entries.extend(
            targets
                .into_iter()
                .filter_map(|id| Some((id, target_node_label(names.resolve(id))?.to_string()))),
        );
        entries.sort_unstable_by(|(_, one), (_, other)| one.cmp(other));
        LabelIndex {
            entries,
            repos: WrittenRepos::of(facets),
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The canonical label `label` as a reader writes it.
    pub fn written<'a>(&self, label: &'a str) -> Cow<'a, str> {
        self.repos.written_label(label)
    }

    /// The names the graph's repositories are written by.
    pub fn repos(&self) -> &WrittenRepos {
        &self.repos
    }

    /// The targets, each with the canonical label it carries, ordered by that
    /// label.
    pub(crate) fn entries(&self) -> &[(Id, String)] {
        &self.entries
    }

    /// Where the targets whose labels start with `prefix` lie among
    /// [`Self::entries`].
    pub(crate) fn starting(&self, prefix: &str) -> Range<usize> {
        fn label(entry: &(Id, String)) -> &str {
            &entry.1
        }
        let start = self.entries.partition_point(|entry| label(entry) < prefix);
        let run = &self.entries[start..];
        start..start + run.partition_point(|entry| label(entry).starts_with(prefix))
    }

    /// Where the target carrying `label` lies among [`Self::entries`].
    pub(crate) fn position(&self, label: &str) -> Option<usize> {
        let found = self
            .entries
            .binary_search_by(|(_, held)| held.as_str().cmp(label));
        found.ok()
    }

    /// The targets `pattern` names, by label.
    pub fn matching<'a>(&'a self, pattern: &'a Pattern) -> impl Iterator<Item = (Id, &'a str)> {
        let pattern = pattern.resolved(&self.repos);
        self.entries
            .iter()
            .filter(move |(_, label)| pattern.matches(label))
            .map(|(id, label)| (*id, label.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::FacetsBuilder;
    use crate::graph::{Kind, Node};

    /// A graph whose boxes read as short names — the leaf of a label, or an
    /// action's mnemonic — over targets in two packages and two external
    /// repositories, plus a group box belonging to no target.
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
            for (canonical, reads) in [
                ("//tools/rules/cpp:toolchain", ":toolchain"),
                ("//tools/rules/java:toolchain", ":toolchain"),
                ("//app:main", ":main"),
                ("@@rules_foo+//bar:lib", ":lib"),
                ("@crates//serde:lib", ":lib"),
            ] {
                let id = names.intern(&format!("target:{canonical}"));
                graph.add_node(Node::new(id, reads, Kind::Target, None));
                builder.describe_label(id, None, canonical);
            }
            let owner = names
                .get("target://app:main")
                .expect("the target just interned");
            let action = names.intern("action0");
            graph.add_node(Node::new(action, "CppCompile", Kind::Action, Some(owner)));
            builder.facets().inherit(action, owner);

            let (facets, _) = builder.seal();
            let group = names.intern("group:target://app:main");
            graph.add_node(Node::new(group, "2 sources", Kind::Source, None));
            Fixture {
                graph,
                names,
                facets,
            }
        }

        fn index(&self) -> LabelIndex {
            LabelIndex::of(&self.graph, &self.names, &self.facets)
        }

        fn matching(&self, pattern: &str) -> Vec<String> {
            let pattern = Pattern::parse(pattern);
            self.index()
                .matching(&pattern)
                .map(|(_, label)| label.to_string())
                .collect()
        }
    }

    /// A box reads as the leaf of its label, which several targets share; the
    /// index holds the canonical label instead, so a pattern naming a package
    /// path reaches the target and a pattern naming the leaf alone does not
    /// stand in for it.
    #[test]
    fn a_target_is_indexed_by_its_canonical_label_and_not_the_name_its_box_reads() {
        let fixture = Fixture::new();
        let labels: Vec<String> = fixture.matching("//...");

        assert_eq!(
            labels,
            [
                "//app:main",
                "//tools/rules/cpp:toolchain",
                "//tools/rules/java:toolchain",
            ]
        );
        assert_eq!(
            fixture.matching("//.../rules/cpp/..."),
            ["//tools/rules/cpp:toolchain"]
        );
    }

    /// An ellipsis covers the package it names and every package below it, and
    /// nothing beside them.
    #[test]
    fn a_package_pattern_covers_the_packages_below_it() {
        let fixture = Fixture::new();

        assert_eq!(
            fixture.matching("//tools/..."),
            [
                "//tools/rules/cpp:toolchain",
                "//tools/rules/java:toolchain"
            ]
        );
        assert_eq!(fixture.matching("//app/..."), ["//app:main"]);
        assert!(fixture.matching("//nowhere/...").is_empty());
    }

    /// A repository is named as it is written on a command line, which is what
    /// a reader has to hand rather than the canonical name a dump carries.
    #[test]
    fn a_repository_pattern_reaches_the_labels_of_that_repository() {
        let fixture = Fixture::new();

        assert_eq!(
            fixture.matching("@rules_foo//..."),
            ["@@rules_foo+//bar:lib"]
        );
    }

    /// A repository no input named canonically is reached by the apparent name
    /// its labels carry, which is both what a reader has to hand and what
    /// identifies the repository.
    #[test]
    fn a_repository_pattern_reaches_a_repository_named_only_apparently() {
        let fixture = Fixture::new();

        assert_eq!(fixture.matching("@crates//..."), ["@crates//serde:lib"]);
    }

    /// Every node of a target answers through the one label it carries, so the
    /// index holds one entry per target however many nodes the graph draws for
    /// it, and none for a node belonging to no target.
    #[test]
    fn the_index_holds_one_entry_per_target() {
        let fixture = Fixture::new();

        assert_eq!(fixture.index().len(), 5);
        assert_eq!(fixture.graph.nodes.len(), 7);
    }

    #[test]
    fn a_package_box_is_written_as_a_labels_package() {
        let mut names = Names::default();
        let written = |names: &mut Names, id: &str| {
            let id = names.intern(id);
            package_label(names, id)
        };
        assert_eq!(
            written(&mut names, "pkg:ui/components").as_deref(),
            Some("//ui/components")
        );
        assert_eq!(written(&mut names, "pkg:").as_deref(), Some("//"));
        assert_eq!(
            written(&mut names, "pkg:@crates//serde").as_deref(),
            Some("@crates//serde")
        );
        assert_eq!(written(&mut names, "target://ui:lib"), None);
    }
}
