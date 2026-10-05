//! The query functions answered by walking the graph or reading its labels,
//! each as Bazel's own of that name in `query2/engine/` answers it.

use fixedbitset::FixedBitSet;
use rustc_hash::FxHashSet;

use super::universe::Universe;
use crate::intern::Id;
use crate::reach::{Bounds, Level, Routed, State, Toward, Walk};

/// Every level of the graph, shallowest first.
const LEVELS: [Level; 3] = [Level::Target, Level::ConfiguredTarget, Level::Action];

impl Universe {
    /// Everything `seeds` depends on within `depth` edges, and `seeds`
    /// themselves. A depth of zero names `seeds` alone, as Bazel's does; any
    /// deeper walk takes in what a seed holds at no cost. A negative depth
    /// reaches nothing.
    ///
    /// A node is a dependency where the walk arrives at it or at anything it
    /// holds, the mirror of [`Self::rdeps`] seeding the walk at all a seed
    /// holds: `x` is among the deps of `y` exactly where `y` is among the
    /// rdeps of `x`, both standing at one level. Across levels the relation
    /// runs one way: a node's deps and rdeps take in those of everything it
    /// holds, while neither covers a node above the level its walk starts at,
    /// so `y` may be among the rdeps of a shallower `x` that is no dep of `y`.
    pub(super) fn deps(&self, seeds: &FixedBitSet, depth: Option<i32>) -> FixedBitSet {
        if depth == Some(0) {
            return self.seeds_alone(seeds, None);
        }
        let mut covered = self.empty();
        for (level, walk) in self.walks(seeds, Toward::Dependencies, depth, None) {
            self.cover_arrivals(&walk, level, &mut covered);
        }
        covered.intersect_with(self.members());
        covered
    }

    /// Everything depending on `seeds` within `depth` edges, and `seeds`
    /// themselves, by paths running within `universe` where one is given.
    /// What a seed holds depends on it as the seed does.
    pub(super) fn rdeps(
        &self,
        universe: Option<&FixedBitSet>,
        seeds: &FixedBitSet,
        depth: Option<i32>,
    ) -> FixedBitSet {
        if depth == Some(0) {
            return self.seeds_alone(seeds, universe);
        }
        let mut covered = self.empty();
        for (level, walk) in self.walks(seeds, Toward::Dependents, depth, universe) {
            let ids = (0..self.ids()).map(|id| Id(id as u32));
            let reached = ids.filter(|&id| walk.reached(id) && !self.index.above(id, level));
            covered.extend(reached.map(|id| id.0 as usize));
        }
        covered.intersect_with(self.members());
        covered
    }

    /// The members of `seeds` within `universe`, which a walk of no edges
    /// covers.
    fn seeds_alone(&self, seeds: &FixedBitSet, universe: Option<&FixedBitSet>) -> FixedBitSet {
        let mut alone = seeds.clone();
        alone.intersect_with(self.members());
        if let Some(universe) = universe {
            alone.intersect_with(universe);
        }
        alone
    }

    /// Every node on a dependency path from one of `from` down to one of `to`.
    pub(super) fn all_paths(&self, from: &FixedBitSet, to: &FixedBitSet) -> FixedBitSet {
        if to.is_clear() {
            return self.empty();
        }
        let below = self.deps(from, None);
        self.rdeps(Some(&below), to, None)
    }

    /// The nodes of one path of fewest edges from one of `from` down to one of
    /// `to`, empty where none of `to` lies below `from`.
    pub(super) fn some_path(&self, from: &FixedBitSet, to: &FixedBitSet) -> FixedBitSet {
        // The fewest edges, the walk and the level it is held to, its state
        // arrived at, and the member of `to` holding that state's node.
        let mut nearest: Option<(u32, Level, Routed, usize, Id)> = None;
        for level in LEVELS {
            let mut states: Vec<usize> = Vec::new();
            for seed in self.at_level(from, level) {
                states.extend([
                    State::arrived(seed.0 as usize),
                    State::lifted(seed.0 as usize),
                ]);
            }
            if states.is_empty() {
                continue;
            }
            let bounds = Bounds {
                ceiling: level,
                ..Bounds::NONE
            };
            let routed = self.index.routed(&states, Toward::Dependencies, bounds);
            let mut best: Option<(u32, usize, Id)> = None;
            for id in (0..self.ids()).map(|id| Id(id as u32)) {
                let state = State::arrived(id.0 as usize);
                let Some(edges) = routed.walk.fewest(state) else {
                    continue;
                };
                let holder = self
                    .holders(id, level)
                    .find(|holder| to.contains(holder.0 as usize));
                if let Some(end) = holder
                    && best.is_none_or(|(fewest, _, _)| edges < fewest)
                {
                    best = Some((edges, state, end));
                }
            }
            if let Some((edges, state, end)) = best
                && nearest.as_ref().is_none_or(|(fewest, ..)| edges < *fewest)
            {
                nearest = Some((edges, level, routed, state, end));
            }
        }
        let mut path = self.empty();
        if let Some((_, level, routed, state, end)) = nearest {
            let (seed, edges) = routed.route(state);
            let ends = edges.into_iter().flat_map(|edge| self.index.ends(edge));
            let nodes = [Id(State::id(seed) as u32), Id(State::id(state) as u32), end];
            // A node above the level is one the walk only passed through.
            let covered = nodes
                .into_iter()
                .chain(ends)
                .filter(|&id| !self.index.above(id, level));
            path.extend(covered.map(|id| id.0 as usize));
            path.intersect_with(self.members());
        }
        path
    }

