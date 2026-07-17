//! Builds a compound graph from `bazel aquery` output: the
//! package -> target -> configured-target -> {action, output artifact}
//! hierarchy. Package containers nest by path and hold each target's box and
//! source files; content-less single-child package chains fold into one box
//! (see the `label` module). Edges run between leaf nodes only (action -> its outputs;
//! producing artifact or source -> consuming action), and the layout engine
//! routes them across nesting levels by placing each in the lowest common
//! ancestor of its endpoints.
//!
//! Source artifacts consumed by an action nest under the package container of
//! their longest-enclosing package, or sit at the root when no known package
//! encloses them.
//! External sources are dropped unless `external_sources` is set. Generated
//! inputs whose producing action is outside the queried scope have no producer
//! node and are skipped.
//!
//! Bazel groups an action's inputs into nested `DepSetOfFiles` that it shares
//! across actions. These are flattened: every direct artifact and every
//! transitive member is wired straight to the consuming action as a
//! `member -> consumer` edge.

use std::collections::{BTreeSet, HashMap, HashSet};

use bazel_query_proto::analysis::{
    Action, ActionGraphContainer, Artifact, Configuration, DepSetOfFiles, PathFragment, Target,
};

use crate::graph::{EdgeKind, Graph, Kind};
use crate::intern::{Id, Names};
use crate::label::{self, PackageHierarchy};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ArtifactId(u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConfigId(u32);

pub fn build(containers: &[ActionGraphContainer], external_sources: bool) -> (Graph, Names) {
    let packages = package_keys(containers);
    let mut names = Names::default();
    let graph = build_with_packages(
        containers,
        external_sources,
        &packages,
        &HashSet::new(),
        &mut names,
    );
    (graph, names)
}

/// The (repository, package path) keys of every target this aquery names, for
/// the package hierarchy.
pub(crate) fn package_keys(containers: &[ActionGraphContainer]) -> BTreeSet<(String, String)> {
    label::package_keys(
        containers
            .iter()
            .flat_map(|c| c.targets.iter())
            .map(|t| t.label.as_str()),
    )
}

/// Builds the aquery graph attaching its boxes to the `packages` hierarchy,
/// which a joint graph passes as the union of every input's packages so shared
/// package boxes carry one label and parent. `labeled_outputs` holds the labels
/// of generated files that some other input knows as targets; an output artifact
/// whose label is among them is promoted to its own `target -> configured-target`
/// box (the concrete file nesting inside) so it merges with that target, rather
/// than sitting as a bare artifact under its producing rule.
pub(crate) fn build_with_packages(
    containers: &[ActionGraphContainer],
    external_sources: bool,
    packages: &BTreeSet<(String, String)>,
    labeled_outputs: &HashSet<String>,
    names: &mut Names,
) -> Graph {
    let index = Index::build(containers);

    let mut graph = Graph::new();
    let hierarchy = PackageHierarchy::build(packages, &mut graph, names);

    let source_nodes: HashSet<Id> = HashSet::new();
    // Per produced artifact, the leaf node that represents it.
    let mut producer_node_id: HashMap<ArtifactId, Id> = HashMap::new();

    // Pass 1: actions and the output artifacts they produce.
    {
        let mut boxes = Boxes {
            graph: &mut graph,
            names: &mut *names,
            hierarchy: &hierarchy,
            index: &index,
            labeled_outputs,
            targets: HashSet::new(),
            configured: HashSet::new(),
        };
        for (i, action) in index.actions.iter().enumerate() {
            let config_id = ConfigId(action.configuration_id);
            let rule_label = index
                .target_by_id
                .get(&action.target_id)
                .map(|t| t.label.clone())
                .unwrap_or_else(|| format!("target {}", action.target_id));
            let ct_id = boxes.target_ct(&rule_label, Kind::Target, config_id);
            let action_id = boxes.names.intern(&format!("action{i}"));
            boxes
                .graph
                .add_node(action_id, action.mnemonic.clone(), Kind::Action, Some(ct_id));
            for &out in &action.output_ids {
                let out = ArtifactId(out);
                let art_id = boxes.output_artifact(out, ct_id, config_id);
                producer_node_id.insert(out, art_id);
                boxes.graph.add_edge(action_id, art_id, EdgeKind::Aquery);
            }
        }
    }

    // Pass 2: wire each action's inputs, flattening its depsets into direct edges.
    let Index {
        actions,
        paths,
        depset_by_id,
        ..
    } = index;
    let mut wiring = Wiring {
        seen_edge: HashSet::new(),
        source_nodes,
        hierarchy: &hierarchy,
        producer_node_id,
        depset_by_id,
        paths,
        external_sources,
        resolved: HashMap::new(),
    };
    for (i, action) in actions.iter().enumerate() {
        let action_id = names.intern(&format!("action{i}"));
        let mut visited: HashSet<u32> = HashSet::new();
        for &dep_set_id in &action.input_dep_set_ids {
            wiring.wire(dep_set_id, action_id, &mut visited, &mut graph, names);
        }
    }

    graph
}

/// The by-id artifact and path-fragment lookups, paired so an artifact's
/// name and path can be reconstructed from one value.
struct Paths<'a> {
    artifact_by_id: HashMap<ArtifactId, &'a Artifact>,
    fragment_by_id: HashMap<u32, &'a PathFragment>,
}

