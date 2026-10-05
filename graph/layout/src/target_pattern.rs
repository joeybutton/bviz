//! Target patterns, read as Bazel reads one at the workspace root
//! (`cmdline/LabelParser.java` and `TargetPattern.Parser.parse` in
//! `cmdline/TargetPattern.java`), so text that Bazel rejects is rejected here
//! too, with the reason Bazel gives, and the targets of a graph each one names.
//!
//! Bazel is cited here as of its tag 9.2.0: a source file by its path beneath
//! `src/main/java/com/google/devtools/build/lib`, a test by its class beneath
//! `src/test/java/com/google/devtools/build/lib`.

mod resolve;

pub use resolve::{Resolved, TargetIndex};

/// What one pattern names, by the kinds Bazel tells patterns apart by.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TargetPattern {
    /// One target, `//foo:bar` or `//foo`, the latter naming `//foo:foo`.
    Single {
        repo: Repo,
        package: String,
        name: String,
    },
    /// The targets of one package its wildcard covers. Where the pattern is
    /// written from the root, as `//foo:all` is and `foo:all` is not, a target
    /// of the wildcard's very name takes precedence over them.
    InPackage {
        repo: Repo,
        package: String,
        wildcard: Wildcard,
        absolute: bool,
    },
    /// The targets of every package at or beneath a directory, `//foo/...`.
    Below {
        repo: Repo,
        directory: String,
        rules_only: bool,
    },
    /// A relative path naming neither a package part nor a wildcard,
    /// `foo/bar.txt`: the target the path is in the deepest package holding
    /// it, or the package itself where the path is one.
    PathAsTarget { path: String },
}

/// The target part standing for the targets of a package.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Wildcard {
    /// `all`, the rules.
    All,
    /// `all-targets`, every target.
    AllTargets,
    /// `*`, every target.
    Star,
}

impl Wildcard {
    fn parse(target: &str) -> Option<Self> {
        let all = [Wildcard::All, Wildcard::AllTargets, Wildcard::Star];
        all.into_iter()
            .find(|wildcard| wildcard.written() == target)
    }

    pub fn written(self) -> &'static str {
        match self {
            Wildcard::All => "all",
            Wildcard::AllTargets => "all-targets",
            Wildcard::Star => "*",
        }
    }

    /// Whether the wildcard leaves out the targets that are no rules.
    pub fn rules_only(self) -> bool {
        self == Wildcard::All
    }
}

/// The repository a pattern names.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Repo {
    /// The repository the build runs in, which a pattern written with no
    /// repository part names.
    Main,
    /// `@@name`.
    Canonical(String),
    /// `@name`, which only the main repository's repository mapping resolves.
    Apparent(String),
}

impl TargetPattern {
    /// The pattern `raw` writes, read as `TargetPattern.Parser.parse` reads a
    /// pattern in the main repository at the workspace root.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let Parts {
            repo,
            absolute,
            package,
            dots,
            target,
        } = Parts::parse(raw)?;
        let colon = raw.contains(':');
        if !absolute && package.is_empty() && !dots && !colon {
            return Ok(TargetPattern::PathAsTarget { path: target });
        }
        let wildcard = Wildcard::parse(&target);
        if dots {
            let rules_only = match wildcard {
                None if target.is_empty() => true,
                Some(wildcard) => wildcard.rules_only(),
                None => return Err(DOTS_WITHOUT_WILDCARD.to_string()),
            };
            return Ok(TargetPattern::Below {
                repo,
                directory: package,
                rules_only,
            });
        }
        Ok(match wildcard {
            Some(wildcard) if colon => TargetPattern::InPackage {
                repo,
                package,
                wildcard,
                absolute,
            },
            _ => TargetPattern::Single {
                repo,
                package,
                name: target,
            },
        })
    }
}

/// A pattern split as `LabelParser.Parts.parse` splits a label, each part
/// validated.
struct Parts {
    repo: Repo,
    /// Whether the package part was written from the root, after `//`.
    absolute: bool,
    /// Without any trailing `...` segment.
    package: String,
    /// Whether the package part ended in a `...` segment.
    dots: bool,
    target: String,
}

/// The package segment standing for a directory and everything beneath it.
const DOTS: &str = "...";

