//! Bazel label parsing and the package-container scheme shared by the aquery,
//! query, and cquery builders, so all three group nodes under identical package
//! container ids.

use std::collections::HashSet;

use crate::graph::Graph;

/// The package a label names, split into the id of its container node and the
/// label shown on it. The repository prefix is preserved so that an external
/// `@repo//pkg:name` and a main-repo `//pkg:name` never collide:
///   - main-repo `//path/to:name` -> id `pkg:path/to`, label `//path/to`
///     (root `//:name` -> id `pkg:`, label `//`),
///   - external `@repo//pkg:name` -> id `pkg:@repo//pkg`, label `@repo//pkg`.
pub(crate) struct Package {
    pub id: String,
    pub label: String,
}

/// Parses a label into its package container. Labels not in `[@repo]//pkg:name`
/// form (no `//`) have no package.
pub(crate) fn package_of(label: &str) -> Option<Package> {
    let (repo, after) = label.split_once("//")?;
    let path = after.split_once(':').map(|(p, _)| p).unwrap_or(after);
    if repo.is_empty() {
        // Main-repo: id `pkg:{path}`, label `//{path}` (root: `pkg:`, `//`).
        Some(main_repo_package(path))
    } else {
        // External: the `@repo//` prefix keeps id and label distinct from any
        // same-named main-repo package.
        let qualified = format!("{repo}//{path}");
        Some(Package {
            id: format!("pkg:{qualified}"),
            label: qualified,
        })
    }
}

/// The main-repo package path of a label `//path/to:name` (`path/to`; root is
/// `""`), or `None` for external labels, which contribute no path that a
/// main-repo source directory could be nested under.
pub(crate) fn main_repo_package_path(label: &str) -> Option<String> {
    let path = label.strip_prefix("//")?;
    let path = path.split_once(':').map(|(p, _)| p).unwrap_or(path);
    Some(path.to_string())
}

/// Container id for a main-repo package path: `pkg:` plus the path (root is
/// `pkg:`).
fn package_box_id(path: &str) -> String {
    format!("pkg:{path}")
}

/// The container for a main-repo package path, matching `package_of`'s main-repo
/// scheme.
pub(crate) fn main_repo_package(path: &str) -> Package {
    Package {
        id: package_box_id(path),
        label: format!("//{path}"),
    }
}

/// Creates the package container `package` once and returns its id.
pub(crate) fn package_box(
    package: Package,
    package_boxes: &mut HashSet<String>,
    graph: &mut Graph,
) -> String {
    if package_boxes.insert(package.id.clone()) {
        graph.add_node(package.id.clone(), package.label, "package", None);
    }
    package.id
}
