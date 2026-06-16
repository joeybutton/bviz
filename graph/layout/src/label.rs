//! Bazel label parsing and the package-container hierarchy shared by the
//! aquery, query, and cquery builders.
//!
//! Packages nest by path component, so `//a/b` contains `//a/b/c`. A chain of
//! single-child packages that hold no content of their own folds into one box
//! labeled with the whole chain, while branch points and packages with content
//! start their own box. Every builder attaches its targets to the same boxes,
//! so the three render with an identical package structure.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::graph::Graph;

/// The repository and package path a label names: `//a/b:c` -> `("", "a/b")`,
/// `//:c` -> `("", "")`, `@r//a:c` -> `("@r", "a")`. A label with no `//` names
/// no package.
pub(crate) fn parse_package(label: &str) -> Option<(&str, &str)> {
    let (repo, after) = label.split_once("//")?;
    let path = after.split_once(':').map(|(p, _)| p).unwrap_or(after);
    Some((repo, path))
}

/// The owned (repository, package path) key of a label's package.
pub(crate) fn package_key(label: &str) -> Option<(String, String)> {
    parse_package(label).map(|(repo, path)| (repo.to_string(), path.to_string()))
}

/// The `:name` shown on a target box: the portion of a label after its last `:`
/// (`//a/b:c` -> `:c`), or the whole label when it has none.
pub(crate) fn target_label(label: &str) -> String {
    match label.rsplit_once(':') {
        Some((_, name)) => format!(":{name}"),
        None => label.to_string(),
    }
}

#[derive(Default)]
struct Trie {
    children: BTreeMap<String, Trie>,
    /// A package with content ends here, as opposed to an intermediate path
    /// segment that exists only to reach deeper packages.
    real: bool,
}

impl Trie {
    fn insert(&mut self, path: &str) {
        let mut node = self;
        if !path.is_empty() {
            for component in path.split('/') {
                node = node.children.entry(component.to_string()).or_default();
            }
        }
        node.real = true;
    }
}

/// The package containers laid out as a nesting hierarchy with single-child
/// chains folded together. Maps each real package to the box that holds its
/// targets, and resolves the box a source file nests under.
pub(crate) struct PackageHierarchy {
    box_id: HashMap<(String, String), String>,
    /// Real main-repo package paths, sorted, for the source-enclosing lookup.
    main_paths: Vec<String>,
}

impl PackageHierarchy {
    /// Builds the boxes for every real package and the folded ancestors that
    /// reach them, emitting one `package` container node per box into `graph`.
    pub(crate) fn build(packages: &BTreeSet<(String, String)>, graph: &mut Graph) -> Self {
        let mut tries: BTreeMap<&str, Trie> = BTreeMap::new();
        for (repo, path) in packages {
            tries.entry(repo.as_str()).or_default().insert(path);
        }
        let mut hierarchy = Self {
            box_id: HashMap::new(),
            main_paths: Vec::new(),
        };
        for (repo, trie) in &tries {
            hierarchy.emit_root(trie, repo, graph);
        }
        hierarchy.main_paths = packages
            .iter()
            .filter(|(repo, _)| repo.is_empty())
            .map(|(_, path)| path.clone())
            .collect();
        hierarchy.main_paths.sort();
        hierarchy
    }

    /// Emits a repository's boxes. The repository root becomes a `//` (or
    /// `@repo//`) box only when it is itself a package; otherwise each top-level
    /// subtree is its own outermost box.
    fn emit_root(&mut self, root: &Trie, repo: &str, graph: &mut Graph) {
        let parent = if root.real {
            let id = box_node_id(repo, "");
            graph.add_node(id.clone(), repo_prefix(repo), "package", None);
            self.box_id.insert((repo.to_string(), String::new()), id.clone());
            Some(id)
        } else {
            None
        };
        for (component, child) in &root.children {
            self.emit(child, repo, component, &[component.clone()], parent.as_deref(), graph);
        }
    }