/// Why Bazel rejects a target that is no wildcard after a [`DOTS`] segment.
const DOTS_WITHOUT_WILDCARD: &str = "'...' can only be used with wildcard targets";

impl Parts {
    fn parse(raw: &str) -> Result<Self, String> {
        let canonical = raw.starts_with("@@");
        let sigil = if canonical { 2 } else { 1 };
        let double_slash = raw.find("//");
        let (repo, absolute, start) = match (raw.starts_with('@'), double_slash) {
            (true, None) => {
                // `@foo` names `@foo//:foo`.
                let name = &raw[sigil..];
                let repo = repo(name, canonical)?;
                return Ok(Parts {
                    repo,
                    absolute: true,
                    package: String::new(),
                    dots: false,
                    target: target_name("", name, false)?,
                });
            }
            (true, Some(at)) => (repo(&raw[sigil..at], canonical)?, true, at + 2),
            (false, _) => {
                let absolute = double_slash == Some(0);
                (Repo::Main, absolute, if absolute { 2 } else { 0 })
            }
        };
        let rest = &raw[start..];
        let colon = rest.find(':');
        let raw_package = colon.map_or(rest, |colon| &rest[..colon]);
        let dots = raw_package == DOTS || raw_package.ends_with("/...");
        let (package, target) = match colon {
            None if dots => (without_dots(raw_package), ""),
            // `foo/bar` names `:foo/bar`.
            None if !absolute => ("", rest),
            None => {
                let package = without_dots(raw_package);
                let last = package.rsplit('/').next().unwrap_or_default();
                (package, last)
            }
            Some(colon) => (without_dots(raw_package), &rest[colon + 1..]),
        };
        validate_package(package, target)?;
        Ok(Parts {
            repo,
            absolute,
            package: package.to_string(),
            dots,
            target: target_name(package, target, dots)?,
        })
    }
}

fn without_dots(package: &str) -> &str {
    match package.strip_suffix("/...") {
        Some(stripped) => stripped,
        None if package == DOTS => "",
        None => package,
    }
}

/// The repository `name` names, `canonical` where written with `@@`, checked
/// as `RepositoryName.validate` checks it. Either way the empty name is the
/// main repository's.
fn repo(name: &str, canonical: bool) -> Result<Repo, String> {
    if name == "." || name == ".." {
        return Err(format!(
            "invalid repository name '{name}': repo names are not allowed to be '{name}'"
        ));
    }
    let allowed = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+');
    if !name.chars().all(allowed) {
        return Err(format!(
            "invalid repository name '{name}': repo names may contain only A-Z, a-z, 0-9, \
             '-', '_', '.' and '+'"
        ));
    }
    Ok(match (name.is_empty(), canonical) {
        (true, _) => Repo::Main,
        (false, true) => Repo::Canonical(name.to_string()),
        (false, false) => Repo::Apparent(name.to_string()),
    })
}

/// Checks a package name as `LabelValidator.validatePackageName` does, with
/// the hint `LabelParser` adds where the name ends in the target's.
fn validate_package(package: &str, target: &str) -> Result<(), String> {
    let Some(problem) = package_problem(package) else {
        return Ok(());
    };
    Err(format!(
        "invalid package name '{package}': {problem}{}",
        perhaps_you_meant(package, target)
    ))
}

fn package_problem(package: &str) -> Option<&'static str> {
    if package.is_empty() {
        return None;
    }
    if package.starts_with('/') {
        return Some("package names may not start with '/'");
    }
    let allowed =
        |c: char| c.is_ascii_alphanumeric() || " !\"#$%&'()*+,-./;<=>?@[]^_`{|}~".contains(c);
    if !package.chars().all(allowed) {
        return Some(
            "package names may contain A-Z, a-z, 0-9, or any of ' !\"#$%&'()*+,-./;<=>?[]^_`{|}~' \
             (any ASCII character except 0-31, 127, ':', or '\\')",
        );
    }
    if package.ends_with('/') {
        return Some("package names may not end with '/'");
    }
    // Read from the end, so that of two faults the last is the one named.
    for segment in package.rsplit('/') {
        if segment.is_empty() {
            return Some("package names may not contain '//' path separators");
        }
        if segment.chars().all(|c| c == '.') {
            return Some("package name component contains only '.' characters");
        }
    }
    None
}

