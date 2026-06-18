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

use crate::graph::{EdgeKind, Graph};
use crate::label::{self, PackageHierarchy};

pub fn build(containers: &[ActionGraphContainer], external_sources: bool) -> Graph {
    let packages = package_keys(containers);
    build_with_packages(containers, external_sources, &packages, &HashSet::new())
}

/// The (repository, package path) keys of every target this aquery names, for
/// the package hierarchy.
pub(crate) fn package_keys(containers: &[ActionGraphContainer]) -> BTreeSet<(String, String)> {
    containers
        .iter()
        .flat_map(|c| c.targets.iter())
        .filter_map(|t| label::package_key(&t.label))
        .collect()
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
) -> Graph {
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

    let artifact_by_id: HashMap<u32, &Artifact> = artifacts.iter().map(|a| (a.id, *a)).collect();
    let fragment_by_id: HashMap<u32, &PathFragment> =
        path_fragments.iter().map(|p| (p.id, *p)).collect();
    let target_by_id: HashMap<u32, &Target> = targets.iter().map(|t| (t.id, *t)).collect();
    let config_by_id: HashMap<u32, &Configuration> =
        configurations.iter().map(|c| (c.id, *c)).collect();
    let depset_by_id: HashMap<u32, &DepSetOfFiles> = dep_sets.iter().map(|d| (d.id, *d)).collect();

    let mut graph = Graph::new();
    let hierarchy = PackageHierarchy::build(packages, &mut graph);

    let mut target_boxes: HashSet<String> = HashSet::new();
    let mut ct_boxes: HashSet<String> = HashSet::new();
    let source_nodes: HashSet<String> = HashSet::new();
    // Per produced artifact, the leaf node that represents it.
    let mut producer_node_id: HashMap<u32, String> = HashMap::new();

    // Pass 1: actions and the output artifacts they produce.
    for (i, action) in actions.iter().enumerate() {
        let config_id = action.configuration_id;
        let rule_label = target_by_id
            .get(&action.target_id)
            .map(|t| t.label.clone())
            .unwrap_or_else(|| format!("target {}", action.target_id));
        let ct_id = target_ct_box(
            &rule_label,
            "target",
            config_id,
            &mut graph,
            &hierarchy,
            &mut target_boxes,
            &mut ct_boxes,
            &config_by_id,
        );
        let action_id = format!("action{i}");
        graph.add_node(
            action_id.clone(),
            action.mnemonic.clone(),
            "action",
            Some(ct_id.clone()),
        );
        for &out in &action.output_ids {
            let path = full_path(out, &artifact_by_id, &fragment_by_id);
            // A labeled generated output nests under its own target's per-config
            // box; an unlabeled one sits directly under the producing rule's box.
            let art_id = match generated_label(&hierarchy, &path)
                .filter(|label| labeled_outputs.contains(label))
            {
                Some(out_label) => {
                    let out_ct = target_ct_box(
                        &out_label,
                        "artifact",
                        config_id,
                        &mut graph,
                        &hierarchy,
                        &mut target_boxes,
                        &mut ct_boxes,
                        &config_by_id,
                    );
                    let id = format!("artifact:{out_label}#{}", config_key(config_id, &config_by_id));
                    graph.add_node(
                        id.clone(),
                        basename(out, &artifact_by_id, &fragment_by_id),
                        "artifact",
                        Some(out_ct),
                    );
                    id
                }
                None => {
                    let id = format!("artifact{out}");
                    graph.add_node(
                        id.clone(),
                        basename(out, &artifact_by_id, &fragment_by_id),
                        "artifact",
                        Some(ct_id.clone()),
                    );
                    id
                }
            };
            producer_node_id.insert(out, art_id.clone());
            graph.add_edge(action_id.clone(), art_id, EdgeKind::Aquery);
        }
    }

    // Pass 2: wire each action's inputs, flattening its depsets into direct edges.
    let mut wiring = Wiring {
        seen_edge: HashSet::new(),
        id_sym: HashMap::new(),
        id_str: Vec::new(),
        source_nodes,
        hierarchy: &hierarchy,
        producer_node_id,
        depset_by_id,
        artifact_by_id,
        fragment_by_id,
        external_sources,
        resolved: HashMap::new(),
    };
    for (i, action) in actions.iter().enumerate() {
        let action_id = wiring.intern(&format!("action{i}"));
        let mut visited: HashSet<u32> = HashSet::new();
        for &dep_set_id in &action.input_dep_set_ids {
            wiring.wire(dep_set_id, action_id, &mut visited, &mut graph);
        }
    }

    graph
}

