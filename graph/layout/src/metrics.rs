//! Dependency metrics per unit of a build: how much of the build each unit
//! pulls in, and how much of it depends on the unit in turn.

use rayon::prelude::*;

use crate::facets::Facets;
use crate::graph::{Edge, Graph, Kind, Node, configured_holders, first_by_id};
use crate::intern::{Id, Names, stable_hash};
use crate::provenance::{Input, InputSet, Provenance};
pub use crate::reduce::Count;
use crate::reduce::{DenseIds, reach_counts};

/// What a point of a measure stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Unit {
    /// An action. An artifact links the action producing it to each action
    /// reading it, and is no point itself. A source file an action reads is a
    /// point that counts towards no other point's measure: a change to it
    /// reruns actions, but it is no work a change reruns.
    Action,
    /// A target in one configuration, standing for its actions and the files
    /// they write. A node no configured target holds, such as a target a query
    /// names or the container a cquery edge falls back to without the
    /// configuration of its dependency, takes no part.
    ConfiguredTarget,
    /// A target, standing for every node that belongs to it — its configured
    /// targets, actions and files — so a graph read at any granularity measures
    /// alike, and an edge between two nodes of one target is no dependency of
    /// it on itself.
    Target,
}

impl Unit {
    /// Finest first.
    pub const ALL: [Unit; 3] = [Unit::Action, Unit::ConfiguredTarget, Unit::Target];

    /// The inputs describing this unit, the only ones a measure in it reads.
    pub fn inputs(self) -> InputSet {
        match self {
            Unit::Action => Input::Aquery.into(),
            Unit::ConfiguredTarget => InputSet::from(Input::Cquery).with(Input::Aquery),
            Unit::Target => InputSet::ALL,
        }
    }
}

/// A unit, and the inputs a measure in it reads: at least one, and only those
/// describing the unit, so two scopes reading alike are equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Scope {
    unit: Unit,
    inputs: InputSet,
}

impl Scope {
    /// The measure in `unit` over those of `inputs` describing it, absent where
    /// none does.
    pub fn new(unit: Unit, inputs: InputSet) -> Option<Self> {
        Some(Self {
            unit,
            inputs: inputs.intersection(unit.inputs())?,
        })
    }

    /// The scope over its own inputs and those of `inputs` describing its
    /// unit.
    pub fn widened(self, inputs: InputSet) -> Self {
        let more = inputs.intersection(self.unit.inputs());
        Self {
            unit: self.unit,
            inputs: more.map_or(self.inputs, |more| self.inputs.union(more)),
        }
    }

    pub fn unit(self) -> Unit {
        self.unit
    }

    pub fn by_unit(self) -> ByUnit {
        let coarser = |unit| Scope {
            unit,
            inputs: self.inputs,
        };
        match self.unit {
            Unit::Action => ByUnit::Action {
                configured_targets: coarser(Unit::ConfiguredTarget),
            },
            Unit::ConfiguredTarget => ByUnit::ConfiguredTarget {
                targets: coarser(Unit::Target),
            },
            Unit::Target => ByUnit::Target,
        }
    }

    pub fn inputs(self) -> InputSet {
        self.inputs
    }

    /// Whether the measure reads the edge `edge`.
    fn reads(self, edge: &Edge) -> bool {
        edge.kind
            .input()
            .is_some_and(|input| self.inputs.contains(input))
    }

    /// Whether the node `id` is one the measure's inputs built.
    fn built(self, provenance: &Provenance, id: Id) -> bool {
        provenance
            .of(id)
            .is_some_and(|built_by| built_by.intersects(self.inputs))
    }
}

/// A scope by the unit of its points, with the scope of the next coarser
/// unit reading the same inputs, which describe that unit as well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByUnit {
    Action {
        /// Of the configured targets holding the actions.
        configured_targets: Scope,
    },
    ConfiguredTarget {
        /// Of the targets the configured targets are copies of.
        targets: Scope,
    },
    Target,
}

