//! Restricts a graph to a chosen set of nodes.
//!
//! Every node outside the set is dropped, along with its containment position
//! and its edges. To keep the containment forest well-formed for the layout
//! engine (which only ever descends from a `parent == None` root, so a node
//! whose parent was dropped would never be reached), each surviving node
//! reparents to the nearest ancestor that also survives, or becomes a root
//! itself. To keep the dependency structure legible, a pair of surviving nodes
//! the graph joins only through dropped ones gets a synthetic
//! [`EdgeKind::Transitive`] bridge in place of the path that no longer exists.
//!
//! A survivor keeps the id it carried in the whole graph, so the restriction's
//! id space is the whole graph's with holes in it. That is what lets a filter
//! apply to one view without disturbing the rest: the expansion set, the
//! selection, the layout's pinning anchors and the name table all key on the
//! id, and all of them go on meaning what they meant.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::graph::{EdgeKind, Graph, Node};
use crate::intern::Id;
use crate::reduce::Condensation;

/// How many kept nodes one run of dropped nodes stands for, and so how many
/// bridges a kept node gains from reaching that run. A dropped node joins each
/// of its kept predecessors to each of its kept successors, so an unbounded
/// count of successors is what would carry hiding a wide layer of a build graph
/// — its actions, say, or its artifacts — towards the graph's transitive
/// closure. Past the cap the lowest ids stand for the run and the rest of what
/// it reaches goes undrawn, so the reader still sees that a dependency leads on
/// without the drawing spending its whole edge budget on where.
pub const MAX_BRIDGE_FAN_OUT: usize = 32;

/// `graph` restricted to `keep`: every kept node, reparented to its nearest
/// surviving ancestor; every edge `graph` had between two kept nodes, with the
/// query provenance it came with; and one [`EdgeKind::Transitive`] bridge for
/// each kept pair joined only through dropped nodes that keeps no direct edge
/// of its own. An id in `keep` that names no node in `graph` is ignored.
///
/// `reduced` supplies the paths the bridges stand for, and must reach among
/// `graph`'s nodes exactly as `graph` does — the transitive reduction is the
/// smallest such graph and the one a caller already holds. The restriction's
/// own edges are read from `graph` itself, so this is not a reduced view: a
/// caller that draws dependencies unreduced gets every one of them, and
/// reducing the result is a further pass it may decline.
pub fn filter(graph: &Graph, reduced: &Graph, keep: &HashSet<Id>) -> Graph {
    let parent_of: HashMap<Id, Option<Id>> = graph.nodes.iter().map(|n| (n.id, n.parent)).collect();

    let mut out = Graph::new();
    out.nodes.reserve(keep.len());
    for node in &graph.nodes {
        if !keep.contains(&node.id) {
            continue;
        }
        let mut parent = node.parent;
        while let Some(p) = parent {
            if keep.contains(&p) {
                break;
            }
            parent = parent_of.get(&p).copied().flatten();
        }
        out.nodes.push(Node {
            parent,
            ..node.clone()
        });
    }

    let mut direct: HashSet<(Id, Id)> = HashSet::with_capacity(graph.edges.len());
    for edge in &graph.edges {
        if keep.contains(&edge.from) && keep.contains(&edge.to) {
            direct.insert((edge.from, edge.to));
            out.add_edge(edge.from, edge.to, edge.kind);
        }
    }

    for (from, to) in bridges(reduced, keep, &direct) {
        out.add_edge(from, to, EdgeKind::Transitive);
    }
    out
}

/// The kept pairs `graph` joins through dropped nodes and `direct` does not
/// already join, each capped at [`MAX_BRIDGE_FAN_OUT`] targets per source.
///
/// A pair `direct` holds is left out rather than bridged, so a dependency the
/// whole graph draws keeps the provenance it was found with instead of being
/// restated as a path through nodes the reader cannot see.
fn bridges(graph: &Graph, keep: &HashSet<Id>, direct: &HashSet<(Id, Id)>) -> BTreeSet<(Id, Id)> {
    let runs = DroppedRuns::new(graph, keep);
    let frontier = runs.frontiers();

    let mut bridges: BTreeSet<(Id, Id)> = BTreeSet::new();
    for &(from, into) in &runs.entries {
        for &to in &frontier[runs.condensation.of[into as usize] as usize] {
            if from != to && !direct.contains(&(from, to)) {
                bridges.insert((from, to));
            }
        }
    }
    bridges
}

