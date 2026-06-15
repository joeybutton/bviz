//! Builds a compound graph from `bazel aquery` output: the
//! target -> configured-target -> {action, output artifact} hierarchy. Edges run
//! between leaf nodes only (action -> its outputs; producing artifact or source
//! -> consuming action), and the layout engine routes them across nesting levels
//! by placing each in the lowest common ancestor of its endpoints.
//!
//! Source artifacts consumed by an action appear as leaf nodes at the root.
//! External sources are dropped unless `external_sources` is set. Generated
//! inputs whose producing action is outside the queried scope have no producer
//! node and are skipped.
//!
//! Bazel groups an action's inputs into nested `DepSetOfFiles` that it shares
//! across actions. A depset referenced by two or more consumers (actions or
//! enclosing depsets) is materialized as a single `depset` node at the root,
//! turning a K-member, M-consumer fan from K*M file->action edges into K member
//! edges in plus M consumer edges out. A depset referenced only once is
//! flattened into its single consumer, since a one-use group node is a useless
//! hop. Empty depsets (no surviving members after the `external_sources` filter)
//! are skipped.

use std::collections::{HashMap, HashSet};

use bazel_query_proto::analysis::{
    Action, ActionGraphContainer, Artifact, Configuration, DepSetOfFiles, PathFragment, Target,
};

use crate::graph::Graph;

pub fn build(containers: &[ActionGraphContainer], external_sources: bool) -> Graph {
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
    let mut target_boxes: HashSet<u32> = HashSet::new();
    let mut ct_boxes: HashSet<String> = HashSet::new();
    let source_nodes: HashSet<u32> = HashSet::new();
    // Per produced artifact, the leaf node that represents it.
    let mut producer_node_id: HashMap<u32, String> = HashMap::new();

    // Pass 1: actions and the output artifacts they produce.
    for (i, action) in actions.iter().enumerate() {
        let ct_id = configured_target_box(
            action,
            &mut graph,
            &mut target_boxes,
            &mut ct_boxes,
            &target_by_id,
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
            let art_id = format!("artifact{out}");
            graph.add_node(
                art_id.clone(),
                basename(out, &artifact_by_id, &fragment_by_id),
                "artifact",
                Some(ct_id.clone()),
            );
            producer_node_id.insert(out, art_id.clone());
            graph.add_edge(action_id.clone(), art_id);
        }
    }

    // Pass 2: wire each action's inputs, materializing shared depsets as nodes.
    let ref_counts = depset_ref_counts(&actions, &dep_sets);
    let mut wiring = Wiring {
        ref_counts,
        live_counts: HashMap::new(),
        depset_nodes: HashSet::new(),
        seen_edge: HashSet::new(),
        source_nodes,
        producer_node_id,
        depset_by_id,
        artifact_by_id,
        fragment_by_id,
        external_sources,
    };
    for (i, action) in actions.iter().enumerate() {
        let action_id = format!("action{i}");
        for &dep_set_id in &action.input_dep_set_ids {
            wiring.wire(dep_set_id, &action_id, &mut graph);
        }
    }

    graph
}

/// Per-depset reference count: how many actions and enclosing depsets name it.
/// A count of 2 or more makes the depset shared and worth its own node.
fn depset_ref_counts(actions: &[&Action], dep_sets: &[&DepSetOfFiles]) -> HashMap<u32, usize> {
    let mut counts: HashMap<u32, usize> = HashMap::new();
    for action in actions {
        for &id in &action.input_dep_set_ids {
            *counts.entry(id).or_default() += 1;
        }
    }
    for dep_set in dep_sets {
        for &id in &dep_set.transitive_dep_set_ids {
            *counts.entry(id).or_default() += 1;
        }
    }
    counts
}

/// Mutable state threaded through the recursive input wiring.
struct Wiring<'a> {
    ref_counts: HashMap<u32, usize>,
    /// Memoized count of distinct transitive members surviving the source filter.
    live_counts: HashMap<u32, usize>,
    /// Shared depset ids already materialized as nodes (members wired once).
    depset_nodes: HashSet<u32>,
    seen_edge: HashSet<(String, String)>,
    source_nodes: HashSet<u32>,
    producer_node_id: HashMap<u32, String>,
    depset_by_id: HashMap<u32, &'a DepSetOfFiles>,
    artifact_by_id: HashMap<u32, &'a Artifact>,
    fragment_by_id: HashMap<u32, &'a PathFragment>,
    external_sources: bool,
}

