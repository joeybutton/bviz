//! What exists in a build only because of each of its units: the dominator
//! tree of a scope's points, walked from each point into its dependencies.
//!
//! A point dominates another when every path from the top of the build to the
//! other passes through it, so a build without the point leaves out every
//! point it dominates. The top of the build stands above every point no
//! other depends on, and above each cycle no point outside it depends on, so
//! every counted point of the scope sits in the tree.

use std::cmp::Reverse;
use std::collections::VecDeque;
use std::ops::Range;

use rayon::prelude::*;
use rustc_hash::FxHashSet;

use crate::facets::Facets;
use crate::graph::{Graph, Kind};
use crate::intern::{Id, Names, stable_hash};
use crate::metrics::{Count, Scope, UnitGraph, unit_graph};
use crate::provenance::Provenance;
use crate::reduce::{DenseIds, components, reach_counts};

/// The dominator tree of the counted points of a scope. A point not counted
/// towards others is no product of the build and takes no part.
///
/// The children of each node, and the tops of the tree, take consecutive node
/// indices, heaviest first and among equal weights by a hash of their points'
/// names, an order every run of the queries gives alike, then by name where two
/// hashes collide.
pub struct Dominators {
    scope: Scope,
    nodes: Vec<Dominated>,
    /// By node, the index its children start at, and past the last node the
    /// node count, so each node's children end where the next node's start
    /// and the tops take every index ahead of the first node's children.
    bounds: Vec<u32>,
    index: DenseIds,
    dependents: Dependents,
}

struct Dominated {
    id: Id,
    kind: Kind,
    weight: u32,
    standing: Standing,
}

/// Where a point stands towards the top-level points of its scope: those
/// nothing depends on but the other members of a cycle they belong to. Every
/// top-level point is a top of the tree; a top that is not one is a
/// dependency several top-level points share.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// Depended on only by the other members of its cycle where `in_cycle`,
    /// and otherwise by nothing.
    TopLevel { in_cycle: bool },
    /// Depended on, directly or transitively, by this many top-level points.
    Below(Count),
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

/// The most points [`Dominators::top_level_above`] reaches on its walk up
/// from a node, however large the graph; each point it walks through has
/// every one of its dependents scanned.
const ABOVE_WALK: usize = 1 << 12;

impl Dominators {
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// How many points the tree holds, which the weights of its tops sum to.
    pub fn total(&self) -> u32 {
        self.nodes.len() as u32
    }

    /// The nodes dominated by the top of the build alone.
    pub fn tops(&self) -> Range<u32> {
        0..self.bounds[0]
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

    /// Up to `limit` top-level points depending on `node`, the nearest first.
    /// The walk up from `node` reaches at most [`ABOVE_WALK`] points, so where
    /// the top-level points lie past those, fewer are named than depend on it.
    pub fn top_level_above(&self, node: u32, limit: usize) -> Vec<u32> {
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
                match self.standing(dependent) {
                    Standing::TopLevel { .. } => found.push(dependent),
                    Standing::Below(_) => walk.push_back(dependent),
                }
            }
        }
        found
    }
}