/// The dropped nodes of a graph, densely indexed, with the cycles among them
/// condensed away: a dropped node's reach is only settled once everything it
/// reaches is, which a cycle makes impossible node by node and possible
/// component by component.
struct DroppedRuns {
    condensation: Condensation,
    /// The kept nodes each dropped index leads straight to.
    exits: Vec<Vec<Id>>,
    /// Each kept node beside a dropped index it leads straight into.
    entries: Vec<(Id, u32)>,
}

impl DroppedRuns {
    fn new(graph: &Graph, keep: &HashSet<Id>) -> Self {
        // Only a dropped node an edge reaches can carry one, so indexing the
        // edges' endpoints indexes every dropped node a bridge could run over.
        let mut index: HashMap<Id, u32> = HashMap::with_capacity(graph.nodes.len());
        for id in graph.edges.iter().flat_map(|e| [e.from, e.to]) {
            if !keep.contains(&id) {
                let next = index.len() as u32;
                index.entry(id).or_insert(next);
            }
        }

        let mut succ: Vec<Vec<u32>> = vec![Vec::new(); index.len()];
        let mut exits: Vec<Vec<Id>> = vec![Vec::new(); index.len()];
        let mut entries: Vec<(Id, u32)> = Vec::new();
        for edge in &graph.edges {
            match (index.get(&edge.from), index.get(&edge.to)) {
                (Some(&u), Some(&v)) if u != v => succ[u as usize].push(v),
                (Some(&u), None) => exits[u as usize].push(edge.to),
                (None, Some(&v)) => entries.push((edge.from, v)),
                _ => {}
            }
        }

        Self {
            condensation: Condensation::new(&succ),
            exits,
            entries,
        }
    }