impl Wiring<'_> {
    /// Connects the inputs reachable from `dep_set_id` to `consumer_id`. A shared,
    /// non-empty depset becomes a node wired to its members once and linked to
    /// each consumer; an unshared one is flattened straight into its consumer.
    fn wire(&mut self, dep_set_id: u32, consumer_id: &str, graph: &mut Graph) {
        if self.live_count(dep_set_id) == 0 {
            return;
        }
        if self.ref_counts.get(&dep_set_id).copied().unwrap_or(0) >= 2 {
            let node_id = format!("depset{dep_set_id}");
            if self.depset_nodes.insert(dep_set_id) {
                let count = self.live_count(dep_set_id);
                graph.add_node(node_id.clone(), files_label(count), "depset", None);
                let dep_set = self.depset_by_id[&dep_set_id];
                let directs: Vec<u32> = dep_set.direct_artifact_ids.clone();
                let transitives: Vec<u32> = dep_set.transitive_dep_set_ids.clone();
                for artifact_id in directs {
                    self.emit_member(artifact_id, &node_id, graph);
                }
                for transitive in transitives {
                    self.wire(transitive, &node_id, graph);
                }
            }
            self.add_edge(&node_id, consumer_id, graph);
        } else if let Some(dep_set) = self.depset_by_id.get(&dep_set_id).copied() {
            let directs: Vec<u32> = dep_set.direct_artifact_ids.clone();
            let transitives: Vec<u32> = dep_set.transitive_dep_set_ids.clone();
            for artifact_id in directs {
                self.emit_member(artifact_id, consumer_id, graph);
            }
            for transitive in transitives {
                self.wire(transitive, consumer_id, graph);
            }
        }
    }

    /// Wires one member artifact to `consumer_id`: a generated input through its
    /// producing artifact node, a source through a deduped source node, applying
    /// the `external_sources` filter and skipping inputs with no producer node.
    fn emit_member(&mut self, artifact_id: u32, consumer_id: &str, graph: &mut Graph) {
        let path = full_path(artifact_id, &self.artifact_by_id, &self.fragment_by_id);
        if !self.external_sources && is_external(&path) {
            return;
        }
        let from = if is_generated(&path) {
            match self.producer_node_id.get(&artifact_id) {
                Some(id) => id.clone(),
                // Produced outside the queried scope: no node to connect from.
                None => return,
            }
        } else {
            source_node(
                artifact_id,
                graph,
                &mut self.source_nodes,
                &self.artifact_by_id,
                &self.fragment_by_id,
            )
        };
        self.add_edge(&from, consumer_id, graph);
    }

    fn add_edge(&mut self, from: &str, to: &str, graph: &mut Graph) {
        if self.seen_edge.insert((from.to_string(), to.to_string())) {
            graph.add_edge(from.to_string(), to.to_string());
        }
    }

    /// Distinct transitive members of `dep_set_id` surviving the source filter,
    /// memoized. Drives both the node label and the empty-depset skip.
    fn live_count(&mut self, dep_set_id: u32) -> usize {
        if let Some(&count) = self.live_counts.get(&dep_set_id) {
            return count;
        }
        let count = self.collect_live(dep_set_id, &mut HashSet::new()).len();
        self.live_counts.insert(dep_set_id, count);
        count
    }

    /// Gathers the distinct surviving member artifact ids reachable from
    /// `dep_set_id`, counting only members `emit_member` would actually wire;
    /// `seen` guards against cycles in the depset graph.
    fn collect_live(&self, dep_set_id: u32, seen: &mut HashSet<u32>) -> HashSet<u32> {
        let mut members: HashSet<u32> = HashSet::new();
        if !seen.insert(dep_set_id) {
            return members;
        }
        let Some(dep_set) = self.depset_by_id.get(&dep_set_id) else {
            return members;
        };
        for &artifact_id in &dep_set.direct_artifact_ids {
            if self.is_wired_member(artifact_id) {
                members.insert(artifact_id);
            }
        }
        for &transitive in &dep_set.transitive_dep_set_ids {
            members.extend(self.collect_live(transitive, seen));
        }
        members
    }

    /// Whether `emit_member` would draw an edge for this member: a source kept by
    /// the external filter, or a generated artifact with a producer node in scope.
    fn is_wired_member(&self, artifact_id: u32) -> bool {
        let path = full_path(artifact_id, &self.artifact_by_id, &self.fragment_by_id);
        if !self.external_sources && is_external(&path) {
            return false;
        }
        if is_generated(&path) {
            return self.producer_node_id.contains_key(&artifact_id);
        }
        true
    }
}

