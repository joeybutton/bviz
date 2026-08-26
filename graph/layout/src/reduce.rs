//! Transitive reduction of a graph's edges: drops a connection whose endpoints a
//! longer path already links, leaving the minimal edge set with the same
//! reachability.
//!
//! Edges are reduced on their `(from, to)` pair, ignoring provenance, so a pair
//! is kept or dropped as a whole and a kept pair keeps all its edges.
//!
//! An acyclic graph reduces in one pass over an integer-indexed reachability
//! bitset, the only feasible way on the whole base graph. Folding a subtree into
//! one box leaves graphs that are not acyclic — two packages each depending on
//! the other through different members become a pair of boxes pointing both
//! ways — so a cyclic graph is reduced by condensing each cycle to a node,
//! reducing the acyclic graph that leaves, and keeping every edge inside a
//! cycle. Reachability survives, and no edge of a cycle is redundant anyway,
//! since each of its nodes already reaches every other.

use std::collections::{HashMap, HashSet};

use crate::graph::Graph;
use crate::intern::Id;

/// Returns `graph` with the same nodes but only the edges of a transitive
/// reduction.
pub fn transitive_reduction(graph: &Graph) -> Graph {
    reduced(graph.clone())
}

/// [`transitive_reduction`] of a graph the caller is done with, which spares the
/// copy of its nodes.
pub fn reduced(graph: Graph) -> Graph {
    match dag_reduction(graph) {
        Ok(reduced) => reduced,
        Err(cyclic) => condensed_reduction(cyclic),
    }
}

/// Transitive reduction of a DAG: the redundant pairs are those whose target is
/// reachable from another of the source's successors. A cycle leaves no
/// topological order, and hands `graph` back for the caller to fall back with.
fn dag_reduction(graph: Graph) -> Result<Graph, Graph> {
    let mut index: HashMap<Id, u32> = HashMap::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let next = index.len() as u32;
        index.entry(node.id).or_insert(next);
    }
    let n = index.len();
    let pairs: HashSet<(u32, u32)> = graph
        .edges
        .iter()
        .filter(|e| e.from != e.to)
        .filter_map(|e| Some((*index.get(&e.from)?, *index.get(&e.to)?)))
        .collect();
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); n];
    let mut indeg: Vec<u32> = vec![0; n];
    for &(u, v) in &pairs {
        succ[u as usize].push(v);
        indeg[v as usize] += 1;
    }

    let Some(order) = topo_order(&succ, indeg) else {
        return Err(graph);
    };
    let redundant = redundant_pairs(&order, &succ);

    let mut out = graph;
    out.edges.retain(|e| {
        e.from == e.to
            || match (index.get(&e.from), index.get(&e.to)) {
                (Some(&u), Some(&v)) => !redundant.contains(&(u, v)),
                _ => true,
            }
    });
    Ok(out)
}

/// Kahn's algorithm over the successor adjacency; `None` when a leftover node
/// betrays a cycle. Consumes `indeg`, decrementing it as nodes are emitted.
fn topo_order(succ: &[Vec<u32>], mut indeg: Vec<u32>) -> Option<Vec<u32>> {
    let n = succ.len();
    let mut order: Vec<u32> = Vec::with_capacity(n);
    let mut ready: Vec<u32> = (0..n as u32).filter(|&i| indeg[i as usize] == 0).collect();
    while let Some(u) = ready.pop() {
        order.push(u);
        for &w in &succ[u as usize] {
            indeg[w as usize] -= 1;
            if indeg[w as usize] == 0 {
                ready.push(w);
            }
        }
    }
    (order.len() == n).then_some(order)
}

