//! What a query is evaluated against: one graph, indexed once for every
//! expression evaluated over it.

use std::sync::Arc;

use fixedbitset::FixedBitSet;
use rustc_hash::FxHashMap;

use super::ast::Function;
use super::config::Configurations;
use super::declared::Declared;
use super::narrow::Patterns;
use super::stack::Value;
use crate::attributes::Attributes;
use crate::capability::{Axis, Capabilities, Support};
use crate::facets::{Facets, Sym};
use crate::graph::{Graph, Kind, id_span};
use crate::intern::{Id, Names};
use crate::joint::Joint;
use crate::provenance::Input;
use crate::reach::{Index, Level};
use crate::select::Scope;
use crate::select::labels::LabelIndex;
use crate::target_pattern::TargetIndex;

/// One graph as a query sees it. A query's value is a set of its nodes by id,
/// sized by [`Universe::ids`].
pub struct Universe {
    pub(super) index: Index,
    pub(super) labels: Arc<LabelIndex>,
    pub(super) targets: Arc<TargetIndex>,
    /// Where the target each node belongs to lies among the labels, by node
    /// id; [`NO_LABEL`] for a node belonging to none.
    owner: Vec<u32>,
    /// The nodes a value may hold: every node but the frames.
    members: FixedBitSet,
    pub(super) facets: Arc<Facets>,
    pub(super) attributes: Arc<Attributes>,
    /// The name of the `visibility` attribute, where some rule states it.
    pub(super) visibility: Option<Sym>,
    pub(super) declared: Declared,
    pub(super) configurations: Configurations,
    pub(super) readable: Readable,
    pub(super) patterns: Patterns,
}

/// What a graph's inputs state of the data some functions read whatever they
/// are called with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Readable {
    holds_actions: bool,
    /// How far the inputs state the configuration each configured target is
    /// built in.
    configured: Support,
    /// How far the inputs state the files each action reads and writes.
    artifacts: Support,
}

impl Readable {
    /// What `graph` states with the inputs `capabilities` counts read.
    pub fn of(graph: &Graph, capabilities: &Capabilities) -> Self {
        let holds_actions = graph.nodes.iter().any(|node| node.kind == Kind::Action);
        Self::holding(holds_actions, capabilities)
    }

    /// What the graph `self` was taken of, with every input that built it
    /// read, states with only the inputs `capabilities` counts read.
    pub fn reading(self, capabilities: &Capabilities) -> Self {
        Self::holding(self.holds_actions, capabilities)
    }

    /// Only aquery describes actions, so a graph's actions are read only with
    /// it.
    fn holding(holds_actions: bool, capabilities: &Capabilities) -> Self {
        Readable {
            holds_actions: holds_actions && capabilities.inputs().contains(Input::Aquery),
            configured: capabilities.support(Axis::Configuration),
            artifacts: capabilities.support(Axis::Artifacts),
        }
    }

