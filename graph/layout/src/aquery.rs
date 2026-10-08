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

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{BTreeSet, HashSet};
use std::num::{NonZeroU32, NonZeroUsize};

use rustc_hash::{FxHashMap, FxHashSet};

use bazel_query_proto::analysis::{
    Action, ActionGraphContainer, Artifact, AspectDescriptor, Configuration, DepSetOfFiles,
    PathFragment, RuleClass, Target,
};

use crate::capability::Axis;
use crate::exec_path::{LeftOutInputs, LeftOutInputsBuilder};
#[cfg(test)]
use crate::facets::Facets;
use crate::facets::{ConfigRef, FacetsBuilder};
use crate::graph::{EdgeKind, Graph, Kind, Node};
use crate::intern::{Id, Names};
use crate::label::{self, PackageHierarchy, RepoMapping};
use crate::provenance::Input;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ArtifactId(u32);
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct ConfigId(u32);

#[cfg(test)]
fn build(containers: &[ActionGraphContainer], external_sources: bool) -> (Graph, Names, Facets) {
    build_promoting(containers, external_sources, &HashSet::new())
}

#[cfg(test)]
fn build_promoting(
    containers: &[ActionGraphContainer],
    external_sources: bool,
    labeled_outputs: &HashSet<String>,
) -> (Graph, Names, Facets) {
    let repos = RepoMapping::default();
    let packages = package_keys(containers, &repos);
    let mut names = Names::default();
    let mut facets = FacetsBuilder::default();
    let graph = build_with_packages(
        containers,
        external_sources,
        &packages,
        labeled_outputs,
        &repos,
        &mut names,
        &mut facets,
    );
    (graph, names, facets.seal(Input::Aquery.into()).0)
}

/// Every target label this aquery names.
pub(crate) fn labels(containers: &[ActionGraphContainer]) -> impl Iterator<Item = &str> {
    containers
        .iter()
        .flat_map(|c| c.targets.iter())
        .map(|t| t.label.as_str())
}

/// The (repository, package path) keys of every target this aquery names, for
/// the package hierarchy.
pub(crate) fn package_keys(
    containers: &[ActionGraphContainer],
    repos: &RepoMapping,
) -> BTreeSet<(String, String)> {
    label::package_keys(labels(containers), repos)
}

/// Builds the aquery graph attaching its boxes to the `packages` hierarchy,
/// which a joint graph passes as the union of every input's packages so shared
/// package boxes carry one label and parent. `labeled_outputs` holds the labels
/// of generated files that some other input knows as targets; an output artifact
/// whose label is among them is promoted to its own `target -> configured-target`
/// box (the concrete file nesting inside) so it merges with that target, rather
/// than sitting as a bare artifact under its producing rule.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_with_packages(
    containers: &[ActionGraphContainer],
    external_sources: bool,
    packages: &BTreeSet<(String, String)>,
    labeled_outputs: &HashSet<String>,
    repos: &RepoMapping,
    names: &mut Names,
    facets: &mut FacetsBuilder,
) -> Graph {
    let index = Index::build(containers, names);
    facets.reserve_exec_paths(index.tables.paths.artifact_by_id.len());

    let mut graph = Graph::new();
    let hierarchy = PackageHierarchy::build(packages, &mut graph, names, facets);

    let producer_node_id = build_action_boxes(
        &index,
        &hierarchy,
        labeled_outputs,
        repos,
        &mut graph,
        names,
        facets,
    );
    wire_action_inputs(
        index,
        producer_node_id,
        &hierarchy,
        external_sources,
        &mut graph,
        names,
        facets,
    );

    graph
}

/// Creates the box of the configured target each action belongs to, the action's
/// own node, and a node per artifact it outputs, returning the node that
/// represents each produced artifact.
#[allow(clippy::too_many_arguments)]
fn build_action_boxes(
    index: &Index,
    hierarchy: &PackageHierarchy,
    labeled_outputs: &HashSet<String>,
    repos: &RepoMapping,
    graph: &mut Graph,
    names: &mut Names,
    facets: &mut FacetsBuilder,
) -> FxHashMap<ArtifactId, Id> {
    let outputs = index
        .actions
        .iter()
        .map(|node| node.action.output_ids.len())
        .sum();
    let mut producer_node_id: FxHashMap<ArtifactId, Id> =
        FxHashMap::with_capacity_and_hasher(outputs, Default::default());
    let tables = &index.tables;
    let mut boxes = Boxes {
        graph,
        names,
        facets,
        hierarchy,
        tables,
        labeled_outputs,
        targets: FxHashSet::default(),
        configured: FxHashSet::default(),
    };
    let mut output_nodes: Vec<Id> = Vec::new();
    for node in &index.actions {
        let ActionNode { id, action, .. } = *node;
        let config_id = ConfigId(action.configuration_id);
        let target = tables.target_by_id.get(&action.target_id).copied();
        let written = target.map_or_else(
            || Cow::Owned(format!("target {}", action.target_id)),
            |t| Cow::Borrowed(t.label.as_str()),
        );
        let canonical = repos.canonicalize_label(&written);
        let ct_id = boxes.target_ct(
            &TargetBox {
                canonical: &canonical,
                kind: Kind::Target,
                rule_class: target.and_then(|t| tables.rule_class(t)),
            },
            config_id,
        );
        boxes.graph.add_node(Node::new(
            id,
            action.mnemonic.clone(),
            Kind::Action,
            Some(ct_id),
        ));
        boxes.describe_action(id, ct_id, action);
        output_nodes.clear();
        output_nodes.reserve(action.output_ids.len());
        for &out in &action.output_ids {
            let art_id = boxes.output_artifact(ArtifactId(out), node, ct_id, config_id);
            producer_node_id.insert(ArtifactId(out), art_id);
            boxes.graph.add_edge(id, art_id, EdgeKind::Aquery);
            if out == action.primary_output_id {
                boxes.facets.facets().set_primary_output(id, art_id);
            }
            output_nodes.push(art_id);
        }
        for aliases in &node.sharer_outputs {
            for (&alias, &art_id) in aliases.iter().zip(&output_nodes) {
                // An id some node lists as its own output names that node's
                // artifact, whichever sharer also lists it.
                producer_node_id.entry(ArtifactId(alias)).or_insert(art_id);
            }
        }
    }
    producer_node_id
}

/// Wires each action's inputs onto its node, flattening the action's input
/// depsets into direct edges.
fn wire_action_inputs(
    index: Index,
    producer_node_id: FxHashMap<ArtifactId, Id>,
    hierarchy: &PackageHierarchy,
    external_sources: bool,
    graph: &mut Graph,
    names: &mut Names,
    facets: &mut FacetsBuilder,
) {
    let Index {
        actions,
        tables: Tables { paths, depsets, .. },
    } = index;
    let mut wiring = Wiring {
        source_nodes: FxHashSet::default(),
        hierarchy,
        facets,
        producer_node_id,
        paths,
        external_sources,
        resolved: vec![Resolution::Unresolved; depsets.artifacts.len()],
        walked: vec![None; depsets.len()],
        wired_from: vec![None; names.len()],
        pending: Vec::new(),
        depsets,
    };
    for (i, node) in actions.iter().enumerate() {
        let walk = Walk {
            consumer: node.id,
            stamp: NonZeroU32::try_from(NonZeroUsize::MIN.saturating_add(i))
                .expect("an aquery holds fewer than u32::MAX actions"),
        };
        wiring.wire(&node.action.input_dep_set_ids, walk, graph, names);
    }
    let left_out = wiring.left_out_inputs(&actions);
    wiring.facets.set_left_out_inputs(left_out);
}

/// The by-id artifact and path-fragment lookups, paired so an artifact's
/// name and path can be reconstructed from one value.
struct Paths<'a> {
    artifact_by_id: FxHashMap<ArtifactId, &'a Artifact>,
    fragment_by_id: FxHashMap<u32, &'a PathFragment>,
}

/// An action that becomes a node.
struct ActionNode<'a> {
    id: Id,
    action: &'a Action,
    /// The output ids of each other action the query lists for this one's
    /// primary output. None of those actions has a node; an artifact id one
    /// lists stands for the file `action` lists at the same place.
    sharer_outputs: Vec<&'a [u32]>,
}

impl ActionNode<'_> {
    /// The exec path of the artifact `out`, read off the node's name where
    /// that is formed from it.
    fn output_path<'n>(&self, out: ArtifactId, paths: &Paths, names: &'n Names) -> Cow<'n, str> {
        label::action_primary_output(names.resolve(self.id))
            .filter(|_| out.0 == self.action.primary_output_id)
            .map_or_else(|| Cow::Owned(paths.full_path(out)), Cow::Borrowed)
    }
}

/// The configured target and aspects an action is registered by, which order
/// the actions sharing a primary output. A label and an aspect name read alike
/// in every aquery of one build, and so does the key of a configuration the
/// query describes; the key of one it does not is formed from the query's own
/// id for it.
struct Owner<'a> {
    /// `None` for a target the query does not describe.
    label: Option<&'a str>,
    config_key: String,
    aspects: Vec<&'a str>,
}

impl Owner<'_> {
    /// What owners sort by: the label first, one the query describes ahead of
    /// one it does not, then the configuration key, then the aspect names.
    /// Owners that compare equal need not be one owner, since aspects that
    /// differ only in their parameters share a name.
    fn precedence(&self) -> (bool, Option<&str>, &str, &[&str]) {
        (
            self.label.is_none(),
            self.label,
            &self.config_key,
            &self.aspects,
        )
    }
}

/// The actions that become nodes, with the lookups they were told apart by.
struct Index<'a> {
    actions: Vec<ActionNode<'a>>,
    tables: Tables<'a>,
}

/// The by-id lookups box creation and input wiring resolve against, gathered
/// across every container.
struct Tables<'a> {
    paths: Paths<'a>,
    target_by_id: FxHashMap<u32, &'a Target>,
    config_by_id: FxHashMap<ConfigId, &'a Configuration>,
    depsets: DepSets,
    rule_class_by_id: FxHashMap<u32, &'a RuleClass>,
    aspect_by_id: FxHashMap<u32, &'a AspectDescriptor>,
}

