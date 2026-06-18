//! Transitive reduction of a visible graph's edges: drops a connection whose
//! endpoints a longer path already links, leaving the minimal edge set with the
//! same reachability. On a dense view this both declutters the drawing and cuts
//! the edge count that dominates layout time.
//!
//! Edges are reduced on their `(from, to)` pair, ignoring provenance, so a pair
//! is kept or dropped as a whole and a kept pair keeps all its edges. A
//! connection is removed only when its endpoints stay connected through the
//! *other* kept connections, checked one removal at a time, so reachability is
//! preserved even when collapsing has left the view with a cycle (where a
//! reduction is not unique); on the acyclic common case this is the unique
//! transitive reduction.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use crate::graph::Graph;
use crate::intern::Id;

/// Returns `graph` with the same nodes but only the edges of a transitive
/// reduction.
pub fn transitive_reduction(graph: &Graph) -> Graph {
    let mut pairs: BTreeSet<(Id, Id)> =
        graph.edges.iter().filter(|e| e.from != e.to).map(|e| (e.from, e.to)).collect();
    let mut succ: HashMap<Id, HashSet<Id>> = HashMap::new();
    for &(u, v) in &pairs {
        succ.entry(u).or_default().insert(v);
    }

    // Visit pairs in a fixed order so the kept set is deterministic.
    for (u, v) in pairs.iter().copied().collect::<Vec<_>>() {
        succ.get_mut(&u).unwrap().remove(&v);
        if reachable(&succ, u, v) {
            pairs.remove(&(u, v));
        } else {
            succ.get_mut(&u).unwrap().insert(v);
        }
    }

    let mut out = graph.clone();
    out.edges.retain(|e| pairs.contains(&(e.from, e.to)));
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