/// The actions to walk plus the by-id lookups box creation and input wiring
/// resolve against, gathered across every container.
struct Index<'a> {
    actions: Vec<&'a Action>,
    paths: Paths<'a>,
    target_by_id: HashMap<u32, &'a Target>,
    config_by_id: HashMap<ConfigId, &'a Configuration>,
    depset_by_id: HashMap<u32, &'a DepSetOfFiles>,
}

impl<'a> Index<'a> {
    fn build(containers: &'a [ActionGraphContainer]) -> Index<'a> {
        let mut artifacts: Vec<&Artifact> = Vec::new();
        let mut actions: Vec<&Action> = Vec::new();
        let mut targets: Vec<&Target> = Vec::new();
        let mut dep_sets: Vec<&DepSetOfFiles> = Vec::new();
        let mut configurations: Vec<&Configuration> = Vec::new();
        let mut path_fragments: Vec<&PathFragment> = Vec::new();
        for c in containers {
            artifacts.extend(c.artifacts.iter());
            actions.extend(c.actions.iter());
            targets.extend(c.targets.iter());
            dep_sets.extend(c.dep_set_of_files.iter());
            configurations.extend(c.configuration.iter());
            path_fragments.extend(c.path_fragments.iter());
        }
        Index {
            paths: Paths {
                artifact_by_id: artifacts.iter().map(|a| (ArtifactId(a.id), *a)).collect(),
                fragment_by_id: path_fragments.iter().map(|p| (p.id, *p)).collect(),
            },
            target_by_id: targets.iter().map(|t| (t.id, *t)).collect(),
            config_by_id: configurations.iter().map(|c| (ConfigId(c.id), *c)).collect(),
            depset_by_id: dep_sets.iter().map(|d| (d.id, *d)).collect(),
            actions,
        }
    }
}

/// Pass-1 box creation: builds the target, configured-target, and artifact
/// boxes into `graph`, deduping repeats through `targets`/`configured` and
/// resolving names and paths through `index` and `hierarchy`. A generated
/// output whose label is in `labeled_outputs` is promoted to its own box.
struct Boxes<'a> {
    graph: &'a mut Graph,
    names: &'a mut Names,
    hierarchy: &'a PackageHierarchy,
    index: &'a Index<'a>,
    labeled_outputs: &'a HashSet<String>,
    targets: HashSet<Id>,
    configured: HashSet<Id>,
}

