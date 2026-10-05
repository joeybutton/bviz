//! Bazel label parsing and the package-container hierarchy shared by the
//! aquery, query, and cquery builders.
//!
//! Packages nest by path component, so `//a/b` contains `//a/b/c`. A chain of
//! single-child packages that hold no content of their own folds into one box
//! labeled with the whole chain, while branch points and packages with content
//! start their own box. Every builder attaches its targets to the same boxes,
//! so the three render with an identical package structure.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use bazel_query_proto::blaze_query::{Target, target::Discriminator};

use crate::facets::{Facets, FacetsBuilder, RepoRef, WrittenRepos};
use crate::graph::{Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::repo::RepoName;

/// The repository and package path a label names: `//a/b:c` -> `("", "a/b")`,
/// `//:c` -> `("", "")`, `@r//a:c` -> `("@r", "a")`. A label with no `//` names
/// no package.
pub(crate) fn parse_package(label: &str) -> Option<(&str, &str)> {
    let (repo, after) = label.split_once("//")?;
    let path = after.split_once(':').map(|(p, _)| p).unwrap_or(after);
    Some((repo, path))
}

/// The canonical repository name a label states outright, or `None` when it
/// names the main repository, an apparent repository, or no package at all.
pub(crate) fn canonical_repo(label: &str) -> Option<&str> {
    let (repo, _) = parse_package(label)?;
    let canonical = repo.strip_prefix("@@")?;
    (!canonical.is_empty()).then_some(canonical)
}

/// Whether `label` names its repository canonically: `@@name//...`, or with no
/// repository part at all, which is how the main repository's labels read. A
/// label that reached bviz by an apparent name no input pinned to a canonical
/// one keeps its single `@`, which states nothing of where its repository comes
/// from.
pub(crate) fn is_canonical(label: &str) -> bool {
    !label.starts_with('@') || label.starts_with("@@")
}

/// The repository `label` identifies, and which of its names does so:
/// `@@r+//a:b` -> canonical `r+`, `//a:b` -> the main repository's canonical
/// empty name, and `@r//a:b` -> apparent `r`. `None` where the label names no
/// package.
pub(crate) fn identified_repo(label: &str) -> Option<RepoName<&str>> {
    parse_package(label).map(|(repo, _)| identified_repo_name(repo))
}

/// [`identified_repo`] for a label's repository part on its own, as a package
/// key holds it: `@@r+` -> canonical `r+`, `@r` -> apparent `r`, and the main
/// repository's canonical empty name for the empty part.
pub(crate) fn identified_repo_name(repo: &str) -> RepoName<&str> {
    match repo.strip_prefix('@') {
        None => RepoName::Canonical(repo),
        Some(apparent) => match apparent.strip_prefix('@') {
            Some(canonical) => RepoName::Canonical(canonical),
            None => RepoName::Apparent(apparent),
        },
    }
}

/// The owned (repository, package path) key of a canonical label's package.
fn package_key(label: &str) -> Option<(String, String)> {
    parse_package(label).map(|(repo, path)| (repo.to_string(), path.to_string()))
}

/// The distinct package keys of `labels` in canonical form, dropping any label
/// with no package.
pub(crate) fn package_keys<'a>(
    labels: impl Iterator<Item = &'a str>,
    repos: &RepoMapping,
) -> BTreeSet<(String, String)> {
    labels
        .filter_map(|label| package_key(&repos.canonicalize_label(label)))
        .collect()
}

/// Rewrites a label into the form aquery emits, so one target carries one node
/// id whichever input names it.
///
/// aquery stringifies labels canonically. query and cquery print the apparent
/// form `@repo//pkg:t` by default and, under `--consistent_labels`, the
/// canonical form plus `@@//pkg:t` for the main repository. Only the apparent
/// form needs this map.
#[derive(Default)]
pub(crate) struct RepoMapping {
    canonical: HashSet<String>,
}