impl<'a> Index<'a> {
    fn build(containers: &'a [ActionGraphContainer], names: &mut Names) -> Index<'a> {
        let mut artifacts: Vec<&Artifact> = Vec::new();
        let mut actions: Vec<&Action> = Vec::new();
        let mut targets: Vec<&Target> = Vec::new();
        let mut dep_sets: Vec<&DepSetOfFiles> = Vec::new();
        let mut configurations: Vec<&Configuration> = Vec::new();
        let mut path_fragments: Vec<&PathFragment> = Vec::new();
        let mut rule_classes: Vec<&RuleClass> = Vec::new();
        let mut aspects: Vec<&AspectDescriptor> = Vec::new();
        for c in containers {
            artifacts.extend(c.artifacts.iter());
            actions.extend(c.actions.iter());
            targets.extend(c.targets.iter());
            dep_sets.extend(c.dep_set_of_files.iter());
            configurations.extend(c.configuration.iter());
            path_fragments.extend(c.path_fragments.iter());
            rule_classes.extend(c.rule_classes.iter());
            aspects.extend(c.aspect_descriptors.iter());
        }
        let tables = Tables {
            paths: Paths {
                artifact_by_id: artifacts.iter().map(|a| (ArtifactId(a.id), *a)).collect(),
                fragment_by_id: path_fragments.iter().map(|p| (p.id, *p)).collect(),
            },
            target_by_id: targets.iter().map(|t| (t.id, *t)).collect(),
            config_by_id: configurations
                .iter()
                .map(|c| (ConfigId(c.id), *c))
                .collect(),
            depsets: DepSets::new(&dep_sets),
            rule_class_by_id: rule_classes.iter().map(|r| (r.id, *r)).collect(),
            aspect_by_id: aspects.iter().map(|a| (a.id, *a)).collect(),
        };
        Index {
            actions: tables.action_nodes(&actions, names),
            tables,
        }
    }
}

impl<'a> Tables<'a> {
    /// The node of each of `actions`, which the query lists in that order: one
    /// per primary output, and one per action naming none.
    ///
    /// Bazel lets several actions write one file only where they are a single
    /// action registered more than once, alike in mnemonic, key, inputs and
    /// outputs (`Actions.canBeShared`); any other two fail analysis as an
    /// action conflict. So the actions naming one primary output make one node,
    /// that of the action whose [`Owner`] sorts first, the earliest listed among
    /// those whose owners compare equal.
    fn action_nodes(&self, actions: &[&'a Action], names: &mut Names) -> Vec<ActionNode<'a>> {
        let mut nodes: Vec<ActionNode> = Vec::with_capacity(actions.len());
        let mut node_of: FxHashMap<Id, usize> =
            FxHashMap::with_capacity_and_hasher(actions.len(), Default::default());
        for (place, &action) in actions.iter().enumerate() {
            let primary_output = self.paths.full_path(ArtifactId(action.primary_output_id));
            if primary_output.is_empty() {
                nodes.push(ActionNode {
                    id: names.intern(&label::unnamed_action_node_id(place)),
                    action,
                    sharer_outputs: Vec::new(),
                });
                continue;
            }
            let id = names.intern(&label::action_node_id(&primary_output));
            match node_of.entry(id) {
                Entry::Vacant(first) => {
                    first.insert(nodes.len());
                    nodes.push(ActionNode {
                        id,
                        action,
                        sharer_outputs: Vec::new(),
                    });
                }
                Entry::Occupied(at) => {
                    let node = &mut nodes[*at.get()];
                    let first =
                        self.owner(action).precedence() < self.owner(node.action).precedence();
                    let sharer = match first {
                        true => std::mem::replace(&mut node.action, action),
                        false => action,
                    };
                    node.sharer_outputs.push(&sharer.output_ids);
                }
            }
        }
        nodes
    }

    fn config_key(&self, config_id: ConfigId) -> String {
        label::config_key(
            config_id.0,
            self.config_by_id
                .get(&config_id)
                .map(|c| c.checksum.as_str()),
        )
    }

    fn owner(&self, action: &Action) -> Owner<'a> {
        Owner {
            label: self
                .target_by_id
                .get(&action.target_id)
                .map(|target| target.label.as_str()),
            config_key: self.config_key(ConfigId(action.configuration_id)),
            aspects: self.aspect_names(action).collect(),
        }
    }

    /// The name of the rule class `target` was declared by, absent for a target
    /// naming none — every interned id is positive, so id `0` names no entry.
    fn rule_class(&self, target: &Target) -> Option<&'a str> {
        Some(
            self.rule_class_by_id
                .get(&target.rule_class_id)?
                .name
                .as_str(),
        )
    }

    /// The names of the aspects that produced `action`, in the order the action
    /// lists their descriptor ids.
    fn aspect_names(&self, action: &Action) -> impl Iterator<Item = &'a str> {
        action
            .aspect_descriptor_ids
            .iter()
            .filter_map(|id| Some(self.aspect_by_id.get(id)?.name.as_str()))
    }
}

/// Every depset of the action graph over dense indices, what it holds and
/// nests resolved to indices once, so walking an action's inputs reads arrays.
struct DepSets {
    /// By id, the index of the depset that id names.
    index_of: FxHashMap<u32, u32>,
    /// By depset, where its entries start in `members` and `nested`; one entry
    /// past the last depset closes both.
    starts: Vec<(u32, u32)>,
    /// The artifacts each depset holds directly, as indices into `artifacts`,
    /// in the order it lists them.
    members: Vec<u32>,
    /// The depsets each depset nests, in the order it lists them. An id naming
    /// no depset holds nothing, and is left out.
    nested: Vec<u32>,
    /// By index, the artifacts depsets hold.
    artifacts: Vec<ArtifactId>,
}

impl DepSets {
    fn new(dep_sets: &[&DepSetOfFiles]) -> Self {
        let index_of: FxHashMap<u32, u32> = dep_sets
            .iter()
            .enumerate()
            .map(|(i, d)| (d.id, i as u32))
            .collect();
        let mut starts = Vec::with_capacity(dep_sets.len() + 1);
        let mut members =
            Vec::with_capacity(dep_sets.iter().map(|d| d.direct_artifact_ids.len()).sum());
        let mut nested = Vec::with_capacity(
            dep_sets
                .iter()
                .map(|d| d.transitive_dep_set_ids.len())
                .sum(),
        );
        let mut artifact_index: FxHashMap<u32, u32> =
            FxHashMap::with_capacity_and_hasher(members.capacity(), Default::default());
        let mut artifacts = Vec::with_capacity(members.capacity());
        for d in dep_sets {
            starts.push((members.len() as u32, nested.len() as u32));
            members.extend(d.direct_artifact_ids.iter().map(|&id| {
                *artifact_index.entry(id).or_insert_with(|| {
                    artifacts.push(ArtifactId(id));
                    artifacts.len() as u32 - 1
                })
            }));
            nested.extend(
                d.transitive_dep_set_ids
                    .iter()
                    .filter_map(|id| index_of.get(id).copied()),
            );
        }
        starts.push((members.len() as u32, nested.len() as u32));
        DepSets {
            index_of,
            starts,
            members,
            nested,
            artifacts,
        }
    }

    fn len(&self) -> usize {
        self.starts.len() - 1
    }

    /// Where the artifacts `depset` holds directly lie in `members`.
    fn members(&self, depset: u32) -> std::ops::Range<usize> {
        let (from, to) = (
            self.starts[depset as usize],
            self.starts[depset as usize + 1],
        );
        from.0 as usize..to.0 as usize
    }

    fn nested(&self, depset: u32) -> &[u32] {
        let (from, to) = (
            self.starts[depset as usize],
            self.starts[depset as usize + 1],
        );
        &self.nested[from.1 as usize..to.1 as usize]
    }
}

/// Mutable state threaded through box creation: the ids of the boxes already
/// added, so a target or configured target that several actions share is
/// created once.
struct Boxes<'a> {
    graph: &'a mut Graph,
    names: &'a mut Names,
    facets: &'a mut FacetsBuilder,
    hierarchy: &'a PackageHierarchy,
    tables: &'a Tables<'a>,
    labeled_outputs: &'a HashSet<String>,
    targets: FxHashSet<Id>,
    configured: FxHashSet<Id>,
}

/// The target a configured-target box belongs to, as one aquery input names it.
struct TargetBox<'a> {
    /// The label in the canonical form node ids are keyed by.
    canonical: &'a str,
    kind: Kind,
    /// The rule class the target was declared by, absent for a target the
    /// action graph names no rule class for, such as a promoted output file.
    rule_class: Option<&'a str>,
}

