//! What exists in a build only because of each of its units: the dominator
//! tree of a scope's points, walked from each point into its dependencies.
//!
//! A point dominates another when every path from the top of the build to the
//! other passes through it, so a build without the point leaves out every
//! point it dominates. The top of the build stands above its roots: by default
//! every point no other depends on, and each cycle no point outside it depends
//! on, so that every counted point of the scope sits in the tree; or a chosen
//! set of points, leaving out of the tree every point none of them reaches.

mod shown;

pub use shown::{Above, Placing, ShownTree};

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::hash::{BuildHasher, Hash, Hasher};
use std::ops::Range;
use std::sync::Arc;

use rayon::prelude::*;
use rustc_hash::{FxBuildHasher, FxHashMap, FxHashSet};

use crate::facets::Facets;
use crate::graph::{Graph, Kind};
use crate::intern::{Id, Names, stable_hash};
use crate::metrics::{Count, Scope, Unit, UnitGraph, unit_graph};
use crate::provenance::Provenance;
use crate::reduce::{Condensation, DenseIds, reach_counts};

/// The dominator tree of the counted points of a scope the roots reach. A
/// point not counted towards others is no product of the build and takes no
/// part.
///
/// The children of each node take consecutive node indices, heaviest first
/// and among equal weights by a hash of their points' names, an order every
/// run of the queries gives alike, then by name where two hashes collide. So
/// do the tops of the tree, in two runs each ordered so: the roots, which are
/// all tops, then the shared tops, each a dependency several roots share.
pub struct Dominators {
    scope: Scope,
    nodes: Vec<Dominated>,
    /// By node, the index its children start at, and past the last node the
    /// node count, so each node's children end where the next node's start
    /// and the tops take every index ahead of the first node's children.
    bounds: Vec<u32>,
    index: DenseIds,
    dependents: Dependents,
    /// How many of the tops are roots.
    roots: u32,
    outside: u32,
    /// How many points the other tops dominate between them.
    shared_weight: u32,
    /// By top, the strongly connected component of the scope's points its
    /// point belongs to.
    cycle: Vec<u32>,
    /// By root, where the roots are configured targets, another copy of its
    /// target the tree depends on.
    elsewhere: Vec<Option<Elsewhere>>,
}

struct Dominated {
    id: Id,
    kind: Kind,
    weight: u32,
    standing: Standing,
}

/// Where a point stands towards the roots of its tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// A root no point outside its own cycle reaches: where `in_cycle`, the
    /// other members of its cycle reach it, and otherwise nothing does.
    Root { in_cycle: bool },
    /// A root this many other roots reach, none of them in its own cycle.
    ReachedRoot(Count),
    /// No root, reached, directly or transitively, from this many roots.
    Below(Count),
}

impl Standing {
    pub fn is_root(self) -> bool {
        !matches!(self, Standing::Below(_))
    }
}

/// What flags a root of configured targets whose target the build also
/// reaches as a dependency: another configured copy of the same target that
/// is a point of the tree and that a point of the tree depends on. The root
/// itself may have no dependent, so each root of a target configured at the
/// top in many configurations is flagged once any one copy is depended on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elsewhere {
    pub copy: Id,
    /// A point of the tree depending on `copy`.
    pub dependent: Id,
    /// How many copies of the target other than the root a point of the tree
    /// depends on, `copy` among them.
    pub copies: u32,
}

/// The points a dominator tree is rooted at.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Roots {
    /// The top-level points of the scope: those nothing depends on but the
    /// other members of a cycle they belong to.
    Tops,
    Chosen(Chosen),
}

/// A set of points, each named once, in id order.
#[derive(Clone, Debug)]
pub struct Chosen {
    ids: Arc<[Id]>,
    /// A hash of `ids`, taken once so that hashing the set takes no pass over
    /// its points.
    fingerprint: u64,
}

impl Chosen {
    pub fn new(ids: impl IntoIterator<Item = Id>) -> Self {
        let mut ids: Vec<Id> = ids.into_iter().collect();
        ids.sort_unstable();
        ids.dedup();
        let fingerprint = FxBuildHasher.hash_one(&ids);
        Chosen {
            ids: ids.into(),
            fingerprint,
        }
    }

    pub fn ids(&self) -> &[Id] {
        &self.ids
    }
}

impl PartialEq for Chosen {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.ids, &other.ids)
            || (self.fingerprint == other.fingerprint && self.ids == other.ids)
    }
}

impl Eq for Chosen {}

impl Hash for Chosen {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.fingerprint);
    }
}

/// By node, the nodes depending on it directly, in node order.
struct Dependents {
    /// By node, where its dependents start in `of`, and past the last node the
    /// length of `of`.
    start: Vec<u32>,
    of: Vec<u32>,
}

impl Dependents {
    fn of(&self, node: u32) -> &[u32] {
        &self.of[self.start[node as usize] as usize..self.start[node as usize + 1] as usize]
    }
}

/// The most points [`Dominators::roots_above`] reaches on its walk up from a
/// node, however large the graph; each point it walks through has every one
/// of its dependents scanned.
const ABOVE_WALK: usize = 1 << 12;

impl Dominators {
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// How many points the tree holds, which the weights of its tops sum to.
    pub fn total(&self) -> u32 {
        self.nodes.len() as u32
    }

    /// The tops that are roots.
    pub fn roots(&self) -> Range<u32> {
        0..self.roots
    }

    /// How many counted points of the scope no root reaches, which the tree
    /// leaves out.
    pub fn outside(&self) -> u32 {
        self.outside
    }

    /// The tops that are no roots.
    pub fn shared_tops(&self) -> Range<u32> {
        self.roots..self.bounds[0]
    }

    /// How many points the [shared tops](Self::shared_tops) dominate between
    /// them, themselves among them.
    pub fn shared_weight(&self) -> u32 {
        self.shared_weight
    }

    pub fn children(&self, node: u32) -> Range<u32> {
        self.bounds[node as usize]..self.bounds[node as usize + 1]
    }

    /// The node immediately dominating `node`, absent for a top.
    pub fn parent(&self, node: u32) -> Option<u32> {
        let after = self.bounds.partition_point(|&start| start <= node);
        after.checked_sub(1).map(|parent| parent as u32)
    }

    pub fn id(&self, node: u32) -> Id {
        self.nodes[node as usize].id
    }

    pub fn kind(&self, node: u32) -> Kind {
        self.nodes[node as usize].kind
    }

    /// How many points `node` dominates, itself among them.
    pub fn weight(&self, node: u32) -> u32 {
        self.nodes[node as usize].weight
    }

    /// The node standing for the point `id`, if it is one of the tree.
    pub fn index_of(&self, id: Id) -> Option<u32> {
        self.index.get(id)
    }

    pub fn standing(&self, node: u32) -> Standing {
        self.nodes[node as usize].standing
    }

    /// Where `node` is a root of configured targets, another copy of its
    /// target the build reaches through a dependency.
    pub fn elsewhere(&self, node: u32) -> Option<Elsewhere> {
        self.elsewhere.get(node as usize).copied().flatten()
    }

    /// Up to `limit` roots depending on `node`, the nearest first, short of
    /// any beyond a root found. Where `node` is a root itself, the roots of
    /// its own cycle are walked through and not named, as its
    /// [standing](Standing::ReachedRoot) counts none of them. The walk up
    /// from `node` reaches at most [`ABOVE_WALK`] points, so where the roots
    /// lie past those, fewer are named than depend on it.
    pub fn roots_above(&self, node: u32, limit: usize) -> Vec<u32> {
        // A root is a top, which `cycle` is by.
        let cycle_of = |root: u32| self.cycle[root as usize];
        let own_cycle = self.standing(node).is_root().then(|| cycle_of(node));
        let named = |at: u32| self.standing(at).is_root() && Some(cycle_of(at)) != own_cycle;
        let mut found: Vec<u32> = Vec::with_capacity(limit);
        let mut seen: FxHashSet<u32> =
            FxHashSet::with_capacity_and_hasher(ABOVE_WALK, Default::default());
        let mut walk: VecDeque<u32> = VecDeque::with_capacity(ABOVE_WALK);
        seen.insert(node);
        walk.push_back(node);
        while let Some(at) = walk.pop_front() {
            for &dependent in self.dependents.of(at) {
                if found.len() == limit || seen.len() == ABOVE_WALK {
                    return found;
                }
                if !seen.insert(dependent) {
                    continue;
                }
                match named(dependent) {
                    true => found.push(dependent),
                    false => walk.push_back(dependent),
                }
            }
        }
        found
    }
}

/// How the points of a dominator tree group into the targets they are
/// configurations of, as [`Facets::label`] names them.
pub struct Targets {
    /// By node.
    held: Vec<Held>,
    /// By node, how many points of the tree are configurations of its target,
    /// itself among them; none where it names no target.
    copies: Vec<u32>,
}

/// The targets the points a node dominates are configurations of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Held {
    targets: u32,
    /// At most `targets`.
    only_here: u32,
}