    /// Why `function` fails over the graph however it is called, if it does.
    pub fn check(self, function: Function) -> Result<(), String> {
        let name = function.name();
        match function {
            Function::Mnemonic
            | Function::Inputs
            | Function::Outputs
            | Function::RunfilesPlumbing
                if !self.holds_actions =>
            {
                Err(format!("{name}() requires an aquery"))
            }
            Function::Inputs | Function::Outputs => match self.artifacts {
                Support::Unavailable { flag } => {
                    Err(format!("{name}() requires an aquery with {flag}"))
                }
                Support::Available | Support::Partial { .. } => Ok(()),
            },
            Function::ToolConfigured if self.configured != Support::Available => Err(format!(
                "{name}() requires every configured target's configuration"
            )),
            _ => Ok(()),
        }
    }
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
        attributes: &Arc<Attributes>,
        capabilities: &Capabilities,
    ) -> Self {
        let labels = Arc::new(LabelIndex::of(graph, names, facets));
        let targets = Arc::new(TargetIndex::of(labels, graph, facets, capabilities));
        Universe::with_targets(graph, targets, facets, attributes, capabilities)
    }

    /// The universe of `joint`'s graph, with the names its node ids resolve
    /// through.
    pub fn of_joint(joint: Joint) -> (Self, Names) {
        let universe = Universe::new(
            &joint.graph,
            &joint.names,
            &Arc::new(joint.facets),
            &Arc::new(joint.attributes),
            &joint.capabilities,
        );
        (universe, joint.names)
    }

    /// The universe of `graph`, whose targets `targets` indexes.
    pub fn with_targets(
        graph: &Graph,
        targets: Arc<TargetIndex>,
        facets: &Arc<Facets>,
        attributes: &Arc<Attributes>,
        capabilities: &Capabilities,
    ) -> Self {
        let labels = Arc::clone(targets.labels());
        let ids = id_span(graph);
        let mut at: FxHashMap<Id, u32> =
            FxHashMap::with_capacity_and_hasher(labels.len(), Default::default());
        at.extend((labels.entries().iter().enumerate()).map(|(at, (id, _))| (*id, at as u32)));
        let mut owner: Vec<u32> = vec![NO_LABEL; ids];
        let mut members = FixedBitSet::with_capacity(ids);
        for node in &graph.nodes {
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
            visibility: attributes.name("visibility"),
            attributes: Arc::clone(attributes),
            index,
            labels,
            targets,
            owner,
            members,
            readable: Readable::of(graph, capabilities),
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

    /// A filter's scope over `value`, which `query` named.
    pub fn scope(&self, query: impl Into<Arc<str>>, value: &Value) -> Scope {
        Scope::new(query.into(), self.scope_nodes(value))
    }

    /// What a scope over `value` holds: each node of its set with every node
    /// it holds, and the files each action among those writes, which the graph
    /// holds beside the action in its configured target; but for what `value`
    /// denies.
    pub(super) fn scope_nodes(&self, value: &Value) -> FixedBitSet {
        self.closure(&value.set, true, |id| !value.denied.contains(id))
    }

    /// `value` with every node its members hold, at any depth: what each
    /// operand of `intersect` and the right operand of `except` stands for. An
    /// action belongs to the configured target running it, as aquery projects
    /// actions from configured targets, so taking a target out of a set or
    /// keeping it takes or keeps its configured targets, their actions and the
    /// files those hold.
    pub(super) fn with_held(&self, value: &FixedBitSet) -> FixedBitSet {
        self.closure(value, false, |_| true)
    }

    /// `value` with every node `admits` that its members hold, and, where
    /// `writes`, the files each action reached writes along with what those
    /// hold.
    fn closure(
        &self,
        value: &FixedBitSet,
        writes: bool,
        admits: impl Fn(usize) -> bool,
    ) -> FixedBitSet {
        let mut closure = value.clone();
        let mut open: Vec<Id> = Vec::with_capacity(value.count_ones(..));
        open.extend(value.ones().map(|id| Id(id as u32)));
        while let Some(id) = open.pop() {
            let written = (writes && self.index.kind(id) == Some(Kind::Action))
                .then(|| self.index.dependents(id))
                .into_iter()
                .flatten()
                .filter(|&file| self.index.kind(file) == Some(Kind::Artifact));
            for reached in self.index.children(id).chain(written) {
                let at = reached.0 as usize;
                if admits(at) && !closure.put(at) {
                    open.push(reached);
                }
            }
        }
        closure
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::joint::build_joint;
    use crate::provenance::InputSet;
    use crate::sources::Sources;
    use crate::test_support::{ActionSpec, action_graph, rule};

    /// The joint graph of one rule query read and the action aquery read
    /// registering it.
    fn joint() -> Joint {
        let query = [rule("//app:bin", "cc_binary", &[], Vec::new())];
        let aquery = action_graph(&[ActionSpec {
            label: "//app:bin",
            mnemonic: "CppLink",
            inputs: &[],
            outputs: &["bazel-out/bin/app/bin"],
        }]);
        build_joint(Some(&[aquery]), None, Some(&query), false)
    }

    /// With aquery off, what reads actions fails as it does over a graph read
    /// without one, and the graph the setting leaves states the same.
    #[test]
    fn what_a_graph_states_follows_the_inputs_read() {
        let joint = joint();
        let every = Readable::of(&joint.graph, &joint.capabilities);
        assert_eq!(every.check(Function::Mnemonic), Ok(()));

        let query = Sources::reading(Input::Query.into());
        let capabilities = joint.capabilities.reading(query.inputs());
        let narrowed = every.reading(&capabilities);
        assert_eq!(
            narrowed.check(Function::Mnemonic),
            Err("mnemonic() requires an aquery".to_owned())
        );
        let masked = query.mask(&joint.graph, &joint.provenance);
        assert_eq!(narrowed, Readable::of(&masked, &capabilities));

        let both = InputSet::of([Input::Query, Input::Aquery]).unwrap();
        assert_eq!(every.reading(&joint.capabilities.reading(both)), every);
    }
}
