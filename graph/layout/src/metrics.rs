//! Per-target dependency metrics: how much of the build each target pulls in,
//! and how much of it depends on the target in turn.
//!
//! A target stands for every node that belongs to it — its configured targets,
//! actions and files — so a graph read at any granularity measures alike, and an
//! edge between two nodes of one target is no dependency of it on itself.

use rayon::prelude::*;

use crate::facets::Facets;
use crate::graph::{Graph, Kind};
use crate::intern::Id;
use crate::reduce::{DenseIds, reach_counts};

/// What one target's dependencies come to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetMetrics {
    pub id: Id,
    pub kind: Kind,
    /// How many targets it depends on, directly or transitively.
    pub closure: u32,
    /// How many targets depend on it, directly or transitively: what a change
    /// to it can break.
    pub blast_radius: u32,
    /// How many targets depend on it directly.
    pub fan_in: u32,
    /// How many targets it depends on directly.
    pub fan_out: u32,
}

/// The metrics of every target of `graph`, in node order: every node that
/// `facets` names as its own target.
pub fn target_metrics(graph: &Graph, facets: &Facets) -> Vec<TargetMetrics> {
    let mut targets: Vec<(Id, Kind)> = graph
        .nodes
        .iter()
        .filter(|node| facets.label(node.id) == Some(node.id))
        .map(|node| (node.id, node.kind))
        .collect();
    let (dense, n) = DenseIds::new(targets.iter().map(|&(id, _)| id));
    // Ids are numbered as first named, so this keeps each target's first node.
    let mut first = 0;
    targets.retain(|&(id, _)| {
        let keep = dense.get(id) == Some(first);
        first += u32::from(keep);
        keep
    });
    let target_of = |id: Id| dense.get(facets.label(id)?);

    let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(graph.edges.len());
    pairs.extend(
        graph
            .edges
            .iter()
            .filter_map(|e| Some((target_of(e.from)?, target_of(e.to)?)))
            .filter(|(from, to)| from != to),
    );
    bviz_parallel::workers().install(|| pairs.par_sort_unstable());
    pairs.dedup();

    let (dependents, dependencies) = adjacency(n as usize, &pairs);
    drop(pairs);
    let blast_radius = reach_counts(&dependents);
    let closure = reach_counts(&dependencies);
    targets
        .into_iter()
        .enumerate()
        .map(|(t, (id, kind))| TargetMetrics {
            id,
            kind,
            closure: closure[t],
            blast_radius: blast_radius[t],
            fan_in: dependents[t].len() as u32,
            fan_out: dependencies[t].len() as u32,
        })
        .collect()
}

/// The distinct `pairs`, which run from a dependency to its dependent over the
/// indices `0..n`, as each index's dependents and its dependencies.
fn adjacency(n: usize, pairs: &[(u32, u32)]) -> (Vec<Vec<u32>>, Vec<Vec<u32>>) {
    let (mut out_degree, mut in_degree) = (vec![0usize; n], vec![0usize; n]);
    for &(from, to) in pairs {
        out_degree[from as usize] += 1;
        in_degree[to as usize] += 1;
    }
    let mut dependents: Vec<Vec<u32>> = out_degree.into_iter().map(Vec::with_capacity).collect();
    let mut dependencies: Vec<Vec<u32>> = in_degree.into_iter().map(Vec::with_capacity).collect();
    for &(from, to) in pairs {
        dependents[from as usize].push(to);
        dependencies[to as usize].push(from);
    }
    (dependents, dependencies)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{EdgeKind, Node};
    use crate::intern::Names;

    /// A graph of targets, each holding one configured target, with the edges
    /// `deps` between the configured targets of the named targets.
    fn configured(targets: &[&str], deps: &[(&str, &str)]) -> (Graph, Facets, Names) {
        let mut graph = Graph::new();
        let mut facets = Facets::default();
        let mut names = Names::default();
        for &name in targets {
            let target = names.intern(name);
            let inner = names.intern(&format!("{name} (config)"));
            graph.add_node(Node::new(target, name, Kind::Target, None));
            graph.add_node(Node::new(inner, name, Kind::ConfiguredTarget, Some(target)));
            facets.set_label(target, target);
            facets.set_label(inner, target);
        }
        for &(from, to) in deps {
            let config = |name: &str| names.get(&format!("{name} (config)")).unwrap();
            graph.add_edge(config(from), config(to), EdgeKind::Cquery);
        }
        (graph, facets, names)
    }

    fn by_name(graph: &Graph, facets: &Facets, names: &Names) -> Vec<(String, u32, u32, u32, u32)> {
        target_metrics(graph, facets)
            .into_iter()
            .map(|m| {
                (
                    names.resolve(m.id).to_string(),
                    m.closure,
                    m.blast_radius,
                    m.fan_in,
                    m.fan_out,
                )
            })
            .collect()
    }

    #[test]
    fn a_chain_measures_both_ways() {
        // lib -> bin -> test: the test pulls in both, a change to lib breaks both.
        let (g, f, n) = configured(
            &["lib", "bin", "test"],
            &[("lib", "bin"), ("bin", "test"), ("lib", "test")],
        );
        assert_eq!(
            by_name(&g, &f, &n),
            [
                ("lib".into(), 0, 2, 2, 0),
                ("bin".into(), 1, 1, 1, 1),
                ("test".into(), 2, 0, 0, 2),
            ]
        );
    }

    #[test]
    fn parallel_edges_between_two_targets_count_once() {
        let (mut g, f, n) = configured(&["a", "b"], &[("a", "b")]);
        g.add_edge(
            n.get("a").unwrap(),
            n.get("b (config)").unwrap(),
            EdgeKind::Query,
        );
        g.add_edge(
            n.get("a").unwrap(),
            n.get("a (config)").unwrap(),
            EdgeKind::Query,
        );
        assert_eq!(
            by_name(&g, &f, &n),
            [("a".into(), 0, 1, 1, 0), ("b".into(), 1, 0, 0, 1)]
        );
    }

    /// Two targets depending on each other through different configured
    /// targets each reach the other and whatever either reaches.
    #[test]
    fn a_cycle_between_targets_counts_each_member_once() {
        let (g, f, n) = configured(&["a", "b", "c"], &[("a", "b"), ("b", "a"), ("b", "c")]);
        assert_eq!(
            by_name(&g, &f, &n),
            [
                ("a".into(), 1, 2, 1, 1),
                ("b".into(), 1, 2, 2, 1),
                ("c".into(), 2, 0, 0, 1),
            ]
        );
    }
}