impl Held {
    /// How many distinct targets.
    pub fn targets(self) -> u32 {
        self.targets
    }

    /// How many of those targets no point outside the subtree is a
    /// configuration of.
    pub fn only_here(self) -> u32 {
        self.only_here
    }
}

impl Targets {
    /// Each node's subtree is gathered into a count by target once, its
    /// children's counts merged smaller into larger, so the whole pass takes
    /// O(n log n) for a tree of n points.
    pub fn of(tree: &Dominators, facets: &Facets) -> Self {
        let n = tree.total() as usize;
        let label: Vec<Option<Id>> = (0..tree.total())
            .map(|node| facets.label(tree.id(node)))
            .collect();
        let mut copies_of: FxHashMap<Id, u32> =
            FxHashMap::with_capacity_and_hasher(n, Default::default());
        for &target in label.iter().flatten() {
            *copies_of.entry(target).or_default() += 1;
        }
        let mut gathered: Vec<Option<Gathered>> = Vec::with_capacity(n);
        gathered.resize_with(n, || None);
        let mut held = vec![
            Held {
                targets: 0,
                only_here: 0
            };
            n
        ];
        for node in (0..tree.total()).rev() {
            let mut children: Vec<Gathered> = tree
                .children(node)
                .filter_map(|child| gathered[child as usize].take())
                .collect();
            let largest = (0..children.len()).max_by_key(|&c| children[c].count.len());
            let mut into = match largest {
                Some(largest) => children.swap_remove(largest),
                None => Gathered::default(),
            };
            for child in children {
                for (target, count) in child.count {
                    into.add(target, count, copies_of[&target]);
                }
            }
            if let Some(target) = label[node as usize] {
                into.add(target, 1, copies_of[&target]);
            }
            held[node as usize] = Held {
                targets: into.count.len() as u32,
                only_here: into.whole,
            };
            gathered[node as usize] = Some(into);
        }
        let copies = label
            .iter()
            .map(|target| target.map_or(0, |target| copies_of[&target]))
            .collect();
        Self { held, copies }
    }

    pub fn held(&self, node: u32) -> Held {
        self.held[node as usize]
    }

    pub fn copies(&self, node: u32) -> u32 {
        self.copies[node as usize]
    }
}

/// The points of a subtree, counted by target.
#[derive(Default)]
struct Gathered {
    count: FxHashMap<Id, u32>,
    /// How many targets every point of which the subtree holds.
    whole: u32,
}

impl Gathered {
    /// Counts `count` more points of `target`, which has `copies` points in
    /// the tree.
    fn add(&mut self, target: Id, count: u32, copies: u32) {
        let counted = self.count.entry(target).or_default();
        *counted += count;
        self.whole += u32::from(*counted == copies);
    }
}

/// The points of one scope and the dependencies between them, with what a
/// dominator tree reads of them whatever its roots, so that trees of one
/// scope from different roots share it.
pub struct ScopeGraph {
    scope: Scope,
    unit: UnitGraph,
    /// By id, the point.
    index: DenseIds,
    /// By point, the hash of its name.
    keys: Vec<u64>,
    /// The strongly connected components of the points, each component's
    /// successors being those it depends on.
    condensation: Condensation,
    /// The top-level points, in ascending order.
    tops: Vec<u32>,
    counted: u32,
}

impl ScopeGraph {
    pub fn new(
        graph: &Graph,
        names: &Names,
        facets: &Facets,
        provenance: &Provenance,
        scope: Scope,
    ) -> Self {
        let unit = unit_graph(graph, facets, provenance, scope);
        let (index, _) = DenseIds::new(unit.points.iter().map(|point| point.id));
        let keys = unit
            .points
            .iter()
            .map(|point| stable_hash(names.resolve(point.id)))
            .collect();
        let condensation = Condensation::with_reverse(&unit.dependencies, &unit.dependents);
        let mut depended_on = vec![false; condensation.succ.len()];
        for &dependency in condensation.succ.iter().flatten() {
            depended_on[dependency as usize] = true;
        }
        let tops = (unit.points.iter().zip(&condensation.of).enumerate())
            .filter(|(_, (point, c))| point.counted && !depended_on[**c as usize])
            .map(|(p, _)| p as u32)
            .collect();
        let counted = unit.points.iter().filter(|point| point.counted).count() as u32;
        Self {
            scope,
            unit,
            index,
            keys,
            condensation,
            tops,
            counted,
        }
    }

    /// This graph where its points are the configured targets.
    pub fn configured_targets(&self) -> Option<ConfiguredTargets<'_>> {
        (self.scope.unit() == Unit::ConfiguredTarget).then_some(ConfiguredTargets(self))
    }

    /// This graph where its points are the actions.
    pub fn actions(&self) -> Option<Actions<'_>> {
        (self.scope.unit() == Unit::Action).then_some(Actions(self))
    }

    /// The dominator tree from `roots`, of which those that are no counted
    /// point of the scope are passed over.
    pub fn dominators(&self, names: &Names, facets: &Facets, roots: &Roots) -> Dominators {
        let unit = &self.unit;
        let roots: Vec<u32> = match roots {
            Roots::Tops => self.tops.clone(),
            Roots::Chosen(chosen) => {
                let mut points: Vec<u32> = Vec::with_capacity(chosen.ids().len());
                points.extend(
                    (chosen.ids().iter())
                        .filter_map(|&id| self.index.get(id))
                        .filter(|&p| unit.points[p as usize].counted),
                );
                points.sort_unstable();
                points
            }
        };
        let tree = Flowgraph {
            unit,
            roots: &roots,
        }
        .dominator_tree();
        let mut weight = vec![1u32; tree.idom.len()];
        for w in (1..tree.idom.len()).rev() {
            weight[tree.idom[w] as usize] += weight[w];
        }
        let standing = self.standing(&roots, &tree);

        let mut by_parent: Vec<u32> = (1..tree.idom.len() as u32).collect();
        bviz_parallel::workers().install(|| {
            by_parent.par_sort_unstable_by_key(|&w| {
                let p = tree.point(w);
                let idom = tree.idom[w as usize];
                let (key, name) = (
                    self.keys[p as usize],
                    names.resolve(unit.points[p as usize].id),
                );
                (
                    idom,
                    idom == TOP && roots.binary_search(&p).is_err(),
                    Reverse(weight[w as usize]),
                    key,
                    name,
                )
            })
        });
        // By preorder number, where its children start in `by_parent`.
        let mut first_child = vec![0u32; tree.idom.len() + 1];
        for &w in &by_parent {
            first_child[tree.idom[w as usize] as usize + 1] += 1;
        }
        for v in 1..first_child.len() {
            first_child[v] += first_child[v - 1];
        }
        let children = |v: u32| {
            &by_parent[first_child[v as usize] as usize..first_child[v as usize + 1] as usize]
        };

        let mut order: Vec<u32> = Vec::with_capacity(by_parent.len());
        let mut bounds: Vec<u32> = Vec::with_capacity(by_parent.len() + 1);
        order.extend_from_slice(children(TOP));
        for at in 0..by_parent.len() {
            bounds.push(order.len() as u32);
            order.extend_from_slice(children(order[at]));
        }
        bounds.push(order.len() as u32);

        let nodes: Vec<Dominated> = order
            .iter()
            .map(|&w| {
                let p = tree.point(w) as usize;
                let point = &unit.points[p];
                Dominated {
                    id: point.id,
                    kind: point.kind,
                    weight: weight[w as usize],
                    standing: standing(p),
                }
            })
            .collect();
        let (index, _) = DenseIds::new(nodes.iter().map(|node| node.id));
        let dependents = dependents(unit, &tree, &order);
        let tops = order[..bounds[0] as usize].iter();
        let cycle = tops
            .map(|&w| self.condensation.of[tree.point(w) as usize])
            .collect();
        let shared_tops = &nodes[roots.len()..bounds[0] as usize];
        let shared_weight = shared_tops.iter().map(|node| node.weight).sum();
        let elsewhere = match self.scope.unit() {
            Unit::ConfiguredTarget => {
                let copies = Copies::of(self, facets, &tree);
                let rooted = order[..roots.len()].iter();
                rooted.map(|&w| copies.elsewhere(tree.point(w))).collect()
            }
            Unit::Action | Unit::Target => Vec::new(),
        };
        Dominators {
            scope: self.scope,
            nodes,
            bounds,
            index,
            dependents,
            roots: roots.len() as u32,
            outside: self.counted - tree.points.len() as u32,
            shared_weight,
            cycle,
            elsewhere,
        }
    }

    /// By point `tree` reaches, where it stands towards `roots`, which are in
    /// ascending order.
    fn standing<'a>(
        &'a self,
        roots: &[u32],
        tree: &'a DominatorTree,
    ) -> impl Fn(usize) -> Standing + 'a {
        let unit = &self.unit;
        let component = &self.condensation.of;
        let components = self.condensation.succ.len();
        let mut is_root = vec![false; unit.points.len()];
        let mut roots_in = vec![0u32; components];
        for &p in roots {
            is_root[p as usize] = true;
            roots_in[component[p as usize] as usize] += 1;
        }
        let below = self.roots_reaching(roots, &is_root, ROOT_SET_WORDS);
        // By component, whether a point the tree reaches outside it depends
        // on a member, which only a point some root outside it reaches does.
        let mut entered = vec![false; components];
        for &q in &tree.points {
            let c = component[q as usize];
            for &d in &unit.dependencies[q as usize] {
                let entering = component[d as usize];
                entered[entering as usize] |= entering != c;
            }
        }
        move |p: usize| {
            let c = component[p] as usize;
            match (is_root[p], entered[c]) {
                (false, _) => Standing::Below(below[p]),
                (true, false) => Standing::Root {
                    in_cycle: (unit.dependents[p].iter()).any(|&q| tree.reached(q)),
                },
                (true, true) => {
                    let others = below[p].less(roots_in[c] - 1);
                    // An estimate can fall short of the root it knows of.
                    let others = match others.value() {
                        0 => Count::Estimated(1),
                        _ => others,
                    };
                    Standing::ReachedRoot(others)
                }
            }
        }
    }
}