/// Mutable state threaded through the recursive input wiring.
struct Wiring<'a> {
    /// Input edges already wired, deduped by their endpoints' interned ids. An
    /// artifact reachable through several of one action's depsets, or the same
    /// source feeding an action twice, is wired once.
    seen_edge: HashSet<(u32, u32)>,
    /// Interned node ids: a node's string id maps to a small integer, so the
    /// per-edge dedup and the resolution cache key on integers and clone the
    /// endpoint strings only when a surviving edge is finally recorded.
    id_sym: HashMap<String, u32>,
    id_str: Vec<String>,
    source_nodes: HashSet<String>,
    hierarchy: &'a PackageHierarchy,
    producer_node_id: HashMap<u32, String>,
    depset_by_id: HashMap<u32, &'a DepSetOfFiles>,
    artifact_by_id: HashMap<u32, &'a Artifact>,
    fragment_by_id: HashMap<u32, &'a PathFragment>,
    external_sources: bool,
    /// The interned leaf node each input artifact wires from, or `None` when it
    /// is dropped. Resolution scans the package set and rebuilds the artifact's
    /// path, so it is cached: an artifact in many actions' inputs resolves once.
    resolved: HashMap<u32, Option<u32>>,
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
        consumer: u32,
        visited: &mut HashSet<u32>,
        graph: &mut Graph,
    ) {
        if !visited.insert(dep_set_id) {
            return;
        }
        let Some(dep_set) = self.depset_by_id.get(&dep_set_id).copied() else {
            return;
        };
        let directs: Vec<u32> = dep_set.direct_artifact_ids.clone();
        let transitives: Vec<u32> = dep_set.transitive_dep_set_ids.clone();
        for artifact_id in directs {
            self.emit_member(artifact_id, consumer, graph);
        }
        for transitive in transitives {
            self.wire(transitive, consumer, visited, graph);
        }
    }

    /// Interns a node id string, returning its small-integer id and recording the
    /// string so a recorded edge can recover its endpoints.
    fn intern(&mut self, id: &str) -> u32 {
        if let Some(&sym) = self.id_sym.get(id) {
            return sym;
        }
        let sym = self.id_str.len() as u32;
        self.id_str.push(id.to_string());
        self.id_sym.insert(id.to_string(), sym);
        sym
    }

    /// Wires one member artifact to `consumer` through the leaf node it resolves
    /// to, creating that node on first sight and skipping a dropped input.
    fn emit_member(&mut self, artifact_id: u32, consumer: u32, graph: &mut Graph) {
        if let Some(from) = self.resolve_member(artifact_id, graph) {
            self.add_edge(from, consumer, graph);
        }
    }

    /// The leaf node `artifact_id` wires from: a generated input through its
    /// producing artifact node, a source through a deduped source node, applying
    /// the `external_sources` filter. `None` for a dropped input (external when
    /// `external_sources` is off, or generated outside the queried scope, which
    /// has no producer node). The result depends only on the artifact, so it is
    /// memoized across the many consumers a shared input feeds.
    fn resolve_member(&mut self, artifact_id: u32, graph: &mut Graph) -> Option<u32> {
        if let Some(&cached) = self.resolved.get(&artifact_id) {
            return cached;
        }
        let from = self.compute_member(artifact_id, graph);
        self.resolved.insert(artifact_id, from);
        from
    }

    fn compute_member(&mut self, artifact_id: u32, graph: &mut Graph) -> Option<u32> {
        let path = full_path(artifact_id, &self.artifact_by_id, &self.fragment_by_id);
        if !self.external_sources && is_external(&path) {
            return None;
        }
        if is_generated(&path) {
            // A generated input with no in-scope producer has no node to connect from.
            let producer = self.producer_node_id.get(&artifact_id).cloned()?;
            return Some(self.intern(&producer));
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
                let node_id = format!("target:{label}");
                if self.source_nodes.insert(node_id.clone()) {
                    let parent = self.hierarchy.package_box(&label).map(str::to_string);
                    graph.add_node(node_id.clone(), label::target_label(&label), "source", parent);
                }
                node_id
            }
            None => root_source_node(
                artifact_id,
                graph,
                &mut self.source_nodes,
                &self.artifact_by_id,
                &self.fragment_by_id,
            ),
        };
        Some(self.intern(&node_id))
    }

    fn add_edge(&mut self, from: u32, to: u32, graph: &mut Graph) {
        if self.seen_edge.insert((from, to)) {
            graph.add_edge(
                self.id_str[from as usize].clone(),
                self.id_str[to as usize].clone(),
                EdgeKind::Aquery,
            );
        }
    }
}

