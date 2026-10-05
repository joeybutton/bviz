//! Which targets of a graph a target set names, as Bazel expands the patterns
//! of a build command line (`skyframe/TargetPatternPhaseFunction.java`) with
//! the packages and targets the graph holds standing for those the workspace
//! holds.
//!
//! Bazel is cited here as of its tag 9.2.0: a source file by its path beneath
//! `src/main/java/com/google/devtools/build/lib`, a test by its class beneath
//! `src/test/java/com/google/devtools/build/lib`.

use std::ops::Range;
use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::capability::{Axis, Capabilities, Support};
use crate::facets::Facets;
use crate::graph::{Graph, Kind};
use crate::intern::Id;
use crate::label::encloses;
use crate::select::labels::LabelIndex;
use crate::select::pattern::{Parts, repo_matches, strip_sigil};
use crate::target_pattern::{Repo, TargetPattern};
use crate::target_set::{PatternError, TargetSet};

/// The targets of one graph by repository, package and name, against which a
/// [`TargetSet`] resolves.
///
/// A package is one some target of the graph sits in, and a repository one
/// some target's label names, so a package or repository the build holds but
/// the graph names nothing of is none here.
pub struct TargetIndex {
    labels: Arc<LabelIndex>,
    /// By target of `labels`.
    sorts: Vec<Sort>,
    /// The repository parts the labels carry, sigil and all, each once.
    repos: Vec<String>,
    tags: Support,
}

/// The targets of a graph a [`TargetSet`] names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Resolved {
    /// In id order.
    pub targets: Vec<Id>,
    /// The rules tagged `manual` that are none of `targets` though a wildcard
    /// adding its targets covers them and no pattern after that wildcard
    /// takes them out, in the order of their labels.
    pub left_out: Vec<Id>,
}

/// The targets one pattern names, each as where it lies among the labels.
#[derive(Default)]
struct Named {
    /// Named on their own, which no tag leaves out.
    alone: Vec<usize>,
    /// Covered by a wildcard.
    covered: Vec<usize>,
}

/// What a wildcard tells targets apart by.
#[derive(Clone, Copy)]
enum Sort {
    /// A file or a group of packages.
    NoRule,
    Rule {
        /// Tagged [`MANUAL`], which only a rule is (`packages/TargetUtils.java`).
        manual: bool,
    },
}

/// The tag a wildcard adding its targets leaves a rule out by
/// (`pkgcache/FilteringPolicies.java`).
const MANUAL: &str = "manual";

impl TargetIndex {
    /// The targets `labels` holds of `graph`.
    pub fn of(
        labels: Arc<LabelIndex>,
        graph: &Graph,
        facets: &Facets,
        capabilities: &Capabilities,
    ) -> Self {
        let entries = labels.entries();
        let mut at: FxHashMap<Id, usize> =
            FxHashMap::with_capacity_and_hasher(entries.len(), Default::default());
        at.extend(entries.iter().enumerate().map(|(at, (id, _))| (*id, at)));
        let manual = facets.strings().get(MANUAL);
        let mut sorts = vec![Sort::NoRule; entries.len()];
        for node in graph.nodes.iter().filter(|n| n.kind == Kind::Target) {
            if let Some(&at) = at.get(&node.id) {
                sorts[at] = Sort::Rule {
                    manual: manual.is_some_and(|tag| facets.tags().contains(node.id, tag)),
                };
            }
        }
        let carried = entries
            .iter()
            .filter_map(|(_, label)| Some(Parts::of(label)?.package.repo()));
        let mut repos: Vec<String> = Vec::with_capacity(facets.repos().len());
        for repo in carried {
            if repos.last().is_none_or(|last| last != repo) {
                repos.push(repo.to_string());
            }
        }
        TargetIndex {
            labels,
            sorts,
            repos,
            tags: capabilities.support(Axis::Tags),
        }
    }

    /// How far the graph states the tags of its rules, which is how far a
    /// wildcard can leave out the rules tagged `manual`: a rule stating no
    /// tags is taken for one carrying none.
    pub fn tags(&self) -> Support {
        self.tags
    }