impl ScopeGraph {
    /// By point, how many of `roots`, which `is_root` marks by point, reach
    /// it, other than itself. Each component carries the set of roots
    /// reaching it, handed on to the components it depends on in one pass,
    /// which counts exactly where the sets take at most `most_words` words in
    /// all; past that the counts are those [`reach_counts`] gives.
    fn roots_reaching(&self, roots: &[u32], is_root: &[bool], most_words: usize) -> Vec<Count> {
        let condensation = &self.condensation;
        let words = roots.len().div_ceil(WORD_BITS);
        let components = condensation.succ.len();
        if words * components > most_words {
            return reach_counts(&self.unit.dependents, |p| is_root[p], |p| self.keys[p]);
        }
        let mut sets = vec![0u64; words * components];
        for (bit, &p) in roots.iter().enumerate() {
            let c = condensation.of[p as usize] as usize;
            sets[c * words + bit / WORD_BITS] |= 1 << (bit % WORD_BITS);
        }
        let mut handed: Vec<u64> = vec![0; words];
        for &c in &condensation.order {
            let c = c as usize;
            handed.copy_from_slice(&sets[c * words..(c + 1) * words]);
            if handed.iter().all(|&word| word == 0) {
                continue;
            }
            for &d in &condensation.succ[c] {
                let d = d as usize;
                let into = &mut sets[d * words..(d + 1) * words];
                for (into, &from) in into.iter_mut().zip(&handed) {
                    *into |= from;
                }
            }
        }
        let reaching = |c: u32| {
            let set = &sets[c as usize * words..(c as usize + 1) * words];
            set.iter().map(|word| word.count_ones()).sum::<u32>()
        };
        (condensation.of.iter().zip(is_root))
            .map(|(&c, &root)| Count::Exact(reaching(c) - u32::from(root)))
            .collect()
    }
}

/// How many roots one word of a set of roots holds.
const WORD_BITS: usize = u64::BITS as usize;

/// The most words, in all, the sets of roots reaching the components of a
/// scope may take, which holds them to 32 MiB; past it the counts are
/// estimated.
const ROOT_SET_WORDS: usize = 1 << 22;

/// A [`ScopeGraph`] whose points are the configured targets.
#[derive(Clone, Copy)]
pub struct ConfiguredTargets<'a>(&'a ScopeGraph);

impl<'a> ConfiguredTargets<'a> {
    /// Whether the configured target `id` is top-level: one nothing depends
    /// on but the other members of a cycle it belongs to.
    pub fn is_top_level(self, id: Id) -> bool {
        let ScopeGraph { index, tops, .. } = self.0;
        index
            .get(id)
            .is_some_and(|p| tops.binary_search(&p).is_ok())
    }

    /// The configured targets `id` depends on directly.
    pub(crate) fn dependencies(self, id: Id) -> impl Iterator<Item = Id> + 'a {
        let ScopeGraph { unit, index, .. } = self.0;
        let point = index.get(id);
        let of = point.map_or(&[][..], |p| &unit.dependencies[p as usize]);
        of.iter().map(|&p| unit.points[p as usize].id)
    }
}

/// A [`ScopeGraph`] whose points are the actions.
#[derive(Clone, Copy)]
pub struct Actions<'a>(&'a ScopeGraph);

impl<'a> Actions<'a> {
    /// The actions reading what the action `id` writes.
    pub(crate) fn readers(self, id: Id) -> impl Iterator<Item = Id> + 'a {
        let ScopeGraph { unit, index, .. } = self.0;
        let point = index.get(id);
        let of = point.map_or(&[][..], |p| &unit.dependents[p as usize]);
        of.iter().map(|&p| unit.points[p as usize].id)
    }
}

/// The points of a tree that are configured copies of one target and that a
/// point of the tree depends on, by target.
struct Copies<'a> {
    scope: &'a ScopeGraph,
    facets: &'a Facets,
    tree: &'a DominatorTree,
    by_target: FxHashMap<Id, Depended>,
}

/// The copies of one target a point of the tree depends on: how many, and the
/// two first by [stable key](StableKeys), each with its first dependent, so
/// that one is left to name beside any one copy.
#[derive(Default)]
struct Depended {
    copies: u32,
    first: [Option<(u32, u32)>; 2],
}

impl<'a> Copies<'a> {
    fn of(scope: &'a ScopeGraph, facets: &'a Facets, tree: &'a DominatorTree) -> Self {
        let key = |p: u32| (scope.keys[p as usize], p);
        let mut by_target: FxHashMap<Id, Depended> =
            FxHashMap::with_capacity_and_hasher(tree.points.len(), Default::default());
        for &p in &tree.points {
            let Some(target) = facets.label(scope.unit.points[p as usize].id) else {
                continue;
            };
            let reached = (scope.unit.dependents[p as usize].iter())
                .copied()
                .filter(|&q| tree.reached(q));
            let Some(dependent) = reached.min_by_key(|&q| key(q)) else {
                continue;
            };
            let depended = by_target.entry(target).or_default();
            depended.copies += 1;
            let held = &mut depended.first;
            match *held {
                [Some(one), _] if key(p) < key(one.0) => *held = [Some((p, dependent)), held[0]],
                [Some(_), Some(two)] if key(p) >= key(two.0) => {}
                [Some(one), _] => *held = [Some(one), Some((p, dependent))],
                [None, _] => held[0] = Some((p, dependent)),
            }
        }
        Copies {
            scope,
            facets,
            tree,
            by_target,
        }
    }

    /// Another copy of the target `root` is a copy of that the tree depends
    /// on, `root` being a point the tree reaches.
    fn elsewhere(&self, root: u32) -> Option<Elsewhere> {
        let points = &self.scope.unit.points;
        let target = self.facets.label(points[root as usize].id)?;
        let depended = self.by_target.get(&target)?;
        let itself =
            (self.scope.unit.dependents[root as usize].iter()).any(|&q| self.tree.reached(q));
        let copies = depended.copies - u32::from(itself);
        let (copy, dependent) = depended
            .first
            .into_iter()
            .flatten()
            .find(|&(copy, _)| copy != root)?;
        Some(Elsewhere {
            copy: points[copy as usize].id,
            dependent: points[dependent as usize].id,
            copies,
        })
    }
}

/// The dependents of each point of `unit` that `tree` reaches, numbered as
/// the nodes `order` puts the preorder numbers of `tree` in.
fn dependents(unit: &UnitGraph, tree: &DominatorTree, order: &[u32]) -> Dependents {
    let mut node_of = vec![UNNUMBERED; unit.points.len()];
    for (node, &w) in order.iter().enumerate() {
        node_of[tree.point(w) as usize] = node as u32;
    }
    let point_dependents = |w: u32| &unit.dependents[tree.point(w) as usize];
    let edges = order.iter().map(|&w| point_dependents(w).len()).sum();
    let mut start: Vec<u32> = Vec::with_capacity(order.len() + 1);
    let mut of: Vec<u32> = Vec::with_capacity(edges);
    for &w in order {
        let from = of.len();
        start.push(from as u32);
        let reached = point_dependents(w)
            .iter()
            .map(|&q| node_of[q as usize])
            .filter(|&node| node != UNNUMBERED);
        of.extend(reached);
        of[from..].sort_unstable();
    }
    start.push(of.len() as u32);
    Dependents { start, of }
}

/// The preorder number of the top of the build, ahead of every point's.
const TOP: u32 = 0;
/// Stands for a point the walk from the top of the build does not number.
const UNNUMBERED: u32 = u32::MAX;
/// Stands for a vertex no ancestor is linked to yet.
const UNLINKED: u32 = u32::MAX;
/// Ends a bucket's list.
const NO_MORE: u32 = u32::MAX;

