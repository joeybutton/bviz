//! Where a Bazel repository comes from, read off its canonical name.
//!
//! The query, cquery, and aquery protos carry repository names and nothing
//! else about a repository, but a canonical name states its origin
//! structurally: the main repository has none, a Bzlmod module is
//! `<module><sep>` or `<module><sep><version>`, and a module extension's
//! repository appends the defining repository, the extension, and the name the
//! extension gave it.

/// Which of a repository's names identifies it.
///
/// A canonical name names one repository outright and states its origin. An
/// apparent name is one module's own word for a repository, resolved through
/// that module's repository mapping, which a dump does not carry; so it
/// identifies a repository only among those no input named canonically, and a
/// repository so identified stands apart from every canonically named one
/// however the two names compare.
///
/// One repository therefore occupies two identities wherever the inputs write
/// it both ways — an aquery printing the canonical name while a query prints
/// the apparent one a dependent renamed it to — and is listed twice, drawing
/// its targets under each. That the two names meet is what the repository
/// mapping states and the files do not, and a name one module coined binds no
/// other module, so taking two names for one repository on the strength of how
/// they read would draw two repositories' targets onto one node, which nothing
/// downstream can tell apart again.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub enum RepoName<T> {
    Canonical(T),
    Apparent(T),
}

impl<T> RepoName<T> {
    pub fn map<U>(self, name: impl FnOnce(T) -> U) -> RepoName<U> {
        match self {
            RepoName::Canonical(held) => RepoName::Canonical(name(held)),
            RepoName::Apparent(held) => RepoName::Apparent(name(held)),
        }
    }
}

impl RepoName<String> {
    pub fn as_deref(&self) -> RepoName<&str> {
        match self {
            RepoName::Canonical(name) => RepoName::Canonical(name),
            RepoName::Apparent(name) => RepoName::Apparent(name),
        }
    }
}

/// The origin a canonical repository name encodes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RepoOrigin<'a> {
    /// The repository the build runs in, which carries the empty name.
    Main,
    /// A Bzlmod module. The version is part of the name only under a
    /// `multiple_version_override`, which is what puts several versions of one
    /// module in a dependency graph and so forces them apart by name.
    Module { version: Option<&'a str> },
    /// A repository a module extension generated. `use_repo_rule` declares its
    /// repositories through an extension named after the repo rule, so those
    /// land here too: nothing in the name tells the two apart.
    ModuleExtension { isolated: bool },
    /// The pseudo-repository holding Bazel's built-in Starlark symbols.
    Builtin,
    /// A WORKSPACE-era repository, an apparent name no repository mapping
    /// resolved, or a string that names no repository at all.
    WorkspaceOrUnknown,
}

/// A [`RepoOrigin`] with what it says about the one repository dropped, which
/// is the grain `repo_category()` selects repositories at.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum RepoCategory {
    Main,
    Module,
    ModuleExtension,
    Builtin,
    WorkspaceOrUnknown,
}

impl RepoCategory {
    pub const ALL: [RepoCategory; 5] = [
        RepoCategory::Main,
        RepoCategory::Module,
        RepoCategory::ModuleExtension,
        RepoCategory::Builtin,
        RepoCategory::WorkspaceOrUnknown,
    ];

    /// The word a query names the category by.
    pub const fn spelling(self) -> &'static str {
        match self {
            RepoCategory::Main => "main",
            RepoCategory::Module => "module",
            RepoCategory::ModuleExtension => "extension",
            RepoCategory::Builtin => "builtin",
            RepoCategory::WorkspaceOrUnknown => "unknown",
        }
    }
}

/// The modules Bazel gives a bare canonical name, carrying neither separator
/// nor version.
const WELL_KNOWN_MODULES: [&str; 2] = ["bazel_tools", "platforms"];

const BUILTINS: &str = "_builtins";

/// Bazel prefixes an isolated extension usage's name with this, which an
/// exported Starlark identifier can never start with.
const ISOLATED_USAGE_MARK: char = '_';