/// The `(u, v)` pairs whose direct edge a longer path already implies.
/// Reachability is held in per-node bitsets over the compact node index, filled
/// in reverse topological `order` so a node's successors are already resolved.
fn redundant_pairs(order: &[u32], succ: &[Vec<u32>]) -> HashSet<(u32, u32)> {
    let n = succ.len();
    let words = n.div_ceil(64);
    let mut reach = vec![0u64; n * words]; // reach[u]: every node reachable from u
    let mut redundant: HashSet<(u32, u32)> = HashSet::new();
    for &u in order.iter().rev() {
        let ui = u as usize;
        // The union of the successors' reach: a target found here is reachable by
        // a path of length >= 2, so the direct edge to it is redundant.
        let mut via = vec![0u64; words];
        for &w in &succ[ui] {
            let wb = w as usize * words;
            for k in 0..words {
                via[k] |= reach[wb + k];
            }
        }
        for &v in &succ[ui] {
            if via[v as usize / 64] >> (v % 64) & 1 == 1 {
                redundant.insert((u, v));
            }
        }
        let ub = ui * words;
        reach[ub..ub + words].copy_from_slice(&via);
        for &w in &succ[ui] {
            reach[ub + w as usize / 64] |= 1u64 << (w % 64);
        }
    }
    redundant
}

/// Cycle-tolerant reduction: drop a pair only while its endpoints stay linked
/// through the pairs kept so far, visiting pairs in a fixed order for a
/// deterministic result.
fn condensed_reduction(graph: Graph) -> Graph {
    let mut index: HashMap<Id, u32> = HashMap::with_capacity(graph.nodes.len());
    for node in &graph.nodes {
        let next = index.len() as u32;
        index.entry(node.id).or_insert(next);
    }
    let n = index.len();
    let pairs: HashSet<(u32, u32)> = graph
        .edges
        .iter()
        .filter(|e| e.from != e.to)
        .filter_map(|e| Some((*index.get(&e.from)?, *index.get(&e.to)?)))
        .collect();
    let mut succ: Vec<Vec<u32>> = vec![Vec::new(); n];
    for &(u, v) in &pairs {
        succ[u as usize].push(v);
    }

    let comp = components(&succ, n);
    let count = comp.iter().copied().max().map_or(0, |c| c as usize + 1);
    let mut condensed: HashSet<(u32, u32)> = HashSet::with_capacity(pairs.len());
    let mut csucc: Vec<Vec<u32>> = vec![Vec::new(); count];
    let mut cindeg: Vec<u32> = vec![0; count];
    for &(u, v) in &pairs {
        let (a, b) = (comp[u as usize], comp[v as usize]);
        if a != b && condensed.insert((a, b)) {
            csucc[a as usize].push(b);
            cindeg[b as usize] += 1;
        }
    }
    let order = topo_order(&csucc, cindeg).expect("a condensation holds no cycle");
    let redundant = redundant_pairs(&order, &csucc);

    let mut within: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();
    // One edge into a cycle reaches all of it, and one out of a cycle leaves
    // from all of it, so a pair of cycles the reduction keeps joined needs only
    // the one edge between them; the lowest is taken so a graph always keeps the
    // same one.
    let mut between: HashMap<(u32, u32), (u32, u32)> = HashMap::new();
    for &(u, v) in &pairs {
        let (a, b) = (comp[u as usize], comp[v as usize]);
        match a == b {
            true => within.entry(a).or_default().push((u, v)),
            false => {
                let lowest = between.entry((a, b)).or_insert((u, v));
                *lowest = (*lowest).min((u, v));
            }
        }
    }
    let mut held: HashSet<(u32, u32)> = HashSet::new();
    for cycle in within.values() {
        held.extend(thin(cycle));
    }
    held.extend(
        between
            .iter()
            .filter(|(pair, _)| !redundant.contains(pair))
            .map(|(_, &edge)| edge),
    );

    let mut out = graph;
    out.edges.retain(|e| {
        e.from == e.to
            || match (index.get(&e.from), index.get(&e.to)) {
                (Some(&u), Some(&v)) => held.contains(&(u, v)),
                _ => true,
            }
    });
    out
}