impl Boxes<'_> {
    fn config_key(&self, config_id: ConfigId) -> String {
        label::config_key(
            config_id.0,
            self.index.config_by_id.get(&config_id).map(|c| c.checksum.as_str()),
        )
    }

    /// The leaf node for one output artifact `out` of an action whose box is
    /// `producing_ct`. A labeled generated output nests under its own target's
    /// per-config box; an unlabeled one sits directly under the producing rule's box.
    fn output_artifact(&mut self, out: ArtifactId, producing_ct: Id, config_id: ConfigId) -> Id {
        let path = self.index.paths.full_path(out);
        match generated_label(self.hierarchy, &path).filter(|label| self.labeled_outputs.contains(label)) {
            Some(out_label) => {
                let out_ct = self.target_ct(&out_label, Kind::Artifact, config_id);
                let config_key = self.config_key(config_id);
                let id = self.names.intern(&format!("artifact:{out_label}#{config_key}"));
                self.graph
                    .add_node(id, self.index.paths.basename(out), Kind::Artifact, Some(out_ct));
                id
            }
            None => {
                let id = self.names.intern(&format!("artifact{}", out.0));
                self.graph.add_node(
                    id,
                    self.index.paths.basename(out),
                    Kind::Artifact,
                    Some(producing_ct),
                );
                id
            }
        }
    }

    /// The configured-target box `ct:{full_label}#{checksum}` for a target in one
    /// configuration, creating the `target:{full_label}` box (of kind `target_kind`)
    /// above it on first use and returning the configured-target id. Used both for a
    /// rule (a `target` box holding actions) and for a labeled generated output (an
    /// `artifact` box holding the concrete file).
    fn target_ct(&mut self, full_label: &str, target_kind: Kind, config_id: ConfigId) -> Id {
        let config_key = self.config_key(config_id);
        let ct_node_id = self.names.intern(&label::ct_node_id(full_label, &config_key));
        if !self.configured.insert(ct_node_id) {
            return ct_node_id;
        }

        let target_node_id = self.names.intern(&format!("target:{full_label}"));
        if self.targets.insert(target_node_id) {
            let package = self.hierarchy.package_box(full_label);
            self.graph.add_node(
                target_node_id,
                label::target_label(full_label),
                target_kind,
                package,
            );
        }

        let mnemonic = label::config_mnemonic(
            config_id.0,
            self.index.config_by_id.get(&config_id).map(|c| c.mnemonic.as_str()),
        );
        self.graph.add_node(
            ct_node_id,
            mnemonic,
            Kind::ConfiguredTarget,
            Some(target_node_id),
        );
        ct_node_id
    }
}

/// Mutable state threaded through the recursive input wiring.
struct Wiring<'a> {
    /// Input edges already wired, deduped by their endpoints' ids. An artifact
    /// reachable through several of one action's depsets, or the same source
    /// feeding an action twice, is wired once.
    seen_edge: HashSet<(Id, Id)>,
    source_nodes: HashSet<Id>,
    hierarchy: &'a PackageHierarchy,
    producer_node_id: HashMap<ArtifactId, Id>,
    depset_by_id: HashMap<u32, &'a DepSetOfFiles>,
    paths: Paths<'a>,
    external_sources: bool,
    /// The leaf node each input artifact wires from, or `None` when it is
    /// dropped. Resolution scans the package set and rebuilds the artifact's
    /// path, so it is cached: an artifact in many actions' inputs resolves once.
    resolved: HashMap<ArtifactId, Option<Id>>,
}