/// The origin of the repository `name` names, with any `@@` or `@` sigil
/// stripped. Every string classifies: one that fits no known shape is
/// [`RepoOrigin::WorkspaceOrUnknown`], so an unfamiliar Bazel release degrades
/// to an unresolved origin rather than a failure.
pub fn classify(name: &str) -> RepoOrigin<'_> {
    let name = strip_sigil(name);
    if name.is_empty() {
        return RepoOrigin::Main;
    }
    if name == BUILTINS {
        return RepoOrigin::Builtin;
    }
    if WELL_KNOWN_MODULES.contains(&name) {
        return RepoOrigin::Module { version: None };
    }
    if is_diagnostic(name) {
        return RepoOrigin::WorkspaceOrUnknown;
    }
    let separator = separator(name);
    let Some((defining, rest)) = name.split_once(separator) else {
        return RepoOrigin::WorkspaceOrUnknown;
    };
    match rest.split_once(separator) {
        None => RepoOrigin::Module {
            version: Some(rest).filter(|version| !version.is_empty()),
        },
        Some((second, tail)) => RepoOrigin::ModuleExtension {
            isolated: extension_name(defining, second, tail, separator)
                .starts_with(ISOLATED_USAGE_MARK),
        },
    }
}

/// The name of the module whose repository `name` is, or `None` when `name`
/// names no module repository.
pub fn module_name(name: &str) -> Option<&str> {
    let name = strip_sigil(name);
    match classify(name) {
        RepoOrigin::Module { .. } => Some(leading(name)),
        _ => None,
    }
}

/// The module `name` is evidence of: the module a module repository belongs to,
/// and the module that defines the extension a generated repository came from.
/// `None` where the name is evidence of no module — the main repository, the
/// built-in repository, an extension the main repository itself declares, or a
/// name encoding no origin.
pub fn evidenced_module(name: &str) -> Option<&str> {
    let name = strip_sigil(name);
    match classify(name) {
        RepoOrigin::Module { .. } | RepoOrigin::ModuleExtension { .. } => {
            Some(leading(name)).filter(|module| !module.is_empty())
        }
        _ => None,
    }
}

/// The name the module extension that generated the repository `name` gave
/// it, which is the last component of its canonical name; `None` where `name`
/// names no extension's repository. Bazel does not keep canonical names
/// stable across releases
/// (<https://bazel.build/external/overview#canonical-repo-name>), so this reads
/// only the layout [`classify`] reads.
pub fn extension_given_name(name: &str) -> Option<&str> {
    let name = strip_sigil(name);
    match classify(name) {
        RepoOrigin::ModuleExtension { .. } => name.rsplit(separator(name)).next(),
        _ => None,
    }
    .filter(|given| !given.is_empty())
}

/// The first component of a canonical name, which is the module the name
/// belongs to or was generated by wherever it names one.
fn leading(name: &str) -> &str {
    name.split_once(separator(name))
        .map_or(name, |(leading, _)| leading)
}

pub(crate) fn strip_sigil(name: &str) -> &str {
    name.strip_prefix("@@")
        .or_else(|| name.strip_prefix('@'))
        .unwrap_or(name)
}

/// The separator joining a canonical name's components: `+` in Bazel 8 and
/// later, `~` in Bazel 7. A module name can contain neither, so a name that
/// holds a `+` anywhere is written in the newer scheme.
fn separator(name: &str) -> char {
    if name.contains('+') { '+' } else { '~' }
}

/// Whether `name` is Bazel's rendering of a repository it could not resolve
/// (`[unknown repo 'x' requested from @@y]`) rather than a repository name,
/// which admits only letters, digits, `-`, `_`, `.` and the separator.
fn is_diagnostic(name: &str) -> bool {
    name.contains(' ') || name.contains('[')
}