    fn emit(
        &mut self,
        node: &Trie,
        repo: &str,
        full_path: &str,
        pending: &[String],
        parent_box: Option<&str>,
        graph: &mut Graph,
    ) {
        // Fold a single-child intermediate into its child; a package with
        // content or a branch point anchors its own box.
        if !node.real && node.children.len() == 1 {
            let (component, child) = node.children.iter().next().unwrap();
            let child_path = format!("{full_path}/{component}");
            let mut chain = pending.to_vec();
            chain.push(component.clone());
            self.emit(child, repo, &child_path, &chain, parent_box, graph);
            return;
        }

        let label = match parent_box {
            None => format!("{}{}", repo_prefix(repo), pending.join("/")),
            Some(_) => pending.join("/"),
        };
        let id = box_node_id(repo, full_path);
        graph.add_node(id.clone(), label, "package", parent_box.map(str::to_string));
        if node.real {
            self.box_id
                .insert((repo.to_string(), full_path.to_string()), id.clone());
        }
        for (component, child) in &node.children {
            let child_path = format!("{full_path}/{component}");
            self.emit(child, repo, &child_path, &[component.clone()], Some(&id), graph);
        }
    }

    /// The box that holds a label's own package, or `None` when the label names
    /// no package.
    pub(crate) fn package_box(&self, label: &str) -> Option<&str> {
        let (repo, path) = parse_package(label)?;
        self.box_id
            .get(&(repo.to_string(), path.to_string()))
            .map(String::as_str)
    }

    /// The path of the deepest real main-repo package enclosing directory `dir`,
    /// or `None` when none does.
    pub(crate) fn enclosing_package(&self, dir: &str) -> Option<&str> {
        self.main_paths
            .iter()
            .filter(|pkg| encloses(pkg, dir))
            .max_by_key(|pkg| pkg.len())
            .map(String::as_str)
    }
}

/// The display prefix of a repository root: `//` for the main repo, `@repo//`
/// for an external one.
fn repo_prefix(repo: &str) -> String {
    if repo.is_empty() {
        "//".to_string()
    } else {
        format!("{repo}//")
    }
}

/// Container node id for a package path, keeping external repos distinct from a
/// same-named main-repo path: main `pkg:a/b`, external `pkg:@repo//a/b`.
fn box_node_id(repo: &str, path: &str) -> String {
    if repo.is_empty() {
        format!("pkg:{path}")
    } else {
        format!("pkg:{repo}//{path}")
    }
}