impl Wiring<'_> {
    /// Flattens the inputs reachable from `dep_set_id` onto `consumer_id`: each
    /// direct artifact is wired straight to the consumer and each transitive
    /// depset is recursed into. `visited` holds the dep-set ids already walked
    /// for this consumer, so a depset reachable by several paths within one
    /// action's input tree is traversed once.
    fn wire(
        &mut self,
        dep_set_id: u32,
        consumer: Id,
        visited: &mut HashSet<u32>,
        graph: &mut Graph,
        names: &mut Names,
    ) {
        if !visited.insert(dep_set_id) {
            return;
        }
        let Some(dep_set) = self.depset_by_id.get(&dep_set_id).copied() else {
            return;
        };
        let directs: Vec<ArtifactId> =
            dep_set.direct_artifact_ids.iter().copied().map(ArtifactId).collect();
        let transitives: Vec<u32> = dep_set.transitive_dep_set_ids.clone();
        for artifact_id in directs {
            self.emit_member(artifact_id, consumer, graph, names);
        }
        for transitive in transitives {
            self.wire(transitive, consumer, visited, graph, names);
        }
    }

    /// Wires one member artifact to `consumer` through the leaf node it resolves
    /// to, creating that node on first sight and skipping a dropped input.
    fn emit_member(&mut self, artifact_id: ArtifactId, consumer: Id, graph: &mut Graph, names: &mut Names) {
        if let Some(from) = self.resolve_member(artifact_id, graph, names) {
            self.add_edge(from, consumer, graph);
        }
    }

    /// The leaf node `artifact_id` wires from: a generated input through its
    /// producing artifact node, a source through a deduped source node, applying
    /// the `external_sources` filter. `None` for a dropped input (external when
    /// `external_sources` is off, or generated outside the queried scope, which
    /// has no producer node). The result depends only on the artifact, so it is
    /// memoized across the many consumers a shared input feeds.
    fn resolve_member(&mut self, artifact_id: ArtifactId, graph: &mut Graph, names: &mut Names) -> Option<Id> {
        if let Some(&cached) = self.resolved.get(&artifact_id) {
            return cached;
        }
        let from = self.compute_member(artifact_id, graph, names);
        self.resolved.insert(artifact_id, from);
        from
    }

    fn compute_member(&mut self, artifact_id: ArtifactId, graph: &mut Graph, names: &mut Names) -> Option<Id> {
        let path = self.paths.full_path(artifact_id);
        if !self.external_sources && is_external(&path) {
            return None;
        }
        if is_generated(&path) {
            // A generated input with no in-scope producer has no node to connect from.
            return self.producer_node_id.get(&artifact_id).copied();
        }
        let dir = path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        let node_id = match self.hierarchy.enclosing_package(dir) {
            Some(pkg) => {
                let rel = if pkg.is_empty() {
                    path.as_str()
                } else {
                    path.strip_prefix(&format!("{pkg}/")).unwrap_or(&path)
                };
                let label = format!("//{pkg}:{rel}");
                let node_id = names.intern(&format!("target:{label}"));
                if self.source_nodes.insert(node_id) {
                    let parent = self.hierarchy.package_box(&label);
                    graph.add_node(node_id, label::target_label(&label), Kind::Source, parent);
                }
                node_id
            }
            None => root_source_node(
                artifact_id,
                graph,
                &mut self.source_nodes,
                &self.paths,
                names,
            ),
        };
        Some(node_id)
    }

    fn add_edge(&mut self, from: Id, to: Id, graph: &mut Graph) {
        if self.seen_edge.insert((from, to)) {
            graph.add_edge(from, to, EdgeKind::Aquery);
        }
    }
}

/// The label of a generated output at exec path `path`: its `bazel-out/<cfg>/bin`
/// prefix stripped, then the rest split into the enclosing package and a
/// package-relative remainder (`bazel-out/k8/bin/src/main/native/jni.h` ->
/// `//src/main/native:jni.h`; a subdir output keeps the subdir in the name).
/// `None` when the path is not a `bin` output or no known package encloses it.
fn generated_label(hierarchy: &PackageHierarchy, path: &str) -> Option<String> {
    let suffix = generated_suffix(path)?;
    let dir = suffix.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let pkg = hierarchy.enclosing_package(dir)?;
    let rel = if pkg.is_empty() {
        suffix
    } else {
        suffix.strip_prefix(&format!("{pkg}/")).unwrap_or(suffix)
    };
    Some(format!("//{pkg}:{rel}"))
}

/// A source artifact that no known package encloses: a root-level node keyed by
/// its artifact id, since it has no package-relative label to identify it.
fn root_source_node(
    artifact_id: ArtifactId,
    graph: &mut Graph,
    source_nodes: &mut HashSet<Id>,
    paths: &Paths,
    names: &mut Names,
) -> Id {
    let node_id = names.intern(&format!("source{}", artifact_id.0));
    if source_nodes.insert(node_id) {
        graph.add_node(
            node_id,
            paths.basename(artifact_id),
            Kind::Source,
            None,
        );
    }
    node_id
}

