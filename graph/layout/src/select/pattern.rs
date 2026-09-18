//! Target patterns over canonical labels, in the vocabulary Bazel's own target
//! patterns use.
//!
//! `//foo:bar` names one target and `//foo:all` every target of one package.
//! `...` stands for any run of path segments wherever it appears, so `//foo/...`
//! reaches `//foo` and every package below it while `//.../cpp/...` reaches
//! every package with a `cpp` segment. A repository part matches a canonical
//! repository name outright and a module's own repository by the module name as
//! well, since `@rules_foo//...` is how that repository is written before a
//! repository mapping resolves it.

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
    /// `:all`, or a package path ending in `...`, both of which leave the
    /// target free. A target genuinely named `all` cannot be written, as it
    /// cannot in a Bazel command line either.
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
                repo_matches(repo, parts.repo)
                    && target.matches(parts.name)
                    && segments_match(package, &components(parts.package))
            }
        }
    }

    fn label(text: &str) -> Option<Self> {
        let (repo, rest) = text.split_once("//")?;
        let (path, written) = match rest.split_once(':') {
            Some((path, "all" | "*")) => (path, Some(Target::All)),
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

    /// Whether the canonical label `label` sits in this package or one below
    /// it, the two split at a path component boundary so that `//foo` covers
    /// `//foo/bar:t` and not `//foobar:t`.
    pub fn matches(&self, label: &str) -> bool {
        let Some(parts) = Parts::of(label) else {
            return false;
        };
        repo_matches(&self.repo, parts.repo) && encloses(&self.path, parts.package)
    }
}

/// A canonical label split into the parts a pattern matches one by one.
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
            repo: strip_sigil(repo),
            package,
            name,
        })
    }
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

    #[test]
    fn a_string_that_is_no_label_matches_no_label_pattern() {
        assert!(!Pattern::parse("//foo/...").matches("action7"));
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