/// What one point's dependencies come to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    pub id: Id,
    pub kind: Kind,
    /// How many points it depends on, directly or transitively.
    pub closure: Count,
    /// How many points depend on it, directly or transitively: what a change
    /// to it can break.
    pub blast_radius: Count,
    /// How many points depend on it directly.
    pub fan_in: u32,
    /// How many points it depends on directly.
    pub fan_out: u32,
    /// How many actions it stands for: an action itself, and a configured
    /// target or target those it holds.
    pub actions: u32,
}

/// The metrics of the points of one scope.
#[derive(Clone, Debug)]
pub struct Measurement {
    pub scope: Scope,
    /// In node order.
    pub points: Vec<Metrics>,
    pub sizing: Sizing,
}

/// Which count of a point stands for its size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sizing {
    /// The actions it stands for, which is what a build of it runs.
    Actions,
    /// The points depending on it directly.
    FanIn,
}

impl Measurement {
    /// Whether any count of it is an estimate.
    pub fn estimated(&self) -> bool {
        let estimated = |m: &Metrics| m.closure.is_estimated() || m.blast_radius.is_estimated();
        self.points.iter().any(estimated)
    }
}

impl Sizing {
    pub fn of(self, metrics: &Metrics) -> u32 {
        match self {
            Sizing::Actions => metrics.actions,
            Sizing::FanIn => metrics.fan_in,
        }
    }
}

/// The metrics of every point of `graph` in `scope`. A point is a node one of
/// the scope's inputs built, and only the edges read from one of them count, so
/// each input's share in the measure can be weighed by leaving it out.
pub fn measure(
    graph: &Graph,
    names: &Names,
    facets: &Facets,
    provenance: &Provenance,
    scope: Scope,
) -> Measurement {
    let points = unit_graph(graph, facets, provenance, scope).metrics(names);
    // Decided by the graph alone, whatever inputs the scope reads, so the
    // sizes of one unit compare across scopes.
    let holds_actions = graph.nodes.iter().any(|node| node.kind == Kind::Action);
    let sizing = match scope.unit {
        Unit::ConfiguredTarget | Unit::Target if holds_actions => Sizing::Actions,
        _ => Sizing::FanIn,
    };
    Measurement {
        scope,
        points,
        sizing,
    }
}

/// The points of `graph` in `scope` and the dependencies between them.
pub(crate) fn unit_graph(
    graph: &Graph,
    facets: &Facets,
    provenance: &Provenance,
    scope: Scope,
) -> UnitGraph {
    match scope.unit {
        Unit::Action => actions(graph, provenance, scope),
        Unit::ConfiguredTarget => configured_targets(graph, provenance, scope),
        Unit::Target => targets(graph, facets, provenance, scope),
    }
}

/// A node of the graph a measure takes as a point.
pub(crate) struct Point {
    pub(crate) id: Id,
    pub(crate) kind: Kind,
    /// Whether the point counts towards the measure of the points reaching it.
    pub(crate) counted: bool,
}

impl Point {
    fn of(node: &Node, counted: bool) -> Self {
        Self {
            id: node.id,
            kind: node.kind,
            counted,
        }
    }
}

/// The points of a measure, each numbered by its index.
struct Points {
    points: Vec<Point>,
    index: DenseIds,
}

impl Points {
    /// The points `candidates` gives, which may name a node more than once.
    fn new(mut candidates: Vec<Point>) -> Self {
        let (index, _) = DenseIds::new(candidates.iter().map(|point| point.id));
        // Ids are numbered as first named, so this keeps each point's first node.
        let mut first = 0;
        candidates.retain(|point| {
            let keep = index.get(point.id) == Some(first);
            first += u32::from(keep);
            keep
        });
        Self {
            points: candidates,
            index,
        }
    }

    /// The index of the point the node `id` is, if it is one.
    fn of(&self, id: Id) -> Option<u32> {
        self.index.get(id)
    }

    /// By point, how many of `graph`'s actions `owner` names it as holding.
    fn owned_actions(&self, graph: &Graph, owner: impl Fn(Id) -> Option<u32>) -> Vec<u32> {
        let mut actions = vec![0u32; self.points.len()];
        for node in graph.nodes.iter().filter(|node| node.kind == Kind::Action) {
            if let Some(p) = owner(node.id) {
                actions[p as usize] += 1;
            }
        }
        actions
    }

