//! Folds a containment forest to a bounded level-of-detail view.
//!
//! Only the descendants of *expanded* containers are shown; every other subtree
//! folds into its shallowest collapsed ancestor, which then renders as a leaf.
//! A singleton container (exactly one child) is always shown expanded, so it
//! never acts as a fold boundary or an expand/collapse handle. Running this
//! before quotient and layout shrinks a graph of tens of thousands of nodes to
//! a view whose size the caller caps.

use std::cell::RefCell;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashSet};

use rustc_hash::{FxHashMap, FxHashSet};

use crate::graph::{EdgeKind, Graph, id_span};
use crate::intern::Id;

/// The ids of nodes that are containers (named as some node's `parent`).
pub fn containers(graph: &Graph) -> HashSet<Id> {
    graph.nodes.iter().filter_map(|n| n.parent).collect()
}

/// The ids of multi-child containers (two or more children). Only these get an
/// expand/collapse handle: leaves can't expand and singletons auto-expand.
pub fn expandable_containers(graph: &Graph) -> HashSet<Id> {
    child_counts(graph)
        .into_iter()
        .filter(|(_, count)| *count >= 2)
        .map(|(id, _)| id)
        .collect()
}

/// Number of nodes naming each container as their `parent`.
fn child_counts(graph: &Graph) -> FxHashMap<Id, usize> {
    let mut counts: FxHashMap<Id, usize> = FxHashMap::default();
    for node in &graph.nodes {
        if let Some(parent) = node.parent {
            *counts.entry(parent).or_default() += 1;
        }
    }
    counts
}

/// Collapses `graph` to the view in which only the descendants of *effectively
/// expanded* containers are shown — a container is effectively expanded when it
/// is in `expanded` or is a singleton (exactly one child), so single-child
/// chains always reveal their descendants. Every node folds to its
/// "representative": the shallowest node on its root->node path that is neither
/// in `expanded` nor a singleton (or the node itself when its whole ancestor
/// chain is effectively expanded). Each distinct representative is emitted as
/// the node it is, so a collapsed container renders as a leaf. Edges are
/// remapped to their endpoints' representatives, self-loops
/// (both endpoints in the same collapsed subtree) dropped, and duplicates merged
/// keeping their `EdgeKind`.
pub fn collapse(graph: &Graph, expanded: &HashSet<Id>) -> Graph {
    collapse_folded(graph, expanded).0
}

/// [`collapse`], with where it folds each node of `graph`.
pub fn collapse_folded(graph: &Graph, expanded: &HashSet<Id>) -> (Graph, Folding) {
    let indexed = IndexedGraph::new(graph);
    let rep = indexed.reps(&indexed.expanded_indices(expanded));

    let mut out = Graph::new();
    for node in &graph.nodes {
        if rep[node.id.0 as usize] == node.id.0 {
            out.add_node(node.clone());
        }
    }

    let mut seen: FxHashSet<(u32, u32, EdgeKind)> =
        FxHashSet::with_capacity_and_hasher(indexed.edges.len(), Default::default());
    for &(from, to, kind) in &indexed.edges {
        if let Some((from, to)) = IndexedGraph::folded(&rep, from, to)
            && seen.insert((from, to, kind))
        {
            out.add_edge(Id(from), Id(to), kind);
        }
    }
    (out, Folding { rep })
}

/// Where [`collapse`] folds each node of one graph.
pub struct Folding {
    rep: Vec<u32>,
}

impl Folding {
    /// The boxes [`collapse`] draws an edge of the graph from `from` to `to`
    /// between, `None` where it folds the edge away.
    pub fn pair(&self, from: Id, to: Id) -> Option<(Id, Id)> {
        IndexedGraph::folded(&self.rep, from.0, to.0).map(|(from, to)| (Id(from), Id(to)))
    }
}

/// The node pairs [`collapse`] would leave, without materializing the folded
/// nodes, for a caller that needs only which boxes stay connected.
pub fn collapse_edges(graph: &Graph, expanded: &HashSet<Id>) -> FxHashSet<(Id, Id)> {
    let indexed = IndexedGraph::new(graph);
    let rep = indexed.reps(&indexed.expanded_indices(expanded));

    let mut pairs: FxHashSet<(Id, Id)> =
        FxHashSet::with_capacity_and_hasher(indexed.edges.len(), Default::default());
    for &(from, to, _) in &indexed.edges {
        if let Some((from, to)) = IndexedGraph::folded(&rep, from, to) {
            pairs.insert((Id(from), Id(to)));
        }
    }
    pairs
}

/// Caps the nodes the first layered view opens with. A layered drawing spreads
/// sideways faster than it gains boxes, so beyond this size the fitted view is a
/// smear of boxes too small to carry a label; the reader opens the containers
/// that interest them out of a legible starting point instead.
pub const DEFAULT_MAX_NODES: usize = 2000;

/// Caps the edges the first layered view opens with. On a dense graph an
/// expansion multiplies the connectors among the boxes already drawn without
/// adding many, and it is the connectors, not the boxes, that decide how far the
/// drawing spreads — so the first view is held to both counts. A view of this
/// density has long since crossed into the cheaper placement strategy, which
/// buys its speed in width, so what this count bounds is the wait rather than
/// how wide the drawing lands.
pub const DEFAULT_MAX_EDGES: usize = 20000;