impl RepoMapping {
    /// The canonical repository names `observed` across the inputs, to which an
    /// apparent name reading the same resolves.
    ///
    /// Such a name is one its repository carries in its own right — a
    /// WORKSPACE-era repository, a module Bazel names bare — and so is the name
    /// a repository mapping resolves to it. Every other apparent name is one
    /// module's own word for a repository, which that module's repository
    /// mapping alone states and [`RepoName`] identifies a repository by.
    pub(crate) fn infer<'a>(observed: impl Iterator<Item = &'a str>) -> Self {
        RepoMapping {
            canonical: observed.map(str::to_string).collect(),
        }
    }

    pub(crate) fn canonicalize_label<'a>(&self, label: &'a str) -> Cow<'a, str> {
        let Some(repo) = label.strip_prefix('@') else {
            return Cow::Borrowed(label);
        };
        if let Some(canonical) = repo.strip_prefix('@') {
            // `@@//pkg:t` is the main repository, whose canonical labels carry
            // no repository part at all.
            if canonical.starts_with("//") {
                return Cow::Borrowed(canonical);
            }
            return Cow::Borrowed(label);
        }
        let Some((apparent, package)) = repo.split_once("//") else {
            return Cow::Borrowed(label);
        };
        if self.canonical.contains(apparent) {
            Cow::Owned(format!("@@{apparent}//{package}"))
        } else {
            Cow::Borrowed(label)
        }
    }
}

/// The `:name` shown on a target box: the portion of a label after its last `:`
/// (`//a/b:c` -> `:c`), or the whole label when it has none.
pub(crate) fn target_label(label: &str) -> String {
    match label.rsplit_once(':') {
        Some((_, name)) => format!(":{name}"),
        None => label.to_string(),
    }
}

/// The query-independent key of a configuration: its `checksum`, or `cfg{id}`
/// when none is recorded. The checksum is stable across aquery and cquery dumps,
/// so configured targets keyed by it merge across builders.
pub(crate) fn config_key(config_id: u32, checksum: Option<&str>) -> String {
    match checksum {
        Some(checksum) if !checksum.is_empty() => checksum.to_string(),
        _ => format!("cfg{config_id}"),
    }
}

/// A configuration's display mnemonic, or `config {id}` when it records no
/// non-empty one.
pub(crate) fn config_mnemonic(config_id: u32, mnemonic: Option<&str>) -> String {
    mnemonic
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("config {config_id}"))
}

/// The node id of the target `label` names.
pub(crate) fn target_node_id(label: &str) -> String {
    format!("{TARGET_PREFIX}{label}")
}

/// The canonical label the target node id `node_id` names, or `None` where it
/// names a node that is no target.
pub(crate) fn target_node_label(node_id: &str) -> Option<&str> {
    node_id.strip_prefix(TARGET_PREFIX)
}

/// What sets a target's node id apart from the package box, configured target,
/// and output artifact that the same label also names.
const TARGET_PREFIX: &str = "target:";

/// The node id of a configured target: `ct:{label}#{config_key}`. Keyed by
/// `config_key` so the aquery and cquery builders emit the same id and merge.
pub(crate) fn ct_node_id(label: &str, config_key: &str) -> String {
    format!("ct:{label}#{config_key}")
}

/// The node id of the action whose primary output lies at exec path
/// `primary_output`. That path is what Bazel's other outputs can name the
/// action by: a build event stream gives it as
/// `ActionCompletedId.primary_output`, a profile as the action's `out` under
/// `--experimental_profile_include_primary_output`, and an execution log lists
/// it, unmarked, among a spawn's outputs.
pub(crate) fn action_node_id(primary_output: &str) -> String {
    format!("{ACTION_PREFIX}{primary_output}")
}

/// The exec path the action node id `node_id` is formed from, or `None` where
/// it is formed from the action's place.
pub(crate) fn action_primary_output(node_id: &str) -> Option<&str> {
    node_id.strip_prefix(ACTION_PREFIX)
}

const ACTION_PREFIX: &str = "action:";

/// The node id of an action naming no primary output, the `place`-th the query
/// lists. A digit follows `action` here and a colon in [`action_node_id`], so
/// no id has both forms.
pub(crate) fn unnamed_action_node_id(place: usize) -> String {
    format!("action{place}")
}

