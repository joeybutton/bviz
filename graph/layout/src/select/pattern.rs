//! Target patterns over canonical labels, in the vocabulary Bazel's own target
//! patterns use.
//!
//! `//foo:bar` names one target and `//foo:all` every target of one package.
//! `...` stands for any run of path segments wherever it appears, so `//foo/...`
//! reaches `//foo` and every package below it while `//.../cpp/...` reaches
//! every package with a `cpp` segment. A repository part matches the name a
//! label carries outright — canonical, or apparent where no input named the
//! repository canonically — and a module's own repository by the module name as
//! well, since `@rules_foo//...` is how that repository is written before a
//! repository mapping resolves it.

use std::borrow::Cow;

use crate::facets::WrittenRepos;
use crate::label::encloses;
use crate::repo::module_name;

/// A pattern a node's canonical label is selected by.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Pattern {
    /// A pattern written in label form.
    Label {
        /// The repository the pattern names, with any sigil stripped, empty for
        /// the main repository.
        repo: String,
        package: Vec<Segment>,
        target: Target,
    },
    /// Text a label is selected by containing it, which is what a pattern that
    /// is no label at all is read as.
    Substring(String),
}

/// One component of a pattern's package path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Segment {
    Named(String),
    /// `...`, which stands for any run of components, of which there may be
    /// none.
    Any,
}

/// The part of a pattern after the colon.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    Named(String),
    /// A package path ending in `...`, or one of the wildcard target parts
    /// Bazel reads: `:all` over a package's rules, `:*` and `:all-targets`
    /// over every target in it. A pattern here is matched over labels alone,
    /// which say nothing of whether a target is a rule, so all three leave the
    /// target equally free. A target genuinely named `all` cannot be written,
    /// as it cannot in a Bazel command line either.
    All,
}

impl Pattern {
    /// The pattern `text` writes. Text that is no label — one holding no `//`,
    /// or one whose package path or target name is malformed — is read as a
    /// substring, so a pattern half typed selects by what has been typed so far
    /// rather than by nothing.
    pub fn parse(text: &str) -> Self {
        Self::label(text).unwrap_or_else(|| Pattern::Substring(text.to_string()))
    }

    /// `self` with a repository part that is an apparent name `repos` resolves
    /// read as the canonical name it stands for, which is the name labels carry.
    pub fn resolved<'a>(&'a self, repos: &WrittenRepos) -> Cow<'a, Self> {
        let Pattern::Label {
            repo,
            package,
            target,
        } = self
        else {
            return Cow::Borrowed(self);
        };
        match repos.canonical(repo) {
            Some(canonical) => Cow::Owned(Pattern::Label {
                repo: canonical.to_string(),
                package: package.clone(),
                target: target.clone(),
            }),
            None => Cow::Borrowed(self),
        }
    }

    /// Whether `label`, a canonical label, is one this pattern names.
    pub fn matches(&self, label: &str) -> bool {
        match self {
            Pattern::Substring(text) => label.contains(text.as_str()),
            Pattern::Label {
                repo,
                package,
                target,
            } => {
                let Some(parts) = Parts::of(label) else {
                    return false;
                };
                target.matches(parts.name)
                    && names_package(repo, package, parts.repo, parts.package)
            }
        }
    }

    /// Whether this pattern covers the whole of the package `repo` and `path`
    /// name, `repo` being the repository part as a label spells it, sigil and
    /// all.
    ///
    /// A package is covered by a pattern that leaves the target free and by no
    /// other, which is the line Bazel's own target patterns draw: `//foo/...`
    /// covers `//foo` and every package below it, `//foo:all` covers `//foo`,
    /// and `//foo:bar` names one target of a package and so covers none. Text
    /// that is no label covers every package whose own label it occurs in.
    ///
    /// This is what a box answers a pattern through. A box belongs to no
    /// target — it is drawn for the package it is captioned by — so a pattern
    /// reaches it by covering that package rather than by matching a label.
    pub fn covers_package(&self, repo: &str, path: &str) -> bool {
        match self {
            Pattern::Substring(text) => format!("{repo}//{path}").contains(text.as_str()),
            Pattern::Label {
                repo: written,
                package,
                target,
            } => *target == Target::All && names_package(written, package, repo, path),
        }
    }

    fn label(text: &str) -> Option<Self> {
        let (repo, rest) = text.split_once("//")?;
        let (path, written) = match rest.split_once(':') {
            Some((path, "all" | "*" | "all-targets")) => (path, Some(Target::All)),
            Some((_, "")) => return None,
            Some((path, name)) => (path, Some(Target::Named(name.to_string()))),
            None => (rest, None),
        };
        let mut package: Vec<Segment> = components(path)
            .iter()
            .map(|&component| match component {
                "..." => Some(Segment::Any),
                "" => None,
                named => Some(Segment::Named(named.to_string())),
            })
            .collect::<Option<_>>()?;
        // One any-segment already stands for a run of any length, and matching
        // a second beside it costs a search over the ways to split that run
        // between them.
        package.dedup_by(|next, held| *next == Segment::Any && *held == Segment::Any);
        // A path ending in `...` leaves the target free, and one written with no
        // colon at all names the target its last component names, as Bazel reads
        // both. A repository written with neither a package nor a colon names
        // every package of it.
        let target = match written {
            Some(target) => target,
            None => match package.last() {
                None => {
                    package.push(Segment::Any);
                    Target::All
                }
                Some(Segment::Any) => Target::All,
                Some(Segment::Named(name)) => Target::Named(name.clone()),
            },
        };
        Some(Pattern::Label {
            repo: strip_sigil(repo).to_string(),
            package,
            target,
        })
    }
}