impl Paths<'_> {
    fn basename(&self, artifact_id: ArtifactId) -> String {
        let Some(artifact) = self.artifact_by_id.get(&artifact_id) else {
            return format!("artifact {}", artifact_id.0);
        };
        self.fragment_by_id
            .get(&artifact.path_fragment_id)
            .map(|f| f.label.clone())
            .unwrap_or_else(|| format!("artifact {}", artifact_id.0))
    }

    /// Reconstructs an artifact's exec-root-relative path by walking path fragments.
    fn full_path(&self, artifact_id: ArtifactId) -> String {
        let Some(artifact) = self.artifact_by_id.get(&artifact_id) else {
            return String::new();
        };
        let mut parts: Vec<&str> = Vec::new();
        let mut seen: HashSet<u32> = HashSet::new();
        let mut fragment_id = artifact.path_fragment_id;
        while fragment_id != 0 && seen.insert(fragment_id) {
            let Some(fragment) = self.fragment_by_id.get(&fragment_id) else {
                break;
            };
            parts.push(&fragment.label);
            fragment_id = fragment.parent_id;
        }
        parts.reverse();
        parts.join("/")
    }
}

/// `bazel-out/<config>/bin/<rest>`: returns `<rest>` for generated paths.
fn generated_suffix(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("bazel-out/")?;
    let (_config, rest) = rest.split_once('/')?;
    rest.strip_prefix("bin/")
}

fn is_generated(path: &str) -> bool {
    generated_suffix(path).is_some()
}

