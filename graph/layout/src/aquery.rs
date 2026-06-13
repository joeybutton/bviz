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
    let mut source_nodes: HashSet<u32> = HashSet::new();
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

    // Pass 2: wire each action's flattened inputs to the action.
    let mut seen_edge: HashSet<(String, String)> = HashSet::new();
    for (i, action) in actions.iter().enumerate() {
        let action_id = format!("action{i}");
        for input in flatten_inputs(&action.input_dep_set_ids, &depset_by_id) {
            let path = full_path(input, &artifact_by_id, &fragment_by_id);
            if !external_sources && is_external(&path) {
                continue;
            }
            let from = if is_generated(&path) {
                match producer_node_id.get(&input) {
                    Some(id) => id.clone(),
                    // Produced outside the queried scope: no node to connect from.
                    None => continue,
                }
            } else {
                source_node(
                    input,
                    &mut graph,
                    &mut source_nodes,
                    &artifact_by_id,
                    &fragment_by_id,
                )
            };
            if seen_edge.insert((from.clone(), action_id.clone())) {
                graph.add_edge(from, action_id.clone());
            }
        }
    }

    graph
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

/// Flattens an action's input dep sets transitively into a sorted list of
/// artifact ids. Sorting keeps edge insertion order deterministic across runs.
fn flatten_inputs(dep_set_ids: &[u32], depset_by_id: &HashMap<u32, &DepSetOfFiles>) -> Vec<u32> {
    let mut artifact_ids: HashSet<u32> = HashSet::new();
    let mut seen: HashSet<u32> = HashSet::new();
    let mut stack: Vec<u32> = dep_set_ids.to_vec();
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(dep_set) = depset_by_id.get(&id) {
            artifact_ids.extend(dep_set.direct_artifact_ids.iter().copied());
            stack.extend(dep_set.transitive_dep_set_ids.iter().copied());
        }
    }
    let mut ids: Vec<u32> = artifact_ids.into_iter().collect();
    ids.sort_unstable();
    ids
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