/// Finds the nodes of one graph by what Bazel's other outputs name them by.
///
/// A label is taken as Bazel prints it: `//pkg:name` or `@@//pkg:name` for
/// the main repository, `@@repo//pkg:name` for another, and `@repo//pkg:name`
/// where it prints a repository's apparent name.
pub struct Resolver<'a> {
    names: &'a Names,
}

impl<'a> Resolver<'a> {
    pub fn new(names: &'a Names) -> Self {
        Self { names }
    }

    /// The target `label` names.
    pub fn target(&self, label: &str) -> Option<Id> {
        self.labeled(label, target_node_id)
    }

    /// The target `label` names as configured under the configuration whose
    /// checksum is `checksum`.
    pub fn configured_target(&self, label: &str, checksum: &str) -> Option<Id> {
        self.labeled(label, |label| ct_node_id(label, checksum))
    }

    /// The action whose primary output lies at exec path `primary_output`.
    pub fn action(&self, primary_output: &str) -> Option<Id> {
        self.names.get(&action_node_id(primary_output))
    }

    /// The exec path of the primary output of `node`, an action; `None` for
    /// an action naming none and for any other node.
    pub fn primary_output(&self, node: Id) -> Option<&'a str> {
        action_primary_output(self.names.resolve(node))
    }

    /// The node `node_id` forms from `label`, under whichever spelling of the
    /// label [`RepoMapping::canonicalize_label`] gave the graph: an apparent
    /// repository name stands in the graph either as written or canonical,
    /// never both.
    fn labeled(&self, label: &str, node_id: impl Fn(&str) -> String) -> Option<Id> {
        let node = |label: &str| self.names.get(&node_id(label));
        let Some(repo) = label.strip_prefix('@') else {
            return node(label);
        };
        match repo.strip_prefix('@') {
            Some(canonical) if canonical.starts_with("//") => node(canonical),
            Some(_) => node(label),
            None => node(label).or_else(|| node(&format!("@{label}"))),
        }
    }
}

/// The label and node kind a target carries, by its populated variant, or `None`
/// when the variant message is absent.
pub(crate) fn label_and_kind(target: &Target) -> Option<(&str, Kind)> {
    match Discriminator::try_from(target.r#type).ok()? {
        Discriminator::Rule => Some((&target.rule.as_ref()?.name, Kind::Target)),
        Discriminator::SourceFile => Some((&target.source_file.as_ref()?.name, Kind::Source)),
        Discriminator::GeneratedFile => {
            Some((&target.generated_file.as_ref()?.name, Kind::Artifact))
        }
        Discriminator::PackageGroup => {
            Some((&target.package_group.as_ref()?.name, Kind::TargetGroup))
        }
        Discriminator::EnvironmentGroup => {
            Some((&target.environment_group.as_ref()?.name, Kind::TargetGroup))
        }
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
    box_id: HashMap<(String, String), Id>,
    /// The real package paths of each repository, for the source-enclosing lookup.
    repo_paths: HashMap<String, Vec<String>>,
}

/// What a run of box emissions writes to, and the repository the boxes of that
/// run belong to.
struct Emit<'a> {
    graph: &'a mut Graph,
    names: &'a mut Names,
    facets: &'a mut FacetsBuilder,
    /// The repository part the boxes are keyed and labeled by.
    written: &'a str,
    repo: RepoRef,
}

impl Emit<'_> {
    /// Emits one box, which belongs to the repository this run is emitting.
    fn r#box(&mut self, id: Id, label: String, parent: Option<Id>) {
        self.graph
            .add_node(Node::new(id, label, Kind::Package, parent));
        self.facets.facets().set_repo(id, Some(self.repo));
    }
}