    /// The graph of these points with `pairs` between them, which run from a
    /// dependency to its dependent by index, and with the actions `actions`
    /// gives by point.
    fn graph(self, mut pairs: Vec<(u32, u32)>, actions: Vec<u32>) -> UnitGraph {
        pairs.retain(|(from, to)| from != to);
        bviz_parallel::workers().install(|| pairs.par_sort_unstable());
        pairs.dedup();
        debug_assert!(
            pairs
                .iter()
                .all(|&(_, to)| self.points[to as usize].counted),
            "an uncounted point depends on another"
        );
        let (dependents, dependencies) = adjacency(self.points.len(), &pairs);
        UnitGraph {
            points: self.points,
            dependents,
            dependencies,
            actions,
        }
    }
}

/// The points of a scope and the distinct dependencies between them, each
/// point numbered by its index. An uncounted point depends on no other point.
pub(crate) struct UnitGraph {
    pub(crate) points: Vec<Point>,
    /// By point, the points depending on it directly.
    pub(crate) dependents: Vec<Vec<u32>>,
    /// By point, the points it depends on directly.
    pub(crate) dependencies: Vec<Vec<u32>>,
    /// By point, how many actions it stands for.
    actions: Vec<u32>,
}

impl UnitGraph {
    /// The metrics of each point, in point order.
    fn metrics(self, names: &Names) -> Vec<Metrics> {
        let Self {
            points,
            dependents,
            dependencies,
            actions,
        } = self;
        let counted = |p: usize| points[p].counted;
        let key = |p: usize| stable_hash(names.resolve(points[p].id));
        let blast_radius = reach_counts(&dependents, counted, key);
        let closure = reach_counts(&dependencies, counted, key);
        let counted_among = |of: &[u32]| of.iter().filter(|&&p| counted(p as usize)).count() as u32;
        points
            .iter()
            .enumerate()
            .map(|(p, point)| Metrics {
                id: point.id,
                kind: point.kind,
                closure: closure[p],
                blast_radius: blast_radius[p],
                fan_in: counted_among(&dependents[p]),
                fan_out: counted_among(&dependencies[p]),
                actions: actions[p],
            })
            .collect()
    }
}

/// Every node that `facets` names as its own target, each edge between two
/// nodes standing for one between their targets.
fn targets(graph: &Graph, facets: &Facets, provenance: &Provenance, scope: Scope) -> UnitGraph {
    let points = Points::new(
        graph
            .nodes
            .iter()
            .filter(|node| facets.label(node.id) == Some(node.id))
            .filter(|node| scope.built(provenance, node.id))
            .map(|node| Point::of(node, true))
            .collect(),
    );
    let target_of = |id: Id| points.of(facets.label(id)?);
    let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(graph.edges.len());
    pairs.extend(
        graph
            .edges
            .iter()
            .filter(|edge| scope.reads(edge))
            .filter_map(|edge| Some((target_of(edge.from)?, target_of(edge.to)?))),
    );
    let actions = points.owned_actions(graph, target_of);
    points.graph(pairs, actions)
}

/// Every configured target, each edge between two nodes standing for one
/// between the configured targets holding them.
fn configured_targets(graph: &Graph, provenance: &Provenance, scope: Scope) -> UnitGraph {
    let held_by = configured_holders(&first_by_id(graph));
    let points = Points::new(
        graph
            .nodes
            .iter()
            .filter(|node| node.kind == Kind::ConfiguredTarget)
            .filter(|node| scope.built(provenance, node.id))
            .map(|node| Point::of(node, true))
            .collect(),
    );
    let configured_of = |id: Id| points.of(held_by[id.0 as usize]?);
    let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(graph.edges.len());
    pairs.extend(
        graph
            .edges
            .iter()
            .filter(|edge| scope.reads(edge))
            .filter_map(|edge| Some((configured_of(edge.from)?, configured_of(edge.to)?))),
    );
    let actions = points.owned_actions(graph, configured_of);
    points.graph(pairs, actions)
}