/// Depset node label, matching the count-of-files phrasing (`"1 file"`).
fn files_label(count: usize) -> String {
    if count == 1 {
        "1 file".to_string()
    } else {
        format!("{count} files")
    }
}

fn configured_target_box(
    action: &Action,
    graph: &mut Graph,
    target_boxes: &mut HashSet<u32>,
    ct_boxes: &mut HashSet<String>,
    target_by_id: &HashMap<u32, &Target>,
    config_by_id: &HashMap<u32, &Configuration>,
) -> String {
    let target_id = action.target_id;
    let config_id = action.configuration_id;
    let key = format!("{target_id}#{config_id}");
    let ct_node_id = format!("ct:{key}");
    if !ct_boxes.insert(key) {
        return ct_node_id;
    }

    let target_node_id = format!("target{target_id}");
    if target_boxes.insert(target_id) {
        let label = target_by_id
            .get(&target_id)
            .map(|t| t.label.clone())
            .unwrap_or_else(|| format!("target {target_id}"));
        graph.add_node(target_node_id.clone(), label, "target", None);
    }

    let label = config_by_id
        .get(&config_id)
        .map(|c| c.mnemonic.clone())
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| format!("config {config_id}"));
    graph.add_node(
        ct_node_id.clone(),
        label,
        "configured-target",
        Some(target_node_id),
    );
    ct_node_id
}