impl PackageHierarchy {
    /// Builds the boxes for every real package and the folded ancestors that
    /// reach them, emitting one `package` container node per box into `graph`.
    pub(crate) fn build(
        packages: &BTreeSet<(String, String)>,
        graph: &mut Graph,
        names: &mut Names,
        facets: &mut FacetsBuilder,
    ) -> Self {
        let mut tries: BTreeMap<&str, Trie> = BTreeMap::new();
        for (repo, path) in packages {
            tries.entry(repo.as_str()).or_default().insert(path);
        }
        let mut hierarchy = Self {
            box_id: HashMap::new(),
            repo_paths: HashMap::new(),
        };
        for (repo, trie) in &tries {
            let carried = match identified_repo_name(repo) {
                RepoName::Canonical(canonical) => facets.facets().intern_repo(canonical),
                RepoName::Apparent(apparent) => facets.facets().intern_apparent_repo(apparent),
            };
            let mut out = Emit {
                graph,
                names,
                facets,
                written: repo,
                repo: carried,
            };
            hierarchy.emit_root(trie, &mut out);
        }
        for (repo, path) in packages {
            hierarchy
                .repo_paths
                .entry(repo.clone())
                .or_default()
                .push(path.clone());
        }
        hierarchy
    }

    /// Emits a repository's boxes. The repository root becomes a `//` (or
    /// `@repo//`) box only when it is itself a package; otherwise each top-level
    /// subtree is its own outermost box.
    fn emit_root(&mut self, root: &Trie, out: &mut Emit) {
        let parent = if root.real {
            let id = out.names.intern(&box_node_id(out.written, ""));
            out.r#box(id, repo_prefix(out.written), None);
            self.box_id
                .insert((out.written.to_string(), String::new()), id);
            Some(id)
        } else {
            None
        };
        for (component, child) in &root.children {
            self.emit(
                child,
                component,
                std::slice::from_ref(component),
                parent,
                out,
            );
        }
    }

    fn emit(
        &mut self,
        node: &Trie,
        full_path: &str,
        pending: &[String],
        parent_box: Option<Id>,
        out: &mut Emit,
    ) {
        // Fold a single-child intermediate into its child; a package with
        // content or a branch point anchors its own box.
        if !node.real && node.children.len() == 1 {
            let (component, child) = node.children.iter().next().unwrap();
            let child_path = format!("{full_path}/{component}");
            let mut chain = pending.to_vec();
            chain.push(component.clone());
            self.emit(child, &child_path, &chain, parent_box, out);
            return;
        }

        let label = match parent_box {
            None => format!("{}{}", repo_prefix(out.written), pending.join("/")),
            Some(_) => pending.join("/"),
        };
        let id = out.names.intern(&box_node_id(out.written, full_path));
        out.r#box(id, label, parent_box);
        if node.real {
            self.box_id
                .insert((out.written.to_string(), full_path.to_string()), id);
        }
        for (component, child) in &node.children {
            let child_path = format!("{full_path}/{component}");
            self.emit(
                child,
                &child_path,
                std::slice::from_ref(component),
                Some(id),
                out,
            );
        }
    }

    /// The box that holds a label's own package, or `None` when the label names
    /// no package.
    pub(crate) fn package_box(&self, label: &str) -> Option<Id> {
        let (repo, path) = parse_package(label)?;
        self.box_id
            .get(&(repo.to_string(), path.to_string()))
            .copied()
    }

