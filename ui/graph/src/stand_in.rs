//! Which points stand for a node of the graph in a view drawing only the
//! points of one unit, as the scatter plot and the exclusive deps icicle do.

use bviz_graph_layout::graph::{Graph, Kind};
use bviz_graph_layout::metrics::Unit;
use bviz_graph_layout::{Facets, Id};
use rustc_hash::FxHashMap;

/// By file, the actions writing it: the action ends of the edges joining an
/// action to a file, as a measure of actions reads them.
pub(crate) struct Writers(FxHashMap<Id, Vec<Id>>);

impl Writers {
    pub(crate) fn of(graph: &Graph) -> Self {
        let is_action = |id: Id| kind(graph, id) == Some(Kind::Action);
        let mut writers: FxHashMap<Id, Vec<Id>> = FxHashMap::default();
        for edge in &graph.edges {
            if is_action(edge.from) && !is_action(edge.to) {
                writers.entry(edge.to).or_default().push(edge.from);
            }
        }
        Writers(writers)
    }

    fn of_file(&self, file: Id) -> &[Id] {
        self.0.get(&file).map_or(&[], Vec::as_slice)
    }
}

/// The points that may stand for a node, read off the graph's shape alone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum StandIns {
    Points(Vec<Id>),
    /// The node holds points of the unit, which [`held`] finds.
    Holding,
}

/// The points of `unit` that may stand for `id`, where `is_point` says which
/// nodes the view takes as points: `id` itself where it is one; else the
/// target it belongs to, the configured target holding it, or the actions
/// writing a file; and for a node coarser than the unit, every point it
/// holds.
pub(crate) fn stand_ins(
    unit: Unit,
    graph: &Graph,
    facets: &Facets,
    writers: &Writers,
    is_point: impl Fn(Id) -> bool,
    id: Id,
) -> StandIns {
    if is_point(id) {
        return StandIns::Points(vec![id]);
    }
    let Some(kind) = kind(graph, id) else {
        return StandIns::Points(Vec::new());
    };
    if holds_points(unit, kind) {
        return StandIns::Holding;
    }
    let points = match unit {
        Unit::Target => facets.label(id).into_iter().collect(),
        Unit::ConfiguredTarget => (ancestors(graph, id))
            .find(|&at| self::kind(graph, at) == Some(Kind::ConfiguredTarget))
            .into_iter()
            .collect(),
        Unit::Action if kind == Kind::Artifact => writers.of_file(id).to_vec(),
        Unit::Action => vec![id],
    };
    StandIns::Points(points)
}

/// Whether a node of `kind` stands for the points of `unit` it holds.
fn holds_points(unit: Unit, kind: Kind) -> bool {
    match unit {
        Unit::Target => matches!(kind, Kind::Package | Kind::MergeGroup),
        Unit::ConfiguredTarget => matches!(kind, Kind::Package | Kind::MergeGroup | Kind::Target),
        Unit::Action => matches!(
            kind,
            Kind::Package | Kind::MergeGroup | Kind::Target | Kind::ConfiguredTarget
        ),
    }
}

/// Those of `points` that `id` holds, at any depth.
pub(crate) fn held<'g>(
    graph: &'g Graph,
    points: impl Iterator<Item = Id> + 'g,
    id: Id,
) -> impl Iterator<Item = Id> + 'g {
    points.filter(move |&point| holds(graph, id, point))
}

/// Whether `holder` holds `point`, at any depth.
pub(crate) fn holds(graph: &Graph, holder: Id, point: Id) -> bool {
    ancestors(graph, point).skip(1).any(|at| at == holder)
}

/// `id` and every node holding it, innermost first.
pub(crate) fn ancestors(graph: &Graph, id: Id) -> impl Iterator<Item = Id> + '_ {
    std::iter::successors(Some(id), |at| graph.nodes.get(at.0 as usize)?.parent)
}

fn kind(graph: &Graph, id: Id) -> Option<Kind> {
    graph.nodes.get(id.0 as usize).map(|node| node.kind)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jump::fixture;
    use bviz_graph_layout::graph::EdgeKind;

    /// What stands for each named node in a view of `unit` taking the nodes
    /// `points` names as its points.
    fn stand_ins_of(unit: Unit, points: &[&str], named: &[&str]) -> Vec<StandIns> {
        let shared = fixture();
        let id = |name: &str| shared.names.get(name).unwrap();
        let mut graph = (*shared.base).clone();
        graph.add_edge(
            id("action:bazel-out/app/liblib.a"),
            id("artifact7"),
            EdgeKind::Aquery,
        );
        let writers = Writers::of(&graph);
        let points: Vec<Id> = points.iter().map(|name| id(name)).collect();
        (named.iter())
            .map(|name| {
                let is_point = |id| points.contains(&id);
                stand_ins(unit, &graph, &shared.facets, &writers, is_point, id(name))
            })
            .collect()
    }

    fn points(ids: &[Id]) -> StandIns {
        StandIns::Points(ids.to_vec())
    }

    #[test]
    fn a_node_stands_for_its_target_and_a_package_holds_targets() {
        let shared = fixture();
        let lib = shared.names.get("target://app:lib").unwrap();
        let named = ["action:bazel-out/app/liblib.a", "pkg:app", "artifact7"];
        assert_eq!(
            stand_ins_of(Unit::Target, &["target://app:lib"], &named),
            [points(&[lib]), StandIns::Holding, points(&[lib])]
        );
    }

    #[test]
    fn a_file_stands_for_its_configured_target_or_the_action_writing_it() {
        let shared = fixture();
        let id = |name: &str| shared.names.get(name).unwrap();
        let (lib_ct, compile) = (
            id("ct://app:lib#3f2a01"),
            id("action:bazel-out/app/liblib.a"),
        );
        let named = ["artifact7", "target://app:lib"];
        assert_eq!(
            stand_ins_of(Unit::ConfiguredTarget, &[], &named),
            [points(&[lib_ct]), StandIns::Holding]
        );
        let named = ["artifact7", "target://app:lib.cc", "ct://app:lib#3f2a01"];
        assert_eq!(
            stand_ins_of(Unit::Action, &[], &named),
            [
                points(&[compile]),
                points(&[id("target://app:lib.cc")]),
                StandIns::Holding
            ]
        );
    }

    #[test]
    fn a_holder_holds_the_points_beneath_it() {
        let shared = fixture();
        let id = |name: &str| shared.names.get(name).unwrap();
        let all = [
            id("target://app:lib"),
            id("target://app:bin"),
            id("ct://app:lib#3f2a01"),
        ];
        let held: Vec<Id> = held(&shared.base, all.into_iter(), id("pkg:app")).collect();
        assert_eq!(held, all);
        let held: Vec<Id> = held_of(&shared, &all, "target://app:lib");
        assert_eq!(held, [id("ct://app:lib#3f2a01")]);
    }

    fn held_of(shared: &crate::model::Shared, all: &[Id], name: &str) -> Vec<Id> {
        let id = shared.names.get(name).unwrap();
        held(&shared.base, all.iter().copied(), id).collect()
    }
}
