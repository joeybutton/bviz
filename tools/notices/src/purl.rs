//! Package URLs (https://github.com/package-url/purl-spec), as package_metadata
//! declarations and rules_rs's crate metadata write them.

use packageurl::PackageUrl;

/// The name a reader knows the package by: the namespace-qualified name
/// where the namespace distinguishes it.
pub fn display_name(purl: &PackageUrl) -> String {
    match purl.namespace() {
        Some(namespace) => format!("{namespace}/{}", purl.name()),
        None => purl.name().to_owned(),
    }
}

/// Where the package's source is published. `None` for a package URL whose
/// shape names no exact source: a crate without its version, or a repository
/// without its revision.
pub fn source(purl: &PackageUrl) -> Option<String> {
    if let Some(vcs) = purl.qualifiers().get("vcs_url") {
        let url = vcs.strip_prefix("git+").unwrap_or(vcs);
        return match url.rsplit_once('@') {
            Some((repo, rev)) if !rev.contains('/') => Some(format!("{repo} at {rev}")),
            _ => None,
        };
    }
    let name = purl.name();
    match purl.ty() {
        "cargo" => Some(format!(
            "https://crates.io/crates/{name}/{}",
            purl.version()?
        )),
        "github" => {
            let repo = format!("https://github.com/{}", display_name(purl));
            Some(match purl.version() {
                Some(v) => format!("{repo}/tree/{v}"),
                None => repo,
            })
        }
        "bazel" => Some(format!("https://registry.bazel.build/modules/{name}")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> PackageUrl<'static> {
        text.parse().unwrap()
    }

    #[test]
    fn crate_from_git_names_the_commit() {
        let purl = parse(
            "pkg:cargo/gpui@0.2.2?vcs_url=git+https://github.com/zed-industries/zed@601ecb3e",
        );
        assert_eq!(display_name(&purl), "gpui");
        assert_eq!(purl.version(), Some("0.2.2"));
        assert_eq!(
            source(&purl).unwrap(),
            "https://github.com/zed-industries/zed at 601ecb3e"
        );
    }

    #[test]
    fn registry_crate_points_at_crates_io() {
        let purl = parse("pkg:cargo/lyon@1.0.19");
        assert_eq!(
            source(&purl).unwrap(),
            "https://crates.io/crates/lyon/1.0.19"
        );
    }

    #[test]
    fn github_package_keeps_its_namespace() {
        let purl = parse("pkg:github/rsms/inter@v4.1");
        assert_eq!(display_name(&purl), "rsms/inter");
        assert_eq!(
            source(&purl).unwrap(),
            "https://github.com/rsms/inter/tree/v4.1"
        );
        let purl = parse("pkg:github/openedges/elk-rs@v0.11.0%2Brs.2");
        assert_eq!(purl.version(), Some("v0.11.0+rs.2"));
    }

    #[test]
    fn unplaceable_shapes_have_no_source() {
        assert_eq!(source(&parse("pkg:generic/thing")), None);
        assert_eq!(source(&parse("pkg:cargo/lyon")), None);
        assert_eq!(
            source(&parse(
                "pkg:cargo/gpui?vcs_url=git+https://github.com/zed-industries/zed"
            )),
            None
        );
    }
}