    /// The path of the deepest real package of repository `repo` enclosing
    /// directory `dir`, or `None` when none does.
    pub(crate) fn enclosing_package(&self, repo: &str, dir: &str) -> Option<&str> {
        self.repo_paths
            .get(repo)?
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

/// Rewrites the boxes that name a repository to name it the way a reader writes
/// it, which only the outermost box of each repository does.
///
/// A box's label is the text the layout measures and the drawing shows, so the
/// name a reader reads stands in the label itself; and it can only be written
/// once every input has been read, since which name a repository is reached by
/// is whatever one of them printed.
pub(crate) fn show_written_repositories(graph: &mut Graph, facets: &Facets) {
    let written = WrittenRepos::of(facets);
    for node in &mut graph.nodes {
        if node.kind != Kind::Package || node.parent.is_some() {
            continue;
        }
        let Some(repo) = facets.repo(node.id) else {
            continue;
        };
        if let Some((_, path)) = node.label.split_once("//") {
            node.label = format!("{}{path}", repo_prefix(written.written(repo)));
        }
    }
}

/// Container node id for a package path, keeping external repos distinct from a
/// same-named main-repo path: main `pkg:a/b`, external `pkg:@repo//a/b`.
fn box_node_id(repo: &str, path: &str) -> String {
    if repo.is_empty() {
        format!("{PACKAGE_PREFIX}{path}")
    } else {
        format!("{PACKAGE_PREFIX}{repo}//{path}")
    }
}

/// What sets a package box's node id apart from the target, configured target,
/// and output artifact that the same package's labels also name.
const PACKAGE_PREFIX: &str = "pkg:";

/// The repository part and package path of the package the box node id
/// `node_id` names, read as [`parse_package`] reads them from a label, or
/// `None` where `node_id` names a node that is no package box.
pub(crate) fn package_node_parts(node_id: &str) -> Option<(&str, &str)> {
    let written = node_id.strip_prefix(PACKAGE_PREFIX)?;
    Some(written.split_once("//").unwrap_or(("", written)))
}

/// Whether package path `pkg` encloses directory `dir`: they are equal, or `dir`
/// lies under `pkg` at a path-segment boundary. The root package `""` encloses
/// every directory.
pub(crate) fn encloses(pkg: &str, dir: &str) -> bool {
    pkg.is_empty()
        || dir == pkg
        || dir
            .strip_prefix(pkg)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(packages: &[(&str, &str)]) -> (Graph, PackageHierarchy, Names) {
        let (graph, hierarchy, names, _) = build_facets(packages);
        (graph, hierarchy, names)
    }

    fn build_facets(
        packages: &[(&str, &str)],
    ) -> (Graph, PackageHierarchy, Names, crate::facets::Facets) {
        let set: BTreeSet<(String, String)> = packages
            .iter()
            .map(|(repo, path)| (repo.to_string(), path.to_string()))
            .collect();
        let mut graph = Graph::new();
        let mut names = Names::default();
        let mut facets = FacetsBuilder::default();
        let hierarchy = PackageHierarchy::build(&set, &mut graph, &mut names, &mut facets);
        (graph, hierarchy, names, facets.seal().0)
    }

    fn node<'a>(graph: &'a Graph, names: &Names, id: &str) -> &'a crate::graph::Node {
        graph
            .nodes
            .iter()
            .find(|n| names.resolve(n.id) == id)
            .unwrap()
    }

    #[test]
    fn an_action_id_holds_its_primary_output_whatever_the_path_starts_with() {
        for path in ["0", "9/out.o", "#1:out.o", ":out.o", "bazel-out/k8/bin/a.o"] {
            let id = action_node_id(path);
            assert_eq!(id, format!("action:{path}"));
            assert_eq!(action_primary_output(&id), Some(path));
            for place in [0, 1, 9] {
                assert_ne!(id, unnamed_action_node_id(place), "{path}");
            }
        }
        assert_ne!(action_node_id("pkg/a.o"), action_node_id("pkg/b.o"));
        assert_eq!(unnamed_action_node_id(7), "action7");
        assert_eq!(action_primary_output("action7"), None);
    }

    #[test]
    fn a_resolver_finds_an_action_and_its_primary_output_from_each_other() {
        let mut names = Names::default();
        let action = names.intern(&action_node_id("bazel-out/k8/bin/a.o"));
        let unnamed = names.intern(&unnamed_action_node_id(0));
        let resolver = Resolver::new(&names);
        assert_eq!(resolver.action("bazel-out/k8/bin/a.o"), Some(action));
        assert_eq!(
            resolver.primary_output(action),
            Some("bazel-out/k8/bin/a.o")
        );
        assert_eq!(resolver.primary_output(unnamed), None);
    }

