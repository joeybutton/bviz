//! Where a Bazel repository comes from, read off its canonical name.
//!
//! The query, cquery, and aquery protos carry repository names and nothing
//! else about a repository, but a canonical name states its origin
//! structurally: the main repository has none, a Bzlmod module is
//! `<module><sep>` or `<module><sep><version>`, and a module extension's
//! repository appends the defining repository, the extension, and the name the
//! extension gave it.

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
    let name = name
        .strip_prefix("@@")
        .or_else(|| name.strip_prefix('@'))
        .unwrap_or(name);
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
                "rules_foo+_ext+mod+1.0+usage+repo",
                RepoOrigin::ModuleExtension { isolated: true },
            ),
            (
                "rules_foo++_ext+mod+1.0+usage+repo",
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