/// The view with every container collapsed, which is the smallest a fold
/// reaches, measured where it is itself past the caps. No expansion of such a
/// graph fits, so these counts are the least a caller can be given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Overflow {
    pub nodes: usize,
    pub edges: usize,
}

/// A startup expansion set: the containers to expand for the largest collapsed
/// view that fits both `max_nodes` and `max_edges`, or the [`Overflow`] of a
/// graph no expansion of which fits.
///
/// `reduced` is `graph` with its transitively-redundant edges removed where the
/// view being sized draws the reduction, and `None` where it draws every edge.
/// A view keeps an edge by the pair of boxes it runs between, so the reduction
/// decides which pairs a candidate is measured over while the count itself is of
/// `graph`'s own edges across those pairs — a cap is only worth anything if it
/// counts what the layout engine is handed. Its nodes must be `graph`'s, which a
/// transitive reduction leaves untouched.
pub fn default_expansion(
    graph: &Graph,
    reduced: Option<&Graph>,
    max_nodes: usize,
    max_edges: usize,
) -> Result<HashSet<Id>, Overflow> {
    largest_fitting(
        graph,
        reduced,
        expansion_order(&Forest::new(graph)),
        max_nodes,
        max_edges,
    )
}

/// As much of `expanded` as `graph` can draw within `max_nodes` and
/// `max_edges`, the containers revealing most giving way first; the
/// [`Overflow`] of a graph no expansion of which fits, as for
/// [`default_expansion`], whose `reduced` this shares.
///
/// This holds an expansion a reader built up to a graph that has since changed
/// under it — the same graph filtered, say — so what it keeps is a part of what
/// it was given and never a container the reader did not open. An id naming no
/// container of `graph` is kept untouched: a group id the quotient pass mints
/// names no node of the graph laid out from, so nothing here can size it.
pub fn fitting_expansion(
    graph: &Graph,
    reduced: Option<&Graph>,
    expanded: &HashSet<Id>,
    max_nodes: usize,
    max_edges: usize,
) -> Result<HashSet<Id>, Overflow> {
    let order: Vec<Id> = expansion_order(&Forest::new(graph))
        .into_iter()
        .filter(|id| expanded.contains(id))
        .collect();
    let sized: HashSet<Id> = order.iter().copied().collect();
    let drawn = IndexedGraph::drawing(graph, reduced);
    let whole: Vec<u32> = order.iter().map(|id| id.0).collect();
    let fitting = match within(drawn.counts(&whole), max_nodes, max_edges) {
        true => order.len(),
        false => largest_fitting_prefix(&drawn, &order, max_nodes, max_edges)?,
    };
    let mut kept: HashSet<Id> = order[..fitting].iter().copied().collect();
    kept.extend(expanded.iter().copied().filter(|id| !sized.contains(id)));
    Ok(kept)
}

/// The longest prefix of `order` that fits both caps, as a set.
fn largest_fitting(
    graph: &Graph,
    reduced: Option<&Graph>,
    order: Vec<Id>,
    max_nodes: usize,
    max_edges: usize,
) -> Result<HashSet<Id>, Overflow> {
    let drawn = IndexedGraph::drawing(graph, reduced);
    let fitting = largest_fitting_prefix(&drawn, &order, max_nodes, max_edges)?;
    Ok(order[..fitting].iter().copied().collect())
}

/// A graph's containment forest indexed by children, answering how much of the
/// graph opening a container puts on screen.
pub struct Forest {
    children: FxHashMap<Id, Vec<Id>>,
    roots: Vec<Id>,
    /// Memoizes [`Self::visible_subtree`].
    visible: RefCell<FxHashMap<Id, usize>>,
}

impl Forest {
    pub fn new(graph: &Graph) -> Self {
        let mut children: FxHashMap<Id, Vec<Id>> = FxHashMap::default();
        let mut roots: Vec<Id> = Vec::new();
        for node in &graph.nodes {
            match node.parent {
                Some(parent) => children.entry(parent).or_default().push(node.id),
                None => roots.push(node.id),
            }
        }
        Self {
            children,
            roots,
            visible: RefCell::new(FxHashMap::default()),
        }
    }

    /// The nearest multi-child container at or below `n`, walking through
    /// single-child nodes (which auto-expand) until a node with two or more or
    /// zero children.
    fn descend(&self, mut n: Id) -> Option<Id> {
        loop {
            match self.children.get(&n).map(Vec::as_slice) {
                Some([only]) => n = *only,
                Some(_) => return Some(n),
                None => return None,
            }
        }
    }

    /// How many nodes expanding `container` puts on screen. `container` must
    /// have children.
    pub fn reveal_count(&self, container: Id) -> usize {
        self.children[&container]
            .iter()
            .map(|&c| self.visible_subtree(c))
            .sum()
    }

    /// `n` plus the chain of singleton descendants that auto-expands below it.
    fn visible_subtree(&self, n: Id) -> usize {
        if let Some(&v) = self.visible.borrow().get(&n) {
            return v;
        }
        let v = 1 + match self.children.get(&n).map(Vec::as_slice) {
            Some([only]) => self.visible_subtree(*only),
            _ => 0,
        };
        self.visible.borrow_mut().insert(n, v);
        v
    }
}