    #[test]
    fn single_child_chain_folds_into_the_branch_box() {
        let base = "src/main/java/com/google/devtools/common/options";
        let (graph, _, names) = build(&[
            ("", base),
            ("", &format!("{base}/processor")),
            ("", &format!("{base}/testing")),
        ]);

        // The target-less chain down to options folds into one outermost box
        // labeled with the full path; processor and testing nest with only their
        // own component.
        let outer = node(&graph, &names, &format!("pkg:{base}"));
        assert_eq!(outer.label, format!("//{base}"));
        assert_eq!(outer.parent, None);
        assert_eq!(
            node(&graph, &names, &format!("pkg:{base}/processor")).label,
            "processor"
        );
        assert_eq!(
            node(&graph, &names, &format!("pkg:{base}/processor"))
                .parent
                .map(|p| names.resolve(p)),
            Some(format!("pkg:{base}").as_str())
        );
        assert_eq!(
            node(&graph, &names, &format!("pkg:{base}/testing")).label,
            "testing"
        );
        // No box was emitted for the folded intermediate segments.
        assert!(!graph.nodes.iter().any(|n| names.resolve(n.id) == "pkg:src"));
    }

    #[test]
    fn nested_single_child_below_a_branch_folds_too() {
        // foo branches (bar, baz); under bar a single-child chain bar/x/y folds.
        let (graph, _, names) = build(&[("", "foo/bar/x/y"), ("", "foo/baz")]);

        assert_eq!(node(&graph, &names, "pkg:foo").label, "//foo");
        assert_eq!(node(&graph, &names, "pkg:foo/baz").label, "baz");
        let folded = node(&graph, &names, "pkg:foo/bar/x/y");
        assert_eq!(folded.label, "bar/x/y");
        assert_eq!(folded.parent.map(|p| names.resolve(p)), Some("pkg:foo"));
    }

    #[test]
    fn a_package_with_content_anchors_even_with_a_single_child() {
        // foo holds content and has one sub-package foo/bar, so foo keeps its own
        // box rather than folding into bar.
        let (graph, _, names) = build(&[("", "foo"), ("", "foo/bar")]);

        assert_eq!(node(&graph, &names, "pkg:foo").label, "//foo");
        assert_eq!(node(&graph, &names, "pkg:foo/bar").label, "bar");
        assert_eq!(
            node(&graph, &names, "pkg:foo/bar")
                .parent
                .map(|p| names.resolve(p)),
            Some("pkg:foo")
        );
    }

    #[test]
    fn separate_top_level_packages_are_separate_outermost_boxes() {
        let (graph, _, names) = build(&[("", "foo"), ("", "bar")]);

        assert_eq!(node(&graph, &names, "pkg:foo").label, "//foo");
        assert_eq!(node(&graph, &names, "pkg:foo").parent, None);
        assert_eq!(node(&graph, &names, "pkg:bar").label, "//bar");
        assert_eq!(node(&graph, &names, "pkg:bar").parent, None);
    }

    #[test]
    fn root_package_wraps_its_sub_packages() {
        // A `//:x` target makes the root a real package, so it becomes a `//`
        // box that holds the nested sub-package.
        let (graph, _, names) = build(&[("", ""), ("", "a")]);

        assert_eq!(node(&graph, &names, "pkg:").label, "//");
        assert_eq!(node(&graph, &names, "pkg:").parent, None);
        assert_eq!(node(&graph, &names, "pkg:a").label, "a");
        assert_eq!(
            node(&graph, &names, "pkg:a")
                .parent
                .map(|p| names.resolve(p)),
            Some("pkg:")
        );
    }

    #[test]
    fn external_repo_packages_carry_their_repo_prefix() {
        let (graph, hierarchy, names) = build(&[("@r", "a/b"), ("", "a/b")]);

        assert_eq!(node(&graph, &names, "pkg:@r//a/b").label, "@r//a/b");
        assert_eq!(node(&graph, &names, "pkg:a/b").label, "//a/b");
        assert_eq!(hierarchy.package_box("@r//a/b:t"), names.get("pkg:@r//a/b"));
        assert_eq!(hierarchy.package_box("//a/b:t"), names.get("pkg:a/b"));
    }