/// The edges of one cycle that hold it together. Each is dropped in turn where
/// the rest still carry its source to its target, leaving every node of the
/// cycle reaching every other over fewer edges than it began with. A cycle has
/// no unique reduction, so which edges survive follows the order they are tried
/// in; that order is by index, so one graph always reduces the same way.
///
/// Every path between two nodes of a cycle stays inside it, so the search needs
/// none of the graph around it and costs the cycle's own size rather than the
/// whole view's.
fn thin(cycle: &[(u32, u32)]) -> HashSet<(u32, u32)> {
    let mut succ: HashMap<u32, HashSet<u32>> = HashMap::with_capacity(cycle.len());
    for &(u, v) in cycle {
        succ.entry(u).or_default().insert(v);
    }
    let mut order = cycle.to_vec();
    order.sort_unstable();
    let mut kept: HashSet<(u32, u32)> = order.iter().copied().collect();
    for (u, v) in order {
        let outs = succ
            .get_mut(&u)
            .expect("the source of one of its own edges");
        outs.remove(&v);
        if reaches(&succ, u, v) {
            kept.remove(&(u, v));
        } else {
            succ.entry(u).or_default().insert(v);
        }
    }
    kept
}

/// Whether `dst` is reachable from `src` over `succ`.
fn reaches(succ: &HashMap<u32, HashSet<u32>>, src: u32, dst: u32) -> bool {
    let mut seen: HashSet<u32> = HashSet::new();
    let mut walk = vec![src];
    while let Some(u) = walk.pop() {
        let Some(outs) = succ.get(&u) else { continue };
        for &w in outs {
            if w == dst {
                return true;
            }
            if seen.insert(w) {
                walk.push(w);
            }
        }
    }
    false
}