/// The multi-child containers of `forest` reachable from its roots, cheapest
/// reveal first (ties by id for a stable order). A container is reached only
/// once its nearest multi-child ancestor is taken, so the order is
/// ancestor-closed and its prefixes nest. Taking the cheapest container next
/// keeps each prefix step small, so a search over the prefixes settles close to
/// a size cap.
fn expansion_order(forest: &Forest) -> Vec<Id> {
    let candidates = forest
        .children
        .values()
        .filter(|kids| kids.len() >= 2)
        .count();
    let mut order: Vec<Id> = Vec::with_capacity(candidates);
    let mut seen: FxHashSet<Id> =
        FxHashSet::with_capacity_and_hasher(candidates, Default::default());
    let mut heap: BinaryHeap<Reverse<(usize, Id)>> = BinaryHeap::with_capacity(forest.roots.len());
    for &r in &forest.roots {
        if let Some(d) = forest.descend(r) {
            heap.push(Reverse((forest.reveal_count(d), d)));
        }
    }
    while let Some(Reverse((_, c))) = heap.pop() {
        if !seen.insert(c) {
            continue;
        }
        order.push(c);
        for &k in &forest.children[&c] {
            if let Some(d) = forest.descend(k)
                && !seen.contains(&d)
            {
                heap.push(Reverse((forest.reveal_count(d), d)));
            }
        }
    }
    order
}