/// The target name `target` stands for, checked as
/// `LabelParser.validateAndProcessTargetName` checks it: empty only after a
/// `...` segment, and with a trailing `/.` dropped.
fn target_name(package: &str, target: &str, dots: bool) -> Result<String, String> {
    if dots && target.is_empty() {
        return Ok(String::new());
    }
    if let Some(problem) = target_problem(target) {
        return Err(format!(
            "invalid target name '{target}': {problem}{}",
            perhaps_you_meant(package, target)
        ));
    }
    Ok(target.strip_suffix("/.").unwrap_or(target).to_string())
}

/// What `LabelValidator.validateTargetName` finds wrong with `target`.
fn target_problem(target: &str) -> Option<String> {
    let problem = |text: &str| Some(text.to_string());
    if target.is_empty() {
        return problem("empty target name");
    }
    if target.starts_with('/') {
        return problem("target names may not start with '/'");
    }
    if target == ".." || target.starts_with("../") {
        return problem("target names may not contain up-level references '..'");
    }
    if target.starts_with("./") {
        return problem("target names may not contain '.' as a path segment");
    }
    if target.ends_with('\r') {
        return problem(
            "target names may not end with carriage returns \
             (perhaps the input source is CRLF-terminated)",
        );
    }
    for (at, c) in target.char_indices() {
        match c {
            '/' => {
                let rest = &target[at..];
                if rest.starts_with("/../") {
                    return problem("target names may not contain up-level references '..'");
                }
                if rest.starts_with("/./") {
                    return problem("target names may not contain '.' as a path segment");
                }
                if rest.starts_with("//") {
                    return problem("target names may not contain '//' path separators");
                }
            }
            ':' | '\\' => return Some(format!("target names may not contain '{c}'")),
            _ if c.is_ascii_control() => {
                return Some(format!(
                    "target names may not contain non-printable characters: '\\x{:02X}'",
                    c as u32
                ));
            }
            _ => {}
        }
    }
    if target.ends_with("/..") {
        return problem("target names may not contain up-level references '..'");
    }
    if target.ends_with('/') {
        return problem("target names may not end with '/'");
    }
    None
}