impl Target {
    fn matches(&self, name: &str) -> bool {
        match self {
            Target::Named(named) => named == name,
            Target::All => true,
        }
    }
}

/// A package and everything below it, written as a label's package part:
/// `//foo/bar`, `@repo//foo`, or `//` for a whole repository.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PackagePrefix {
    repo: String,
    path: String,
}

impl PackagePrefix {
    /// The prefix `text` writes, or `None` where it names no package, which is
    /// any text holding no `//`.
    pub fn parse(text: &str) -> Option<Self> {
        let (repo, rest) = text.split_once("//")?;
        let path = rest.split_once(':').map_or(rest, |(path, _)| path);
        Some(PackagePrefix {
            repo: strip_sigil(repo).to_string(),
            path: path.trim_end_matches('/').to_string(),
        })
    }

    /// [`Pattern::resolved`] for a package prefix's repository part.
    pub fn resolved<'a>(&'a self, repos: &WrittenRepos) -> Cow<'a, Self> {
        match repos.canonical(&self.repo) {
            Some(canonical) => Cow::Owned(PackagePrefix {
                repo: canonical.to_string(),
                path: self.path.clone(),
            }),
            None => Cow::Borrowed(self),
        }
    }

    /// Whether the canonical label `label` sits in a package this prefix
    /// covers.
    pub fn matches(&self, label: &str) -> bool {
        Parts::of(label).is_some_and(|parts| self.covers_package(parts.repo, parts.package))
    }

    /// Whether the package `repo` and `path` name is this one or one below it,
    /// the two split at a path component boundary so that `//foo` covers
    /// `//foo/bar` and not `//foobar`.
    pub fn covers_package(&self, repo: &str, path: &str) -> bool {
        repo_matches(&self.repo, strip_sigil(repo)) && encloses(&self.path, path)
    }
}

/// A canonical label split into the parts a pattern matches one by one, the
/// repository part written as the label spells it, sigil and all.
struct Parts<'a> {
    repo: &'a str,
    package: &'a str,
    name: &'a str,
}

impl<'a> Parts<'a> {
    /// `None` for a string that is no label, a label naming both a package and,
    /// after a colon, a target.
    fn of(label: &'a str) -> Option<Self> {
        let (repo, rest) = label.split_once("//")?;
        let (package, name) = rest.split_once(':')?;
        Some(Parts {
            repo,
            package,
            name,
        })
    }
}