    /// The targets `set` names: each pattern's targets are added to those
    /// before it, or taken out of them where it is negative, so a negative
    /// pattern ahead of every positive one takes nothing out. A wildcard
    /// leaves out a rule tagged `manual` where it adds, and not where it
    /// takes out; a target named on its own is never left out.
    pub fn resolve(&self, set: &TargetSet) -> Result<Resolved, PatternError> {
        let mut chosen: FxHashSet<usize> = FxHashSet::default();
        let mut left_out: FxHashSet<usize> = FxHashSet::default();
        for signed in set.patterns() {
            let Named { alone, covered } =
                self.named(signed.pattern())
                    .map_err(|message| PatternError {
                        pattern: signed.written().to_string(),
                        message,
                    })?;
            match signed.negative() {
                false => {
                    chosen.extend(alone);
                    for at in covered {
                        match self.sorts[at] {
                            Sort::Rule { manual: true } => left_out.insert(at),
                            Sort::Rule { manual: false } | Sort::NoRule => chosen.insert(at),
                        };
                    }
                }
                true => {
                    for at in alone.iter().chain(&covered) {
                        chosen.remove(at);
                        left_out.remove(at);
                    }
                }
            }
        }
        let id = |at: usize| self.labels.entries()[at].0;
        let mut left_out: Vec<usize> = left_out.difference(&chosen).copied().collect();
        left_out.sort_unstable();
        let mut targets: Vec<Id> = chosen.into_iter().map(id).collect();
        targets.sort_unstable();
        Ok(Resolved {
            targets,
            left_out: left_out.into_iter().map(id).collect(),
        })
    }

    /// The targets `pattern` names.
    fn named(&self, pattern: &TargetPattern) -> Result<Named, String> {
        let kept = |at: usize, rules_only: bool| match self.sorts[at] {
            Sort::NoRule => !rules_only,
            Sort::Rule { .. } => true,
        };
        let alone = |alone: Vec<usize>| Named {
            alone,
            ..Named::default()
        };
        match pattern {
            TargetPattern::Single {
                repo,
                package,
                name,
            } => (self.single(&self.carried(repo)?, &part(repo), package, name)).map(alone),
            TargetPattern::InPackage {
                repo,
                package,
                wildcard,
                absolute,
            } => {
                let mut named = Named::default();
                let mut held = false;
                for carried in self.carried(repo)? {
                    let targets = self.package(carried, package);
                    held |= !targets.is_empty();
                    // A target named as the wildcard is, which only a pattern
                    // written from the root names (`TargetsInPackage.getWildcardConflict`
                    // in `cmdline/TargetPattern.java`).
                    let conflict = (absolute.then_some(wildcard.written()))
                        .and_then(|name| self.target(carried, package, name));
                    match conflict {
                        Some(target) => named.alone.push(target),
                        None => {
                            let kept = targets.filter(|&at| kept(at, wildcard.rules_only()));
                            named.covered.extend(kept);
                        }
                    }
                }
                match held {
                    true => Ok(named),
                    false => Err(self.no_package(&part(repo), package)),
                }
            }
            TargetPattern::Below {
                repo,
                directory,
                rules_only,
            } => {
                let repos = self.carried(repo)?;
                let beneath = || repos.iter().flat_map(|repo| self.beneath(repo, directory));
                if beneath().next().is_none() {
                    return Err(format!("no targets found beneath '{directory}'"));
                }
                let covered = beneath().filter(|&at| kept(at, *rules_only)).collect();
                Ok(Named {
                    covered,
                    ..Named::default()
                })
            }
            TargetPattern::PathAsTarget { path } => self.path_as_target(path).map(alone),
        }
    }

    /// The targets `name` of the package `package` in the repositories whose
    /// labels carry the parts `carried`, which a pattern wrote as `written`.
    fn single(
        &self,
        carried: &[&str],
        written: &str,
        package: &str,
        name: &str,
    ) -> Result<Vec<usize>, String> {
        let targets = carried
            .iter()
            .filter_map(|repo| self.target(repo, package, name));
        let found: Vec<usize> = targets.collect();
        if !found.is_empty() {
            return Ok(found);
        }
        let held = (carried.iter()).any(|repo| !self.package(repo, package).is_empty());
        Err(match held {
            true => {
                let label = format!("{written}//{package}:{name}");
                format!("no target '{}' in the build", self.labels.written(&label))
            }
            false => self.no_package(written, package),
        })
    }