/// A unit graph's counted points beneath the top of the build, each edge
/// running from a point into one of its dependencies.
struct Flowgraph<'a> {
    unit: &'a UnitGraph,
    /// The points the top of the build has an edge to, in ascending order.
    roots: &'a [u32],
}

/// Each vertex's immediate dominator, by preorder number of a walk from the
/// top of the build.
struct DominatorTree {
    /// By point, its preorder number, [`UNNUMBERED`] for a point the walk
    /// does not reach.
    number: Vec<u32>,
    /// By preorder number, the point it numbers; the top of the build is none.
    points: Vec<u32>,
    /// By preorder number, the preorder number of its immediate dominator,
    /// always smaller.
    idom: Vec<u32>,
}

impl DominatorTree {
    fn point(&self, w: u32) -> u32 {
        self.points[w as usize - 1]
    }

    fn reached(&self, p: u32) -> bool {
        self.number[p as usize] != UNNUMBERED
    }
}

impl Flowgraph<'_> {
    fn is_root(&self, p: u32) -> bool {
        self.roots.binary_search(&p).is_ok()
    }

    fn successors(&self, points: &[u32], v: u32) -> &[u32] {
        match v {
            TOP => self.roots,
            _ => &self.unit.dependencies[points[v as usize - 1] as usize],
        }
    }

    /// The dominator tree by Lengauer and Tarjan's algorithm with simple
    /// path compression: T. Lengauer and R. E. Tarjan, "A Fast Algorithm for
    /// Finding Dominators in a Flowgraph", ACM TOPLAS 1(1), 1979.
    fn dominator_tree(&self) -> DominatorTree {
        let counted = |p: u32| self.unit.points[p as usize].counted;
        let most = 1 + self.unit.points.len();

        // Preorder numbers by an iterative walk: a dependency chain can run
        // deeper than a thread's stack.
        let mut number = vec![UNNUMBERED; self.unit.points.len()];
        let mut points: Vec<u32> = Vec::with_capacity(most - 1);
        let mut parent: Vec<u32> = Vec::with_capacity(most);
        parent.push(TOP);
        let mut walk: Vec<(u32, usize)> = Vec::with_capacity(most);
        walk.push((TOP, 0));
        while let Some((v, next)) = walk.last_mut() {
            let v = *v;
            let Some(&p) = self.successors(&points, v).get(*next) else {
                walk.pop();
                continue;
            };
            *next += 1;
            if counted(p) && number[p as usize] == UNNUMBERED {
                let w = parent.len() as u32;
                number[p as usize] = w;
                points.push(p);
                parent.push(v);
                walk.push((w, 0));
            }
        }
        let n = parent.len();

        let mut forest = Forest {
            semi: (0..n as u32).collect(),
            label: (0..n as u32).collect(),
            ancestor: vec![UNLINKED; n],
            path: Vec::new(),
        };
        let mut idom = vec![TOP; n];
        let mut bucket = vec![NO_MORE; n];
        let mut next_in_bucket = vec![NO_MORE; n];
        for w in (1..n as u32).rev() {
            let p = points[w as usize - 1];
            let dependents = self.unit.dependents[p as usize].iter();
            let reached = dependents
                .map(|&q| number[q as usize])
                .filter(|&v| v != UNNUMBERED);
            let from_top = self.is_root(p).then_some(TOP);
            for v in reached.chain(from_top) {
                let u = forest.eval(v);
                let semi = forest.semi[u as usize];
                if semi < forest.semi[w as usize] {
                    forest.semi[w as usize] = semi;
                }
            }
            let s = forest.semi[w as usize] as usize;
            next_in_bucket[w as usize] = bucket[s];
            bucket[s] = w;
            let pw = parent[w as usize];
            forest.ancestor[w as usize] = pw;
            let mut v = std::mem::replace(&mut bucket[pw as usize], NO_MORE);
            while v != NO_MORE {
                let u = forest.eval(v);
                idom[v as usize] = if forest.semi[u as usize] < forest.semi[v as usize] {
                    u
                } else {
                    pw
                };
                v = next_in_bucket[v as usize];
            }
        }
        for w in 1..n {
            if idom[w] != forest.semi[w] {
                idom[w] = idom[idom[w] as usize];
            }
        }
        DominatorTree {
            number,
            points,
            idom,
        }
    }
}

/// The forest Lengauer and Tarjan's algorithm links vertices into, by preorder
/// number, as it settles their semidominators.
struct Forest {
    semi: Vec<u32>,
    label: Vec<u32>,
    ancestor: Vec<u32>,
    /// Scratch for compressing a path.
    path: Vec<u32>,
}