fn perhaps_you_meant(package: &str, target: &str) -> String {
    match package.ends_with(&format!("/{target}")) {
        true => format!(" (perhaps you meant \":{target}\"?)"),
        false => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(raw: &str) -> TargetPattern {
        TargetPattern::parse(raw).unwrap_or_else(|message| panic!("{raw:?}: {message}"))
    }

    fn single(repo: Repo, package: &str, name: &str) -> TargetPattern {
        TargetPattern::Single {
            repo,
            package: package.to_string(),
            name: name.to_string(),
        }
    }

    fn in_package(repo: Repo, package: &str, wildcard: Wildcard, absolute: bool) -> TargetPattern {
        TargetPattern::InPackage {
            repo,
            package: package.to_string(),
            wildcard,
            absolute,
        }
    }

    fn below(repo: Repo, directory: &str, rules_only: bool) -> TargetPattern {
        TargetPattern::Below {
            repo,
            directory: directory.to_string(),
            rules_only,
        }
    }

    fn path(path: &str) -> TargetPattern {
        TargetPattern::PathAsTarget {
            path: path.to_string(),
        }
    }

    fn apparent(name: &str) -> Repo {
        Repo::Apparent(name.to_string())
    }

    fn canonical(name: &str) -> Repo {
        Repo::Canonical(name.to_string())
    }

    /// The patterns `validPatterns_mainRepo_atRepoRoot` of
    /// `cmdline/TargetPatternTest` parses, each read as it reads them. An
    /// apparent repository is left for resolution against a build, which
    /// knows the names it maps.
    #[test]
    fn the_patterns_bazel_reads_at_the_root_parse_as_it_reads_them() {
        use Repo::Main;
        use Wildcard::{All, Star};
        let cases = [
            (":foo", single(Main, "", "foo")),
            ("foo:bar", single(Main, "foo", "bar")),
            ("foo:all", in_package(Main, "foo", All, false)),
            ("foo/...:all", below(Main, "foo", true)),
            ("foo:*", in_package(Main, "foo", Star, false)),
            ("foo", path("foo")),
            ("...", below(Main, "", true)),
            ("foo/bar", path("foo/bar")),
            ("//foo", single(Main, "foo", "foo")),
            ("//foo:bar", single(Main, "foo", "bar")),
            ("//foo:all", in_package(Main, "foo", All, true)),
            ("//foo/all", single(Main, "foo/all", "all")),
            ("//foo/...:all", below(Main, "foo", true)),
            ("//...", below(Main, "", true)),
            ("@repo", single(apparent("repo"), "", "repo")),
            ("@repo//foo:bar", single(apparent("repo"), "foo", "bar")),
            (
                "@repo//foo:all",
                in_package(apparent("repo"), "foo", All, true),
            ),
            ("@repo//:bar", single(apparent("repo"), "", "bar")),
            ("@repo//...", below(apparent("repo"), "", true)),
            ("@@repo", single(canonical("repo"), "", "repo")),
            (
                "@@repo//foo:all",
                in_package(canonical("repo"), "foo", All, true),
            ),
            ("@@repo//:bar", single(canonical("repo"), "", "bar")),
        ];
        for (raw, expected) in cases {
            assert_eq!(parsed(raw), expected, "{raw}");
        }
    }

    #[test]
    fn every_wildcard_over_all_targets_reaches_beyond_the_rules() {
        assert_eq!(parsed("//foo/...:*"), below(Repo::Main, "foo", false));
        assert_eq!(
            parsed("//foo/...:all-targets"),
            below(Repo::Main, "foo", false)
        );
        assert_eq!(
            parsed("//foo:all-targets"),
            in_package(Repo::Main, "foo", Wildcard::AllTargets, true)
        );
    }

    #[test]
    fn the_empty_repository_name_is_the_main_repository() {
        assert_eq!(parsed("@//foo:bar"), single(Repo::Main, "foo", "bar"));
        assert_eq!(parsed("@@//foo:bar"), single(Repo::Main, "foo", "bar"));
    }

    /// The patterns `invalidPatterns` of `cmdline/TargetPatternTest` rejects.
    #[test]
    fn the_patterns_bazel_rejects_are_rejected() {
        for raw in ["//Bar\\java", "", "/foo", "///foo", "@", "@foo//", "@@"] {
            assert!(TargetPattern::parse(raw).is_err(), "{raw:?}");
        }
    }

    /// The messages `testMistypedTarget` and `testEmptyTarget` of
    /// `pkgcache/LoadingPhaseRunnerTest` expect.
    #[test]
    fn a_rejected_pattern_says_why_as_bazel_does() {
        assert_eq!(
            TargetPattern::parse("foo//bar:missing"),
            Err(
                "invalid package name 'foo//bar': package names may not contain '//' path \
                 separators"
                    .to_string()
            )
        );
        assert_eq!(
            TargetPattern::parse(""),
            Err("invalid target name '': empty target name".to_string())
        );
    }

    #[test]
    fn an_ellipsis_takes_no_target_but_a_wildcard() {
        assert_eq!(
            TargetPattern::parse("//foo/...:bar"),
            Err("'...' can only be used with wildcard targets".to_string())
        );
        assert!(TargetPattern::parse("//.../foo").is_err());
    }

    #[test]
    fn a_package_ending_in_its_target_suggests_a_colon_and_a_target_holds_no_up_level_reference() {
        assert_eq!(
            TargetPattern::parse("//foo//bar.c"),
            Err(
                "invalid package name 'foo//bar.c': package names may not contain '//' path \
                 separators (perhaps you meant \":bar.c\"?)"
                    .to_string()
            )
        );
        assert_eq!(
            TargetPattern::parse("//foo:a/../b"),
            Err(
                "invalid target name 'a/../b': target names may not contain up-level \
                 references '..'"
                    .to_string()
            )
        );
    }

    #[test]
    fn a_repository_name_holds_only_what_bazel_allows() {
        assert!(TargetPattern::parse("@re po//foo").is_err());
        assert!(TargetPattern::parse("@..//foo").is_err());
        assert_eq!(
            parsed("@rules_foo+//foo"),
            single(apparent("rules_foo+"), "foo", "foo")
        );
    }

    #[test]
    fn a_path_is_kept_as_written_but_for_a_trailing_dot_segment() {
        assert_eq!(parsed("foo/bar.txt"), path("foo/bar.txt"));
        assert_eq!(parsed("foo/."), path("foo"));
        assert!(TargetPattern::parse("foo/../bar").is_err());
    }
}