/// Every action, and every source file an action reads, each artifact standing
/// for the actions producing it.
fn actions(graph: &Graph, provenance: &Provenance, scope: Scope) -> UnitGraph {
    let node_of = first_by_id(graph);
    let is_action = |id: Id| node_of[id.0 as usize].is_some_and(|node| node.kind == Kind::Action);
    let mut read: Vec<&Edge> = Vec::with_capacity(graph.edges.len());
    read.extend(graph.edges.iter().filter(|e| scope.reads(e)));

    // By artifact, the actions producing it.
    let mut produced: Vec<(Id, Id)> = Vec::with_capacity(read.len());
    produced.extend(
        read.iter()
            .filter(|e| is_action(e.from) && !is_action(e.to))
            .map(|e| (e.to, e.from)),
    );
    produced.sort_unstable();
    produced.dedup();
    let producers = |file: Id| {
        let from = produced.partition_point(|&(artifact, _)| artifact < file);
        let to = produced.partition_point(|&(artifact, _)| artifact <= file);
        produced[from..to].iter().map(|&(_, action)| action)
    };

    let inputs: Vec<&Edge> = read.into_iter().filter(|e| is_action(e.to)).collect();
    let mut source = vec![false; node_of.len()];
    for edge in &inputs {
        if !is_action(edge.from) && producers(edge.from).next().is_none() {
            source[edge.from.0 as usize] = true;
        }
    }
    let points = Points::new(
        graph
            .nodes
            .iter()
            .filter_map(|node| {
                let counted = match node.kind {
                    Kind::Action if scope.built(provenance, node.id) => true,
                    _ if source[node.id.0 as usize] => false,
                    _ => return None,
                };
                Some(Point::of(node, counted))
            })
            .collect(),
    );
    let mut pairs: Vec<(u32, u32)> = Vec::with_capacity(inputs.len());
    for edge in &inputs {
        let Some(consumer) = points.of(edge.to) else {
            continue;
        };
        if is_action(edge.from) || source[edge.from.0 as usize] {
            pairs.extend(points.of(edge.from).map(|p| (p, consumer)));
        } else {
            let producing = producers(edge.from).filter_map(|action| points.of(action));
            pairs.extend(producing.map(|p| (p, consumer)));
        }
    }
    let actions = points.owned_actions(graph, |id| points.of(id));
    points.graph(pairs, actions)
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
    use crate::graph::EdgeKind;
    use crate::intern::Names;
    use crate::provenance::Input;

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
        let everything = Provenance::new(vec![InputSet::ALL; names.len()]);
        measured_by(graph, facets, names, &everything, InputSet::ALL)
    }

    fn measured_by(
        graph: &Graph,
        facets: &Facets,
        names: &Names,
        provenance: &Provenance,
        inputs: InputSet,
    ) -> Vec<(String, u32, u32, u32, u32)> {
        let scope = Scope::new(Unit::Target, inputs).unwrap();
        measure(graph, names, facets, provenance, scope)
            .points
            .into_iter()
            .map(|m| {
                (
                    names.resolve(m.id).to_string(),
                    m.closure.value(),
                    m.blast_radius.value(),
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

    /// query names `lib` and `app`, and the aquery `lib` and `gen`: each input
    /// measures only the targets it built, through only its own edges.
    #[test]
    fn an_input_left_out_takes_its_targets_and_edges_along() {
        let (mut g, f, n) = configured(&["lib", "app", "gen"], &[("gen", "lib")]);
        g.edges[0].kind = EdgeKind::Aquery;
        let id = |name: &str| n.get(name).unwrap();
        g.add_edge(id("lib"), id("app"), EdgeKind::Query);
        let (query, aquery) = (InputSet::from(Input::Query), InputSet::from(Input::Aquery));
        let mut built_by = vec![query; n.len()];
        for name in ["gen", "gen (config)", "lib (config)"] {
            built_by[id(name).0 as usize] = aquery;
        }
        built_by[id("lib").0 as usize] = query.with(Input::Aquery);
        let provenance = Provenance::new(built_by);

        assert_eq!(
            measured_by(&g, &f, &n, &provenance, query),
            [("lib".into(), 0, 1, 1, 0), ("app".into(), 1, 0, 0, 1)]
        );
        assert_eq!(
            measured_by(&g, &f, &n, &provenance, aquery),
            [("lib".into(), 1, 0, 0, 1), ("gen".into(), 0, 1, 1, 0)]
        );
        assert_eq!(
            measured_by(&g, &f, &n, &provenance, InputSet::ALL),
            [
                ("lib".into(), 1, 1, 1, 1),
                ("app".into(), 2, 0, 0, 1),
                ("gen".into(), 0, 2, 1, 0),
            ]
        );
    }

    /// A build of actions, each under the configured target it names, the
    /// files they write and read, and the source files among what they read.
    #[derive(Default)]
    struct Build {
        graph: Graph,
        names: Names,
    }

    impl Build {
        fn node(&mut self, name: &str, kind: Kind, parent: Option<Id>) -> Id {
            if let Some(id) = self.names.get(name) {
                return id;
            }
            let id = self.names.intern(name);
            self.graph.add_node(Node::new(id, name, kind, parent));
            id
        }

        fn action(&mut self, name: &str, configured: &str) -> &mut Self {
            let target = self.node(&format!("target {configured}"), Kind::Target, None);
            let ct = self.node(configured, Kind::ConfiguredTarget, Some(target));
            self.node(name, Kind::Action, Some(ct));
            self
        }

        /// `action` writes the file `artifact`, beside it in its configured target.
        fn writes(&mut self, action: &str, artifact: &str) -> &mut Self {
            let action = self.names.get(action).unwrap();
            let ct = self.graph.nodes[action.0 as usize].parent;
            let artifact = self.node(artifact, Kind::Artifact, ct);
            self.graph.add_edge(action, artifact, EdgeKind::Aquery);
            self
        }

        /// `action` reads `file`, a source file where no action writes it.
        fn reads(&mut self, action: &str, file: &str) -> &mut Self {
            let file = self.node(file, Kind::Source, None);
            let action = self.names.get(action).unwrap();
            self.graph.add_edge(file, action, EdgeKind::Aquery);
            self
        }

        /// A chain of actions `names`, each reading what the one before writes.
        fn chain(&mut self, configured: &str, names: &[&str]) -> &mut Self {
            for pair in names.windows(2) {
                let written = format!("{} out", pair[0]);
                self.action(pair[0], configured)
                    .action(pair[1], configured)
                    .writes(pair[0], &written)
                    .reads(pair[1], &written);
            }
            self
        }

        /// An edge of `kind` from the node `from` to the node `to`.
        fn edge(&mut self, from: &str, to: &str, kind: EdgeKind) -> &mut Self {
            let (from, to) = (self.names.get(from).unwrap(), self.names.get(to).unwrap());
            self.graph.add_edge(from, to, kind);
            self
        }

        /// Each action's name, kind, closure, blast radius, fan-in and fan-out.
        fn actions(&self) -> Vec<(&str, Kind, u32, u32, u32, u32)> {
            self.measured(Unit::Action)
        }

        fn measured(&self, unit: Unit) -> Vec<(&str, Kind, u32, u32, u32, u32)> {
            let everything = Provenance::new(vec![InputSet::ALL; self.names.len()]);
            let scope = Scope::new(unit, InputSet::ALL).unwrap();
            measure(
                &self.graph,
                &self.names,
                &Facets::default(),
                &everything,
                scope,
            )
            .points
            .into_iter()
            .map(|m| {
                let name = self.names.resolve(m.id);
                let (closure, blast_radius) = (m.closure.value(), m.blast_radius.value());
                (name, m.kind, closure, blast_radius, m.fan_in, m.fan_out)
            })
            .collect()
        }
    }

    #[test]
    fn a_chain_of_actions_links_through_the_files_between_them() {
        let mut build = Build::default();
        build
            .chain("lib", &["compile", "archive", "link"])
            .reads("compile", "lib.c");
        assert_eq!(
            build.actions(),
            [
                ("compile", Kind::Action, 0, 2, 1, 0),
                ("archive", Kind::Action, 1, 1, 1, 1),
                ("link", Kind::Action, 2, 0, 0, 1),
                // A source file reruns what reads it, but counts towards
                // nothing that depends on it.
                ("lib.c", Kind::Source, 0, 3, 1, 0),
            ]
        );
    }

    /// One file two actions read makes both dependents of its producer, and
    /// the action joining them counts the producer once.
    #[test]
    fn a_shared_input_fans_out_and_its_join_counts_once() {
        let mut build = Build::default();
        build
            .action("gen", "g")
            .action("left", "l")
            .action("right", "r")
            .action("join", "j")
            .writes("gen", "header")
            .reads("left", "header")
            .reads("right", "header")
            .writes("left", "left.o")
            .writes("right", "right.o")
            .reads("join", "left.o")
            .reads("join", "right.o");
        assert_eq!(
            build.actions(),
            [
                ("gen", Kind::Action, 0, 3, 2, 0),
                ("left", Kind::Action, 1, 1, 1, 1),
                ("right", Kind::Action, 1, 1, 1, 1),
                ("join", Kind::Action, 3, 0, 0, 2),
            ]
        );
    }

    /// A file two actions write stands for both, so the action reading it
    /// depends on each.
    #[test]
    fn a_file_several_actions_write_links_each_to_its_readers() {
        let mut build = Build::default();
        build
            .action("gen", "g")
            .action("regen", "r")
            .action("cc", "c")
            .writes("gen", "shared.h")
            .writes("regen", "shared.h")
            .reads("cc", "shared.h");
        assert_eq!(
            build.actions(),
            [
                ("gen", Kind::Action, 0, 1, 1, 0),
                ("regen", Kind::Action, 0, 1, 1, 0),
                ("cc", Kind::Action, 2, 0, 0, 2),
            ]
        );
    }

    /// A tool built in the exec configuration reaches every action of every
    /// target running it, though those targets depend on no target of it.
    #[test]
    fn an_exec_tool_reaches_every_action_running_it() {
        let mut build = Build::default();
        build
            .action("build tool", "tool (exec)")
            .writes("build tool", "tool");
        for target in ["a", "b", "c"] {
            let [generate, compile] = [format!("{target} gen"), format!("{target} cc")];
            build
                .chain(target, &[&generate, &compile])
                .reads(&generate, "tool");
        }
        let tool = build.actions()[0];
        assert_eq!(tool, ("build tool", Kind::Action, 0, 6, 3, 0));
    }

    #[test]
    fn actions_in_a_cycle_reach_each_other() {
        let mut build = Build::default();
        build
            .chain("t", &["one", "two", "three"])
            .writes("two", "back")
            .reads("one", "back");
        assert_eq!(
            build.actions(),
            [
                ("one", Kind::Action, 1, 2, 1, 1),
                ("two", Kind::Action, 1, 2, 2, 1),
                ("three", Kind::Action, 2, 0, 0, 1),
            ]
        );
    }

    #[test]
    fn only_aquery_describes_actions() {
        assert_eq!(Scope::new(Unit::Action, Input::Query.into()), None);
        let scope = Scope::new(Unit::Action, InputSet::ALL).unwrap();
        assert_eq!(scope.inputs(), Input::Aquery.into());
        let targets = Scope::new(Unit::Target, InputSet::ALL).unwrap();
        assert_eq!(targets.inputs(), InputSet::ALL);
    }

    /// The scope a scope gives for the next coarser unit is the one that unit
    /// has over the same inputs.
    #[test]
    fn a_scope_reads_the_next_coarser_unit_from_its_own_inputs() {
        let over = |unit: Unit, inputs: InputSet| Scope::new(unit, inputs).unwrap();
        let actions = over(Unit::Action, InputSet::ALL);
        let configured = over(Unit::ConfiguredTarget, actions.inputs());
        let held = ByUnit::Action {
            configured_targets: configured,
        };
        assert_eq!(actions.by_unit(), held);
        for inputs in [Input::Cquery.into(), InputSet::ALL] {
            let configured = over(Unit::ConfiguredTarget, inputs);
            let targets = over(Unit::Target, configured.inputs());
            assert_eq!(configured.by_unit(), ByUnit::ConfiguredTarget { targets });
            assert_eq!(targets.by_unit(), ByUnit::Target);
        }
    }

    /// aquery links `app`'s actions to `lib`'s and to a tool's built for the
    /// exec configuration, and cquery links `app` to `test` directly and to a
    /// `data` target only by its label, which no configured target holds.
    #[test]
    fn configured_targets_link_through_what_they_hold() {
        let mut build = Build::default();
        build
            .action("lib cc", "lib")
            .writes("lib cc", "lib.o")
            .action("tool cc", "tool (exec)")
            .writes("tool cc", "tool")
            .action("app gen", "app")
            .reads("app gen", "tool")
            .action("app link", "app")
            .reads("app link", "lib.o")
            .action("test run", "test")
            .edge("app", "test", EdgeKind::Cquery);
        build.node("target data", Kind::Target, None);
        build.edge("target data", "app", EdgeKind::Cquery);
        assert_eq!(
            build.measured(Unit::ConfiguredTarget),
            [
                ("lib", Kind::ConfiguredTarget, 0, 2, 1, 0),
                ("tool (exec)", Kind::ConfiguredTarget, 0, 2, 1, 0),
                ("app", Kind::ConfiguredTarget, 2, 1, 1, 2),
                ("test", Kind::ConfiguredTarget, 3, 0, 0, 1),
            ]
        );
    }

    #[test]
    fn a_widened_scope_takes_only_the_inputs_describing_its_unit() {
        let actions = Scope::new(Unit::Action, InputSet::ALL).unwrap();
        assert_eq!(actions.widened(InputSet::ALL), actions);
        let cquery = Scope::new(Unit::ConfiguredTarget, Input::Cquery.into()).unwrap();
        assert_eq!(
            cquery.widened(InputSet::ALL),
            Scope::new(Unit::ConfiguredTarget, InputSet::ALL).unwrap()
        );
        assert_eq!(cquery.widened(Input::Query.into()), cquery);
    }

    #[test]
    fn query_describes_no_configured_target() {
        assert_eq!(
            Scope::new(Unit::ConfiguredTarget, Input::Query.into()),
            None
        );
        let scope = Scope::new(Unit::ConfiguredTarget, InputSet::ALL).unwrap();
        assert_eq!(
            scope.inputs(),
            InputSet::from(Input::Cquery).with(Input::Aquery)
        );
    }

    /// A target with actions in two configurations weighs all of them, each
    /// configured target its own, an action itself and a source file nothing.
    #[test]
    fn a_point_weighs_the_actions_it_holds() {
        let mut build = Build::default();
        build
            .chain("app", &["gen", "cc", "link"])
            .action("gen tool", "app (exec)")
            .reads("gen tool", "gen.py");
        let mut facets = Facets::default();
        let app = build.node("target app", Kind::Target, None);
        for node in &build.graph.nodes {
            facets.set_label(node.id, app);
        }
        let everything = Provenance::new(vec![InputSet::ALL; build.names.len()]);
        let weighed = |unit| {
            let scope = Scope::new(unit, InputSet::ALL).unwrap();
            let measured = measure(&build.graph, &build.names, &facets, &everything, scope);
            let sizes = measured.points.iter().map(|m| {
                let name = build.names.resolve(m.id);
                (name, m.actions, measured.sizing.of(m))
            });
            (measured.sizing, sizes.collect::<Vec<_>>())
        };

        assert_eq!(
            weighed(Unit::Target),
            (Sizing::Actions, vec![("target app", 4, 4)])
        );
        assert_eq!(
            weighed(Unit::ConfiguredTarget),
            (Sizing::Actions, vec![("app", 3, 3), ("app (exec)", 1, 1)])
        );
        assert_eq!(
            weighed(Unit::Action),
            (
                Sizing::FanIn,
                vec![
                    ("gen", 1, 1),
                    ("cc", 1, 1),
                    ("link", 1, 0),
                    ("gen tool", 1, 0),
                    ("gen.py", 0, 1),
                ]
            )
        );
    }

    #[test]
    fn a_graph_holding_no_action_sizes_by_fan_in() {
        let (g, f, n) = configured(&["a", "b"], &[("a", "b")]);
        let everything = Provenance::new(vec![InputSet::ALL; n.len()]);
        let scope = Scope::new(Unit::Target, InputSet::ALL).unwrap();
        let measured = measure(&g, &n, &f, &everything, scope);
        assert_eq!(measured.sizing, Sizing::FanIn);
    }
}