    fn no_package(&self, written: &str, package: &str) -> String {
        let label = format!("{written}//{package}");
        format!("no package '{}' in the build", self.labels.written(&label))
    }

    /// The target the main repository's file `path` stands for, read as
    /// `InterpretPathAsTarget` in `cmdline/TargetPattern.java` reads it: the
    /// package `path` names, or else the file in the deepest package holding
    /// it.
    fn path_as_target(&self, path: &str) -> Result<Vec<usize>, String> {
        const MAIN: &str = "";
        let is_package = |package: &str| !self.package(MAIN, package).is_empty();
        let last = path.rsplit('/').next().unwrap_or(path);
        let cuts = path.rmatch_indices('/');
        let held = cuts.map(|(at, _)| (&path[..at], &path[at + 1..]));
        let mut read = std::iter::once((path, last))
            .chain(held)
            .chain(std::iter::once((MAIN, path)));
        match read.find(|(package, _)| is_package(package)) {
            Some((package, name)) => self.single(&[MAIN], MAIN, package, name),
            None => Err(format!("couldn't determine target from filename '{path}'")),
        }
    }

    /// The repository parts the labels of the repositories `repo` names
    /// carry. An apparent name reaches every repository a pattern of that
    /// name selects, a canonical one the repository named so apparently
    /// where none is named so canonically.
    fn carried(&self, repo: &Repo) -> Result<Vec<&str>, String> {
        let held = |part: &str| self.repos.iter().find(|held| *held == part);
        let found: Vec<&str> = match repo {
            Repo::Main => return Ok(vec![""]),
            Repo::Canonical(name) => {
                let named = held(&format!("@@{name}")).or_else(|| held(&format!("@{name}")));
                named.into_iter().map(String::as_str).collect()
            }
            Repo::Apparent(name) => {
                let named = self.labels.repos().canonical(name).unwrap_or(name);
                let repos = self.repos.iter().map(String::as_str);
                repos
                    .filter(|held| repo_matches(named, strip_sigil(held)))
                    .collect()
            }
        };
        match found.is_empty() {
            true => Err(format!("no repository '{}' in the build", part(repo))),
            false => Ok(found),
        }
    }

    /// Where the targets of one package lie among those of `labels`, `repo`
    /// being the repository part its labels carry.
    fn package(&self, repo: &str, package: &str) -> Range<usize> {
        self.labels.starting(&format!("{repo}//{package}:"))
    }

    fn target(&self, repo: &str, package: &str, name: &str) -> Option<usize> {
        self.labels.position(&format!("{repo}//{package}:{name}"))
    }

    /// The targets of the packages at or beneath `directory` in the
    /// repository whose labels carry the part `repo`.
    fn beneath(&self, repo: &str, directory: &str) -> impl Iterator<Item = usize> {
        let entries = self.labels.entries();
        let within = self.labels.starting(&format!("{repo}//{directory}"));
        within.filter(move |&at| {
            Parts::of(&entries[at].1).is_some_and(|parts| encloses(directory, parts.package.path()))
        })
    }
}