    /// The kept nodes each component leads to over dropped nodes alone, capped
    /// at [`MAX_BRIDGE_FAN_OUT`]. Components settle in reverse topological
    /// order, so each one reads its successors' frontiers already complete and
    /// one pass suffices.
    fn frontiers(&self) -> Vec<BTreeSet<Id>> {
        fn cap(reach: &mut BTreeSet<Id>) {
            while reach.len() > MAX_BRIDGE_FAN_OUT {
                reach.pop_last();
            }
        }

        let mut frontier: Vec<BTreeSet<Id>> = vec![BTreeSet::new(); self.condensation.succ.len()];
        for (dropped, exits) in self.exits.iter().enumerate() {
            let reach = &mut frontier[self.condensation.of[dropped] as usize];
            reach.extend(exits.iter().copied());
            cap(reach);
        }
        for &component in self.condensation.order.iter().rev() {
            let mut reach = std::mem::take(&mut frontier[component as usize]);
            for &onward in &self.condensation.succ[component as usize] {
                reach.extend(frontier[onward as usize].iter().copied());
                cap(&mut reach);
            }
            frontier[component as usize] = reach;
        }
        frontier
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::graph::Kind;
    use crate::intern::Names;
    use crate::lod;
    use crate::map::map_layout;
    use crate::reduce;

    /// A graph built from `(from, to, EdgeKind::Query)` edges plus, for any node
    /// not otherwise named, a lone node with no parent.
    fn build(edges: &[(&str, &str)]) -> (Graph, Names) {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mut seen = HashSet::new();
        for &(a, b) in edges {
            for id in [a, b] {
                if seen.insert(id) {
                    g.add_node(Node::new(n.intern(id), id, Kind::Action, None));
                }
            }
        }
        for &(a, b) in edges {
            g.add_edge(n.intern(a), n.intern(b), EdgeKind::Query);
        }
        (g, n)
    }

    fn ids(n: &Names, labels: &[&str]) -> HashSet<Id> {
        labels.iter().map(|l| n.get(l).unwrap()).collect()
    }

    fn edges(g: &Graph, n: &Names) -> HashSet<(String, String, EdgeKind)> {
        g.edges
            .iter()
            .map(|e| {
                (
                    n.resolve(e.from).to_string(),
                    n.resolve(e.to).to_string(),
                    e.kind,
                )
            })
            .collect()
    }

    #[test]
    fn a_chain_collapses_to_one_transitive_edge() {
        let (g, n) = build(&[("a", "b"), ("b", "c")]);
        let out = filter(&g, &reduce::transitive_reduction(&g), &ids(&n, &["a", "c"]));
        assert_eq!(out.nodes.len(), 2);
        assert_eq!(
            edges(&out, &n),
            [("a".to_string(), "c".to_string(), EdgeKind::Transitive)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn a_direct_edge_needs_no_transitive_duplicate() {
        let (g, n) = build(&[("a", "b"), ("a", "c")]);
        let out = filter(&g, &reduce::transitive_reduction(&g), &ids(&n, &["a", "c"]));
        assert_eq!(
            edges(&out, &n),
            [("a".to_string(), "c".to_string(), EdgeKind::Query)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn fan_out_through_a_dropped_hub_reaches_every_kept_successor() {
        let (g, n) = build(&[("a", "d"), ("d", "b"), ("d", "c")]);
        let out = filter(
            &g,
            &reduce::transitive_reduction(&g),
            &ids(&n, &["a", "b", "c"]),
        );
        assert_eq!(
            edges(&out, &n),
            [
                ("a".to_string(), "b".to_string(), EdgeKind::Transitive),
                ("a".to_string(), "c".to_string(), EdgeKind::Transitive),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn fan_in_through_a_dropped_hub_reaches_every_kept_predecessor() {
        let (g, n) = build(&[("a", "d"), ("b", "d"), ("d", "c")]);
        let out = filter(
            &g,
            &reduce::transitive_reduction(&g),
            &ids(&n, &["a", "b", "c"]),
        );
        assert_eq!(
            edges(&out, &n),
            [
                ("a".to_string(), "c".to_string(), EdgeKind::Transitive),
                ("b".to_string(), "c".to_string(), EdgeKind::Transitive),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn a_cycle_of_dropped_nodes_still_terminates() {
        // a -> d1 -> d2 -> d1 (cycle) and d2 -> b: b must still be reached.
        let mut g = Graph::new();
        let mut n = Names::default();
        for id in ["a", "d1", "d2", "b"] {
            g.add_node(Node::new(n.intern(id), id, Kind::Action, None));
        }
        for (from, to) in [("a", "d1"), ("d1", "d2"), ("d2", "d1"), ("d2", "b")] {
            g.add_edge(n.intern(from), n.intern(to), EdgeKind::Query);
        }
        let out = filter(&g, &reduce::transitive_reduction(&g), &ids(&n, &["a", "b"]));
        assert_eq!(
            edges(&out, &n),
            [("a".to_string(), "b".to_string(), EdgeKind::Transitive)]
                .into_iter()
                .collect()
        );
    }

    /// Settling one dropped node at a time cannot answer this: whichever of the
    /// cycle is settled first is settled while the other's reach is still
    /// unknown, and comes out short. Condensing the cycle to one component is
    /// what lets a single pass name the whole edge set asserted here.
    #[test]
    fn a_cycle_shares_reach_between_both_of_its_dropped_nodes() {
        // d1 <-> d2 is a cycle; d1 also reaches b and d2 also reaches c. Both a
        // (which only enters at d1) and x (which only enters at d2) must see
        // both b and c, since either dropped node can reach the other.
        let mut g = Graph::new();
        let mut n = Names::default();
        for id in ["a", "x", "d1", "d2", "b", "c"] {
            g.add_node(Node::new(n.intern(id), id, Kind::Action, None));
        }
        for (from, to) in [
            ("a", "d1"),
            ("x", "d2"),
            ("d1", "d2"),
            ("d2", "d1"),
            ("d1", "b"),
            ("d2", "c"),
        ] {
            g.add_edge(n.intern(from), n.intern(to), EdgeKind::Query);
        }
        let out = filter(
            &g,
            &reduce::transitive_reduction(&g),
            &ids(&n, &["a", "x", "b", "c"]),
        );
        assert_eq!(
            edges(&out, &n),
            [
                ("a".to_string(), "b".to_string(), EdgeKind::Transitive),
                ("a".to_string(), "c".to_string(), EdgeKind::Transitive),
                ("x".to_string(), "b".to_string(), EdgeKind::Transitive),
                ("x".to_string(), "c".to_string(), EdgeKind::Transitive),
            ]
            .into_iter()
            .collect()
        );
    }

    #[test]
    fn dropping_an_intermediate_container_reparents_to_the_nearest_kept_ancestor() {
        let mut g = Graph::new();
        let mut n = Names::default();
        let root = n.intern("root");
        g.add_node(Node::new(root, "root", Kind::Package, None));
        let mid = n.intern("mid");
        g.add_node(Node::new(mid, "mid", Kind::Package, Some(root)));
        let leaf = n.intern("leaf");
        g.add_node(Node::new(leaf, "leaf", Kind::Target, Some(mid)));

        let out = filter(
            &g,
            &reduce::transitive_reduction(&g),
            &ids(&n, &["root", "leaf"]),
        );
        let leaf_out = out.nodes.iter().find(|node| node.id == leaf).unwrap();
        assert_eq!(leaf_out.parent, Some(root));
    }

    #[test]
    fn dropping_every_ancestor_leaves_a_root() {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mid = n.intern("mid");
        g.add_node(Node::new(mid, "mid", Kind::Package, None));
        let leaf = n.intern("leaf");
        g.add_node(Node::new(leaf, "leaf", Kind::Target, Some(mid)));

        let out = filter(&g, &reduce::transitive_reduction(&g), &ids(&n, &["leaf"]));
        assert_eq!(out.nodes.len(), 1);
        assert_eq!(out.nodes[0].parent, None);
    }

    /// Two packages, each holding two targets, each holding two actions, with a
    /// dependency from each action to the next, so a restriction of it has both
    /// containment and edges to carry over. Each node's label is the name it is
    /// interned under, so resolving an id says which node it names.
    fn forest() -> (Graph, Names) {
        let mut g = Graph::new();
        let mut n = Names::default();
        let mut actions: Vec<Id> = Vec::new();
        for p in 0..2 {
            let package = format!("//p{p}");
            g.add_node(Node::new(n.intern(&package), &package, Kind::Package, None));
            for t in 0..2 {
                let target = format!("{package}:t{t}");
                let parent = n.get(&package);
                g.add_node(Node::new(n.intern(&target), &target, Kind::Target, parent));
                for a in 0..2 {
                    let action = format!("{target}#a{a}");
                    let parent = n.get(&target);
                    g.add_node(Node::new(n.intern(&action), &action, Kind::Action, parent));
                    actions.push(n.get(&action).unwrap());
                }
            }
        }
        for pair in actions.windows(2) {
            g.add_edge(pair[0], pair[1], EdgeKind::Aquery);
        }
        (g, n)
    }

    /// Every other id of [`forest`], which drops some containers whose children
    /// survive and carries the largest surviving id well past the survivors'
    /// count.
    fn every_other(graph: &Graph) -> HashSet<Id> {
        graph
            .nodes
            .iter()
            .map(|node| node.id)
            .filter(|id| id.0 % 2 == 0)
            .collect()
    }

    #[test]
    fn a_survivor_keeps_the_id_it_carried_in_the_whole_graph() {
        let (g, _) = forest();
        let keep = every_other(&g);

        let out = filter(&g, &reduce::transitive_reduction(&g), &keep);

        assert!(out.nodes.len() < g.nodes.len(), "nothing was dropped");
        assert_eq!(
            out.nodes
                .iter()
                .map(|node| node.id)
                .collect::<HashSet<Id>>(),
            keep,
        );
        assert!(
            out.nodes
                .iter()
                .any(|node| node.id.0 as usize >= out.nodes.len()),
            "every surviving id fits within the survivors' count",
        );
    }

    /// The name table is built over the whole graph and never rebuilt, so a
    /// restriction's ids have to go on resolving through it.
    #[test]
    fn a_survivor_resolves_through_the_whole_graph_name_table() {
        let (g, n) = forest();
        let out = filter(&g, &reduce::transitive_reduction(&g), &every_other(&g));
        for node in &out.nodes {
            assert_eq!(n.resolve(node.id), node.label);
        }
    }

    /// The passes downstream of a restriction index by id, so they have to take
    /// an id space with holes in it.
    #[test]
    fn a_restricted_graph_feeds_the_fold_the_reduction_and_the_map() {
        let (g, _) = forest();
        let keep = every_other(&g);
        let out = filter(&g, &reduce::transitive_reduction(&g), &keep);

        let folded = lod::collapse(&out, &lod::expandable_containers(&out));
        assert!(folded.nodes.iter().all(|node| keep.contains(&node.id)));

        let reduced = reduce::transitive_reduction(&out);
        assert!(reduced.edges.len() <= out.edges.len());

        let map = map_layout(Arc::new(out.clone()), Arc::new(reduced));
        let finest = map.level(map.level_count() - 1);
        assert_eq!(
            finest.boxes.iter().map(|b| b.id).collect::<HashSet<Id>>(),
            keep,
        );
    }

    /// A merge group's container is told from an ordinary box by its `group`
    /// marker and not by its kind, so a restriction that drops the marker leaves
    /// a container the reader can no longer open or close.
    #[test]
    fn a_kept_group_container_keeps_its_marker() {
        let mut g = Graph::new();
        let mut n = Names::default();
        let action = n.intern("action0");
        g.add_node(Node::new(action, "Compile", Kind::Action, None));
        for source in ["source1", "source2"] {
            let id = n.intern(source);
            g.add_node(Node::new(id, source, Kind::Source, None));
            g.add_edge(id, action, EdgeKind::Aquery);
        }
        let other = n.intern("other");
        g.add_node(Node::new(other, "other", Kind::Action, None));
        let group = n.intern("group:source1");
        let (view, _) = crate::quotient::quotient(&g, &[group].into_iter().collect(), &mut n);
        assert!(
            crate::test_support::find(&view, &n, "group:source1").group,
            "the fixture's quotient minted no group container",
        );

        let keep: HashSet<Id> = view
            .nodes
            .iter()
            .map(|node| node.id)
            .filter(|&id| id != other)
            .collect();
        let out = filter(&view, &reduce::transitive_reduction(&view), &keep);

        assert!(crate::test_support::find(&out, &n, "group:source1").group);
    }

    #[test]
    fn an_id_absent_from_the_graph_is_ignored() {
        let (g, n) = build(&[("a", "b")]);
        let mut keep = ids(&n, &["a", "b"]);
        keep.insert(Id(9999));
        let out = filter(&g, &reduce::transitive_reduction(&g), &keep);
        assert_eq!(out.nodes.len(), 2);
    }

    /// The reduction drops `a -> c` as implied by `a -> b -> c`, so the paths
    /// the bridges are read from hold only the one through `b`. The pair is
    /// nevertheless a dependency the reader asked about directly, and keeps the
    /// query it was found by rather than being restated as a path through a node
    /// no longer drawn.
    #[test]
    fn a_pair_the_reduction_dropped_keeps_its_own_edge() {
        let (g, n) = build(&[("a", "b"), ("b", "c"), ("a", "c")]);
        let reduced = reduce::transitive_reduction(&g);
        assert!(
            !reduced
                .edges
                .iter()
                .any(|e| e.from == n.get("a").unwrap() && e.to == n.get("c").unwrap()),
            "the fixture's direct edge survives the reduction",
        );

        let out = filter(&g, &reduced, &ids(&n, &["a", "c"]));

        assert_eq!(
            edges(&out, &n),
            [("a".to_string(), "c".to_string(), EdgeKind::Query)]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn a_dropped_hub_bridges_no_further_than_the_fan_out_cap() {
        let targets = MAX_BRIDGE_FAN_OUT * 3;
        let mut owned: Vec<(String, String)> = vec![
            ("a".to_string(), "hub".to_string()),
            ("b".to_string(), "hub".to_string()),
        ];
        owned.extend((0..targets).map(|i| ("hub".to_string(), format!("t{i:03}"))));
        let refs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(x, y)| (x.as_str(), y.as_str()))
            .collect();
        let (g, n) = build(&refs);
        let keep: HashSet<Id> = g
            .nodes
            .iter()
            .map(|node| node.id)
            .filter(|&id| id != n.get("hub").unwrap())
            .collect();

        let out = filter(&g, &reduce::transitive_reduction(&g), &keep);

        for source in ["a", "b"] {
            let source = n.get(source).unwrap();
            assert_eq!(
                out.edges.iter().filter(|e| e.from == source).count(),
                MAX_BRIDGE_FAN_OUT,
            );
        }
    }

    /// Every kept node's reach over `graph`, for comparing two restrictions that
    /// need not draw the same edges.
    fn reach(graph: &Graph) -> HashMap<Id, BTreeSet<Id>> {
        let mut successors: HashMap<Id, Vec<Id>> = HashMap::with_capacity(graph.nodes.len());
        for edge in &graph.edges {
            successors.entry(edge.from).or_default().push(edge.to);
        }
        let mut reach: HashMap<Id, BTreeSet<Id>> = HashMap::with_capacity(graph.nodes.len());
        for node in &graph.nodes {
            let mut seen: BTreeSet<Id> = BTreeSet::new();
            let mut walk = vec![node.id];
            while let Some(u) = walk.pop() {
                for &w in successors.get(&u).map(Vec::as_slice).unwrap_or(&[]) {
                    if seen.insert(w) {
                        walk.push(w);
                    }
                }
            }
            reach.insert(node.id, seen);
        }
        reach
    }

    /// A reduction reaches exactly as far as the graph it came from, so which
    /// kept nodes a restriction connects cannot depend on which of the two the
    /// bridges were read from — even where the particular pairs bridged differ,
    /// a path the reduction routes through a kept node being drawn as the two
    /// bridges it breaks into. Read from the graph itself, which satisfies the
    /// same-reachability requirement trivially, and from its reduction, over
    /// every subset of a fixture with a cycle and several shortcuts in it.
    #[test]
    fn bridging_over_the_reduction_keeps_the_reach_among_kept_nodes() {
        let (g, n) = build(&[
            ("a", "b"),
            ("b", "c"),
            ("a", "c"),
            ("c", "d"),
            ("d", "e"),
            ("e", "c"),
            ("b", "e"),
            ("e", "f"),
            ("a", "f"),
        ]);
        let reduced = reduce::transitive_reduction(&g);
        let all: Vec<Id> = g.nodes.iter().map(|node| node.id).collect();

        for subset in 0..1u32 << all.len() {
            let keep: HashSet<Id> = all
                .iter()
                .enumerate()
                .filter(|(i, _)| subset >> i & 1 == 1)
                .map(|(_, &id)| id)
                .collect();
            let named: Vec<&str> = keep.iter().map(|&id| n.resolve(id)).collect();
            assert_eq!(
                reach(&filter(&g, &g, &keep)),
                reach(&filter(&g, &reduced, &keep)),
                "keeping {named:?}",
            );
        }
    }
}