/// Whether the package part of a label pattern — the repository `written` and
/// the segments `package` — names the package `repo` and `path` give.
fn names_package(written: &str, package: &[Segment], repo: &str, path: &str) -> bool {
    repo_matches(written, strip_sigil(repo)) && segments_match(package, &components(path))
}

fn strip_sigil(repo: &str) -> &str {
    repo.trim_start_matches('@')
}

/// Whether the repository part `written` of a pattern names the repository
/// whose canonical name is `canonical`.
fn repo_matches(written: &str, canonical: &str) -> bool {
    written == canonical || module_name(canonical) == Some(written)
}

/// The components of a package path, of which the root package has none.
fn components(path: &str) -> Vec<&str> {
    if path.is_empty() {
        Vec::new()
    } else {
        path.split('/').collect()
    }
}

fn segments_match(pattern: &[Segment], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((Segment::Named(name), rest)) => match path.split_first() {
            Some((&head, tail)) => head == name && segments_match(rest, tail),
            None => false,
        },
        Some((Segment::Any, rest)) => {
            (0..=path.len()).any(|skipped| segments_match(rest, &path[skipped..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::Facets;

    fn segments(pattern: &Pattern) -> &[Segment] {
        match pattern {
            Pattern::Label { package, .. } => package,
            Pattern::Substring(text) => panic!("{text:?} parsed as a substring, not a label"),
        }
    }

    #[test]
    fn a_label_pattern_parses_into_its_repository_package_and_target() {
        assert_eq!(
            Pattern::parse("@@rules_foo+//bar/baz:qux"),
            Pattern::Label {
                repo: "rules_foo+".to_string(),
                package: vec![
                    Segment::Named("bar".to_string()),
                    Segment::Named("baz".to_string())
                ],
                target: Target::Named("qux".to_string()),
            }
        );
    }

    #[test]
    fn all_and_a_trailing_ellipsis_leave_the_target_free() {
        assert_eq!(
            Pattern::parse("//foo:all"),
            Pattern::Label {
                repo: String::new(),
                package: vec![Segment::Named("foo".to_string())],
                target: Target::All,
            }
        );
        for wildcard in ["//foo:*", "//foo:all-targets"] {
            assert_eq!(Pattern::parse(wildcard), Pattern::parse("//foo:all"));
        }
        assert_eq!(
            segments(&Pattern::parse("//foo/...")).last(),
            Some(&Segment::Any)
        );
        assert_eq!(Pattern::parse("//foo/...:all"), Pattern::parse("//foo/..."));
    }

    /// Bazel reads a label with no colon as naming the target its last path
    /// component names, and so does a pattern written that way.
    #[test]
    fn a_pattern_with_no_colon_names_the_target_its_last_component_does() {
        assert_eq!(Pattern::parse("//foo/bar"), Pattern::parse("//foo/bar:bar"));
    }

    #[test]
    fn text_that_is_no_label_is_a_substring() {
        for text in ["cpp", "foo:bar", "//foo:", "//foo//bar:baz"] {
            assert_eq!(
                Pattern::parse(text),
                Pattern::Substring(text.to_string()),
                "parsing {text:?}"
            );
        }
    }

    #[test]
    fn a_substring_matches_wherever_it_occurs_in_the_label() {
        let pattern = Pattern::parse("rules/cpp");
        assert!(pattern.matches("@@rules_foo+//tools/rules/cpp:lib"));
        assert!(!pattern.matches("//tools/rules/java:lib"));
    }

    #[test]
    fn an_ellipsis_covers_the_package_it_names_and_those_below_it() {
        let pattern = Pattern::parse("//foo/...");
        assert!(pattern.matches("//foo/bar:baz"));
        assert!(pattern.matches("//foo:baz"));
        assert!(!pattern.matches("//foobar:baz"));
        assert!(!pattern.matches("//bar/foo:baz"));
    }

    /// Matching walks the any-segments by backtracking, so a run of them costs
    /// a search over the ways to divide the path between them. A run says no
    /// more than a single one does, and is parsed as one.
    #[test]
    fn a_run_of_ellipses_parses_as_a_single_one() {
        assert_eq!(
            segments(&Pattern::parse("//.../.../.../rules/.../...")),
            [
                Segment::Any,
                Segment::Named("rules".to_string()),
                Segment::Any,
            ],
        );
        assert_eq!(Pattern::parse("//.../..."), Pattern::parse("//..."));
    }

    #[test]
    fn an_ellipsis_between_components_covers_any_run_of_them() {
        let pattern = Pattern::parse("//.../rules/cpp/...");
        assert!(pattern.matches("//src/main/rules/cpp:lib"));
        assert!(pattern.matches("//rules/cpp/toolchain:cc"));
        assert!(!pattern.matches("//src/rules/java:lib"));
    }

    #[test]
    fn all_selects_the_targets_of_one_package_and_no_package_below_it() {
        let pattern = Pattern::parse("//foo:all");
        assert!(pattern.matches("//foo:app"));
        assert!(pattern.matches("//foo:lib"));
        assert!(!pattern.matches("//foo/bar:app"));
    }

    #[test]
    fn a_named_target_matches_that_target_alone() {
        let pattern = Pattern::parse("//foo:app");
        assert!(pattern.matches("//foo:app"));
        assert!(!pattern.matches("//foo:app2"));
        assert!(!pattern.matches("//bar:app"));
    }

    #[test]
    fn the_root_package_is_named_by_an_empty_path() {
        let pattern = Pattern::parse("//:all");
        assert!(pattern.matches("//:app"));
        assert!(!pattern.matches("//foo:app"));
        assert!(Pattern::parse("//...").matches("//:app"));
    }

    /// Naming a repository and nothing else is what a reader has typed as soon
    /// as they have named it, and covers all of it — where a substring match on
    /// the same text would reach no canonical label at all.
    #[test]
    fn a_repository_written_with_no_package_covers_every_package_of_it() {
        assert_eq!(
            Pattern::parse("@rules_foo//"),
            Pattern::parse("@rules_foo//...")
        );

        let pattern = Pattern::parse("@rules_foo//");
        assert!(pattern.matches("@@rules_foo+//foo/bar:app"));
        assert!(pattern.matches("@@rules_foo+//:app"));
        assert!(!pattern.matches("//foo:app"));

        assert!(Pattern::parse("//").matches("//foo:app"));
    }

    #[test]
    fn a_pattern_with_no_repository_part_names_the_main_repository() {
        let pattern = Pattern::parse("//foo/...");
        assert!(pattern.matches("//foo:app"));
        assert!(!pattern.matches("@@rules_foo+//foo:app"));
    }

    #[test]
    fn a_repository_is_named_by_its_canonical_name_or_its_module_name() {
        for written in ["@@rules_foo+1.2.3//foo/...", "@rules_foo//foo/..."] {
            let pattern = Pattern::parse(written);
            assert!(pattern.matches("@@rules_foo+1.2.3//foo:app"), "{written}");
            assert!(!pattern.matches("//foo:app"), "{written}");
        }
    }

    /// A repository generated by a module extension carries its module's name as
    /// a prefix but stands for an apparent name of its own.
    #[test]
    fn a_module_name_does_not_reach_its_extensions_repositories() {
        assert!(!Pattern::parse("@rules_foo//foo/...").matches("@@rules_foo++ext+gen//foo:app"));
    }

    /// A repository is drawn and listed under the name it is written by, so
    /// that name has to be one a reader can type: two versions of one module
    /// would be written by the same module name, so each is written by the
    /// canonical name identifying it, and a pattern reaches a repository by the
    /// name it is written by whichever of the two that is.
    #[test]
    fn a_repository_is_reached_by_the_name_it_is_written_by() {
        let mut facets = Facets::default();
        let one = facets.intern_repo("rules_bar+1.2.3");
        let two = facets.intern_repo("rules_bar+2.0.0");
        let sole = facets.intern_repo("protobuf+");
        let written = WrittenRepos::of(&facets);

        let reaches = |repo, label| {
            Pattern::parse(&format!("{}//...", written.written(repo)))
                .resolved(&written)
                .matches(label)
        };
        assert_eq!(written.written(one), "@@rules_bar+1.2.3");
        assert!(reaches(one, "@@rules_bar+1.2.3//src:lib"));
        assert!(!reaches(one, "@@rules_bar+2.0.0//src:lib"));
        assert!(reaches(two, "@@rules_bar+2.0.0//src:lib"));
        assert_eq!(written.written(sole), "@protobuf");
        assert!(reaches(sole, "@@protobuf+//src:lib"));
        assert!(
            PackagePrefix::parse("@protobuf//src")
                .expect("a package prefix")
                .resolved(&written)
                .matches("@@protobuf+//src:lib")
        );
    }

    #[test]
    fn a_string_that_is_no_label_matches_no_label_pattern() {
        assert!(!Pattern::parse("//foo/...").matches("action7"));
    }

    /// A trailing ellipsis stands for a directory and everything under it, so
    /// it covers the package it names and every package below that one.
    #[test]
    fn an_ellipsis_covers_the_packages_it_reaches() {
        let pattern = Pattern::parse("//foo/...");
        assert!(pattern.covers_package("", "foo"));
        assert!(pattern.covers_package("", "foo/bar"));
        assert!(!pattern.covers_package("", "foobar"));
        assert!(!pattern.covers_package("", "bar/foo"));
        assert!(!pattern.covers_package("@@rules_foo+", "foo"));

        let everything = Pattern::parse("//...");
        assert!(everything.covers_package("", ""));
        assert!(everything.covers_package("", "foo/bar"));
    }

    #[test]
    fn all_covers_the_one_package_it_names() {
        let pattern = Pattern::parse("//foo:all");
        assert!(pattern.covers_package("", "foo"));
        assert!(!pattern.covers_package("", "foo/bar"));
    }

    /// A pattern naming a target names one target of a package, which says
    /// nothing of the package itself — and a pattern written with no colon
    /// names a target just the same.
    #[test]
    fn a_named_target_covers_no_package() {
        for written in ["//foo/bar:app", "//foo/bar"] {
            let pattern = Pattern::parse(written);
            assert!(!pattern.covers_package("", "foo/bar"), "{written}");
            assert!(!pattern.covers_package("", "foo"), "{written}");
        }
    }

    /// A package's own label is the head of every label of a target in it, so
    /// text found in one is found in the other.
    #[test]
    fn a_substring_covers_the_packages_whose_label_holds_it() {
        let pattern = Pattern::parse("tools/cpp");
        assert!(pattern.covers_package("", "tools/cpp"));
        assert!(pattern.covers_package("@@rules_foo+", "tools/cpp/toolchain"));
        assert!(!pattern.covers_package("", "tools/java"));
    }

    #[test]
    fn a_repository_is_covered_by_its_canonical_name_or_its_module_name() {
        for written in ["@@rules_foo+1.2.3//...", "@rules_foo//..."] {
            let pattern = Pattern::parse(written);
            assert!(
                pattern.covers_package("@@rules_foo+1.2.3", "foo"),
                "{written}"
            );
            assert!(!pattern.covers_package("", "foo"), "{written}");
        }
    }

    #[test]
    fn a_package_prefix_covers_the_packages_below_it() {
        let prefix = PackagePrefix::parse("//foo").unwrap();
        assert!(prefix.matches("//foo:app"));
        assert!(prefix.matches("//foo/bar:app"));
        assert!(!prefix.matches("//foobar:app"));
        assert!(!prefix.matches("@@rules_foo+//foo:app"));
    }

    #[test]
    fn a_repository_root_prefix_covers_every_package_of_that_repository() {
        let prefix = PackagePrefix::parse("@rules_foo//").unwrap();
        assert!(prefix.matches("@@rules_foo+//foo/bar:app"));
        assert!(prefix.matches("@@rules_foo+//:app"));
        assert!(!prefix.matches("//foo:app"));
    }

    #[test]
    fn a_package_prefix_ignores_any_target_written_after_it() {
        assert_eq!(
            PackagePrefix::parse("//foo/bar:app"),
            PackagePrefix::parse("//foo/bar")
        );
    }

    #[test]
    fn text_naming_no_package_is_no_package_prefix() {
        assert_eq!(PackagePrefix::parse("foo/bar"), None);
    }
}
