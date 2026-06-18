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
/// (no topological order), so the caller can fall back. Reachability is held in
/// per-node bitsets over a compact node index, built in reverse topological order
/// so a node's successors are already resolved.
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

    // Kahn's algorithm; a leftover node means a cycle.
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
    if order.len() != n {
        return None;
    }

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
}