fn source_node(
    artifact_id: u32,
    graph: &mut Graph,
    source_nodes: &mut HashSet<u32>,
    artifact_by_id: &HashMap<u32, &Artifact>,
    fragment_by_id: &HashMap<u32, &PathFragment>,
) -> String {
    let node_id = format!("source{artifact_id}");
    if source_nodes.insert(artifact_id) {
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

    fn edges(graph: &Graph) -> HashSet<(&str, &str)> {
        graph
            .edges
            .iter()
            .map(|e| (e.from.as_str(), e.to.as_str()))
            .collect()
    }

    fn depset_nodes(graph: &Graph) -> Vec<&Node> {
        graph.nodes.iter().filter(|n| n.kind == "depset").collect()
    }

    #[test]
    fn shared_depset_becomes_one_node() {
        let (a1, f1) = source_artifact(1, "one.c");
        let (a2, f2) = source_artifact(2, "two.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            dep_set_of_files: vec![dep_set(10, &[1, 2], &[])],
            actions: vec![action(1, "Compile", &[10]), action(2, "Link", &[10])],
            ..Default::default()
        };

        let graph = build(&[container], false);

        let depsets = depset_nodes(&graph);
        assert_eq!(depsets.len(), 1);
        assert_eq!(depsets[0].id, "depset10");
        assert_eq!(depsets[0].label, "2 files");
        assert_eq!(depsets[0].parent, None);

        let edges = edges(&graph);
        assert!(edges.contains(&("source1", "depset10")));
        assert!(edges.contains(&("source2", "depset10")));
        assert!(edges.contains(&("depset10", "action0")));
        assert!(edges.contains(&("depset10", "action1")));
        // No flattened file->action edges bypass the shared node.
        assert!(!edges.contains(&("source1", "action0")));
        assert!(!edges.contains(&("source1", "action1")));
    }

    #[test]
    fn single_use_depset_is_flattened() {
        let (a1, f1) = source_artifact(1, "one.c");
        let (a2, f2) = source_artifact(2, "two.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            dep_set_of_files: vec![dep_set(10, &[1, 2], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let graph = build(&[container], false);

        assert!(depset_nodes(&graph).is_empty());
        let edges = edges(&graph);
        assert!(edges.contains(&("source1", "action0")));
        assert!(edges.contains(&("source2", "action0")));
    }

    #[test]
    fn nested_sharing_produces_depset_to_depset_edges() {
        let (a1, f1) = source_artifact(1, "leaf.c");
        let (a2, f2) = source_artifact(2, "outer.c");
        // Inner depset 20 is shared by the two outer depsets 10 and 11; each
        // outer depset is shared by two actions.
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            dep_set_of_files: vec![
                dep_set(20, &[1], &[]),
                dep_set(10, &[2], &[20]),
                dep_set(11, &[2], &[20]),
            ],
            actions: vec![
                action(1, "A", &[10]),
                action(2, "B", &[10]),
                action(3, "C", &[11]),
                action(4, "D", &[11]),
            ],
            ..Default::default()
        };

        let graph = build(&[container], false);

        let mut ids: Vec<&str> = depset_nodes(&graph).iter().map(|n| n.id.as_str()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["depset10", "depset11", "depset20"]);

        let edges = edges(&graph);
        assert!(edges.contains(&("depset20", "depset10")));
        assert!(edges.contains(&("depset20", "depset11")));
        assert!(edges.contains(&("source1", "depset20")));
    }

    #[test]
    fn external_members_are_dropped_without_empty_depsets() {
        let (a1, f1) = source_artifact(1, "local.c");
        let (a2, f2) = source_artifact(2, "external/repo/dep.c");
        let (a3, f3) = source_artifact(3, "external/repo/only.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2, a3],
            path_fragments: vec![f1, f2, f3],
            dep_set_of_files: vec![
                // Shared, mixed: keeps one member after filtering.
                dep_set(10, &[1, 2], &[]),
                // Shared, all external: empty after filtering, so no node.
                dep_set(11, &[3], &[]),
            ],
            actions: vec![action(1, "A", &[10, 11]), action(2, "B", &[10, 11])],
            ..Default::default()
        };

        let graph = build(&[container], false);

        let depsets = depset_nodes(&graph);
        assert_eq!(depsets.len(), 1);
        assert_eq!(depsets[0].id, "depset10");
        assert_eq!(depsets[0].label, "1 file");

        let edges = edges(&graph);
        assert!(edges.contains(&("source1", "depset10")));
        // The external member never becomes a node or an edge.
        assert!(!graph.nodes.iter().any(|n| n.id == "source2"));
        assert!(!graph.nodes.iter().any(|n| n.id == "source3"));
        assert!(!graph.nodes.iter().any(|n| n.id == "depset11"));
        assert!(!edges.iter().any(|(_, to)| *to == "depset11"));
    }

    #[test]
    fn shared_depset_of_generated_member_with_producer() {
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

        let depsets = depset_nodes(&graph);
        assert_eq!(depsets.len(), 1);
        assert_eq!(depsets[0].id, "depset10");
        assert_eq!(depsets[0].label, "1 file");

        let edges = edges(&graph);
        // The producing action feeds its artifact node, which feeds the depset,
        // which feeds both consumers. Producer is action0; consumers action1/2.
        assert!(edges.contains(&("action0", "artifact1")));
        assert!(edges.contains(&("artifact1", "depset10")));
        assert!(edges.contains(&("depset10", "action1")));
        assert!(edges.contains(&("depset10", "action2")));
    }

    #[test]
    fn shared_depset_of_generated_member_without_producer_is_empty() {
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

        // The depset's only member cannot be wired, so it is treated as empty:
        // no node, no dangling edges.
        assert!(depset_nodes(&graph).is_empty());
        let edges = edges(&graph);
        assert!(!edges.iter().any(|(from, _)| *from == "depset10"));
        assert!(!edges.iter().any(|(_, to)| *to == "depset10"));
    }
}