fn is_external(path: &str) -> bool {
    path.starts_with("external/")
        || generated_suffix(path).is_some_and(|rest| rest.starts_with("external/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Node;
    use crate::test_support::{edges, find, parent_of};

    /// A single-fragment source artifact whose path is `<label>`; id `i` gets
    /// path fragment id `i` so artifacts and fragments stay in lockstep.
    fn source_artifact(i: u32, label: &str) -> (Artifact, PathFragment) {
        (
            Artifact {
                id: i,
                path_fragment_id: i,
                is_tree_artifact: false,
            },
            PathFragment {
                id: i,
                label: label.to_string(),
                parent_id: 0,
            },
        )
    }

    /// A generated artifact at `bazel-out/k8-fastbuild/bin/<name>`, built from a
    /// chain of path fragments rooted at fragment id `base` (its four fragments
    /// take ids `base..base+4`); the artifact id is `i`.
    fn generated_artifact(i: u32, base: u32, name: &str) -> (Artifact, Vec<PathFragment>) {
        let frag = |id: u32, label: &str, parent_id: u32| PathFragment {
            id,
            label: label.to_string(),
            parent_id,
        };
        let fragments = vec![
            frag(base, "bazel-out", 0),
            frag(base + 1, "k8-fastbuild", base),
            frag(base + 2, "bin", base + 1),
            frag(base + 3, name, base + 2),
        ];
        (
            Artifact {
                id: i,
                path_fragment_id: base + 3,
                is_tree_artifact: false,
            },
            fragments,
        )
    }

    fn dep_set(id: u32, directs: &[u32], transitives: &[u32]) -> DepSetOfFiles {
        DepSetOfFiles {
            id,
            direct_artifact_ids: directs.to_vec(),
            transitive_dep_set_ids: transitives.to_vec(),
        }
    }

    fn action(target_id: u32, mnemonic: &str, dep_set_ids: &[u32]) -> Action {
        Action {
            target_id,
            mnemonic: mnemonic.to_string(),
            configuration_id: 1,
            input_dep_set_ids: dep_set_ids.to_vec(),
            ..Default::default()
        }
    }

    fn target(id: u32, label: &str) -> Target {
        Target {
            id,
            label: label.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn shared_input_flattens_to_each_consumer() {
        let (a1, f1) = source_artifact(1, "one.c");
        let (a2, f2) = source_artifact(2, "two.c");
        // A depset shared by two actions flattens straight onto each consumer.
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            dep_set_of_files: vec![dep_set(10, &[1, 2], &[])],
            actions: vec![action(1, "Compile", &[10]), action(2, "Link", &[10])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        let edges = edges(&graph, &names);
        assert!(edges.contains(&("source1", "action0")));
        assert!(edges.contains(&("source2", "action0")));
        assert!(edges.contains(&("source1", "action1")));
        assert!(edges.contains(&("source2", "action1")));
    }

    #[test]
    fn nested_transitive_members_flatten_onto_the_consumer() {
        let (a1, f1) = source_artifact(1, "leaf.c");
        // The only member sits in the nested transitive depset 20, so wiring
        // must recurse to reach it.
        let container = ActionGraphContainer {
            artifacts: vec![a1],
            path_fragments: vec![f1],
            dep_set_of_files: vec![dep_set(10, &[], &[20]), dep_set(20, &[1], &[])],
            actions: vec![action(1, "Compile", &[10]), action(2, "Link", &[10])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        let edges = edges(&graph, &names);
        assert!(edges.contains(&("source1", "action0")));
        assert!(edges.contains(&("source1", "action1")));
    }

    #[test]
    fn shared_transitive_depset_is_walked_once_per_consumer() {
        let (a1, f1) = source_artifact(1, "leaf.c");
        // Depset 20 is reachable through both 10 and 11, which the one action
        // names; the per-action visited guard wires its member just once.
        let container = ActionGraphContainer {
            artifacts: vec![a1],
            path_fragments: vec![f1],
            dep_set_of_files: vec![
                dep_set(10, &[], &[20]),
                dep_set(11, &[], &[20]),
                dep_set(20, &[1], &[]),
            ],
            actions: vec![action(1, "Compile", &[10, 11])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        let edges = edges(&graph, &names);
        assert!(edges.contains(&("source1", "action0")));
        // Edge dedup plus the visited guard leave exactly one input edge.
        assert_eq!(edges.iter().filter(|(_, to)| *to == "action0").count(), 1);
    }

    #[test]
    fn external_members_are_dropped() {
        let (a1, f1) = source_artifact(1, "local.c");
        let (a2, f2) = source_artifact(2, "external/repo/dep.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            dep_set_of_files: vec![dep_set(10, &[1, 2], &[])],
            actions: vec![action(1, "A", &[10]), action(2, "B", &[10])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        let edges = edges(&graph, &names);
        // The local member flattens onto both consumers.
        assert!(edges.contains(&("source1", "action0")));
        assert!(edges.contains(&("source1", "action1")));
        // The external member never becomes a node or edge.
        assert!(!graph.nodes.iter().any(|n| names.resolve(n.id) == "source2"));
    }

    #[test]
    fn generated_member_flattens_through_its_producer() {
        let (artifact, gen_frags) = generated_artifact(1, 100, "gen.o");
        let producer = Action {
            target_id: 9,
            mnemonic: "Produce".to_string(),
            configuration_id: 1,
            output_ids: vec![1],
            ..Default::default()
        };
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: gen_frags,
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![
                producer,
                action(1, "Compile", &[10]),
                action(2, "Link", &[10]),
            ],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        let edges = edges(&graph, &names);
        // The producing action feeds its artifact node, which flattens directly
        // onto both consumers. Producer is action0; consumers action1/2.
        assert!(edges.contains(&("action0", "artifact1")));
        assert!(edges.contains(&("artifact1", "action1")));
        assert!(edges.contains(&("artifact1", "action2")));
    }

    #[test]
    fn generated_member_without_producer_is_skipped() {
        let (artifact, gen_frags) = generated_artifact(1, 100, "gen.o");
        // No action produces artifact 1, so it has no producer node in scope.
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: gen_frags,
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![action(1, "Compile", &[10]), action(2, "Link", &[10])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        // The member cannot be wired, so it produces no node and no edge.
        assert!(!graph.nodes.iter().any(|n| names.resolve(n.id) == "artifact1"));
        assert!(edges(&graph, &names).is_empty());
    }

    #[test]
    fn targets_and_sources_nest_in_their_package_containers() {
        let (a1, f1) = source_artifact(1, "foo/main.c");
        let (a2, f2) = source_artifact(2, "bar/lib.c");
        // Each target consumes its own source through a single-use depset.
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            targets: vec![target(1, "//foo:app"), target(2, "//bar:lib")],
            dep_set_of_files: vec![dep_set(10, &[1], &[]), dep_set(11, &[2], &[])],
            actions: vec![action(1, "Compile", &[10]), action(2, "Compile", &[11])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        assert_eq!(find(&graph, &names, "pkg:foo").kind, Kind::Package);
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");
        assert_eq!(find(&graph, &names, "pkg:bar").label, "//bar");

        assert_eq!(parent_of(&graph, &names, "target://foo:app"), Some("pkg:foo"));
        assert_eq!(parent_of(&graph, &names, "target://bar:lib"), Some("pkg:bar"));
        assert_eq!(parent_of(&graph, &names, "target://foo:main.c"), Some("pkg:foo"));
        assert_eq!(parent_of(&graph, &names, "target://bar:lib.c"), Some("pkg:bar"));
    }

    #[test]
    fn source_below_package_dir_nests_in_longest_package() {
        // Both `//foo` and `//foo/bar` are packages; a source in `foo/bar/sub`
        // belongs to the longer, enclosing `//foo/bar`, not `//foo`.
        let (a1, f1) = source_artifact(1, "foo/bar/sub/deep.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1],
            path_fragments: vec![f1],
            targets: vec![target(1, "//foo:app"), target(2, "//foo/bar:lib")],
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            // Action belongs to the inner target so the source feeds it.
            actions: vec![action(2, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        assert_eq!(parent_of(&graph, &names, "target://foo/bar:sub/deep.c"), Some("pkg:foo/bar"));
    }

    #[test]
    fn source_in_no_known_package_stays_at_root() {
        let (a1, f1) = source_artifact(1, "elsewhere/orphan.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1],
            path_fragments: vec![f1],
            targets: vec![target(1, "//foo:app")],
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        assert_eq!(parent_of(&graph, &names, "source1"), None);
    }

    #[test]
    fn root_package_target_uses_empty_package_box() {
        let (a1, f1) = source_artifact(1, "top.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1],
            path_fragments: vec![f1],
            targets: vec![target(1, "//:root")],
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        assert_eq!(find(&graph, &names, "pkg:").label, "//");
        assert_eq!(parent_of(&graph, &names, "target://:root"), Some("pkg:"));
        assert_eq!(parent_of(&graph, &names, "target://:top.c"), Some("pkg:"));
    }

    /// Two sources with the same single consumer but in different packages keep
    /// distinct package parents, so the quotient pass (which groups only nodes
    /// sharing kind, parent, and neighbors) cannot merge them; two in the same
    /// package can.
    #[test]
    fn package_parent_separates_otherwise_mergeable_sources() {
        let (a1, f1) = source_artifact(1, "foo/a.c");
        let (a2, f2) = source_artifact(2, "bar/b.c");
        let (a3, f3) = source_artifact(3, "foo/c.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2, a3],
            path_fragments: vec![f1, f2, f3],
            targets: vec![target(1, "//foo:app"), target(2, "//bar:lib")],
            // One depset feeds one action, so all three are that action's inputs.
            dep_set_of_files: vec![dep_set(10, &[1, 2, 3], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, mut names) = build(&[container], false);
        let quotiented =
            crate::quotient::quotient(&graph, &std::collections::HashSet::new(), &mut names).0;

        // foo/a.c and foo/c.c share package, consumer, and (no) producer: merged.
        let merged: Vec<&Node> = quotiented
            .nodes
            .iter()
            .filter(|n| n.kind == Kind::Source)
            .collect();
        let foo_group = merged
            .iter()
            .find(|n| n.parent.map(|p| names.resolve(p)) == Some("pkg:foo"))
            .expect("a foo-package source node");
        assert_eq!(foo_group.label, "2 sources");
        // bar/b.c is alone in its package, so it stays an unmerged singleton.
        let bar_sources: Vec<&&Node> = merged
            .iter()
            .filter(|n| n.parent.map(|p| names.resolve(p)) == Some("pkg:bar"))
            .collect();
        assert_eq!(bar_sources.len(), 1);
        assert!(!bar_sources[0].label.ends_with("sources"));
    }

    #[test]
    fn labeled_output_is_promoted_to_its_own_target() {
        let (artifact, gen_frags) = generated_artifact(1, 100, "pkg/gen.out");
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: gen_frags,
            targets: vec![target(9, "//pkg:gen")],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8".into(),
                checksum: "ck1".into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 9,
                mnemonic: "Genrule".to_string(),
                configuration_id: 1,
                output_ids: vec![1],
                ..Default::default()
            }],
            ..Default::default()
        };

        let containers = vec![container];
        let labeled = HashSet::from(["//pkg:gen.out".to_string()]);
        let mut names = Names::default();
        let graph =
            build_with_packages(&containers, false, &package_keys(&containers), &labeled, &mut names);

        // The output is promoted to its own target box, nesting its per-config
        // box and the concrete artifact, instead of sitting under the rule.
        assert_eq!(find(&graph, &names, "target://pkg:gen.out").kind, Kind::Artifact);
        assert_eq!(parent_of(&graph, &names, "ct://pkg:gen.out#ck1"), Some("target://pkg:gen.out"));
        let concrete = find(&graph, &names, "artifact://pkg:gen.out#ck1");
        assert_eq!(concrete.kind, Kind::Artifact);
        assert_eq!(concrete.parent.map(|p| names.resolve(p)), Some("ct://pkg:gen.out#ck1"));
        // The producing action feeds the promoted concrete artifact.
        assert!(edges(&graph, &names).contains(&("action0", "artifact://pkg:gen.out#ck1")));
    }

    #[test]
    fn unlabeled_output_stays_under_its_rule() {
        let (artifact, gen_frags) = generated_artifact(1, 100, "pkg/gen.out");
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: gen_frags,
            targets: vec![target(9, "//pkg:gen")],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8".into(),
                checksum: "ck1".into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 9,
                mnemonic: "Genrule".to_string(),
                configuration_id: 1,
                output_ids: vec![1],
                ..Default::default()
            }],
            ..Default::default()
        };

        let containers = vec![container];
        let labeled = HashSet::new();
        let mut names = Names::default();
        let graph =
            build_with_packages(&containers, false, &package_keys(&containers), &labeled, &mut names);

        // No promotion: the output is a bare artifact under the genrule's box.
        assert!(!graph.nodes.iter().any(|n| names.resolve(n.id) == "target://pkg:gen.out"));
        assert_eq!(parent_of(&graph, &names, "artifact1"), Some("ct://pkg:gen#ck1"));
    }

    #[test]
    fn external_target_nests_in_repo_qualified_package() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "@somerepo//foo:bar")],
            actions: vec![action(1, "Compile", &[])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        let pkg = parent_of(&graph, &names, "target:@somerepo//foo:bar").unwrap();
        assert_ne!(pkg, "pkg:foo");
        assert_eq!(find(&graph, &names, pkg).label, "@somerepo//foo");
    }

    #[test]
    fn external_and_main_repo_same_path_keep_separate_packages() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "//foo:x"), target(2, "@somerepo//foo:y")],
            actions: vec![action(1, "Compile", &[]), action(2, "Compile", &[])],
            ..Default::default()
        };

        let (graph, names) = build(&[container], false);

        // Main-repo package keeps its historical id and label.
        assert_eq!(parent_of(&graph, &names, "target://foo:x"), Some("pkg:foo"));
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");

        // The external package is a distinct container.
        let external = parent_of(&graph, &names, "target:@somerepo//foo:y").unwrap();
        assert_ne!(external, "pkg:foo");
        assert_eq!(find(&graph, &names, external).label, "@somerepo//foo");
    }
}