/// The configured-target box `ct:{full_label}#{checksum}` for a target in one
/// configuration, creating the `target:{full_label}` box (of kind `target_kind`)
/// above it on first use and returning the configured-target id. Used both for a
/// rule (a `target` box holding actions) and for a labeled generated output (an
/// `artifact` box holding the concrete file).
#[allow(clippy::too_many_arguments)]
fn target_ct_box(
    full_label: &str,
    target_kind: &str,
    config_id: u32,
    graph: &mut Graph,
    hierarchy: &PackageHierarchy,
    target_boxes: &mut HashSet<String>,
    ct_boxes: &mut HashSet<String>,
    config_by_id: &HashMap<u32, &Configuration>,
) -> String {
    let ct_node_id = format!("ct:{full_label}#{}", config_key(config_id, config_by_id));
    if !ct_boxes.insert(ct_node_id.clone()) {
        return ct_node_id;
    }

    let target_node_id = format!("target:{full_label}");
    if target_boxes.insert(target_node_id.clone()) {
        let package = hierarchy.package_box(full_label).map(str::to_string);
        graph.add_node(
            target_node_id.clone(),
            label::target_label(full_label),
            target_kind,
            package,
        );
    }

    let mnemonic = config_by_id
        .get(&config_id)
        .map(|c| c.mnemonic.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("config {config_id}"));
    graph.add_node(
        ct_node_id.clone(),
        mnemonic,
        "configured-target",
        Some(target_node_id),
    );
    ct_node_id
}

/// The query-independent key of a configuration: its checksum, or `cfg{id}` when
/// none is recorded. The checksum is stable across aquery and cquery dumps.
fn config_key(config_id: u32, config_by_id: &HashMap<u32, &Configuration>) -> String {
    config_by_id
        .get(&config_id)
        .map(|c| c.checksum.clone())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| format!("cfg{config_id}"))
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
    artifact_id: u32,
    graph: &mut Graph,
    source_nodes: &mut HashSet<String>,
    artifact_by_id: &HashMap<u32, &Artifact>,
    fragment_by_id: &HashMap<u32, &PathFragment>,
) -> String {
    let node_id = format!("source{artifact_id}");
    if source_nodes.insert(node_id.clone()) {
        graph.add_node(
            node_id.clone(),
            basename(artifact_id, artifact_by_id, fragment_by_id),
            "source",
            None,
        );
    }
    node_id
}

fn basename(
    artifact_id: u32,
    artifact_by_id: &HashMap<u32, &Artifact>,
    fragment_by_id: &HashMap<u32, &PathFragment>,
) -> String {
    let Some(artifact) = artifact_by_id.get(&artifact_id) else {
        return format!("artifact {artifact_id}");
    };
    fragment_by_id
        .get(&artifact.path_fragment_id)
        .map(|f| f.label.clone())
        .unwrap_or_else(|| format!("artifact {artifact_id}"))
}

