//! What a query is evaluated against: one graph, indexed once for every
//! expression evaluated over it.

use std::sync::Arc;

use fixedbitset::FixedBitSet;
use rustc_hash::FxHashMap;

use super::config::Configurations;
use super::declared::Declared;
use super::narrow::Patterns;
use crate::capability::{Axis, Capabilities, Support};
use crate::facets::Facets;
use crate::graph::{Graph, Kind, id_span};
use crate::intern::{Id, Names};
use crate::reach::{Index, Level};
use crate::select::labels::LabelIndex;
use crate::target_pattern::TargetIndex;

/// One graph as a query sees it. A query's value is a set of its nodes by id,
/// sized by [`Universe::ids`].
pub struct Universe {
    pub(super) index: Index,
    pub(super) labels: Arc<LabelIndex>,
    pub(super) targets: TargetIndex,
    /// Where the target each node belongs to lies among the labels, by node
    /// id; [`NO_LABEL`] for a node belonging to none.
    owner: Vec<u32>,
    /// The nodes a value may hold: every node but the frames.
    members: FixedBitSet,
    pub(super) facets: Arc<Facets>,
    pub(super) declared: Declared,
    pub(super) configurations: Configurations,
    /// How far the inputs state each rule's tags.
    pub(super) tags: Support,
    /// How far the inputs state whether each rule is `testonly`.
    pub(super) testonly: Support,
    /// How far the inputs state the files each action reads and writes.
    pub(super) artifacts: Support,
    pub(super) holds_actions: bool,
    pub(super) patterns: Patterns,
}

/// How many nodes a query's value holds, and how many of them are of the kinds
/// a reader tells a build's size by.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Counts {
    pub nodes: usize,
    pub targets: usize,
    pub configured_targets: usize,
    pub actions: usize,
}

/// Stands in [`Universe::owner`] for a node belonging to no target.
const NO_LABEL: u32 = u32::MAX;

impl Universe {
    pub fn new(
        graph: &Graph,
        names: &Names,
        facets: &Arc<Facets>,
        capabilities: &Capabilities,
    ) -> Self {
        let labels = Arc::new(LabelIndex::of(graph, names, facets));
        let targets = TargetIndex::of(labels.clone(), graph, facets, capabilities);
        let ids = id_span(graph);
        let mut at: FxHashMap<Id, u32> =
            FxHashMap::with_capacity_and_hasher(labels.len(), Default::default());
        at.extend((labels.entries().iter().enumerate()).map(|(at, (id, _))| (*id, at as u32)));
        let mut owner: Vec<u32> = vec![NO_LABEL; ids];
        let mut members = FixedBitSet::with_capacity(ids);
        let mut holds_actions = false;
        for node in &graph.nodes {
            holds_actions |= node.kind == Kind::Action;
            if let Some(&at) = facets.label(node.id).and_then(|label| at.get(&label)) {
                owner[node.id.0 as usize] = at;
            }
            members.set(node.id.0 as usize, !node.kind.is_frame());
        }
        let index = Index::new(graph, &[]);
        Universe {
            declared: Declared::new(labels.entries(), &index, facets),
            configurations: Configurations::new(graph, facets),
            facets: Arc::clone(facets),
            index,
            labels,
            targets,
            owner,
            members,
            tags: capabilities.support(Axis::Tags),
            testonly: capabilities.support(Axis::Testonly),
            artifacts: capabilities.support(Axis::Artifacts),
            holds_actions,
            patterns: Patterns::default(),
        }
    }

    /// How many ids a value spans.
    pub fn ids(&self) -> usize {
        self.members.len()
    }

    /// An empty value.
    pub fn empty(&self) -> FixedBitSet {
        FixedBitSet::with_capacity(self.ids())
    }

    pub fn counts(&self, value: &FixedBitSet) -> Counts {
        let mut counts = Counts::default();
        for id in value.ones() {
            counts.nodes += 1;
            match self.index.kind(Id(id as u32)) {
                Some(Kind::Target) => counts.targets += 1,
                Some(Kind::ConfiguredTarget) => counts.configured_targets += 1,
                Some(Kind::Action) => counts.actions += 1,
                _ => {}
            }
        }
        counts
    }

    pub(super) fn members(&self) -> &FixedBitSet {
        &self.members
    }

    /// The level a walk from `id` is held to: its own, and the shallowest for
    /// a node standing at every level.
    pub(super) fn level(&self, id: Id) -> Level {
        self.index.level(id).unwrap_or(Level::Target)
    }

    /// The canonical label of the target `id` belongs to.
    pub fn label(&self, id: Id) -> Option<&str> {
        self.position(id)
            .map(|at| self.labels.entries()[at].1.as_str())
    }

    /// Where the target `id` belongs to lies among the labels.
    pub(super) fn position(&self, id: Id) -> Option<usize> {
        let at = *self.owner.get(id.0 as usize)?;
        (at != NO_LABEL).then_some(at as usize)
    }

    /// `targets` with the configured targets each holds, which a target
    /// pattern stands for in a graph holding both.
    pub(super) fn with_configured(&self, targets: impl IntoIterator<Item = Id>) -> FixedBitSet {
        let mut value = self.empty();
        for target in targets {
            value.insert(target.0 as usize);
            let held = self.index.children(target);
            let configured = held.filter(|&c| self.index.kind(c) == Some(Kind::ConfiguredTarget));
            value.extend(configured.map(|c| c.0 as usize));
        }
        value
    }
}