/// The extension component of an extension repository's name, whose leading
/// components are the canonical name of the repository that defines the
/// extension: empty for the main repository, and `<module><sep><version>` for
/// a module, whose version component can never begin with
/// [`ISOLATED_USAGE_MARK`].
fn extension_name<'a>(
    defining: &'a str,
    second: &'a str,
    tail: &'a str,
    separator: char,
) -> &'a str {
    if defining.is_empty() || second.starts_with(ISOLATED_USAGE_MARK) {
        second
    } else {
        tail.split_once(separator)
            .map_or(tail, |(extension, _)| extension)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_canonical_name_states_its_repositorys_origin() {
        let cases = [
            ("", RepoOrigin::Main),
            ("bazel_tools", RepoOrigin::Module { version: None }),
            ("platforms", RepoOrigin::Module { version: None }),
            ("_builtins", RepoOrigin::Builtin),
            ("rules_foo+", RepoOrigin::Module { version: None }),
            (
                "rules_foo+1.2.3",
                RepoOrigin::Module {
                    version: Some("1.2.3"),
                },
            ),
            // Bazel 7 joined the same components with a tilde.
            (
                "rules_foo~1.2.3",
                RepoOrigin::Module {
                    version: Some("1.2.3"),
                },
            ),
            (
                "rules_foo++ext+repo",
                RepoOrigin::ModuleExtension { isolated: false },
            ),
            (
                "rules_foo~~ext~repo",
                RepoOrigin::ModuleExtension { isolated: false },
            ),
            // An extension defined in the main repository, as `use_repo_rule`
            // makes one, leaves the defining component empty.
            (
                "+http_archive+foo",
                RepoOrigin::ModuleExtension { isolated: false },
            ),
            (
                "rules_foo++_ext+mod+1.0+usage+repo",
                RepoOrigin::ModuleExtension { isolated: true },
            ),
            // A well-known module's canonical name carries no separator, so an
            // extension it defines puts the isolation mark straight after it.
            (
                "bazel_tools+_ext+mod+1.0+usage+repo",
                RepoOrigin::ModuleExtension { isolated: true },
            ),
            // A repository declared in WORKSPACE has a plain name.
            ("somerepo", RepoOrigin::WorkspaceOrUnknown),
            (
                "[unknown repo 'foo' requested from @@bar+]",
                RepoOrigin::WorkspaceOrUnknown,
            ),
        ];

        for (name, expected) in cases {
            assert_eq!(classify(name), expected, "classifying {name:?}");
        }
    }

    /// A canonical name names the module it belongs to or the one whose
    /// extension generated it, which is what proves that module exists.
    #[test]
    fn a_canonical_name_is_evidence_of_the_module_it_names() {
        let cases = [
            ("rules_foo+", Some("rules_foo")),
            ("rules_foo+1.2.3", Some("rules_foo")),
            ("rules_foo~1.2.3", Some("rules_foo")),
            ("bazel_tools", Some("bazel_tools")),
            ("llvm++llvm+llvm-project", Some("llvm")),
            ("rules_foo+1.2.3+ext+repo", Some("rules_foo")),
            ("rules_foo++_ext+mod+1.0+usage+repo", Some("rules_foo")),
            ("bazel_tools+_ext+mod+1.0+usage+repo", Some("bazel_tools")),
            // An extension the main repository declares names no module.
            ("+http_archive+foo", None),
            ("", None),
            ("_builtins", None),
            ("somerepo", None),
        ];

        for (name, expected) in cases {
            assert_eq!(evidenced_module(name), expected, "reading {name:?}");
        }
    }

    #[test]
    fn an_extension_s_repository_is_given_its_name_by_the_last_component() {
        let cases = [
            ("@@llvm++llvm+llvm-project", Some("llvm-project")),
            ("rules_foo~~ext~repo", Some("repo")),
            ("rules_foo++_ext+mod+1.0+usage+repo", Some("repo")),
            ("+http_archive+foo", Some("foo")),
            ("rules_foo+1.2.3", None),
            ("somerepo", None),
            ("", None),
        ];
        for (name, expected) in cases {
            assert_eq!(extension_given_name(name), expected, "reading {name:?}");
        }
    }

    #[test]
    fn a_sigil_on_the_name_does_not_change_its_origin() {
        assert_eq!(classify("@@"), RepoOrigin::Main);
        assert_eq!(
            classify("@@rules_foo+"),
            RepoOrigin::Module { version: None }
        );
        assert_eq!(
            classify("@rules_foo+"),
            RepoOrigin::Module { version: None }
        );
    }
}