/// The repository part a pattern naming `repo` writes, sigil and all.
fn part(repo: &Repo) -> String {
    match repo {
        Repo::Main => String::new(),
        Repo::Canonical(name) => format!("@@{name}"),
        Repo::Apparent(name) => format!("@{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::FacetsBuilder;
    use crate::graph::Node;
    use crate::intern::Names;

    /// A graph of targets, each written as its canonical label, a rule unless
    /// its name has a dot, tagged as `tags` says.
    struct Fixture {
        graph: Graph,
        names: Names,
        builder: FacetsBuilder,
    }

    impl Fixture {
        fn new() -> Self {
            Fixture {
                graph: Graph::new(),
                names: Names::default(),
                builder: FacetsBuilder::default(),
            }
        }

        fn target(&mut self, label: &str, tags: &[&str]) -> &mut Self {
            let id = self.names.intern(&format!("target:{label}"));
            let name = label.rsplit_once(':').unwrap().1;
            let kind = if name.contains('.') {
                Kind::Source
            } else {
                Kind::Target
            };
            self.graph.add_node(Node::new(id, name, kind, None));
            self.builder.describe_label(id, None, label);
            if kind == Kind::Target {
                self.builder.set_tags(id, tags.iter().copied());
                self.builder.observe(Axis::Tags, true);
            }
            self
        }

        fn targets(&mut self, labels: &[&str]) -> &mut Self {
            for label in labels {
                self.target(label, &[]);
            }
            self
        }

        fn index(&mut self) -> (TargetIndex, Names) {
            let builder = std::mem::take(&mut self.builder);
            let (facets, capabilities) = builder.seal();
            let labels = Arc::new(LabelIndex::of(&self.graph, &self.names, &facets));
            let index = TargetIndex::of(labels, &self.graph, &facets, &capabilities);
            (index, std::mem::take(&mut self.names))
        }
    }

    /// The labels of the targets `set` names, sorted, and of the manual ones
    /// its wildcards left out, in the order told.
    fn resolved_both(fixture: &mut Fixture, set: &str) -> Result<[Vec<String>; 2], String> {
        let (index, names) = fixture.index();
        let set = TargetSet::parse(set).map_err(|e| e.to_string())?;
        let Resolved { targets, left_out } = index.resolve(&set).map_err(|e| e.to_string())?;
        let label = |&id| names.resolve(id).trim_start_matches("target:").to_string();
        let mut targets: Vec<String> = targets.iter().map(label).collect();
        targets.sort();
        Ok([targets, left_out.iter().map(label).collect()])
    }

    fn resolved(fixture: &mut Fixture, set: &str) -> Result<Vec<String>, String> {
        resolved_both(fixture, set).map(|[targets, _]| targets)
    }

    /// `testManualTarget` and `testAllExcludesManualTest` of
    /// `pkgcache/LoadingPhaseRunnerTest`, which every test named here is of.
    #[test]
    fn a_wildcard_leaves_a_manual_rule_out_and_naming_it_keeps_it() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture
                .target("//cc:my_lib", &["manual"])
                .target("//cc:my_test", &[])
                .target("//cc:lib.cc", &[]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set).unwrap();
        assert_eq!(set("//cc:all"), ["//cc:my_test"]);
        assert_eq!(set("//cc/..."), ["//cc:my_test"]);
        assert_eq!(set("//cc:my_lib"), ["//cc:my_lib"]);
        assert_eq!(set("//cc:*"), ["//cc:lib.cc", "//cc:my_test"]);
    }

    /// A negative pattern takes out what it names, manual or not, and only
    /// what the patterns before it added.
    #[test]
    fn a_negative_pattern_takes_out_what_the_patterns_before_it_added() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture
                .target("//a:manual", &["manual"])
                .targets(&["//a:one", "//a:two", "//b:three"]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set).unwrap();
        assert_eq!(set("//... -//a:one"), ["//a:two", "//b:three"]);
        assert_eq!(set("//a:manual //b:three -//a:all"), ["//b:three"]);
        assert_eq!(set("//a:manual -//a/..."), Vec::<String>::new());
        // `testNegativeTargetDoesNotShowUpAtAll`.
        assert_eq!(set("-//a:one"), Vec::<String>::new());
        assert_eq!(set("-//a:one //a:one"), ["//a:one"]);
    }

    /// The manual rules a wildcard left out are told, short of those a
    /// pattern names on its own or takes out.
    #[test]
    fn the_manual_rules_a_wildcard_left_out_are_told_unless_named_or_taken_out() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture
                .target("//a:manual", &["manual"])
                .target("//a/b:manual", &["manual"])
                .target("//a:all.txt", &[])
                .targets(&["//a:one", "//c:two"]);
            fixture
        };
        let left_out = |set: &str| resolved_both(&mut build(), set).unwrap()[1].clone();
        assert_eq!(left_out("//a:all"), ["//a:manual"]);
        assert_eq!(left_out("//a:*"), ["//a:manual"]);
        assert_eq!(left_out("//..."), ["//a/b:manual", "//a:manual"]);
        assert_eq!(left_out("//a:one //c:two //a:manual"), Vec::<String>::new());
        assert_eq!(left_out("//a/... //a:manual"), ["//a/b:manual"]);
        assert_eq!(left_out("//a:manual //a/..."), ["//a/b:manual"]);
        assert_eq!(left_out("//... -//a/b/..."), ["//a:manual"]);
        assert_eq!(left_out("//... -//a/b:manual"), ["//a:manual"]);
        assert_eq!(left_out("//... -//a/..."), Vec::<String>::new());
        assert_eq!(left_out("//c/..."), Vec::<String>::new());
    }

    /// `testRecursiveAllRules`, `testRecursiveAllTargets` and
    /// `testNonExistentRecursive`.
    #[test]
    fn an_ellipsis_names_every_package_beneath_its_directory() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture.targets(&[
                "//base:base",
                "//base:base.txt",
                "//base/foo:foo",
                "//base/bar:bar",
                "//base/bar:bar.txt",
                "//basement:x",
            ]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set);
        assert_eq!(
            set("//base/..."),
            Ok(vec![
                "//base/bar:bar".to_string(),
                "//base/foo:foo".to_string(),
                "//base:base".to_string()
            ])
        );
        assert_eq!(
            set("//base/bar/..."),
            Ok(vec!["//base/bar:bar".to_string()])
        );
        assert_eq!(set("//base/bar/...:*").unwrap().len(), 2);
        assert_eq!(set("//base/...:all-targets").unwrap().len(), 5);
        assert_eq!(set("...").unwrap().len(), 4);
        assert_eq!(
            set("//nowhere/..."),
            Err("'//nowhere/...': no targets found beneath 'nowhere'".to_string())
        );
    }

    /// `testNonExistentTarget` and `testNonExistentPackage`.
    #[test]
    fn a_target_or_package_the_build_lacks_is_an_error() {
        let set = |set: &str| {
            let mut fixture = Fixture::new();
            fixture.targets(&["//base:hello"]);
            resolved(&mut fixture, set)
        };
        assert_eq!(
            set("//base:missing"),
            Err("'//base:missing': no target '//base:missing' in the build".to_string())
        );
        assert_eq!(
            set("//other:x"),
            Err("'//other:x': no package '//other' in the build".to_string())
        );
        assert_eq!(
            set("//other:all"),
            Err("'//other:all': no package '//other' in the build".to_string())
        );
    }

    /// `testWildcardConflict` and `testAbsolutePatternEndsWithSlashAll`:
    /// a target named as the wildcard is takes precedence where the pattern
    /// is written from the root.
    #[test]
    fn a_target_named_as_a_wildcard_is_what_an_absolute_pattern_names() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture.targets(&[
                "//foo/lib:lib1",
                "//foo/lib:all",
                "//foo/lib:all-targets",
                "//foo/all:all",
            ]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set).unwrap();
        assert_eq!(set("//foo/lib:all"), ["//foo/lib:all"]);
        assert_eq!(set("//foo/lib:all-targets"), ["//foo/lib:all-targets"]);
        assert_eq!(set("//foo/all"), ["//foo/all:all"]);
        assert_eq!(set("foo/lib:all").len(), 3);
    }

    #[test]
    fn a_path_names_the_target_it_is_in_the_deepest_package_holding_it() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture.targets(&[
                "//foo:foo",
                "//foo:bar/baz.txt",
                "//foo/pkg:pkg",
                "//:root.txt",
            ]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set);
        assert_eq!(
            set("foo/bar/baz.txt"),
            Ok(vec!["//foo:bar/baz.txt".to_string()])
        );
        assert_eq!(set("foo/pkg"), Ok(vec!["//foo/pkg:pkg".to_string()]));
        assert_eq!(set("foo"), Ok(vec!["//foo:foo".to_string()]));
        assert_eq!(set("root.txt"), Ok(vec!["//:root.txt".to_string()]));
        assert_eq!(
            set("foo/missing.txt"),
            Err("'foo/missing.txt': no target '//foo:missing.txt' in the build".to_string())
        );
    }

    #[test]
    fn a_path_outside_every_package_names_nothing() {
        let mut fixture = Fixture::new();
        fixture.targets(&["//foo:foo"]);
        assert_eq!(
            resolved(&mut fixture, "bar/baz.txt"),
            Err("'bar/baz.txt': couldn't determine target from filename 'bar/baz.txt'".to_string())
        );
    }

    /// The main repository's wildcard stays in it, and a repository is named
    /// by any name a reader writes it by.
    #[test]
    fn a_repository_is_reached_by_the_names_a_reader_writes_it_by() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture.targets(&[
                "//app:main",
                "@@rules_foo+//bar:lib",
                "@crates//serde:lib",
                "@@bazel_tools//tools:x",
            ]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set);
        assert_eq!(set("//..."), Ok(vec!["//app:main".to_string()]));
        let foo = Ok(vec!["@@rules_foo+//bar:lib".to_string()]);
        assert_eq!(set("@rules_foo//..."), foo);
        assert_eq!(set("@@rules_foo+//bar:lib"), foo);
        assert_eq!(
            set("@crates//serde:lib"),
            Ok(vec!["@crates//serde:lib".to_string()])
        );
        assert_eq!(set("@bazel_tools//tools:all").unwrap().len(), 1);
        assert_eq!(
            set("@nowhere//..."),
            Err("'@nowhere//...': no repository '@nowhere' in the build".to_string())
        );
        assert_eq!(
            set("@@rules_foo+//bar:missing"),
            Err(
                "'@@rules_foo+//bar:missing': no target '@rules_foo//bar:missing' in the build"
                    .to_string()
            )
        );
    }

    /// A repository the inputs write both canonically and apparently holds two
    /// identities, and the apparent name a reader writes it by reaches both.
    #[test]
    fn an_apparent_name_reaches_a_repository_under_each_of_its_identities() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture.targets(&[
                "@@rules_foo+//bar:lib",
                "@rules_foo//bar:lib",
                "@rules_foo//baz:tool",
            ]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set).unwrap();
        assert_eq!(
            set("@rules_foo//..."),
            [
                "@@rules_foo+//bar:lib",
                "@rules_foo//bar:lib",
                "@rules_foo//baz:tool"
            ]
        );
        assert_eq!(
            set("@rules_foo//bar:lib"),
            ["@@rules_foo+//bar:lib", "@rules_foo//bar:lib"]
        );
        assert_eq!(set("@rules_foo//bar:all").len(), 2);
        assert_eq!(set("@rules_foo//baz:tool"), ["@rules_foo//baz:tool"]);
        assert_eq!(set("@@rules_foo+//..."), ["@@rules_foo+//bar:lib"]);
    }

    /// A canonical name reaches the repository named so only apparently,
    /// which no input named canonically.
    #[test]
    fn a_canonical_name_reaches_a_repository_named_so_apparently() {
        let build = || {
            let mut fixture = Fixture::new();
            fixture.targets(&["@crates//serde:lib", "@@tools//a:a", "@tools//b:b"]);
            fixture
        };
        let set = |set: &str| resolved(&mut build(), set);
        assert_eq!(
            set("@@crates//..."),
            Ok(vec!["@crates//serde:lib".to_string()])
        );
        assert_eq!(set("@@tools//..."), Ok(vec!["@@tools//a:a".to_string()]));
        assert_eq!(
            set("@@nowhere//a"),
            Err("'@@nowhere//a': no repository '@@nowhere' in the build".to_string())
        );
    }

    /// A rule described without its attributes states no tags, so no
    /// wildcard can tell it is manual, which the index reports.
    #[test]
    fn the_index_says_whether_the_graph_states_tags() {
        let mut fixture = Fixture::new();
        fixture.targets(&["//a:a"]);
        assert_eq!(fixture.index().0.tags(), Support::Available);
        let mut builder = FacetsBuilder::default();
        builder.observe(Axis::Tags, false);
        let (facets, capabilities) = builder.seal();
        let (graph, names) = (Graph::new(), Names::default());
        let labels = Arc::new(LabelIndex::of(&graph, &names, &facets));
        let index = TargetIndex::of(labels, &graph, &facets, &capabilities);
        assert!(matches!(index.tags(), Support::Unavailable { .. }));
    }
}