/// Whether package path `pkg` encloses directory `dir`: they are equal, or `dir`
/// lies under `pkg` at a path-segment boundary. The root package `""` encloses
/// every directory.
fn encloses(pkg: &str, dir: &str) -> bool {
    pkg.is_empty()
        || dir == pkg
        || dir
            .strip_prefix(pkg)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(packages: &[(&str, &str)]) -> (Graph, PackageHierarchy) {
        let set: BTreeSet<(String, String)> = packages
            .iter()
            .map(|(repo, path)| (repo.to_string(), path.to_string()))
            .collect();
        let mut graph = Graph::new();
        let hierarchy = PackageHierarchy::build(&set, &mut graph);
        (graph, hierarchy)
    }

    fn node<'a>(graph: &'a Graph, id: &str) -> &'a crate::graph::Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap()
    }

    #[test]
    fn single_child_chain_folds_into_the_branch_box() {
        let base = "src/main/java/com/google/devtools/common/options";
        let (graph, _) = build(&[
            ("", base),
            ("", &format!("{base}/processor")),
            ("", &format!("{base}/testing")),
        ]);

        // The target-less chain down to options folds into one outermost box
        // labeled with the full path; processor and testing nest with only their
        // own component.
        let outer = node(&graph, &format!("pkg:{base}"));
        assert_eq!(outer.label, format!("//{base}"));
        assert_eq!(outer.parent, None);
        assert_eq!(node(&graph, &format!("pkg:{base}/processor")).label, "processor");
        assert_eq!(
            node(&graph, &format!("pkg:{base}/processor")).parent.as_deref(),
            Some(format!("pkg:{base}").as_str())
        );
        assert_eq!(node(&graph, &format!("pkg:{base}/testing")).label, "testing");
        // No box was emitted for the folded intermediate segments.
        assert!(!graph.nodes.iter().any(|n| n.id == "pkg:src"));
    }

    #[test]
    fn nested_single_child_below_a_branch_folds_too() {
        // foo branches (bar, baz); under bar a single-child chain bar/x/y folds.
        let (graph, _) = build(&[("", "foo/bar/x/y"), ("", "foo/baz")]);

        assert_eq!(node(&graph, "pkg:foo").label, "//foo");
        assert_eq!(node(&graph, "pkg:foo/baz").label, "baz");
        let folded = node(&graph, "pkg:foo/bar/x/y");
        assert_eq!(folded.label, "bar/x/y");
        assert_eq!(folded.parent.as_deref(), Some("pkg:foo"));
    }

    #[test]
    fn a_package_with_content_anchors_even_with_a_single_child() {
        // foo holds content and has one sub-package foo/bar, so foo keeps its own
        // box rather than folding into bar.
        let (graph, _) = build(&[("", "foo"), ("", "foo/bar")]);

        assert_eq!(node(&graph, "pkg:foo").label, "//foo");
        assert_eq!(node(&graph, "pkg:foo/bar").label, "bar");
        assert_eq!(node(&graph, "pkg:foo/bar").parent.as_deref(), Some("pkg:foo"));
    }

    #[test]
    fn separate_top_level_packages_are_separate_outermost_boxes() {
        let (graph, _) = build(&[("", "foo"), ("", "bar")]);

        assert_eq!(node(&graph, "pkg:foo").label, "//foo");
        assert_eq!(node(&graph, "pkg:foo").parent, None);
        assert_eq!(node(&graph, "pkg:bar").label, "//bar");
        assert_eq!(node(&graph, "pkg:bar").parent, None);
    }

    #[test]
    fn root_package_wraps_its_sub_packages() {
        // A `//:x` target makes the root a real package, so it becomes a `//`
        // box that holds the nested sub-package.
        let (graph, _) = build(&[("", ""), ("", "a")]);

        assert_eq!(node(&graph, "pkg:").label, "//");
        assert_eq!(node(&graph, "pkg:").parent, None);
        assert_eq!(node(&graph, "pkg:a").label, "a");
        assert_eq!(node(&graph, "pkg:a").parent.as_deref(), Some("pkg:"));
    }

    #[test]
    fn external_repo_packages_carry_their_repo_prefix() {
        let (graph, hierarchy) = build(&[("@r", "a/b"), ("", "a/b")]);

        assert_eq!(node(&graph, "pkg:@r//a/b").label, "@r//a/b");
        assert_eq!(node(&graph, "pkg:a/b").label, "//a/b");
        assert_eq!(hierarchy.package_box("@r//a/b:t"), Some("pkg:@r//a/b"));
        assert_eq!(hierarchy.package_box("//a/b:t"), Some("pkg:a/b"));
    }

    #[test]
    fn enclosing_package_picks_the_deepest_real_package() {
        let (_graph, hierarchy) = build(&[("", "foo"), ("", "foo/bar")]);

        // A source under foo/bar/sub belongs to the deeper foo/bar.
        assert_eq!(hierarchy.enclosing_package("foo/bar/sub"), Some("foo/bar"));
        assert_eq!(hierarchy.enclosing_package("foo/other"), Some("foo"));
        assert_eq!(hierarchy.enclosing_package("elsewhere"), None);
    }

    #[test]
    fn target_label_keeps_only_the_name() {
        assert_eq!(target_label("//a/b:c"), ":c");
        assert_eq!(target_label("@r//a:c"), ":c");
        assert_eq!(target_label("//:c"), ":c");
    }
}