/// The strongly connected component each index belongs to — two indices share
/// one exactly when each reaches the other — numbered densely from zero.
fn components(succ: &[Vec<u32>], n: usize) -> Vec<u32> {
    // The order the search finishes with each index, which is the order the
    // second walk has to reach them in, reversed.
    let mut seen = vec![false; n];
    let mut finished: Vec<u32> = Vec::with_capacity(n);
    let mut walk: Vec<(u32, usize)> = Vec::new();
    for start in 0..n as u32 {
        if seen[start as usize] {
            continue;
        }
        seen[start as usize] = true;
        walk.push((start, 0));
        while let Some((u, next)) = walk.pop() {
            match succ[u as usize].get(next) {
                Some(&w) => {
                    walk.push((u, next + 1));
                    if !seen[w as usize] {
                        seen[w as usize] = true;
                        walk.push((w, 0));
                    }
                }
                None => finished.push(u),
            }
        }
    }

    let mut pred: Vec<Vec<u32>> = vec![Vec::new(); n];
    for (u, forward) in succ.iter().enumerate() {
        for &w in forward {
            pred[w as usize].push(u as u32);
        }
    }

    /// Stands in the component table for an index no component has claimed.
    const UNCLAIMED: u32 = u32::MAX;

    let mut comp = vec![UNCLAIMED; n];
    let mut next = 0;
    for &root in finished.iter().rev() {
        if comp[root as usize] != UNCLAIMED {
            continue;
        }
        comp[root as usize] = next;
        let mut walk = vec![root];
        while let Some(u) = walk.pop() {
            for &w in &pred[u as usize] {
                if comp[w as usize] == UNCLAIMED {
                    comp[w as usize] = next;
                    walk.push(w);
                }
            }
        }
        next += 1;
    }
    comp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Kind};
    use crate::intern::Names;

    fn build(edges: &[(&str, &str)]) -> (Graph, Names) {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mut seen = std::collections::HashSet::new();
        for &(a, b) in edges {
            for id in [a, b] {
                if seen.insert(id) {
                    g.add_node(n.intern(id), id, Kind::Action, None);
                }
            }
        }
        for &(a, b) in edges {
            g.add_edge(n.intern(a), n.intern(b), EdgeKind::Aquery);
        }
        (g, n)
    }

    fn kept(g: &Graph, n: &Names) -> std::collections::HashSet<(String, String)> {
        g.edges
            .iter()
            .map(|e| (n.resolve(e.from).to_string(), n.resolve(e.to).to_string()))
            .collect()
    }

    #[test]
    fn drops_an_edge_a_longer_path_implies() {
        // a->b->c plus a redundant a->c.
        let (g, n) = build(&[("a", "b"), ("b", "c"), ("a", "c")]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert!(e.contains(&("a".into(), "b".into())));
        assert!(e.contains(&("b".into(), "c".into())));
        assert!(!e.contains(&("a".into(), "c".into())), "a->c is redundant");
        // All nodes survive.
        assert_eq!(r.nodes.len(), 3);
    }

    #[test]
    fn keeps_a_direct_edge_with_no_alternate_path() {
        let (g, n) = build(&[("a", "b"), ("a", "c")]);
        let r = transitive_reduction(&g);
        assert_eq!(kept(&r, &n).len(), 2);
    }

    #[test]
    fn a_cycle_keeps_every_edge_so_reachability_survives() {
        // a->b->c->a: none is redundant (its endpoints only connect through it).
        let (g, n) = build(&[("a", "b"), ("b", "c"), ("c", "a")]);
        assert_eq!(kept(&transitive_reduction(&g), &n).len(), 3);
    }

    #[test]
    fn reduction_never_strands_a_reachable_node() {
        // a fans into a 2-cycle {b,c}; removing both a->b and a->c would strand
        // them, so at least one survives.
        let (g, n) = build(&[("a", "b"), ("a", "c"), ("b", "c"), ("c", "b")]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert!(e.contains(&("a".into(), "b".into())) || e.contains(&("a".into(), "c".into())));
    }

    #[test]
    fn diamond_keeps_both_branches_drops_the_shortcut() {
        // a->b->d and a->c->d: both branches stay (neither implies the other),
        // only the direct a->d is redundant.
        let (g, n) = build(&[("a", "b"), ("a", "c"), ("b", "d"), ("c", "d"), ("a", "d")]);
        let e = kept(&transitive_reduction(&g), &n);
        assert_eq!(e.len(), 4);
        assert!(!e.contains(&("a".into(), "d".into())));
    }

    #[test]
    fn a_chain_drops_every_skip_edge() {
        // a->b->c->d with all forward skips; only the three chain steps remain.
        let (g, n) = build(&[
            ("a", "b"),
            ("b", "c"),
            ("c", "d"),
            ("a", "c"),
            ("a", "d"),
            ("b", "d"),
        ]);
        let e = kept(&transitive_reduction(&g), &n);
        assert_eq!(
            e,
            [("a", "b"), ("b", "c"), ("c", "d")]
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect()
        );
    }

    /// A path `n0 -> n1 -> ... -> n{len-1}` plus, from `n0`, a redundant skip to
    /// every later node, named so the reachability bitset must span several
    /// 64-bit words.
    fn long_chain_with_skips(len: usize) -> Vec<(String, String)> {
        let name = |i: usize| format!("n{i:03}");
        let mut edges: Vec<(String, String)> =
            (0..len - 1).map(|i| (name(i), name(i + 1))).collect();
        edges.extend((2..len).map(|j| (name(0), name(j))));
        edges
    }

    #[test]
    fn the_bitset_path_spans_multiple_words() {
        // 100 nodes > 64, so reach[] uses two words and the v/64 indexing matters.
        let len = 100;
        let owned = long_chain_with_skips(len);
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (g, n) = build(&refs);
        // The fast path must apply (the graph is acyclic).
        assert!(dag_reduction(g.clone()).is_ok());
        let e = kept(&transitive_reduction(&g), &n);
        let chain: HashSet<(String, String)> = (0..len - 1)
            .map(|i| (format!("n{i:03}"), format!("n{:03}", i + 1)))
            .collect();
        assert_eq!(e, chain);
    }

    /// Every pair one graph links the other must link too, which is what a
    /// reduction is allowed to trade edges for.
    fn reaches_alike(before: &Graph, after: &Graph, n: &Names) {
        let out = |g: &Graph| {
            let mut m: HashMap<Id, Vec<Id>> = HashMap::new();
            for e in &g.edges {
                m.entry(e.from).or_default().push(e.to);
            }
            m
        };
        let (was, now) = (out(before), out(after));
        let seen_from = |m: &HashMap<Id, Vec<Id>>, src: Id| {
            let mut seen: HashSet<Id> = HashSet::new();
            let mut walk = vec![src];
            while let Some(u) = walk.pop() {
                for &w in m.get(&u).map(Vec::as_slice).unwrap_or(&[]) {
                    if seen.insert(w) {
                        walk.push(w);
                    }
                }
            }
            seen
        };
        for node in &before.nodes {
            let (a, b) = (seen_from(&was, node.id), seen_from(&now, node.id));
            for target in a {
                assert!(
                    b.contains(&target),
                    "{} no longer reaches {}",
                    n.resolve(node.id),
                    n.resolve(target),
                );
            }
        }
    }

    /// A cycle keeps its nodes reaching one another, but the edges that do more
    /// than that are still dropped: a chord across a cycle is implied by the way
    /// round.
    #[test]
    fn a_cycle_drops_a_chord_it_already_implies() {
        let (g, n) = build(&[
            ("a", "b"),
            ("b", "c"),
            ("c", "d"),
            ("d", "a"),
            // Implied by b -> c -> d.
            ("b", "d"),
        ]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert!(!e.contains(&("b".into(), "d".into())), "the chord survived");
        assert_eq!(e.len(), 4, "the cycle itself lost an edge: {e:?}");
        reaches_alike(&g, &r, &n);
    }

    /// An edge into a cycle that another edge into the same cycle already
    /// implies is dropped: reaching any node of a cycle reaches all of them.
    #[test]
    fn one_edge_into_a_cycle_stands_for_the_rest() {
        let (g, n) = build(&[
            ("a", "b"),
            ("b", "c"),
            ("c", "a"),
            ("d", "a"),
            // d already reaches b through a.
            ("d", "b"),
        ]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        assert_eq!(
            e.iter().filter(|(from, _)| from == "d").count(),
            1,
            "d keeps more than one way into the cycle: {e:?}",
        );
        reaches_alike(&g, &r, &n);
    }

    /// Two cycles joined by several edges keep one of them; the rest are what
    /// that one already implies.
    #[test]
    fn cycles_joined_many_ways_keep_one_way() {
        let (g, n) = build(&[
            ("a1", "a2"),
            ("a2", "a1"),
            ("b1", "b2"),
            ("b2", "b1"),
            ("a1", "b1"),
            ("a1", "b2"),
            ("a2", "b1"),
            ("a2", "b2"),
        ]);
        let r = transitive_reduction(&g);
        let e = kept(&r, &n);
        let crossing = e
            .iter()
            .filter(|(from, to)| from.starts_with('a') && to.starts_with('b'))
            .count();
        assert_eq!(crossing, 1, "more than one crossing edge kept: {e:?}");
        reaches_alike(&g, &r, &n);
    }

    /// A graph a fold leaves cyclic still reduces in one pass over the acyclic
    /// graph its cycles condense to, so its cost follows its size rather than
    /// the product of its edges and its nodes.
    #[test]
    fn a_graph_of_many_cycles_reduces() {
        // Twenty cycles of five, chained so the condensation is a long path,
        // and every chain step doubled so the reduction has something to drop.
        let mut edges: Vec<(String, String)> = Vec::new();
        for c in 0..20 {
            for i in 0..5 {
                edges.push((format!("c{c}n{i}"), format!("c{c}n{}", (i + 1) % 5)));
            }
            if c > 0 {
                edges.push((format!("c{}n0", c - 1), format!("c{c}n0")));
                edges.push((format!("c{}n1", c - 1), format!("c{c}n1")));
            }
        }
        let refs: Vec<(&str, &str)> = edges
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (g, n) = build(&refs);
        let r = transitive_reduction(&g);
        reaches_alike(&g, &r, &n);
        // Each pair of neighbouring cycles is joined two ways and needs one.
        assert_eq!(kept(&r, &n).len(), 20 * 5 + 19);
    }

    #[test]
    fn the_two_reductions_agree_on_a_dag() {
        // On a DAG the reduction is unique, so the fast bitset path and the
        // per-edge fallback must yield the same edges.
        let owned = long_chain_with_skips(70);
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        let (g, n) = build(&refs);
        assert_eq!(
            kept(&dag_reduction(g.clone()).ok().unwrap(), &n),
            kept(&condensed_reduction(g), &n),
        );
    }
}