impl Boxes<'_> {
    fn intern_config(&mut self, config_id: ConfigId) -> Option<ConfigRef> {
        let config = *self.tables.config_by_id.get(&config_id)?;
        self.facets.facets().intern_configuration(config)
    }

    /// Records what `action` states about the node it became, and what the
    /// configured-target box `owner` it runs under states.
    fn describe_action(&mut self, node: Id, owner: Id, action: &Action) {
        self.facets
            .observe(Input::Aquery, Axis::Mnemonic, !action.mnemonic.is_empty());
        if !action.mnemonic.is_empty() {
            let mnemonic = self.facets.facets().intern_mnemonic(&action.mnemonic);
            self.facets.facets().set_mnemonic(node, Some(mnemonic));
        }
        // An action lists the aspects that produced it whether or not any did,
        // so every action carries the field the aspect axis reads.
        self.facets.observe(Input::Aquery, Axis::Aspect, true);
        let aspects: Vec<&str> = self.tables.aspect_names(action).collect();
        self.facets.set_aspects(node, aspects);
        let files = !action.output_ids.is_empty() || !action.input_dep_set_ids.is_empty();
        self.facets.observe(Input::Aquery, Axis::Artifacts, files);
        self.facets.facets().inherit(node, owner);
    }

    /// The leaf node for one output artifact `out` of an action whose box is
    /// `producing_ct`. A labeled generated output nests under its own target's
    /// per-config box; an unlabeled one sits directly under the producing rule's box.
    fn output_artifact(
        &mut self,
        out: ArtifactId,
        producer: &ActionNode,
        producing_ct: Id,
        config_id: ConfigId,
    ) -> Id {
        let path = (producer.output_path(out, &self.tables.paths, self.names)).into_owned();
        let label = generated_label(self.hierarchy, &path)
            .filter(|label| self.labeled_outputs.contains(label));
        let id = match label {
            Some(out_label) => {
                let out_ct = self.target_ct(
                    &TargetBox {
                        canonical: &out_label,
                        kind: Kind::Artifact,
                        rule_class: None,
                    },
                    config_id,
                );
                let config_key = self.tables.config_key(config_id);
                let id = self
                    .names
                    .intern(&format!("artifact:{out_label}#{config_key}"));
                self.graph.add_node(Node::new(
                    id,
                    self.tables.paths.basename(out),
                    Kind::Artifact,
                    Some(out_ct),
                ));
                self.facets.facets().inherit(id, out_ct);
                id
            }
            None => {
                let id = self.names.intern(&format!("artifact{}", out.0));
                self.graph.add_node(Node::new(
                    id,
                    self.tables.paths.basename(out),
                    Kind::Artifact,
                    Some(producing_ct),
                ));
                self.facets.facets().inherit(id, producing_ct);
                id
            }
        };
        self.facets.set_exec_path(id, &path);
        id
    }

    /// The configured-target box `ct:{label}#{checksum}` for `target` in one
    /// configuration, creating the `target:{label}` box above it on first use and
    /// returning the configured-target id. Used both for a rule (a `target` box
    /// holding actions) and for a labeled generated output (an `artifact` box
    /// holding the concrete file).
    fn target_ct(&mut self, target: &TargetBox, config_id: ConfigId) -> Id {
        let config_key = self.tables.config_key(config_id);
        let ct_node_id = self
            .names
            .intern(&label::ct_node_id(target.canonical, &config_key));
        if !self.configured.insert(ct_node_id) {
            return ct_node_id;
        }

        let target_node_id = self.names.intern(&label::target_node_id(target.canonical));
        if self.targets.insert(target_node_id) {
            let package = self.hierarchy.package_box(target.canonical);
            self.graph.add_node(Node::new(
                target_node_id,
                label::target_label(target.canonical),
                target.kind,
                package,
            ));
            self.facets
                .describe_label(Input::Aquery, target_node_id, package, target.canonical);
            if target.kind == Kind::Target {
                // Only a rule is declared by a rule class; the box a promoted
                // output file gets is a file, which no rule class describes.
                let declared = target.rule_class.is_some();
                self.facets
                    .observe(Input::Aquery, Axis::RuleClass, declared);
                self.facets
                    .observe(Input::Aquery, Axis::RuleCategory, declared);
            }
            if let Some(name) = target.rule_class {
                let facets = self.facets.facets();
                let rule_class = facets.intern_rule_class(name);
                facets.set_rule_class(target_node_id, Some(rule_class));
            }
        }

        let mnemonic = label::config_mnemonic(
            config_id.0,
            self.tables
                .config_by_id
                .get(&config_id)
                .map(|c| c.mnemonic.as_str()),
        );
        self.graph.add_node(Node::new(
            ct_node_id,
            mnemonic,
            Kind::ConfiguredTarget,
            Some(target_node_id),
        ));
        let config = self.intern_config(config_id);
        self.facets
            .observe(Input::Aquery, Axis::Configuration, config.is_some());
        if let Some(config) = config {
            self.facets.facets().set_config(ct_node_id, Some(config));
        }
        self.facets.facets().inherit(ct_node_id, target_node_id);
        ct_node_id
    }
}

/// Mutable state threaded through the input wiring.
struct Wiring<'a, 'f> {
    source_nodes: FxHashSet<Id>,
    hierarchy: &'a PackageHierarchy,
    facets: &'f mut FacetsBuilder,
    producer_node_id: FxHashMap<ArtifactId, Id>,
    depsets: DepSets,
    paths: Paths<'a>,
    external_sources: bool,
    /// By depset artifact, what it wires from. Resolution scans the package set
    /// and rebuilds the artifact's path, so it is cached: an artifact in many
    /// actions' inputs resolves once.
    resolved: Vec<Resolution>,
    /// By depset, the stamp of the last walk to enter it.
    walked: Vec<Option<NonZeroU32>>,
    /// By node id, the stamp of the last walk to wire an edge from it. A walk
    /// has one consumer, so this dedupes the walk's edges: an artifact reachable
    /// through several of one action's depsets, or two artifacts resolving to
    /// one node, is wired once.
    wired_from: Vec<Option<NonZeroU32>>,
    /// The depsets the walk in progress has yet to enter, next on top.
    pending: Vec<u32>,
}

/// What a depset artifact wires from.
#[derive(Clone, Copy)]
enum Resolution {
    Unresolved,
    /// Nothing: the artifact is an input the graph leaves out.
    Dropped,
    From(Id),
}

/// One action's input walk.
#[derive(Clone, Copy)]
struct Walk {
    /// The node the action's inputs are wired to.
    consumer: Id,
    /// Marks what this walk has seen, distinct from every other walk's.
    stamp: NonZeroU32,
}

impl Wiring<'_, '_> {
    /// Flattens the inputs reachable from the depsets `inputs` names onto the
    /// walk's consumer: each depset's direct artifacts are wired straight to the
    /// consumer before the depsets it nests are walked, and a depset reachable
    /// by several paths is walked once.
    fn wire(&mut self, inputs: &[u32], walk: Walk, graph: &mut Graph, names: &mut Names) {
        self.pending.clear();
        self.pending.extend(
            inputs
                .iter()
                .rev()
                .filter_map(|id| self.depsets.index_of.get(id).copied()),
        );
        while let Some(depset) = self.pending.pop() {
            if self.walked[depset as usize].replace(walk.stamp) == Some(walk.stamp) {
                continue;
            }
            for member in self.depsets.members(depset) {
                let artifact = self.depsets.members[member];
                if let Some(from) = self.resolve_member(artifact, graph, names) {
                    self.add_edge(from, walk, graph);
                }
            }
            let nested = self.depsets.nested(depset);
            self.pending.extend(nested.iter().rev());
        }
    }

    /// The leaf node depset artifact `artifact` wires from, memoized across the
    /// many consumers a shared input feeds.
    fn resolve_member(
        &mut self,
        artifact: u32,
        graph: &mut Graph,
        names: &mut Names,
    ) -> Option<Id> {
        match self.resolved[artifact as usize] {
            Resolution::From(from) => return Some(from),
            Resolution::Dropped => return None,
            Resolution::Unresolved => {}
        }
        let from = self.compute_member(self.depsets.artifacts[artifact as usize], graph, names);
        self.resolved[artifact as usize] = from.map_or(Resolution::Dropped, Resolution::From);
        from
    }