/// The length of the longest prefix of `order` whose expansion collapses to a
/// view fitting both caps. Both counts rise monotonically with the prefix
/// length — more expansion only reveals nodes and de-aggregates edges — so the
/// empty prefix decides whether any prefix fits at all, and the rest is a binary
/// search.
fn largest_fitting_prefix(
    indexed: &IndexedGraph,
    order: &[Id],
    max_nodes: usize,
    max_edges: usize,
) -> Result<usize, Overflow> {
    let order_idx: Vec<u32> = order.iter().map(|id| id.0).collect();
    let fits = |counts| within(counts, max_nodes, max_edges);

    let (nodes, edges) = indexed.counts(&[]);
    if !fits((nodes, edges)) {
        return Err(Overflow { nodes, edges });
    }

    let (mut lo, mut hi) = (0usize, order.len());
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if fits(indexed.counts(&order_idx[..mid])) {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    Ok(lo)
}

/// Whether a view of `(nodes, edges)` fits both caps.
fn within((nodes, edges): (usize, usize), max_nodes: usize, max_edges: usize) -> bool {
    nodes <= max_nodes && edges <= max_edges
}

/// Stands in a representative table for an id the graph holds no node of. Such
/// an id folds to nothing, so nothing is drawn for it and no edge is drawn to
/// it.
const ABSENT: u32 = u32::MAX;

/// An integer-indexed copy of a graph's containment forest and of the edges a
/// view of it draws, so a collapsed view can be computed (by [`collapse`]) or
/// merely sized (by [`Self::counts`], for the startup search) without re-walking
/// string ids.
struct IndexedGraph {
    /// Containment parent of each id, or `None` for a root, an id the graph
    /// holds no node of, and a node whose parent is such an id.
    parent: Vec<Option<u32>>,
    /// Whether each id is a singleton container, which always auto-expands.
    singleton: Vec<bool>,
    /// Whether each id names a node of the graph.
    present: Vec<bool>,
    edges: Vec<(u32, u32, EdgeKind)>,
    /// The edges deciding which box pairs a view keeps, absent where it keeps
    /// every pair.
    skeleton: Option<Vec<(u32, u32)>>,
}

impl IndexedGraph {
    fn new(graph: &Graph) -> Self {
        Self::drawing(graph, None)
    }

    /// `graph`'s containment forest over its own edges, of which a view keeps
    /// those folding onto a pair `skeleton` also folds onto — every id
    /// `skeleton` names must be one `graph` names.
    fn drawing(graph: &Graph, skeleton: Option<&Graph>) -> Self {
        let child_counts = child_counts(graph);
        let span = id_span(graph);
        let mut present: Vec<bool> = vec![false; span];
        for node in &graph.nodes {
            present[node.id.0 as usize] = true;
        }
        let mut parent: Vec<Option<u32>> = vec![None; span];
        let mut singleton: Vec<bool> = vec![false; span];
        for node in &graph.nodes {
            let i = node.id.0 as usize;
            parent[i] = node.parent.map(|p| p.0).filter(|&p| present[p as usize]);
            singleton[i] = child_counts.get(&node.id) == Some(&1);
        }
        let edges = graph
            .edges
            .iter()
            .map(|e| (e.from.0, e.to.0, e.kind))
            .collect();
        let skeleton = skeleton.map(|skeleton| {
            skeleton
                .edges
                .iter()
                .map(|e| (e.from.0, e.to.0))
                .collect::<Vec<(u32, u32)>>()
        });
        Self {
            parent,
            singleton,
            present,
            edges,
            skeleton,
        }
    }

    /// The indices of the `expanded` ids that name a node in this graph.
    fn expanded_indices(&self, expanded: &HashSet<Id>) -> Vec<u32> {
        expanded
            .iter()
            .map(|id| id.0)
            .filter(|&i| self.present.get(i as usize) == Some(&true))
            .collect()
    }

    /// The representative each index folds to under `expanded`: the shallowest
    /// node on its root->node chain that is neither expanded nor a singleton
    /// (both always reveal their children), or the node itself when its whole
    /// chain is, or [`ABSENT`] where the graph holds no node of that id. A node
    /// is its own representative exactly when [`collapse`] emits it.
    fn reps(&self, expanded: &[u32]) -> Vec<u32> {
        let mut eff = self.singleton.clone();
        for &i in expanded {
            eff[i as usize] = true;
        }
        (0..self.parent.len() as u32)
            .map(|i| {
                if !self.present[i as usize] {
                    return ABSENT;
                }
                let mut best = if eff[i as usize] { None } else { Some(i) };
                let mut cur = i;
                while let Some(p) = self.parent[cur as usize] {
                    if !eff[p as usize] {
                        best = Some(p);
                    }
                    cur = p;
                }
                best.unwrap_or(i)
            })
            .collect()
    }

    /// The pair of representatives an edge folds to, or `None` where either end
    /// folds to nothing or the two fold together into one box.
    fn folded(rep: &[u32], from: u32, to: u32) -> Option<(u32, u32)> {
        let (from, to) = (rep[from as usize], rep[to as usize]);
        (from != to && from != ABSENT && to != ABSENT).then_some((from, to))
    }

    /// The (node, edge) counts of the view collapsing every container outside
    /// `expanded` (given as indices), without materializing it: representatives
    /// that are graph nodes, and edges remapped to representatives, self-loops
    /// dropped, those over a pair the view leaves out dropped, and the rest
    /// deduped by `(from, to, kind)`.
    fn counts(&self, expanded: &[u32]) -> (usize, usize) {
        let rep = self.reps(expanded);
        let nodes = rep
            .iter()
            .enumerate()
            .filter(|&(i, &r)| r == i as u32)
            .count();
        let kept = self.kept_pairs(&rep);
        let mut seen: FxHashSet<(u32, u32, EdgeKind)> =
            FxHashSet::with_capacity_and_hasher(self.edges.len(), Default::default());
        for &(from, to, kind) in &self.edges {
            if let Some(pair) = Self::folded(&rep, from, to)
                && kept.as_ref().is_none_or(|kept| kept.contains(&pair))
            {
                seen.insert((pair.0, pair.1, kind));
            }
        }
        (nodes, seen.len())
    }

    /// The box pairs a view over `rep` keeps, or `None` where it keeps every
    /// pair its edges fold onto.
    fn kept_pairs(&self, rep: &[u32]) -> Option<FxHashSet<(u32, u32)>> {
        let skeleton = self.skeleton.as_ref()?;
        Some(
            skeleton
                .iter()
                .filter_map(|&(from, to)| Self::folded(rep, from, to))
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Kind, Node};
    use crate::intern::Names;
    use crate::test_support::{edges, find};

    /// The interned ids of `ids`, which must already be present in `names`; used
    /// both as the expected set in assertions and as the `expanded` argument to
    /// `collapse`/`default_expansion`.
    fn expanded(names: &Names, ids: &[&str]) -> HashSet<Id> {
        ids.iter().map(|s| names.get(s).unwrap()).collect()
    }

    fn set<'a>(ids: &[&'a str]) -> HashSet<&'a str> {
        ids.iter().copied().collect()
    }

    fn node_ids<'a>(graph: &Graph, names: &'a Names) -> HashSet<&'a str> {
        graph.nodes.iter().map(|n| names.resolve(n.id)).collect()
    }

    /// Interns `name` after burning `spacing - 1` ids on nothing, so a fixture
    /// built with `spacing > 1` leaves holes in its id space.
    fn spaced(names: &mut Names, spacing: usize, name: &str) -> Id {
        for i in 1..spacing {
            names.intern(&format!("{name}/hole{i}"));
        }
        names.intern(name)
    }

    /// Two roots A and B, each a multi-child container holding `x`/`y`
    /// grandchild leaves, so each is a fold boundary.
    fn two_towers() -> (Graph, Names) {
        two_towers_spaced(1)
    }

    /// [`two_towers`] over an id space `spacing` times as wide, so its nodes
    /// carry the ids they would after a restriction dropped the rest.
    fn two_towers_spaced(spacing: usize) -> (Graph, Names) {
        let mut g = Graph::new();
        let mut names = Names::default();
        for root in ["A", "B"] {
            let root_id = spaced(&mut names, spacing, root);
            g.add_node(Node::new(root_id, root, Kind::Package, None));
            for kid in ["x", "y"] {
                let child = spaced(&mut names, spacing, &format!("{root}.{kid}"));
                g.add_node(Node::new(child, "leaf", Kind::Action, Some(root_id)));
            }
        }
        (g, names)
    }

    /// A single-child chain `A -> b -> c -> leaf`: every node but the leaf is a
    /// singleton container.
    fn singleton_chain() -> (Graph, Names) {
        let mut g = Graph::new();
        let mut names = Names::default();
        let a = names.intern("A");
        let b = names.intern("b");
        let c = names.intern("c");
        let leaf = names.intern("leaf");
        g.add_node(Node::new(a, "A", Kind::Package, None));
        g.add_node(Node::new(b, "b", Kind::Target, Some(a)));
        g.add_node(Node::new(c, "c", Kind::Target, Some(b)));
        g.add_node(Node::new(leaf, "leaf", Kind::Action, Some(c)));
        (g, names)
    }

    #[test]
    fn containers_are_the_parent_referenced_ids() {
        let (g, names) = two_towers();
        assert_eq!(containers(&g), expanded(&names, &["A", "B"]));
    }

    #[test]
    fn expandable_containers_are_only_multi_child() {
        let (mut g, mut names) = singleton_chain();
        // Make `b` a two-child container; its other child is a leaf.
        let b = names.get("b").unwrap();
        let extra = names.intern("b.extra");
        g.add_node(Node::new(extra, "leaf", Kind::Action, Some(b)));

        // The singleton-only ancestors (A) and leaves never qualify; `b` does.
        assert_eq!(expandable_containers(&g), expanded(&names, &["b"]));
    }

    #[test]
    fn empty_expanded_folds_each_tower_to_its_root() {
        let (mut g, names) = two_towers();
        // Cross-tower edge between leaves of the two folded roots.
        g.add_edge(
            names.get("A.x").unwrap(),
            names.get("B.y").unwrap(),
            EdgeKind::Aquery,
        );

        let out = collapse(&g, &HashSet::new());

        assert_eq!(node_ids(&out, &names), set(&["A", "B"]));
        assert_eq!(edges(&out, &names), [("A", "B")].into_iter().collect());
        // The folded root keeps its own kind and renders as a leaf (below).
        assert_eq!(find(&out, &names, "A").kind, Kind::Package);
    }

    #[test]
    fn singleton_chain_auto_expands_whole_chain() {
        let (g, names) = singleton_chain();
        // No container in the chain is expandable, and yet, with nothing
        // expanded, every singleton auto-expands down to the leaf.
        assert!(expandable_containers(&g).is_empty());

        let out = collapse(&g, &HashSet::new());

        assert_eq!(node_ids(&out, &names), set(&["A", "b", "c", "leaf"]));
    }

    #[test]
    fn multi_child_container_is_a_collapse_boundary() {
        let mut g = Graph::new();
        let mut names = Names::default();
        let r = names.intern("R");
        let x = names.intern("x");
        let y = names.intern("y");
        g.add_node(Node::new(r, "R", Kind::Package, None));
        g.add_node(Node::new(x, "leaf", Kind::Action, Some(r)));
        g.add_node(Node::new(y, "leaf", Kind::Action, Some(r)));

        // R is multi-child, so it is the only expand handle and a fold boundary.
        assert_eq!(expandable_containers(&g), expanded(&names, &["R"]));
        let out = collapse(&g, &HashSet::new());
        assert_eq!(node_ids(&out, &names), set(&["R"]));
    }

    #[test]
    fn multi_child_root_over_singleton_chains() {
        let mut g = Graph::new();
        let mut names = Names::default();
        let r = names.intern("R");
        g.add_node(Node::new(r, "R", Kind::Package, None));
        // Each of R's two children is the head of a singleton chain to a leaf.
        for c in ["p", "q"] {
            let c_id = names.intern(c);
            g.add_node(Node::new(c_id, c, Kind::Target, Some(r)));
            let mid = names.intern(&format!("{c}.mid"));
            let leaf = names.intern(&format!("{c}.leaf"));
            g.add_node(Node::new(mid, "mid", Kind::Target, Some(c_id)));
            g.add_node(Node::new(leaf, "leaf", Kind::Action, Some(mid)));
        }

        // R is the sole expand handle; its singleton descendants are transparent.
        assert_eq!(expandable_containers(&g), expanded(&names, &["R"]));

        // Folded: R alone (its children are singletons but R is the boundary).
        assert_eq!(
            node_ids(&collapse(&g, &HashSet::new()), &names),
            set(&["R"])
        );

        // Expanded: R, both children, and each child's auto-expanded chain.
        let out = collapse(&g, &expanded(&names, &["R"]));
        assert_eq!(
            node_ids(&out, &names),
            set(&["R", "p", "p.mid", "p.leaf", "q", "q.mid", "q.leaf"]),
        );
    }

    #[test]
    fn cross_child_edge_under_one_expanded_root() {
        let mut g = Graph::new();
        let mut names = Names::default();
        let r = names.intern("R");
        g.add_node(Node::new(r, "R", Kind::Package, None));
        for c in ["c1", "c2"] {
            let c_id = names.intern(c);
            g.add_node(Node::new(c_id, c, Kind::Target, Some(r)));
            // Two leaves keep each child a multi-child fold boundary.
            for leaf in ["a", "b"] {
                let id = names.intern(&format!("{c}.{leaf}"));
                g.add_node(Node::new(id, "leaf", Kind::Action, Some(c_id)));
            }
        }
        // Internal to c1 (self-loop once folded), and cross-child c1 -> c2.
        g.add_edge(
            names.get("c1.a").unwrap(),
            names.get("c1.b").unwrap(),
            EdgeKind::Aquery,
        );
        g.add_edge(
            names.get("c1.a").unwrap(),
            names.get("c2.a").unwrap(),
            EdgeKind::Aquery,
        );

        let out = collapse(&g, &expanded(&names, &["R"]));

        // The expanded root and its two children are shown; grandchildren fold
        // into their child.
        assert_eq!(node_ids(&out, &names), set(&["R", "c1", "c2"]));
        // c1.a -> c1.b is a self-loop (both fold to c1) and dropped; the
        // cross-child c1.a -> c2.a remaps to c1 -> c2.
        assert_eq!(edges(&out, &names), [("c1", "c2")].into_iter().collect());
    }

    #[test]
    fn duplicate_edges_dedup_distinct_kinds_kept() {
        let (mut g, names) = two_towers();
        let (ax, ay) = (names.get("A.x").unwrap(), names.get("A.y").unwrap());
        let (bx, by) = (names.get("B.x").unwrap(), names.get("B.y").unwrap());
        // Two A-subtree -> B-subtree edges of the same kind fold to one A -> B.
        g.add_edge(ax, bx, EdgeKind::Aquery);
        g.add_edge(ay, by, EdgeKind::Aquery);
        // A different kind between the same folded pair survives separately.
        g.add_edge(ax, by, EdgeKind::Query);

        let out = collapse(&g, &HashSet::new());

        let (a, b) = (names.get("A").unwrap(), names.get("B").unwrap());
        let kinds: HashSet<EdgeKind> = out
            .edges
            .iter()
            .filter(|e| e.from == a && e.to == b)
            .map(|e| e.kind)
            .collect();
        assert_eq!(
            kinds,
            [EdgeKind::Aquery, EdgeKind::Query].into_iter().collect()
        );
        assert_eq!(out.edges.len(), 2);
    }

    #[test]
    fn default_expansion_honors_caps() {
        let (mut g, names) = two_towers();
        g.add_edge(
            names.get("A.x").unwrap(),
            names.get("B.y").unwrap(),
            EdgeKind::Aquery,
        );

        // Caps that only the root view (2 nodes, 1 edge) fits: no root expands.
        let tiny = default_expansion(&g, None, 2, 1).expect("the root view fits these caps");
        assert!(tiny.is_empty());
        let v = collapse(&g, &tiny);
        assert!(v.nodes.len() <= 2 && v.edges.len() <= 1);

        // Generous caps expand every multi-child container: the full graph shows.
        let all = default_expansion(&g, None, 100, 100).expect("the whole graph fits these caps");
        let v = collapse(&g, &all);
        assert_eq!(v.nodes.len(), g.nodes.len());
        assert!(v.nodes.len() <= 100 && v.edges.len() <= 100);
        assert_eq!(all, expandable_containers(&g));
    }

    #[test]
    fn default_expansion_view_fits_both_caps() {
        let (mut g, names) = two_towers();
        // A single folded edge between the towers, so the minimal (root) view of
        // 2 nodes and 1 edge always fits these caps.
        g.add_edge(
            names.get("A.x").unwrap(),
            names.get("B.y").unwrap(),
            EdgeKind::Aquery,
        );

        for (max_nodes, max_edges) in [(2, 1), (4, 1), (4, 2), (6, 2), (100, 100)] {
            let expansion = default_expansion(&g, None, max_nodes, max_edges)
                .unwrap_or_else(|over| panic!("{max_nodes}/{max_edges} fits nothing: {over:?}"));
            let view = collapse(&g, &expansion);
            assert!(view.nodes.len() <= max_nodes);
            assert!(view.edges.len() <= max_edges);
        }
    }

    /// The search settles the empty prefix before searching the rest, so a graph
    /// already past a cap with every container collapsed is reported as such
    /// rather than answered with a prefix that fits no better.
    #[test]
    fn a_root_view_past_a_cap_overflows_instead_of_reporting_a_prefix() {
        let (mut g, names) = two_towers();
        g.add_edge(
            names.get("A.x").unwrap(),
            names.get("B.y").unwrap(),
            EdgeKind::Aquery,
        );

        // Each tower folds to its root and the one edge between their leaves
        // folds with them, so the root view is two nodes and one edge.
        let root_view = Overflow { nodes: 2, edges: 1 };
        assert_eq!(default_expansion(&g, None, 1, 1), Err(root_view));
        assert_eq!(default_expansion(&g, None, 2, 0), Err(root_view));

        // Meeting both caps exactly is fitting, and expands nothing.
        assert_eq!(default_expansion(&g, None, 2, 1), Ok(HashSet::new()));
        // Room for one tower's two leaves opens the tower that reveals cheapest.
        assert_eq!(
            default_expansion(&g, None, 4, 1),
            Ok(expanded(&names, &["A"]))
        );
    }

    /// An expansion within the caps is the reader's to keep, whichever
    /// containers they opened and whichever a fresh search would have picked.
    #[test]
    fn an_expansion_that_fits_is_kept_whole() {
        let (g, names) = two_towers();
        let opened = expanded(&names, &["B"]);

        assert_eq!(
            fitting_expansion(&g, None, &opened, 100, 100),
            Ok(opened.clone())
        );
        assert_ne!(
            default_expansion(&g, None, 100, 100),
            Ok(opened),
            "the fixture's fresh search picks the same containers, so keeping \
             them proves nothing",
        );
    }

    /// Past the caps the expansion gives way container by container, so what is
    /// left is part of what the reader had opened rather than a fresh view.
    #[test]
    fn an_expansion_past_the_caps_gives_way_a_container_at_a_time() {
        let (mut g, names) = two_towers();
        g.add_edge(
            names.get("A.x").unwrap(),
            names.get("B.y").unwrap(),
            EdgeKind::Aquery,
        );
        let both = expanded(&names, &["A", "B"]);

        // Room for one tower's leaves beside the other's folded root.
        let kept = fitting_expansion(&g, None, &both, 4, 1).expect("the root view fits");
        assert_eq!(kept.len(), 1);
        assert!(kept.is_subset(&both));

        assert_eq!(fitting_expansion(&g, None, &both, 2, 1), Ok(HashSet::new()));
        assert_eq!(
            fitting_expansion(&g, None, &both, 1, 1),
            Err(Overflow { nodes: 2, edges: 1 }),
        );
    }

    /// The quotient pass mints group ids past every node of the graph it laid
    /// out, so an expansion naming one is sized against nothing and kept.
    #[test]
    fn an_expanded_id_the_graph_holds_no_container_for_is_kept() {
        let (g, names) = two_towers();
        let group = Id(names.len() as u32 + 7);
        let opened: HashSet<Id> = [names.get("A").unwrap(), group].into_iter().collect();

        assert_eq!(
            fitting_expansion(&g, None, &opened, 100, 100),
            Ok(opened.clone())
        );
        assert_eq!(
            fitting_expansion(&g, None, &opened, 2, 0),
            Ok([group].into_iter().collect()),
        );
    }

    /// One container over a chain of leaves carrying every forward edge the
    /// chain implies, so its transitive reduction is a small fraction of the
    /// edges drawn: `len` leaves, `len - 1` steps out of `len * (len - 1) / 2`.
    fn chain_with_skips(len: u32) -> (Graph, Names) {
        let mut g = Graph::new();
        let mut names = Names::default();
        let root = names.intern("R");
        g.add_node(Node::new(root, "R", Kind::Package, None));
        let leaves: Vec<Id> = (0..len)
            .map(|i| {
                let id = names.intern(&format!("n{i:03}"));
                g.add_node(Node::new(id, "leaf", Kind::Action, Some(root)));
                id
            })
            .collect();
        for (i, &from) in leaves.iter().enumerate() {
            for &to in &leaves[i + 1..] {
                g.add_edge(from, to, EdgeKind::Aquery);
            }
        }
        (g, names)
    }

    /// A view drawing the reduction is sized by the edges the reduction leaves,
    /// so a graph whose redundant edges alone would break the cap still opens.
    #[test]
    fn a_reduced_view_is_sized_on_the_edges_the_reduction_leaves() {
        let (g, names) = chain_with_skips(8);
        let reduced = crate::reduce::transitive_reduction(&g);
        let open = expanded(&names, &["R"]);
        assert_eq!(collapse(&g, &open).edges.len(), 28);
        assert_eq!(collapse(&reduced, &open).edges.len(), 7);

        let between_the_two = 10;
        assert_eq!(
            default_expansion(&g, Some(&reduced), 100, between_the_two),
            Ok(open)
        );
    }

    /// A view keeps an edge by the pair of boxes it runs between, so a pair the
    /// reduction leaves brings with it every edge the graph has between those
    /// two, one per query that found the dependency. The budget counts them all.
    #[test]
    fn a_reduced_view_is_sized_on_every_kind_over_a_pair_it_keeps() {
        let mut g = Graph::new();
        let mut names = Names::default();
        let source = names.intern("source");
        g.add_node(Node::new(source, "source", Kind::Action, None));
        let container = names.intern("C");
        g.add_node(Node::new(container, "C", Kind::Package, None));
        let (near, far) = (names.intern("C.near"), names.intern("C.far"));
        for leaf in [near, far] {
            g.add_node(Node::new(leaf, "leaf", Kind::Action, Some(container)));
        }
        // `source -> far` is implied by `source -> near -> far`, so the
        // reduction drops it while the fold takes both to one pair of boxes.
        g.add_edge(source, near, EdgeKind::Query);
        g.add_edge(near, far, EdgeKind::Query);
        g.add_edge(source, far, EdgeKind::Cquery);
        let reduced = crate::reduce::transitive_reduction(&g);

        // The root view is `source` beside the folded `C`, joined by the two
        // edges into it, whose kinds differ.
        assert_eq!(
            default_expansion(&g, Some(&reduced), 100, 1),
            Err(Overflow { nodes: 2, edges: 2 }),
        );
        assert!(
            default_expansion(&g, Some(&reduced), 100, 2).is_ok(),
            "two edges over one kept pair are past a cap of two",
        );
    }

    /// A view drawing every edge is sized by every edge, so the same cap holds
    /// the same container shut.
    #[test]
    fn an_unreduced_view_is_sized_on_every_edge_it_draws() {
        let (g, _) = chain_with_skips(8);

        let between_the_two = 10;
        assert_eq!(
            default_expansion(&g, None, 100, between_the_two),
            Ok(HashSet::new())
        );
    }

    #[test]
    fn default_expansion_sees_through_singleton_chains() {
        // A singleton-chain root whose chain ends in a multi-child container.
        let mut g = Graph::new();
        let mut names = Names::default();
        let a = names.intern("A");
        let b = names.intern("b");
        g.add_node(Node::new(a, "A", Kind::Package, None));
        g.add_node(Node::new(b, "b", Kind::Target, Some(a)));
        for leaf in ["x", "y"] {
            let id = names.intern(leaf);
            g.add_node(Node::new(id, "leaf", Kind::Action, Some(b)));
        }

        // `b` is the reachable multi-child container; A is a transparent
        // singleton, so generous caps expand `b` alone.
        let all = default_expansion(&g, None, 100, 100).expect("the whole graph fits these caps");
        assert_eq!(all, expanded(&names, &["b"]));
        assert_eq!(
            node_ids(&collapse(&g, &all), &names),
            set(&["A", "b", "x", "y"])
        );
    }

    /// A graph's ids need not run without gaps, and its largest may sit well
    /// past its node count, so the fold reaches the same view either way.
    #[test]
    fn a_gapped_id_space_folds_to_the_same_view() {
        let (mut dense, dense_names) = two_towers_spaced(1);
        let (mut gapped, gapped_names) = two_towers_spaced(3);
        assert!(
            gapped
                .nodes
                .iter()
                .any(|n| n.id.0 as usize >= gapped.nodes.len()),
            "the fixture's ids stay within its node count",
        );
        for (g, names) in [(&mut dense, &dense_names), (&mut gapped, &gapped_names)] {
            g.add_edge(
                names.get("A.x").unwrap(),
                names.get("B.y").unwrap(),
                EdgeKind::Aquery,
            );
        }

        for open in [vec![], vec!["A"], vec!["A", "B"]] {
            let a = collapse(&dense, &expanded(&dense_names, &open));
            let b = collapse(&gapped, &expanded(&gapped_names, &open));
            assert_eq!(node_ids(&a, &dense_names), node_ids(&b, &gapped_names));
            assert_eq!(edges(&a, &dense_names), edges(&b, &gapped_names));
        }

        for caps in [(2, 1), (4, 1), (6, 2), (100, 100)] {
            let a =
                default_expansion(&dense, None, caps.0, caps.1).expect("the dense fixture fits");
            let b =
                default_expansion(&gapped, None, caps.0, caps.1).expect("the gapped fixture fits");
            assert_eq!(
                node_ids(&collapse(&dense, &a), &dense_names),
                node_ids(&collapse(&gapped, &b), &gapped_names),
            );
        }
    }

    /// Expanding an id the graph holds no node of leaves the view it would have
    /// had, rather than reading past the end of the tables indexed by id.
    #[test]
    fn expanding_an_id_the_graph_holds_no_node_of_changes_nothing() {
        let (g, names) = two_towers();
        let past = Id(g.nodes.len() as u32 + 100);
        assert_eq!(
            node_ids(&collapse(&g, &[past].into_iter().collect()), &names),
            node_ids(&collapse(&g, &HashSet::new()), &names),
        );
    }

    /// An edge to an id the graph holds no node of has no box at its far end,
    /// so the fold drops it instead of drawing to nothing.
    #[test]
    fn an_edge_to_an_id_the_graph_holds_no_node_of_is_dropped() {
        let (mut g, names) = two_towers();
        g.add_edge(
            names.get("A.x").unwrap(),
            Id(g.nodes.len() as u32 + 100),
            EdgeKind::Aquery,
        );

        let out = collapse(&g, &HashSet::new());

        assert_eq!(node_ids(&out, &names), set(&["A", "B"]));
        assert!(out.edges.is_empty());
        assert!(collapse_edges(&g, &HashSet::new()).is_empty());
    }

    /// A merge group's container is told from an ordinary box by its `group`
    /// marker and not by its kind, so a fold that drops the marker leaves a box
    /// the reader can no longer open or close.
    #[test]
    fn a_folded_group_keeps_its_marker() {
        let mut g = Graph::new();
        let mut n = Names::default();
        let action = n.intern("action0");
        g.add_node(Node::new(action, "Compile", Kind::Action, None));
        for source in ["source1", "source2"] {
            let id = n.intern(source);
            g.add_node(Node::new(id, source, Kind::Source, None));
            g.add_edge(id, action, EdgeKind::Aquery);
        }
        let (merged, _) = crate::quotient::quotient(&g, &HashSet::new(), &mut n);
        assert!(
            find(&merged, &n, "group:source1").group,
            "the fixture's merge minted no group",
        );

        let folded = collapse(&merged, &HashSet::new());

        assert!(find(&folded, &n, "group:source1").group);
    }

    #[test]
    fn collapsed_container_is_emitted_as_a_leaf() {
        let (g, names) = two_towers();
        let out = collapse(&g, &HashSet::new());
        let a = names.get("A").unwrap();
        // A is a container in the source graph but, folded, nothing names it as
        // parent in the collapsed view.
        assert!(out.nodes.iter().all(|n| n.parent != Some(a)));
        assert!(node_ids(&out, &names).contains("A"));
    }
}