/// The dominator tree of the points of `graph` in `scope`.
pub fn dominators(
    graph: &Graph,
    names: &Names,
    facets: &Facets,
    provenance: &Provenance,
    scope: Scope,
) -> Dominators {
    let unit = unit_graph(graph, facets, provenance, scope);
    let flowgraph = Flowgraph::new(&unit);
    let tree = flowgraph.dominator_tree();
    let mut weight = vec![1u32; tree.idom.len()];
    for w in (1..tree.idom.len()).rev() {
        weight[tree.idom[w] as usize] += weight[w];
    }

    let point = |w: u32| &unit.points[tree.point(w) as usize];
    let mut top_level = vec![false; unit.points.len()];
    for &p in &flowgraph.tops {
        top_level[p as usize] = true;
    }
    let below = reach_counts(
        &unit.dependents,
        |p| top_level[p],
        |p| stable_hash(names.resolve(unit.points[p].id)),
    );
    let mut by_parent: Vec<u32> = (1..tree.idom.len() as u32).collect();
    bviz_parallel::workers().install(|| {
        by_parent.par_sort_unstable_by_key(|&w| {
            let id = point(w).id;
            let name = names.resolve(id);
            let key = stable_hash(name);
            (
                tree.idom[w as usize],
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
    let children =
        |v: u32| &by_parent[first_child[v as usize] as usize..first_child[v as usize + 1] as usize];

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
                standing: match top_level[p] {
                    true => Standing::TopLevel {
                        in_cycle: !unit.dependents[p].is_empty(),
                    },
                    false => Standing::Below(below[p]),
                },
            }
        })
        .collect();
    let (index, _) = DenseIds::new(nodes.iter().map(|node| node.id));
    let dependents = dependents(&unit, &tree, &order);
    Dominators {
        scope,
        nodes,
        bounds,
        index,
        dependents,
    }
}

/// The dependents of each point of `unit`, numbered as the nodes `order` puts
/// the preorder numbers of `tree` in.
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
        of.extend(point_dependents(w).iter().map(|&q| node_of[q as usize]));
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
    tops: Vec<u32>,
}

/// Each vertex's immediate dominator, by preorder number of a walk from the
/// top of the build.
struct DominatorTree {
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
}

impl<'a> Flowgraph<'a> {
    fn new(unit: &'a UnitGraph) -> Self {
        let of = components(&unit.dependencies, &unit.dependents);
        let count = of.iter().max().map_or(0, |&c| c as usize + 1);
        let mut depended_on = vec![false; count];
        for (dependent, dependencies) in unit.dependencies.iter().enumerate() {
            for &dependency in dependencies {
                let c = of[dependency as usize];
                if c != of[dependent] {
                    depended_on[c as usize] = true;
                }
            }
        }
        let tops = (unit.points.iter().zip(&of).enumerate())
            .filter(|(_, (point, c))| point.counted && !depended_on[**c as usize])
            .map(|(p, _)| p as u32)
            .collect();
        Self { unit, tops }
    }

    fn is_top(&self, p: u32) -> bool {
        self.tops.binary_search(&p).is_ok()
    }

    fn successors(&self, points: &[u32], v: u32) -> &[u32] {
        match v {
            TOP => &self.tops,
            _ => &self.unit.dependencies[points[v as usize - 1] as usize],
        }
    }

    /// The dominator tree by Lengauer and Tarjan's algorithm with simple
    /// path compression: T. Lengauer and R. E. Tarjan, "A Fast Algorithm for
    /// Finding Dominators in a Flowgraph", ACM TOPLAS 1(1), 1979.
    fn dominator_tree(&self) -> DominatorTree {
        let counted = |p: u32| self.unit.points[p as usize].counted;
        let n = 1 + self.unit.points.iter().filter(|p| p.counted).count();

        // Preorder numbers by an iterative walk: a dependency chain can run
        // deeper than a thread's stack.
        let mut number = vec![UNNUMBERED; self.unit.points.len()];
        let mut points: Vec<u32> = Vec::with_capacity(n - 1);
        let mut parent: Vec<u32> = Vec::with_capacity(n);
        parent.push(TOP);
        let mut walk: Vec<(u32, usize)> = Vec::with_capacity(n);
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
            let from_top = self.is_top(p).then_some(TOP);
            for v in dependents.map(|&q| number[q as usize]).chain(from_top) {
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
        DominatorTree { points, idom }
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
    use crate::metrics::Unit;
    use crate::provenance::InputSet;

    /// Configured targets `deps` names, each pair running from a dependency to
    /// its dependent, in the order first named.
    fn configured(deps: &[(&str, &str)]) -> (Graph, Names) {
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

    fn tree_of(graph: &Graph, names: &Names, unit: Unit) -> Dominators {
        tree_with(graph, names, &Facets::default(), unit)
    }

    fn tree_with(graph: &Graph, names: &Names, facets: &Facets, unit: Unit) -> Dominators {
        let everything = Provenance::new(vec![InputSet::ALL; names.len()]);
        let scope = Scope::new(unit, InputSet::ALL).unwrap();
        dominators(graph, names, facets, &everything, scope)
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
        draw(tree, label, tree.tops(), &mut out);
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
            format!("a 2 {{ only a 1 }} {}", leaves(&["b", "base"]))
        );
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

    fn node(tree: &Dominators, names: &Names, name: &str) -> u32 {
        tree.index_of(names.get(name).unwrap()).unwrap()
    }

    /// A dependency two top-level points share stands among the tops as they
    /// do, but is depended on by both, and one below a top by that one alone.
    #[test]
    fn a_top_shared_by_top_level_points_counts_them() {
        let (graph, names) = configured(&[("base", "a"), ("base", "b"), ("only a", "a")]);
        let tree = tree_of(&graph, &names, Unit::ConfiguredTarget);
        let standing = |name| tree.standing(node(&tree, &names, name));
        let alone = Standing::TopLevel { in_cycle: false };
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
            assert_eq!(standing, Standing::TopLevel { in_cycle: true }, "{name}");
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
            let found = tree.top_level_above(node(&tree, &names, "base"), limit);
            let named = found.into_iter().map(|n| names.resolve(tree.id(n)));
            named.collect::<Vec<_>>()
        };
        assert_eq!(above(5), ["near", "far"]);
        assert_eq!(above(1), ["near"]);
        assert_eq!(above(0), Vec::<&str>::new());
        let near = node(&tree, &names, "near");
        assert!(tree.top_level_above(near, 5).is_empty());
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
        let found = tree.top_level_above(node(&tree, &names, "0"), 5);
        let named: Vec<&str> = found.iter().map(|&n| names.resolve(tree.id(n))).collect();
        assert_eq!(named, ["near"]);
        let base = tree.standing(node(&tree, &names, "0"));
        assert_eq!(base, Standing::Below(Count::Exact(2)));
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
            tree.tops().map(|top| tree.weight(top)).sum::<u32>(),
            LINKS as u32
        );
        let up = std::iter::successors(Some(bottom), |&node| tree.parent(node));
        assert_eq!(up.count(), LINKS);
    }

    #[test]
    fn an_empty_scope_has_an_empty_tree() {
        let tree = tree_of(&Graph::new(), &Names::default(), Unit::ConfiguredTarget);
        assert_eq!((tree.total(), tree.tops()), (0, 0..0));
    }

    /// A xorshift generator of indices below a bound, so a randomized test is
    /// the same on every run.
    fn random_indices() -> impl FnMut(usize) -> usize {
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
        let dominated: Vec<Vec<bool>> = (0..n)
            .map(|d| {
                let left = reached(succ, &tops, Some(d));
                (0..n).map(|v| v != d && !left[v]).collect()
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
    fn random_build(
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
    /// give each point the immediate dominator the definition does, and weigh
    /// each node by what it dominates; among actions, the source files they
    /// read take no part.
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

                    let tops = tree.tops().map(|top| tree.weight(top)).sum::<u32>();
                    assert_eq!(tops, nodes as u32);
                    assert_eq!(tree.total(), nodes as u32);
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
}
