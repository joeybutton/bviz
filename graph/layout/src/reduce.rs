//! Transitive reduction of a graph's edges: drops a connection whose endpoints a
//! longer path already links, leaving the minimal edge set with the same
//! reachability.
//!
//! Edges are reduced on their `(from, to)` pair, ignoring provenance, so a pair
//! is kept or dropped as a whole and a kept pair keeps all its edges.
//!
//! A dependency graph is acyclic, so the fast path reduces it in one pass over an
//! integer-indexed reachability bitset (the only feasible way on the whole base
//! graph). A graph left cyclic by node merging falls back to removing a
//! connection only when its endpoints stay linked through the others — checked
//! one removal at a time, so reachability survives a cycle (where the reduction
//! is not unique).

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::graph::Graph;
use crate::intern::Id;

/// Returns `graph` with the same nodes but only the edges of a transitive
/// reduction.
pub fn transitive_reduction(graph: &Graph) -> Graph {
    match dag_reduction(graph) {
        Some(reduced) => reduced,
        None => iterative_reduction(graph),
    }
}

/// Transitive reduction of a DAG: the redundant pairs are those whose target is
/// reachable from another of the source's successors. `None` if a cycle is found
/// (no topological order), so the caller can fall back.
fn dag_reduction(graph: &Graph) -> Option<Graph> {
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

    let order = topo_order(&succ, indeg)?;
    let redundant = redundant_pairs(&order, &succ);

    let mut out = graph.clone();
    out.edges.retain(|e| {
        e.from == e.to
            || match (index.get(&e.from), index.get(&e.to)) {
                (Some(&u), Some(&v)) => !redundant.contains(&(u, v)),
                _ => true,
            }
    });
    Some(out)
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
fn iterative_reduction(graph: &Graph) -> Graph {
    let mut pairs: BTreeSet<(Id, Id)> =
        graph.edges.iter().filter(|e| e.from != e.to).map(|e| (e.from, e.to)).collect();
    let mut succ: HashMap<Id, HashSet<Id>> = HashMap::new();
    for &(u, v) in &pairs {
        succ.entry(u).or_default().insert(v);
    }
    for (u, v) in pairs.iter().copied().collect::<Vec<_>>() {
        succ.get_mut(&u).unwrap().remove(&v);
        if reachable(&succ, u, v) {
            pairs.remove(&(u, v));
        } else {
            succ.get_mut(&u).unwrap().insert(v);
        }
    }
    let mut out = graph.clone();
    out.edges.retain(|e| e.from == e.to || pairs.contains(&(e.from, e.to)));
    out
}

/// Whether `dst` is reachable from `src` over the current successor adjacency.
fn reachable(succ: &HashMap<Id, HashSet<Id>>, src: Id, dst: Id) -> bool {
    let mut seen: HashSet<Id> = HashSet::new();
    let mut queue: VecDeque<Id> = VecDeque::new();
    queue.push_back(src);
    while let Some(u) = queue.pop_front() {
        let Some(outs) = succ.get(&u) else { continue };
        for &w in outs {
            if w == dst {
                return true;
            }
            if seen.insert(w) {
                queue.push_back(w);
            }
        }
    }
    false
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
        let mut edges: Vec<(String, String)> = (0..len - 1).map(|i| (name(i), name(i + 1))).collect();
        edges.extend((2..len).map(|j| (name(0), name(j))));
        edges
    }

    #[test]
    fn the_bitset_path_spans_multiple_words() {
        // 100 nodes > 64, so reach[] uses two words and the v/64 indexing matters.
        let len = 100;
        let owned = long_chain_with_skips(len);
        let refs: Vec<(&str, &str)> = owned.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (g, n) = build(&refs);
        // The fast path must apply (the graph is acyclic).
        assert!(dag_reduction(&g).is_some());
        let e = kept(&transitive_reduction(&g), &n);
        let chain: HashSet<(String, String)> =
            (0..len - 1).map(|i| (format!("n{i:03}"), format!("n{:03}", i + 1))).collect();
        assert_eq!(e, chain);
    }

    #[test]
    fn the_two_reductions_agree_on_a_dag() {
        // On a DAG the reduction is unique, so the fast bitset path and the
        // per-edge fallback must yield the same edges.
        let owned = long_chain_with_skips(70);
        let refs: Vec<(&str, &str)> = owned.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        let (g, n) = build(&refs);
        assert_eq!(kept(&dag_reduction(&g).unwrap(), &n), kept(&iterative_reduction(&g), &n));
    }
}