    /// Every target in a package any of `value` belongs to, with the
    /// configured targets each holds.
    pub(super) fn siblings(&self, value: &FixedBitSet) -> FixedBitSet {
        let mut packages: FxHashSet<&str> = FxHashSet::default();
        let mut targets: Vec<Id> = Vec::new();
        for id in value.ones() {
            let Some(package) = self.package(Id(id as u32)) else {
                continue;
            };
            if packages.insert(package) {
                let held = self.labels.starting(package);
                targets.extend(held.map(|at| self.labels.entries()[at].0));
            }
        }
        self.with_configured(targets)
    }

    /// What depends directly on one of `value` at its own level and belongs
    /// to its package.
    pub(super) fn same_package_direct_rdeps(
        &self,
        value: &FixedBitSet,
    ) -> Result<FixedBitSet, String> {
        let mut rdeps = self.empty();
        for id in value.ones().map(|id| Id(id as u32)) {
            let level = self.level(id);
            if level == Level::Action {
                return Err(
                    "same_pkg_direct_rdeps() takes targets and configured targets, \
                     and no actions or the files they write"
                        .to_string(),
                );
            }
            let package = self.package(id);
            let alike = |dependent: Id| {
                self.level(dependent) == level && self.package(dependent) == package
            };
            rdeps.extend(
                self.index
                    .dependents(id)
                    .filter(|&d| alike(d))
                    .map(|d| d.0 as usize),
            );
        }
        rdeps.intersect_with(self.members());
        Ok(rdeps)
    }

    /// The first `count` of `value` by label, `None` where that is none of
    /// them.
    pub(super) fn some(&self, value: &FixedBitSet, count: i32) -> Option<FixedBitSet> {
        let count = usize::try_from(count).ok().filter(|&count| count > 0)?;
        let mut members: Vec<(usize, usize)> = value
            .ones()
            .map(|id| (self.position(Id(id as u32)).unwrap_or(usize::MAX), id))
            .collect();
        if members.is_empty() {
            return None;
        }
        members.sort_unstable();
        let mut some = self.empty();
        some.extend(members.into_iter().take(count).map(|(_, id)| id));
        Some(some)
    }

    /// The label prefix naming the package of the target `id` belongs to,
    /// colon and all.
    fn package(&self, id: Id) -> Option<&str> {
        let label = self.label(id)?;
        let colon = label.find(':')?;
        Some(&label[..=colon])
    }

    /// The members of `value` standing at `level`.
    fn at_level<'a>(
        &'a self,
        value: &'a FixedBitSet,
        level: Level,
    ) -> impl Iterator<Item = Id> + 'a {
        let ids = value.ones().map(|id| Id(id as u32));
        ids.filter(move |&id| self.level(id) == level)
    }

    /// `id` and the nodes holding it, up to the last at or below `ceiling`.
    fn holders(&self, id: Id, ceiling: Level) -> impl Iterator<Item = Id> + '_ {
        let up = std::iter::successors(Some(id), |&at| self.index.parent(at));
        up.take_while(move |&at| !self.index.above(at, ceiling))
    }

    /// Adds to `covered` each node `walk`, held to `ceiling`, arrived at, and
    /// every node holding one up to the ceiling.
    fn cover_arrivals(&self, walk: &Walk, ceiling: Level, covered: &mut FixedBitSet) {
        for id in (0..self.ids()).map(|id| Id(id as u32)) {
            if !walk.arrived(id) {
                continue;
            }
            covered.extend(self.holders(id, ceiling).map(|holder| holder.0 as usize));
        }
    }

    /// The walks `toward` from `seeds` within `depth` edges and `within`, one
    /// for each level `seeds` stand at, held to it. A walk toward dependents
    /// starts at every node a seed holds, one toward dependencies at both
    /// states of each seed.
    fn walks(
        &self,
        seeds: &FixedBitSet,
        toward: Toward,
        depth: Option<i32>,
        within: Option<&FixedBitSet>,
    ) -> Vec<(Level, Walk)> {
        let depth = match depth.map(u32::try_from) {
            Some(Err(_)) => return Vec::new(),
            Some(Ok(depth)) => Some(depth),
            None => None,
        };
        let mut walks: Vec<(Level, Walk)> = Vec::with_capacity(LEVELS.len());
        let mut states: Vec<usize> = Vec::new();
        for level in LEVELS {
            states.clear();
            for seed in self.at_level(seeds, level) {
                match toward {
                    Toward::Dependencies => states.extend([
                        State::arrived(seed.0 as usize),
                        State::lifted(seed.0 as usize),
                    ]),
                    Toward::Dependents => self.subtree_arrivals(seed, &mut states),
                }
            }
            if states.is_empty() {
                continue;
            }
            let bounds = Bounds {
                depth,
                within,
                ceiling: level,
            };
            walks.push((level, self.index.walk(&states, toward, bounds)));
        }
        walks
    }

    /// Pushes the arrival at `root` and at each node it holds onto `states`.
    fn subtree_arrivals(&self, root: Id, states: &mut Vec<usize>) {
        let start = states.len();
        states.push(State::arrived(root.0 as usize));
        let mut at = start;
        while at < states.len() {
            let held = self.index.children(Id(State::id(states[at]) as u32));
            states.extend(held.map(|child| State::arrived(child.0 as usize)));
            at += 1;
        }
    }
}