    /// A rule over repositories reaches a box through this facet alone, so every
    /// box of a repository carries it — the branch points and folded chains that
    /// hold no package of their own as much as the packages themselves.
    #[test]
    fn every_box_of_a_repository_carries_that_repository() {
        let (graph, _, names, facets) =
            build_facets(&[("@@r+", "a/b"), ("@@r+", "a/c"), ("", "a")]);

        let repo_of = |id: &str| {
            let node = names.get(id).expect("a box");
            facets.repo(node).map(|repo| facets.repo_name(repo))
        };
        assert_eq!(repo_of("pkg:@@r+//a"), Some(RepoName::Canonical("r+")));
        assert_eq!(repo_of("pkg:@@r+//a/b"), Some(RepoName::Canonical("r+")));
        assert_eq!(repo_of("pkg:@@r+//a/c"), Some(RepoName::Canonical("r+")));
        assert_eq!(repo_of("pkg:a"), Some(RepoName::Canonical("")));
        assert_eq!(graph.nodes.len(), 4);
    }

    /// Only the box that carries a repository part is rewritten, and only that
    /// part of it: the boxes nested below read their own path component, and a
    /// repository a reader has no other name for keeps its canonical one.
    #[test]
    fn the_box_naming_a_repository_reads_it_the_way_a_reader_writes_it() {
        let (mut graph, _, names, facets) = build_facets(&[
            ("@@rules_foo+", "bar/baz"),
            ("@@rules_foo+", "bar/qux"),
            ("@@rules_foo++ext+gen", "a"),
            ("", "a"),
        ]);

        show_written_repositories(&mut graph, &facets);

        let label = |id: &str| node(&graph, &names, id).label.clone();
        assert_eq!(label("pkg:@@rules_foo+//bar"), "@rules_foo//bar");
        assert_eq!(label("pkg:@@rules_foo+//bar/baz"), "baz");
        assert_eq!(
            label("pkg:@@rules_foo++ext+gen//a"),
            "@@rules_foo++ext+gen//a"
        );
        assert_eq!(label("pkg:a"), "//a");
    }

    /// The boxes of a repository no input named canonically still belong to a
    /// repository, so a rule over repositories reaches them. It is identified by
    /// the apparent name, which is all the input stated.
    #[test]
    fn a_box_of_an_apparent_repository_is_identified_by_the_apparent_name() {
        let (_, _, names, facets) = build_facets(&[("@r", "a")]);

        let repo = facets
            .repo(names.get("pkg:@r//a").expect("a box"))
            .expect("the repository the box belongs to");
        assert_eq!(facets.repo_name(repo), RepoName::Apparent("r"));
        assert_eq!(facets.repos().count(), 1);
    }

    /// Two repositories whose names read alike are one repository only where
    /// the names are of one kind: an apparent name is whatever the module that
    /// wrote it maps it to, which need not be the repository Bazel gave that
    /// canonical name.
    #[test]
    fn an_apparent_repository_is_not_the_canonical_one_reading_the_same() {
        let (_, _, names, facets) = build_facets(&[("@r", "a"), ("@@r", "b")]);

        let repo_of = |id: &str| facets.repo(names.get(id).expect("a box"));
        assert_ne!(repo_of("pkg:@r//a"), repo_of("pkg:@@r//b"));
        assert_eq!(facets.repos().count(), 2);
    }

    #[test]
    fn a_repository_part_states_which_of_its_names_identifies_it() {
        assert_eq!(
            identified_repo_name("@@somerepo+"),
            RepoName::Canonical("somerepo+")
        );
        assert_eq!(identified_repo_name(""), RepoName::Canonical(""));
        assert_eq!(
            identified_repo_name("@somerepo"),
            RepoName::Apparent("somerepo")
        );
    }

    #[test]
    fn enclosing_package_picks_the_deepest_real_package() {
        let (_graph, hierarchy, _names) = build(&[("", "foo"), ("", "foo/bar")]);

        // A source under foo/bar/sub belongs to the deeper foo/bar.
        assert_eq!(
            hierarchy.enclosing_package("", "foo/bar/sub"),
            Some("foo/bar")
        );
        assert_eq!(hierarchy.enclosing_package("", "foo/other"), Some("foo"));
        assert_eq!(hierarchy.enclosing_package("", "elsewhere"), None);
    }