    /// The leaf node `artifact_id` wires from: a derived input through its
    /// producing artifact node, a source through a deduped source node. `None`
    /// for a dropped input: an external source when `external_sources` is off,
    /// or a derived input produced outside the queried scope, which has no
    /// producer node.
    fn compute_member(
        &mut self,
        artifact_id: ArtifactId,
        graph: &mut Graph,
        names: &mut Names,
    ) -> Option<Id> {
        if let Some(&producer) = self.producer_node_id.get(&artifact_id) {
            return Some(producer);
        }
        let path = self.paths.full_path(artifact_id);
        if is_derived(&path) {
            return None;
        }
        let (repo, repo_path) = match external_repo_path(&path) {
            Some((repo, repo_path)) => {
                if !self.external_sources {
                    return None;
                }
                (repo_label(repo), repo_path)
            }
            None => (String::new(), path.as_str()),
        };
        let node_id = match package_label(self.hierarchy, &repo, repo_path) {
            Some(label) => {
                let node_id = names.intern(&label::target_node_id(&label));
                if self.source_nodes.insert(node_id) {
                    let parent = self.hierarchy.package_box(&label);
                    graph.add_node(Node::new(
                        node_id,
                        label::target_label(&label),
                        Kind::Source,
                        parent,
                    ));
                    self.facets
                        .describe_label(Input::Aquery, node_id, parent, &label);
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
        self.facets.set_exec_path(node_id, &path);
        Some(node_id)
    }

    /// The files `actions` read that the wiring left out, in the nested sets
    /// holding them.
    fn left_out_inputs(&self, actions: &[ActionNode]) -> LeftOutInputs {
        const UNSEEN: u8 = 0;
        const OPEN: u8 = 1;
        const HOLDS_NONE: u8 = 2;
        const HOLDS: u8 = 3;
        let depsets = &self.depsets;
        let left_out = |member: usize| {
            let artifact = depsets.members[member] as usize;
            matches!(self.resolved[artifact], Resolution::Dropped)
        };
        // Each set, nested sets first, by whether it holds a left-out file.
        let mut state: Vec<u8> = vec![UNSEEN; depsets.len()];
        let mut holding: Vec<u32> = Vec::new();
        let mut stack: Vec<(u32, bool)> = Vec::new();
        for root in 0..depsets.len() as u32 {
            stack.push((root, false));
            while let Some((set, closing)) = stack.pop() {
                let at = set as usize;
                if closing {
                    let holds = depsets.members(set).any(left_out)
                        || (depsets.nested(set).iter()).any(|&n| state[n as usize] == HOLDS);
                    state[at] = if holds { HOLDS } else { HOLDS_NONE };
                    if holds {
                        holding.push(set);
                    }
                    continue;
                }
                if state[at] != UNSEEN {
                    continue;
                }
                state[at] = OPEN;
                stack.push((set, true));
                let unseen = depsets
                    .nested(set)
                    .iter()
                    .filter(|&&n| state[n as usize] == UNSEEN);
                stack.extend(unseen.map(|&n| (n, false)));
            }
        }
        let direct: usize = (holding.iter())
            .map(|&set| depsets.members(set).filter(|&m| left_out(m)).count())
            .sum();
        let nested: usize = holding.iter().map(|&set| depsets.nested(set).len()).sum();
        let mut table = LeftOutInputsBuilder::with_capacity(holding.len(), direct, nested, direct);
        let mut handle_of_set: Vec<Option<u32>> = vec![None; depsets.len()];
        let mut handle_of_path: FxHashMap<u32, u32> =
            FxHashMap::with_capacity_and_hasher(direct, Default::default());
        let mut paths: Vec<u32> = Vec::new();
        for &set in &holding {
            paths.clear();
            for member in depsets.members(set).filter(|&m| left_out(m)) {
                let artifact = depsets.members[member];
                let path = *handle_of_path.entry(artifact).or_insert_with(|| {
                    table.push_path(&self.paths.full_path(depsets.artifacts[artifact as usize]))
                });
                paths.push(path);
            }
            let nested = depsets.nested(set).iter();
            let nested = nested.filter_map(|&n| handle_of_set[n as usize]);
            handle_of_set[set as usize] = Some(table.push_set(paths.iter().copied(), nested));
        }
        for node in actions {
            let roots = node.action.input_dep_set_ids.iter();
            let roots = roots.filter_map(|id| depsets.index_of.get(id));
            let mut roots = roots
                .filter_map(|&set| handle_of_set[set as usize])
                .peekable();
            if roots.peek().is_some() {
                table.push_action(node.id, roots);
            }
        }
        table.seal()
    }

    fn add_edge(&mut self, from: Id, walk: Walk, graph: &mut Graph) {
        let at = from.0 as usize;
        if at >= self.wired_from.len() {
            self.wired_from.resize(at + 1, None);
        }
        if self.wired_from[at].replace(walk.stamp) != Some(walk.stamp) {
            graph.add_edge(from, walk.consumer, EdgeKind::Aquery);
        }
    }
}

/// The label of a generated output at exec path `path`, resolved within the
/// repository that owns it (`bazel-out/k8/bin/src/main/native/jni.h` ->
/// `//src/main/native:jni.h`; a subdir output keeps the subdir in the name).
/// `None` when the path is not a `bin` output or no known package encloses it.
fn generated_label(hierarchy: &PackageHierarchy, path: &str) -> Option<String> {
    let (repo, repo_path) = generated_bin_path(path)?;
    package_label(hierarchy, &repo_label(repo), repo_path)
}

/// The `{repo}//{pkg}:{rel}` label of the file at repository-relative path
/// `repo_path` in the repository labelled `repo`, split on the deepest package
/// of that repository enclosing it. `None` when no known package does.
fn package_label(hierarchy: &PackageHierarchy, repo: &str, repo_path: &str) -> Option<String> {
    let dir = repo_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let pkg = hierarchy.enclosing_package(repo, dir)?;
    let rel = repo_path
        .strip_prefix(&format!("{pkg}/"))
        .unwrap_or(repo_path);
    Some(format!("{repo}//{pkg}:{rel}"))
}

/// The repository part of a label naming a file of repository `repo`: `@@<repo>`,
/// or empty for the main repository, whose labels carry no repository part.
fn repo_label(repo: &str) -> String {
    if repo.is_empty() {
        String::new()
    } else {
        format!("@@{repo}")
    }
}

/// A source artifact that no known package encloses: a root-level node keyed by
/// its artifact id, since it has no package-relative label to identify it.
fn root_source_node(
    artifact_id: ArtifactId,
    graph: &mut Graph,
    source_nodes: &mut FxHashSet<Id>,
    paths: &Paths,
    names: &mut Names,
) -> Id {
    let node_id = names.intern(&format!("source{}", artifact_id.0));
    if source_nodes.insert(node_id) {
        graph.add_node(Node::new(
            node_id,
            paths.basename(artifact_id),
            Kind::Source,
            None,
        ));
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
        let mut seen: FxHashSet<u32> = FxHashSet::default();
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

/// The repository and repository-relative path of a `bazel-out/<config>/bin/`
/// output, the repository empty for one of the main repository. An external
/// repository is named either by a segment ahead of the configuration
/// (`bazel-out/<repo>/<config>/bin/<rest>`, the layout
/// `--experimental_sibling_repository_layout` selects) or by an `external/<repo>/`
/// prefix on the path below `bin/`. `None` when `path` is not a `bin` output.
fn generated_bin_path(path: &str) -> Option<(&str, &str)> {
    let derived = path.strip_prefix("bazel-out/")?;
    let (first, after_first) = derived.split_once('/')?;
    let (sibling_repo, suffix) = match after_first.strip_prefix("bin/") {
        Some(suffix) => ("", suffix),
        None => {
            let (_config, after_config) = after_first.split_once('/')?;
            (first, after_config.strip_prefix("bin/")?)
        }
    };
    Some(external_repo_path(suffix).unwrap_or((sibling_repo, suffix)))
}

/// Whether `path` names an output tree, which no source file lives in.
fn is_derived(path: &str) -> bool {
    path.starts_with("bazel-out/")
}

/// The repository and repository-relative remainder of a path inside an external
/// repository: `external/<repo>/<rest>`, or `../<repo>/<rest>` under
/// `--experimental_sibling_repository_layout`.
fn external_repo_path(path: &str) -> Option<(&str, &str)> {
    path.strip_prefix("external/")
        .or_else(|| path.strip_prefix("../"))?
        .split_once('/')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facets::{RuleCategory, Tri};
    use crate::graph::Node;
    use crate::label::Resolver;
    use crate::repo::RepoName;
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

    /// An artifact with id `i` at exec path `path`, built from one path fragment
    /// per component, the fragments taking consecutive ids from `base`.
    fn artifact_at(i: u32, base: u32, path: &str) -> (Artifact, Vec<PathFragment>) {
        let mut fragments = Vec::new();
        let mut parent_id = 0;
        for (n, component) in path.split('/').enumerate() {
            let id = base + n as u32;
            fragments.push(PathFragment {
                id,
                label: component.to_string(),
                parent_id,
            });
            parent_id = id;
        }
        (
            Artifact {
                id: i,
                path_fragment_id: parent_id,
                is_tree_artifact: false,
            },
            fragments,
        )
    }

    /// A generated artifact at `bazel-out/k8-fastbuild/bin/<name>`.
    fn generated_artifact(i: u32, base: u32, name: &str) -> (Artifact, Vec<PathFragment>) {
        artifact_at(i, base, &format!("bazel-out/k8-fastbuild/bin/{name}"))
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

    /// A target declared by the rule class the `rule_classes` table gives
    /// `rule_class_id`.
    fn declared_target(id: u32, label: &str, rule_class_id: u32) -> Target {
        Target {
            rule_class_id,
            ..target(id, label)
        }
    }

    fn rule_class(id: u32, name: &str) -> RuleClass {
        RuleClass {
            id,
            name: name.to_string(),
        }
    }

    fn aspect(id: u32, name: &str) -> AspectDescriptor {
        AspectDescriptor {
            id,
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// An action of `target_id` produced by the aspects `aspects` names, in the
    /// reverse order Bazel writes them.
    fn aspect_action(target_id: u32, mnemonic: &str, aspects: &[u32]) -> Action {
        Action {
            aspect_descriptor_ids: aspects.iter().rev().copied().collect(),
            ..action(target_id, mnemonic, &[])
        }
    }

    /// The aspects `facets` records for `node`, in a stable order.
    fn aspects_of(facets: &Facets, node: Id) -> Vec<&str> {
        let mut aspects: Vec<&str> = facets
            .aspects()
            .values(node)
            .iter()
            .map(|&aspect| facets.strings().resolve(aspect))
            .collect();
        aspects.sort_unstable();
        aspects
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

        let (graph, names, _) = build(&[container], false);

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

        let (graph, names, _) = build(&[container], false);

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

        let (graph, names, _) = build(&[container], false);

        let edges = edges(&graph, &names);
        assert!(edges.contains(&("source1", "action0")));
        // Edge dedup plus the visited guard leave exactly one input edge.
        assert_eq!(edges.iter().filter(|(_, to)| *to == "action0").count(), 1);
    }

    #[test]
    fn external_source_members_are_dropped() {
        let (a1, f1) = source_artifact(1, "local.c");
        let (a2, f2) = source_artifact(2, "external/repo/dep.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            dep_set_of_files: vec![dep_set(10, &[1, 2], &[])],
            actions: vec![action(1, "A", &[10]), action(2, "B", &[10])],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

        let edges = edges(&graph, &names);
        // The local member flattens onto both consumers.
        assert!(edges.contains(&("source1", "action0")));
        assert!(edges.contains(&("source1", "action1")));
        // The external source never becomes a node or edge.
        assert!(!graph.nodes.iter().any(|n| names.resolve(n.id) == "source2"));
    }

    #[test]
    fn external_source_members_carry_their_repository_package() {
        let (a1, f1) = source_artifact(1, "external/somerepo+/foo/dep.c");
        // The sibling repository layout puts the same file beside the exec root.
        let (a2, f2) = source_artifact(2, "../somerepo+/foo/sub/other.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1, a2],
            path_fragments: vec![f1, f2],
            targets: vec![target(1, "@@somerepo+//foo:lib")],
            dep_set_of_files: vec![dep_set(10, &[1, 2], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], true);

        let dep = find(&graph, &names, "target:@@somerepo+//foo:dep.c");
        assert_eq!(dep.kind, Kind::Source);
        assert_eq!(dep.label, ":dep.c");
        let pkg = parent_of(&graph, &names, "target:@@somerepo+//foo:dep.c").unwrap();
        assert_eq!(find(&graph, &names, pkg).label, "@@somerepo+//foo");
        assert_eq!(
            find(&graph, &names, "target:@@somerepo+//foo:sub/other.c").label,
            ":sub/other.c"
        );
        let edges = edges(&graph, &names);
        assert!(edges.contains(&("target:@@somerepo+//foo:dep.c", "action0")));
        assert!(edges.contains(&("target:@@somerepo+//foo:sub/other.c", "action0")));
    }

    #[test]
    fn external_source_in_no_known_package_stays_at_root() {
        let (a1, f1) = source_artifact(1, "external/somerepo+/foo/dep.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1],
            path_fragments: vec![f1],
            targets: vec![target(1, "//app:main")],
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], true);

        assert_eq!(parent_of(&graph, &names, "source1"), None);
    }

    #[test]
    fn an_action_depends_on_the_producer_of_each_output_it_reads() {
        let (header, mut fragments) = generated_artifact(1, 100, "a.h");
        let (object, more) = generated_artifact(2, 200, "a.o");
        fragments.extend(more);
        let (binary, more) = generated_artifact(3, 300, "a");
        fragments.extend(more);
        let (source, fragment) = source_artifact(4, "a.c");
        fragments.push(fragment);
        let writing = |target_id, mnemonic: &str, inputs: &[u32], output: u32| Action {
            output_ids: vec![output],
            primary_output_id: output,
            ..action(target_id, mnemonic, inputs)
        };
        let container = ActionGraphContainer {
            artifacts: vec![header, object, binary, source],
            path_fragments: fragments,
            // The link reads the header too, through the compile's depset.
            dep_set_of_files: vec![dep_set(10, &[1, 4], &[]), dep_set(11, &[2], &[10])],
            actions: vec![
                writing(1, "Genrule", &[], 1),
                writing(1, "CppCompile", &[10], 2),
                writing(1, "CppLink", &[11], 3),
            ],
            targets: vec![target(1, "//a:a")],
            ..Default::default()
        };

        let (h, o, a) = (bin("a.h"), bin("a.o"), bin("a"));
        assert_eq!(
            dependency_outputs(container),
            [(a.clone(), h.clone()), (a, o.clone()), (o, h)],
            "a source read has no producer"
        );
    }

    fn bin(name: &str) -> String {
        format!("bazel-out/k8-fastbuild/bin/{name}")
    }

    /// The primary outputs of each pair of actions of `container` where the
    /// first takes an input from the second, sorted.
    fn dependency_outputs(container: ActionGraphContainer) -> Vec<(String, String)> {
        let joint = crate::build_joint(Some(&[container]), None, None, false);
        let resolver = Resolver::new(&joint.names);
        let found = crate::action_dependencies(&joint.graph);
        let out = |position: usize| {
            resolver
                .primary_output(found.actions[position])
                .expect("an action")
                .to_owned()
        };
        let mut pairs: Vec<(String, String)> = found
            .dependencies
            .iter()
            .map(|&(dependent, dependency)| (out(dependent), out(dependency)))
            .collect();
        pairs.sort_unstable();
        pairs
    }

    #[test]
    fn an_action_depends_on_the_producer_of_an_output_other_than_its_primary() {
        let (header, mut fragments) = generated_artifact(1, 100, "a.h");
        let (deps, more) = generated_artifact(2, 200, "a.d");
        fragments.extend(more);
        let (object, more) = generated_artifact(3, 300, "a.o");
        fragments.extend(more);
        let container = ActionGraphContainer {
            artifacts: vec![header, deps, object],
            path_fragments: fragments,
            dep_set_of_files: vec![dep_set(10, &[2], &[])],
            actions: vec![
                Action {
                    output_ids: vec![1, 2],
                    primary_output_id: 1,
                    ..action(1, "Genrule", &[])
                },
                Action {
                    output_ids: vec![3],
                    primary_output_id: 3,
                    ..action(1, "CppCompile", &[10])
                },
            ],
            targets: vec![target(1, "//a:a")],
            ..Default::default()
        };
        assert_eq!(dependency_outputs(container), [(bin("a.o"), bin("a.h"))]);
    }

    /// Bazel shares an action between the targets that declare it alike, so
    /// every sharer reads the same inputs.
    #[test]
    fn actions_sharing_a_primary_output_stand_as_one() {
        let (header, mut fragments) = generated_artifact(1, 100, "a.h");
        let (object, more) = generated_artifact(2, 200, "a.o");
        fragments.extend(more);
        let writing = |target_id, mnemonic: &str, inputs: &[u32], output: u32| Action {
            output_ids: vec![output],
            primary_output_id: output,
            ..action(target_id, mnemonic, inputs)
        };
        let container = ActionGraphContainer {
            artifacts: vec![header, object],
            path_fragments: fragments,
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![
                writing(1, "Genrule", &[], 1),
                writing(1, "CppCompile", &[10], 2),
                writing(2, "CppCompile", &[10], 2),
            ],
            targets: vec![target(1, "//a:a"), target(2, "//a:b")],
            ..Default::default()
        };
        let joint = crate::build_joint(Some(std::slice::from_ref(&container)), None, None, false);
        assert_eq!(crate::action_dependencies(&joint.graph).actions.len(), 2);
        assert_eq!(dependency_outputs(container), [(bin("a.o"), bin("a.h"))]);
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

        let (graph, names, _) = build(&[container], false);

        let edges = edges(&graph, &names);
        // The producing action feeds its artifact node, which flattens directly
        // onto both consumers. Producer is action0; consumers action1/2.
        assert!(edges.contains(&("action0", "artifact1")));
        assert!(edges.contains(&("artifact1", "action1")));
        assert!(edges.contains(&("artifact1", "action2")));
    }

    #[test]
    fn external_generated_member_flattens_through_its_producer() {
        let (artifact, gen_frags) = generated_artifact(1, 100, "external/repo/lib.so");
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
            actions: vec![producer, action(1, "Link", &[10])],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

        let edges = edges(&graph, &names);
        // An output of an external repository's action is in scope through its
        // producer, so the `external_sources` filter leaves it wired. Producer is
        // action0; the consumer action1.
        assert!(edges.contains(&("action0", "artifact1")));
        assert!(edges.contains(&("artifact1", "action1")));
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

        let (graph, names, _) = build(&[container], false);

        // The member cannot be wired, so it produces no node and no edge.
        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| names.resolve(n.id) == "artifact1")
        );
        assert!(edges(&graph, &names).is_empty());
    }

    #[test]
    fn derived_member_outside_the_bin_tree_flattens_through_its_producer() {
        let (artifact, frags) = artifact_at(1, 100, "bazel-out/k8-fastbuild/genfiles/gen.h");
        let producer = Action {
            target_id: 9,
            mnemonic: "Produce".to_string(),
            configuration_id: 1,
            output_ids: vec![1],
            ..Default::default()
        };
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: frags,
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![producer, action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

        let edges = edges(&graph, &names);
        assert!(edges.contains(&("action0", "artifact1")));
        assert!(edges.contains(&("artifact1", "action1")));
        assert!(!graph.nodes.iter().any(|n| n.kind == Kind::Source));
    }

    #[test]
    fn derived_member_outside_the_bin_tree_without_producer_is_skipped() {
        let (artifact, frags) = artifact_at(1, 100, "bazel-out/stable-status.txt");
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: frags,
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

        assert!(!graph.nodes.iter().any(|n| n.kind == Kind::Source));
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

        let (graph, names, _) = build(&[container], false);

        assert_eq!(find(&graph, &names, "pkg:foo").kind, Kind::Package);
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");
        assert_eq!(find(&graph, &names, "pkg:bar").label, "//bar");

        assert_eq!(
            parent_of(&graph, &names, "target://foo:app"),
            Some("pkg:foo")
        );
        assert_eq!(
            parent_of(&graph, &names, "target://bar:lib"),
            Some("pkg:bar")
        );
        assert_eq!(
            parent_of(&graph, &names, "target://foo:main.c"),
            Some("pkg:foo")
        );
        assert_eq!(
            parent_of(&graph, &names, "target://bar:lib.c"),
            Some("pkg:bar")
        );
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

        let (graph, names, _) = build(&[container], false);

        assert_eq!(
            parent_of(&graph, &names, "target://foo/bar:sub/deep.c"),
            Some("pkg:foo/bar")
        );
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

        let (graph, names, _) = build(&[container], false);

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

        let (graph, names, _) = build(&[container], false);

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

        let (graph, mut names, _) = build(&[container], false);
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

        let labeled = HashSet::from(["//pkg:gen.out".to_string()]);
        let (graph, names, _) = build_promoting(&[container], false, &labeled);

        // The output is promoted to its own target box, nesting its per-config
        // box and the concrete artifact, instead of sitting under the rule.
        assert_eq!(
            find(&graph, &names, "target://pkg:gen.out").kind,
            Kind::Artifact
        );
        assert_eq!(
            parent_of(&graph, &names, "ct://pkg:gen.out#ck1"),
            Some("target://pkg:gen.out")
        );
        let concrete = find(&graph, &names, "artifact://pkg:gen.out#ck1");
        assert_eq!(concrete.kind, Kind::Artifact);
        assert_eq!(
            concrete.parent.map(|p| names.resolve(p)),
            Some("ct://pkg:gen.out#ck1")
        );
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

        let (graph, names, _) = build(&[container], false);

        // No promotion: the output is a bare artifact under the genrule's box.
        assert!(
            !graph
                .nodes
                .iter()
                .any(|n| names.resolve(n.id) == "target://pkg:gen.out")
        );
        assert_eq!(
            parent_of(&graph, &names, "artifact1"),
            Some("ct://pkg:gen#ck1")
        );
    }

    #[test]
    fn an_action_names_its_primary_output_among_those_it_writes() {
        let (first, mut frags) = generated_artifact(1, 100, "pkg/gen.h");
        let (second, more) = generated_artifact(2, 200, "pkg/gen.c");
        frags.extend(more);
        let container = ActionGraphContainer {
            artifacts: vec![first, second],
            path_fragments: frags,
            targets: vec![target(9, "//pkg:gen")],
            actions: vec![Action {
                target_id: 9,
                mnemonic: "Genrule".to_string(),
                configuration_id: 1,
                output_ids: vec![1, 2],
                primary_output_id: 2,
                ..Default::default()
            }],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let action = names.get("action:bazel-out/k8-fastbuild/bin/pkg/gen.c");
        let primary = facets.primary_output(action.unwrap());
        assert_eq!(primary.map(|id| names.resolve(id)), Some("artifact2"));
    }

    #[test]
    fn each_file_an_action_reads_or_writes_carries_its_exec_path() {
        let (source, source_fragment) = source_artifact(1, "in.c");
        let (first, mut frags) = generated_artifact(2, 100, "pkg/gen.h");
        let (second, more) = generated_artifact(3, 200, "pkg/gen.c");
        frags.extend(more);
        frags.push(source_fragment);
        let container = ActionGraphContainer {
            artifacts: vec![source, first, second],
            path_fragments: frags,
            targets: vec![target(9, "//pkg:gen")],
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![Action {
                output_ids: vec![2, 3],
                primary_output_id: 3,
                ..action(9, "Genrule", &[10])
            }],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let path = |name: &str| {
            let path = facets.exec_path(names.get(name).unwrap());
            path.map(|path| path.to_string())
        };
        assert_eq!(path("source1").as_deref(), Some("in.c"));
        assert_eq!(
            path("artifact2").as_deref(),
            Some("bazel-out/k8-fastbuild/bin/pkg/gen.h")
        );
        assert_eq!(
            path("artifact3").as_deref(),
            Some("bazel-out/k8-fastbuild/bin/pkg/gen.c")
        );
        assert_eq!(path("action:bazel-out/k8-fastbuild/bin/pkg/gen.c"), None);
    }

    #[test]
    fn the_inputs_left_out_are_kept_in_the_sets_holding_them() {
        let (kept, kept_fragment) = source_artifact(1, "pkg/in.c");
        let (external, external_fragment) = source_artifact(2, "external/repo+/lib.h");
        let (orphan, mut frags) = generated_artifact(3, 100, "other/gen.h");
        frags.extend([kept_fragment, external_fragment]);
        let container = ActionGraphContainer {
            artifacts: vec![kept, external, orphan],
            path_fragments: frags,
            targets: vec![target(9, "//pkg:a"), target(8, "//pkg:b")],
            // Set 10 nests set 11, which holds the external header; set 12
            // holds only a file the graph keeps.
            dep_set_of_files: vec![
                dep_set(10, &[3], &[11]),
                dep_set(11, &[2], &[]),
                dep_set(12, &[1], &[]),
            ],
            actions: vec![action(9, "Compile", &[10, 12]), action(8, "Link", &[12])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let left_out = facets.left_out_inputs();
        let reads = |action: &str| {
            let mut paths: Vec<String> = Vec::new();
            let mut open: Vec<u32> = left_out.read_by(names.get(action).unwrap()).to_vec();
            while let Some(set) = open.pop() {
                let direct = left_out.direct(set).iter();
                paths.extend(direct.map(|&path| left_out.path(path).to_string()));
                open.extend_from_slice(left_out.nested(set));
            }
            paths.sort();
            paths
        };
        assert_eq!(
            reads("action0"),
            [
                "bazel-out/k8-fastbuild/bin/other/gen.h",
                "external/repo+/lib.h"
            ]
        );
        assert!(reads("action1").is_empty());
        assert_eq!(
            left_out.sets(),
            2,
            "a set holding nothing left out is not kept"
        );
    }

    /// An action of `target_id` whose one output, and so its primary one, is
    /// artifact `out`.
    fn writing(target_id: u32, out: u32) -> Action {
        Action {
            output_ids: vec![out],
            primary_output_id: out,
            ..action(target_id, "Genrule", &[])
        }
    }

    const GEN_H: &str = "action:bazel-out/k8-fastbuild/bin/pkg/gen.h";
    const GEN_C: &str = "action:bazel-out/k8-fastbuild/bin/pkg/gen.c";

    /// The names of the action nodes of `graph`, sorted.
    fn action_names<'a>(graph: &Graph, names: &'a Names) -> Vec<&'a str> {
        let mut actions: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == Kind::Action)
            .map(|node| names.resolve(node.id))
            .collect();
        actions.sort_unstable();
        actions
    }

    /// The names of the artifact nodes of `graph`, sorted.
    fn artifact_names<'a>(graph: &Graph, names: &'a Names) -> Vec<&'a str> {
        let mut artifacts: Vec<&str> = graph
            .nodes
            .iter()
            .filter(|node| node.kind == Kind::Artifact)
            .map(|node| names.resolve(node.id))
            .collect();
        artifacts.sort_unstable();
        artifacts
    }

    /// Every edge of `graph`, one entry per edge, sorted.
    fn edge_list<'a>(graph: &Graph, names: &'a Names) -> Vec<(&'a str, &'a str)> {
        let mut edges: Vec<(&str, &str)> = graph
            .edges
            .iter()
            .map(|edge| (names.resolve(edge.from), names.resolve(edge.to)))
            .collect();
        edges.sort_unstable();
        edges
    }

    #[test]
    fn an_action_is_named_by_its_primary_output_not_its_place() {
        let (header, mut frags) = generated_artifact(1, 100, "pkg/gen.h");
        let (source, more) = generated_artifact(2, 200, "pkg/gen.c");
        frags.extend(more);
        let writers = |actions: Vec<Action>| {
            let container = ActionGraphContainer {
                artifacts: vec![header, source],
                path_fragments: frags.clone(),
                targets: vec![target(9, "//pkg:gen")],
                actions,
                ..Default::default()
            };
            let (graph, names, _) = build(&[container], false);
            edge_list(&graph, &names)
                .into_iter()
                .map(|(from, to)| (from.to_string(), to.to_string()))
                .collect::<Vec<_>>()
        };

        let header_first = writers(vec![writing(9, 1), writing(9, 2)]);
        let header_second = writers(vec![writing(9, 2), writing(9, 1)]);

        assert_eq!(
            header_first,
            [
                (GEN_C.to_string(), "artifact2".to_string()),
                (GEN_H.to_string(), "artifact1".to_string()),
            ]
        );
        assert_eq!(header_first, header_second);
    }

    #[test]
    fn actions_sharing_a_primary_output_are_one_node_under_the_owner_sorting_first() {
        let (shared, frags) = generated_artifact(1, 100, "pkg/gen.h");
        for actions in [
            vec![writing(8, 1), writing(9, 1)],
            vec![writing(9, 1), writing(8, 1)],
        ] {
            let container = ActionGraphContainer {
                artifacts: vec![shared],
                path_fragments: frags.clone(),
                targets: vec![target(8, "//pkg:a"), target(9, "//pkg:b")],
                actions,
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(action_names(&graph, &names), [GEN_H]);
            assert_eq!(parent_of(&graph, &names, GEN_H), Some("ct://pkg:a#cfg1"));
            let artifacts: Vec<&Node> = graph
                .nodes
                .iter()
                .filter(|node| node.kind == Kind::Artifact)
                .collect();
            assert_eq!(artifacts.len(), 1);
            assert_eq!(names.resolve(artifacts[0].id), "artifact1");
            assert_eq!(
                parent_of(&graph, &names, "artifact1"),
                Some("ct://pkg:a#cfg1")
            );
            assert_eq!(edge_list(&graph, &names), [(GEN_H, "artifact1")]);
        }
    }

    #[test]
    fn three_actions_sharing_a_primary_output_are_one_node() {
        let (shared, frags) = generated_artifact(1, 100, "pkg/gen.h");
        for order in [
            [7, 8, 9],
            [7, 9, 8],
            [8, 7, 9],
            [8, 9, 7],
            [9, 7, 8],
            [9, 8, 7],
        ] {
            let container = ActionGraphContainer {
                artifacts: vec![shared],
                path_fragments: frags.clone(),
                targets: vec![
                    target(7, "//pkg:a"),
                    target(8, "//pkg:b"),
                    target(9, "//pkg:c"),
                ],
                actions: order.map(|owner| writing(owner, 1)).to_vec(),
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(action_names(&graph, &names), [GEN_H], "{order:?}");
            assert_eq!(
                parent_of(&graph, &names, GEN_H),
                Some("ct://pkg:a#cfg1"),
                "{order:?}"
            );
            assert_eq!(
                edge_list(&graph, &names),
                [(GEN_H, "artifact1")],
                "{order:?}"
            );
        }
    }

    /// One target built in two configurations registers, in each, the action
    /// writing a file whose path names neither. Here each configured target
    /// lists the file under an artifact id of its own, which its dependents
    /// read.
    #[test]
    fn a_file_two_configurations_share_is_one_artifact_every_reader_reads() {
        let (in_one, mut frags) = generated_artifact(1, 100, "pkg/gen.h");
        let (in_other, more) = generated_artifact(2, 200, "pkg/gen.h");
        frags.extend(more);
        let configuration = |id: u32, checksum: &str| Configuration {
            id,
            mnemonic: "k8".into(),
            checksum: checksum.into(),
            ..Default::default()
        };
        let in_configuration = |configuration_id: u32, out: u32| Action {
            configuration_id,
            ..writing(9, out)
        };
        for writers in [
            [in_configuration(1, 1), in_configuration(2, 2)],
            [in_configuration(2, 2), in_configuration(1, 1)],
        ] {
            let mut actions = writers.to_vec();
            actions.extend([action(7, "Compile", &[10]), action(7, "Link", &[11])]);
            let container = ActionGraphContainer {
                artifacts: vec![in_one, in_other],
                path_fragments: frags.clone(),
                targets: vec![target(7, "//app:main"), target(9, "//pkg:gen")],
                configuration: vec![configuration(1, "ck-late"), configuration(2, "ck-early")],
                dep_set_of_files: vec![dep_set(10, &[1], &[]), dep_set(11, &[2], &[])],
                actions,
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(action_names(&graph, &names), ["action2", "action3", GEN_H]);
            assert_eq!(
                parent_of(&graph, &names, GEN_H),
                Some("ct://pkg:gen#ck-early")
            );
            assert_eq!(
                parent_of(&graph, &names, "artifact2"),
                Some("ct://pkg:gen#ck-early")
            );
            assert!(names.get("artifact1").is_none());
            assert_eq!(
                edge_list(&graph, &names),
                [
                    (GEN_H, "artifact2"),
                    ("artifact2", "action2"),
                    ("artifact2", "action3"),
                ]
            );
        }
    }

    /// The shape aquery dumps show a shared primary output in: one configured
    /// target lists the same action twice, with the same artifact ids.
    #[test]
    fn an_action_a_target_lists_twice_is_one_node() {
        let (shared, frags) = generated_artifact(1, 100, "pkg/gen.h");
        let (input, fragment) = source_artifact(2, "in.c");
        let listed = || Action {
            input_dep_set_ids: vec![10],
            ..writing(9, 1)
        };
        let mut path_fragments = frags;
        path_fragments.push(fragment);
        let container = ActionGraphContainer {
            artifacts: vec![shared, input],
            path_fragments,
            targets: vec![target(9, "//pkg:gen")],
            dep_set_of_files: vec![dep_set(10, &[2], &[])],
            actions: vec![listed(), listed()],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

        assert_eq!(action_names(&graph, &names), [GEN_H]);
        assert_eq!(artifact_names(&graph, &names), ["artifact1"]);
        assert_eq!(
            edge_list(&graph, &names),
            [(GEN_H, "artifact1"), ("source2", GEN_H)]
        );
    }

    #[test]
    fn sharers_owned_alike_are_the_node_of_the_earliest_listed() {
        let (first, mut frags) = generated_artifact(1, 100, "pkg/gen.h");
        let (second, more) = generated_artifact(2, 200, "pkg/gen.h");
        frags.extend(more);
        for (order, kept) in [([1, 2], "artifact1"), ([2, 1], "artifact2")] {
            let container = ActionGraphContainer {
                artifacts: vec![first, second],
                path_fragments: frags.clone(),
                targets: vec![target(9, "//pkg:gen")],
                actions: order.map(|out| writing(9, out)).to_vec(),
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(action_names(&graph, &names), [GEN_H]);
            assert_eq!(artifact_names(&graph, &names), [kept]);
        }
    }

    #[test]
    fn a_sharer_of_a_known_target_stands_ahead_of_one_of_an_unknown_target() {
        let (shared, frags) = generated_artifact(1, 100, "pkg/gen.h");
        for order in [[5, 9], [9, 5]] {
            let container = ActionGraphContainer {
                artifacts: vec![shared],
                path_fragments: frags.clone(),
                targets: vec![target(9, "//pkg:gen")],
                actions: order.map(|owner| writing(owner, 1)).to_vec(),
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(
                parent_of(&graph, &names, GEN_H),
                Some("ct://pkg:gen#cfg1"),
                "{order:?}"
            );
        }
    }

    #[test]
    fn a_sharers_label_decides_ahead_of_its_configuration() {
        let (shared, frags) = generated_artifact(1, 100, "pkg/gen.h");
        let in_configuration = |target_id: u32, configuration_id: u32| Action {
            configuration_id,
            ..writing(target_id, 1)
        };
        for actions in [
            vec![in_configuration(8, 2), in_configuration(9, 1)],
            vec![in_configuration(9, 1), in_configuration(8, 2)],
        ] {
            let container = ActionGraphContainer {
                artifacts: vec![shared],
                path_fragments: frags.clone(),
                targets: vec![target(8, "//pkg:a"), target(9, "//pkg:b")],
                actions,
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(parent_of(&graph, &names, GEN_H), Some("ct://pkg:a#cfg2"));
        }
    }

    /// Two actions write the same two files, each listing them under artifact
    /// ids of its own; a reader names the second file by the id of the action
    /// that has no node.
    #[test]
    fn a_sharers_secondary_output_resolves_to_the_artifact_of_the_action_chosen() {
        let outputs = ["pkg/gen.h", "pkg/gen.c", "pkg/gen.h", "pkg/gen.c"];
        let mut artifacts = Vec::with_capacity(outputs.len());
        let mut path_fragments = Vec::new();
        for (id, name) in (1..).zip(outputs) {
            let (artifact, frags) = generated_artifact(id, id * 100, name);
            artifacts.push(artifact);
            path_fragments.extend(frags);
        }
        let writing_both = |target_id: u32, header: u32, source: u32| Action {
            output_ids: vec![header, source],
            primary_output_id: header,
            ..action(target_id, "Genrule", &[])
        };
        for writers in [
            [writing_both(8, 1, 2), writing_both(9, 3, 4)],
            [writing_both(9, 3, 4), writing_both(8, 1, 2)],
        ] {
            let mut actions = writers.to_vec();
            actions.push(action(7, "Compile", &[10]));
            let container = ActionGraphContainer {
                artifacts: artifacts.clone(),
                path_fragments: path_fragments.clone(),
                targets: vec![
                    target(7, "//app:main"),
                    target(8, "//pkg:a"),
                    target(9, "//pkg:b"),
                ],
                dep_set_of_files: vec![dep_set(10, &[4], &[])],
                actions,
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(artifact_names(&graph, &names), ["artifact1", "artifact2"]);
            assert_eq!(
                edge_list(&graph, &names),
                [
                    (GEN_H, "artifact1"),
                    (GEN_H, "artifact2"),
                    ("artifact2", "action2"),
                ]
            );
        }
    }

    /// An artifact id a sharer lists out of place is also an output a node of
    /// its own writes; a reader of it reads that node's artifact.
    #[test]
    fn an_id_a_sharer_lists_never_stands_in_for_an_artifact_with_a_producer() {
        let (header, mut path_fragments) = generated_artifact(1, 100, "pkg/gen.h");
        let (source, more) = generated_artifact(2, 200, "pkg/gen.c");
        path_fragments.extend(more);
        let sharer = Action {
            output_ids: vec![2, 1],
            primary_output_id: 1,
            ..action(9, "Genrule", &[])
        };
        for actions in [
            vec![writing(8, 1), sharer.clone(), writing(8, 2)],
            vec![writing(8, 2), sharer.clone(), writing(8, 1)],
        ] {
            let mut actions = actions;
            actions.push(action(7, "Compile", &[10]));
            let container = ActionGraphContainer {
                artifacts: vec![header, source],
                path_fragments: path_fragments.clone(),
                targets: vec![
                    target(7, "//app:main"),
                    target(8, "//pkg:a"),
                    target(9, "//pkg:b"),
                ],
                dep_set_of_files: vec![dep_set(10, &[2], &[])],
                actions,
                ..Default::default()
            };

            let (graph, names, _) = build(&[container], false);

            assert_eq!(
                edge_list(&graph, &names),
                [
                    (GEN_C, "artifact2"),
                    (GEN_H, "artifact1"),
                    ("artifact2", "action3"),
                ]
            );
        }
    }

    #[test]
    fn an_action_a_target_shares_with_an_aspect_on_it_is_the_targets_own() {
        let (shared, frags) = generated_artifact(1, 100, "pkg/gen.h");
        let own = || writing(9, 1);
        let of_aspect = || Action {
            aspect_descriptor_ids: vec![1],
            ..writing(9, 1)
        };
        for actions in [vec![own(), of_aspect()], vec![of_aspect(), own()]] {
            let container = ActionGraphContainer {
                artifacts: vec![shared],
                path_fragments: frags.clone(),
                targets: vec![target(9, "//pkg:gen")],
                aspect_descriptors: vec![aspect(1, "//aspects:a.bzl%tag_aspect")],
                actions,
                ..Default::default()
            };

            let (graph, names, facets) = build(&[container], false);

            assert_eq!(action_names(&graph, &names), [GEN_H]);
            assert!(aspects_of(&facets, names.get(GEN_H).unwrap()).is_empty());
            assert_eq!(edge_list(&graph, &names), [(GEN_H, "artifact1")]);
        }
    }

    #[test]
    fn a_sole_writer_beside_sharers_keeps_a_node_of_its_own() {
        let (header, mut frags) = generated_artifact(1, 100, "pkg/gen.h");
        let (source, more) = generated_artifact(2, 200, "pkg/gen.c");
        frags.extend(more);
        let container = ActionGraphContainer {
            artifacts: vec![header, source],
            path_fragments: frags,
            targets: vec![target(8, "//pkg:a"), target(9, "//pkg:b")],
            actions: vec![writing(9, 1), writing(9, 2), writing(8, 1)],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

        assert_eq!(action_names(&graph, &names), [GEN_C, GEN_H]);
        assert_eq!(parent_of(&graph, &names, GEN_H), Some("ct://pkg:a#cfg1"));
        assert_eq!(parent_of(&graph, &names, GEN_C), Some("ct://pkg:b#cfg1"));
        assert_eq!(
            parent_of(&graph, &names, "artifact1"),
            Some("ct://pkg:a#cfg1")
        );
        assert_eq!(
            parent_of(&graph, &names, "artifact2"),
            Some("ct://pkg:b#cfg1")
        );
        assert_eq!(
            edge_list(&graph, &names),
            [(GEN_C, "artifact2"), (GEN_H, "artifact1")]
        );
    }

    #[test]
    fn an_action_naming_no_primary_output_is_named_by_its_place_in_the_query() {
        let (header, mut frags) = generated_artifact(1, 100, "pkg/gen.h");
        let (source, more) = generated_artifact(2, 200, "pkg/gen.c");
        frags.extend(more);
        let container = ActionGraphContainer {
            artifacts: vec![header, source],
            path_fragments: frags,
            targets: vec![target(8, "//pkg:a"), target(9, "//pkg:b")],
            actions: vec![
                writing(8, 1),
                writing(9, 1),
                action(9, "Compile", &[]),
                writing(9, 2),
            ],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

        assert_eq!(action_names(&graph, &names), ["action2", GEN_C, GEN_H]);
    }

    /// The graph of a genrule `@@somerepo+//pkg:gen` whose one output, labelled
    /// `@@somerepo+//pkg:gen.out` by another input, lies at exec path `path`.
    fn external_genrule(path: &str) -> (Graph, Names) {
        let (artifact, frags) = artifact_at(1, 100, path);
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: frags,
            targets: vec![target(9, "@@somerepo+//pkg:gen")],
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
        let labeled = HashSet::from(["@@somerepo+//pkg:gen.out".to_string()]);
        let (graph, names, _) = build_promoting(&[container], false, &labeled);
        (graph, names)
    }

    #[test]
    fn external_labeled_output_is_promoted_within_its_repository() {
        let (graph, names) =
            external_genrule("bazel-out/k8-fastbuild/bin/external/somerepo+/pkg/gen.out");

        assert_eq!(
            find(&graph, &names, "target:@@somerepo+//pkg:gen.out").kind,
            Kind::Artifact
        );
        assert_eq!(
            parent_of(&graph, &names, "target:@@somerepo+//pkg:gen.out"),
            Some("pkg:@@somerepo+//pkg")
        );
        assert_eq!(
            parent_of(&graph, &names, "artifact:@@somerepo+//pkg:gen.out#ck1"),
            Some("ct:@@somerepo+//pkg:gen.out#ck1")
        );
        assert!(
            edges(&graph, &names).contains(&("action0", "artifact:@@somerepo+//pkg:gen.out#ck1"))
        );
    }

    #[test]
    fn sibling_layout_output_is_promoted_within_its_repository() {
        // `--experimental_sibling_repository_layout` names the repository ahead
        // of the configuration instead of below the output tree.
        let (graph, names) = external_genrule("bazel-out/somerepo+/k8-fastbuild/bin/pkg/gen.out");

        assert_eq!(
            parent_of(&graph, &names, "target:@@somerepo+//pkg:gen.out"),
            Some("pkg:@@somerepo+//pkg")
        );
        assert!(
            edges(&graph, &names).contains(&("action0", "artifact:@@somerepo+//pkg:gen.out#ck1"))
        );
    }

    #[test]
    fn external_target_nests_in_repo_qualified_package() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "@somerepo//foo:bar")],
            actions: vec![action(1, "Compile", &[])],
            ..Default::default()
        };

        let (graph, names, _) = build(&[container], false);

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

        let (graph, names, _) = build(&[container], false);

        // Main-repo package keeps its historical id and label.
        assert_eq!(parent_of(&graph, &names, "target://foo:x"), Some("pkg:foo"));
        assert_eq!(find(&graph, &names, "pkg:foo").label, "//foo");

        // The external package is a distinct container.
        let external = parent_of(&graph, &names, "target:@somerepo//foo:y").unwrap();
        assert_ne!(external, "pkg:foo");
        assert_eq!(find(&graph, &names, external).label, "@somerepo//foo");
    }

    #[test]
    fn an_actions_mnemonic_facet_names_the_work_it_runs() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "//foo:app")],
            actions: vec![action(1, "CppCompile", &[])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let node = names.get("action0").unwrap();
        let mnemonic = facets.mnemonic(node).unwrap();
        assert_eq!(facets.strings().resolve(mnemonic), "CppCompile");
        assert!(facets.aspects().values(node).is_empty());
    }

    #[test]
    fn an_aspect_action_carries_the_names_its_descriptors_give() {
        // A Starlark aspect is named `<bzl label>%<name>`, a native one by its
        // Java class; both reach the facet as Bazel writes them.
        let container = ActionGraphContainer {
            targets: vec![target(1, "//foo:app")],
            aspect_descriptors: vec![
                aspect(1, "//aspects:rust.bzl%rust_analyzer_aspect"),
                aspect(
                    2,
                    "com.google.devtools.build.lib.rules.java.JavaSourceJarsAspect",
                ),
            ],
            actions: vec![aspect_action(1, "Compile", &[1, 2])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        assert_eq!(
            aspects_of(&facets, names.get("action0").unwrap()),
            [
                "//aspects:rust.bzl%rust_analyzer_aspect",
                "com.google.devtools.build.lib.rules.java.JavaSourceJarsAspect",
            ]
        );
    }

    #[test]
    fn a_target_box_is_declared_by_the_rule_class_its_id_names() {
        let container = ActionGraphContainer {
            targets: vec![declared_target(1, "//foo:app", 7)],
            rule_classes: vec![rule_class(7, "cc_test")],
            actions: vec![action(1, "Compile", &[])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let node = names.get("target://foo:app").unwrap();
        let rule_class = facets.rule_class(node).unwrap();
        assert_eq!(facets.strings().resolve(rule_class.sym()), "cc_test");
        assert_eq!(facets.rule_category(rule_class), Some(RuleCategory::Test));
        assert_eq!(facets.label(node), Some(node));
        assert_eq!(facets.package(node), names.get("pkg:foo"));
        assert!(facets.repo(node).is_some());
    }

    #[test]
    fn a_target_naming_no_rule_class_carries_none() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "//foo:app")],
            actions: vec![action(1, "Compile", &[])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let node = names.get("target://foo:app").unwrap();
        assert_eq!(facets.rule_class(node), None);
        assert_eq!(facets.label(node), Some(node));
    }

    #[test]
    fn a_configured_target_box_is_built_in_the_configuration_its_actions_run_in() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "//foo:app")],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8-opt-exec".into(),
                checksum: "ck1".into(),
                is_tool: true,
                ..Default::default()
            }],
            actions: vec![action(1, "Compile", &[])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let config = facets
            .config(names.get("ct://foo:app#ck1").unwrap())
            .unwrap();
        assert_eq!(facets.strings().resolve(config.sym()), "ck1");
        let entry = facets.config_entry(config).unwrap();
        assert_eq!(
            entry.mnemonic.map(|name| facets.strings().resolve(name)),
            Some("k8-opt-exec")
        );
        assert_eq!(entry.is_tool, Tri::Yes);
    }

    #[test]
    fn a_table_entry_written_after_the_action_naming_it_still_resolves() {
        // Each interned entry reaches the stream the first time it is seen, so
        // an action can name an id whose entry arrives in a later container.
        let naming = ActionGraphContainer {
            targets: vec![declared_target(1, "//foo:app", 7)],
            actions: vec![aspect_action(1, "Compile", &[3])],
            ..Default::default()
        };
        let describing = ActionGraphContainer {
            rule_classes: vec![rule_class(7, "cc_library")],
            aspect_descriptors: vec![aspect(3, "//aspects:a.bzl%tag_aspect")],
            ..Default::default()
        };

        let (_, names, facets) = build(&[naming, describing], false);

        let rule_class = facets
            .rule_class(names.get("target://foo:app").unwrap())
            .unwrap();
        assert_eq!(facets.strings().resolve(rule_class.sym()), "cc_library");
        assert_eq!(
            aspects_of(&facets, names.get("action0").unwrap()),
            ["//aspects:a.bzl%tag_aspect"]
        );
    }

    #[test]
    fn a_source_leaf_carries_the_label_its_package_gives_it() {
        let (a1, f1) = source_artifact(1, "foo/in.c");
        let container = ActionGraphContainer {
            artifacts: vec![a1],
            path_fragments: vec![f1],
            targets: vec![target(1, "//foo:app")],
            dep_set_of_files: vec![dep_set(10, &[1], &[])],
            actions: vec![action(1, "Compile", &[10])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let node = names.get("target://foo:in.c").unwrap();
        assert_eq!(facets.label(node), Some(node));
        assert_eq!(facets.package(node), names.get("pkg:foo"));
        assert!(facets.repo(node).is_some());
    }

    #[test]
    fn one_rule_class_reaches_a_targets_whole_subtree() {
        let (artifact, frags) = generated_artifact(1, 100, "foo/app.o");
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: frags,
            targets: vec![declared_target(1, "//foo:app", 7)],
            rule_classes: vec![rule_class(7, "cc_binary")],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8-fastbuild".into(),
                checksum: "ck1".into(),
                ..Default::default()
            }],
            actions: vec![Action {
                target_id: 1,
                mnemonic: "CppCompile".into(),
                configuration_id: 1,
                output_ids: vec![1],
                ..Default::default()
            }],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let declared = facets
            .rule_class(names.get("target://foo:app").unwrap())
            .unwrap();
        assert_eq!(facets.strings().resolve(declared.sym()), "cc_binary");
        for node in ["ct://foo:app#ck1", "action0", "artifact1"] {
            let id = names.get(node).unwrap();
            assert_eq!(facets.rule_class(id), Some(declared), "{node}");
        }
    }

    #[test]
    fn an_action_belongs_to_its_targets_repository_and_configuration() {
        let container = ActionGraphContainer {
            targets: vec![target(1, "@@somerepo+//foo:app")],
            configuration: vec![Configuration {
                id: 1,
                mnemonic: "k8-opt-exec".into(),
                checksum: "ck1".into(),
                is_tool: true,
                ..Default::default()
            }],
            actions: vec![action(1, "CppCompile", &[])],
            ..Default::default()
        };

        let (_, names, facets) = build(&[container], false);

        let node = names.get("action0").unwrap();
        let repo = facets.repo(node).unwrap();
        assert_eq!(facets.repo_name(repo), RepoName::Canonical("somerepo+"));
        let config = facets.config(node).unwrap();
        assert_eq!(facets.strings().resolve(config.sym()), "ck1");
        assert_eq!(facets.config_entry(config).unwrap().is_tool, Tri::Yes);
        assert_eq!(facets.label(node), names.get("target:@@somerepo+//foo:app"));
        assert_eq!(facets.package(node), names.get("pkg:@@somerepo+//foo"));
    }

    #[test]
    fn a_promoted_output_belongs_to_its_own_target_not_the_rule_producing_it() {
        let (artifact, gen_frags) = generated_artifact(1, 100, "pkg/gen.out");
        let container = ActionGraphContainer {
            artifacts: vec![artifact],
            path_fragments: gen_frags,
            targets: vec![declared_target(9, "//pkg:gen", 7)],
            rule_classes: vec![rule_class(7, "genrule")],
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
        let labeled = HashSet::from(["//pkg:gen.out".to_string()]);

        let (_, names, facets) = build_promoting(&[container], false, &labeled);

        let concrete = names.get("artifact://pkg:gen.out#ck1").unwrap();
        assert_eq!(facets.label(concrete), names.get("target://pkg:gen.out"));
        // The output file is not itself declared by a rule, so nothing states a
        // rule class for it; the genrule's does not reach across.
        assert_eq!(facets.rule_class(concrete), None);
        assert_eq!(
            facets.config(concrete),
            facets.config(names.get("ct://pkg:gen#ck1").unwrap())
        );
    }
}