/// Reconstructs an artifact's exec-root-relative path by walking path fragments.
fn full_path(
    artifact_id: u32,
    artifact_by_id: &HashMap<u32, &Artifact>,
    fragment_by_id: &HashMap<u32, &PathFragment>,
) -> String {
    let Some(artifact) = artifact_by_id.get(&artifact_id) else {
        return String::new();
    };
    let mut parts: Vec<&str> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut fragment_id = artifact.path_fragment_id;
    while fragment_id != 0 && seen.insert(fragment_id) {
        let Some(fragment) = fragment_by_id.get(&fragment_id) else {
            break;
        };
        parts.push(&fragment.label);
        fragment_id = fragment.parent_id;
    }
    parts.reverse();
    parts.join("/")
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

    fn find<'a>(graph: &'a Graph, id: &str) -> &'a Node {
        graph.nodes.iter().find(|n| n.id == id).unwrap()
    }

    fn parent_of<'a>(graph: &'a Graph, id: &str) -> Option<&'a str> {
        find(graph, id).parent.as_deref()
    }

    fn edges(graph: &Graph) -> HashSet<(&str, &str)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
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

        let graph = build(&[container], false);

        let edges = edges(&graph);
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

        let graph = build(&[container], false);

        let edges = edges(&graph);
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

        let graph = build(&[container], false);

        let edges = edges(&graph);
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

        let graph = build(&[container], false);

        let edges = edges(&graph);
        // The local member flattens onto both consumers.
        assert!(edges.contains(&("source1", "action0")));
        assert!(edges.contains(&("source1", "action1")));
        // The external member never becomes a node or edge.
        assert!(!graph.nodes.iter().any(|n| n.id == "source2"));
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

        let graph = build(&[container], false);

        let edges = edges(&graph);
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

        let graph = build(&[container], false);

        // The member cannot be wired, so it produces no node and no edge.
        assert!(!graph.nodes.iter().any(|n| n.id == "artifact1"));
        assert!(edges(&graph).is_empty());
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

        let graph = build(&[container], false);

        assert_eq!(find(&graph, "pkg:foo").kind, "package");
        assert_eq!(find(&graph, "pkg:foo").label, "//foo");
        assert_eq!(find(&graph, "pkg:bar").label, "//bar");

        assert_eq!(parent_of(&graph, "target://foo:app"), Some("pkg:foo"));
        assert_eq!(parent_of(&graph, "target://bar:lib"), Some("pkg:bar"));
        assert_eq!(parent_of(&graph, "target://foo:main.c"), Some("pkg:foo"));
        assert_eq!(parent_of(&graph, "target://bar:lib.c"), Some("pkg:bar"));
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

        let graph = build(&[container], false);

        assert_eq!(parent_of(&graph, "target://foo/bar:sub/deep.c"), Some("pkg:foo/bar"));
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

        let graph = build(&[container], false);

        assert_eq!(parent_of(&graph, "source1"), None);
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

        let graph = build(&[container], false);

        assert_eq!(find(&graph, "pkg:").label, "//");
        assert_eq!(parent_of(&graph, "target://:root"), Some("pkg:"));
        assert_eq!(parent_of(&graph, "target://:top.c"), Some("pkg:"));
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

        let graph = build(&[container], false);
        let quotiented = crate::quotient::quotient(&graph, &std::collections::HashSet::new());

        // foo/a.c and foo/c.c share package, consumer, and (no) producer: merged.
        let merged: Vec<&Node> = quotiented
            .nodes
            .iter()
            .filter(|n| n.kind == "source")
            .collect();
        let foo_group = merged
            .iter()
            .find(|n| n.parent.as_deref() == Some("pkg:foo"))
            .expect("a foo-package source node");
        assert_eq!(foo_group.label, "2 sources");
        // bar/b.c is alone in its package, so it stays an unmerged singleton.
        let bar_sources: Vec<&&Node> = merged
            .iter()
            .filter(|n| n.parent.as_deref() == Some("pkg:bar"))
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
        let graph = build_with_packages(&containers, false, &package_keys(&containers), &labeled);

        // The output is promoted to its own target box, nesting its per-config
        // box and the concrete artifact, instead of sitting under the rule.
        assert_eq!(find(&graph, "target://pkg:gen.out").kind, "artifact");
        assert_eq!(parent_of(&graph, "ct://pkg:gen.out#ck1"), Some("target://pkg:gen.out"));
        let concrete = find(&graph, "artifact://pkg:gen.out#ck1");
        assert_eq!(concrete.kind, "artifact");
        assert_eq!(concrete.parent.as_deref(), Some("ct://pkg:gen.out#ck1"));
        // The producing action feeds the promoted concrete artifact.
        assert!(edges(&graph).contains(&("action0", "artifact://pkg:gen.out#ck1")));
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
        let graph = build_with_packages(&containers, false, &package_keys(&containers), &labeled);

        // No promotion: the output is a bare artifact under the genrule's box.
        assert!(!graph.nodes.iter().any(|n| n.id == "target://pkg:gen.out"));
        assert_eq!(parent_of(&graph, "artifact1"), Some("ct://pkg:gen#ck1"));
    }

    #[test]
    fn external_target_nests_in_repo_qualified_package() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "@somerepo//foo:bar")],
            actions: vec![action(1, "Compile", &[])],
            ..Default::default()
        };

        let graph = build(&[container], false);

        let pkg = parent_of(&graph, "target:@somerepo//foo:bar").unwrap();
        assert_ne!(pkg, "pkg:foo");
        assert_eq!(find(&graph, pkg).label, "@somerepo//foo");
    }

    #[test]
    fn external_and_main_repo_same_path_keep_separate_packages() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "//foo:x"), target(2, "@somerepo//foo:y")],
            actions: vec![action(1, "Compile", &[]), action(2, "Compile", &[])],
            ..Default::default()
        };

        let graph = build(&[container], false);

        // Main-repo package keeps its historical id and label.
        assert_eq!(parent_of(&graph, "target://foo:x"), Some("pkg:foo"));
        assert_eq!(find(&graph, "pkg:foo").label, "//foo");

        // The external package is a distinct container.
        let external = parent_of(&graph, "target:@somerepo//foo:y").unwrap();
        assert_ne!(external, "pkg:foo");
        assert_eq!(find(&graph, external).label, "@somerepo//foo");
    }
}