    #[test]
    fn enclosing_package_searches_only_the_named_repository() {
        let (_graph, hierarchy, _names) = build(&[("", "foo"), ("@@r", "bar")]);

        assert_eq!(hierarchy.enclosing_package("@@r", "bar/sub"), Some("bar"));
        assert_eq!(hierarchy.enclosing_package("@@r", "foo/sub"), None);
        assert_eq!(hierarchy.enclosing_package("", "bar/sub"), None);
    }

    /// A `package_group` is a Bazel target, told apart from the group a merge
    /// pass synthesizes.
    #[test]
    fn a_package_group_target_carries_the_group_target_kind() {
        let target = Target {
            r#type: Discriminator::PackageGroup as i32,
            package_group: Some(bazel_query_proto::blaze_query::PackageGroup {
                name: "//foo:visible".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(
            label_and_kind(&target),
            Some(("//foo:visible", Kind::TargetGroup))
        );
    }

    fn mapping(observed: &[&str]) -> RepoMapping {
        RepoMapping::infer(observed.iter().copied())
    }

    #[test]
    fn the_main_repositorys_canonical_form_loses_its_repository_part() {
        let repos = RepoMapping::default();

        assert_eq!(repos.canonicalize_label("@@//foo:app"), "//foo:app");
        assert_eq!(repos.canonicalize_label("//foo:app"), "//foo:app");
    }

    #[test]
    fn an_apparent_name_resolves_to_the_repository_whose_canonical_name_it_is() {
        let repos = mapping(&["somerepo"]);

        assert_eq!(
            repos.canonicalize_label("@somerepo//bar:baz"),
            "@@somerepo//bar:baz"
        );
        assert_eq!(
            repos.canonicalize_label("@@somerepo//bar:baz"),
            "@@somerepo//bar:baz"
        );
    }

    /// A module name is the word the module's own mapping gives its repository,
    /// and any module depending on another under that word gives it the same
    /// one, so a label writing it states which repository it reaches only for
    /// the module that wrote the label.
    #[test]
    fn an_apparent_name_a_module_also_bears_reaches_neither_repository() {
        // `bazel_dep(name = "foo", repo_name = "bar")` makes `@bar` the writing
        // module's word for `foo+`, while the module named `bar` is in the
        // graph under a canonical name of its own.
        let repos = mapping(&["foo+", "bar+"]);

        assert_eq!(repos.canonicalize_label("@bar//pkg:t"), "@bar//pkg:t");
        assert_eq!(repos.canonicalize_label("@@bar+//pkg:t"), "@@bar+//pkg:t");
    }

    #[test]
    fn an_apparent_name_no_repository_bears_stays_unresolved() {
        let repos = mapping(&["rules_foo+"]);

        assert_eq!(
            repos.canonicalize_label("@somerepo//bar:baz"),
            "@somerepo//bar:baz"
        );
    }

    #[test]
    fn canonical_repo_reads_only_a_canonical_repository_part() {
        assert_eq!(canonical_repo("@@somerepo+//foo:bar"), Some("somerepo+"));
        assert_eq!(canonical_repo("@somerepo//foo:bar"), None);
        assert_eq!(canonical_repo("//foo:bar"), None);
        assert_eq!(canonical_repo("@@//foo:bar"), None);
    }

    #[test]
    fn a_box_node_id_reads_back_as_the_package_it_was_written_for() {
        for (repo, path) in [("", ""), ("", "a/b"), ("@r", "a/b"), ("@@r+", "")] {
            assert_eq!(
                package_node_parts(&box_node_id(repo, path)),
                Some((repo, path))
            );
        }
        assert_eq!(package_node_parts("target://foo:app"), None);
    }

    #[test]
    fn target_label_keeps_only_the_name() {
        assert_eq!(target_label("//a/b:c"), ":c");
        assert_eq!(target_label("@r//a:c"), ":c");
        assert_eq!(target_label("//:c"), ":c");
    }
}