impl Forest {
    /// Of the vertices on `v`'s path in the forest below its root, the one
    /// with the smallest semidominator.
    fn eval(&mut self, v: u32) -> u32 {
        if self.ancestor[v as usize] == UNLINKED {
            return v;
        }
        let mut at = v;
        while self.ancestor[self.ancestor[at as usize] as usize] != UNLINKED {
            self.path.push(at);
            at = self.ancestor[at as usize];
        }
        while let Some(y) = self.path.pop() {
            let a = self.ancestor[y as usize] as usize;
            let (la, ly) = (self.label[a], self.label[y as usize]);
            if self.semi[la as usize] < self.semi[ly as usize] {
                self.label[y as usize] = la;
            }
            self.ancestor[y as usize] = self.ancestor[a];
        }
        self.label[v as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Node};
    use crate::provenance::InputSet;

    /// Configured targets `deps` names, each pair running from a dependency to
    /// its dependent, in the order first named.
    pub(super) fn configured(deps: &[(&str, &str)]) -> (Graph, Names) {
        let mut graph = Graph::new();
        let mut names = Names::default();
        for name in deps.iter().flat_map(|&(a, b)| [a, b]) {
            if names.get(name).is_none() {
                let id = names.intern(name);
                graph.add_node(Node::new(id, name, Kind::ConfiguredTarget, None));
            }
        }
        for &(from, to) in deps {
            graph.add_edge(names.intern(from), names.intern(to), EdgeKind::Cquery);
        }
        (graph, names)
    }

    pub(super) fn tree_of(graph: &Graph, names: &Names, unit: Unit) -> Dominators {
        tree_with(graph, names, &Facets::default(), unit)
    }

    /// The nodes dominated by the top of the build alone.
    fn tops(tree: &Dominators) -> Range<u32> {
        0..tree.bounds[0]
    }

    /// Asserts that the tops of `tree` come in the two runs its type states.
    fn assert_tops_in_two_runs(tree: &Dominators) {
        let standing_so = (0..tree.total()).filter(|&node| tree.standing(node).is_root());
        assert_eq!(
            standing_so.collect::<Vec<_>>(),
            Vec::from_iter(tree.roots())
        );
        assert_eq!(tree.roots().end, tree.shared_tops().start);
        assert_eq!(tree.shared_tops().end, tops(tree).end);
        let weight = |node: u32| tree.weight(node);
        assert_eq!(
            tree.shared_weight(),
            tree.shared_tops().map(weight).sum::<u32>()
        );
        for run in [tree.roots(), tree.shared_tops()] {
            let weights: Vec<u32> = run.map(weight).collect();
            assert!(weights.is_sorted_by(|a, b| a >= b), "{weights:?}");
        }
    }

    fn tree_with(graph: &Graph, names: &Names, facets: &Facets, unit: Unit) -> Dominators {
        let everything = Provenance::new(vec![InputSet::ALL; names.len()]);
        let scope = Scope::new(unit, InputSet::ALL).unwrap();
        let prepared = ScopeGraph::new(graph, names, facets, &everything, scope);
        prepared.dominators(names, facets, &Roots::Tops)
    }

    /// The tree of configured targets from the points `roots` names.
    pub(super) fn tree_from(graph: &Graph, names: &Names, roots: &[&str]) -> Dominators {
        let everything = Provenance::new(vec![InputSet::ALL; names.len()]);
        let scope = Scope::new(Unit::ConfiguredTarget, InputSet::ALL).unwrap();
        let facets = Facets::default();
        let prepared = ScopeGraph::new(graph, names, &facets, &everything, scope);
        let roots = Chosen::new(roots.iter().map(|&root| names.get(root).unwrap()));
        prepared.dominators(names, &facets, &Roots::Chosen(roots))
    }

    /// Leaves of weight one named `names`, drawn in the order of their names'
    /// keys.
    fn leaves(names: &[&str]) -> String {
        let mut names = names.to_vec();
        names.sort_by_key(|name| stable_hash(name));
        let drawn: Vec<String> = names.iter().map(|name| format!("{name} 1")).collect();
        drawn.join(" ")
    }

    /// Each node as its name and weight, its children braced after it.
    fn drawn(tree: &Dominators, names: &Names) -> String {
        drawn_as(tree, &|id| names.resolve(id).to_string())
    }

    /// Each node as `label` names its point, and its weight, its children
    /// braced after it.
    fn drawn_as(tree: &Dominators, label: &dyn Fn(Id) -> String) -> String {
        fn draw(
            tree: &Dominators,
            label: &dyn Fn(Id) -> String,
            nodes: Range<u32>,
            out: &mut Vec<String>,
        ) {
            for node in nodes {
                out.push(format!("{} {}", label(tree.id(node)), tree.weight(node)));
                if !tree.children(node).is_empty() {
                    out.push("{".into());
                    draw(tree, label, tree.children(node), out);
                    out.push("}".into());
                }
            }
        }
        let mut out = Vec::new();
        draw(tree, label, tops(tree), &mut out);
        out.join(" ")
    }

    fn configured_tree(deps: &[(&str, &str)]) -> String {
        let (graph, names) = configured(deps);
        drawn(&tree_of(&graph, &names, Unit::ConfiguredTarget), &names)
    }

    #[test]
    fn each_link_of_a_chain_dominates_all_below_it() {
        assert_eq!(
            configured_tree(&[("lib", "bin"), ("bin", "test")]),
            "test 3 { bin 2 { lib 1 } }"
        );
    }

    #[test]
    fn a_dependency_two_sides_share_is_dominated_by_their_join() {
        assert_eq!(
            configured_tree(&[
                ("base", "left"),
                ("base", "right"),
                ("left", "app"),
                ("right", "app"),
            ]),
            format!("app 4 {{ {} }}", leaves(&["base", "left", "right"]))
        );
    }

    #[test]
    fn a_dependency_two_tops_share_is_a_top_itself() {
        assert_eq!(
            configured_tree(&[("base", "a"), ("base", "b"), ("only a", "a")]),
            "a 2 { only a 1 } b 1 base 1"
        );
    }

    /// The roots come ahead of the shared tops, however much either holds.
    #[test]
    fn the_tops_are_the_roots_and_then_those_they_share() {
        let (graph, names) = configured(&[
            ("base", "a"),
            ("base", "b"),
            ("under base", "base"),
            ("other", "a"),
            ("other", "b"),
        ]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        assert_eq!(
            drawn(&tree, &names),
            format!("{} base 2 {{ under base 1 }} other 1", leaves(&["a", "b"]))
        );
        assert_eq!((tree.roots(), tree.shared_tops()), (0..2, 2..4));
        assert_eq!(tree.shared_weight(), 3);
        let (graph, names) = configured(&[("lib", "bin")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        assert!(tree.shared_tops().is_empty());
        assert_eq!(tree.shared_weight(), 0);
    }

    /// Every member of a cycle at the top is a top: the build reaches each
    /// without passing through the other.
    #[test]
    fn each_member_of_a_cycle_at_the_top_is_a_top() {
        assert_eq!(
            configured_tree(&[("lib", "a"), ("a", "b"), ("b", "a")]),
            "a 2 { lib 1 } b 1"
        );
    }

    #[test]
    fn a_node_finds_its_way_up_to_the_top() {
        let (graph, names) = configured(&[("lib", "bin"), ("bin", "test"), ("tool", "test")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let lib = tree.index_of(names.get("lib").unwrap()).unwrap();
        let up = std::iter::successors(Some(lib), |&node| tree.parent(node));
        let path: Vec<&str> = up.map(|node| names.resolve(tree.id(node))).collect();
        assert_eq!(path, ["lib", "bin", "test"]);
    }

    pub(super) fn node(tree: &Dominators, names: &Names, name: &str) -> u32 {
        tree.index_of(names.get(name).unwrap()).unwrap()
    }

    /// A dependency two top-level points share stands among the tops as they
    /// do, but is depended on by both, and one below a top by that one alone.
    #[test]
    fn a_top_shared_by_top_level_points_counts_them() {
        let (graph, names) = configured(&[("base", "a"), ("base", "b"), ("only a", "a")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let standing = |name| tree.standing(node(&tree, &names, name));
        let alone = Standing::Root { in_cycle: false };
        assert_eq!(standing("a"), alone);
        assert_eq!(standing("b"), alone);
        assert_eq!(standing("base"), Standing::Below(Count::Exact(2)));
        assert_eq!(standing("only a"), Standing::Below(Count::Exact(1)));
    }

    /// Every member of a cycle at the top is top-level, though the others
    /// depend on it.
    #[test]
    fn every_member_of_a_cycle_at_the_top_is_top_level_in_a_cycle() {
        let (graph, names) = configured(&[("lib", "a"), ("a", "b"), ("b", "a")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        for name in ["a", "b"] {
            let standing = tree.standing(node(&tree, &names, name));
            assert_eq!(standing, Standing::Root { in_cycle: true }, "{name}");
        }
        let lib = tree.standing(node(&tree, &names, "lib"));
        assert_eq!(lib, Standing::Below(Count::Exact(2)));
    }

    /// The top-level points above a node are named nearest first, as many as
    /// asked for, passing over the points between that are not top-level.
    #[test]
    fn the_top_level_points_above_a_node_are_named_nearest_first() {
        let (graph, names) = configured(&[
            ("base", "near"),
            ("base", "mid"),
            ("mid", "far"),
            ("base", "x"),
            ("x", "far"),
        ]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let above = |limit| {
            let found = tree.roots_above(node(&tree, &names, "base"), limit);
            let named = found.into_iter().map(|n| names.resolve(tree.id(n)));
            named.collect::<Vec<_>>()
        };
        assert_eq!(above(5), ["near", "far"]);
        assert_eq!(above(1), ["near"]);
        assert_eq!(above(0), Vec::<&str>::new());
        let near = node(&tree, &names, "near");
        assert!(tree.roots_above(near, 5).is_empty());
    }

    /// However many points lie between a node and the top-level points
    /// above it, naming them walks through a bounded number, and names those
    /// it reaches.
    #[test]
    fn naming_the_top_level_points_above_a_node_walks_a_bounded_way() {
        let chain: Vec<String> = (0..ABOVE_WALK + 1).map(|i| format!("{i}")).collect();
        let mut deps: Vec<(&str, &str)> = chain
            .windows(2)
            .map(|pair| (pair[0].as_str(), pair[1].as_str()))
            .collect();
        deps.push(("0", "near"));
        let (graph, names) = configured(&deps);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let found = tree.roots_above(node(&tree, &names, "0"), 5);
        let named: Vec<&str> = found.iter().map(|&n| names.resolve(tree.id(n))).collect();
        assert_eq!(named, ["near"]);
        let base = tree.standing(node(&tree, &names, "0"));
        assert_eq!(base, Standing::Below(Count::Exact(2)));
    }

    /// `app` builds `lib` in its own configuration and a `tool` in the exec
    /// one, which builds `lib` there too: the tool's subtree holds one of the
    /// two configurations of `lib`, and all of the tool.
    #[test]
    fn a_subtree_counts_its_targets_and_those_configured_only_in_it() {
        let (graph, mut names) = configured(&[
            ("lib", "app"),
            ("tool (exec)", "app"),
            ("lib (exec)", "tool (exec)"),
        ]);
        let mut facets = Facets::default();
        for (configured, target) in [
            ("app", "//:app"),
            ("lib", "//:lib"),
            ("lib (exec)", "//:lib"),
            ("tool (exec)", "//:tool"),
        ] {
            let target = names.intern(target);
            facets.set_label(names.get(configured).unwrap(), target);
        }
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let targets = Targets::of(&tree, &facets);
        let held = |name| {
            let held = targets.held(node(&tree, &names, name));
            (held.targets(), held.only_here())
        };
        assert_eq!(held("app"), (3, 3));
        assert_eq!(held("tool (exec)"), (2, 1));
        assert_eq!(held("lib (exec)"), (1, 0));
        assert_eq!(targets.copies(node(&tree, &names, "lib (exec)")), 2);
        assert_eq!(targets.copies(node(&tree, &names, "app")), 1);
    }

    /// On random trees whose points are configurations of a few targets,
    /// each subtree counts what enumerating it finds.
    #[test]
    fn random_subtrees_count_the_targets_enumerating_them_finds() {
        const TARGETS: usize = 6;
        let mut next = random_indices();
        for (nodes, edges) in [(30, 45), (120, 160), (120, 300)] {
            let deps: Vec<(usize, usize)> = (0..edges)
                .map(|_| {
                    let (a, b) = (next(nodes), next(nodes));
                    (a.max(b), a.min(b))
                })
                .collect();
            let (graph, mut names) =
                random_build(Unit::ConfiguredTarget, nodes, &deps, 0, &mut next);
            let mut facets = Facets::default();
            let targets: Vec<Id> = (0..TARGETS)
                .map(|t| names.intern(&format!("//:t{t}")))
                .collect();
            for p in 0..nodes {
                facets.set_label(Id(p as u32), targets[next(TARGETS)]);
            }
            let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
            let counted = Targets::of(&tree, &facets);
            let label = |node: u32| facets.label(tree.id(node)).unwrap();
            let all: Vec<Id> = (0..tree.total()).map(label).collect();
            for node in 0..tree.total() {
                let mut subtree = vec![node];
                let mut at = 0;
                while let Some(&v) = subtree.get(at) {
                    subtree.extend(tree.children(v));
                    at += 1;
                }
                let mut held: Vec<Id> = subtree.iter().map(|&v| label(v)).collect();
                held.sort_unstable();
                let in_subtree = |t: Id| held.iter().filter(|&&h| h == t).count();
                let in_tree = |t: Id| all.iter().filter(|&&h| h == t).count();
                let mut distinct = held.clone();
                distinct.dedup();
                let only_here = distinct
                    .iter()
                    .filter(|&&t| in_subtree(t) == in_tree(t))
                    .count() as u32;
                let found = counted.held(node);
                assert_eq!(
                    (found.targets(), found.only_here()),
                    (distinct.len() as u32, only_here)
                );
                assert_eq!(counted.copies(node), in_tree(label(node)) as u32);
            }
        }
    }

    #[test]
    fn a_point_naming_no_target_counts_none() {
        let (graph, names) = configured(&[("lib", "app")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let targets = Targets::of(&tree, &Facets::default());
        let app = node(&tree, &names, "app");
        assert_eq!((targets.held(app).targets(), targets.copies(app)), (0, 0));
    }

    /// A tree from chosen roots holds what they reach, and counts the rest of
    /// the scope as outside it.
    #[test]
    fn a_tree_from_chosen_roots_holds_what_they_reach() {
        let (graph, names) = configured(&[
            ("lib", "bin"),
            ("bin", "test"),
            ("tool", "test"),
            ("other", "x"),
        ]);
        let tree = tree_from(&graph, &names, &["bin"]);
        assert_eq!(drawn(&tree, &names), "bin 2 { lib 1 }");
        assert_eq!((tree.roots(), tree.outside()), (0..1, 4));
        assert_eq!(tree.index_of(names.get("test").unwrap()), None);
        let bin = node(&tree, &names, "bin");
        assert!(tree.roots_above(node(&tree, &names, "lib"), 5) == [bin]);
    }

    /// A root another root reaches is a top all the same, since it was asked
    /// for, and is known to be reached; a dependency the two share is
    /// counted as theirs.
    #[test]
    fn a_root_another_root_reaches_stays_a_top() {
        let (graph, names) = configured(&[("lib", "app"), ("app", "test"), ("base", "lib")]);
        let tree = tree_from(&graph, &names, &["test", "app"]);
        assert_eq!(drawn(&tree, &names), "app 3 { lib 2 { base 1 } } test 1");
        assert_eq!((tree.roots(), tree.shared_tops()), (0..2, 2..2));
        let standing = |name| tree.standing(node(&tree, &names, name));
        assert_eq!(standing("app"), Standing::ReachedRoot(Count::Exact(1)));
        assert_eq!(standing("test"), Standing::Root { in_cycle: false });
        assert_eq!(standing("lib"), Standing::Below(Count::Exact(2)));
        let app = node(&tree, &names, "app");
        let test = node(&tree, &names, "test");
        assert_eq!(tree.roots_above(app, 5), [test]);
        assert_eq!(tree.roots_above(node(&tree, &names, "base"), 5), [app]);
    }

    /// Roots in one cycle reach each other, which makes neither reached from
    /// beyond it; a root outside the cycle reaching it does, and is the one
    /// named above each.
    #[test]
    fn roots_in_one_cycle_are_reached_only_from_beyond_it() {
        let (graph, names) = configured(&[("a", "b"), ("b", "a"), ("a", "c")]);
        let tree = tree_from(&graph, &names, &["a", "b"]);
        for name in ["a", "b"] {
            let standing = tree.standing(node(&tree, &names, name));
            assert_eq!(standing, Standing::Root { in_cycle: true }, "{name}");
        }
        let tree = tree_from(&graph, &names, &["a", "b", "c"]);
        for name in ["a", "b"] {
            let standing = tree.standing(node(&tree, &names, name));
            assert_eq!(standing, Standing::ReachedRoot(Count::Exact(1)), "{name}");
            let above = tree.roots_above(node(&tree, &names, name), 5);
            assert_eq!(above, [node(&tree, &names, "c")], "{name}");
        }
    }

    /// The top-level points as chosen roots give the tree the top-level
    /// points do.
    #[test]
    fn the_top_level_points_chosen_give_the_tree_from_the_tops() {
        let (graph, names) = configured(&[("base", "a"), ("base", "b"), ("only a", "a")]);
        let chosen = tree_from(&graph, &names, &["a", "b"]);
        let tops = tree_of(&graph, &names, Unit::ConfiguredTarget);
        assert_eq!(drawn(&chosen, &names), drawn(&tops, &names));
        assert_eq!((chosen.roots(), chosen.outside()), (0..2, 0));
        let standing = |tree: &Dominators| {
            (0..tree.total())
                .map(|node| tree.standing(node))
                .collect::<Vec<_>>()
        };
        assert_eq!(standing(&chosen), standing(&tops));
    }

    /// Sets naming the same points are one key, however each was gathered.
    #[test]
    fn chosen_points_named_alike_are_equal_and_hash_alike() {
        let hash = |chosen: &Chosen| FxBuildHasher.hash_one(chosen);
        let (one, other) = (Chosen::new([Id(3), Id(1)]), Chosen::new([1, 3, 3].map(Id)));
        assert_eq!(one, other);
        assert_eq!(hash(&one), hash(&other));
        assert_eq!(one, one.clone());
        assert_ne!(one, Chosen::new([Id(1)]));
    }

    #[test]
    fn a_root_that_is_no_point_of_the_scope_is_passed_over() {
        let (mut graph, mut names) = configured(&[("lib", "app")]);
        let target = names.intern("target");
        graph.add_node(Node::new(target, "target", Kind::Target, None));
        let tree = tree_from(&graph, &names, &["target"]);
        assert_eq!((tree.total(), tree.roots(), tree.outside()), (0, 0..0, 2));
    }

    /// A build of `cargo` at the top, beside the copy of `cargo` in the exec
    /// configuration a wrapper builds: the copy at the top is a root the
    /// build also reaches, as a dependency of the wrapper, in another
    /// configuration.
    fn cargo() -> (Graph, Names, Facets) {
        let (graph, mut names) = configured(&[
            ("cargo (exec)", "wrapper"),
            ("cargo (exec)", "other wrapper"),
            ("lib", "cargo"),
            ("lib", "lonely"),
        ]);
        let mut facets = Facets::default();
        for (configured, target) in [
            ("cargo", "//:cargo"),
            ("cargo (exec)", "//:cargo"),
            ("wrapper", "//:wrapper"),
            ("other wrapper", "//:wrapper"),
            ("lib", "//:lib"),
            ("lonely", "//:lonely"),
        ] {
            let target = names.intern(target);
            facets.set_label(names.get(configured).unwrap(), target);
        }
        (graph, names, facets)
    }

    #[test]
    fn a_root_whose_target_the_tree_depends_on_elsewhere_is_flagged() {
        let (graph, names, facets) = cargo();
        let tree = tree_with(&graph, &names, &facets, Unit::ConfiguredTarget);
        let cargo = node(&tree, &names, "cargo");
        let flagged = tree.elsewhere(cargo).expect("cargo reached elsewhere");
        assert_eq!(names.resolve(flagged.copy), "cargo (exec)");
        assert!(["wrapper", "other wrapper"].contains(&names.resolve(flagged.dependent)));
        assert_eq!(flagged.copies, 1);
        for name in ["wrapper", "other wrapper", "lonely", "lib"] {
            assert_eq!(tree.elsewhere(node(&tree, &names, name)), None, "{name}");
        }
    }

    /// A copy outside the tree is no part of the build the tree stands for.
    #[test]
    fn a_copy_outside_the_tree_reaches_no_root() {
        let (graph, names, facets) = cargo();
        let everything = Provenance::new(vec![InputSet::ALL; names.len()]);
        let scope = Scope::new(Unit::ConfiguredTarget, InputSet::ALL).unwrap();
        let prepared = ScopeGraph::new(&graph, &names, &facets, &everything, scope);
        let roots = Chosen::new(["cargo", "lonely"].map(|name| names.get(name).unwrap()));
        let tree = prepared.dominators(&names, &facets, &Roots::Chosen(roots));
        assert_eq!(tree.elsewhere(node(&tree, &names, "cargo")), None);
    }

    /// Two copies of one target both at the top are two roots, neither of
    /// which the build reaches through a dependency.
    #[test]
    fn copies_of_one_target_all_at_the_top_flag_no_root() {
        let (graph, mut names) = configured(&[("lib", "a (one)"), ("lib", "a (two)")]);
        let mut facets = Facets::default();
        let target = names.intern("//:a");
        for copy in ["a (one)", "a (two)"] {
            facets.set_label(names.get(copy).unwrap(), target);
        }
        let tree = tree_with(&graph, &names, &facets, Unit::ConfiguredTarget);
        for copy in ["a (one)", "a (two)"] {
            assert_eq!(tree.elsewhere(node(&tree, &names, copy)), None, "{copy}");
        }
    }

    /// The roots reaching each point, counted through sets of roots or by
    /// [`reach_counts`] where the sets would grow too large, come to the same
    /// on graphs small enough for both to count exactly.
    #[test]
    fn the_roots_reaching_a_point_count_alike_either_way() {
        let mut next = random_indices();
        for (nodes, edges, roots) in [(30, 45, 3), (120, 300, 70), (120, 160, 120)] {
            let deps: Vec<(usize, usize)> =
                (0..edges).map(|_| (next(nodes), next(nodes))).collect();
            let (graph, names) = random_build(Unit::ConfiguredTarget, nodes, &deps, 0, &mut next);
            let everything = Provenance::new(vec![InputSet::ALL; names.len()]);
            let scope = Scope::new(Unit::ConfiguredTarget, InputSet::ALL).unwrap();
            let prepared = ScopeGraph::new(&graph, &names, &Facets::default(), &everything, scope);
            let mut chosen: Vec<u32> = (0..roots).map(|_| next(nodes) as u32).collect();
            chosen.sort_unstable();
            chosen.dedup();
            let mut is_root = vec![false; nodes];
            for &p in &chosen {
                is_root[p as usize] = true;
            }
            let by_sets = prepared.roots_reaching(&chosen, &is_root, usize::MAX);
            let counted = prepared.roots_reaching(&chosen, &is_root, 0);
            assert_eq!(by_sets, counted, "{nodes} nodes, {edges} edges");
        }
    }

    #[test]
    fn children_go_heaviest_first_and_by_key_among_equals() {
        assert_eq!(
            configured_tree(&[
                ("z", "app"),
                ("b", "app"),
                ("deep", "y"),
                ("y", "app"),
                ("a", "app"),
            ]),
            format!("app 6 {{ y 2 {{ deep 1 }} {} }}", leaves(&["a", "b", "z"]))
        );
    }

    #[test]
    fn source_files_take_no_part_in_the_tree_of_actions() {
        let mut graph = Graph::new();
        let mut names = Names::default();
        let mut node = |name: &str, kind| {
            let id = names.intern(name);
            graph.add_node(Node::new(id, name, kind, None));
            id
        };
        let (compile, link) = (node("compile", Kind::Action), node("link", Kind::Action));
        let (source, object) = (node("lib.c", Kind::Source), node("lib.o", Kind::Artifact));
        graph.add_edge(source, compile, EdgeKind::Aquery);
        graph.add_edge(compile, object, EdgeKind::Aquery);
        graph.add_edge(object, link, EdgeKind::Aquery);
        graph.add_edge(source, link, EdgeKind::Aquery);
        let tree = tree_of(&graph, &names, Unit::Action);
        assert_eq!(drawn(&tree, &names), "link 2 { compile 1 }");
        assert_eq!(tree.index_of(source), None);
        assert_eq!(tree.kind(0), Kind::Action);
    }

    /// Actions named, as an aquery names them, by the file each writes, listed
    /// in `order` with the files each reads.
    fn listed_actions(
        actions: &[(&str, &[&str])],
        order: &[usize],
        edges_reversed: bool,
    ) -> (Graph, Names) {
        let mut graph = Graph::new();
        let mut names = Names::default();
        let mut edges: Vec<(Id, Id)> = Vec::new();
        let file = |name: &str, graph: &mut Graph, names: &mut Names| {
            names.get(name).unwrap_or_else(|| {
                let written = actions.iter().any(|&(output, _)| output == name);
                let kind = if written {
                    Kind::Artifact
                } else {
                    Kind::Source
                };
                let id = names.intern(name);
                graph.add_node(Node::new(id, name, kind, None));
                id
            })
        };
        for &a in order {
            let (output, inputs) = actions[a];
            let action = names.intern(&format!("action:{output}"));
            graph.add_node(Node::new(action, "Action", Kind::Action, None));
            let written = file(output, &mut graph, &mut names);
            edges.push((action, written));
            for input in inputs {
                edges.push((file(input, &mut graph, &mut names), action));
            }
        }
        if edges_reversed {
            edges.reverse();
        }
        for (from, to) in edges {
            graph.add_edge(from, to, EdgeKind::Aquery);
        }
        (graph, names)
    }

    /// Siblings of equal weight keep their order however the aquery lists the
    /// actions and their edges.
    #[test]
    fn siblings_keep_their_order_however_the_actions_are_listed() {
        let actions: &[(&str, &[&str])] = &[
            ("a.o", &["a.c"]),
            ("b.o", &["b.c"]),
            ("c.o", &["c.c"]),
            ("d.o", &["d.c", "c.o"]),
            ("app", &["a.o", "b.o", "d.o", "e.o"]),
            ("e.o", &["e.c"]),
            ("gen1", &["gen.py"]),
            ("gen2", &["gen.py"]),
            ("gen3", &[]),
        ];
        let tree_listed = |order: &[usize], edges_reversed: bool| {
            let (graph, names) = listed_actions(actions, order, edges_reversed);
            let tree = tree_with(&graph, &names, &Facets::default(), Unit::Action);
            drawn(&tree, &names)
        };
        let forward: Vec<usize> = (0..actions.len()).collect();
        let shuffled = [4, 8, 1, 6, 3, 0, 7, 5, 2];
        let drawn = tree_listed(&forward, false);
        let compiled = leaves(&["action:a.o", "action:b.o", "action:e.o"]);
        let generated = leaves(&["action:gen1", "action:gen2", "action:gen3"]);
        assert_eq!(
            drawn,
            format!("action:app 6 {{ action:d.o 2 {{ action:c.o 1 }} {compiled} }} {generated}")
        );
        assert_eq!(tree_listed(&shuffled, true), drawn);
        assert_eq!(
            tree_listed(&forward.iter().rev().copied().collect::<Vec<_>>(), true),
            drawn
        );
    }

    #[test]
    fn a_chain_deeper_than_a_stack_hangs_each_link_beneath_the_next() {
        const LINKS: usize = 200_000;
        let names: Vec<String> = (0..LINKS).map(|i| format!("{i}")).collect();
        let deps: Vec<(&str, &str)> = names
            .windows(2)
            .map(|pair| (pair[0].as_str(), pair[1].as_str()))
            .collect();
        let (graph, names) = configured(&deps);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let bottom = tree.index_of(names.get("0").unwrap()).unwrap();
        assert_eq!(tree.weight(bottom), 1);
        assert_eq!(
            tops(&tree).map(|top| tree.weight(top)).sum::<u32>(),
            LINKS as u32
        );
        let up = std::iter::successors(Some(bottom), |&node| tree.parent(node));
        assert_eq!(up.count(), LINKS);
    }

    #[test]
    fn an_empty_scope_has_an_empty_tree() {
        let tree = tree_of(&Graph::new(), &Names::default(), Unit::ConfiguredTarget);
        assert_eq!((tree.total(), tops(&tree)), (0, 0..0));
    }

    /// A xorshift generator of indices below a bound, so a randomized test is
    /// the same on every run.
    pub(super) fn random_indices() -> impl FnMut(usize) -> usize {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        move |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        }
    }

    /// The points `from` reaches over `succ`, itself among them, never passing
    /// through `removed`.
    fn reached(succ: &[Vec<usize>], from: &[usize], removed: Option<usize>) -> Vec<bool> {
        let mut seen = vec![false; succ.len()];
        let mut walk: Vec<usize> = from
            .iter()
            .copied()
            .filter(|&p| Some(p) != removed)
            .collect();
        for &p in &walk {
            seen[p] = true;
        }
        while let Some(p) = walk.pop() {
            for &q in &succ[p] {
                if Some(q) != removed && !seen[q] {
                    seen[q] = true;
                    walk.push(q);
                }
            }
        }
        seen
    }

    /// By point of the graph `succ` runs from dependents into dependencies,
    /// its immediate dominator by the definition, absent beneath the top of
    /// the build alone.
    fn naive_idoms(succ: &[Vec<usize>]) -> Vec<Option<usize>> {
        let n = succ.len();
        let reaches: Vec<Vec<bool>> = (0..n).map(|p| reached(succ, &[p], None)).collect();
        // A top is depended on by nothing it does not depend on in turn.
        let tops: Vec<usize> = (0..n)
            .filter(|&p| (0..n).all(|q| !reaches[q][p] || reaches[p][q]))
            .collect();
        naive_idoms_from(succ, &tops)
    }

    /// By point of the graph `succ` runs from dependents into dependencies,
    /// its immediate dominator by the definition below a top of the build
    /// standing above `roots`, absent beneath that top alone and where
    /// `roots` do not reach it.
    fn naive_idoms_from(succ: &[Vec<usize>], roots: &[usize]) -> Vec<Option<usize>> {
        let n = succ.len();
        let reached_at_all = reached(succ, roots, None);
        let dominated: Vec<Vec<bool>> = (0..n)
            .map(|d| {
                let left = reached(succ, roots, Some(d));
                (0..n)
                    .map(|v| reached_at_all[v] && v != d && !left[v])
                    .collect()
            })
            .collect();
        (0..n)
            .map(|v| {
                let strict: Vec<usize> = (0..n).filter(|&d| dominated[d][v]).collect();
                strict
                    .iter()
                    .copied()
                    .find(|&d| strict.iter().all(|&e| e == d || dominated[e][d]))
            })
            .collect()
    }

    /// The points `0..nodes`, named `n{p}` and numbered alike, with `deps`
    /// between them, each running from a dependency to its dependent: in the
    /// Action unit, actions linked through the files they write, each also
    /// reading one of `sources` source files at random.
    pub(super) fn random_build(
        unit: Unit,
        nodes: usize,
        deps: &[(usize, usize)],
        sources: usize,
        next: &mut impl FnMut(usize) -> usize,
    ) -> (Graph, Names) {
        let mut graph = Graph::new();
        let mut names = Names::default();
        let kind = match unit {
            Unit::Action => Kind::Action,
            _ => Kind::ConfiguredTarget,
        };
        let mut node = |name: String, kind| {
            let id = names.intern(&name);
            graph.add_node(Node::new(id, name, kind, None));
            id
        };
        let points: Vec<Id> = (0..nodes).map(|p| node(format!("n{p}"), kind)).collect();
        let mut edges: Vec<(Id, Id)> = Vec::with_capacity(deps.len() + 2 * nodes);
        match unit {
            Unit::Action => {
                let sources: Vec<Id> = (0..sources)
                    .map(|s| node(format!("s{s}"), Kind::Source))
                    .collect();
                let mut outputs: Vec<Id> = Vec::with_capacity(nodes);
                for (p, &action) in points.iter().enumerate() {
                    let output = node(format!("o{p}"), Kind::Artifact);
                    outputs.push(output);
                    edges.push((action, output));
                    edges.push((sources[next(sources.len())], action));
                }
                edges.extend(deps.iter().map(|&(from, to)| (outputs[from], points[to])));
            }
            _ => edges.extend(deps.iter().map(|&(from, to)| (points[from], points[to]))),
        }
        let edge_kind = match unit {
            Unit::Action => EdgeKind::Aquery,
            _ => EdgeKind::Cquery,
        };
        for (from, to) in edges {
            graph.add_edge(from, to, edge_kind);
        }
        (graph, names)
    }

    /// Random graphs with cycles among their points, at their top among them,
    /// give each point the immediate dominator the definition does, weigh
    /// each node by what it dominates, and order their tops as the tree's
    /// type states; among actions, the source files they read take no part.
    #[test]
    fn random_graphs_dominate_as_the_definition_does() {
        const SOURCES: usize = 5;
        let mut next = random_indices();
        for unit in [Unit::ConfiguredTarget, Unit::Action] {
            for (nodes, edges) in [(8, 10), (30, 45), (120, 160), (120, 300)] {
                for _ in 0..10 {
                    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); nodes];
                    let mut deps: Vec<(usize, usize)> = Vec::with_capacity(edges);
                    for _ in 0..edges {
                        let (a, b) = (next(nodes), next(nodes));
                        let (dependent, dependency) = match next(8) {
                            0 => (a.max(b), a.min(b)),
                            _ => (a.min(b), a.max(b)),
                        };
                        succ[dependent].push(dependency);
                        deps.push((dependency, dependent));
                    }
                    let (graph, names) = random_build(unit, nodes, &deps, SOURCES, &mut next);
                    let tree = tree_of(&graph, &names, unit);
                    let point =
                        |node: u32| names.resolve(tree.id(node))[1..].parse::<usize>().unwrap();

                    let found: Vec<Option<usize>> = (0..nodes)
                        .map(|p| {
                            let node = tree.index_of(Id(p as u32)).unwrap();
                            tree.parent(node).map(point)
                        })
                        .collect();
                    let idoms = naive_idoms(&succ);
                    assert_eq!(found, idoms, "{unit:?}: {nodes} nodes, {edges} edges");

                    let weighed = tops(&tree).map(|top| tree.weight(top)).sum::<u32>();
                    assert_eq!(weighed, nodes as u32);
                    assert_eq!(tree.total(), nodes as u32);
                    assert_tops_in_two_runs(&tree);
                    for node in 0..tree.total() {
                        let children = tree.children(node);
                        assert!(
                            children
                                .clone()
                                .all(|child| tree.parent(child) == Some(node))
                        );
                        let below: u32 = children.map(|child| tree.weight(child)).sum();
                        assert_eq!(tree.weight(node), below + 1);
                    }
                }
            }
        }
    }

    /// Random graphs from random roots give each point they reach the
    /// immediate dominator the definition does below a top standing above
    /// those roots, and leave out the rest; each root stands towards the
    /// others as the roots reaching it say, and the tops come ordered as the
    /// tree's type states.
    #[test]
    fn random_root_sets_dominate_as_the_definition_does() {
        const SOURCES: usize = 5;
        const MOST_ROOTS: usize = 4;
        let mut next = random_indices();
        for unit in [Unit::ConfiguredTarget, Unit::Action] {
            for (nodes, edges) in [(8, 10), (30, 45), (120, 160), (120, 300)] {
                for _ in 0..10 {
                    let mut succ: Vec<Vec<usize>> = vec![Vec::new(); nodes];
                    let mut deps: Vec<(usize, usize)> = Vec::with_capacity(edges);
                    for _ in 0..edges {
                        let (a, b) = (next(nodes), next(nodes));
                        let (dependent, dependency) = match next(8) {
                            0 => (a.max(b), a.min(b)),
                            _ => (a.min(b), a.max(b)),
                        };
                        succ[dependent].push(dependency);
                        deps.push((dependency, dependent));
                    }
                    let (graph, names) = random_build(unit, nodes, &deps, SOURCES, &mut next);
                    let roots: Vec<usize> =
                        (0..1 + next(MOST_ROOTS)).map(|_| next(nodes)).collect();
                    let everything = Provenance::new(vec![InputSet::ALL; names.len()]);
                    let scope = Scope::new(unit, InputSet::ALL).unwrap();
                    let facets = Facets::default();
                    let prepared = ScopeGraph::new(&graph, &names, &facets, &everything, scope);
                    let chosen = Chosen::new(roots.iter().map(|&p| Id(p as u32)));
                    let tree = prepared.dominators(&names, &facets, &Roots::Chosen(chosen));
                    let point =
                        |node: u32| names.resolve(tree.id(node))[1..].parse::<usize>().unwrap();

                    let reached = reached(&succ, &roots, None);
                    let found: Vec<Option<usize>> = (0..nodes)
                        .map(|p| {
                            let node = tree.index_of(Id(p as u32));
                            assert_eq!(node.is_some(), reached[p], "{p} in the tree");
                            tree.parent(node?).map(point)
                        })
                        .collect();
                    let idoms = naive_idoms_from(&succ, &roots);
                    let case = format!("{unit:?}: {nodes} nodes, {edges} edges, roots {roots:?}");
                    assert_eq!(found, idoms, "{case}");

                    let inside = reached.iter().filter(|&&r| r).count() as u32;
                    assert_eq!(tree.total(), inside, "{case}");
                    assert_eq!(tree.outside(), nodes as u32 - inside, "{case}");
                    let weighed = tops(&tree).map(|top| tree.weight(top)).sum::<u32>();
                    assert_eq!(weighed, inside, "{case}");
                    assert_tops_in_two_runs(&tree);
                    for node in 0..tree.total() {
                        let below: u32 = tree.children(node).map(|c| tree.weight(c)).sum();
                        assert_eq!(tree.weight(node), below + 1, "{case}");
                    }

                    let reaches: Vec<Vec<bool>> = (0..nodes)
                        .map(|p| self::reached(&succ, &[p], None))
                        .collect();
                    let mut distinct = roots.clone();
                    distinct.sort_unstable();
                    distinct.dedup();
                    for node in 0..tree.total() {
                        let p = point(node);
                        let reaching = |cycle_too: bool| {
                            let others = distinct.iter().filter(|&&q| q != p && reaches[q][p]);
                            others.filter(|&&q| cycle_too || !reaches[p][q]).count() as u32
                        };
                        let expected = match distinct.binary_search(&p).is_ok() {
                            false => Standing::Below(Count::Exact(reaching(true))),
                            true => match reaching(false) {
                                0 => Standing::Root {
                                    in_cycle: (0..nodes)
                                        .any(|q| q != p && reached[q] && succ[q].contains(&p)),
                                },
                                others => Standing::ReachedRoot(Count::Exact(others)),
                            },
                        };
                        assert_eq!(tree.standing(node), expected, "{case}: n{p}");
                    }
                }
            }
        }
    }
}
